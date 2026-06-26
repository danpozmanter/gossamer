//! Single-threaded cooperative scheduler for wasm32-unknown-unknown.
//!
//! The native [`super::multi`] is a work-stealing M:N runtime built on
//! OS threads, crossbeam deques, and a mio netpoller. None of those
//! exist in a browser, so the wasm playground runs goroutines
//! cooperatively: a spawned task is driven to completion immediately
//! on the calling stack, matching the eager `gossamer_coro::Goroutine`
//! shim. A goroutine that tries to block (channel wait with no ready
//! value, mutex contention, real I/O) reaches `gossamer_coro::suspend`,
//! which panics with the documented "blocking not supported" message -
//! the cooperative-single-thread v1 limit.
//!
//! This module re-exports the same public surface as the native one
//! (`ParkReason`, `SchedTask`, `SendTask`, `MultiStats`,
//! `MultiScheduler`) so `gossamer_sched` and `gossamer_std` compile
//! unchanged.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

use super::task::{Gid, Step, Task};

/// Marker for any [`Task`] that can be scheduled. Single-threaded here,
/// but the `Send` bound is kept for source compatibility with native.
pub trait SchedTask: Task + Send {}
impl<T: Task + Send> SchedTask for T {}

/// Boxed schedulable task, mirroring the native alias.
pub type SendTask = Box<dyn SchedTask + Send>;

/// Reason a goroutine has parked. Carried for diagnostic parity with
/// the native scheduler; the wasm runtime never actually parks (a
/// would-be park diverges through `gossamer_coro::suspend`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParkReason {
    /// Generic park.
    Other,
    /// Waiting on a channel send / receive.
    Chan,
    /// Waiting on a mutex / rwlock / once / wait-group.
    Sync,
    /// Waiting on the netpoller for a socket.
    Io,
    /// Waiting on a timer.
    Timer,
}

/// Scheduler counters, mirroring the native [`super::multi::MultiStats`].
#[derive(Debug, Clone, Copy, Default)]
pub struct MultiStats {
    /// Total tasks spawned.
    pub spawned: u64,
    /// Total tasks completed.
    pub finished: u64,
    /// Total `Task::step` calls issued.
    pub steps: u64,
    /// Total [`Step::Yield`] observations.
    pub yields: u64,
    /// Total successful steals from peer workers (always 0 here).
    pub steals: u64,
    /// Total successful pulls from the global injector (always 0 here).
    pub injects: u64,
    /// Total goroutines parked at least once (always 0 here).
    pub parks: u64,
    /// Total successful `unpark` calls (always 0 here).
    pub unparks: u64,
}

/// Cooperative single-threaded scheduler. `spawn` runs the task to
/// completion immediately; there is no run queue or worker pool.
pub struct MultiScheduler {
    next_gid: AtomicU32,
    spawned: AtomicU64,
    finished: AtomicU64,
    steps: AtomicU64,
}

impl MultiScheduler {
    /// Constructs the scheduler. `worker_count` is ignored - the wasm
    /// runtime is single-threaded.
    #[must_use]
    pub fn new(_worker_count: usize) -> Self {
        Self {
            next_gid: AtomicU32::new(0),
            spawned: AtomicU64::new(0),
            finished: AtomicU64::new(0),
            steps: AtomicU64::new(0),
        }
    }

    fn run_to_completion<T: SchedTask + 'static>(&self, mut task: T) -> Gid {
        let gid = Gid(self.next_gid.fetch_add(1, Ordering::Relaxed));
        self.spawned.fetch_add(1, Ordering::Relaxed);
        // Make the goroutine's gid observable to `current_gid()` while
        // its body runs, restoring the caller's gid afterwards so
        // nested eager spawns see the correct identity.
        let prev = crate::sched_global::current_gid_raw();
        crate::sched_global::set_current_gid(gid);
        // The eager coro shim resumes the body to completion (or
        // catches its panic) on the first step, so this settles every
        // run-to-completion goroutine in one iteration; the loop is a
        // safety net should a non-coro task yield.
        loop {
            self.steps.fetch_add(1, Ordering::Relaxed);
            if task.step() == Step::Done {
                break;
            }
        }
        crate::sched_global::set_current_gid_raw(prev);
        self.finished.fetch_add(1, Ordering::Relaxed);
        gid
    }

    /// Runs `task` to completion immediately and returns its gid.
    pub fn spawn<T: SchedTask + 'static>(&self, task: T) -> Gid {
        self.run_to_completion(task)
    }

    /// Runs `task` to completion immediately; never refuses (there is
    /// no live-goroutine cap without a worker pool).
    pub fn try_spawn<T: SchedTask + 'static>(&self, task: T) -> Option<Gid> {
        Some(self.run_to_completion(task))
    }

    /// No-op: nothing is ever parked on the single-threaded runtime.
    pub fn unpark(&self, _gid: Gid) -> bool {
        false
    }

    /// No-op on the single-threaded runtime - there is one cooperative
    /// worker (the calling stack). Kept for API parity with native.
    pub fn set_worker_count(&self, _n: usize) {}

    /// Accepts and echoes the requested cap. The single-threaded
    /// runtime never enforces it (goroutines run to completion
    /// immediately), so the previous value is reported as the new one.
    pub fn set_max_goroutines(&self, n: usize) -> usize {
        n
    }

    /// Snapshot of the scheduler counters.
    #[must_use]
    pub fn stats(&self) -> MultiStats {
        MultiStats {
            spawned: self.spawned.load(Ordering::Relaxed),
            finished: self.finished.load(Ordering::Relaxed),
            steps: self.steps.load(Ordering::Relaxed),
            ..MultiStats::default()
        }
    }

    /// Live goroutine count - always 0 once `spawn` returns, since the
    /// body has already run to completion.
    #[must_use]
    pub fn live_goroutines(&self) -> usize {
        0
    }
}
