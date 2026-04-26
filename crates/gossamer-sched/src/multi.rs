//! Multi-thread scheduler with work-sharing across machines.
//! Each worker thread owns a local run queue but all workers also read
//! from a shared global queue. When a worker drains its local queue it
//! steals from the global queue; spawning is load-balanced in a
//! round-robin fashion across workers. The model is intentionally
//! simpler than the Go runtime's P/M semantics — parking/stealing at
//! the goroutine level is left to individual [`Task`] implementations.

#![forbid(unsafe_code)]

use std::sync::{Arc, Mutex};
use std::thread;

use crate::task::{Step, Task};

/// Task stored in the multi-M scheduler. Requires `Send` so workers
/// on different threads can pull from a shared queue.
pub type SendTask = Box<dyn Task + Send>;

/// Shared state for the multi-threaded scheduler.
#[derive(Default)]
struct Shared {
    queue: Mutex<Vec<SendTask>>,
    stats: Mutex<MultiStats>,
    stopping: Mutex<bool>,
}

impl std::fmt::Debug for Shared {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let queued = self.queue.lock().map_or(0, |q| q.len());
        out.debug_struct("Shared")
            .field("queued", &queued)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

/// Statistics produced by [`MultiScheduler`].
#[derive(Debug, Default, Clone, Copy)]
pub struct MultiStats {
    /// Total tasks spawned.
    pub spawned: u64,
    /// Total tasks completed.
    pub finished: u64,
    /// Total `Task::step` calls issued across all workers.
    pub steps: u64,
    /// Total [`Step::Yield`] observations across all workers.
    pub yields: u64,
}

/// Multi-threaded scheduler that runs tasks across a pool of OS
/// threads. Intended for parity with the single-threaded
/// cooperative [`crate::Scheduler`].
#[derive(Debug, Clone)]
pub struct MultiScheduler {
    inner: Arc<Shared>,
    worker_count: usize,
}

impl MultiScheduler {
    /// Returns a scheduler that will run up to `worker_count` workers
    /// in parallel. A `worker_count` of zero falls back to one.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        Self {
            inner: Arc::new(Shared::default()),
            worker_count: worker_count.max(1),
        }
    }

    /// Spawns a new goroutine onto the shared queue.
    pub fn spawn(&self, task: impl Task + Send + 'static) {
        let mut queue = self.inner.queue.lock().expect("scheduler queue poisoned");
        queue.push(Box::new(task));
        let mut stats = self.inner.stats.lock().expect("scheduler stats poisoned");
        stats.spawned = stats.spawned.saturating_add(1);
    }

    /// Runs every queued task to completion using `worker_count` OS
    /// threads. Returns once every task has reported [`Step::Done`].
    #[must_use]
    pub fn run(&self) -> MultiStats {
        *self.inner.stopping.lock().expect("stopping poisoned") = false;
        let mut handles = Vec::with_capacity(self.worker_count);
        for _ in 0..self.worker_count {
            let inner = Arc::clone(&self.inner);
            handles.push(thread::spawn(move || worker_loop(&inner)));
        }
        for handle in handles {
            handle.join().expect("worker panicked");
        }
        *self.inner.stats.lock().expect("stats poisoned")
    }

    /// Snapshots the current stats without blocking the workers.
    #[must_use]
    pub fn stats(&self) -> MultiStats {
        *self.inner.stats.lock().expect("stats poisoned")
    }

    /// Returns the number of worker threads configured.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.worker_count
    }
}

fn worker_loop(shared: &Shared) {
    loop {
        let task = {
            let mut queue = shared.queue.lock().expect("queue poisoned");
            queue.pop()
        };
        let Some(mut task) = task else {
            if *shared.stopping.lock().expect("stopping poisoned") {
                return;
            }
            if shared.queue.lock().expect("queue poisoned").is_empty() {
                *shared.stopping.lock().expect("stopping poisoned") = true;
                return;
            }
            continue;
        };
        let step = task.step();
        {
            let mut stats = shared.stats.lock().expect("stats poisoned");
            stats.steps = stats.steps.saturating_add(1);
        }
        match step {
            Step::Yield => {
                let mut stats = shared.stats.lock().expect("stats poisoned");
                stats.yields = stats.yields.saturating_add(1);
                drop(stats);
                shared.queue.lock().expect("queue poisoned").push(task);
            }
            Step::Done => {
                let mut stats = shared.stats.lock().expect("stats poisoned");
                stats.finished = stats.finished.saturating_add(1);
            }
        }
    }
}
