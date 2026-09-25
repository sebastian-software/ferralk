//! [`Walker::stream_parallel`]: the parallel walk, delivered to the iterating
//! thread through a bounded channel.
//!
//! The walk itself is the one `collect()` runs, on a driver thread that plays
//! the part of `collect()`'s calling thread: it starts alone, adds helpers on
//! the same evidence, and captures panics the same way. Nothing here touches
//! the scheduler's termination or wake-up protocol; the only difference from
//! `collect()` is where a worker puts what it kept.

use std::{
    panic::resume_unwind,
    sync::{
        Arc,
        mpsc::{Receiver, SyncSender, sync_channel},
    },
    thread::{self, JoinHandle},
};

#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use super::{
    CancellationToken, DirectoryBackend, ErrorPolicy, SystemBackend, WalkEntry, WalkError,
    WalkOperation, WalkStream, Walker, parallel,
};

/// Channel messages per configured worker.
///
/// Each message is one batch of about [`parallel::STREAM_BATCH_SIZE`]
/// entries at most, or one error. A few per worker let every worker hand over a batch
/// while the consumer is still busy with an earlier one, and keep what a
/// stalled consumer leaves buffered proportional to the thread budget.
const CHANNEL_MESSAGES_PER_WORKER: usize = 4;

/// What a streaming walk sends from its workers to the iterating thread.
pub(crate) enum StreamMessage {
    Entries(Vec<WalkEntry>),
    Error(WalkError),
}

/// Stops a running walk the way an abort or a panic does, waking its parked
/// workers.
type StopWalk = Box<dyn Fn() + Send + Sync>;

/// How the consumer stops a walk it no longer reads.
///
/// A worker blocked on a full channel learns it from the failed send, but a
/// worker with nothing to send, or one parked waiting for work, would not:
/// the walk's own stop request reaches both, and wakes the parked ones at
/// once instead of at their next cancellation poll. The walk exists only
/// once the driver has built it, so the driver attaches its stop here, and
/// whichever of the two sides comes second carries the stop out.
#[derive(Default)]
pub(crate) struct StreamControl {
    state: std::sync::Mutex<ControlState>,
}

#[derive(Default)]
struct ControlState {
    closed: bool,
    stop_walk: Option<StopWalk>,
}

impl StreamControl {
    fn state(&self) -> std::sync::MutexGuard<'_, ControlState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Called by the driver once the walk exists.
    pub(crate) fn attach(&self, stop_walk: StopWalk) {
        let mut state = self.state();
        if state.closed {
            drop(state);
            stop_walk();
        } else {
            state.stop_walk = Some(stop_walk);
        }
    }

    /// Called by the consumer when it stops reading. Idempotent.
    fn close(&self) {
        let mut state = self.state();
        state.closed = true;
        let stop_walk = state.stop_walk.take();
        drop(state);
        if let Some(stop_walk) = stop_walk {
            stop_walk();
        }
    }
}

impl std::fmt::Debug for StreamControl {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("StreamControl")
            .field("closed", &self.state().closed)
            .finish_non_exhaustive()
    }
}

/// The workers' end of a streaming walk.
pub(crate) struct StreamSink {
    sender: SyncSender<StreamMessage>,
    control: Arc<StreamControl>,
    /// Entries handed to the channel, for the backpressure test.
    #[cfg(test)]
    produced: Arc<AtomicUsize>,
}

impl StreamSink {
    pub(crate) fn control(&self) -> Arc<StreamControl> {
        Arc::clone(&self.control)
    }

    /// Blocks while the channel is full. Returns `false` when the consumer is
    /// gone, which is the walk's cue to stop.
    pub(crate) fn send(&self, message: StreamMessage) -> bool {
        #[cfg(test)]
        if let StreamMessage::Entries(batch) = &message {
            self.produced.fetch_add(batch.len(), Ordering::Relaxed);
        }
        self.sender.send(message).is_ok()
    }
}

