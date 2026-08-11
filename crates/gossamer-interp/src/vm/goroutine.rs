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
/// interpreter and JIT. The byte-budget recursion guard is armed at the
/// thread's shallowest point so native recursion reports a clean overflow
/// before reaching the OS guard page.
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

/// Elastic worker pool that runs goroutines spawned via the bytecode
/// `Op::Spawn`. Workers share one task queue so a goroutine costs a queue
/// push rather than a fresh OS thread and a cold-started `Vm`.
///
/// The pool starts small: each worker owns a large VM stack, and a test or
/// embedding can run many interpreter processes concurrently, so claiming a
/// thread per CPU up front multiplies that footprint across processes.
/// `GOSSAMER_VM_GOROUTINE_WORKERS` sets that starting size.
///
/// It then grows on demand up to [`MAX_WORKERS`], because a goroutine
/// blocked in a channel operation holds its worker until the operation
/// completes. A pool that only ever had its initial threads would let that
/// many blocked goroutines starve every goroutine still queued behind them,
/// including the one whose send or close would release them.
///
/// `outstanding` tracks queued + in-flight tasks so
/// [`join_outstanding_goroutines`] can wait for completion.
pub(crate) struct GoroutinePool {
    inner: parking_lot::Mutex<PoolInner>,
    cv: parking_lot::Condvar,
    /// Wake-up condition for `drain()` to learn that the
    /// counter has reached zero.
    drain_cv: parking_lot::Condvar,
    /// Total tasks that have not yet completed (queued +
    /// running). Used by `drain()` for completion wait.
    outstanding: AtomicU64,
    /// Worker threads created so far, counted at creation rather than on
    /// thread entry so a growth decision cannot race a starting worker.
    workers: AtomicUsize,
    /// Workers parked waiting for a task. Read and written under `inner`,
    /// so a spawn sees an accurate count when deciding whether to grow.
    idle: AtomicUsize,
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
            idle: AtomicUsize::new(0),
        });
        // wasm32 is single-threaded: there are no worker threads. `go` /
        // `spawn` run the goroutine body to completion immediately (see
        // the wasm `spawn` below), matching the eager coro shim.
        #[cfg(target_arch = "wasm32")]
        let _ = num_workers;
        #[cfg(not(target_arch = "wasm32"))]
        for _ in 0..num_workers {
            Self::start_worker(&pool);
        }
        pool
    }

    /// Adds one worker thread to `pool`, counting it before the thread
    /// starts so a concurrent growth decision cannot double-count it.
    #[cfg(not(target_arch = "wasm32"))]
    fn start_worker(pool: &Arc<Self>) {
        pool.workers.fetch_add(1, Ordering::Relaxed);
        {
            let p = Arc::clone(pool);
            let spawned = spawn_goroutine_thread("gossamer-worker", move || {
                ON_GOROUTINE_WORKER.with(|flag| flag.set(true));
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
                            p.idle.fetch_add(1, Ordering::Relaxed);
                            p.cv.wait(&mut inner);
                            p.idle.fetch_sub(1, Ordering::Relaxed);
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
                pool.workers.fetch_sub(1, Ordering::Relaxed);
                eprintln!("gossamer-worker spawn failed: {e}");
            }
        }
    }

    /// Enqueues a task on `pool`, waking a parked worker or adding one.
    ///
    /// A goroutine that blocks in a channel operation keeps its worker for
    /// the duration, so "every worker busy" does not mean the machine is
    /// saturated - it routinely means the workers are parked waiting for
    /// something only a queued goroutine can do. Growing when nothing is
    /// idle is what keeps that queued goroutine reachable.
    #[cfg(not(target_arch = "wasm32"))]
    pub(crate) fn spawn(pool: &Arc<Self>, task: GoroutineTask) {
        let mut inner = pool.inner.lock();
        // Publish the counter and task under the same mutex used by
        // `drain()`, so a drain cannot observe an empty program while a
        // concurrent goroutine spawn is about to enqueue work.
        pool.outstanding.fetch_add(1, Ordering::AcqRel);
        inner.queue.push_back(task);
        let idle = pool.idle.load(Ordering::Relaxed);
        let queued = inner.queue.len();
        let workers = pool.workers.load(Ordering::Relaxed);
        if idle < queued {
            if workers < MAX_WORKERS {
                drop(inner);
                Self::start_worker(pool);
                return;
            }
            drop(inner);
            warn_worker_ceiling_once();
            pool.cv.notify_one();
            return;
        }
        pool.cv.notify_one();
    }

    /// Single-threaded wasm: run the goroutine body to completion
    /// immediately. A body that tries to block reaches
    /// `gossamer_coro::suspend`, which panics with the documented
    /// "blocking not supported in the playground" message.
    #[cfg(target_arch = "wasm32")]
    pub(crate) fn spawn(_pool: &Arc<Self>, task: GoroutineTask) {
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
/// Ceiling on worker threads. A goroutine blocked in a channel operation
/// holds its worker, so this bounds how many may block at once. Each worker
/// reserves a 16 MiB stack, but the reservation is virtual and only the
/// pages actually used are committed, so the ceiling costs address space
/// rather than memory. Reaching it means the program has more
/// simultaneously-blocked goroutines than the host can back with threads.
const MAX_WORKERS: usize = 1024;

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

/// Says once that the pool cannot grow further. A goroutine queued in this
/// state waits for a running one to finish; if those are all blocked on it,
/// the program stops making progress, and silence would leave nothing to
/// explain why.
#[cfg(not(target_arch = "wasm32"))]
fn warn_worker_ceiling_once() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        eprintln!(
            "gossamer: {MAX_WORKERS} goroutine workers in use and all are busy; \
             further goroutines wait for one to finish"
        );
    });
}

