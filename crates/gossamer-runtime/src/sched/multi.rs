//! Work-stealing M:N scheduler.
//!
//! Each worker thread owns a [`crossbeam_deque::Worker`] local deque
//! plus a [`Stealer`] handle published into the shared `MultiState`.
//! When a worker drains its local deque it first tries the global
//! [`Injector`], then steals from a peer chosen round-robin. Spawning
//! from outside the scheduler pushes onto the injector so any worker
//! can pick the new task up.
//!
//! The model follows Go's P/M split:
//!
//! - A `Worker<SendTask>` is the "P" — the run-queue half a worker
//!   thread owns exclusively.
//! - The OS thread driving a `Worker` is the "M".
//! - Goroutines (`SendTask`) are the "G".
//!
//! When a goroutine parks (e.g. blocked on I/O or a mutex), the M
//! removes it from the local deque, hands it to the side `parked` map
//! keyed by [`Gid`], and continues running other tasks. An external
//! agent (poller, mutex-release, channel-send) calls
//! [`MultiScheduler::unpark`] to resurrect the parked goroutine onto a
//! ready queue. Workers waiting on an empty deque park themselves on a
//! per-worker [`Condvar`] until either new work lands or another
//! worker shouts via the `wake_one` helper.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_deque::{Injector, Steal, Stealer, Worker as Deque};
use parking_lot::{Condvar, Mutex};

use super::task::{Gid, Step, Task};

/// Task stored in the multi-M scheduler. Requires `Send` so workers
/// on different threads can pull from a shared queue.
pub trait SchedTask: Task + Send {}
impl<T: Task + Send> SchedTask for T {}

/// Boxed schedulable task moved through the deques and injector.
pub type SendTask = Box<dyn SchedTask + Send>;

/// Reason a goroutine has been parked. Carried alongside the task in
/// the `parked` table so introspection / debugging tools can attribute
/// the wait.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkReason {
    /// Generic park — the runtime did not specify a more specific
    /// reason.
    Other,
    /// Waiting on a channel send / receive.
    Chan,
    /// Waiting on a mutex / rwlock / once / wait-group.
    Sync,
    /// Waiting on the netpoller for a socket to become readable /
    /// writable.
    Io,
    /// Waiting on a timer to expire.
    Timer,
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
    /// Total successful steals from peer workers.
    pub steals: u64,
    /// Total successful pulls from the global injector.
    pub injects: u64,
    /// Total goroutines parked at least once.
    pub parks: u64,
    /// Total `unpark` calls that successfully resurrected a parked
    /// goroutine.
    pub unparks: u64,
}

#[derive(Default, Debug)]
struct AtomicStats {
    spawned: AtomicU64,
    finished: AtomicU64,
    steps: AtomicU64,
    yields: AtomicU64,
    steals: AtomicU64,
    injects: AtomicU64,
    parks: AtomicU64,
    unparks: AtomicU64,
}

impl AtomicStats {
    fn snapshot(&self) -> MultiStats {
        MultiStats {
            spawned: self.spawned.load(Ordering::Relaxed),
            finished: self.finished.load(Ordering::Relaxed),
            steps: self.steps.load(Ordering::Relaxed),
            yields: self.yields.load(Ordering::Relaxed),
            steals: self.steals.load(Ordering::Relaxed),
            injects: self.injects.load(Ordering::Relaxed),
            parks: self.parks.load(Ordering::Relaxed),
            unparks: self.unparks.load(Ordering::Relaxed),
        }
    }
}

