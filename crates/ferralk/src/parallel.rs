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
    WalkResult, Walker, is_git_ignored, scheduler::Scheduler,
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
        return finish(shared);
    }
    let helper_count = walker
        .threads
        .saturating_sub(1)
        .min(pending.saturating_sub(1));
    if helper_count == 0 {
        run_worker(&shared, &mut caller, &[]);
        return finish(shared);
    }

    let mut helpers = (0..helper_count)
        .map(|_| WorkerScratch::new())
        .collect::<Vec<_>>();
    let mut stealers = Vec::with_capacity(helper_count + 1);
    stealers.push(caller.queue.stealer());
    stealers.extend(helpers.iter().map(|worker| worker.queue.stealer()));

    thread::scope(|scope| {
        let mut joins = Vec::with_capacity(helper_count);
        for mut helper in helpers.drain(..) {
            let shared = Arc::clone(&shared);
            let stealers = &stealers;
            joins.push(
                scope.spawn(move || run_worker_catching_panics(&shared, &mut helper, stealers)),
            );
        }
        run_worker_catching_panics(&shared, &mut caller, &stealers);
        for join in joins {
            // Panics are captured inside the worker so joining only waits for
            // completion and cannot bypass the shared cancellation state.
            join.join().expect("worker panic is captured before join");
        }
    });
    finish(shared)
}

fn finish(shared: Arc<Shared>) -> Result<WalkResult, WalkError> {
    if let Some(payload) = lock(&shared.panic).take() {
        resume_unwind(payload);
    }
    if let Some(error) = lock(&shared.abort_error).take() {
        return Err(error);
    }
    let mut entries = std::mem::take(&mut *lock(&shared.entries));
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
    entries: Mutex<Vec<WalkEntry>>,
    errors: Mutex<Vec<WalkError>>,
    abort_error: Mutex<Option<WalkError>>,
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
            entries: Mutex::new(Vec::new()),
            errors: Mutex::new(Vec::new()),
            abort_error: Mutex::new(None),
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
}

impl WorkerScratch {
    fn new() -> Self {
        Self {
            queue: Worker::new_fifo(),
            gitignore_cache: HashMap::new(),
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
    if let Err(payload) = catch_unwind(AssertUnwindSafe(|| run_worker(shared, worker, stealers))) {
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
    if entry.is_dir
        && !shared
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.covers_subtree(bytes))
    {
        shared.schedule(&worker.queue, entry.path.clone());
    }
    if !shared.walker.includes.is_empty()
        && !shared
            .walker
            .includes
            .iter()
            .any(|pattern| pattern.is_match(bytes))
    {
        return;
    }
    if git_ignored {
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
    lock(&shared.entries).push(WalkEntry {
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
