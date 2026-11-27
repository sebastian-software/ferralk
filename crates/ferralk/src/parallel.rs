//! Parallel `collect()` and `stream_parallel()` implementation built on the
//! crate-local scheduler.

use std::{
    any::Any,
    panic::{AssertUnwindSafe, catch_unwind, resume_unwind},
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
};

#[cfg(test)]
use std::collections::HashSet;

use crossbeam_deque::{Steal, Stealer, Worker};

use super::{
    AncestorChain, CANCELLATION_STRIDE, DirectoryBackend, DirectoryEntryCheck, EntryVisitor,
    ErrorPolicy, Listing, Verdict, WalkEntry, WalkError, WalkOperation, WalkResult, Walker,
    check_directory_entry,
    classify::{
        DirectoryTask, EmittedEntry, EntryAction, RootTask, TraversalContext, classify_entry,
    },
    own_path,
    parallel_stream::{StreamControl, StreamMessage, StreamSink},
    push_entry_name, reset_to_directory,
    scheduler::{CacheLine, Coordinator, Scheduler, WorkerSlot},
};

pub(super) fn collect<B: DirectoryBackend + Sync>(
    walker: Walker,
    backend: &B,
    visitor: EntryVisitor<'_>,
) -> Result<WalkResult, WalkError> {
    let walker = Arc::new(walker);
    let shared = Arc::new(Shared::new(Arc::clone(&walker), backend, visitor));
    run(&walker, backend, shared)
}

/// Runs the walk [`collect`] runs, but hands entries and kept errors to
/// `sink` as the workers produce them instead of retaining them.
///
/// This is what `Walker::stream_parallel` runs on its driver thread, which is
/// the walk's caller worker. Only the output differs: scheduling, the helper
/// floors, cancellation and panic capture are the ones `collect` uses. Returns
/// the error that ended the walk under `ErrorPolicy::Abort`.
///
/// The consumer's side of the sink is given a way to stop this walk: the same
/// [`Shared::stop`] an abort or a panic uses, which wakes parked workers at
/// once rather than at their next poll. The backend is `'static` so that the
/// handle can outlive this call without borrowing anything.
pub(super) fn stream<B: DirectoryBackend + Sync + 'static>(
    walker: Walker,
    backend: &'static B,
    sink: StreamSink,
) -> Result<(), WalkError> {
    let walker = Arc::new(walker);
    let control = sink.control();
    let mut shared = Shared::new(Arc::clone(&walker), backend, &super::keep_every_entry);
    shared.sink = Some(sink);
    let shared = Arc::new(shared);
    attach_stop(&shared, &control);
    run(&walker, backend, shared).map(drop)
}

/// Gives the consumer's side of a stream the walk's own stop. The handle is
/// weak, so a consumer that outlives the walk keeps nothing of it alive.
fn attach_stop(shared: &Arc<Shared<'static>>, control: &StreamControl) {
    let walk = Arc::downgrade(shared);
    control.attach(Box::new(move || {
        if let Some(shared) = walk.upgrade() {
            shared.stop();
        }
    }));
}

fn run<B: DirectoryBackend + Sync>(
    walker: &Walker,
    backend: &B,
    shared: Arc<Shared<'_>>,
) -> Result<WalkResult, WalkError> {
    if shared.should_stop() {
        return finish(shared, Vec::new());
    }
    let mut caller = WorkerScratch::new(0);
    let caller_slot = shared.coordinator.claim_caller_slot();

    // The caller starts alone. Helpers are created only after the walk makes
    // parallel directory work available, as required by ADR-0009. Build the
    // first root for the caller and stream remaining roots directly into the
    // shared scheduler one at a time (without staging a `Vec<RootTask>`) so
    // multi-root walks keep startup memory low and widen as soon as
    // `tree_is_worth_helpers()` is met.
    if walker.roots.is_empty() {
        drop(caller_slot);
        return finish(shared, Vec::new());
    }
    let Some(first_root) = walker.build_root_task(backend, 0) else {
        shared.stop();
        drop(caller_slot);
        return finish(shared, Vec::new());
    };
    if !enqueue_remaining_roots(
        &shared,
        (1..walker.roots.len()).map(|index| walker.build_root_task(backend, index)),
    ) {
        shared.stop();
        drop(caller_slot);
        return finish(shared, Vec::new());
    }
    catch_worker_panic(&shared, || {
        if !shared.should_stop() {
            shared.coordinator.begin_task();
            let _root_task = shared.coordinator.claim_task();
            process_directory(
                &shared,
                &mut caller,
                ParallelTask::from_root(first_root),
                None,
            );
        }
    });

    if shared.should_stop() {
        drop(caller_slot);
        return finish(shared, std::mem::take(&mut caller.entries));
    }

    // A walk that never crosses the floor never builds any of what follows.
    // The caller keeps walking on its own queue instead, which is what a tree
    // too small for a thread should cost: the spare deques, the stealer list,
    // the two mutexes and `thread::scope` are all paid once per walk, and a
    // twelve-file tree paid them to spawn nothing.
    //
    caller.flush_into(&shared.scheduler);

    // Remaining roots sit in the shared scheduler alongside anything the first
    // root uncovered — a walk that never widens drains every root on the caller
    // thread, while one that crosses the floor widens immediately so helpers
    // share remaining roots as well as subdirectories.
    //
    // Panics are caught here the way `run_worker` catches them, because this
    // stands in for `run_worker` on the route that never widens.
    let widened =
        catch_worker_panic(&shared, || drain_alone(&shared, &mut caller)).unwrap_or(false);
    if !widened {
        drop(caller_slot);
        return finish(shared, std::mem::take(&mut caller.entries));
    }
    shared.coordinator.widen();

    // Every helper slot is built here so the stealer list is complete and stays
    // lock-free once the walk runs. Reversing `spare` makes `idle.pop()` hand
    // out indices 1, 2, ..., threads - 1 in ascending order so all spawned
    // stealers live in `&stealers[..spawned_workers]`. Only the threads are
    // lazy: a slot stays unused until the backlog outgrows the workers that are
    // already running, which is what lets a tree that only fans out below the
    // root widen too.
    let mut spare = (1..walker.threads.max(1))
        .map(WorkerScratch::new)
        .collect::<Vec<_>>();
    let mut stealers = Vec::with_capacity(spare.len() + 1);
    stealers.push(caller.queue.stealer());
    stealers.extend(spare.iter().map(|worker| worker.queue.stealer()));
    spare.reverse();
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
        let _ = pool.grow();
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
    stealers: &'env [Stealer<ParallelTask>],
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
/// cheaper entry leaves less work to amortize a thread and should raise this.
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
/// In work units since #88, so the listing itself counts toward it. At this
/// size that is a 2% difference and the constant keeps its meaning: one
/// directory holding about a thousand files.
const HELPER_LISTING_FLOOR: usize = 1024;

/// Entries one continuation materializes before handing the listing back.
///
/// Directory tasks stay local to the worker that classified them; the single
/// continuation goes through the shared injector. That makes the worker drain
/// its children before it can produce another batch, while an idle helper can
/// take over producing the next one. The frontier is therefore bounded by one
/// batch per active worker rather than by the width of the directory.
///
/// The batch is sized so that an ordinary directory is classified in one
/// piece. A hand-off moves the listing's name buffers into the continuation,
/// and the local queue is depth-first, so a worker reading wide siblings hands
/// every one of them off before any continuation comes back to it: each such
/// directory then costs a fresh set of buffers, which the consumer drops. At
/// 64 that was every 100-file directory of the 53,600-file repository fixture,
/// 40% more user-space instructions in a four-thread walk than in a serial
/// one, all of them in the allocator, and a quarter of its wall time on Linux.
/// Directories wider than this are rare enough that the bound still holds
/// where it matters: at the directory that would otherwise queue a hundred
/// thousand tasks.
const LISTING_BATCH_SIZE: usize = 1024;

/// Kept entries a streaming worker buffers before it hands them to the
/// consumer in one channel message.
///
/// A worker also hands over whatever it holds when it finishes a directory, so
/// this bounds only a wide listing: a sparse match in a large tree reaches the
/// consumer when its directory is done rather than when a batch fills. One
/// message per entry would put every entry through the channel's shared state,
/// which every worker and the consumer contend on.
///
/// The size is checked at the [`CANCELLATION_STRIDE`] rather than per entry,
/// where the worker already stops to read shared state, so that `collect()`
/// carries no per-entry test for a stream it does not have. A batch therefore
/// holds fewer than this plus one stride of entries.
pub(super) const STREAM_BATCH_SIZE: usize = 256;

impl<'scope, 'env> HelperPool<'scope, 'env> {
    /// Starts helpers while queued directories outnumber the running workers
    /// and the thread budget has room. Cheap enough to call after every
    /// directory: a full pool costs the two atomic loads that reject the claim.
    ///
    /// Returns `false` if the walk stopped before or during growth (for
    /// instance, a refused spawn under `ErrorPolicy::Abort` or a closed stream
    /// sink).
    fn grow(self) -> bool {
        // Fast path: once every helper slot has been spawned (or the operating
        // system refused to start one), no further growth is possible and we
        // skip polling `work_seen` / `active_workers`.
        if self.shared.helpers_capped.load(Ordering::Acquire) {
            return true;
        }
        while !self.shared.should_stop() {
            if self.shared.helpers_capped.load(Ordering::Acquire) {
                return true;
            }
            if !self.shared.tree_is_worth_helpers() {
                return true;
            }
            let Some(slot) = self
                .shared
                .coordinator
                .claim_worker_slot(self.shared.walker.threads)
            else {
                return true;
            };
            // Dropping the slot here releases it again for a later attempt.
            let (scratch, became_empty) = {
                let mut idle = lock(self.idle);
                let Some(scratch) = idle.pop() else {
                    self.shared.helpers_capped.store(true, Ordering::Release);
                    return true;
                };
                (scratch, idle.is_empty())
            };
            if !self.spawn(slot, scratch) {
                return !self.shared.should_stop();
            }
            if became_empty {
                self.shared.helpers_capped.store(true, Ordering::Release);
                return true;
            }
        }
        false
    }

    /// Reports whether the pool may keep growing after this attempt.
    fn spawn(self, slot: WorkerSlot<'env>, scratch: WorkerScratch) -> bool {
        #[cfg(test)]
        count_worker_spawn_attempt();
        #[cfg(test)]
        if should_fail_next_worker_spawn() {
            self.shared
                .record_spawn_failure(std::io::Error::other("injected worker start failure"));
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
                // Publish this helper's stealer slot once the thread is running
                // and before it can push any task into its local deque, so a
                // refused OS spawn never leaves an unspawned deque in the
                // active steal prefix.
                self.shared
                    .spawned_workers
                    .fetch_max(scratch.index + 1, Ordering::Release);
                run_worker_catching_panics(self, &mut scratch);
                lock(self.shards).push(std::mem::take(&mut scratch.entries));
            });
        match spawn {
            // The scope joins the helper, so the handle is not needed here.
            Ok(_) => true,
            // The closure, and with it the slot, is dropped on failure.
            Err(source) => {
                self.shared.record_spawn_failure(source);
                false
            }
        }
    }
}

