//! Parallel `collect()` implementation built on the crate-local scheduler.

use std::{
    any::Any,
    collections::{HashMap, HashSet},
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use crossbeam_deque::{Steal, Stealer, Worker};

use super::{
    BackendEntry, DirectoryBackend, ErrorPolicy, GitIgnoreNode, StdBackend, WalkEntry, WalkError,
    WalkResult, Walker, has_hidden_component, is_git_ignored, scheduler::Scheduler,
};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn collect(walker: Walker) -> Result<WalkResult, WalkError> {
    let walker = Arc::new(walker);
    let shared = Arc::new(Shared::new(Arc::clone(&walker)));
    let mut caller = WorkerScratch::new();

    // The caller starts alone. Helpers are created only after the root made
    // parallel directory work available, as required by ADR-0009.
    shared.begin_task();
    process_directory(&shared, &mut caller, walker.root.clone());
    shared.finish_task();
    caller.flush_into(&shared.scheduler);

    let pending = shared.pending.load(Ordering::Acquire);
    if pending == 0 {
        return finish(shared, caller.entries);
    }
    let helper_count = walker
        .threads
        .saturating_sub(1)
        .min(pending.saturating_sub(1));
    if helper_count == 0 {
        run_worker(&shared, &mut caller, &[]);
        return finish(shared, caller.entries);
    }

    let mut helpers = (0..helper_count)
        .map(|_| WorkerScratch::new())
        .collect::<Vec<_>>();
    let mut stealers = Vec::with_capacity(helper_count + 1);
    stealers.push(caller.queue.stealer());
    stealers.extend(helpers.iter().map(|worker| worker.queue.stealer()));

    let entries = thread::scope(|scope| {
        let mut joins = Vec::with_capacity(helper_count);
        for mut helper in helpers.drain(..) {
            #[cfg(test)]
            if should_fail_next_worker_spawn() {
                shared.record_startup_error(std::io::Error::other("injected worker start failure"));
                break;
            }
            let worker_shared = Arc::clone(&shared);
            let stealers = &stealers;
            let spawn = thread::Builder::new()
                .name("ferralk-worker".into())
                .spawn_scoped(scope, move || {
                    run_worker_catching_panics(&worker_shared, &mut helper, stealers);
                    helper
                });
            match spawn {
                Ok(join) => joins.push(join),
                Err(source) => {
                    shared.record_startup_error(source);
                    break;
                }
            }
        }
        run_worker_catching_panics(&shared, &mut caller, &stealers);
        let mut entries = std::mem::take(&mut caller.entries);
        for join in joins {
            // Panics are captured inside the worker so joining only waits for
            // completion and cannot bypass the shared cancellation state.
            let mut helper = join.join().expect("worker panic is captured before join");
            entries.append(&mut helper.entries);
        }
        entries
    });
    finish(shared, entries)
}

fn finish(shared: Arc<Shared>, mut entries: Vec<WalkEntry>) -> Result<WalkResult, WalkError> {
    if let Some(payload) = lock(&shared.panic).take() {
        resume_unwind(payload);
    }
    if let Some(error) = lock(&shared.startup_error).take() {
        return Err(error);
    }
    if let Some(error) = lock(&shared.abort_error).take() {
        return Err(error);
    }
    if shared.walker.options.sort {
        entries.sort_by(|left, right| left.path.cmp(&right.path));
    }
    Ok(WalkResult {
        entries,
        errors: std::mem::take(&mut *lock(&shared.errors)),
        cancelled: shared.cancellation.is_cancelled(),
    })
}

struct Shared {
    walker: Arc<Walker>,
    scheduler: Scheduler<PathBuf>,
    pending: AtomicUsize,
    cancellation: super::CancellationToken,
    wake_lock: Mutex<()>,
    wake: Condvar,
    errors: Mutex<Vec<WalkError>>,
    abort_error: Mutex<Option<WalkError>>,
    startup_error: Mutex<Option<WalkError>>,
    panic: Mutex<Option<Box<dyn Any + Send + 'static>>>,
    visited_directories: Mutex<HashSet<PathBuf>>,
}

impl Shared {
    fn new(walker: Arc<Walker>) -> Self {
        let cancellation = walker.cancellation.clone().unwrap_or_default();
        Self {
            walker,
            scheduler: Scheduler::new(),
            pending: AtomicUsize::new(0),
            cancellation,
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
            errors: Mutex::new(Vec::new()),
            abort_error: Mutex::new(None),
            startup_error: Mutex::new(None),
            panic: Mutex::new(None),
            visited_directories: Mutex::new(HashSet::new()),
        }
    }

    fn begin_task(&self) {
        self.pending.fetch_add(1, Ordering::AcqRel);
    }

    fn finish_task(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake.notify_all();
        }
    }

    fn schedule(&self, worker: &Worker<PathBuf>, directory: PathBuf) {
        self.begin_task();
        worker.push(directory);
        self.wake.notify_all();
    }

    fn should_stop(&self) -> bool {
        self.cancellation.is_cancelled()
    }

    fn record_error(&self, operation: &'static str, path: PathBuf, source: std::io::Error) {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => {
                let mut abort_error = lock(&self.abort_error);
                if abort_error.is_none() {
                    *abort_error = Some(error);
                    self.cancellation.cancel();
                    self.wake.notify_all();
                }
            }
            ErrorPolicy::Skip => {}
            ErrorPolicy::Collect => lock(&self.errors).push(error),
        }
    }

    fn record_startup_error(&self, source: std::io::Error) {
        let mut startup_error = lock(&self.startup_error);
        if startup_error.is_none() {
            *startup_error = Some(WalkError::new(
                "spawn_worker",
                self.walker.root.clone(),
                source,
            ));
            self.cancellation.cancel();
            self.wake.notify_all();
        }
    }

    fn record_panic(&self, payload: Box<dyn Any + Send + 'static>) {
        let mut panic = lock(&self.panic);
        if panic.is_none() {
            *panic = Some(payload);
            self.cancellation.cancel();
            self.wake.notify_all();
        }
    }
}

