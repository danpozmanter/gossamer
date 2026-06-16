//! Goroutine worker pool backing the bytecode VM's `Op::Spawn`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Joins every outstanding goroutine and returns once they finish.
/// Called by the CLI entrypoint after `main` returns so spawned work
/// has a chance to land before the process exits.
pub fn join_outstanding_goroutines() {
    pool().drain();
}

/// Goroutine task: a closure to run on a pool worker.
type GoroutineTask = Box<dyn FnOnce() + Send + 'static>;

/// Spawns an OS thread that runs Gossamer goroutine code. Sizes its
/// stack to the goroutine stack contract (`gossamer_coro::stack_size()`,
/// matching the compiled tier's coroutines) and arms the byte-budget
/// recursion guard at the thread's shallowest point, so deeply
/// recursive goroutine code raises a clean `RuntimeError::StackOverflow`
/// instead of overflowing the OS stack and aborting the whole process.
fn spawn_goroutine_thread<F: FnOnce() + Send + 'static>(
    name: &str,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(gossamer_coro::stack_size())
        .spawn(move || {
            gossamer_coro::arm_stack_guard(
                gossamer_coro::stack_size() - gossamer_coro::STACK_GUARD_MARGIN,
            );
            body();
        })
}

/// Fixed-size worker pool that runs goroutines spawned via the bytecode
/// `Op::Spawn`. A bounded set of workers shares one task queue so a
/// goroutine costs a queue push rather than a fresh OS thread and a
/// cold-started `Vm`.
///
/// Pool size: `num_cpus()`. Tasks queue when all workers are busy;
/// workers park on a `Condvar` when the queue is empty. `outstanding`
/// tracks queued + in-flight tasks so [`join_outstanding_goroutines`]
/// can wait for completion.
pub(crate) struct GoroutinePool {
    inner: parking_lot::Mutex<PoolInner>,
    cv: parking_lot::Condvar,
    /// Wake-up condition for `drain()` to learn that the
    /// counter has reached zero.
    drain_cv: parking_lot::Condvar,
    /// Total tasks that have not yet completed (queued +
    /// running). Used by `drain()` for completion wait.
    outstanding: AtomicU64,
    /// Total number of worker threads spawned. Capped at
    /// initialisation; never grows.
    workers: AtomicUsize,
}

struct PoolInner {
    queue: VecDeque<GoroutineTask>,
    /// `true` once the runtime is shutting down; workers exit
    /// once `queue` drains. Currently never set in practice
    /// (the process exits right after `drain()` returns), but
    /// kept for cleanliness.
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
            let spawned = spawn_goroutine_thread("gossamer-worker", move || {
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
                                // Last in-flight task settled -
                                // wake any drain() waiter.
                                p.drain_cv.notify_all();
                            }
                        }
                        None => break,
                    }
                }
            });
            if let Err(e) = spawned {
                // A pool below its requested size starves blocking
                // goroutines (e.g. `go http::serve`); say so instead
                // of failing silently.
                eprintln!("gossamer-worker spawn failed: {e}");
            }
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

    /// Blocks until every queued / in-flight task has finished.
    /// Called by [`join_outstanding_goroutines`] at program exit.
    pub(crate) fn drain(&self) {
        let mut inner = self.inner.lock();
        while self.outstanding.load(Ordering::Acquire) > 0 {
            self.drain_cv.wait(&mut inner);
        }
    }
}

static POOL: OnceLock<Arc<GoroutinePool>> = OnceLock::new();

/// Lazily-initialised process-wide goroutine pool. First call
/// builds the pool with `num_cpus()` workers.
pub(crate) fn pool() -> &'static Arc<GoroutinePool> {
    POOL.get_or_init(|| {
        // Conservative default: physical cores via `available_parallelism`.
        // Fall back to 4 when the platform refuses to report.
        let n = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
            .min(64);
        GoroutinePool::new(n)
    })
}