/// Per-worker shared handles published into [`Shared`] so peers can
/// steal from this worker and so the scheduler can wake it.
struct WorkerSlot {
    /// Steal half of this worker's deque. Used by other workers when
    /// their local deque is empty.
    #[allow(dead_code)]
    stealer: Stealer<SendTask>,
    /// Per-worker incoming queue. `unpark(gid)` pushes the
    /// resurrected task onto the home worker's `inbox` so the
    /// goroutine resumes on the same OS thread it parked on.
    /// Required because stackful coroutines from `gossamer-coro`
    /// (corosensei) are not safe to migrate across OS threads
    /// while suspended.
    inbox: Injector<SendTask>,
    /// `true` while the OS thread for this worker is parked on the
    /// `cv` waiting for new work.
    parked: AtomicBool,
    /// Mutex/condvar pair — workers park here when their deque is
    /// empty; spawn / unpark calls notify this condvar.
    cv: Condvar,
    cv_mu: Mutex<()>,
    /// `true` when this slot has been retired (e.g. because
    /// `set_max_procs` shrank the worker count). Workers consult this
    /// before parking and exit.
    retired: AtomicBool,
    /// Opaque OS-thread handle (Unix: `pthread_t` cast to `u64`,
    /// other platforms: 0). Captured by `worker_loop` on entry; the
    /// watchdog uses it to send a targeted SIGURG via
    /// [`crate::preempt::signal_thread_sigurg`] when this
    /// worker's task overstays its budget.
    thread_handle: AtomicU64,
}

impl WorkerSlot {
    fn wake(&self) {
        if self.parked.swap(false, Ordering::AcqRel) {
            // Notify; the lock is held briefly only as the condvar
            // contract requires.
            let _g = self.cv_mu.lock();
            self.cv.notify_one();
        }
    }
}

/// State shared across every worker thread plus user-facing handles.
struct Shared {
    injector: Injector<SendTask>,
    workers: Mutex<Vec<Arc<WorkerSlot>>>,
    parked: Mutex<HashMap<Gid, ParkedEntry>>,
    /// Gids whose `unpark(gid)` arrived *before* the suspending
    /// worker had a chance to insert them into `parked`. The
    /// worker's Yield→park path checks this set and, if the gid
    /// is present, immediately re-ejects the task to the
    /// injector. Closes the wake-before-park race window.
    pre_unpark: Mutex<std::collections::HashSet<Gid>>,
    next_gid: AtomicU64,
    /// Live (spawned but not yet finished) goroutine count. The
    /// scheduler refuses new spawns above `max_live`.
    live_goroutines: AtomicUsize,
    /// Maximum live goroutines this scheduler will admit. Honours
    /// `runtime::set_max_procs` and `GOSSAMER_MAX_PROCS`. Default is
    /// `1_000_000`.
    max_live: AtomicUsize,
    stats: AtomicStats,
    /// Set to `true` when [`MultiScheduler::shutdown`] is called.
    /// Workers exit once their local deque is drained.
    stopping: AtomicBool,
    /// Number of running worker threads. Used to coordinate dynamic
    /// resize.
    live_workers: AtomicUsize,
    /// Most recent target P count. The active worker pool is grown to
    /// match.
    target_workers: AtomicUsize,
    /// `true` once the watchdog thread has been spawned.
    watchdog_started: AtomicBool,
    /// Set when the scheduler should request that all goroutines
    /// reach a safepoint (used by the GC).
    request_safepoint: AtomicBool,
    /// Per-worker timestamps of the last yield observed; used by the
    /// watchdog to decide which workers to preempt.
    last_yield: Mutex<Vec<Instant>>,
    /// Idle signal — workers notify when they reach a quiescent
    /// state (deque empty + no peer work + no parked tasks). The
    /// orchestrator's `wait_until_idle` parks on this Condvar
    /// instead of polling, so an empty `gos run` does not consume
    /// CPU on the main thread while the workers are idle.
    idle_mu: Mutex<()>,
    idle_cv: Condvar,
}

struct ParkedEntry {
    task: SendTask,
    #[allow(dead_code)]
    reason: ParkReason,
    /// Hint indicating which worker this task previously ran on, used
    /// to maintain locality on resume.
    home: usize,
}

impl fmt::Debug for Shared {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("Shared")
            .field("injector_len", &self.injector.len())
            .field("live_workers", &self.live_workers.load(Ordering::Relaxed))
            .field(
                "target_workers",
                &self.target_workers.load(Ordering::Relaxed),
            )
            .field("parked", &self.parked.lock().len())
            .field("stats", &self.stats.snapshot())
            .finish_non_exhaustive()
    }
}

/// Multi-threaded work-stealing scheduler.
#[derive(Clone)]
pub struct MultiScheduler {
    inner: Arc<Shared>,
}

