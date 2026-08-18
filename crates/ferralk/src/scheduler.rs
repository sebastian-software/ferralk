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
