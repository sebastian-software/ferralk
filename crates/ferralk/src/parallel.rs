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

    // The caller starts alone. Helpers are created only after the roots made
    // parallel directory work available, as required by ADR-0009. Several roots
    // are several initial tasks and nothing more: the caller reads them in
    // order, and whatever they uncover is ordinary work for the same pool.
    let roots = walker.root_tasks(backend);
    for root in roots {
        shared.coordinator.begin_task();
        let _root_task = shared.coordinator.claim_task();
        process_directory(&shared, &mut caller, root);
    }

    // A walk that never crosses the floor never builds any of what follows.
    // The caller keeps walking on its own queue instead, which is what a tree
    // too small for a thread should cost: the spare deques, the stealer list,
    // the two mutexes and `thread::scope` are all paid once per walk, and a
    // twelve-file tree paid them to spawn nothing.
    //
    caller.flush_into(&shared.scheduler);

    // Every root has been read by the time this starts, so the queue it drains
    // holds whatever all of them uncovered — a walk that never widens still
    // finishes every root, in the order the eager route read them.
    //
    // Panics are caught here the way `run_worker` catches them, because this
    // stands in for `run_worker` on the route that never widens. Reading the
    // roots above is outside it on both routes alike.
    let widened =
        catch_worker_panic(&shared, || drain_alone(&shared, &mut caller)).unwrap_or(false);
    if !widened {
        drop(caller_slot);
        return finish(shared, std::mem::take(&mut caller.entries));
    }
    shared.widen();

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

/// What one directory listing is worth in the units [`HELPER_WORK_FLOOR`]
/// counts, beside the entries it returned.
///
/// A directory costs a syscall; an entry costs a few pointer moves. Fitting a
/// cost model to the #88 sweep on this host put a listing at roughly 16us and
/// an entry at 0.8us, so a directory is worth about twenty entries. The same
/// ratio falls out of the sweep directly: hold the entry count at 150 and move
/// it between 2 and 75 directories, and the walk goes from 200us to 1374us -
/// 16us per directory added, with the entries unchanged.
///
/// This is what makes the floor a mixed signal rather than an entry count. The
/// two are the same thing on an ordinary tree, where entries arrive with the
/// directories that hold them, and come apart exactly where #88 found them
/// apart: a tree of empty directories has no entries but plenty to do.
const DIRECTORY_WEIGHT: usize = 20;

/// Work a walk must have seen before a queue counts as worth paying a thread
/// for, counted in entries with each listing worth [`DIRECTORY_WEIGHT`].
///
/// Starting a thread costs more than a small tree does: the Palamedes trial
/// measured every parallel arm losing to its own serial form on a twelve-file
/// tree, where `ignore` at four threads ran 0.38x its own serial time. Its
/// sixteen-directory, twelve-file tree cleared the old queue floor at the root -
/// sixteen directories queued at once - and started a pool with nothing to
/// give it, running at 0.76x its own serial time.
///
/// **The #88 sweep.** Thirty-six shapes, directory count against entries per
/// directory, each walked process-fresh with helpers forced off and forced on,
/// arms alternating every round. Pooling turns from loss to win at a strikingly
/// constant point: whatever the shape, once the serial walk would take about
/// 480us. In work units that lands between 370, the largest shape where serial
/// still wins, and 520, the smallest where pooling wins; 224 is the largest
/// value that trips on every shape the sweep says should pool while trapping
/// none that should not, because the queue has to still hold
/// [`HELPER_QUEUE_FLOOR`] directories at the moment the floor is met.
///
/// **What the sweep corrected.** The floor counted entries alone, which reads a
/// tree of empty directories as trivial: twenty-four to thirty-one empty
/// directories stayed serial although pooling won there by up to 26%, and
/// thirty-two pooled only because thirty-two names in the root listing are
/// thirty-two entries. Weighting the listing removes that cliff. It also stops
/// one shape the old floor pooled by mistake - fourteen directories of four
/// files, where serial wins by 4%.
///
/// **What it did not correct.** The pair in the issue, nine directories of
/// sixteen files against seventeen, was already decided correctly, and still is.
/// The old floor got that right for a reason worth writing down: the entry
/// floor never stood alone, and requiring [`HELPER_QUEUE_FLOOR`] directories
/// still queued when it is met already weighed directories, just implicitly.
///
/// Re-swept after #84 made a walked entry cheaper, on the theory that a
/// cheaper entry leaves less work to amortise a thread and should raise this.
/// It does not: these walks are dominated by the syscall each directory costs,
/// and #84 changed per-entry work rather than that.
const HELPER_WORK_FLOOR: usize = 224;