/// Incremental traversal produced by [`Walker::stream_parallel`], walked on
/// the configured workers and yielded on the thread that iterates it.
///
/// Items are the same `Result<WalkEntry, WalkError>` a [`WalkStream`] yields,
/// with the same caveat for adapters that count items; see [`WalkStream`] for
/// the idioms. What differs is order and threads:
///
/// - Entries arrive in no particular order. Workers interleave, so a directory
///   is not necessarily yielded before its subtree, and
///   [`WalkOptions::sort`](crate::WalkOptions::sort) does not apply.
/// - An error arrives as soon as a worker meets it, which can be before
///   entries of its own directory and of that directory's siblings.
/// - The walk runs on up to [`Walker::threads`] threads of its own, the
///   iterating thread not among them. With `threads(1)` no thread is started
///   and the stream is exactly [`Walker::stream`].
///
/// Nothing starts until the first call to `next`. Workers pause while the
/// consumer is behind, so a stalled consumer holds a bounded number of
/// entries in memory rather than the rest of the tree.
///
/// Dropping the stream stops the walk and waits for its threads before `drop`
/// returns, so no thread outlives the stream. That wait is short: a worker
/// notices at its next directory, or within 64 entries of a wide one, as it
/// does for cancellation.
///
/// ```
/// use ferralk::Walker;
///
/// # let root = std::env::temp_dir().join(format!("ferralk-doc-parallel-stream-{}", std::process::id()));
/// # let _ = std::fs::remove_dir_all(&root);
/// # std::fs::create_dir_all(root.join("src/parser"))?;
/// # std::fs::write(root.join("src/lib.rs"), "")?;
/// # std::fs::write(root.join("src/parser/mod.rs"), "")?;
/// # std::fs::write(root.join("README.md"), "")?;
/// // Stop at the first Rust file any worker finds. Dropping the stream stops
/// // the rest of the walk and joins its threads.
/// let first = Walker::new(&root)
///     .include("**/*.rs")?
///     .threads(4)
///     .stream_parallel()
///     .find_map(Result::ok);
/// assert!(first.is_some_and(|entry| entry.path().extension() == Some("rs".as_ref())));
/// # std::fs::remove_dir_all(&root)?;
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
#[must_use = "a walk stream does nothing unless iterated"]
#[derive(Debug)]
pub struct ParallelWalkStream {
    state: State,
    /// Whether a cancellation request ended the threaded walk. The serial
    /// forms answer from their own [`WalkStream`].
    cancelled: bool,
}

#[derive(Debug)]
enum State {
    /// Not iterated yet; no thread has been started.
    Idle(Walker),
    /// `threads(1)`, or no thread could be started: the ordinary stream.
    Serial(WalkStream),
    Threaded(Threaded),
    Done,
}

/// The iterating thread's end of a threaded walk.
#[derive(Debug)]
struct Threaded {
    /// `None` once the walk has ended or the consumer let go of it.
    receiver: Option<Receiver<StreamMessage>>,
    control: Arc<StreamControl>,
    /// Runs the walk as its caller worker and joins every helper before it
    /// ends. Returns the error that ended the walk under `ErrorPolicy::Abort`.
    driver: Option<JoinHandle<Result<(), WalkError>>>,
    /// The batch being yielded.
    batch: std::vec::IntoIter<WalkEntry>,
    cancellation: Option<CancellationToken>,
    #[cfg(test)]
    produced: Arc<AtomicUsize>,
}

/// What one step of a threaded walk produced.
enum Step {
    Item(Result<WalkEntry, WalkError>),
    /// The walk is over; `cancelled` says whether a cancellation request ended
    /// it.
    End {
        cancelled: bool,
    },
}

impl ParallelWalkStream {
    pub(crate) fn new(walker: Walker) -> Self {
        let state = if walker.threads > 1 {
            State::Idle(walker)
        } else {
            State::Serial(walker.stream())
        };
        Self {
            state,
            cancelled: false,
        }
    }

