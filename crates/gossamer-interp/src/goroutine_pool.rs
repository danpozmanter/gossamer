//! Process-wide goroutine worker pool used by `Op::Spawn`.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Goroutine task: a closure to run on a pool worker.
type GoroutineTask = Box<dyn FnOnce() + Send + 'static>;

/// Fixed-size worker pool that runs goroutines spawned via
/// [`crate::bytecode::Op::Spawn`]. Pool size: `num_cpus()`. Tasks queue
/// when all workers are busy; workers park on a `Condvar` when the
/// queue is empty. `outstanding` tracks queued + in-flight tasks so
/// [`join_outstanding_goroutines`] can wait for completion.
pub(crate) struct GoroutinePool {
    inner: parking_lot::Mutex<PoolInner>,
    cv: parking_lot::Condvar,
    /// Wake-up condition for `drain()` to learn that the counter reached zero.
    drain_cv: parking_lot::Condvar,
    /// Total tasks that have not yet completed (queued + running).
    outstanding: AtomicU64,
    /// Total number of worker threads spawned.
    workers: AtomicUsize,
}

struct PoolInner {
    queue: VecDeque<GoroutineTask>,
    shutting_down: bool,
}

impl GoroutinePool {
    fn new(num_workers: usize) -> Arc<Self> {
        let pool = Arc::new(Self {
            inner: parking_lot::Mutex::new(PoolInner {
                queue: VecDeque::new(),
                shutting_down: false,
            }),
            cv: parking_lot::Condvar::new(),
            drain_cv: parking_lot::Condvar::new(),
            outstanding: AtomicU64::new(0),
            workers: AtomicUsize::new(0),
        });
        for _ in 0..num_workers {
            let p = Arc::clone(&pool);
            let _ = std::thread::Builder::new()
                .name("gossamer-worker".to_string())
                .spawn(move || {
                    p.workers.fetch_add(1, Ordering::Relaxed);
                    loop {
                        let task = {
                            let mut inner = p.inner.lock();
                            loop {
                                if let Some(task) = inner.queue.pop_front() {
                                    break Some(task);
                                }
                                if inner.shutting_down {
                                    break None;
                                }
                                p.cv.wait(&mut inner);
                            }
                        };
                        match task {
                            Some(task) => {
                                task();
                                let prev = p.outstanding.fetch_sub(1, Ordering::AcqRel);
                                if prev == 1 {
                                    p.drain_cv.notify_all();
                                }
                            }
                            None => break,
                        }
                    }
                });
        }
        pool
    }

    /// Enqueues a task. Wakes one parked worker.
    pub(crate) fn spawn(&self, task: GoroutineTask) {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        let mut inner = self.inner.lock();
        inner.queue.push_back(task);
        self.cv.notify_one();
    }

    /// Blocks until every queued and in-flight task has finished.
    pub(crate) fn drain(&self) {
        let mut inner = self.inner.lock();
        while self.outstanding.load(Ordering::Acquire) > 0 {
            self.drain_cv.wait(&mut inner);
        }
    }
}

static POOL: OnceLock<Arc<GoroutinePool>> = OnceLock::new();

/// Lazily-initialised process-wide goroutine pool. First call
/// builds the pool with `available_parallelism()` workers (capped at 64).
pub(crate) fn pool() -> &'static Arc<GoroutinePool> {
    POOL.get_or_init(|| {
        let n = std::thread::available_parallelism()
            .map_or(4, std::num::NonZeroUsize::get)
            .min(64);
        GoroutinePool::new(n)
    })
}

/// Waits for all outstanding goroutines to finish. Call this after
/// `main` returns so goroutine output has a chance to land.
pub fn join_outstanding_goroutines() {
    pool().drain();
}