impl fmt::Debug for MultiScheduler {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        out.debug_struct("MultiScheduler")
            .field("shared", &self.inner)
            .finish()
    }
}

impl MultiScheduler {
    /// Returns a scheduler sized for `worker_count` workers (clamped
    /// to at least 1). The workers are not spawned until [`Self::run`]
    /// or [`Self::start`] is called.
    #[must_use]
    pub fn new(worker_count: usize) -> Self {
        let n = worker_count.max(1);
        let shared = Arc::new(Shared {
            injector: Injector::new(),
            workers: Mutex::new(Vec::new()),
            parked: Mutex::new(HashMap::new()),
            pre_unpark: Mutex::new(std::collections::HashSet::new()),
            next_gid: AtomicU64::new(0),
            live_goroutines: AtomicUsize::new(0),
            max_live: AtomicUsize::new(default_max_live()),
            stats: AtomicStats::default(),
            stopping: AtomicBool::new(false),
            live_workers: AtomicUsize::new(0),
            target_workers: AtomicUsize::new(n),
            watchdog_started: AtomicBool::new(false),
            request_safepoint: AtomicBool::new(false),
            last_yield: Mutex::new(Vec::new()),
            idle_mu: Mutex::new(()),
            idle_cv: Condvar::new(),
        });
        Self { inner: shared }
    }

