//! Work-stealing queue primitives and the worker protocol behind them.
//!
//! The protocol types below use `loom`'s primitives when the crate is built
//! with `--cfg loom`, so the models in `loom_models` exercise the very code the
//! walker runs instead of a re-implementation of it.

use std::time::Duration;

#[cfg(loom)]
use loom::sync::{
    Condvar, Mutex, MutexGuard,
    atomic::{AtomicUsize, Ordering},
};
#[cfg(not(loom))]
use std::sync::{
    Condvar, Mutex, MutexGuard,
    atomic::{AtomicUsize, Ordering},
};

use crossbeam_deque::{Injector, Steal, Worker};

/// Backstop for cancellation requested through a `CancellationToken` that the
/// walk does not own, which therefore cannot notify the parked workers.
///
/// Progress never depends on it: hand-off and termination are driven by
/// notifications that are sent while `wake_lock` is held.
const CANCELLATION_POLL_INTERVAL: Duration = Duration::from_millis(10);

pub(crate) struct Scheduler<T> {
    injector: Injector<T>,
}

impl<T> Scheduler<T> {
    pub(crate) fn new() -> Self {
        Self {
            injector: Injector::new(),
        }
    }

    pub(crate) fn push(&self, task: T) {
        self.injector.push(task);
    }

    pub(crate) fn worker(&self) -> Worker<T> {
        Worker::new_fifo()
    }

    pub(crate) fn steal_into(&self, worker: &Worker<T>) -> Option<T> {
        loop {
            match self.injector.steal_batch_and_pop(worker) {
                Steal::Success(task) => return Some(task),
                Steal::Empty => return None,
                Steal::Retry => continue,
            }
        }
    }
}

/// Task accounting and parking protocol shared by the workers of one walk.
///
/// Tasks are counted here rather than queued: `begin_task` announces one,
/// the guard returned by `claim_task` releases it even while a panic unwinds,
/// and `wait_for_task` parks a worker until work or the end of the walk is
/// observable. Every notification is sent while `wake_lock` is held, so a
/// worker that has decided to park cannot miss one.
pub(crate) struct Coordinator {
    pending: AtomicUsize,
    active_workers: AtomicUsize,
    wake_lock: Mutex<()>,
    wake: Condvar,
}

impl Coordinator {
    pub(crate) fn new() -> Self {
        Self {
            pending: AtomicUsize::new(0),
            active_workers: AtomicUsize::new(0),
            wake_lock: Mutex::new(()),
            wake: Condvar::new(),
        }
    }

    /// Announces one task that some worker will claim later.
    pub(crate) fn begin_task(&self) {
        self.pending.fetch_add(1, Ordering::AcqRel);
    }

    /// Takes ownership of one announced task. The guard releases it on drop,
    /// including while a panic unwinds through the work.
    pub(crate) fn claim_task(&self) -> TaskGuard<'_> {
        TaskGuard { coordinator: self }
    }

    pub(crate) fn pending(&self) -> usize {
        self.pending.load(Ordering::Acquire)
    }

    /// Wakes every parked worker. Taking `wake_lock` around the notification
    /// is what makes the hand-off free of lost wakeups: a worker that observed
    /// an empty queue holds the lock until it parks, so it either sees the new
    /// state before parking or is notified afterwards.
    pub(crate) fn wake_waiters(&self) {
        drop(lock(&self.wake_lock));
        self.wake.notify_all();
    }

    /// Parks until `try_take` yields a task, the walk is cancelled, or no task
    /// is outstanding any more.
    pub(crate) fn wait_for_task<T>(
        &self,
        should_stop: impl Fn() -> bool,
        mut try_take: impl FnMut() -> Option<T>,
    ) -> Option<T> {
        loop {
            if should_stop() {
                return None;
            }
            if let Some(task) = try_take() {
                return Some(task);
            }
            let guard = lock(&self.wake_lock);
            // Re-checked under the lock: a notification sent from here on
            // cannot be missed, and anything announced earlier is visible.
            if self.pending.load(Ordering::Acquire) == 0 || should_stop() {
                return None;
            }
            if let Some(task) = try_take() {
                return Some(task);
            }
            let _ = self
                .wake
                .wait_timeout(guard, CANCELLATION_POLL_INTERVAL)
                .unwrap_or_else(|poisoned| poisoned.into_inner());
        }
    }

    /// Reserves the slot of the thread that starts the walk.
    pub(crate) fn claim_caller_slot(&self) -> WorkerSlot<'_> {
        self.active_workers.fetch_add(1, Ordering::AcqRel);
        WorkerSlot { coordinator: self }
    }

    /// Reserves a slot for one more worker while queued tasks outnumber the
    /// workers already running and `budget` has room. The compare-exchange
    /// keeps concurrent growth attempts from overshooting the budget.
    pub(crate) fn claim_worker_slot(&self, budget: usize) -> Option<WorkerSlot<'_>> {
        let mut active = self.active_workers.load(Ordering::Acquire);
        loop {
            if active >= budget || self.pending() <= active {
                return None;
            }
            match self.active_workers.compare_exchange_weak(
                active,
                active + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Some(WorkerSlot { coordinator: self }),
                Err(observed) => active = observed,
            }
        }
    }

    /// Observation point for the loom models; the walk itself only ever asks
    /// for slots through [`Coordinator::claim_worker_slot`].
    #[cfg(loom)]
    pub(crate) fn active_workers(&self) -> usize {
        self.active_workers.load(Ordering::Acquire)
    }

    fn finish_task(&self) {
        if self.pending.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.wake_waiters();
        }
    }
}

