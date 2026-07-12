//! Goroutine worker pool backing the bytecode VM's `Op::Spawn`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Joins every outstanding goroutine and returns once they finish.
/// Called by the CLI entrypoint after `main` returns so spawned work
/// has a chance to land before the process exits.
pub fn join_outstanding_goroutines() {
    if let Some(pool) = POOL.get() {
        pool.drain();
    }
}

/// Goroutine task: a closure to run on a pool worker.
type GoroutineTask = Box<dyn FnOnce() + Send + 'static>;

/// Spawns an OS thread that runs Gossamer goroutine code. Sizes its
/// stack to [`crate::vm::VM_THREAD_STACK_BYTES`] - the same reserve the
/// main VM thread uses - because a goroutine body runs the bytecode
/// interpreter, whose frames are large; the small compiled-tier
/// coroutine stack would let `MAX_CALL_DEPTH`'s frames overrun the OS
/// guard page and abort the process on a workload the main thread runs
/// fine. The byte-budget recursion guard is armed at the thread's
/// shallowest point as a backstop.
#[cfg(not(target_arch = "wasm32"))]
fn spawn_goroutine_thread<F: FnOnce() + Send + 'static>(
    name: &str,
    body: F,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name(name.to_string())
        .stack_size(crate::vm::VM_THREAD_STACK_BYTES)
        .spawn(move || {
            gossamer_coro::arm_stack_guard(
                crate::vm::VM_THREAD_STACK_BYTES - gossamer_coro::STACK_GUARD_MARGIN,
            );
            body();
        })
}

/// Fixed-size worker pool that runs goroutines spawned via the bytecode
/// `Op::Spawn`. A bounded set of workers shares one task queue so a
/// goroutine costs a queue push rather than a fresh OS thread and a
/// cold-started `Vm`.
///
/// Pool size is deliberately capped below the host CPU count by default.
/// Each worker owns a large VM stack, and a test or embedding can run many
/// interpreter processes concurrently; blindly using every visible CPU in
/// each process multiplies that footprint into hundreds of parked threads.
/// `GOSSAMER_VM_GOROUTINE_WORKERS` opts into a larger (or smaller) pool.
/// Tasks queue when all workers are busy;
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
        // wasm32 is single-threaded: there are no worker threads. `go` /
        // `spawn` run the goroutine body to completion immediately (see
        // the wasm `spawn` below), matching the eager coro shim.
        #[cfg(target_arch = "wasm32")]
        let _ = num_workers;
        #[cfg(not(target_arch = "wasm32"))]
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
                            // A worker task must settle the outstanding counter even
                            // when an implementation panic escapes its VM boundary.
                            // Otherwise program exit waits forever after main has
                            // already printed all of its output. User panics are
                            // converted to RuntimeError by `spawn_goroutine_native`;
                            // this catch is the final containment for host panics.
                            if std::panic::catch_unwind(std::panic::AssertUnwindSafe(task)).is_err()
                            {
                                eprintln!("gossamer: goroutine worker panicked");
                            }
                            // `drain()` checks `outstanding` while holding
                            // `inner` and then waits on `drain_cv`.  Update
                            // the predicate under that same mutex: notifying
                            // without it admits the classic lost-wakeup race
                            // where a completed task signals between the
                            // check and the Condvar wait, stranding program
                            // shutdown after all user output is printed.
                            let _inner = p.inner.lock();
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
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn(&self, task: GoroutineTask) {
        let mut inner = self.inner.lock();
        // Publish the counter and task under the same mutex used by
        // `drain()`, so a drain cannot observe an empty program while a
        // concurrent goroutine spawn is about to enqueue work.
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        inner.queue.push_back(task);
        self.cv.notify_one();
    }

    /// Single-threaded wasm: run the goroutine body to completion
    /// immediately. A body that tries to block reaches
    /// `gossamer_coro::suspend`, which panics with the documented
    /// "blocking not supported in the playground" message.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn spawn(&self, task: GoroutineTask) {
        task();
    }

    /// Blocks until every queued / in-flight task has finished.
    /// Called by [`join_outstanding_goroutines`] at program exit.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn drain(&self) {
        let mut inner = self.inner.lock();
        while self.outstanding.load(Ordering::Acquire) > 0 {
            self.drain_cv.wait(&mut inner);
        }
    }

    /// wasm runs goroutines eagerly to completion in `spawn`, so there
    /// is never anything outstanding to drain at exit.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn drain(&self) {}
}

static POOL: OnceLock<Arc<GoroutinePool>> = OnceLock::new();

const DEFAULT_MAX_WORKERS: usize = 4;
const MAX_WORKERS: usize = 64;

fn default_worker_count() -> usize {
    if let Ok(raw) = std::env::var("GOSSAMER_VM_GOROUTINE_WORKERS")
        && let Ok(requested) = raw.parse::<usize>()
        && requested > 0
    {
        return requested.min(MAX_WORKERS);
    }
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(DEFAULT_MAX_WORKERS)
        .min(DEFAULT_MAX_WORKERS)
}

/// Lazily-initialised process-wide goroutine pool. First call
/// builds the pool with the bounded default worker count.
pub(crate) fn pool() -> &'static Arc<GoroutinePool> {
    POOL.get_or_init(|| GoroutinePool::new(default_worker_count()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    #[test]
    fn panicking_task_does_not_strand_drain() {
        let pool = GoroutinePool::new(1);
        let ran = Arc::new(AtomicBool::new(false));
        pool.spawn(Box::new(|| panic!("intentional worker panic")));
        let ran_after_panic = Arc::clone(&ran);
        pool.spawn(Box::new(move || {
            ran_after_panic.store(true, Ordering::Release);
        }));

        // Use `drain` itself to observe completion. Polling the atomic can
        // spuriously time out when another test temporarily starves this
        // private worker; the condition-variable handoff is the liveness
        // guarantee this regression is meant to exercise. A bounded helper
        // prevents a future lost wakeup from hanging the entire test process.
        let drain_pool = Arc::clone(&pool);
        let (drained_tx, drained_rx) = mpsc::channel();
        let drainer = std::thread::spawn(move || {
            drain_pool.drain();
            drained_tx.send(()).expect("test receiver remains live");
        });
        drained_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("drain must settle panicking and queued tasks");
        drainer.join().expect("drain helper must not panic");
        assert_eq!(pool.outstanding.load(Ordering::Acquire), 0);
        assert!(ran.load(Ordering::Acquire));
    }

    #[test]
    fn default_worker_count_is_bounded() {
        assert!((1..=DEFAULT_MAX_WORKERS).contains(&default_worker_count()));
    }

    #[test]
    fn drain_cannot_miss_a_last_task_completion() {
        let pool = GoroutinePool::new(1);
        // Repeat the check because the old implementation's lost wakeup
        // depended on a narrow scheduling window.  Each task waits until the
        // drainer is armed, then becomes the last outstanding task.
        for _ in 0..128 {
            let (started_tx, started_rx) = mpsc::channel();
            let (release_tx, release_rx) = mpsc::channel();
            pool.spawn(Box::new(move || {
                started_tx.send(()).expect("test receiver remains live");
                release_rx.recv().expect("test release remains live");
            }));
            started_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("worker starts the task");

            let drain_pool = Arc::clone(&pool);
            let (drained_tx, drained_rx) = mpsc::channel();
            std::thread::spawn(move || {
                drain_pool.drain();
                drained_tx.send(()).expect("test receiver remains live");
            });
            release_tx.send(()).expect("worker remains live");
            drained_rx
                .recv_timeout(Duration::from_secs(1))
                .expect("drain must observe the final completion");
        }
    }
}