    /// Pushes a task onto the global injector. Workers that have an
    /// empty local deque will pick it up.
    ///
    /// Returns `None` when the live-goroutine cap (set via
    /// `runtime::set_max_procs` or `GOSSAMER_MAX_PROCS`) would be
    /// exceeded — surface the refusal to user code instead of
    /// silently overcommitting kernel resources.
    pub fn try_spawn<T: SchedTask + 'static>(&self, task: T) -> Option<Gid> {
        let max = self.inner.max_live.load(Ordering::Relaxed);
        let prev = self.inner.live_goroutines.fetch_add(1, Ordering::AcqRel);
        if prev >= max {
            self.inner.live_goroutines.fetch_sub(1, Ordering::AcqRel);
            return None;
        }
        let raw = self.inner.next_gid.fetch_add(1, Ordering::Relaxed);
        let gid = Gid(u32::try_from(raw & 0xFFFF_FFFF).unwrap_or(u32::MAX));
        crate::sigquit::register(gid.as_u32(), std::any::type_name::<T>());
        // Wrap the task with a `GidStamped` adapter so the worker
        // publishes the goroutine's `gid` into the race-detector
        // thread-local before each `step` and clears it after. This
        // is a no-op when the race detector is disabled (the only
        // cost is one TLS write per step).
        let stamped = GidStamped { gid, inner: task };
        self.inner.injector.push(Box::new(stamped));
        self.inner.stats.spawned.fetch_add(1, Ordering::Relaxed);
        self.wake_any();
        Some(gid)
    }

    /// Backwards-compatible wrapper: refusal panics. Callers that
    /// need graceful refusal should use `try_spawn`.
    pub fn spawn<T: SchedTask + 'static>(&self, task: T) -> Gid {
        self.try_spawn(task)
            .expect("MultiScheduler::spawn refused: live-goroutine cap reached")
    }

    /// Sets the maximum live-goroutine count. Returns the previous
    /// value. A value of zero disables the cap (interpreted as
    /// `usize::MAX`).
    #[must_use]
    pub fn set_max_goroutines(&self, n: usize) -> usize {
        let new = if n == 0 { usize::MAX } else { n };
        self.inner.max_live.swap(new, Ordering::AcqRel)
    }

    /// Current live-goroutine count.
    #[must_use]
    pub fn live_goroutines(&self) -> usize {
        self.inner.live_goroutines.load(Ordering::Relaxed)
    }

    /// Resizes the worker pool to `n`. Honoured asynchronously: extra
    /// workers are spawned immediately; surplus workers retire after
    /// finishing their current task. A value of `0` is clamped to one.
    /// Values larger than the worker-count cap (see
    /// [`Self::worker_count_cap`]) are clamped down so a runaway
    /// caller cannot exhaust kernel-thread budget by asking for tens
    /// of thousands of OS threads.
    pub fn set_worker_count(&self, n: usize) {
        let cap = Self::worker_count_cap();
        let target = n.clamp(1, cap);
        self.inner.target_workers.store(target, Ordering::Relaxed);
        self.reconcile_pool();
    }

    /// Hard upper bound on the worker pool. The default is
    /// `min(num_cpus * 4, 256)`; the `GOSSAMER_MAX_WORKERS`
    /// environment variable overrides it (clamped to `[1, 4096]`).
    /// Exposed so tests and tooling can read the bound the same way
    /// `set_worker_count` enforces it.
    #[must_use]
    pub fn worker_count_cap() -> usize {
        if let Ok(v) = std::env::var("GOSSAMER_MAX_WORKERS") {
            if let Ok(n) = v.parse::<usize>() {
                return n.clamp(1, 4096);
            }
        }
        let cores = std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        cores.saturating_mul(4).clamp(1, 256)
    }

    /// Returns the configured target worker count.
    #[must_use]
    pub fn worker_count(&self) -> usize {
        self.inner.target_workers.load(Ordering::Relaxed)
    }

    /// Snapshot of scheduler statistics.
    #[must_use]
    pub fn stats(&self) -> MultiStats {
        self.inner.stats.snapshot()
    }

    /// Drives the scheduler until every spawned task (and every task
    /// any of those spawn) has finished. Returns the final statistics.
    /// Callers may push additional work before the final completion.
    #[must_use]
    pub fn run(&self) -> MultiStats {
        self.inner.stopping.store(false, Ordering::Release);
        crate::preempt::init();
        crate::sigquit::install_handler();
        self.start_watchdog();
        self.reconcile_pool();
        self.wait_until_idle();
        self.shutdown();
        self.inner.stats.snapshot()
    }

    /// Starts the worker pool without blocking. Tasks pushed via
    /// [`Self::spawn`] run in the background; call [`Self::shutdown`]
    /// to drain workers when done.
    pub fn start(&self) {
        self.inner.stopping.store(false, Ordering::Release);
        crate::preempt::init();
        crate::sigquit::install_handler();
        self.start_watchdog();
        self.reconcile_pool();
    }

    fn start_watchdog(&self) {
        if self.inner.watchdog_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let inner = Arc::clone(&self.inner);
        thread::Builder::new()
            .name("gos-preempt-watchdog".to_string())
            .spawn(move || watchdog_loop(inner))
            .expect("spawn watchdog");
    }

    /// Signals every worker to exit once their deques drain, then
    /// joins them.
    pub fn shutdown(&self) {
        self.inner.stopping.store(true, Ordering::Release);
        let workers = self.inner.workers.lock().clone();
        for slot in &workers {
            slot.retired.store(true, Ordering::Release);
            slot.wake();
        }
        // Joining the threads themselves happens inside
        // `reconcile_pool` / the ParkedJoinHandles store; there is no
        // join handle stored here because workers self-detach when
        // they retire. A future iteration could move handles into
        // `Shared` for a deterministic join.
    }

    /// Parks the goroutine identified by `gid`. The supplied `task`
    /// is held until [`Self::unpark`] resurrects it. The `home` hint
    /// indicates which worker should pick the task back up; values
    /// outside the worker count fall through to the injector.
    pub fn park(&self, gid: Gid, reason: ParkReason, home: usize, task: SendTask) {
        self.inner
            .parked
            .lock()
            .insert(gid, ParkedEntry { task, reason, home });
        self.inner.stats.parks.fetch_add(1, Ordering::Relaxed);
    }

    /// Resurrects a previously parked goroutine. Returns `true` when a
    /// parked entry was found and re-enqueued.
    ///
    /// If the gid is not yet in `parked` — because the goroutine has
    /// armed its wakeup source but hasn't suspended yet — the gid is
    /// recorded in `pre_unpark`. The worker that's about to park the
    /// task checks this set and, if the gid is present, re-ejects
    /// the task to the injector instead of leaving it parked.
    pub fn unpark(&self, gid: Gid) -> bool {
        let entry = self.inner.parked.lock().remove(&gid);
        let Some(entry) = entry else {
            // Park hasn't landed yet. Record so the worker's
            // `pre_unpark` check sees it.
            self.inner.pre_unpark.lock().insert(gid);
            return false;
        };
        let home = entry.home;
        let preferred = {
            let workers = self.inner.workers.lock();
            workers.get(home).map(Arc::clone)
        };
        if let Some(slot) = preferred {
            // Pin the resumed goroutine to the home worker —
            // stackful coroutines are not safe to migrate across
            // OS threads while suspended. Push onto the worker's
            // private inbox; the worker drains it before its main
            // deque on the next iteration.
            slot.inbox.push(entry.task);
            slot.wake();
        } else {
            // Home worker retired; fall back to the global
            // injector. (This only happens during shutdown or a
            // shrinking pool resize.)
            self.inner.injector.push(entry.task);
            self.wake_any();
        }
        self.inner.stats.unparks.fetch_add(1, Ordering::Relaxed);
        true
    }

    /// Returns the number of currently parked goroutines. Exposed for
    /// tests and introspection.
    #[must_use]
    pub fn parked_count(&self) -> usize {
        self.inner.parked.lock().len()
    }

    /// Asks every running goroutine to reach a safepoint at its next
    /// poll. Used by the GC before the concurrent mark phase.
    pub fn request_safepoint(&self) {
        self.inner.request_safepoint.store(true, Ordering::Release);
        crate::preempt::request_yield_all();
    }

    /// Clears the safepoint request once the caller is done.
    pub fn clear_safepoint(&self) {
        self.inner.request_safepoint.store(false, Ordering::Release);
    }

    fn reconcile_pool(&self) {
        let target = self.inner.target_workers.load(Ordering::Relaxed);
        let current = self.inner.live_workers.load(Ordering::Relaxed);
        if current >= target {
            // Mark surplus slots retired; they exit on next park.
            let workers = self.inner.workers.lock();
            for slot in workers.iter().skip(target) {
                slot.retired.store(true, Ordering::Release);
                slot.wake();
            }
            return;
        }
        for index in current..target {
            self.spawn_worker(index);
        }
    }

    fn spawn_worker(&self, index: usize) {
        let deque: Deque<SendTask> = Deque::new_fifo();
        let stealer = deque.stealer();
        let slot = Arc::new(WorkerSlot {
            stealer,
            inbox: Injector::new(),
            parked: AtomicBool::new(false),
            cv: Condvar::new(),
            cv_mu: Mutex::new(()),
            retired: AtomicBool::new(false),
            thread_handle: AtomicU64::new(0),
        });
        {
            let mut workers = self.inner.workers.lock();
            // Slot vector grows monotonically; we may overwrite a
            // retired slot if `index < workers.len()`.
            if index < workers.len() {
                workers[index] = Arc::clone(&slot);
            } else {
                while workers.len() < index {
                    // Pad: if for some reason we're spawning out of
                    // order, fill with retired placeholders.
                    let placeholder = Arc::new(WorkerSlot {
                        stealer: Deque::<SendTask>::new_fifo().stealer(),
                        inbox: Injector::new(),
                        parked: AtomicBool::new(false),
                        cv: Condvar::new(),
                        cv_mu: Mutex::new(()),
                        retired: AtomicBool::new(true),
                        thread_handle: AtomicU64::new(0),
                    });
                    workers.push(placeholder);
                }
                workers.push(Arc::clone(&slot));
            }
        }
        self.inner.live_workers.fetch_add(1, Ordering::AcqRel);
        let inner = Arc::clone(&self.inner);
        let _: JoinHandle<()> = thread::Builder::new()
            .name(format!("gos-sched-{index}"))
            .spawn(move || worker_loop(index, deque, slot, inner))
            .expect("scheduler worker thread spawn failed");
    }

    fn wake_any(&self) {
        let workers = self.inner.workers.lock();
        for slot in workers.iter() {
            if slot.parked.load(Ordering::Acquire) {
                slot.wake();
                return;
            }
        }
    }

    fn wait_until_idle(&self) {
        let mut g = self.inner.idle_mu.lock();
        loop {
            if self.is_idle_snapshot() {
                return;
            }
            // Bounded wait so a missed wake-up never strands the
            // orchestrator. The bound is loose (200 ms) because
            // workers actively notify on every transition that
            // could make us idle — the timeout is only a safety
            // net for races during scheduler resize / shutdown.
            self.inner
                .idle_cv
                .wait_for(&mut g, Duration::from_millis(200));
        }
    }

    fn is_idle_snapshot(&self) -> bool {
        let injector_empty = self.inner.injector.is_empty();
        let parked_empty = self.inner.parked.lock().is_empty();
        let workers = self.inner.workers.lock();
        let all_parked = !workers.is_empty()
            && workers
                .iter()
                .all(|s| s.parked.load(Ordering::Acquire) || s.retired.load(Ordering::Acquire));
        let no_local_work = workers.iter().all(|s| s.stealer.is_empty());
        drop(workers);
        injector_empty && parked_empty && all_parked && no_local_work
    }
}