fn finish(shared: Arc<Shared>, mut entries: Vec<WalkEntry>) -> Result<WalkResult, WalkError> {
    if let Some(payload) = lock(&shared.panic).take() {
        resume_unwind(payload);
    }
    if let Some(error) = lock(&shared.abort_error).take() {
        return Err(error);
    }
    if shared.walker.options.sort {
        crate::sort_order::sort_entries(&shared.walker, &mut entries);
    }
    Ok(WalkResult {
        entries,
        errors: std::mem::take(&mut *lock(&shared.errors)),
        cancelled: shared.should_stop(),
    })
}

/// What [`Shared::emit`] tells its caller to do after asking the visitor.
enum EmitOutcome {
    /// The entry's subtree may be walked and the current listing continues.
    Descend,
    /// Skip this entry's subtree (`Verdict::Prune`), but continue the listing.
    Prune,
    /// Stop the walk immediately (`Verdict::Stop`).
    Stop,
}

struct Shared<'backend> {
    walker: Arc<Walker>,
    /// The caller's per-entry filter, run on whichever worker produced the
    /// entry. Shared rather than cloned, which is why it is `Sync`.
    visitor: EntryVisitor<'backend>,
    /// Set by a [`Verdict::Stop`]. Kept apart from `cancellation` so a stop
    /// never reaches into a token the caller owns and may reuse.
    stopped: AtomicBool,
    /// Work seen so far, the size signal behind [`HELPER_WORK_FLOOR`]: entries
    /// plus [`DIRECTORY_WEIGHT`] for each listing they came from. Accumulated
    /// per listing rather than per entry, so a directory costs one atomic add
    /// however much it holds - and the weight rides along in that same add.
    ///
    /// On its own cache line: every worker adds to it once per directory and
    /// every `grow` reads it, so packing it beside `stopped` — which the same
    /// loops read — made unrelated values share one line.
    work_seen: CacheLine<AtomicUsize>,
    /// Every filesystem call of the walk goes through here, which is what lets
    /// a test mock drive the parallel frontend the same way it drives the
    /// serial one.
    backend: &'backend (dyn DirectoryBackend + Sync),
    scheduler: Scheduler<ParallelTask>,
    /// Task accounting, worker slots and the parking protocol.
    coordinator: Coordinator,
    /// Caller-controlled external cancellation. Internal shutdown is always
    /// represented by `stopped`, so one walk cannot poison a shared token.
    cancellation: Option<super::CancellationToken>,
    errors: Mutex<Vec<WalkError>>,
    abort_error: Mutex<Option<WalkError>>,
    /// Set once every helper slot has been spawned or a helper could not be
    /// started. Helpers are an optimization (ADR-0009), so the walk goes on
    /// with the workers it has; this stops later directory boundaries from
    /// polling `work_seen` / `active_workers` or retrying refused spawns.
    helpers_capped: AtomicBool,
    /// Number of workers whose stealers occupy the active prefix of
    /// `HelperPool::stealers` (starts at 1 for the caller at index 0).
    spawned_workers: AtomicUsize,
    panic: Mutex<Option<Box<dyn Any + Send + 'static>>>,
    /// Where a streaming walk sends its entries and kept errors. `None` for
    /// `collect()` and `visit()`, which retain them for the `WalkResult`.
    sink: Option<StreamSink>,
}

impl<'backend> Shared<'backend> {
    fn new(
        walker: Arc<Walker>,
        backend: &'backend (dyn DirectoryBackend + Sync),
        visitor: EntryVisitor<'backend>,
    ) -> Self {
        let cancellation = walker.cancellation.clone();
        Self {
            walker,
            visitor,
            stopped: AtomicBool::new(false),
            work_seen: CacheLine(AtomicUsize::new(0)),
            backend,
            scheduler: Scheduler::new(),
            coordinator: Coordinator::new(),
            cancellation,
            errors: Mutex::new(Vec::new()),
            abort_error: Mutex::new(None),
            helpers_capped: AtomicBool::new(false),
            spawned_workers: AtomicUsize::new(1),
            panic: Mutex::new(None),
            sink: None,
        }
    }

    fn schedule(&self, worker: &Worker<ParallelTask>, task: ParallelTask) {
        self.coordinator.schedule(|| worker.push(task));
    }

    fn schedule_shared(&self, task: ParallelTask) {
        self.coordinator.schedule(|| self.scheduler.push(task));
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
        self.cancellation
            .as_ref()
            .is_some_and(super::CancellationToken::is_cancelled)
            || self.stopped.load(Ordering::Acquire)
    }

    /// Runs the visitor and keeps the entry if it survived.
    ///
    /// A [`Verdict::Stop`] ends the walk the way cancellation does, waking the
    /// parked workers so none of them sits out the rest of the walk.
    ///
    /// The entry's path is copied out of the worker's scratch here, into the
    /// buffer the last dropped entry gave back — so a worker whose visitor
    /// keeps nothing runs without allocating.
    ///
    /// Distinguishes [`EmitOutcome::Prune`] (skip this directory's subtree but
    /// keep classifying siblings) from [`EmitOutcome::Stop`] (stop the current
    /// listing immediately). The caller asks before it schedules the directory,
    /// so pruning is a decision about one task and never reaches the
    /// scheduler's accounting.
    fn emit(&self, worker: &mut WorkerScratch, emitted: EmittedEntry) -> EmitOutcome {
        let WorkerScratch {
            entries,
            path,
            spare,
            ..
        } = worker;
        let entry = emitted.with_path(own_path(spare, path));
        match (self.visitor)(&entry) {
            Verdict::Keep => {
                entries.push(entry);
                EmitOutcome::Descend
            }
            Verdict::Skip => {
                *spare = entry.path;
                EmitOutcome::Descend
            }
            Verdict::Prune => {
                *spare = entry.path;
                EmitOutcome::Prune
            }
            Verdict::Stop => {
                *spare = entry.path;
                self.stop();
                EmitOutcome::Stop
            }
        }
    }

    /// Hands a streaming worker's buffered entries to the consumer.
    ///
    /// The send blocks while the channel is full, which is the stream's
    /// backpressure: a worker holds at most one batch beyond what the channel
    /// holds. A consumer that went away fails the send and stops the walk.
    /// Entries buffered after the walk stopped are dropped, so none of them
    /// trails the error that ended it under `ErrorPolicy::Abort`.
    ///
    /// Returns `false` if the walk stopped (either already stopped or because
    /// the consumer closed its channel).
    ///
    /// Callers test `self.sink` first and pass it in, once per directory or
    /// per cancellation stride, so a collecting walk never gets this far and
    /// pays nothing per entry for the stream's existence.
    fn flush_stream(&self, sink: &StreamSink, worker: &mut WorkerScratch) -> bool {
        if worker.entries.is_empty() {
            return true;
        }
        if self.should_stop() {
            worker.entries.clear();
            return false;
        }
        // Sized after the batch it replaces: a narrow directory does not pay
        // for a full batch, and a wide listing keeps a full-batch buffer.
        let capacity = worker.entries.len();
        let batch = std::mem::replace(&mut worker.entries, Vec::with_capacity(capacity));
        if !sink.send(StreamMessage::Entries(batch)) {
            self.stop();
            return false;
        }
        true
    }

    /// Records a recoverable error under the configured policy. Returns `false`
    /// once the error ended the walk (under `ErrorPolicy::Abort` or because the
    /// streaming consumer closed its channel).
    fn record_error(
        &self,
        operation: WalkOperation,
        path: PathBuf,
        source: std::io::Error,
        is_root: bool,
    ) -> bool {
        let error = WalkError::new(operation, path, source);
        match self.walker.error_policy {
            ErrorPolicy::Abort => {
                let mut abort_error = lock(&self.abort_error);
                if abort_error.is_none() {
                    *abort_error = Some(error);
                    self.stop();
                }
                false
            }
            ErrorPolicy::Skip if !is_root => true,
            ErrorPolicy::Skip | ErrorPolicy::Collect => match &self.sink {
                None => {
                    lock(&self.errors).push(error);
                    true
                }
                // A stream delivers a kept error in band, when it happens,
                // rather than after the walk.
                Some(sink) => {
                    if !sink.send(StreamMessage::Error(error)) {
                        self.stop();
                        false
                    } else {
                        true
                    }
                }
            },
        }
    }