    /// Whether a cancellation request ended this stream.
    ///
    /// As for [`WalkStream::was_cancelled`], the token is read before every
    /// item, and a stream that observed it yields nothing further. The
    /// entries the workers had already found are dropped with it.
    ///
    /// That includes the error that ends a walk under
    /// [`ErrorPolicy::Abort`](crate::ErrorPolicy::Abort): when the token fires
    /// as the walk aborts, the stream can observe the request first, end
    /// without yielding the error, and report `true` here. [`WalkStream`]
    /// behaves the same way.
    ///
    /// ```
    /// use ferralk::{CancellationToken, Walker};
    ///
    /// # let root = std::env::temp_dir().join(format!("ferralk-doc-parallel-cancel-{}", std::process::id()));
    /// # let _ = std::fs::remove_dir_all(&root);
    /// # for index in 0..32 {
    /// #     std::fs::create_dir_all(root.join(format!("dir-{index}")))?;
    /// #     std::fs::write(root.join(format!("dir-{index}/file.txt")), "")?;
    /// # }
    /// let token = CancellationToken::default();
    /// let mut stream = Walker::new(&root)
    ///     .threads(4)
    ///     .cancellation(token.clone())
    ///     .stream_parallel();
    /// let first = stream.next();
    /// assert!(first.is_some());
    ///
    /// // Another thread would normally do this.
    /// token.cancel();
    /// assert!(stream.next().is_none());
    /// assert!(stream.was_cancelled());
    /// # drop(stream);
    /// # std::fs::remove_dir_all(&root)?;
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    #[must_use]
    pub fn was_cancelled(&self) -> bool {
        match &self.state {
            State::Serial(stream) => stream.was_cancelled(),
            _ => self.cancelled,
        }
    }

    /// Entries the workers have handed to the channel so far.
    #[cfg(test)]
    pub(crate) fn produced(&self) -> usize {
        match &self.state {
            State::Threaded(threaded) => threaded.produced.load(Ordering::Relaxed),
            _ => 0,
        }
    }

    /// Starts the walk on its driver thread. Returns an item only when a
    /// driver that could not be started has something to report at once.
    fn start(&mut self) -> Option<Result<WalkEntry, WalkError>> {
        let walker = match std::mem::replace(&mut self.state, State::Done) {
            State::Idle(walker) => walker,
            other => {
                self.state = other;
                return None;
            }
        };
        let cancellation = walker.cancellation.clone();
        if cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            self.cancelled = true;
            return None;
        }

        SystemBackend.begin_walk();
        let (sender, receiver) = sync_channel(walker.threads * CHANNEL_MESSAGES_PER_WORKER);
        let control = Arc::new(StreamControl::default());
        #[cfg(test)]
        let produced = Arc::new(AtomicUsize::new(0));
        let sink = StreamSink {
            sender,
            control: Arc::clone(&control),
            #[cfg(test)]
            produced: Arc::clone(&produced),
        };
        // The walker travels to the driver only once it exists, so a refused
        // start leaves it here for the serial fallback.
        let (hand_over, handed_over) = sync_channel::<Walker>(1);
        let driver = match spawn_driver(move || match handed_over.recv() {
            Ok(walker) => parallel::stream(walker, &SystemBackend, sink),
            Err(_) => Ok(()),
        }) {
            Ok(driver) => driver,
            Err(source) => return self.fall_back_to_serial(walker, source),
        };
        if let Err(returned) = hand_over.send(walker) {
            // The driver ended before it took the walker, which it never does
            // on its own. Joining it surfaces a panic if that is why.
            if let Err(payload) = driver.join() {
                resume_unwind(payload);
            }
            return self.fall_back_to_serial(
                returned.0,
                std::io::Error::other("the stream's driver thread ended before the walk"),
            );
        }
        self.state = State::Threaded(Threaded {
            receiver: Some(receiver),
            control,
            driver: Some(driver),
            batch: Vec::new().into_iter(),
            cancellation,
            #[cfg(test)]
            produced,
        });
        None
    }

    /// A thread is an optimization here as it is for `collect()`: the walk
    /// goes on as [`Walker::stream`] on the iterating thread, and the refusal
    /// is one recoverable `spawn_worker` error, kept under `Collect`, dropped
    /// under `Skip`, and the stream's only item under `Abort`.
    fn fall_back_to_serial(
        &mut self,
        walker: Walker,
        source: std::io::Error,
    ) -> Option<Result<WalkEntry, WalkError>> {
        let error = WalkError::new(
            WalkOperation::SpawnWorker,
            walker.roots().next().expect("a walk has a root").into(),
            source,
        );
        match walker.error_policy {
            ErrorPolicy::Abort => {
                self.state = State::Done;
                Some(Err(error))
            }
            ErrorPolicy::Skip => {
                self.state = State::Serial(walker.stream());
                None
            }
            ErrorPolicy::Collect => {
                self.state = State::Serial(walker.stream());
                Some(Err(error))
            }
        }
    }
}