/// Work seen from which a walk is worth helpers whatever its queue looks like.
///
/// A handful of directories holding very many files each never queues much: the
/// old floor left three directories of eighty thousand files running at 1.00x
/// its own serial time, an entire tree on one thread with three others idle.
/// One listing that large is worth splitting even though nothing else is
/// waiting, and by then a thread costs nothing against it.
///
/// In work units since #88, so the listing itself counts towards it. At this
/// size that is a 2% difference and the constant keeps its meaning: one
/// directory holding about a thousand files.
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
    /// Whether the caller is still the only worker. See [`Shared::schedule`].
    lone: AtomicBool,
    /// Work seen so far, the size signal behind [`HELPER_WORK_FLOOR`]: entries
    /// plus [`DIRECTORY_WEIGHT`] for each listing they came from. Accumulated
    /// per listing rather than per entry, so a directory costs one atomic add
    /// however much it holds - and the weight rides along in that same add.
    work_seen: AtomicUsize,
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
            lone: AtomicBool::new(true),
            work_seen: AtomicUsize::new(0),
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
        // A walk that has not widened yet has nobody to wake, and the
        // notification is a lock and a broadcast for every directory it finds.
        // [`Shared::widen`] clears this before the first helper can exist, and
        // starting a thread synchronizes with it, so no helper can park without
        // having seen the flag already cleared.
        if !self.lone.load(Ordering::Relaxed) {
            self.coordinator.wake_waiters();
        }
    }

    /// Announces that this walk is about to have more than one worker.
    ///
    /// Called once, before the pool is built, so every notification a helper
    /// could need is sent from then on.
    fn widen(&self) {
        self.lone.store(false, Ordering::Relaxed);
    }

    /// Whether the tree has shown enough work to be worth a helper.
    ///
    /// Queued work says a helper would have something to pick up; work seen says
    /// the tree is large enough for that to be worth a thread. Both together is
    /// the usual path. A single listing past [`HELPER_LISTING_FLOOR`] is enough
    /// on its own, because a tree that wide in one directory never queues much
    /// and would otherwise never spawn at all.
    fn tree_is_worth_helpers(&self) -> bool {
        let work = self.work_seen.load(Ordering::Acquire);
        (work >= HELPER_WORK_FLOOR && self.coordinator.pending() >= HELPER_QUEUE_FLOOR)
            || work >= HELPER_LISTING_FLOOR
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
                self.walker
                    .roots()
                    .next()
                    .expect("a walk has a root")
                    .into(),
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

/// Runs `work`, recording a panic as this walk's rather than letting it escape.
/// `None` says the work panicked; [`finish`] resumes it on the caller.
fn catch_worker_panic<T>(shared: &Shared, work: impl FnOnce() -> T) -> Option<T> {
    match catch_unwind(AssertUnwindSafe(work)) {
        Ok(value) => Some(value),
        Err(payload) => {
            shared.record_panic(payload);
            None
        }
    }
}

/// Walks on the caller thread alone, for as long as the tree is too small to
/// be worth a helper.
///
/// Reports whether the walk should widen: `true` once the floor unlocks a
/// helper and there is work queued for one to take, `false` when the tree ran
/// out first and no thread was ever needed.
///
/// The floor is checked at the same two moments [`HelperPool::grow`] is called
/// on the parallel route — before the first directory and after every one —
/// so the two routes decide to widen on exactly the same evidence. Queued work
/// is required as well, which is the condition the eager route applied once
/// after the root: a helper with nothing to steal is a thread spent on joining
/// itself.
fn drain_alone(shared: &Shared, caller: &mut WorkerScratch) -> bool {
    loop {
        let has_work = !caller.queue.is_empty() || !shared.scheduler.is_empty();
        if has_work && shared.tree_is_worth_helpers() {
            return true;
        }
        let Some(directory) = caller
            .queue
            .pop()
            .or_else(|| shared.scheduler.steal_into(&caller.queue))
        else {
            return false;
        };
        let _task = shared.coordinator.claim_task();
        if shared.should_stop() {
            return false;
        }
        process_directory(shared, caller, directory);
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
        root,
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
    // One add for the whole listing: the entries it returned, and the listing
    // itself, which cost a syscall to get them.
    shared.work_seen.fetch_add(
        worker.listing.entries().len() + DIRECTORY_WEIGHT,
        Ordering::AcqRel,
    );
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
            root,
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
        Some(rendezvous)
            if shared.walker.roots().next() == Some(rendezvous.root.as_path())
                && !rendezvous.released =>
        {
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

    /// Replays what a two-level tree shows the floor, and reports whether it
    /// ever trips.
    ///
    /// The root listing queues `dirs` directories and is worth that many
    /// entries; each directory the caller then reads adds its own files and
    /// takes one off the queue. That draining is the point: the floor has to be
    /// met while directories are still waiting, which is what stops a tree from
    /// pooling once there is nothing left to steal.
    fn floor_trips_for(dirs: usize, per_dir: usize) -> bool {
        let walker = Arc::new(Walker::new("."));
        let shared = Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        );
        let mut queued = Vec::new();
        for _ in 0..dirs {
            shared.coordinator.begin_task();
            queued.push(shared.coordinator.claim_task());
        }
        shared
            .work_seen
            .store(dirs + super::DIRECTORY_WEIGHT, Ordering::Release);
        if shared.tree_is_worth_helpers() {
            return true;
        }
        while queued.pop().is_some() {
            shared
                .work_seen
                .fetch_add(per_dir + super::DIRECTORY_WEIGHT, Ordering::AcqRel);
            if shared.tree_is_worth_helpers() {
                return true;
            }
        }
        false
    }

    /// The dividing line the #88 sweep measured, pinned shape by shape.
    ///
    /// Each row was walked process-fresh with helpers forced off and forced on,
    /// arms alternating every round; the comment is what that measured. The
    /// rows without one sat inside the noise either way and are here to fix the
    /// boundary, not because a wrong answer would cost anything.
    #[test]
    fn the_floor_divides_where_the_sweep_says_it_should() {
        // Directories of files. Pooling loses on the small ones and wins once
        // there is enough of the tree to amortise a thread.
        for (dirs, per_dir, expected, note) in [
            (9, 16, false, "pooling loses 10%"),
            (17, 16, true, "pooling wins 8.5%"),
            (4, 16, false, "pooling loses 38%"),
            (8, 16, false, "pooling loses 10%"),
            (14, 16, true, "pooling wins 15%"),
            (10, 4, false, "pooling loses 15%"),
            (14, 4, false, "pooling loses 4%"),
            (20, 4, true, "pooling wins 11%"),
            (12, 1, false, "pooling loses 8%"),
            (24, 1, true, "pooling wins 15%"),
            (48, 1, true, "pooling wins 15%"),
        ] {
            assert_eq!(
                floor_trips_for(dirs, per_dir),
                expected,
                "{dirs} directories of {per_dir} files: {note}"
            );
        }

        // Empty directories, where the old entry-only floor read a tree with
        // real work in it as trivial and left it on one thread.
        for (dirs, expected, note) in [
            (16, false, "inside the noise"),
            (24, true, "pooling wins 9%, and used to be refused"),
            (28, true, "pooling wins 26%, and used to be refused"),
            (31, true, "pooling wins 5%, and used to be refused"),
            (40, true, "pooling wins 30%"),
            (96, true, "pooling wins 21%"),
        ] {
            assert_eq!(
                floor_trips_for(dirs, 0),
                expected,
                "{dirs} empty directories: {note}"
            );
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
    fn the_floor_weighs_the_tree_and_the_queue_together() {
        let walker = Arc::new(Walker::new("."));
        let shared = Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        );

        // The trial's shape: everything it has, queued at once, is still
        // nothing to do. Sixteen listings and twelve files is 332 work units,
        // which would clear the floor - except that reading them empties the
        // queue, so the two conditions are never true together. The store here
        // is the generous reading, one listing's worth.
        shared
            .work_seen
            .store(28 + super::DIRECTORY_WEIGHT, Ordering::Release);
        for _ in 0..16 {
            shared.coordinator.begin_task();
        }
        assert!(
            !shared.tree_is_worth_helpers(),
            "a wide but trivial tree must not start a pool"
        );

        // The same queue, on a tree that turned out to have work in it.
        shared
            .work_seen
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
            .work_seen
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

    /// Several roots share one pool, which is the point of the feature.
    ///
    /// The rendezvous makes that observable: it releases only once `expected`
    /// distinct threads have joined it, so reaching four across a three-root
    /// walk means the roots were worked by one set of helpers rather than by
    /// three pools taking turns.
    #[test]
    fn several_roots_are_walked_by_one_pool() {
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
        let root = unique_root("multi-root-pool");
        for name in ["alpha", "beta", "gamma"] {
            create_wide_fixture(&root.join(name));
        }
        let roots = ["alpha", "beta", "gamma"].map(|name| root.join(name));

        let serial = Walker::new(&roots[0])
            .add_root(&roots[1])
            .expect("root")
            .add_root(&roots[2])
            .expect("root")
            .threads(1)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("serial walk succeeds");

        expect_worker_threads(roots[0].clone(), 4);
        let parallel = Walker::new(&roots[0])
            .add_root(&roots[1])
            .expect("root")
            .add_root(&roots[2])
            .expect("root")
            .threads(4)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("parallel walk succeeds");
        let observed = observed_worker_threads();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(
            observed, 4,
            "three roots must share the walk's one thread budget"
        );
        assert_eq!(walked_paths(&parallel), walked_paths(&serial));
        assert!(parallel.errors().is_empty());
    }

    /// The helper floor counts the whole walk, not one root of it.
    ///
    /// Three tiny roots are still a tiny walk, and starting a pool for them
    /// would cost more than it saves - the same judgement #76 made for one
    /// root, applied to the sum rather than to whichever root came first.
    #[test]
    fn the_floor_counts_across_roots() {
        let walker = Arc::new(Walker::new("."));
        let shared = Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        );

        // Three roots' worth of a trial-sized tree, queued together.
        shared
            .work_seen
            .store(12 + super::DIRECTORY_WEIGHT, Ordering::Release);
        for _ in 0..3 {
            shared.coordinator.begin_task();
        }
        assert!(
            !shared.tree_is_worth_helpers(),
            "three tiny roots are still a tiny walk"
        );

        // The same three roots once they turn out to hold work between them.
        shared
            .work_seen
            .store(super::HELPER_WORK_FLOOR, Ordering::Release);
        for _ in 0..super::HELPER_QUEUE_FLOOR {
            shared.coordinator.begin_task();
        }
        assert!(
            shared.tree_is_worth_helpers(),
            "work summed across roots reaches the floor like work under one"
        );
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

    /// A tree below the floor never builds the scoped machinery, so a panic
    /// there unwinds through a different frame than a widened walk's does. It
    /// still has to reach the caller, and still has to cancel the walk on its
    /// way out — the two routes may not differ on what a panic means.
    #[test]
    fn a_panic_below_the_floor_resumes_on_the_caller_and_cancels() {
        let root = unique_root("lean-panic");
        // One subdirectory holding four files: below every floor, so the walk
        // never widens and the panic is raised on the caller thread itself.
        let only = root.join("only");
        fs::create_dir_all(&only).expect("create fixture directory");
        for index in 0..4 {
            fs::write(only.join(format!("file-{index}.txt")), b"fixture")
                .expect("write fixture file");
        }
        panic_in_directory(only);

        let cancellation = CancellationToken::default();
        let walk_cancellation = cancellation.clone();
        let outcome = catch_unwind(AssertUnwindSafe(|| {
            Walker::new(&root)
                .threads(4)
                .cancellation(walk_cancellation)
                .collect()
        }));
        let _ = fs::remove_dir_all(&root);

        assert!(
            outcome.is_err(),
            "the injected panic must resume on the caller"
        );
        assert!(
            cancellation.is_cancelled(),
            "and must cancel the walk on its way out"
        );
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