fn worker_loop(index: usize, deque: Deque<SendTask>, slot: Arc<WorkerSlot>, shared: Arc<Shared>) {
    // Round-robin steal cursor — biases away from always poking the
    // same peer first, which would imbalance work.
    let mut steal_cursor = index.wrapping_add(1);
    // Publish this thread's pthread_t so the watchdog can pthread_kill
    // a stuck worker (Defense #2). Released so the watchdog observes
    // the value before it tries to use it.
    slot.thread_handle
        .store(crate::preempt::current_thread_handle(), Ordering::Release);
    {
        let mut last = shared.last_yield.lock();
        while last.len() <= index {
            last.push(Instant::now());
        }
    }
    loop {
        if slot.retired.load(Ordering::Acquire) {
            shared.live_workers.fetch_sub(1, Ordering::AcqRel);
            return;
        }
        let task = next_task(index, &deque, &slot, &shared, &mut steal_cursor);
        let Some(mut task) = task else {
            if shared.stopping.load(Ordering::Acquire) {
                shared.live_workers.fetch_sub(1, Ordering::AcqRel);
                // The exiting worker may have been the last one
                // holding wait_until_idle awake. Notify in case
                // shutdown is in progress.
                let _g = shared.idle_mu.lock();
                shared.idle_cv.notify_all();
                return;
            }
            // About to park: tell wait_until_idle to re-snapshot.
            // The Condvar is signalled inside park_worker after the
            // parked flag is set so the snapshot sees a consistent
            // view.
            park_worker(&slot, &shared);
            continue;
        };
        let step = task.step();
        shared.stats.steps.fetch_add(1, Ordering::Relaxed);
        match step {
            Step::Yield => {
                shared.stats.yields.fetch_add(1, Ordering::Relaxed);
                if let Some(slot) = shared.last_yield.lock().get_mut(index) {
                    *slot = Instant::now();
                }
                // The goroutine may have requested a park via
                // `sched_global::park`. The park helper writes
                // `(gid, reason)` into a thread-local slot before
                // suspending; we honour it here so the suspended
                // goroutine sits in the parked map until its
                // wakeup source unparks it, instead of busy-
                // looping back through the run queue.
                if let Some((gid, reason)) = crate::sched_global::take_pending_park() {
                    let mut parked = shared.parked.lock();
                    parked.insert(
                        gid,
                        ParkedEntry {
                            task,
                            reason,
                            home: index,
                        },
                    );
                    shared.stats.parks.fetch_add(1, Ordering::Relaxed);
                    // Race-window protection: if `unpark(gid)`
                    // already fired (poller delivery between
                    // `arm()` and the park insertion), the gid is
                    // queued in `pre_unpark`. Drain that and, if
                    // our gid is in it, immediately re-eject the
                    // task back onto the injector.
                    let mut pre = shared.pre_unpark.lock();
                    if pre.remove(&gid) {
                        if let Some(entry) = parked.remove(&gid) {
                            drop(pre);
                            drop(parked);
                            shared.injector.push(entry.task);
                            shared.stats.unparks.fetch_add(1, Ordering::Relaxed);
                            // Wake any worker that may have parked
                            // while there was no work — we just
                            // produced some.
                            let workers = shared.workers.lock();
                            for slot in workers.iter() {
                                if slot.parked.load(Ordering::Acquire) {
                                    slot.wake();
                                    break;
                                }
                            }
                        }
                    }
                } else {
                    deque.push(task);
                }
            }
            Step::Done => {
                shared.stats.finished.fetch_add(1, Ordering::Relaxed);
                shared.live_goroutines.fetch_sub(1, Ordering::AcqRel);
                if let Some(slot) = shared.last_yield.lock().get_mut(index) {
                    *slot = Instant::now();
                }
            }
        }
    }
}