/// One in-flight task. Dropping it releases the task, so a panic inside the
/// work cannot strand the pending count and leave siblings waiting forever.
pub(crate) struct TaskGuard<'a> {
    coordinator: &'a Coordinator,
}

impl Drop for TaskGuard<'_> {
    fn drop(&mut self) {
        self.coordinator.finish_task();
    }
}

/// One running worker. Dropping it frees the slot for a later helper, whether
/// the worker finished, failed to start, or unwound.
pub(crate) struct WorkerSlot<'a> {
    coordinator: &'a Coordinator,
}

impl Drop for WorkerSlot<'_> {
    fn drop(&mut self) {
        self.coordinator
            .active_workers
            .fetch_sub(1, Ordering::AcqRel);
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(all(test, not(loom)))]
mod tests {
    use super::Scheduler;

    #[test]
    fn injector_distributes_tasks_to_worker_local_queues() {
        let scheduler = Scheduler::new();
        scheduler.push(1);
        scheduler.push(2);
        let worker = scheduler.worker();

        let first = scheduler.steal_into(&worker).expect("first task");
        let second = worker.pop().unwrap_or_else(|| {
            scheduler
                .steal_into(&worker)
                .expect("second task after local batch")
        });
        assert_ne!(first, second);
        assert!(scheduler.steal_into(&worker).is_none());
    }
}

/// Loom models of the production protocol. They drive [`Coordinator`] itself,
/// with loom's primitives substituted underneath it, so every interleaving of
/// the real hand-off is checked.
///
/// Run with `RUSTFLAGS="--cfg loom" cargo test -p ferralk --lib loom_models`.
///
/// Note that loom's `Condvar::wait_timeout` never times out, so the poll
/// interval cannot paper over a missing notification here: a lost wakeup shows
/// up as a deadlock.
#[cfg(loom)]
mod loom_models {
    use std::{
        collections::VecDeque,
        panic::{AssertUnwindSafe, catch_unwind},
        sync::atomic::{AtomicUsize, Ordering},
    };

    use loom::{
        sync::{Arc, Mutex},
        thread,
    };

    use super::Coordinator;

    /// Reports how much of the state space a model actually covered, so a run
    /// shows its work instead of just passing silently.
    fn report(model: &str, executions: &AtomicUsize) {
        eprintln!(
            "loom explored {} executions of {model}",
            executions.load(Ordering::Relaxed)
        );
    }

    fn pop(queue: &Mutex<VecDeque<usize>>) -> Option<usize> {
        queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .pop_front()
    }

    fn push(queue: &Mutex<VecDeque<usize>>, task: usize) {
        queue
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push_back(task);
    }

    /// A task announced while a worker is on its way into `wait_for_task` must
    /// still reach that worker, and the walk must end once it is done.
    #[test]
    fn a_queued_task_reaches_a_parking_worker() {
        static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
        loom::model(|| {
            EXECUTIONS.fetch_add(1, Ordering::Relaxed);
            let coordinator = Arc::new(Coordinator::new());
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            // The root task is outstanding before the helper starts.
            coordinator.begin_task();

            let producer_coordinator = Arc::clone(&coordinator);
            let producer_queue = Arc::clone(&queue);
            let producer = thread::spawn(move || {
                let _root = producer_coordinator.claim_task();
                producer_coordinator.begin_task();
                push(&producer_queue, 7);
                producer_coordinator.wake_waiters();
            });

            let consumer_coordinator = Arc::clone(&coordinator);
            let consumer_queue = Arc::clone(&queue);
            let consumer = thread::spawn(move || {
                let task = consumer_coordinator.wait_for_task(|| false, || pop(&consumer_queue));
                assert_eq!(task, Some(7), "the queued task must reach the worker");
                drop(consumer_coordinator.claim_task());
                // With every task done the walk has to end instead of parking.
                assert_eq!(
                    consumer_coordinator.wait_for_task(|| false, || pop(&consumer_queue)),
                    None,
                    "a drained walk must let its workers finish"
                );
            });

            producer.join().expect("producer joins");
            consumer.join().expect("consumer joins");
            assert_eq!(coordinator.pending(), 0);
        });
        report("a_queued_task_reaches_a_parking_worker", &EXECUTIONS);
    }