/// Lazily-initialised process-wide goroutine pool. First call
/// builds the pool with the bounded default worker count.
pub(crate) fn pool() -> &'static Arc<GoroutinePool> {
    POOL.get_or_init(|| GoroutinePool::new(default_worker_count()))
}

/// Goroutine tasks queued or running. A task that has not started yet is
/// counted, so it always reads as able to make progress.
fn outstanding_goroutines() -> u64 {
    POOL.get()
        .map_or(0, |p| p.outstanding.load(Ordering::Acquire))
}

thread_local! {
    /// Set on every goroutine worker thread, so the deadlock report can tell
    /// a goroutine's own wait from the program's main thread.
    static ON_GOROUTINE_WORKER: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Threads currently suspended inside a channel wait.
static CHANNEL_WAITERS: AtomicUsize = AtomicUsize::new(0);

/// Channels holding a waiter whose operation would complete if it woke now -
/// a queued value with a receiver for it, or room for a blocked sender's.
/// A non-zero count means the program can still move even with every thread
/// inside a channel wait.
static PENDING_HANDOFFS: AtomicUsize = AtomicUsize::new(0);

/// Adds or removes one channel from the ready count.
pub fn adjust_pending_handoffs(ready: bool) {
    if ready {
        PENDING_HANDOFFS.fetch_add(1, Ordering::AcqRel);
    } else {
        PENDING_HANDOFFS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Marks its thread as suspended in a channel wait for as long as it lives.
pub(crate) struct ChannelWait;

impl ChannelWait {
    /// Enters a channel wait, reporting a deadlock when doing so leaves
    /// nothing in the program able to run.
    ///
    /// Participants are the main thread plus every queued or running
    /// goroutine task. A task waiting on a timer, on I/O, or on a blocking
    /// call is outstanding without being a channel waiter, so it keeps the
    /// count short and the program reads as able to progress - which it is.
    ///
    /// Returns `None` when every participant is waiting on a channel and no
    /// channel holds a waiter that could proceed. `can_progress` recomputes
    /// that second condition from the channels themselves, so a counter left
    /// behind by an interleaving cannot turn a working program into a
    /// failure. Nothing can deliver a
    /// value in that state, so waiting longer cannot change the answer and
    /// the caller reports a deadlock.
    ///
    /// Call with the channel's own lock held and the caller already counted
    /// among that channel's waiters, so a handoff this caller completes is
    /// visible to the readiness count.
    pub(crate) fn enter(can_progress: impl FnOnce() -> bool) -> Option<Self> {
        let waiting = CHANNEL_WAITERS.fetch_add(1, Ordering::AcqRel) + 1;
        // Reported only with no goroutine left to run: the caller is then the
        // whole program, and a channel that can hand nothing over will never
        // be able to. With a goroutine still live the counts are read while
        // it is free to change them, and a deadlock claimed over a state
        // still being written would end a working program - so that case
        // waits, exactly as it did before this check existed.
        let alone = outstanding_goroutines() == 0;
        if alone && waiting >= 1 && PENDING_HANDOFFS.load(Ordering::Acquire) == 0 && !can_progress()
        {
            CHANNEL_WAITERS.fetch_sub(1, Ordering::AcqRel);
            // A deadlock is a property of the whole program, not of the
            // goroutine that happens to notice. Ending only this goroutine
            // would leave the others asleep with nothing left to wake them,
            // so a worker reports for the program and stops it; the main
            // thread returns instead, and its error carries a call stack.
            if ON_GOROUTINE_WORKER.with(std::cell::Cell::get) {
                report_fatal_deadlock();
            }
            return None;
        }
        Some(Self)
    }
}

/// Prints the deadlock report and stops the program, matching the exit code
/// a panic produces.
fn report_fatal_deadlock() -> ! {
    use std::io::Write as _;
    let mut err = std::io::stderr();
    let _ = writeln!(
        err,
        "error: runtime error: error[GX0005]: panic: all goroutines are \
         asleep - deadlock!"
    );
    let _ = err.flush();
    std::process::exit(101);
}

impl Drop for ChannelWait {
    fn drop(&mut self) {
        CHANNEL_WAITERS.fetch_sub(1, Ordering::AcqRel);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    /// A task that blocks holds its worker, so a pool that could not grow
    /// would never start the task whose completion releases it.
    #[test]
    fn blocked_tasks_do_not_starve_a_queued_task() {
        let pool = GoroutinePool::new(1);
        let (blocked_tx, blocked_rx) = mpsc::channel::<()>();
        // Occupy every initial worker, and then some, with tasks that cannot
        // finish until released below.
        let mut releases = Vec::new();
        for _ in 0..4 {
            let blocked = blocked_tx.clone();
            let (release_tx, release_rx) = mpsc::channel::<()>();
            releases.push(release_tx);
            GoroutinePool::spawn(
                &pool,
                Box::new(move || {
                    blocked.send(()).ok();
                    release_rx.recv().ok();
                }),
            );
        }
        for _ in 0..4 {
            blocked_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("every blocking task started");
        }
        let (done_tx, done_rx) = mpsc::channel::<()>();
        GoroutinePool::spawn(
            &pool,
            Box::new(move || {
                done_tx.send(()).ok();
            }),
        );
        done_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("queued task ran while the others were blocked");
        for release in releases {
            release.send(()).ok();
        }
        pool.drain();
    }

    #[test]
    fn panicking_task_does_not_strand_drain() {
        let pool = GoroutinePool::new(1);
        let ran = Arc::new(AtomicBool::new(false));
        GoroutinePool::spawn(&pool, Box::new(|| panic!("intentional worker panic")));
        let ran_after_panic = Arc::clone(&ran);
        GoroutinePool::spawn(&pool, Box::new(move || {
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
            GoroutinePool::spawn(&pool, Box::new(move || {
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
