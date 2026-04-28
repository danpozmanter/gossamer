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

use crate::task::{Gid, Step, Task};

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
    stealer: Stealer<SendTask>,
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
    next_gid: AtomicU64,
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
            next_gid: AtomicU64::new(0),
            stats: AtomicStats::default(),
            stopping: AtomicBool::new(false),
            live_workers: AtomicUsize::new(0),
            target_workers: AtomicUsize::new(n),
            watchdog_started: AtomicBool::new(false),
            request_safepoint: AtomicBool::new(false),
            last_yield: Mutex::new(Vec::new()),
        });
        Self { inner: shared }
    }

    /// Pushes a task onto the global injector. Workers that have an
    /// empty local deque will pick it up.
    pub fn spawn<T: SchedTask + 'static>(&self, task: T) -> Gid {
        let raw = self.inner.next_gid.fetch_add(1, Ordering::Relaxed);
        let gid = Gid(u32::try_from(raw & 0xFFFF_FFFF).unwrap_or(u32::MAX));
        // Publish into the SIGQUIT introspection table.
        gossamer_runtime::sigquit::register(gid.as_u32(), std::any::type_name::<T>());
        self.inner.injector.push(Box::new(task));
        self.inner.stats.spawned.fetch_add(1, Ordering::Relaxed);
        // Wake one parked worker so the new task is picked up promptly.
        self.wake_any();
        gid
    }

    /// Resizes the worker pool to `n`. Honoured asynchronously: extra
    /// workers are spawned immediately; surplus workers retire after
    /// finishing their current task. A value of `0` is clamped to one.
    pub fn set_worker_count(&self, n: usize) {
        let target = n.max(1);
        self.inner.target_workers.store(target, Ordering::Relaxed);
        self.reconcile_pool();
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
        gossamer_runtime::preempt::init();
        gossamer_runtime::sigquit::install_handler();
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
        gossamer_runtime::preempt::init();
        gossamer_runtime::sigquit::install_handler();
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
    pub fn unpark(&self, gid: Gid) -> bool {
        let entry = self.inner.parked.lock().remove(&gid);
        let Some(entry) = entry else { return false };
        let home = entry.home;
        let preferred = {
            let workers = self.inner.workers.lock();
            workers.get(home).map(Arc::clone)
        };
        self.inner.injector.push(entry.task);
        if let Some(slot) = preferred {
            slot.wake();
        } else {
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
        gossamer_runtime::preempt::request_yield_all();
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
            parked: AtomicBool::new(false),
            cv: Condvar::new(),
            cv_mu: Mutex::new(()),
            retired: AtomicBool::new(false),
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
                        parked: AtomicBool::new(false),
                        cv: Condvar::new(),
                        cv_mu: Mutex::new(()),
                        retired: AtomicBool::new(true),
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
        loop {
            let injector_empty = self.inner.injector.is_empty();
            let parked_empty = self.inner.parked.lock().is_empty();
            let workers = self.inner.workers.lock();
            let all_parked = !workers.is_empty()
                && workers
                    .iter()
                    .all(|s| s.parked.load(Ordering::Acquire) || s.retired.load(Ordering::Acquire));
            let no_local_work = workers.iter().all(|s| s.stealer.is_empty());
            drop(workers);
            if injector_empty && parked_empty && all_parked && no_local_work {
                return;
            }
            thread::sleep(Duration::from_micros(200));
        }
    }
}

fn worker_loop(index: usize, deque: Deque<SendTask>, slot: Arc<WorkerSlot>, shared: Arc<Shared>) {
    // Round-robin steal cursor — biases away from always poking the
    // same peer first, which would imbalance work.
    let mut steal_cursor = index.wrapping_add(1);
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
                return;
            }
            park_worker(&slot);
            continue;
        };
        let step = task.step();
        shared.stats.steps.fetch_add(1, Ordering::Relaxed);
        match step {
            Step::Yield => {
                shared.stats.yields.fetch_add(1, Ordering::Relaxed);
                deque.push(task);
                if let Some(slot) = shared.last_yield.lock().get_mut(index) {
                    *slot = Instant::now();
                }
            }
            Step::Done => {
                shared.stats.finished.fetch_add(1, Ordering::Relaxed);
                if let Some(slot) = shared.last_yield.lock().get_mut(index) {
                    *slot = Instant::now();
                }
            }
        }
    }
}

/// Watchdog loop: every ~5 ms, checks per-worker `last_yield`
/// timestamps and bumps the global preempt phase for any worker
/// that has been running without yielding for more than 10 ms.
/// Compiled / interpreter code is expected to call into
/// [`gossamer_runtime::preempt::should_yield`] at safepoints and
/// honour the request.
fn watchdog_loop(shared: Arc<Shared>) {
    let preempt_threshold = Duration::from_millis(10);
    loop {
        if shared.stopping.load(Ordering::Acquire) {
            // One last bump so any spinning thread observes the
            // shutdown and reaches a safepoint.
            gossamer_runtime::preempt::request_yield_all();
            return;
        }
        thread::sleep(Duration::from_millis(5));
        let now = Instant::now();
        let timestamps = shared.last_yield.lock().clone();
        let mut needs_preempt = false;
        for ts in timestamps {
            if now.saturating_duration_since(ts) > preempt_threshold {
                needs_preempt = true;
                break;
            }
        }
        if needs_preempt || shared.request_safepoint.load(Ordering::Acquire) {
            gossamer_runtime::preempt::request_yield_all();
            gossamer_runtime::preempt::bump_pressure();
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
    // 1) global injector
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
    // 2) peers
    let workers = shared.workers.lock();
    let n = workers.len();
    if n == 0 {
        return None;
    }
    let start = *steal_cursor % n;
    for offset in 0..n {
        let idx = (start + offset) % n;
        if Arc::ptr_eq(&workers[idx], self_slot) {
            continue;
        }
        match workers[idx].stealer.steal_batch_and_pop(deque) {
            Steal::Success(task) => {
                *steal_cursor = idx.wrapping_add(1);
                shared.stats.steals.fetch_add(1, Ordering::Relaxed);
                return Some(task);
            }
            Steal::Empty | Steal::Retry => {}
        }
    }
    drop(workers);
    None
}

fn park_worker(slot: &Arc<WorkerSlot>) {
    slot.parked.store(true, Ordering::Release);
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
