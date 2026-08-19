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
    BackendEntry, DirectoryBackend, ErrorPolicy, GitIgnoreNode, SystemBackend, WalkEntry,
    WalkError, WalkResult, Walker, glob_path_bytes, has_hidden_component, is_git_ignored,
    scheduler::Scheduler, should_skip_git_directory,
};

const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(super) fn collect(walker: Walker) -> Result<WalkResult, WalkError> {
    let walker = Arc::new(walker);
    let shared = Arc::new(Shared::new(Arc::clone(&walker)));
    let mut caller = WorkerScratch::new();

    // The caller starts alone. Helpers are created only after the root made
    // parallel directory work available, as required by ADR-0009.
    shared.begin_task();
    {
        let _root_task = TaskGuard::claim(&shared);
        process_directory(&shared, &mut caller, walker.root.clone());
    }
    caller.flush_into(&shared.scheduler);

    if shared.pending.load(Ordering::Acquire) == 0 {
        return finish(shared, caller.entries);
    }

    // Every helper slot is built here so the stealer list is complete and stays
    // lock-free once the walk runs. Only the threads are lazy: a slot stays
    // unused until the backlog outgrows the workers that are already running,
    // which is what lets a tree that only fans out below the root widen too.
    let spare = (0..walker.threads.saturating_sub(1))
        .map(|_| WorkerScratch::new())
        .collect::<Vec<_>>();
    let mut stealers = Vec::with_capacity(spare.len() + 1);
    stealers.push(caller.queue.stealer());
    stealers.extend(spare.iter().map(|worker| worker.queue.stealer()));
    let idle = Mutex::new(spare);
    let shards = Mutex::new(Vec::new());

    let mut entries = thread::scope(|scope| {
        let pool = HelperPool {
            scope,
            shared: &shared,
            stealers: &stealers,
            idle: &idle,
            shards: &shards,
        };
        pool.grow();
        run_worker_catching_panics(pool, &mut caller);
        std::mem::take(&mut caller.entries)
    });
    // The scope joined every helper, so the shards are complete. Panics are
    // captured inside each worker and cannot bypass the shared cancellation
    // state, so no join here can observe one.
    for shard in lock(&shards).drain(..) {
        entries.extend(shard);
    }
    finish(shared, entries)
}

/// Hands out the helper slots of one walk. Any worker may grow the pool, so a
/// subtree that only becomes parallel below the root still reaches the
/// configured thread budget instead of being capped by the root fan-out.
#[derive(Clone, Copy)]
struct HelperPool<'scope, 'env> {
    scope: &'scope thread::Scope<'scope, 'env>,
    shared: &'env Arc<Shared>,
    stealers: &'env [Stealer<PathBuf>],
    idle: &'env Mutex<Vec<WorkerScratch>>,
    shards: &'env Mutex<Vec<Vec<WalkEntry>>>,
}

impl<'scope, 'env> HelperPool<'scope, 'env> {
    /// Starts helpers while queued directories outnumber the running workers
    /// and the thread budget has room. Cheap enough to call after every
    /// directory: the common case is two atomic loads.
    fn grow(self) {
        while self.needs_another_worker() {
            let Some(scratch) = lock(self.idle).pop() else {
                return;
            };
            if !self.spawn(scratch) {
                return;
            }
        }
    }

    fn needs_another_worker(self) -> bool {
        if self.shared.should_stop() {
            return false;
        }
        let active = self.shared.active_workers.load(Ordering::Acquire);
        active < self.shared.walker.threads && self.shared.pending.load(Ordering::Acquire) > active
    }