/// Default `MultiScheduler::max_live` — 1M live goroutines, or
/// `GOSSAMER_MAX_GOROUTINES` if set. Surfaces a
/// `for _ in 0.. { go work() }` runaway as a refused spawn rather
/// than a kernel-thread OOM.
///
/// `GOSSAMER_MAX_PROCS` controls the worker (P) count, not the
/// live-goroutine cap; the two were previously conflated, which
/// surprised callers that wanted "use 4 cores" but inadvertently
/// limited themselves to 4 live goroutines. Use
/// `GOSSAMER_MAX_GOROUTINES` to cap the goroutine count.
fn default_max_live() -> usize {
    if let Ok(s) = std::env::var("GOSSAMER_MAX_GOROUTINES") {
        if let Ok(n) = s.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    1_000_000
}

/// Watchdog loop: every ~5 ms, checks per-worker `last_yield`
/// timestamps and bumps the global preempt phase for any worker
/// that has been running without yielding for more than 10 ms.
/// Compiled / interpreter code is expected to call into
/// [`crate::preempt::should_yield`] at safepoints and
/// honour the request.
///
/// Defense #2: when a worker has been running for more than
/// `kill_threshold`, the watchdog also sends SIGURG to that worker's
/// OS thread. The cooperative bump alone is silent if the worker is
/// inside a tight C-side loop or a blocking syscall; the kernel
/// signal interrupts both.
fn watchdog_loop(shared: Arc<Shared>) {
    let preempt_threshold = Duration::from_millis(10);
    let kill_threshold = Duration::from_millis(100);
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            // One last bump so any spinning thread observes the
            // shutdown and reaches a safepoint.
            crate::preempt::request_yield_all();
            return;
        }
        thread::sleep(Duration::from_millis(5));
        let now = Instant::now();
        let timestamps = shared.last_yield.lock().clone();
        let mut needs_preempt = false;
        let mut kill_indices: Vec<usize> = Vec::new();
        for (i, ts) in timestamps.iter().enumerate() {
            let elapsed = now.saturating_duration_since(*ts);
            if elapsed > preempt_threshold {
                needs_preempt = true;
            }
            if elapsed > kill_threshold {
                kill_indices.push(i);
            }
        }
        if needs_preempt || shared.request_safepoint.load(Ordering::Acquire) {
            crate::preempt::request_yield_all();
            crate::preempt::bump_pressure();
        }
        if !kill_indices.is_empty() {
            let workers = shared.workers.lock();
            for i in kill_indices {
                if let Some(slot) = workers.get(i) {
                    let handle = slot.thread_handle.load(Ordering::Acquire);
                    let _ = crate::preempt::signal_thread_sigurg(handle);
                }
            }
        }
    }
}