    /// The task guard has to release its task while the panic unwinds, or the
    /// sibling worker parks on a count that never reaches zero.
    #[test]
    fn a_panicking_worker_releases_its_task() {
        static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
        // The injected panic is expected, so its default report is filtered out
        // instead of printing once per explored execution. Loom's own failures
        // still reach the previous hook.
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !info.to_string().contains("injected task panic") {
                previous(info);
            }
        }));

        loom::model(|| {
            EXECUTIONS.fetch_add(1, Ordering::Relaxed);
            let coordinator = Arc::new(Coordinator::new());
            coordinator.begin_task();

            let panicking_coordinator = Arc::clone(&coordinator);
            let panicking = thread::spawn(move || {
                let outcome = catch_unwind(AssertUnwindSafe(|| {
                    let _task = panicking_coordinator.claim_task();
                    panic!("injected task panic");
                }));
                assert!(outcome.is_err(), "the injected panic must be captured");
            });

            let sibling_coordinator = Arc::clone(&coordinator);
            let sibling = thread::spawn(move || {
                assert_eq!(
                    sibling_coordinator.wait_for_task(|| false, || None::<usize>),
                    None,
                    "the sibling must not park forever after a panic"
                );
            });

            panicking.join().expect("panicking worker joins");
            sibling.join().expect("sibling joins");
            assert_eq!(coordinator.pending(), 0);
        });

        let _ = std::panic::take_hook();
        report("a_panicking_worker_releases_its_task", &EXECUTIONS);
    }

    /// Cancellation has to release parked workers as well, and the notification
    /// must not be lost between the re-check and the park.
    #[test]
    fn cancellation_releases_a_parking_worker() {
        static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
        loom::model(|| {
            EXECUTIONS.fetch_add(1, Ordering::Relaxed);
            let coordinator = Arc::new(Coordinator::new());
            let cancelled = Arc::new(Mutex::new(false));
            // A task stays outstanding, so only cancellation can end the wait.
            coordinator.begin_task();

            let canceller_coordinator = Arc::clone(&coordinator);
            let canceller_flag = Arc::clone(&cancelled);
            let canceller = thread::spawn(move || {
                *canceller_flag
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner()) = true;
                canceller_coordinator.wake_waiters();
            });

            let worker_coordinator = Arc::clone(&coordinator);
            let worker_flag = Arc::clone(&cancelled);
            let worker = thread::spawn(move || {
                let stopped = || {
                    *worker_flag
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                };
                assert_eq!(
                    worker_coordinator.wait_for_task(stopped, || None::<usize>),
                    None,
                    "a cancelled walk must release its parked workers"
                );
            });

            canceller.join().expect("canceller joins");
            worker.join().expect("worker joins");
            drop(coordinator.claim_task());
        });
        report("cancellation_releases_a_parking_worker", &EXECUTIONS);
    }

    /// Concurrent growth attempts must not put more workers on the walk than
    /// the configured budget allows.
    #[test]
    fn concurrent_growth_stays_within_the_thread_budget() {
        static EXECUTIONS: AtomicUsize = AtomicUsize::new(0);
        loom::model(|| {
            EXECUTIONS.fetch_add(1, Ordering::Relaxed);
            let coordinator = Arc::new(Coordinator::new());
            let caller = coordinator.claim_caller_slot();
            // Backlog of three tasks against a budget of two workers.
            for _ in 0..3 {
                coordinator.begin_task();
            }

            let contender_coordinator = Arc::clone(&coordinator);
            let contender = thread::spawn(move || {
                let slot = contender_coordinator.claim_worker_slot(2);
                assert!(contender_coordinator.active_workers() <= 2);
                drop(slot);
            });

            let slot = coordinator.claim_worker_slot(2);
            assert!(coordinator.active_workers() <= 2);
            drop(slot);
            contender.join().expect("contender joins");

            drop(caller);
            assert_eq!(coordinator.active_workers(), 0);
            for _ in 0..3 {
                drop(coordinator.claim_task());
            }
            assert_eq!(coordinator.pending(), 0);
        });
        report(
            "concurrent_growth_stays_within_the_thread_budget",
            &EXECUTIONS,
        );
    }
}