    /// A helper the operating system refused to start is a recoverable
    /// `spawn_worker` error on the first root, not the walk's failure: the
    /// caller thread is always one worker, so the walk completes on whatever
    /// pool exists. Under `ErrorPolicy::Abort` the error ends the walk like
    /// any other.
    fn record_spawn_failure(&self, source: std::io::Error) {
        self.helpers_capped.store(true, Ordering::Release);
        let _ = self.record_error(
            WalkOperation::SpawnWorker,
            self.walker
                .roots()
                .next()
                .expect("a walk has a root")
                .into(),
            source,
            false,
        );
    }

    fn record_panic(&self, payload: Box<dyn Any + Send + 'static>) {
        let mut panic = lock(&self.panic);
        if panic.is_none() {
            *panic = Some(payload);
            self.stop();
        }
    }

    /// Stops only this walk. The configured token is caller-owned external
    /// cancellation: internal aborts, startup failures, and panics must wake
    /// siblings without poisoning a token the caller may share or reuse.
    fn stop(&self) {
        self.stopped.store(true, Ordering::Release);
        self.coordinator.wake_waiters();
    }
}

struct WorkerScratch {
    /// Position of this worker's stealer in the shared stealer list, so the
    /// idle scan can start next door and skip its own queue.
    index: usize,
    queue: Worker<ParallelTask>,
    entries: Vec<WalkEntry>,
    /// The directory this worker is reading. It takes tasks off a queue rather
    /// than descending by recursion, so one directory is open at a time and
    /// one set of buffers is enough.
    listing: Listing,
    /// That directory's path, with the entry being classified pushed onto it.
    path: PathBuf,
    glob_bytes: Vec<u8>,
    ignore_scratch: super::gitignore::IgnoreScratch,
    /// The path buffer the last dropped entry left behind. See [`own_path`].
    spare: PathBuf,
}

/// Streams remaining roots into `shared.scheduler` in a single
/// cancellation-aware pass without staging a `Vec<RootTask>`. Returns `false`
/// if the walk stopped or root construction observed cancellation before every
/// remaining root was scheduled.
fn enqueue_remaining_roots(
    shared: &Shared,
    roots: impl IntoIterator<Item = Option<RootTask>>,
) -> bool {
    let mut roots = roots.into_iter();
    loop {
        if shared.should_stop() {
            return false;
        }
        let Some(next_root) = roots.next() else {
            return true;
        };
        let Some(root) = next_root else {
            return false;
        };
        if shared.should_stop() {
            return false;
        }
        shared.schedule_shared(ParallelTask::from_root(root));
    }
}

/// Work local to the parallel frontend.
///
/// A task sits inline in the work-stealing deques, which double up to 65,536
/// slots on a wide listing, so its size is paid once per queued directory.
/// Rare variants — a root carrying ancestor ignore-read errors and a wide
/// listing's continuation — live behind a pointer so error-free roots and
/// subdirectories pay zero extra bytes or locking.
enum ParallelTask {
    Directory(DirectoryTask),
    RootWithIgnoreErrors(Box<RootTask>),
    Continuation(Box<Continuation>),
}

impl ParallelTask {
    fn from_root(root: RootTask) -> Self {
        if root.ignore_errors.is_empty() {
            Self::Directory(root.task)
        } else {
            Self::RootWithIgnoreErrors(Box::new(root))
        }
    }
}

/// The unvisited tail of a wide directory listing. It lets the serial route
/// make already-discovered child directories available to helpers without
/// reopening the directory or its ignore files.
struct Continuation {
    path: PathBuf,
    depth: usize,
    root: usize,
    ancestors: AncestorChain,
    ignores: super::gitignore::IgnoreScope,
    listing: Listing,
    next_entry: usize,
}

impl WorkerScratch {
    fn new(index: usize) -> Self {
        Self {
            index,
            // Depth-first local work keeps a freshly listed directory close to
            // the children it just queued; thieves still take older work from
            // the opposite end to spread independent subtrees. On the native
            // macOS benchmark this is 10.3% faster over 53,600 files and 3.5%
            // faster over 5,120; a 128-file tree pays 0.04 ms (4%), where the
            // helper floor already keeps the absolute cost close to one ms.
            queue: Worker::new_lifo(),
            entries: Vec::new(),
            listing: Listing::default(),
            path: PathBuf::new(),
            glob_bytes: Vec::new(),
            ignore_scratch: super::gitignore::IgnoreScratch::default(),
            spare: PathBuf::new(),
        }
    }

    fn flush_into(&self, scheduler: &Scheduler<ParallelTask>) {
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
        if shared.should_stop() {
            return false;
        }
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
        process_directory(shared, caller, directory, None);
    }
}