/// A sink over a channel of `capacity` messages, with the receiver the
/// stream would hold.
#[cfg(test)]
pub(crate) fn test_sink(capacity: usize) -> (StreamSink, Receiver<StreamMessage>) {
    let (sender, receiver) = sync_channel(capacity);
    let sink = StreamSink {
        sender,
        control: Arc::new(StreamControl::default()),
        produced: Arc::new(AtomicUsize::new(0)),
    };
    (sink, receiver)
}

/// Closes a sink's stream the way the consumer does.
#[cfg(test)]
pub(crate) fn close_test_sink(control: &StreamControl) {
    control.close();
}

fn spawn_driver(
    body: impl FnOnce() -> Result<(), WalkError> + Send + 'static,
) -> std::io::Result<JoinHandle<Result<(), WalkError>>> {
    #[cfg(test)]
    if parallel::should_fail_next_worker_spawn() {
        return Err(std::io::Error::other("injected worker start failure"));
    }
    thread::Builder::new()
        .name("ferralk-worker".into())
        .spawn(body)
}

impl Iterator for ParallelWalkStream {
    type Item = Result<WalkEntry, WalkError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match &mut self.state {
                State::Serial(stream) => return stream.next(),
                State::Done => return None,
                State::Idle(_) => {
                    if let Some(item) = self.start() {
                        return Some(item);
                    }
                }
                State::Threaded(threaded) => match threaded.step() {
                    Step::Item(item) => return Some(item),
                    Step::End { cancelled } => {
                        self.cancelled = cancelled;
                        self.state = State::Done;
                        return None;
                    }
                },
            }
        }
    }
}

impl Threaded {
    fn is_cancelled(&self) -> bool {
        self.cancellation
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
    }

    fn step(&mut self) -> Step {
        // Read before every item, as `WalkStream` does, so nothing the
        // workers found before the request is yielded after it.
        // An `Abort` error the walk may have ended on meanwhile goes unyielded
        // too: once the request is seen, the stream yields nothing further.
        if self.is_cancelled() {
            let _ = self.finish();
            return Step::End { cancelled: true };
        }
        if let Some(entry) = self.batch.next() {
            return Step::Item(Ok(entry));
        }
        let Some(receiver) = &self.receiver else {
            return Step::End { cancelled: false };
        };
        loop {
            match receiver.recv() {
                Ok(StreamMessage::Entries(batch)) => {
                    self.batch = batch.into_iter();
                    if let Some(entry) = self.batch.next() {
                        return Step::Item(Ok(entry));
                    }
                }
                Ok(StreamMessage::Error(error)) => return Step::Item(Err(error)),
                // Every sender is gone, so the walk is over and its threads
                // are ending. The driver says how it ended.
                Err(_) => {
                    return match self.finish() {
                        Err(error) => Step::Item(Err(error)),
                        Ok(()) => Step::End {
                            cancelled: self.is_cancelled(),
                        },
                    };
                }
            }
        }
    }

    /// Lets go of the channel and joins the driver, which has joined every
    /// helper by the time it returns. A panic inside the walk resumes here.
    fn finish(&mut self) -> Result<(), WalkError> {
        self.close();
        match self.driver.take().map(JoinHandle::join) {
            None | Some(Ok(Ok(()))) => Ok(()),
            Some(Ok(Err(error))) => Err(error),
            Some(Err(payload)) => resume_unwind(payload),
        }
    }

    /// Stops the walk without waiting for it: the walk's own stop reaches
    /// workers with nothing to send and wakes parked ones at once, and
    /// dropping the receiver fails the send of every worker blocked on a full
    /// channel.
    fn close(&mut self) {
        self.control.close();
        self.receiver = None;
        self.batch = Vec::new().into_iter();
    }
}