struct WorkerScratch {
    queue: Worker<PathBuf>,
    gitignore_cache: HashMap<PathBuf, Arc<GitIgnoreNode>>,
    entries: Vec<WalkEntry>,
}

impl WorkerScratch {
    fn new() -> Self {
        Self {
            queue: Worker::new_fifo(),
            gitignore_cache: HashMap::new(),
            entries: Vec::new(),
        }
    }

    fn flush_into(&self, scheduler: &Scheduler<PathBuf>) {
        while let Some(directory) = self.queue.pop() {
            scheduler.push(directory);
        }
    }
}

fn run_worker_catching_panics(
    shared: &Shared,
    worker: &mut WorkerScratch,
    stealers: &[Stealer<PathBuf>],
) {
    catch_worker_panic(shared, || run_worker(shared, worker, stealers));
}

fn catch_worker_panic(shared: &Shared, work: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(work)) {
        shared.record_panic(payload);
    }
}

fn run_worker(shared: &Shared, worker: &mut WorkerScratch, stealers: &[Stealer<PathBuf>]) {
    while let Some(directory) = next_task(shared, worker, stealers) {
        if !shared.should_stop() {
            process_directory(shared, worker, directory);
        }
        shared.finish_task();
    }
}

fn next_task(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<PathBuf>],
) -> Option<PathBuf> {
    loop {
        if let Some(task) = try_take(shared, worker, stealers) {
            return Some(task);
        }
        let guard = lock(&shared.wake_lock);
        if shared.pending.load(Ordering::Acquire) == 0 {
            return None;
        }
        if let Some(task) = try_take(shared, worker, stealers) {
            return Some(task);
        }
        let _ = shared
            .wake
            .wait_timeout(guard, CANCELLATION_POLL_INTERVAL)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
    }
}

fn try_take(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<PathBuf>],
) -> Option<PathBuf> {
    worker
        .queue
        .pop()
        .or_else(|| shared.scheduler.steal_into(&worker.queue))
        .or_else(|| {
            stealers.iter().find_map(|stealer| {
                loop {
                    match stealer.steal() {
                        Steal::Success(task) => break Some(task),
                        Steal::Empty => break None,
                        Steal::Retry => continue,
                    }
                }
            })
        })
}

fn process_directory(shared: &Shared, worker: &mut WorkerScratch, directory: PathBuf) {
    if shared.should_stop() {
        return;
    }
    if shared.walker.options.follow_symlinks && !mark_directory(shared, &directory) {
        return;
    }
    let entries = match StdBackend.read_directory(&directory) {
        Ok(entries) => entries,
        Err(source) => {
            shared.record_error("read_dir", directory, source);
            return;
        }
    };
    for entry in entries {
        if shared.should_stop() {
            return;
        }
        process_entry(shared, worker, entry);
    }
}

