//! Parallel `collect()` implementation built on the crate-local scheduler.

use std::{
    any::Any,
    collections::HashSet,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

use crossbeam_deque::{Steal, Stealer, Worker};

use super::{
    CYCLE_KEY_OPERATION, CycleKey, DirectoryBackend, EntryVisitor, ErrorPolicy, Listing, Verdict,
    WalkEntry, WalkError, WalkResult, Walker,
    classify::{DirectoryTask, EmittedEntry, EntryAction, classify_entry},
    gitignore::IgnoreScope,
    own_path,
    scheduler::{Coordinator, Scheduler, WorkerSlot},
};

pub(super) fn collect<B: DirectoryBackend + Sync>(
    walker: Walker,
    backend: &B,
    visitor: EntryVisitor<'_>,
) -> Result<WalkResult, WalkError> {
    let walker = Arc::new(walker);
    let shared = Arc::new(Shared::new(Arc::clone(&walker), backend, visitor));
    let mut caller = WorkerScratch::new(0);
    let caller_slot = shared.coordinator.claim_caller_slot();

    // The caller starts alone. Helpers are created only after the root made
    // parallel directory work available, as required by ADR-0009.
    shared.coordinator.begin_task();
    {
        let _root_task = shared.coordinator.claim_task();
        let root = DirectoryTask {
            path: walker.root.clone(),
            depth: 0,
            ignores: IgnoreScope::root(&walker, backend),
        };
        process_directory(&shared, &mut caller, root);
    }
    caller.flush_into(&shared.scheduler);

    if shared.coordinator.pending() == 0 {
        drop(caller_slot);
        return finish(shared, caller.entries);
    }

    // Every helper slot is built here so the stealer list is complete and stays
    // lock-free once the walk runs. Only the threads are lazy: a slot stays
    // unused until the backlog outgrows the workers that are already running,
    // which is what lets a tree that only fans out below the root widen too.
    let spare = (1..walker.threads.max(1))
        .map(WorkerScratch::new)
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
    drop(caller_slot);
    finish(shared, entries)
}

/// Hands out the helper slots of one walk. Any worker may grow the pool, so a
/// subtree that only becomes parallel below the root still reaches the
/// configured thread budget instead of being capped by the root fan-out.
#[derive(Clone, Copy)]
struct HelperPool<'scope, 'env> {
    scope: &'scope thread::Scope<'scope, 'env>,
    shared: &'env Arc<Shared<'env>>,
    stealers: &'env [Stealer<DirectoryTask>],
    idle: &'env Mutex<Vec<WorkerScratch>>,
    shards: &'env Mutex<Vec<Vec<WalkEntry>>>,
}

/// Queued directories that make a pool worth starting, once the tree has shown
/// it is not trivial.
///
/// This is the signal #64 measured and it is unchanged: work waiting right now
/// is what a helper can pick up. What it lacked was any notion of how much
/// work, which is what [`HELPER_WORK_FLOOR`] adds.
const HELPER_QUEUE_FLOOR: usize = 8;

/// Directory entries a walk must have seen before a queue counts as worth
/// paying a thread for.
///
/// Starting a thread costs more than a small tree does: the Palamedes trial
/// measured every parallel arm losing to its own serial form on a twelve-file
/// tree, where `ignore` at four threads ran 0.38x its own serial time. Its
/// sixteen-directory, twelve-file tree cleared the old queue floor at the root -
/// sixteen directories queued at once - and started a pool with nothing to
/// give it, running at 0.76x its own serial time.
///
/// Entries, not directories, because entries are what there is to do. The
/// trial's tree holds 28 of them in total, so a floor above that keeps it
/// serial, while a tree that fans out for real reaches the floor from its root
/// listing alone.
const HELPER_WORK_FLOOR: usize = 32;

/// Entries seen from which a walk is worth helpers whatever its queue looks
/// like.
///
/// A handful of directories holding very many files each never queues much: the
/// old floor left three directories of eighty thousand files running at 1.00x
/// its own serial time, an entire tree on one thread with three others idle.
/// One listing that large is worth splitting even though nothing else is
/// waiting, and by then a thread costs nothing against it.
const HELPER_LISTING_FLOOR: usize = 1024;

impl<'scope, 'env> HelperPool<'scope, 'env> {
    /// Starts helpers while queued directories outnumber the running workers
    /// and the thread budget has room. Cheap enough to call after every
    /// directory: a full pool costs the two atomic loads that reject the claim.
    fn grow(self) {
        while !self.shared.should_stop() {
            if !self.shared.tree_is_worth_helpers() {
                return;
            }
            let Some(slot) = self
                .shared
                .coordinator
                .claim_worker_slot(self.shared.walker.threads)
            else {
                return;
            };
            // Dropping the slot here releases it again for a later attempt.
            let Some(scratch) = lock(self.idle).pop() else {
                return;
            };
            if !self.spawn(slot, scratch) {
                return;
            }
        }
    }

    /// Reports whether the pool may keep growing after this attempt.
    fn spawn(self, slot: WorkerSlot<'env>, scratch: WorkerScratch) -> bool {
        #[cfg(test)]
        if should_fail_next_worker_spawn() {
            self.shared
                .record_startup_error(std::io::Error::other("injected worker start failure"));
            lock(self.idle).push(scratch);
            return false;
        }
        let spawn = thread::Builder::new()
            .name("ferralk-worker".into())
            .spawn_scoped(self.scope, move || {
                // The slot rides along so it is released when this helper ends,
                // however it ends.
                let _slot = slot;
                let mut scratch = scratch;
                run_worker_catching_panics(self, &mut scratch);
                lock(self.shards).push(std::mem::take(&mut scratch.entries));
            });
        match spawn {
            // The scope joins the helper, so the handle is not needed here.
            Ok(_) => true,
            // The closure, and with it the slot, is dropped on failure.
            Err(source) => {
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
        cancelled: shared.should_stop(),
    })
}

struct Shared<'backend> {
    walker: Arc<Walker>,
    /// The caller's per-entry filter, run on whichever worker produced the
    /// entry. Shared rather than cloned, which is why it is `Sync`.
    visitor: EntryVisitor<'backend>,
    /// Set by a [`Verdict::Stop`]. Kept apart from `cancellation` so a stop
    /// never reaches into a token the caller owns and may reuse.
    stopped: AtomicBool,
    /// Directory entries seen so far, the size signal behind
    /// [`HELPER_WORK_FLOOR`]. Counted per listing rather than per entry, so a
    /// directory costs one atomic add however much it holds.
    entries_seen: AtomicUsize,
    /// Every filesystem call of the walk goes through here, which is what lets
    /// a test mock drive the parallel frontend the same way it drives the
    /// serial one.
    backend: &'backend (dyn DirectoryBackend + Sync),
    scheduler: Scheduler<DirectoryTask>,
    /// Task accounting, worker slots and the parking protocol.
    coordinator: Coordinator,
    cancellation: super::CancellationToken,
    errors: Mutex<Vec<WalkError>>,
    abort_error: Mutex<Option<WalkError>>,
    startup_error: Mutex<Option<WalkError>>,
    panic: Mutex<Option<Box<dyn Any + Send + 'static>>>,
    /// The follow-symlinks guard. One mutex rather than shards: the key is
    /// sixteen `Copy` bytes, so the critical section is a hash and an insert,
    /// and the measurement in the pull request found sharding unmeasurable
    /// against a walk that is dominated by syscalls.
    visited_directories: Mutex<HashSet<CycleKey>>,
}

impl<'backend> Shared<'backend> {
    fn new(
        walker: Arc<Walker>,
        backend: &'backend (dyn DirectoryBackend + Sync),
        visitor: EntryVisitor<'backend>,
    ) -> Self {
        let cancellation = walker.cancellation.clone().unwrap_or_default();
        Self {
            walker,
            visitor,
            stopped: AtomicBool::new(false),
            entries_seen: AtomicUsize::new(0),
            backend,
            scheduler: Scheduler::new(),
            coordinator: Coordinator::new(),
            cancellation,
            errors: Mutex::new(Vec::new()),
            abort_error: Mutex::new(None),
            startup_error: Mutex::new(None),
            panic: Mutex::new(None),
            visited_directories: Mutex::new(HashSet::new()),
        }
    }

    fn schedule(&self, worker: &Worker<DirectoryTask>, task: DirectoryTask) {
        self.coordinator.begin_task();
        worker.push(task);
        self.coordinator.wake_waiters();
    }

    /// Whether the tree has shown enough work to be worth a helper.
    ///
    /// Queued work says a helper would have something to pick up; entries say
    /// the tree is large enough for that to be worth a thread. Both together
    /// is the usual path. A single listing past [`HELPER_LISTING_FLOOR`] is
    /// enough on its own, because a tree that wide in one directory never
    /// queues much and would otherwise never spawn at all.
    fn tree_is_worth_helpers(&self) -> bool {
        let entries = self.entries_seen.load(Ordering::Acquire);
        (entries >= HELPER_WORK_FLOOR && self.coordinator.pending() >= HELPER_QUEUE_FLOOR)
            || entries >= HELPER_LISTING_FLOOR
    }

    fn should_stop(&self) -> bool {
        self.cancellation.is_cancelled() || self.stopped.load(Ordering::Acquire)
    }

    /// Runs the visitor and keeps the entry if it survived.
    ///
    /// A [`Verdict::Stop`] ends the walk the way cancellation does, waking the
    /// parked workers so none of them sits out the rest of the walk.
    ///
    /// The entry's path is copied out of the worker's scratch here, into the
    /// buffer the last dropped entry gave back — so a worker whose visitor
    /// keeps nothing runs without allocating.
    fn emit(&self, worker: &mut WorkerScratch, emitted: EmittedEntry) {
        let WorkerScratch {
            entries,
            path,
            spare,
            ..
        } = worker;
        let entry = emitted.with_path(own_path(spare, path));
        match (self.visitor)(&entry) {
            Verdict::Keep => entries.push(entry),
            Verdict::Skip => *spare = entry.path,
            Verdict::Stop => {
                *spare = entry.path;
                self.stopped.store(true, Ordering::Release);
                self.coordinator.wake_waiters();
            }
        }
    }

    fn record_error(&self, operation: &'static str, path: PathBuf, source: std::io::Error) {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => {
                let mut abort_error = lock(&self.abort_error);
                if abort_error.is_none() {
                    *abort_error = Some(error);
                    self.cancellation.cancel();
                    self.coordinator.wake_waiters();
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
            self.coordinator.wake_waiters();
        }
    }

    fn record_panic(&self, payload: Box<dyn Any + Send + 'static>) {
        let mut panic = lock(&self.panic);
        if panic.is_none() {
            *panic = Some(payload);
            self.cancellation.cancel();
            self.coordinator.wake_waiters();
        }
    }
}

struct WorkerScratch {
    /// Position of this worker's stealer in the shared stealer list, so the
    /// idle scan can start next door and skip its own queue.
    index: usize,
    queue: Worker<DirectoryTask>,
    entries: Vec<WalkEntry>,
    /// The directory this worker is reading. It takes tasks off a queue rather
    /// than descending by recursion, so one directory is open at a time and
    /// one set of buffers is enough.
    listing: Listing,
    /// That directory's path, with the entry being classified pushed onto it.
    path: PathBuf,
    /// The path buffer the last dropped entry left behind. See [`own_path`].
    spare: PathBuf,
}

impl WorkerScratch {
    fn new(index: usize) -> Self {
        Self {
            index,
            queue: Worker::new_fifo(),
            entries: Vec::new(),
            listing: Listing::default(),
            path: PathBuf::new(),
            spare: PathBuf::new(),
        }
    }

    fn flush_into(&self, scheduler: &Scheduler<DirectoryTask>) {
        while let Some(task) = self.queue.pop() {
            scheduler.push(task);
        }
    }
}

fn run_worker_catching_panics(pool: HelperPool<'_, '_>, worker: &mut WorkerScratch) {
    catch_worker_panic(pool.shared, || run_worker(pool, worker));
}

fn catch_worker_panic(shared: &Shared, work: impl FnOnce()) {
    if let Err(payload) = catch_unwind(AssertUnwindSafe(work)) {
        shared.record_panic(payload);
    }
}

fn run_worker(pool: HelperPool<'_, '_>, worker: &mut WorkerScratch) {
    let shared = pool.shared;
    while let Some(directory) = next_task(shared, worker, pool.stealers) {
        let _task = shared.coordinator.claim_task();
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

/// Waits for the next directory. Cancellation ends the search right away:
/// queued tasks are plain paths, so leaving them behind has no observable
/// effect and the walk stops without draining the rest of the tree.
fn next_task(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<DirectoryTask>],
) -> Option<DirectoryTask> {
    shared.coordinator.wait_for_task(
        || shared.should_stop(),
        || try_take(shared, worker, stealers),
    )
}

fn try_take(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<DirectoryTask>],
) -> Option<DirectoryTask> {
    worker
        .queue
        .pop()
        .or_else(|| shared.scheduler.steal_into(&worker.queue))
        .or_else(|| {
            // Start next door and leave the own queue out: stealing from
            // itself can only come up empty, and the offset keeps idle workers
            // from all probing the same victim first.
            let (head, tail) = stealers.split_at((worker.index + 1).min(stealers.len()));
            tail.iter()
                .chain(head.iter().take(worker.index))
                .find_map(|stealer| {
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

fn process_directory(shared: &Shared, worker: &mut WorkerScratch, task: DirectoryTask) {
    let DirectoryTask {
        path,
        depth,
        ignores,
    } = task;
    #[cfg(test)]
    if should_panic_in_directory(&path) {
        panic!("injected directory panic");
    }
    if shared.should_stop() {
        return;
    }
    if shared.walker.options.follow_symlinks && !mark_directory(shared, &path) {
        return;
    }
    if let Err(source) = shared.backend.read_directory(&path, &mut worker.listing) {
        shared.record_error("read_dir", path, source);
        return;
    }
    shared
        .entries_seen
        .fetch_add(worker.listing.entries().len(), Ordering::AcqRel);
    // The directory's own ignore files join the chain here. Every directory is
    // processed once, so every ignore file is read once, whatever the worker
    // count.
    let ignores = ignores.enter(&shared.walker, shared.backend, &path, &worker.listing);
    worker.path.clear();
    worker.path.push(&path);
    for index in 0..worker.listing.entries().len() {
        if shared.should_stop() {
            return;
        }
        // The entry's path exists only for as long as it is being decided
        // about; anything that outlives that copies it out.
        worker.path.push(worker.listing.entries()[index].name());
        let action = classify_entry(
            &shared.walker,
            shared.backend,
            &worker.path,
            &worker.listing.entries()[index],
            &ignores,
            depth,
        );
        act(shared, worker, action);
        worker.path.pop();
    }
}

fn mark_directory(shared: &Shared, directory: &Path) -> bool {
    // The key is computed outside the lock, so the critical section holds only
    // the hash and the insert.
    match shared.backend.cycle_key(directory) {
        Ok(key) => lock(&shared.visited_directories).insert(key),
        Err(source) => {
            shared.record_error(CYCLE_KEY_OPERATION, directory.to_path_buf(), source);
            false
        }
    }
}

/// Carries out what classification decided about one entry. The entry's path
/// is still on the worker's scratch, which is where `emit` copies it from.
fn act(shared: &Shared, worker: &mut WorkerScratch, action: EntryAction) {
    match action {
        EntryAction::Skip => {}
        EntryAction::Descend(task) => shared.schedule(&worker.queue, task),
        EntryAction::DescendAndEmit(entry, task) => {
            shared.schedule(&worker.queue, task);
            shared.emit(worker, entry);
        }
        EntryAction::Emit(entry) => shared.emit(worker, entry),
        EntryAction::Failed { failure, descend } => {
            if let Some(task) = descend {
                shared.schedule(&worker.queue, task);
            }
            shared.record_error(failure.operation, failure.path, failure.source);
        }
    }
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

/// Held for the whole of any test that uses the rendezvous.
///
/// The rendezvous is one process-global slot and `observed_worker_threads`
/// takes it, so two tests arming it at once would read each other's walk — or
/// an empty slot. libtest runs tests on parallel threads, so that is the
/// default rather than the exception once more than one test uses it.
#[cfg(test)]
static WORKER_RENDEZVOUS_GUARD: Mutex<()> = Mutex::new(());

#[cfg(test)]
static WORKER_RENDEZVOUS_WAKE: std::sync::Condvar = std::sync::Condvar::new();

/// Bounds the barrier so a walk that stays narrow fails the assertion instead
/// of blocking the suite.
#[cfg(test)]
const WORKER_RENDEZVOUS_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Re-checks the barrier often enough that its own bound stays responsive.
#[cfg(test)]
const WORKER_RENDEZVOUS_POLL: std::time::Duration = std::time::Duration::from_millis(10);

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
    // Below the size floor the walk is deliberately serial, so a rendezvous
    // there would block the one worker that still has to read its way past the
    // floor. Standing aside on exactly the production condition keeps the
    // harness measuring the pool rather than the floor.
    if !shared.tree_is_worth_helpers() {
        return;
    }
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
            .wait_timeout(state, WORKER_RENDEZVOUS_POLL)
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state = resumed;
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
        path::{Path, PathBuf},
        sync::{
            Arc, Mutex,
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        Shared, WORKER_RENDEZVOUS_GUARD, Walker, catch_worker_panic, expect_worker_threads,
        fail_next_worker_spawn, finish, lock, observed_worker_threads, panic_in_directory,
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
    /// A tree wide enough to be worth threads.
    ///
    /// It holds far more entries than [`super::HELPER_WORK_FLOOR`] on purpose:
    /// the walk stays serial below the floor, so a pool test on a smaller tree
    /// would be asserting against a configuration the walk never uses.
    fn create_wide_fixture(root: &Path) {
        for branch in 0..10 {
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

    /// What the floor is for, in the two directions it used to get wrong.
    ///
    /// A queue on its own says nothing about size: the Palamedes trial's tree
    /// queues sixteen directories at its root and holds twelve files in total.
    /// A size on its own says nothing about width: three directories of many
    /// files each never queue enough to look worth it, and used to walk on one
    /// thread whatever the budget said.
    #[test]
    fn the_floor_weighs_work_rather_than_directories() {
        let walker = Arc::new(Walker::new("."));
        let shared = Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        );

        // The trial's shape: everything it has, queued at once, is still
        // nothing to do.
        shared.entries_seen.store(28, Ordering::Release);
        for _ in 0..16 {
            shared.coordinator.begin_task();
        }
        assert!(
            !shared.tree_is_worth_helpers(),
            "a wide but trivial tree must not start a pool"
        );

        // The same queue, on a tree that turned out to have work in it.
        shared
            .entries_seen
            .store(super::HELPER_WORK_FLOOR, Ordering::Release);
        assert!(shared.tree_is_worth_helpers());

        // One listing large enough to be worth splitting, with almost nothing
        // queued behind it.
        let shared = Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        );
        shared
            .entries_seen
            .store(super::HELPER_LISTING_FLOOR, Ordering::Release);
        shared.coordinator.begin_task();
        assert!(
            shared.tree_is_worth_helpers(),
            "a single huge directory is worth helpers even with an empty queue"
        );
    }

    #[test]
    fn worker_panic_cancels_siblings_and_resumes_on_the_caller() {
        let cancellation = CancellationToken::default();
        let walker = Arc::new(Walker::new(".").cancellation(cancellation.clone()));
        let shared = Arc::new(Shared::new(
            walker,
            &crate::SystemBackend,
            &crate::keep_every_entry,
        ));

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
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
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
    fn a_visitor_runs_on_every_worker_of_the_walk() {
        // The point of the API: a caller's predicate must not be funnelled back
        // onto one thread. The rendezvous is what makes that observable without
        // racing the pool — a plain thread-id count would pass or fail on how
        // quickly the caller drains a small tree.
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
        let root = unique_root("visitor-threads");
        create_wide_fixture(&root);

        expect_worker_threads(root.clone(), 4);
        let seen = Mutex::new(HashSet::new());
        let result = Walker::new(&root)
            .threads(4)
            .visit(|_| {
                lock(&seen).insert(thread::current().id());
                crate::Verdict::Keep
            })
            .expect("visited walk succeeds");
        let workers = observed_worker_threads();
        let visitor_threads = lock(&seen).len();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(workers, 4, "the walk did not reach its thread budget");
        assert!(
            visitor_threads > 1,
            "the visitor ran on {visitor_threads} thread(s) across {workers} workers"
        );
        assert!(!result.entries().is_empty());
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
        // Wide enough to reach the size floor, or no helper is ever attempted
        // and there is nothing for the injected failure to land on.
        create_wide_fixture(&root);
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