impl Drop for Threaded {
    fn drop(&mut self) {
        self.close();
        if let Some(driver) = self.driver.take()
            && let Err(payload) = driver.join()
            && !thread::panicking()
        {
            // A panic inside the walk reaches the caller even when the
            // caller stopped reading, as it would from `collect()`.
            resume_unwind(payload);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        fs,
        panic::{AssertUnwindSafe, catch_unwind},
        path::PathBuf,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::{Duration, SystemTime, UNIX_EPOCH},
    };

    use super::{ParallelWalkStream, parallel};
    use crate::{CancellationToken, ErrorPolicy, WalkOptions, Walker};

    /// A hang is the regression most of these tests guard against, so each
    /// walk runs on a thread of its own and the test gives up after this long
    /// instead of blocking the suite.
    const HANG_TIMEOUT: Duration = Duration::from_secs(60);

    static NEXT_ROOT: AtomicUsize = AtomicUsize::new(0);

    struct Tree(PathBuf);

    impl Tree {
        fn new(label: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "ferralk-stream-{label}-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after epoch")
                    .as_nanos(),
                NEXT_ROOT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&root).expect("create fixture root");
            Self(root)
        }

        /// `branches` directories of `nested` directories of `files` files.
        /// At ten by four by four it is well past the helper floor, so the
        /// walk really runs on several workers.
        fn wide(label: &str, branches: usize, nested: usize, files: usize) -> Self {
            let tree = Self::new(label);
            for branch in 0..branches {
                for inner in 0..nested {
                    let directory = tree
                        .0
                        .join(format!("branch-{branch}"))
                        .join(format!("nested-{inner}"));
                    fs::create_dir_all(&directory).expect("create fixture directory");
                    for file in 0..files {
                        fs::write(directory.join(format!("file-{file}.txt")), b"fixture")
                            .expect("write fixture file");
                    }
                }
            }
            tree
        }
    }

    impl Drop for Tree {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn collected(walker: Walker) -> BTreeSet<PathBuf> {
        walker
            .threads(1)
            .collect()
            .expect("reference walk succeeds")
            .entries()
            .iter()
            .map(|entry| entry.path().to_path_buf())
            .collect()
    }

    fn streamed(stream: ParallelWalkStream) -> BTreeSet<PathBuf> {
        let mut paths = BTreeSet::new();
        for item in stream {
            let entry = item.expect("the fixture walks without errors");
            assert!(
                paths.insert(entry.path().to_path_buf()),
                "{} was yielded twice",
                entry.path().display()
            );
        }
        paths
    }

    /// Runs `work` on a thread of its own and fails the test if it does not
    /// finish in time. A panic inside `work` is the test's failure, not a hang.
    fn without_hanging<T: Send + 'static>(
        label: &str,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> T {
        let (sender, receiver) = mpsc::channel();
        let runner = thread::Builder::new()
            .name(format!("ferralk-stream-test-{label}"))
            .spawn(move || {
                let _ = sender.send(work());
            })
            .expect("spawn the test thread");
        match receiver.recv_timeout(HANG_TIMEOUT) {
            Ok(value) => {
                runner.join().expect("the test thread joins");
                value
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => match runner.join() {
                Err(payload) => std::panic::resume_unwind(payload),
                Ok(()) => panic!("{label}: the test thread ended without a result"),
            },
            Err(mpsc::RecvTimeoutError::Timeout) => panic!("{label}: the stream hung"),
        }
    }

    #[test]
    fn a_parallel_stream_can_move_to_another_thread() {
        fn assert_send<T: Send>() {}
        assert_send::<ParallelWalkStream>();
    }

    #[test]
    fn every_thread_budget_yields_the_collected_set() {
        let tree = Tree::wide("parity", 10, 4, 4);
        let expected = collected(Walker::new(&tree.0));
        for threads in [1, 2, 8] {
            let root = tree.0.clone();
            let actual = without_hanging("parity", move || {
                streamed(Walker::new(&root).threads(threads).stream_parallel())
            });
            assert_eq!(actual, expected, "{threads} threads");
        }
    }

    /// The rendezvous holds every worker until four have arrived, so passing
    /// it proves the stream's walk went as wide as `collect()`'s does. The
    /// consumer keeps reading meanwhile, which is what lets the held workers'
    /// batches through.
    #[test]
    fn a_wide_tree_is_streamed_by_the_whole_pool() {
        let _rendezvous = parallel::lock(&parallel::WORKER_RENDEZVOUS_GUARD);
        let tree = Tree::wide("pool", 10, 4, 4);
        let expected = collected(Walker::new(&tree.0));

        parallel::expect_worker_threads(tree.0.clone(), 4);
        let root = tree.0.clone();
        let actual = without_hanging("pool", move || {
            streamed(Walker::new(&root).threads(4).stream_parallel())
        });
        assert_eq!(parallel::observed_worker_threads(), 4);
        assert_eq!(actual, expected);
    }

    #[test]
    fn threads_one_is_the_serial_stream_item_for_item() {
        let tree = Tree::wide("serial", 3, 3, 3);
        let serial = Walker::new(&tree.0)
            .stream()
            .map(|item| item.expect("walks").path().to_path_buf())
            .collect::<Vec<_>>();
        let parallel = Walker::new(&tree.0)
            .threads(1)
            .stream_parallel()
            .map(|item| item.expect("walks").path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(parallel, serial, "the same items in the same order");
    }

    /// Dropping after a few items, with workers mid-walk and some of them
    /// blocked on a full channel, must stop the walk and join its threads
    /// rather than hang. Repeated so the drop lands at different moments.
    #[test]
    fn dropping_early_stops_the_walk_without_hanging() {
        let tree = Tree::wide("early-drop", 12, 6, 6);
        let expected = collected(Walker::new(&tree.0)).len();
        let root = tree.0.clone();
        without_hanging("early-drop", move || {
            for round in 0..60 {
                let threads = [2, 4, 8][round % 3];
                let mut stream = Walker::new(&root).threads(threads).stream_parallel();
                for _ in 0..round % 7 {
                    stream
                        .next()
                        .expect("the tree holds more than seven entries")
                        .expect("walks");
                }
                if round % 5 == 0 {
                    // Let the workers fill the channel and block on it first.
                    thread::sleep(Duration::from_millis(5));
                }
                assert!(!stream.was_cancelled());
                drop(stream);
                if round % 10 == 0 {
                    let all = Walker::new(&root)
                        .threads(threads)
                        .stream_parallel()
                        .count();
                    assert_eq!(all, expected, "round {round}: a full stream after a drop");
                }
            }
        });
    }

    /// A consumer that stops reading pauses the walk: what the workers hand
    /// over is bounded by the channel, one batch per worker blocked on it, and
    /// the consumer's own batch - not by the tree. Reading on afterwards still
    /// yields every entry.
    #[test]
    fn a_stalled_consumer_pauses_the_walk() {
        const THREADS: usize = 2;
        // One flat directory, so the batches are full ones and the bound is
        // the one that matters; wide enough to overrun it twice.
        let tree = Tree::new("stalled");
        for file in 0..6_000 {
            fs::write(tree.0.join(format!("file-{file:04}.txt")), b"").expect("write file");
        }
        let expected = collected(Walker::new(&tree.0));
        let root = tree.0.clone();
        let (produced, actual) = without_hanging("stalled", move || {
            let mut stream = Walker::new(&root).threads(THREADS).stream_parallel();
            let first = stream.next().expect("an entry").expect("walks");
            thread::sleep(Duration::from_millis(200));
            let produced = stream.produced();
            let mut actual = streamed(stream);
            actual.insert(first.path().to_path_buf());
            (produced, actual)
        });
        // A batch is checked at the cancellation stride, so it can overrun its
        // size by up to one stride.
        let bound = (THREADS * super::CHANNEL_MESSAGES_PER_WORKER + THREADS + 1)
            * (parallel::STREAM_BATCH_SIZE + crate::CANCELLATION_STRIDE);
        assert!(
            produced <= bound,
            "{produced} entries handed over while the consumer stalled, bound {bound}"
        );
        assert!(produced < expected.len(), "the walk did not pause at all");
        assert_eq!(actual, expected);
    }

    #[test]
    fn a_cancelled_token_ends_the_stream_and_is_reported() {
        let tree = Tree::wide("cancel", 10, 4, 4);
        let root = tree.0.clone();
        without_hanging("cancel", move || {
            // Cancelled before the first item: nothing starts.
            let token = CancellationToken::default();
            token.cancel();
            let mut stream = Walker::new(&root)
                .threads(4)
                .cancellation(token)
                .stream_parallel();
            assert!(stream.next().is_none());
            assert!(stream.was_cancelled());
            assert_eq!(stream.produced(), 0, "no thread started");

            // Cancelled mid-walk: nothing after the request.
            let token = CancellationToken::default();
            let mut stream = Walker::new(&root)
                .threads(4)
                .cancellation(token.clone())
                .stream_parallel();
            for _ in 0..5 {
                stream.next().expect("an entry").expect("walks");
            }
            assert!(!stream.was_cancelled());
            token.cancel();
            assert!(stream.next().is_none());
            assert!(stream.next().is_none(), "the stream stays ended");
            assert!(stream.was_cancelled());
            assert!(token.is_cancelled(), "the caller's token is left as it is");
        });
    }

    /// A panic inside a worker resumes on the consuming thread, and leaves a
    /// caller-owned token reusable, exactly as it does from `collect()`.
    #[test]
    fn a_worker_panic_resumes_on_the_consumer() {
        for round in 0..3 {
            let tree = Tree::wide("panic", 10, 4, 4);
            parallel::panic_in_directory(tree.0.join("branch-3").join("nested-2"));
            let token = CancellationToken::default();
            let root = tree.0.clone();
            let walk_token = token.clone();
            let panicked = without_hanging("panic", move || {
                catch_unwind(AssertUnwindSafe(|| {
                    Walker::new(&root)
                        .threads(4)
                        .cancellation(walk_token)
                        .stream_parallel()
                        .count()
                }))
                .is_err()
            });
            assert!(panicked, "round {round}: the worker panic must resume");
            assert!(!token.is_cancelled(), "round {round}");
        }
    }

    /// A consumer that stopped reading still learns of a panic: dropping the
    /// stream joins the walk and resumes it, as `collect()` would have.
    #[test]
    fn a_worker_panic_resumes_from_drop() {
        let tree = Tree::new("panic-drop");
        fs::write(tree.0.join("first.txt"), b"").expect("write file");
        let panics = tree.0.join("panics");
        fs::create_dir_all(&panics).expect("create directory");
        parallel::panic_in_directory(panics.clone());
        let root = tree.0.clone();
        let panicked = without_hanging("panic-drop", move || {
            let mut stream = Walker::new(&root).threads(4).stream_parallel();
            stream.next().expect("an entry").expect("walks");
            // The tree is below the helper floor, so the driver reads the root
            // and then the panicking directory; wait until it has.
            while parallel::panic_in_directory_is_armed(&panics) {
                thread::sleep(Duration::from_millis(1));
            }
            catch_unwind(AssertUnwindSafe(move || drop(stream))).is_err()
        });
        assert!(panicked, "dropping the stream must resume the worker panic");
    }

    /// A driver thread the operating system refuses is not the walk's
    /// failure: the walk goes on as `stream()` on the calling thread, and the
    /// refusal is one `spawn_worker` error under the policy.
    #[test]
    fn a_refused_driver_falls_back_to_the_serial_stream() {
        let tree = Tree::wide("refused", 3, 3, 3);
        let expected = collected(Walker::new(&tree.0));
        for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip, ErrorPolicy::Abort] {
            parallel::fail_next_worker_spawn();
            let items = Walker::new(&tree.0)
                .threads(4)
                .error_policy(policy)
                .stream_parallel()
                .collect::<Vec<_>>();
            assert!(
                !parallel::should_fail_next_worker_spawn(),
                "{policy:?}: the injected refusal was used"
            );
            let errors = items
                .iter()
                .filter_map(|item| item.as_ref().err())
                .map(|error| (error.operation().as_str(), error.path().to_path_buf()))
                .collect::<Vec<_>>();
            let entries = items
                .iter()
                .filter_map(|item| item.as_ref().ok())
                .map(|entry| entry.path().to_path_buf())
                .collect::<BTreeSet<_>>();
            match policy {
                ErrorPolicy::Collect => {
                    assert_eq!(errors, [("spawn_worker", tree.0.clone())]);
                    assert!(items[0].is_err(), "the refusal comes first");
                    assert_eq!(entries, expected);
                }
                ErrorPolicy::Skip => {
                    assert!(errors.is_empty(), "{errors:?}");
                    assert_eq!(entries, expected);
                }
                ErrorPolicy::Abort => {
                    assert_eq!(errors, [("spawn_worker", tree.0.clone())]);
                    assert_eq!(items.len(), 1, "the refusal ends the walk");
                }
            }
        }
    }

    /// Recoverable errors below the root and at a root, with the pool running,
    /// under each policy: the same errors and entries `collect()` reports, the
    /// errors in band, and under `Abort` the error as the last item.
    #[cfg(unix)]
    #[test]
    fn errors_follow_the_policy_table_with_the_pool_running() {
        use std::os::unix::fs::PermissionsExt;

        let tree = Tree::wide("errors", 10, 4, 4);
        let blocked = tree.0.join("branch-4").join("nested-1");
        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o000))
            .expect("restrict fixture directory");
        if fs::read_dir(&blocked).is_ok() {
            // Running as root: nothing is unreadable, so there is no error to
            // compare. Restore and leave rather than fail.
            fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755))
                .expect("restore fixture directory");
            return;
        }
        let missing = tree.0.join("missing-root");
        let build = |threads: usize, policy: ErrorPolicy| {
            Walker::new(&tree.0)
                .add_root(&missing)
                .expect("second root")
                .threads(threads)
                .error_policy(policy)
        };
        let described = |items: &[Result<crate::WalkEntry, crate::WalkError>]| {
            let mut errors = items
                .iter()
                .filter_map(|item| item.as_ref().err())
                .map(|error| (error.operation().as_str(), error.path().to_path_buf()))
                .collect::<Vec<_>>();
            errors.sort();
            let entries = items
                .iter()
                .filter_map(|item| item.as_ref().ok())
                .map(|entry| entry.path().to_path_buf())
                .collect::<BTreeSet<_>>();
            (entries, errors)
        };

        for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip, ErrorPolicy::Abort] {
            let reference = build(4, policy).collect();
            for threads in [2, 8] {
                let items = build(threads, policy).stream_parallel().collect::<Vec<_>>();
                let (entries, errors) = described(&items);
                match &reference {
                    Ok(result) => {
                        let mut expected_errors = result
                            .errors()
                            .iter()
                            .map(|error| (error.operation().as_str(), error.path().to_path_buf()))
                            .collect::<Vec<_>>();
                        expected_errors.sort();
                        let expected_entries = result
                            .entries()
                            .iter()
                            .map(|entry| entry.path().to_path_buf())
                            .collect::<BTreeSet<_>>();
                        assert!(!expected_errors.is_empty(), "{policy:?}: the fixture fails");
                        assert_eq!(errors, expected_errors, "{policy:?} on {threads} threads");
                        assert_eq!(entries, expected_entries, "{policy:?} on {threads} threads");
                    }
                    Err(error) => {
                        assert_eq!(errors.len(), 1, "{threads} threads: {errors:?}");
                        assert_eq!(errors[0].0, error.operation().as_str());
                        assert!(
                            items.last().is_some_and(Result::is_err),
                            "under Abort the error is the last item"
                        );
                    }
                }
            }
        }
        let skipped = build(4, ErrorPolicy::Skip)
            .stream_parallel()
            .filter_map(Result::err)
            .map(|error| error.path().to_path_buf())
            .collect::<Vec<_>>();
        assert_eq!(skipped, [missing], "Skip keeps only the root error");

        fs::set_permissions(&blocked, fs::Permissions::from_mode(0o755))
            .expect("restore fixture directory");
    }

    /// Sort is a collect-only operation for this stream as for `stream()`:
    /// asking for it changes nothing and reports nothing.
    #[test]
    fn sort_is_ignored_without_a_diagnostic() {
        let tree = Tree::wide("sort", 10, 4, 4);
        let expected = collected(Walker::new(&tree.0));
        let root = tree.0.clone();
        let actual = without_hanging("sort", move || {
            streamed(
                Walker::new(&root)
                    .threads(4)
                    .options(WalkOptions::default().sort(true))
                    .stream_parallel(),
            )
        });
        assert_eq!(actual, expected);
    }
}