fn next_task(
    _index: usize,
    deque: &Deque<SendTask>,
    self_slot: &Arc<WorkerSlot>,
    shared: &Arc<Shared>,
    steal_cursor: &mut usize,
) -> Option<SendTask> {
    if let Some(task) = deque.pop() {
        return Some(task);
    }
    // 1) own inbox — unparked goroutines pinned to this worker
    loop {
        match self_slot.inbox.steal_batch_and_pop(deque) {
            Steal::Success(task) => {
                shared.stats.unparks.fetch_add(1, Ordering::Relaxed);
                return Some(task);
            }
            Steal::Empty => break,
            Steal::Retry => {}
        }
    }
    // 2) global injector (new spawns from the main thread or from
    //    other non-worker contexts)
    loop {
        match shared.injector.steal_batch_and_pop(deque) {
            Steal::Success(task) => {
                shared.stats.injects.fetch_add(1, Ordering::Relaxed);
                return Some(task);
            }
            Steal::Empty => break,
            Steal::Retry => {}
        }
    }
    // Peer-stealing is disabled: stackful coroutines from
    // [`gossamer_coro::Goroutine`] (built on `corosensei`) are not
    // safe to migrate across OS worker threads while suspended.
    // Once a goroutine lands on a worker (via the global injector
    // on first spawn, or on its `home` worker after unpark), it
    // stays there for its lifetime. The trade-off: load imbalance
    // under non-uniform per-goroutine work; for the dominant
    // HTTP keep-alive shape (each connection = one goroutine,
    // uniform per-request work), all workers stay busy because
    // the injector hands out new connections one by one.
    let _ = self_slot;
    let _ = steal_cursor;
    let _ = shared;
    None
}