    /// Reports whether the pool may keep growing after this attempt.
    fn spawn(self, scratch: WorkerScratch) -> bool {
        #[cfg(test)]
        if should_fail_next_worker_spawn() {
            self.shared
                .record_startup_error(std::io::Error::other("injected worker start failure"));
            lock(self.idle).push(scratch);
            return false;
        }
        self.shared.active_workers.fetch_add(1, Ordering::AcqRel);
        let spawn = thread::Builder::new()
            .name("ferralk-worker".into())
            .spawn_scoped(self.scope, move || {
                let mut scratch = scratch;
                run_worker_catching_panics(self, &mut scratch);
                lock(self.shards).push(std::mem::take(&mut scratch.entries));
            });
        match spawn {
            // The scope joins the helper, so the handle is not needed here.
            Ok(_) => true,
            Err(source) => {
                self.shared.active_workers.fetch_sub(1, Ordering::AcqRel);
                self.shared.record_startup_error(source);
                false
            }
        }
    }
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
    /// Workers currently running, the caller included. Compared against the
    /// pending count to decide whether another helper would find work.
    active_workers: AtomicUsize,
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
            active_workers: AtomicUsize::new(1),
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

/// Owns one in-flight task that `begin_task` already counted and releases it
/// on drop, so a panic inside `process_directory` cannot strand the pending
/// count and leave the sibling workers waiting for work that never completes.
struct TaskGuard<'a> {
    shared: &'a Shared,
}

impl<'a> TaskGuard<'a> {
    fn claim(shared: &'a Shared) -> Self {
        Self { shared }
    }
}

impl Drop for TaskGuard<'_> {
    fn drop(&mut self) {
        self.shared.finish_task();
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

fn run_worker_catching_panics(pool: HelperPool<'_, '_>, worker: &mut WorkerScratch) {
    catch_worker_panic(pool.shared, || run_worker(pool, worker));
    pool.shared.active_workers.fetch_sub(1, Ordering::AcqRel);
}

fn catch_worker_panic(shared: &Shared, work: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(work)) {
        shared.record_panic(payload);
    }
}

fn run_worker(pool: HelperPool<'_, '_>, worker: &mut WorkerScratch) {
    let shared = pool.shared;
    while let Some(directory) = next_task(shared, worker, pool.stealers) {
        let _task = TaskGuard::claim(shared);
        if !shared.should_stop() {
            process_directory(shared, worker, directory);
            // The directory may have opened the tree up, so re-check whether
            // the walk can use more of the configured thread budget.
            pool.grow();
        }
        #[cfg(test)]
        join_worker_rendezvous(shared);
    }
}