fn mark_directory(shared: &Shared, directory: &PathBuf) -> bool {
    match std::fs::canonicalize(directory) {
        Ok(canonical) => lock(&shared.visited_directories).insert(canonical),
        Err(source) => {
            shared.record_error("canonicalize", directory.clone(), source);
            false
        }
    }
}

fn process_entry(shared: &Shared, worker: &mut WorkerScratch, mut entry: BackendEntry) {
    let relative = entry
        .path
        .strip_prefix(&shared.walker.root)
        .unwrap_or(entry.path.as_path());
    let bytes = relative.as_os_str().as_encoded_bytes();
    if shared.walker.options.skip_hidden && has_hidden_component(bytes) {
        return;
    }
    if shared
        .walker
        .excludes
        .iter()
        .any(|pattern| pattern.matches(bytes))
    {
        return;
    }
    let git_ignored = is_git_ignored(
        &shared.walker,
        &entry.path,
        entry.is_dir,
        &mut worker.gitignore_cache,
    );
    if git_ignored && !entry.is_dir {
        return;
    }
    if entry.is_symlink && shared.walker.options.follow_symlinks {
        match std::fs::metadata(&entry.path) {
            Ok(metadata) => entry.is_dir = metadata.is_dir(),
            Err(source) => {
                shared.record_error("metadata", entry.path, source);
                return;
            }
        }
    }
    if !entry.is_dir && !shared.walker.may_include_file(bytes) {
        return;
    }
    if entry.is_dir
        && !shared
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.covers_subtree(bytes))
        && shared.walker.may_descend_into(bytes)
    {
        shared.schedule(&worker.queue, entry.path.clone());
    }
    if !shared.walker.includes.is_empty()
        && !shared
            .walker
            .includes
            .iter()
            .any(|pattern| pattern.matches(bytes))
    {
        return;
    }
    if git_ignored {
        return;
    }
    if shared.walker.options.directories_only && !entry.is_dir {
        return;
    }
    let metadata = if shared.walker.options.metadata {
        match std::fs::symlink_metadata(&entry.path) {
            Ok(metadata) => Some(metadata),
            Err(source) => {
                shared.record_error("symlink_metadata", entry.path, source);
                return;
            }
        }
    } else {
        None
    };
    worker.entries.push(WalkEntry {
        path: entry.path,
        is_dir: entry.is_dir,
        metadata,
    });
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_WORKER_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

#[cfg(test)]
fn fail_next_worker_spawn() {
    FAIL_NEXT_WORKER_SPAWN.with(|failure| failure.set(true));
}

#[cfg(test)]
fn should_fail_next_worker_spawn() -> bool {
    FAIL_NEXT_WORKER_SPAWN.with(std::cell::Cell::take)
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
        sync::Arc,
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::{Shared, Walker, catch_worker_panic, fail_next_worker_spawn, finish};
    use crate::CancellationToken;

    #[test]
    fn worker_panic_cancels_siblings_and_resumes_on_the_caller() {
        let cancellation = CancellationToken::default();
        let walker = Arc::new(Walker::new(".").cancellation(cancellation.clone()));
        let shared = Arc::new(Shared::new(walker));

        catch_worker_panic(&shared, || panic!("injected worker panic"));
        assert!(cancellation.is_cancelled());
        assert!(catch_unwind(AssertUnwindSafe(|| finish(shared, Vec::new()))).is_err());
    }

    #[test]
    fn worker_start_failure_returns_a_structured_error_and_cancels() {
        let root = std::env::temp_dir().join(format!(
            "ferralk-worker-start-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        ));
        fs::create_dir_all(root.join("left")).expect("create left fixture");
        fs::create_dir_all(root.join("right")).expect("create right fixture");
        let cancellation = CancellationToken::default();
        fail_next_worker_spawn();

        let error = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect_err("injected worker start failure is returned");
        let _ = fs::remove_dir_all(&root);

        assert_eq!(error.operation(), "spawn_worker");
        assert_eq!(error.path(), PathBuf::from(&root));
        assert!(cancellation.is_cancelled());
    }
}