fn park_worker(slot: &Arc<WorkerSlot>, shared: &Arc<Shared>) {
    slot.parked.store(true, Ordering::Release);
    // Wake any orchestrator waiting in `wait_until_idle`: now that
    // this worker is parked, the snapshot may show all-idle.
    {
        let _g = shared.idle_mu.lock();
        shared.idle_cv.notify_all();
    }
    let mut g = slot.cv_mu.lock();
    // Brief timeout so a missed wake doesn't strand the worker forever.
    let _ = slot.cv.wait_for(&mut g, Duration::from_millis(50));
    slot.parked.store(false, Ordering::Release);
}

#[allow(dead_code)]
fn since_ms(t: Instant) -> u64 {
    let ms = t.elapsed().as_millis().min(u128::from(u64::MAX));
    u64::try_from(ms).unwrap_or(u64::MAX)
}

/// Task adapter that publishes the goroutine's `gid` into the
/// race-detector thread-local for the duration of each `step`,
/// so `crate::race::current_gid` returns the right
/// value when the task touches a sync primitive. Cleared after
/// the step so the host thread's gid does not pollute work the
/// scheduler queue runs between tasks.
struct GidStamped<T> {
    gid: Gid,
    inner: T,
}

impl<T: Task> Task for GidStamped<T> {
    fn step(&mut self) -> Step {
        crate::race::set_current_gid(self.gid.as_u32());
        crate::sched_global::set_current_gid(self.gid);
        let result = self.inner.step();
        crate::sched_global::clear_current_gid();
        crate::race::set_current_gid(0);
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountTask {
        counter: Arc<AtomicUsize>,
        budget: usize,
    }

    impl Task for CountTask {
        fn step(&mut self) -> Step {
            if self.budget == 0 {
                return Step::Done;
            }
            self.budget -= 1;
            self.counter.fetch_add(1, Ordering::Relaxed);
            if self.budget == 0 {
                Step::Done
            } else {
                Step::Yield
            }
        }
    }

    #[test]
    fn drains_all_tasks_across_workers() {
        let sched = MultiScheduler::new(4);
        let counter = Arc::new(AtomicUsize::new(0));
        for _ in 0..256 {
            sched.spawn(CountTask {
                counter: Arc::clone(&counter),
                budget: 8,
            });
        }
        let stats = sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 256 * 8);
        assert_eq!(stats.finished, 256);
        assert!(stats.steps >= 256 * 8);
    }

    #[test]
    fn park_unpark_round_trip() {
        let sched = MultiScheduler::new(2);
        sched.start();
        // Push a parked task directly.
        let task: SendTask = Box::new(CountTask {
            counter: Arc::new(AtomicUsize::new(0)),
            budget: 1,
        });
        let gid = Gid(99);
        sched.park(gid, ParkReason::Other, 0, task);
        assert_eq!(sched.parked_count(), 1);
        assert!(sched.unpark(gid));
        assert_eq!(sched.parked_count(), 0);
        let _ = sched.run();
    }

    #[test]
    fn set_worker_count_grows_pool() {
        let sched = MultiScheduler::new(1);
        sched.start();
        sched.set_worker_count(4);
        assert_eq!(sched.worker_count(), 4);
        let _ = sched.run();
    }
}