fn next_task(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<PathBuf>],
) -> Option<PathBuf> {
    loop {
        // Cancellation ends the search right away. Queued tasks are plain
        // paths, so leaving them behind has no observable effect, and workers
        // stop within one poll interval instead of draining the whole tree.
        if shared.should_stop() {
            return None;
        }
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
    #[cfg(test)]
    if should_panic_in_directory(&directory) {
        panic!("injected directory panic");
    }
    if shared.should_stop() {
        return;
    }
    if shared.walker.options.follow_symlinks && !mark_directory(shared, &directory) {
        return;
    }
    let entries = match SystemBackend.read_directory(&directory) {
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
    let depth = relative.components().count();
    if !shared.walker.includes_depth(relative) {
        return;
    }
    let bytes = glob_path_bytes(relative);
    if shared.walker.options.skip_hidden && has_hidden_component(bytes.as_ref()) {
        return;
    }
    if should_skip_git_directory(&shared.walker, &entry.path) {
        return;
    }
    if shared
        .walker
        .excludes
        .iter()
        .any(|pattern| pattern.matches(bytes.as_ref(), entry.is_dir))
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
    if !entry.is_dir && !shared.walker.may_include_file(bytes.as_ref()) {
        return;
    }
    if entry.is_dir
        && !shared
            .walker
            .excludes
            .iter()
            .any(|pattern| pattern.covers_subtree(bytes.as_ref()))
        && shared.walker.may_descend_path(relative, bytes.as_ref())
    {
        shared.schedule(&worker.queue, entry.path.clone());
    }
    if !shared.walker.includes.is_empty()
        && !shared
            .walker
            .includes
            .iter()
            .any(|pattern| pattern.matches(bytes.as_ref(), entry.is_dir))
    {
        return;
    }
    if git_ignored {
        return;
    }
    if shared.walker.options.directories_only && !entry.is_dir {
        return;
    }
    if shared.walker.options.files_only && entry.is_dir {
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
        is_symlink: entry.is_symlink,
        depth,
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

/// Directory whose traversal panics once, on whichever worker picks it up.
/// The trigger is process-wide because helper threads own their tasks, and it
/// matches one absolute path so concurrent tests stay independent.
#[cfg(test)]
static PANIC_IN_DIRECTORY: Mutex<Option<PathBuf>> = Mutex::new(None);

#[cfg(test)]
fn panic_in_directory(directory: PathBuf) {
    *lock(&PANIC_IN_DIRECTORY) = Some(directory);
}

#[cfg(test)]
fn should_panic_in_directory(directory: &std::path::Path) -> bool {
    let mut target = lock(&PANIC_IN_DIRECTORY);
    if target.as_deref() == Some(directory) {
        *target = None;
        return true;
    }
    false
}

/// Barrier that holds every worker of one walk after it processed a directory
/// until `expected` distinct threads have arrived. It turns "the walk really
/// went wide" into a structural assertion: no worker can race ahead and finish
/// the tree alone, so the observed width never depends on wall-clock timing.
#[cfg(test)]
struct WorkerRendezvous {
    root: PathBuf,
    expected: usize,
    threads: HashSet<std::thread::ThreadId>,
    released: bool,
}

#[cfg(test)]
static WORKER_RENDEZVOUS: Mutex<Option<WorkerRendezvous>> = Mutex::new(None);

#[cfg(test)]
static WORKER_RENDEZVOUS_WAKE: Condvar = Condvar::new();

/// Bounds the barrier so a walk that stays narrow fails the assertion instead
/// of blocking the suite.
#[cfg(test)]
const WORKER_RENDEZVOUS_TIMEOUT: Duration = Duration::from_secs(10);

#[cfg(test)]
fn expect_worker_threads(root: PathBuf, expected: usize) {
    *lock(&WORKER_RENDEZVOUS) = Some(WorkerRendezvous {
        root,
        expected,
        threads: HashSet::new(),
        released: false,
    });
}

#[cfg(test)]
fn observed_worker_threads() -> usize {
    lock(&WORKER_RENDEZVOUS)
        .take()
        .map_or(0, |rendezvous| rendezvous.threads.len())
}

#[cfg(test)]
fn join_worker_rendezvous(shared: &Shared) {
    let mut state = lock(&WORKER_RENDEZVOUS);
    match state.as_mut() {
        Some(rendezvous) if rendezvous.root == shared.walker.root && !rendezvous.released => {
            rendezvous.threads.insert(std::thread::current().id());
        }
        _ => return,
    }
    let deadline = std::time::Instant::now() + WORKER_RENDEZVOUS_TIMEOUT;
    loop {
        let Some(rendezvous) = state.as_mut() else {
            return;
        };
        let waited_long_enough = std::time::Instant::now() >= deadline;
        if rendezvous.released || rendezvous.threads.len() >= rendezvous.expected {
            rendezvous.released = true;
            WORKER_RENDEZVOUS_WAKE.notify_all();
            return;
        }
        if waited_long_enough {
            // The walk never widened. Release everyone so the test reports the
            // observed width rather than hanging.
            rendezvous.released = true;
            WORKER_RENDEZVOUS_WAKE.notify_all();
            return;
        }
        let (resumed, _) = WORKER_RENDEZVOUS_WAKE
            .wait_timeout(state, CANCELLATION_POLL_INTERVAL)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = resumed;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        Shared, Walker, catch_worker_panic, expect_worker_threads, fail_next_worker_spawn, finish,
        observed_worker_threads, panic_in_directory,
    };
    use crate::{CancellationToken, WalkOptions, WalkResult};

    /// A hung `collect()` is the regression under test, so the assertion has to
    /// time out instead of blocking the suite forever.
    const RESUME_TIMEOUT: Duration = Duration::from_secs(30);

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

    fn unique_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "ferralk-{label}-{}-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos(),
            NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
        ))
    }

    /// Builds a tree wide enough to keep several helpers busy while one of them
    /// panics, so the surviving workers have to notice the cancellation.
    fn create_wide_fixture(root: &Path) {
        for branch in 0..6 {
            for nested in 0..4 {
                let directory = root
                    .join(format!("branch-{branch}"))
                    .join(format!("nested-{nested}"));
                fs::create_dir_all(&directory).expect("create fixture directory");
                for file in 0..4 {
                    fs::write(directory.join(format!("file-{file}.txt")), b"fixture")
                        .expect("write fixture file");
                }
            }
        }
    }

    #[test]
    fn worker_panic_cancels_siblings_and_resumes_on_the_caller() {
        let cancellation = CancellationToken::default();
        let walker = Arc::new(Walker::new(".").cancellation(cancellation.clone()));
        let shared = Arc::new(Shared::new(walker));

        catch_worker_panic(&shared, || panic!("injected worker panic"));
        assert!(cancellation.is_cancelled());
        assert!(catch_unwind(AssertUnwindSafe(|| finish(shared, Vec::new()))).is_err());
    }

    fn walked_paths(result: &WalkResult) -> Vec<PathBuf> {
        result
            .entries()
            .iter()
            .map(|entry| entry.path().to_path_buf())
            .collect()
    }

    #[test]
    fn a_single_root_subdirectory_still_uses_the_configured_threads() {
        let root = unique_root("single-subtree");
        // The root has exactly one traversable child, so the root fan-out that
        // used to size the helper pool is one and the walk stayed serial.
        create_wide_fixture(&root.join("only"));

        let serial = Walker::new(&root)
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("serial walk succeeds");

        expect_worker_threads(root.clone(), 4);
        let parallel = Walker::new(&root)
            .threads(4)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel walk succeeds");
        let observed = observed_worker_threads();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(
            observed, 4,
            "a subtree below a single root child must still use the thread budget"
        );
        assert_eq!(walked_paths(&parallel), walked_paths(&serial));
        assert!(parallel.errors().is_empty());
    }

    #[test]
    fn worker_panic_during_traversal_resumes_without_hanging_the_walk() {
        // Repeated rounds keep the panic landing on different workers, so a
        // stranded pending count would show up as a hang instead of flaking.
        for round in 0..4 {
            let root = unique_root("worker-panic");
            create_wide_fixture(&root);
            panic_in_directory(root.join("branch-3").join("nested-2"));

            let cancellation = CancellationToken::default();
            let walk_root = root.clone();
            let walk_cancellation = cancellation.clone();
            let (sender, receiver) = mpsc::channel();
            let runner = thread::Builder::new()
                .name("ferralk-panic-regression".into())
                .spawn(move || {
                    let outcome = catch_unwind(AssertUnwindSafe(|| {
                        Walker::new(&walk_root)
                            .threads(4)
                            .cancellation(walk_cancellation)
                            .collect()
                    }));
                    let _ = sender.send(outcome.is_err());
                })
                .expect("spawn the walking thread");

            let resumed = receiver.recv_timeout(RESUME_TIMEOUT);
            let _ = fs::remove_dir_all(&root);
            assert_eq!(
                resumed,
                Ok(true),
                "round {round}: the injected worker panic must resume on the caller"
            );
            runner.join().expect("the walking thread joins");
            assert!(
                cancellation.is_cancelled(),
                "round {round}: the panic must cancel the sibling workers"
            );
        }
    }

    #[test]
    fn worker_start_failure_returns_a_structured_error_and_cancels() {
        let root = unique_root("worker-start");
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
