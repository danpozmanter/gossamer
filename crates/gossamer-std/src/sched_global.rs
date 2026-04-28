//! Process-wide scheduler + netpoller singleton.
//!
//! The Gossamer runtime owns one [`MultiScheduler`] for the entire
//! process and one [`OsPoller`] thread that delivers OS-level
//! readiness events back into the scheduler. Stdlib code (timers,
//! networking, mutexes, signals) routes through the helpers in this
//! module so park / unpark goes through a single source of truth.
//!
//! This module exists in `gossamer-std` rather than `gossamer-sched`
//! because `gossamer-sched` wants to stay free of stdlib dependencies
//! and the singleton needs to host a long-running poller thread that
//! is best initialised lazily on first stdlib access.

#![forbid(unsafe_code)]

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use gossamer_sched::{Gid, MultiScheduler, OsPoller, Poller, Readiness, Step};
use parking_lot::Mutex;

use crate::runtime;

/// Wakers registered with the global poller. Keyed by [`Gid`], the
/// closure invoked when the poller delivers a readiness for that gid.
type WakerMap = std::collections::HashMap<Gid, Box<dyn Fn() + Send + Sync>>;

struct Globals {
    scheduler: MultiScheduler,
    poller: Mutex<OsPoller>,
    wakers: Mutex<WakerMap>,
    /// Monotonic gid allocator handed out for park/unpark purposes
    /// outside of `MultiScheduler::spawn` (timers, signal handlers,
    /// blocking thread pool callbacks).
    gid_alloc: AtomicU64,
    /// Set after the poller thread has been started.
    poller_started: AtomicBool,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

fn globals() -> &'static Globals {
    GLOBALS.get_or_init(|| {
        let workers = runtime::max_procs();
        let scheduler = MultiScheduler::new(workers);
        let poller = Mutex::new(OsPoller::new().expect("OsPoller::new"));
        let g = Globals {
            scheduler,
            poller,
            wakers: Mutex::new(WakerMap::new()),
            gid_alloc: AtomicU64::new(1_000_000),
            poller_started: AtomicBool::new(false),
        };
        // Reserve all G ids ≥ 1_000_000 for the runtime's
        // bookkeeping. Application gids come from MultiScheduler.
        gossamer_runtime::c_abi::set_spawn_handler(spawn_via_scheduler);
        g
    })
}

/// Installed into `gossamer_runtime` so the C-ABI spawn helpers
/// route compiled `go fn(args)` calls onto the work-stealing
/// pool instead of fanning out to bare `std::thread::spawn`. The
/// `FnOnce` is wrapped in a one-shot closure that runs once and
/// reports `Step::Done` back to the scheduler.
fn spawn_via_scheduler(task: Box<dyn FnOnce() + Send + 'static>) {
    let mut once = Some(task);
    globals().scheduler.spawn(move || {
        if let Some(f) = once.take() {
            f();
        }
        Step::Done
    });
}

/// Returns a handle to the process-wide scheduler. The first caller
/// boots both the scheduler and the poller thread.
#[must_use]
pub fn scheduler() -> &'static MultiScheduler {
    let g = globals();
    ensure_poller_thread(g);
    &g.scheduler
}

fn ensure_poller_thread(g: &'static Globals) {
    if g.poller_started.swap(true, Ordering::AcqRel) {
        return;
    }
    thread::Builder::new()
        .name("gos-netpoller".to_string())
        .spawn(poller_loop)
        .expect("spawn netpoller thread");
}

fn poller_loop() {
    let g = globals();
    loop {
        let events = {
            let mut poller = g.poller.lock();
            poller
                .poll(Some(Duration::from_millis(50)))
                .unwrap_or_default()
        };
        for ev in events {
            deliver_event(ev);
        }
    }
}

fn deliver_event(ev: Readiness) {
    let waker = globals().wakers.lock().remove(&ev.gid);
    if let Some(w) = waker {
        w();
    } else {
        // No waker registered — likely an unparked-then-resubscribed
        // race. Falling back to a direct unpark gives the goroutine
        // a chance to re-arm itself.
        globals().scheduler.unpark(ev.gid);
    }
}

/// Allocates a fresh [`Gid`] for use as a runtime-internal wait
/// handle (timers, blocking pool callbacks). These ids do not
/// correspond to user-spawned goroutines.
#[must_use]
pub fn alloc_runtime_gid() -> Gid {
    let raw = globals().gid_alloc.fetch_add(1, Ordering::Relaxed);
    Gid(u32::try_from(raw & 0xFFFF_FFFF).unwrap_or(u32::MAX))
}

/// Registers `waker` to be invoked when the poller delivers the
/// next event tagged with `gid`.
pub fn register_waker(gid: Gid, waker: Box<dyn Fn() + Send + Sync>) {
    globals().wakers.lock().insert(gid, waker);
}

/// Removes any waker associated with `gid`.
pub fn forget_waker(gid: Gid) {
    globals().wakers.lock().remove(&gid);
}

/// Adds a one-shot timer firing at `deadline`. Returns the [`Gid`]
/// the caller passes to [`register_waker`].
#[must_use]
pub fn add_timer(deadline: Instant) -> Gid {
    let gid = alloc_runtime_gid();
    let _ = scheduler();
    let g = globals();
    g.poller.lock().add_timer(deadline, gid);
    gid
}

/// Sleeps the calling OS thread until `deadline` using the netpoller's
/// internal timer wheel + a thread parking primitive. This is the
/// blocking path used by [`crate::time::sleep`] when no goroutine
/// context is in scope (e.g. from synchronous user code).
pub fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline <= now {
        return;
    }
    // Park the current OS thread on a parking_lot::Condvar that the
    // netpoller signals when the timer fires.
    let pair = std::sync::Arc::new((Mutex::new(false), parking_lot::Condvar::new()));
    let pair2 = std::sync::Arc::clone(&pair);
    let gid = add_timer(deadline);
    register_waker(
        gid,
        Box::new(move || {
            let (mu, cv) = &*pair2;
            let mut done = mu.lock();
            *done = true;
            cv.notify_all();
        }),
    );
    let (mu, cv) = &*pair;
    let mut done = mu.lock();
    while !*done {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        let _ = cv.wait_for(&mut done, remaining);
    }
    forget_waker(gid);
}

/// Borrows the netpoller for a closure. Used by the I/O bridge code
/// in `net.rs`.
pub fn with_poller<R>(f: impl FnOnce(&mut OsPoller) -> R) -> R {
    let _ = scheduler();
    let mut poller = globals().poller.lock();
    f(&mut poller)
}

/// Convenience wrapper around `Poller::poll(0)` that returns any
/// already-ready events without blocking. Useful in tests.
pub fn drain_ready() -> io::Result<Vec<Readiness>> {
    globals().poller.lock().poll(Some(Duration::ZERO))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sleep_until_returns_promptly() {
        let start = Instant::now();
        sleep_until(start + Duration::from_millis(20));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_millis(15));
        assert!(elapsed < Duration::from_millis(500));
    }
}