fn run_worker(pool: HelperPool<'_, '_>, worker: &mut WorkerScratch) {
    let shared = pool.shared;
    while let Some(directory) = next_task(shared, worker, pool.stealers) {
        let _task = shared.coordinator.claim_task();
        if !shared.should_stop() {
            process_directory(shared, worker, directory, Some(pool));
            // The directory may have opened the tree up, so re-check whether
            // the walk can use more of the configured thread budget.
            let _ = pool.grow();
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
    stealers: &[Stealer<ParallelTask>],
) -> Option<ParallelTask> {
    shared.coordinator.wait_for_task(
        || shared.should_stop(),
        || try_take(shared, worker, stealers),
    )
}

fn try_take(
    shared: &Shared,
    worker: &WorkerScratch,
    stealers: &[Stealer<ParallelTask>],
) -> Option<ParallelTask> {
    worker
        .queue
        .pop()
        .or_else(|| {
            let task = shared.scheduler.steal_into(&worker.queue);
            if task.is_some() && !worker.queue.is_empty() {
                shared.coordinator.announce_batch_handoff();
            }
            task
        })
        .or_else(|| {
            // Only probe stealers belonging to workers that have actually been
            // spawned. Start next door and leave the own queue out: stealing
            // from itself can only come up empty, and the offset keeps idle
            // workers from all probing the same victim first.
            let active_len = shared
                .spawned_workers
                .load(Ordering::Acquire)
                .min(stealers.len());
            let active = &stealers[..active_len];
            let (head, tail) = active.split_at((worker.index + 1).min(active.len()));
            tail.iter()
                .chain(head.iter().take(worker.index))
                .find_map(|stealer| {
                    loop {
                        // A batch, not one task: an idle worker that takes a
                        // single directory is back contending for a victim as
                        // soon as it has read it, and the victim pays for the
                        // steal either way. This is what the injector path
                        // already does through `Scheduler::steal_into`.
                        match stealer.steal_batch_and_pop(&worker.queue) {
                            Steal::Success(task) => {
                                if !worker.queue.is_empty() {
                                    shared.coordinator.announce_batch_handoff();
                                }
                                break Some(task);
                            }
                            Steal::Empty => break None,
                            Steal::Retry => continue,
                        }
                    }
                })
        })
}

/// Walks one task and, for a streaming walk, hands what it kept to the
/// consumer: a directory's entries reach the consumer when the directory is
/// done, however few of them there are.
fn process_directory(
    shared: &Shared,
    worker: &mut WorkerScratch,
    task: ParallelTask,
    pool: Option<HelperPool<'_, '_>>,
) {
    walk_directory(shared, worker, task, pool);
    if let Some(sink) = &shared.sink {
        let _ = shared.flush_stream(sink, worker);
    }
}

struct PreparedListing {
    path: PathBuf,
    depth: usize,
    root: usize,
    ancestors: AncestorChain,
    ignores: super::gitignore::IgnoreScope,
    start_index: usize,
}

fn prepare_directory(
    shared: &Shared,
    worker: &mut WorkerScratch,
    task: DirectoryTask,
    root_ignore_errors: Vec<super::gitignore::IgnoreReadError>,
) -> Option<PreparedListing> {
    let DirectoryTask {
        path,
        open,
        depth,
        root,
        ancestors,
        ignores,
    } = task;
    #[cfg(test)]
    if should_panic_in_directory(&path) {
        panic!("injected directory panic");
    }
    if shared.should_stop() {
        return None;
    }
    let is_root = depth == 0;
    for error in root_ignore_errors {
        let (path, source) = error.into_parts();
        if !shared.record_error(super::IGNORE_FILE_OPERATION, path, source, false)
            || shared.should_stop()
        {
            return None;
        }
    }
    let ancestors = match check_directory_entry(
        &shared.walker,
        shared.backend,
        &path,
        root,
        ancestors,
        is_root,
    ) {
        DirectoryEntryCheck::Enter(ancestors) => ancestors,
        DirectoryEntryCheck::Skip => return None,
        DirectoryEntryCheck::Failed(operation, source) => {
            let _ = shared.record_error(operation, path, source, is_root);
            return None;
        }
    };
    if let Err(source) = shared.backend.read_scheduled_directory(
        &path,
        &open,
        shared.walker.options.follow_symlinks,
        !shared.walker.options.follow_symlinks && depth > 0,
        &mut worker.listing,
    ) {
        let _ = shared.record_error(WalkOperation::ReadDir, path, source, is_root);
        return None;
    }
    // One add for the whole listing: the entries it returned, and the listing
    // itself, which cost a syscall to get them. Once the helper pool is full or
    // capped, nothing reads `work_seen` anymore, so skip the atomic RMW to
    // avoid cross-core contention.
    if !shared.helpers_capped.load(Ordering::Relaxed) {
        shared.work_seen.fetch_add(
            worker.listing.entries().len() + DIRECTORY_WEIGHT,
            Ordering::AcqRel,
        );
    }
    // The directory's own ignore files join the chain here. Every directory is
    // processed once, so every ignore file is read once, whatever the worker
    // count.
    let (ignores, ignore_errors) =
        ignores.enter(&shared.walker, shared.backend, &path, &worker.listing);
    for error in ignore_errors {
        let (path, source) = error.into_parts();
        if !shared.record_error(super::IGNORE_FILE_OPERATION, path, source, false)
            || shared.should_stop()
        {
            return None;
        }
    }
    Some(PreparedListing {
        path,
        depth,
        root,
        ancestors,
        ignores,
        start_index: 0,
    })
}

fn walk_directory(
    shared: &Shared,
    worker: &mut WorkerScratch,
    task: ParallelTask,
    pool: Option<HelperPool<'_, '_>>,
) {
    let PreparedListing {
        path,
        depth,
        root,
        ancestors,
        ignores,
        start_index,
    } = match task {
        ParallelTask::Continuation(continuation) => {
            let Continuation {
                path,
                depth,
                root,
                ancestors,
                ignores,
                listing,
                next_entry,
            } = *continuation;
            worker.listing = listing;
            PreparedListing {
                path,
                depth,
                root,
                ancestors,
                ignores,
                start_index: next_entry,
            }
        }
        ParallelTask::Directory(task) => {
            let Some(prepared) = prepare_directory(shared, worker, task, Vec::new()) else {
                return;
            };
            prepared
        }
        ParallelTask::RootWithIgnoreErrors(root_task) => {
            let RootTask {
                task,
                ignore_errors,
            } = *root_task;
            let Some(prepared) = prepare_directory(shared, worker, task, ignore_errors) else {
                return;
            };
            prepared
        }
    };
    worker.path.clear();
    worker.path.push(&path);
    let end_index = (start_index + LISTING_BATCH_SIZE).min(worker.listing.entries().len());
    for index in start_index..end_index {
        // Once per directory above, and then every [`CANCELLATION_STRIDE`]
        // entries so a listing of a hundred thousand names still stops
        // promptly.
        if index.is_multiple_of(CANCELLATION_STRIDE) {
            if shared.should_stop() {
                return;
            }
            if let Some(pool) = pool
                && !pool.grow()
            {
                return;
            }
            if let Some(sink) = &shared.sink
                && worker.entries.len() >= STREAM_BATCH_SIZE
                && !shared.flush_stream(sink, worker)
            {
                return;
            }
        }
        // The entry's path exists only for as long as it is being decided
        // about; anything that outlives that copies it out.
        push_entry_name(&mut worker.path, worker.listing.entries()[index].name());
        let action = classify_entry(
            &shared.walker,
            shared.backend,
            &worker.path,
            &worker.listing.entries()[index],
            &ignores,
            depth,
            TraversalContext {
                root,
                ancestors: &ancestors,
                listing: &worker.listing,
                glob_bytes_scratch: &mut worker.glob_bytes,
                ignore_scratch: &mut worker.ignore_scratch,
            },
        );
        let should_continue = act(shared, worker, action);
        reset_to_directory(&mut worker.path, &path);
        if !should_continue {
            return;
        }
    }
    if end_index < worker.listing.entries().len() {
        if shared.should_stop() {
            return;
        }
        let listing = std::mem::take(&mut worker.listing);
        shared.schedule_shared(ParallelTask::Continuation(Box::new(Continuation {
            path,
            depth,
            root,
            ancestors,
            ignores,
            listing,
            next_entry: end_index,
        })));
        return;
    }
    while let Some(error) = worker.listing.take_deferred_error() {
        if shared.should_stop() {
            worker.listing.shrink_if_oversized();
            return;
        }
        if !shared.record_error(
            WalkOperation::ReadDir,
            error.path,
            error.source.into_io_error(),
            false,
        ) {
            worker.listing.shrink_if_oversized();
            return;
        }
    }
    worker.listing.shrink_if_oversized();
}

/// Carries out what classification decided about one entry. The entry's path
/// is still on the worker's scratch, which is where `emit` copies it from.
///
/// Returns `false` once this entry ended the walk (by `Verdict::Stop`,
/// `ErrorPolicy::Abort`, or a closed stream sink), so the caller stops the
/// current listing immediately instead of waiting for the next cancellation
/// stride.
fn act(shared: &Shared, worker: &mut WorkerScratch, action: EntryAction) -> bool {
    match action {
        EntryAction::Skip => true,
        EntryAction::Descend(task) => {
            shared.schedule(&worker.queue, ParallelTask::Directory(task));
            true
        }
        // The visitor is asked first, because its verdict may prune the
        // directory. A pruned task is dropped before it is ever counted.
        EntryAction::DescendAndEmit(entry, task) => match shared.emit(worker, entry) {
            EmitOutcome::Descend => {
                shared.schedule(&worker.queue, ParallelTask::Directory(task));
                true
            }
            EmitOutcome::Prune => true,
            EmitOutcome::Stop => false,
        },
        // Nothing to walk below this entry, so `Prune` is `Skip` for it.
        EntryAction::Emit(entry) => !matches!(shared.emit(worker, entry), EmitOutcome::Stop),
        // Record the error before scheduling any descent so `ErrorPolicy::Abort`
        // or a closed stream sink never wakes helpers for discarded work.
        EntryAction::Failed { failure, descend } => {
            if !shared.record_error(failure.operation, failure.path, failure.source, false) {
                return false;
            }
            if let Some(task) = descend {
                shared.schedule(&worker.queue, ParallelTask::Directory(task));
            }
            true
        }
    }
}

pub(super) fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
std::thread_local! {
    static FAIL_NEXT_WORKER_SPAWN: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    /// Spawn attempts made from this thread, so a test can tell one refused
    /// spawn from a walk that keeps asking at every directory boundary.
    static WORKER_SPAWN_ATTEMPTS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
pub(super) fn fail_next_worker_spawn() {
    FAIL_NEXT_WORKER_SPAWN.with(|failure| failure.set(true));
}

#[cfg(test)]
pub(super) fn should_fail_next_worker_spawn() -> bool {
    FAIL_NEXT_WORKER_SPAWN.with(std::cell::Cell::take)
}

#[cfg(test)]
fn count_worker_spawn_attempt() {
    WORKER_SPAWN_ATTEMPTS.with(|attempts| attempts.set(attempts.get() + 1));
}

#[cfg(test)]
fn take_worker_spawn_attempts() -> usize {
    WORKER_SPAWN_ATTEMPTS.with(std::cell::Cell::take)
}

/// Directory whose traversal panics once, on whichever worker picks it up.
/// The trigger is process-wide because helper threads own their tasks, and it
/// matches absolute paths so concurrent tests stay independent. A set rather
/// than one slot, so two tests arming it at once do not disarm each other.
#[cfg(test)]
static PANIC_IN_DIRECTORY: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

#[cfg(test)]
pub(super) fn panic_in_directory(directory: PathBuf) {
    lock(&PANIC_IN_DIRECTORY).push(directory);
}

/// Whether the panic armed for `directory` has not fired yet.
#[cfg(test)]
pub(super) fn panic_in_directory_is_armed(directory: &std::path::Path) -> bool {
    lock(&PANIC_IN_DIRECTORY)
        .iter()
        .any(|target| target == directory)
}

#[cfg(test)]
fn should_panic_in_directory(directory: &std::path::Path) -> bool {
    let mut targets = lock(&PANIC_IN_DIRECTORY);
    if let Some(index) = targets.iter().position(|target| target == directory) {
        targets.swap_remove(index);
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
pub(super) static WORKER_RENDEZVOUS_GUARD: Mutex<()> = Mutex::new(());

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
pub(super) fn expect_worker_threads(root: PathBuf, expected: usize) {
    *lock(&WORKER_RENDEZVOUS) = Some(WorkerRendezvous {
        root,
        expected,
        threads: HashSet::new(),
        released: false,
    });
}

#[cfg(test)]
pub(super) fn observed_worker_threads() -> usize {
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
    if !shared.helpers_capped.load(Ordering::Acquire) && !shared.tree_is_worth_helpers() {
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
            atomic::{AtomicBool, AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{
        Shared, WORKER_RENDEZVOUS_GUARD, Walker, catch_worker_panic, expect_worker_threads,
        fail_next_worker_spawn, finish, lock, observed_worker_threads, panic_in_directory,
        take_worker_spawn_attempts,
    };
    use crate::{CancellationToken, ErrorPolicy, WalkOptions, WalkResult};

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
        // there is enough of the tree to amortize a thread.
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

    /// A worker whose directories match nothing never sends, so it cannot
    /// learn from a failed send that the consumer went away, and a parked one
    /// would sleep until its next poll. Closing the stream has to stop the
    /// walk itself, the way an abort does, without touching a caller token -
    /// whether the consumer closes before the driver attached or after.
    #[test]
    fn a_closed_stream_stops_the_walk_itself() {
        for close_first in [false, true] {
            let cancellation = CancellationToken::default();
            let walker = Arc::new(Walker::new(".").cancellation(cancellation.clone()));
            let mut shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
            let (sink, receiver) = crate::parallel_stream::test_sink(1);
            let control = sink.control();
            shared.sink = Some(sink);
            let shared = Arc::new(shared);

            if close_first {
                crate::parallel_stream::close_test_sink(&control);
                assert!(!shared.should_stop(), "nothing to stop before attaching");
            }
            super::attach_stop(&shared, &control);
            if !close_first {
                assert!(!shared.should_stop());
                crate::parallel_stream::close_test_sink(&control);
            }
            assert!(
                shared.stopped.load(Ordering::Acquire),
                "close_first={close_first}: closing sets the walk's own stop, which wakes parked workers"
            );
            assert!(shared.should_stop());
            assert!(
                !cancellation.is_cancelled(),
                "the caller's token is untouched"
            );
            drop(receiver);
        }
    }

    #[test]
    fn worker_panic_stops_siblings_without_cancelling_the_caller_token() {
        let cancellation = CancellationToken::default();
        let walker = Arc::new(Walker::new(".").cancellation(cancellation.clone()));
        let shared = Arc::new(Shared::new(
            walker,
            &crate::SystemBackend,
            &crate::keep_every_entry,
        ));

        catch_worker_panic(&shared, || panic!("injected worker panic"));
        assert!(
            !cancellation.is_cancelled(),
            "a panic must not cancel a caller-owned token"
        );
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
    fn a_single_wide_listing_widens_before_its_last_entry() {
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
        let root = unique_root("wide-listing");
        // The helper-listing floor is deliberately much higher than the
        // cancellation stride. If widening waited for a directory boundary,
        // the 1,100 independent directories here would all be discovered on
        // the caller before a helper could start.
        for index in 0..1_100 {
            fs::create_dir_all(root.join(format!("branch-{index:04}")))
                .expect("create fixture directory");
        }

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
            "a wide listing must make its queued children available before it ends"
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
    /// would cost more than it saves - the same judgment #76 made for one
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

    /// Pruning is decided before a directory is scheduled, so a widened walk
    /// that drops tasks on several workers still accounts for every task it
    /// did schedule and terminates, and no pruned directory is ever read.
    #[test]
    fn a_widened_walk_prunes_on_every_worker_and_terminates() {
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
        let root = unique_root("visitor-prune");
        create_wide_fixture(&root);

        expect_worker_threads(root.clone(), 4);
        let asked = Mutex::new(Vec::new());
        let result = Walker::new(&root)
            .threads(4)
            .visit(|entry| {
                let relative = entry
                    .path()
                    .strip_prefix(&root)
                    .expect("entry below the root")
                    .to_path_buf();
                let pruned = entry.basename() == Some("nested-1".as_ref());
                lock(&asked).push(relative);
                if pruned {
                    crate::Verdict::Prune
                } else {
                    crate::Verdict::Keep
                }
            })
            .expect("visited walk succeeds");
        let workers = observed_worker_threads();
        let _ = fs::remove_dir_all(&root);

        assert_eq!(workers, 4, "the walk did not reach its thread budget");
        assert!(!result.was_cancelled());
        let asked = asked.into_inner().expect("visitor lock");
        assert!(
            asked.iter().all(|path| !path
                .parent()
                .is_some_and(|parent| parent.ends_with("nested-1"))),
            "a pruned directory was read"
        );
        // Ten branches, each with itself, three kept nested directories and
        // their four files.
        assert_eq!(result.entries().len(), 10 * (1 + 3 * (1 + 4)));
    }

    /// A tree below the floor never builds the scoped machinery, so a panic
    /// there unwinds through a different frame than a widened walk's does. It
    /// still has to reach the caller and stop this walk on its way out — the
    /// two routes may not differ on what a panic means.
    #[test]
    fn a_panic_below_the_floor_resumes_on_the_caller_without_poisoning_its_token() {
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
        assert!(
            outcome.is_err(),
            "the injected panic must resume on the caller"
        );
        assert!(
            !cancellation.is_cancelled(),
            "the caller-owned token remains reusable after a panic"
        );
        let reused = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("the same token can drive a later walk");
        let _ = fs::remove_dir_all(&root);
        assert!(!reused.was_cancelled());
    }

    #[test]
    fn root_visitor_panic_uses_the_parallel_shutdown_protocol() {
        let root = unique_root("root-visitor-panic");
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir_all(&first).expect("create first root");
        fs::create_dir_all(&second).expect("create second root");
        let panic_path = first.join("panic.txt");
        fs::write(&panic_path, b"fixture").expect("write first root file");
        fs::write(second.join("unvisited.txt"), b"fixture").expect("write second root file");

        let cancellation = CancellationToken::default();
        let seen_second = Arc::new(AtomicBool::new(false));
        let walk_first = first.clone();
        let walk_second = second.clone();
        let walk_panic_path = panic_path.clone();
        let walk_cancellation = cancellation.clone();
        let walk_seen_second = Arc::clone(&seen_second);
        let (sender, receiver) = mpsc::channel();
        let runner = thread::Builder::new()
            .name("ferralk-root-panic-regression".into())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    Walker::new(&walk_first)
                        .add_root(&walk_second)
                        .expect("second root")
                        .threads(4)
                        .cancellation(walk_cancellation)
                        .visit(move |entry| {
                            if entry.path() == walk_panic_path {
                                panic!("injected root visitor panic");
                            }
                            if entry.path().starts_with(&walk_second) {
                                walk_seen_second.store(true, Ordering::Release);
                            }
                            crate::Verdict::Keep
                        })
                }));
                let _ = sender.send(outcome.is_err());
            })
            .expect("spawn the walking thread");

        assert_eq!(
            receiver.recv_timeout(RESUME_TIMEOUT),
            Ok(true),
            "a root visitor panic must resume without hanging"
        );
        runner.join().expect("the walking thread joins");
        assert!(
            !cancellation.is_cancelled(),
            "the panic must leave the caller-owned token reusable"
        );
        assert!(
            !seen_second.load(Ordering::Acquire),
            "a root panic must stop the remaining roots"
        );
        let reused = Walker::new(&first)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("the same token can drive a later walk");
        let _ = fs::remove_dir_all(&root);
        assert!(!reused.was_cancelled());
    }

    #[test]
    fn deeper_visitor_panic_matches_the_root_panic_protocol() {
        let root = unique_root("deep-visitor-panic");
        create_wide_fixture(&root);
        let panic_path = root.join("branch-3").join("nested-2").join("file-0.txt");

        let cancellation = CancellationToken::default();
        let walk_root = root.clone();
        let walk_panic_path = panic_path.clone();
        let walk_cancellation = cancellation.clone();
        let (sender, receiver) = mpsc::channel();
        let runner = thread::Builder::new()
            .name("ferralk-deep-panic-regression".into())
            .spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    Walker::new(&walk_root)
                        .threads(4)
                        .cancellation(walk_cancellation)
                        .visit(move |entry| {
                            if entry.path() == walk_panic_path {
                                panic!("injected deep visitor panic");
                            }
                            crate::Verdict::Keep
                        })
                }));
                let _ = sender.send(outcome.is_err());
            })
            .expect("spawn the walking thread");

        assert_eq!(
            receiver.recv_timeout(RESUME_TIMEOUT),
            Ok(true),
            "a deeper visitor panic must resume without hanging"
        );
        runner.join().expect("the walking thread joins");
        assert!(
            !cancellation.is_cancelled(),
            "root and deeper panics have the same token-ownership contract"
        );
        let reused = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("the same token can drive a later walk");
        let _ = fs::remove_dir_all(&root);
        assert!(!reused.was_cancelled());
    }

    #[test]
    fn externally_cancelled_token_still_ends_a_parallel_walk() {
        let root = unique_root("external-cancellation");
        create_wide_fixture(&root);
        let cancellation = CancellationToken::default();
        cancellation.cancel();

        let result = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("external cancellation is a normal partial result");
        let _ = fs::remove_dir_all(&root);

        assert!(result.was_cancelled());
        assert!(result.entries().is_empty());
        assert!(cancellation.is_cancelled());
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
            assert_eq!(
                resumed,
                Ok(true),
                "round {round}: the injected worker panic must resume on the caller"
            );
            runner.join().expect("the walking thread joins");
            assert!(
                !cancellation.is_cancelled(),
                "round {round}: a panic must not cancel the caller-owned token"
            );
            let reused = Walker::new(&root)
                .threads(4)
                .cancellation(cancellation.clone())
                .collect()
                .expect("the same token can drive a later walk");
            let _ = fs::remove_dir_all(&root);
            assert!(!reused.was_cancelled());
        }
    }

    /// A helper the operating system refuses is not the walk's failure: the
    /// workers that exist finish the tree, the refusal is one recoverable
    /// `spawn_worker` error under `Collect` and nothing under `Skip`, and the
    /// pool stops asking instead of retrying at every directory boundary.
    #[test]
    fn worker_start_failure_caps_the_pool_and_completes_the_walk() {
        let root = unique_root("worker-start-capped");
        // Wide enough to reach the size floor, or no helper is ever attempted
        // and there is nothing for the injected failure to land on.
        create_wide_fixture(&root);
        let expected = Walker::new(&root)
            .threads(1)
            .collect()
            .expect("serial reference walk")
            .entries()
            .len();
        let cancellation = CancellationToken::default();

        for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip] {
            fail_next_worker_spawn();
            let _ = take_worker_spawn_attempts();
            let result = Walker::new(&root)
                .threads(4)
                .cancellation(cancellation.clone())
                .error_policy(policy)
                .collect()
                .expect("a refused helper leaves the walk to the workers that exist");
            assert_eq!(
                result.entries().len(),
                expected,
                "{policy:?}: the walk completes on the caller thread"
            );
            assert!(!result.was_cancelled(), "{policy:?}");
            assert!(
                !cancellation.is_cancelled(),
                "{policy:?}: a refused helper must not cancel a caller-owned token"
            );
            match policy {
                ErrorPolicy::Collect => {
                    let errors = result.errors();
                    assert_eq!(errors.len(), 1, "{errors:?}");
                    assert_eq!(errors[0].operation().as_str(), "spawn_worker");
                    assert_eq!(errors[0].path(), PathBuf::from(&root));
                }
                _ => assert!(
                    result.errors().is_empty(),
                    "{policy:?} drops the recoverable spawn error: {:?}",
                    result.errors()
                ),
            }
            assert_eq!(
                take_worker_spawn_attempts(),
                1,
                "{policy:?}: one refusal caps the pool for the rest of the walk"
            );
        }
        let _ = fs::remove_dir_all(&root);
    }

    /// The deques hold tasks inline and double up to 65,536 slots on a wide
    /// listing, so the rare continuation must not set the size of every
    /// queued directory. 352 bytes per task, before the continuation moved
    /// behind a pointer, cost a flat 100,000-directory tree five times its
    /// result in peak heap.
    #[test]
    fn a_queued_directory_costs_no_more_than_its_task() {
        assert_eq!(
            std::mem::size_of::<super::ParallelTask>(),
            std::mem::size_of::<crate::DirectoryTask>(),
            "the continuation payload must stay behind a pointer"
        );
        assert!(
            std::mem::size_of::<super::ParallelTask>() <= 128,
            "a task is {} bytes",
            std::mem::size_of::<super::ParallelTask>()
        );
    }

    #[test]
    fn worker_start_failure_aborts_under_abort_without_cancelling_the_token() {
        let root = unique_root("worker-start-abort");
        create_wide_fixture(&root);
        let cancellation = CancellationToken::default();
        fail_next_worker_spawn();

        let error = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .error_policy(ErrorPolicy::Abort)
            .collect()
            .expect_err("under Abort the refused helper ends the walk");
        assert_eq!(error.operation().as_str(), "spawn_worker");
        assert_eq!(error.path(), PathBuf::from(&root));
        assert!(
            !cancellation.is_cancelled(),
            "a startup failure must not cancel a caller-owned token"
        );
        let reused = Walker::new(&root)
            .threads(4)
            .cancellation(cancellation.clone())
            .collect()
            .expect("the same token can drive a later walk");
        let _ = fs::remove_dir_all(&root);
        assert!(!reused.was_cancelled());
    }

    /// `Verdict::Stop` must halt the worker's current listing immediately rather
    /// than classifying up to `CANCELLATION_STRIDE - 1` more entries, and must
    /// never fall through to draining deferred directory errors (even on the
    /// last entry or on a listing longer than `LISTING_BATCH_SIZE`).
    #[test]
    fn verdict_stop_halts_the_current_listing_immediately_and_skips_deferred_errors() {
        struct FlatListingBackend {
            root: PathBuf,
            entries: Vec<(String, bool)>,
            deferred_error: bool,
            dir_reads: AtomicUsize,
        }

        impl crate::DirectoryBackend for FlatListingBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut crate::Listing,
            ) -> std::io::Result<()> {
                self.dir_reads.fetch_add(1, Ordering::Relaxed);
                assert_eq!(
                    path, self.root,
                    "child directory must not be read after Stop"
                );
                listing.clear();
                for (name, is_dir) in &self.entries {
                    listing.push(name.as_ref(), *is_dir, false);
                }
                if self.deferred_error {
                    listing.defer_error(
                        self.root.join("deferred-entry"),
                        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "deferred"),
                    );
                }
                Ok(())
            }
        }

        let root = PathBuf::from("/parallel-verdict-stop");

        // Stop on entry 0 (both file and directory) within a single stride:
        // visitor must be called exactly once and no child directory read.
        for first_is_dir in [false, true] {
            let mut entries = vec![("first".to_owned(), first_is_dir)];
            for index in 1..20 {
                entries.push((format!("entry-{index:02}"), index % 2 == 0));
            }
            let backend = FlatListingBackend {
                root: root.clone(),
                entries,
                deferred_error: true,
                dir_reads: AtomicUsize::new(0),
            };
            let visitor_calls = AtomicUsize::new(0);
            let result = Walker::new(&root)
                .threads(4)
                .error_policy(ErrorPolicy::Abort)
                .walk(&backend, &|_| {
                    visitor_calls.fetch_add(1, Ordering::Relaxed);
                    crate::Verdict::Stop
                })
                .expect("Verdict::Stop must not turn into an abort error");
            assert_eq!(
                visitor_calls.load(Ordering::Relaxed),
                1,
                "first_is_dir={first_is_dir}: visitor must not run after Stop"
            );
            assert_eq!(backend.dir_reads.load(Ordering::Relaxed), 1);
            assert!(result.entries().is_empty());
            assert!(result.errors().is_empty());
            assert!(result.was_cancelled());
        }

        // Stop on the very last entry of a directory carrying a deferred error,
        // under both Abort and Collect, and also on a listing wider than
        // LISTING_BATCH_SIZE where a continuation would otherwise be queued.
        for (count, stop_at) in [(3, 2), (super::LISTING_BATCH_SIZE + 8, 5)] {
            for policy in [ErrorPolicy::Abort, ErrorPolicy::Collect] {
                let entries = (0..count)
                    .map(|index| (format!("file-{index:04}.txt"), false))
                    .collect();
                let backend = FlatListingBackend {
                    root: root.clone(),
                    entries,
                    deferred_error: true,
                    dir_reads: AtomicUsize::new(0),
                };
                let visitor_calls = AtomicUsize::new(0);
                let result = Walker::new(&root)
                    .threads(4)
                    .error_policy(policy)
                    .walk(&backend, &|_| {
                        let seen = visitor_calls.fetch_add(1, Ordering::Relaxed);
                        if seen == stop_at {
                            crate::Verdict::Stop
                        } else {
                            crate::Verdict::Keep
                        }
                    })
                    .expect("Stop must not drain deferred errors under Abort");
                assert_eq!(
                    visitor_calls.load(Ordering::Relaxed),
                    stop_at + 1,
                    "count={count} policy={policy:?}"
                );
                assert_eq!(result.entries().len(), stop_at);
                assert!(
                    result.errors().is_empty(),
                    "count={count} policy={policy:?}: deferred errors must be skipped after Stop"
                );
                assert!(result.was_cancelled());
            }
        }
    }

    /// Under `ErrorPolicy::Abort` or when a streaming consumer has closed its
    /// channel, `EntryAction::Failed` must record the error before scheduling
    /// any descent and halt the listing immediately without classifying
    /// subsequent entries or descending into children.
    #[test]
    fn abort_and_closed_sink_stop_immediately_without_scheduling_descent() {
        struct FailingMetadataBackend {
            root: PathBuf,
            dir_reads: AtomicUsize,
            metadata_calls: AtomicUsize,
        }

        impl crate::DirectoryBackend for FailingMetadataBackend {
            fn read_directory(
                &self,
                path: &Path,
                _follow_symlinks: bool,
                _refuse_final_symlink: bool,
                listing: &mut crate::Listing,
            ) -> std::io::Result<()> {
                self.dir_reads.fetch_add(1, Ordering::Relaxed);
                assert_eq!(
                    path, self.root,
                    "no child directory may be read after abort or closed sink"
                );
                listing.clear();
                for index in 0..20 {
                    listing.push(format!("dir-{index:02}").as_ref(), true, false);
                }
                Ok(())
            }

            fn symlink_metadata(&self, _path: &Path) -> std::io::Result<fs::Metadata> {
                self.metadata_calls.fetch_add(1, Ordering::Relaxed);
                Err(std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected metadata failure",
                ))
            }
        }

        let root = PathBuf::from("/parallel-abort-order");

        // ErrorPolicy::Abort: error on dir-00 stops before dir-01..dir-19 and
        // never schedules dir-00 for descent.
        let backend = FailingMetadataBackend {
            root: root.clone(),
            dir_reads: AtomicUsize::new(0),
            metadata_calls: AtomicUsize::new(0),
        };
        let error = Walker::new(&root)
            .threads(4)
            .options(WalkOptions::default().metadata(true))
            .error_policy(ErrorPolicy::Abort)
            .collect_with(&backend)
            .expect_err("metadata failure aborts the walk");
        assert_eq!(error.operation().as_str(), "symlink_metadata");
        assert_eq!(error.path(), root.join("dir-00"));
        assert_eq!(
            backend.metadata_calls.load(Ordering::Relaxed),
            1,
            "Abort must stop immediately instead of classifying remaining entries in the stride"
        );
        assert_eq!(
            backend.dir_reads.load(Ordering::Relaxed),
            1,
            "descend must not be scheduled before recording the abort error"
        );

        // Closed StreamSink under ErrorPolicy::Collect: failed send on dir-00's
        // error stops immediately without scheduling dir-00 or classifying
        // dir-01..dir-19.
        let backend = FailingMetadataBackend {
            root: root.clone(),
            dir_reads: AtomicUsize::new(0),
            metadata_calls: AtomicUsize::new(0),
        };
        let walker = Arc::new(
            Walker::new(&root)
                .threads(4)
                .options(WalkOptions::default().metadata(true))
                .error_policy(ErrorPolicy::Collect),
        );
        let (sink, receiver) = crate::parallel_stream::test_sink(1);
        drop(receiver);
        let mut shared = Shared::new(Arc::clone(&walker), &backend, &crate::keep_every_entry);
        shared.sink = Some(sink);
        let shared = Arc::new(shared);
        let result = super::run(&walker, &backend, shared)
            .expect("closed stream sink stops cleanly without abort error");
        assert!(result.was_cancelled());
        assert_eq!(
            backend.metadata_calls.load(Ordering::Relaxed),
            1,
            "closed sink must stop immediately on the failing entry"
        );
        assert_eq!(
            backend.dir_reads.load(Ordering::Relaxed),
            1,
            "descend must not be scheduled after a closed sink stops the walk"
        );
    }

    /// Direct unit test of `act()` on `EntryAction::Failed { descend: Some(..) }`:
    /// under `ErrorPolicy::Abort` or a closed `StreamSink`, the error is
    /// recorded first and no child task is pushed onto the worker queue,
    /// shared injector, or coordinator pending count (and no helper spawn is
    /// attempted). Under `ErrorPolicy::Collect` with no closed sink, the child
    /// task is scheduled as expected.
    #[test]
    fn act_leaves_no_child_task_queued_on_abort_or_closed_sink() {
        use crate::{
            AncestorChain, DirectoryOpen, WalkOperation,
            classify::{DirectoryTask, EntryAction, EntryFailure},
            gitignore::IgnoreScope,
        };

        let root = PathBuf::from("/parallel-act-failed");
        let make_action = || EntryAction::Failed {
            failure: EntryFailure {
                operation: WalkOperation::SymlinkMetadata,
                path: root.join("dir-00"),
                source: std::io::Error::new(
                    std::io::ErrorKind::PermissionDenied,
                    "injected metadata failure",
                ),
            },
            descend: Some(DirectoryTask {
                path: root.join("dir-00"),
                open: DirectoryOpen::default(),
                depth: 1,
                root: 0,
                ancestors: AncestorChain::default(),
                ignores: IgnoreScope::default(),
            }),
        };

        // 1. ErrorPolicy::Abort: `act()` returns `false` and queues nothing.
        let walker = Arc::new(
            Walker::new(&root)
                .threads(4)
                .error_policy(ErrorPolicy::Abort),
        );
        let shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        let mut worker = super::WorkerScratch::new(0);
        let stealer = worker.queue.stealer();
        let _ = take_worker_spawn_attempts();
        assert!(
            !super::act(&shared, &mut worker, make_action()),
            "ErrorPolicy::Abort must stop the current listing"
        );
        assert!(shared.should_stop());
        assert!(worker.queue.is_empty());
        assert!(worker.queue.pop().is_none());
        assert!(stealer.is_empty());
        assert!(shared.scheduler.is_empty());
        assert_eq!(
            shared.coordinator.pending(),
            0,
            "Abort must not schedule the failed entry's descent task"
        );
        assert_eq!(take_worker_spawn_attempts(), 0);
        assert!(lock(&shared.abort_error).is_some());

        // 2. Closed StreamSink under ErrorPolicy::Collect: `act()` returns
        // `false` and queues nothing.
        let walker = Arc::new(
            Walker::new(&root)
                .threads(4)
                .error_policy(ErrorPolicy::Collect),
        );
        let (sink, receiver) = crate::parallel_stream::test_sink(1);
        drop(receiver);
        let mut shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        shared.sink = Some(sink);
        let mut worker = super::WorkerScratch::new(0);
        let stealer = worker.queue.stealer();
        let _ = take_worker_spawn_attempts();
        assert!(
            !super::act(&shared, &mut worker, make_action()),
            "a closed stream sink must stop the current listing"
        );
        assert!(shared.should_stop());
        assert!(worker.queue.is_empty());
        assert!(worker.queue.pop().is_none());
        assert!(stealer.is_empty());
        assert!(shared.scheduler.is_empty());
        assert_eq!(
            shared.coordinator.pending(),
            0,
            "a closed sink must not schedule the failed entry's descent task"
        );
        assert_eq!(take_worker_spawn_attempts(), 0);

        // 3. Control case: ErrorPolicy::Collect without a closed sink records
        // the error and does schedule the descent task.
        let walker = Arc::new(
            Walker::new(&root)
                .threads(4)
                .error_policy(ErrorPolicy::Collect),
        );
        let shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        let mut worker = super::WorkerScratch::new(0);
        assert!(super::act(&shared, &mut worker, make_action()));
        assert!(!shared.should_stop());
        assert_eq!(shared.coordinator.pending(), 1);
        assert!(worker.queue.pop().is_some());
        assert_eq!(lock(&shared.errors).len(), 1);
    }

    /// Many flat roots (each with zero subdirectories) must still widen and
    /// parallelize across roots once the combined work and waiting roots clear
    /// the helper floor, rather than walking every root serially on the caller.
    #[test]
    fn many_flat_roots_widen_and_parallelize_across_roots() {
        let _rendezvous = lock(&WORKER_RENDEZVOUS_GUARD);
        let base = unique_root("many-flat-roots");
        let roots: Vec<PathBuf> = (0..32)
            .map(|index| {
                let dir = base.join(format!("root-{index:02}"));
                fs::create_dir_all(&dir).expect("create flat root");
                for file in 0..12 {
                    fs::write(dir.join(format!("file-{file:02}.txt")), b"flat")
                        .expect("write flat file");
                }
                dir
            })
            .collect();

        let build_walker = |threads| {
            let mut walker = Walker::new(&roots[0])
                .threads(threads)
                .options(WalkOptions::default().sort(true));
            for root in &roots[1..] {
                walker = walker.add_root(root).expect("add flat root");
            }
            walker
        };

        let serial = build_walker(1).collect().expect("serial walk succeeds");
        expect_worker_threads(roots[0].clone(), 4);
        let parallel = build_walker(4).collect().expect("parallel walk succeeds");
        let observed = observed_worker_threads();
        let _ = fs::remove_dir_all(&base);

        assert_eq!(
            observed, 4,
            "many flat roots must widen and distribute remaining roots across helpers"
        );
        assert_eq!(walked_paths(&parallel), walked_paths(&serial));
        assert_eq!(parallel.entries().len(), 32 * 12);
        assert!(parallel.errors().is_empty());
    }

    /// Verifies multi-root parallel walks across `collect`, `visit`, `stream`,
    /// `stream_parallel`, `ErrorPolicy::{Collect, Skip, Abort}`, `Verdict::Stop`,
    /// `same_file_system`, and pre-cancellation.
    #[test]
    fn multi_root_parallel_preserves_semantics_across_frontends_and_error_policies() {
        let base = unique_root("multi-root-semantics");
        let roots: Vec<PathBuf> = (0..16)
            .map(|index| {
                let dir = base.join(format!("r-{index:02}"));
                fs::create_dir_all(dir.join("sub")).expect("create root subdir");
                for file in 0..10 {
                    fs::write(dir.join(format!("top-{file:02}.rs")), b"top")
                        .expect("write top file");
                    fs::write(dir.join("sub").join(format!("leaf-{file:02}.rs")), b"leaf")
                        .expect("write leaf file");
                }
                dir
            })
            .collect();
        let missing = base.join("missing-root");

        let make_valid_walker = |threads| {
            let mut walker = Walker::new(&roots[0]).threads(threads);
            for root in &roots[1..] {
                walker = walker.add_root(root).expect("add root");
            }
            walker
        };

        // 1. collect (sorted + same_file_system), visit, stream, stream_parallel
        // all agree on the exact entry set.
        let serial = make_valid_walker(1)
            .options(WalkOptions::default().sort(true).same_file_system(true))
            .collect()
            .expect("serial multi-root collect succeeds");
        let parallel = make_valid_walker(4)
            .options(WalkOptions::default().sort(true).same_file_system(true))
            .collect()
            .expect("parallel multi-root collect succeeds");
        assert_eq!(walked_paths(&parallel), walked_paths(&serial));
        assert!(parallel.errors().is_empty());

        let visited = make_valid_walker(4)
            .options(WalkOptions::default().sort(true))
            .visit(|entry| {
                if entry.path().extension().and_then(|e| e.to_str()) == Some("rs") {
                    crate::Verdict::Keep
                } else {
                    crate::Verdict::Skip
                }
            })
            .expect("parallel multi-root visit succeeds");
        assert_eq!(visited.entries().len(), 16 * 20);

        // Verdict::Stop stops early and marks the walk cancelled.
        let stopped = make_valid_walker(4)
            .visit(|_| crate::Verdict::Stop)
            .expect("Verdict::Stop returns Ok");
        assert!(stopped.was_cancelled());
        assert!(stopped.entries().is_empty());

        let mut streamed: Vec<PathBuf> = make_valid_walker(1)
            .stream()
            .map(|item| item.expect("stream item").path().to_path_buf())
            .collect();
        streamed.sort();
        let mut parallel_streamed: Vec<PathBuf> = make_valid_walker(4)
            .stream_parallel()
            .map(|item| item.expect("parallel stream item").path().to_path_buf())
            .collect();
        parallel_streamed.sort();
        assert_eq!(streamed, walked_paths(&serial));
        assert_eq!(parallel_streamed, walked_paths(&serial));

        // 2. ErrorPolicy::{Collect, Skip, Abort} with a missing root queued
        // among valid roots.
        let make_failing_walker = |threads, policy| {
            let mut walker = Walker::new(&roots[0]).threads(threads).error_policy(policy);
            for root in &roots[1..8] {
                walker = walker.add_root(root).expect("add root");
            }
            walker = walker.add_root(&missing).expect("add missing root");
            for root in &roots[8..] {
                walker = walker.add_root(root).expect("add root");
            }
            walker
        };

        for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip] {
            let collected = make_failing_walker(4, policy)
                .options(WalkOptions::default().sort(true))
                .collect()
                .expect("Collect/Skip returns Ok");
            assert_eq!(walked_paths(&collected), walked_paths(&serial));
            assert_eq!(
                collected.errors().len(),
                1,
                "{policy:?} must keep the missing root error"
            );
            assert_eq!(collected.errors()[0].path(), missing);

            let mut ps_entries = Vec::new();
            let mut ps_errors = Vec::new();
            for item in make_failing_walker(4, policy).stream_parallel() {
                match item {
                    Ok(entry) => ps_entries.push(entry.path().to_path_buf()),
                    Err(error) => ps_errors.push(error.path().to_path_buf()),
                }
            }
            ps_entries.sort();
            assert_eq!(ps_entries, walked_paths(&serial));
            assert_eq!(ps_errors, vec![missing.clone()]);
        }

        let aborted = make_failing_walker(4, ErrorPolicy::Abort)
            .collect()
            .expect_err("Abort must return the missing root error");
        assert_eq!(aborted.path(), missing);

        let stream_abort_errors: Vec<PathBuf> = make_failing_walker(4, ErrorPolicy::Abort)
            .stream_parallel()
            .filter_map(Result::err)
            .map(|error| error.path().to_path_buf())
            .collect();
        assert_eq!(stream_abort_errors, vec![missing.clone()]);

        // 3. Pre-cancelled multi-root walk performs no work and marks cancelled.
        let token = CancellationToken::default();
        token.cancel();
        let cancelled = make_failing_walker(4, ErrorPolicy::Abort)
            .cancellation(token)
            .collect()
            .expect("pre-cancelled walk returns Ok without hitting missing root");
        assert!(cancelled.was_cancelled());
        assert!(cancelled.entries().is_empty());
        assert!(cancelled.errors().is_empty());

        let _ = fs::remove_dir_all(&base);
    }

    /// Once every helper slot has been spawned, `helpers_capped` is set,
    /// `spawned_workers` tracks the spawned prefix, and `work_seen` stops
    /// updating on subsequent directories. Also verifies `try_take` skips
    /// stealers beyond `spawned_workers`.
    #[test]
    fn helpers_capped_and_spawned_workers_bound_contention_and_steal_probes() {
        let root = unique_root("helpers-capped");
        create_wide_fixture(&root);

        let walker = Arc::new(Walker::new(&root).threads(4));
        let shared = Arc::new(Shared::new(
            Arc::clone(&walker),
            &crate::SystemBackend,
            &crate::keep_every_entry,
        ));
        let result = super::run(&walker, &crate::SystemBackend, Arc::clone(&shared))
            .expect("wide parallel walk succeeds");
        assert!(result.errors().is_empty());
        assert!(
            shared.helpers_capped.load(Ordering::Acquire),
            "a full pool must set helpers_capped once all helper slots are spawned"
        );
        assert_eq!(shared.spawned_workers.load(Ordering::Acquire), 4);

        // Once capped, processing another directory must not touch `work_seen`.
        let frozen_work = shared.work_seen.load(Ordering::Acquire);
        let mut scratch = super::WorkerScratch::new(0);
        let root_task = walker
            .build_root_task(&crate::SystemBackend, 0)
            .expect("root task")
            .task;
        super::walk_directory(
            &shared,
            &mut scratch,
            super::ParallelTask::Directory(root_task),
            None,
        );
        assert_eq!(
            shared.work_seen.load(Ordering::Acquire),
            frozen_work,
            "work_seen must not be incremented after helpers_capped is set"
        );

        // Verify `try_take` only probes `&stealers[..spawned_workers]`.
        let walker = Arc::new(Walker::new(&root).threads(4));
        let shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        let w0 = super::WorkerScratch::new(0);
        let w1 = super::WorkerScratch::new(1);
        let w2 = super::WorkerScratch::new(2);
        let w3 = super::WorkerScratch::new(3);
        let stealers = [
            w0.queue.stealer(),
            w1.queue.stealer(),
            w2.queue.stealer(),
            w3.queue.stealer(),
        ];
        // Push a task only into unspawned worker 3's queue while
        // `spawned_workers` is 2 (workers 0 and 1 active).
        let dummy_task = || {
            super::ParallelTask::Directory(crate::classify::DirectoryTask {
                path: root.clone(),
                open: crate::DirectoryOpen::default(),
                depth: 1,
                root: 0,
                ancestors: crate::AncestorChain::default(),
                ignores: crate::gitignore::IgnoreScope::default(),
            })
        };
        shared.spawned_workers.store(2, Ordering::Release);
        w3.queue.push(dummy_task());
        assert!(
            super::try_take(&shared, &w0, &stealers).is_none(),
            "try_take must not probe stealers beyond spawned_workers"
        );
        // Once worker 1 has a task, worker 0 steals it immediately.
        w1.queue.push(dummy_task());
        assert!(
            super::try_take(&shared, &w0, &stealers).is_some(),
            "try_take must probe active stealers within spawned_workers"
        );
        // Raising spawned_workers to 4 makes worker 3's queue visible.
        shared.spawned_workers.store(4, Ordering::Release);
        assert!(super::try_take(&shared, &w0, &stealers).is_some());

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn parallel_task_keeps_sparse_variants_boxed_within_directory_task_budget() {
        use std::mem::size_of;

        use crate::classify::DirectoryTask;

        assert_eq!(
            size_of::<super::ParallelTask>(),
            size_of::<DirectoryTask>(),
            "ParallelTask ({}) must share DirectoryTask's size ({}) via niche optimization",
            size_of::<super::ParallelTask>(),
            size_of::<DirectoryTask>()
        );
        #[cfg(not(any(
            all(feature = "native-macos", target_os = "macos"),
            all(feature = "native-linux", target_os = "linux"),
        )))]
        {
            let budget = size_of::<PathBuf>() + 6 * size_of::<usize>();
            assert!(
                size_of::<super::ParallelTask>() <= budget,
                "portable ParallelTask ({}) exceeded PathBuf + 6-word budget ({budget})",
                size_of::<super::ParallelTask>(),
            );
        }
    }

    #[test]
    fn multi_root_enqueue_stops_promptly_on_cancellation() {
        let token = CancellationToken::default();
        let walker = Arc::new(Walker::new(".").threads(4).cancellation(token.clone()));
        let shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        let produced = AtomicUsize::new(0);
        let make_task = |index| crate::classify::RootTask {
            task: crate::classify::DirectoryTask {
                path: PathBuf::from(format!("root-{index}")),
                open: crate::DirectoryOpen::default(),
                depth: 0,
                root: index,
                ancestors: crate::AncestorChain::default(),
                ignores: crate::gitignore::IgnoreScope::default(),
            },
            ignore_errors: Vec::new(),
        };
        let roots = (0..1_000).map(|index| {
            let count = produced.fetch_add(1, Ordering::Relaxed);
            if count == 4 {
                token.cancel();
            }
            Some(make_task(index))
        });

        assert!(
            !super::enqueue_remaining_roots(&shared, roots),
            "cancellation during lazy multi-root enqueue must return false promptly"
        );
        assert_eq!(
            shared.coordinator.pending(),
            4,
            "the root whose build observed cancellation and all later roots must not be scheduled"
        );
        assert_eq!(
            produced.load(Ordering::Relaxed),
            5,
            "lazy iterator must stop without constructing the remaining 995 roots"
        );

        // Also verify `None` returned by `build_root_task` stops immediately.
        let walker = Arc::new(Walker::new(".").threads(4));
        let shared = Shared::new(walker, &crate::SystemBackend, &crate::keep_every_entry);
        let built = AtomicUsize::new(0);
        let roots = (0..1_000).map(|index| {
            let count = built.fetch_add(1, Ordering::Relaxed);
            if count == 3 {
                None
            } else {
                Some(make_task(index))
            }
        });
        assert!(!super::enqueue_remaining_roots(&shared, roots));
        assert_eq!(shared.coordinator.pending(), 3);
        assert_eq!(built.load(Ordering::Relaxed), 4);
    }

    #[test]
    fn multi_root_sparse_root_ignore_errors_preserve_error_policies() {
        let base = unique_root("multi-root-ignore-errors");
        fs::create_dir_all(&base).expect("create multi-root base");
        let base = fs::canonicalize(&base).unwrap_or(base);
        let ok_roots: Vec<PathBuf> = (0..10)
            .map(|index| {
                let dir = base.join(format!("ok-{index:02}"));
                fs::create_dir_all(&dir).expect("create ok root");
                fs::write(dir.join("file.txt"), b"ok").expect("write ok file");
                dir
            })
            .collect();

        // One root inside a repository whose ancestor `.gitignore` exceeds the
        // 100,000-rule safety limit (portable failure across all OSes/users).
        let bad_repo = base.join("bad-repo");
        let bad_walk_root = bad_repo.join("sub").join("walk-root");
        fs::create_dir_all(bad_repo.join(".git")).expect("create .git dir");
        fs::create_dir_all(&bad_walk_root).expect("create bad walk root");
        let bad_gitignore = bad_repo.join(".gitignore");
        fs::write(&bad_gitignore, vec![b'\n'; 100_001]).expect("write oversized .gitignore");
        fs::write(bad_walk_root.join("kept.txt"), b"kept").expect("write kept file");

        // Verify sparse boxing: error-free roots stay unboxed `Directory`,
        // while the root with an ancestor ignore error uses
        // `RootWithIgnoreErrors`.
        let probe_walker = Walker::new(&ok_roots[0])
            .add_root(&bad_walk_root)
            .expect("add bad root")
            .respect_git_ignore(true);
        assert!(matches!(
            super::ParallelTask::from_root(
                probe_walker
                    .build_root_task(&crate::SystemBackend, 0)
                    .expect("first root")
            ),
            super::ParallelTask::Directory(_)
        ));
        assert!(matches!(
            super::ParallelTask::from_root(
                probe_walker
                    .build_root_task(&crate::SystemBackend, 1)
                    .expect("second root")
            ),
            super::ParallelTask::RootWithIgnoreErrors(_)
        ));

        // Test both placements: bad root enqueued as a remaining root AND as
        // the first root.
        for bad_first in [false, true] {
            let make_walker = |policy| {
                let mut walker = if bad_first {
                    let mut w = Walker::new(&bad_walk_root);
                    for ok in &ok_roots {
                        w = w.add_root(ok).expect("add ok root");
                    }
                    w
                } else {
                    let mut w = Walker::new(&ok_roots[0]);
                    for ok in &ok_roots[1..5] {
                        w = w.add_root(ok).expect("add ok root");
                    }
                    w = w.add_root(&bad_walk_root).expect("add bad root");
                    for ok in &ok_roots[5..] {
                        w = w.add_root(ok).expect("add ok root");
                    }
                    w
                };
                walker = walker
                    .threads(4)
                    .respect_git_ignore(true)
                    .error_policy(policy)
                    .options(WalkOptions::default().sort(true));
                walker
            };

            let collected = make_walker(ErrorPolicy::Collect)
                .collect()
                .expect("Collect returns Ok");
            assert_eq!(collected.entries().len(), ok_roots.len() + 1);
            assert_eq!(collected.errors().len(), 1);
            assert_eq!(
                collected.errors()[0].operation(),
                crate::WalkOperation::ReadIgnore
            );
            assert_eq!(collected.errors()[0].path(), bad_gitignore);

            let skipped = make_walker(ErrorPolicy::Skip)
                .collect()
                .expect("Skip returns Ok");
            assert_eq!(skipped.entries().len(), ok_roots.len() + 1);
            assert!(
                skipped.errors().is_empty(),
                "Skip must drop non-root read_ignore errors"
            );

            let aborted = make_walker(ErrorPolicy::Abort)
                .collect()
                .expect_err("Abort returns the root ignore error");
            assert_eq!(aborted.operation(), crate::WalkOperation::ReadIgnore);
            assert_eq!(aborted.path(), bad_gitignore);
        }

        let _ = fs::remove_dir_all(&base);
    }
}
