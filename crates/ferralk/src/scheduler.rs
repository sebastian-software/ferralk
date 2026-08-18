//! Work-stealing queue primitives used by the walker scheduler.

use crossbeam_deque::{Injector, Steal, Worker};

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

#[cfg(test)]
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

#[cfg(test)]
mod loom_models {
    use std::collections::VecDeque;

    use loom::{
        sync::{
            Arc, Mutex,
            atomic::{AtomicBool, AtomicUsize, Ordering},
        },
        thread,
    };

    #[test]
    fn queued_child_keeps_waiting_workers_alive_until_it_is_observed() {
        loom::model(|| {
            // One root task is active before the helper starts. Its child must
            // make the helper wait rather than incorrectly conclude that the
            // queue is complete during the hand-off.
            let queue = Arc::new(Mutex::new(VecDeque::new()));
            let pending = Arc::new(AtomicUsize::new(1));

            let producer_queue = Arc::clone(&queue);
            let producer_pending = Arc::clone(&pending);
            let producer = thread::spawn(move || {
                producer_pending.fetch_add(1, Ordering::AcqRel);
                thread::yield_now();
                producer_queue.lock().expect("loom queue lock").push_back(7);
                producer_pending.fetch_sub(1, Ordering::AcqRel);
            });

            let consumer_queue = Arc::clone(&queue);
            let consumer_pending = Arc::clone(&pending);
            let consumer = thread::spawn(move || {
                loop {
                    if let Some(task) = consumer_queue.lock().expect("loom queue lock").pop_front()
                    {
                        assert_eq!(task, 7);
                        consumer_pending.fetch_sub(1, Ordering::AcqRel);
                        break;
                    }
                    if consumer_pending.load(Ordering::Acquire) == 0 {
                        break;
                    }
                    thread::yield_now();
                }
            });

            producer.join().expect("producer joins");
            consumer.join().expect("consumer joins");
            assert_eq!(pending.load(Ordering::Acquire), 0);
        });
    }

    #[test]
    fn last_completion_notifies_exactly_once() {
        loom::model(|| {
            let pending = Arc::new(AtomicUsize::new(2));
            let completed = Arc::new(AtomicBool::new(false));
            let notifications = Arc::new(AtomicUsize::new(0));
            let mut workers = Vec::new();
            for _ in 0..2 {
                let pending = Arc::clone(&pending);
                let completed = Arc::clone(&completed);
                let notifications = Arc::clone(&notifications);
                workers.push(thread::spawn(move || {
                    if pending.fetch_sub(1, Ordering::AcqRel) == 1 {
                        completed.store(true, Ordering::Release);
                        notifications.fetch_add(1, Ordering::AcqRel);
                    }
                }));
            }
            for worker in workers {
                worker.join().expect("worker joins");
            }
            assert_eq!(pending.load(Ordering::Acquire), 0);
            assert!(completed.load(Ordering::Acquire));
            assert_eq!(notifications.load(Ordering::Acquire), 1);
        });
    }
}
