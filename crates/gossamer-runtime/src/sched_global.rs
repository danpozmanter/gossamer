//! Process-wide M:N scheduler + netpoller singleton owned by the
//! runtime crate.
//!
//! Every compiled Gossamer binary links `gossamer-runtime` as a
//! `staticlib`, so the singleton ships with the program automatically
//! — no extra registration step is required from user code.
//!
//! Boot ordering:
//!
//! 1. The first call to [`scheduler`] / [`with_poller`] / [`add_timer`]
//!    constructs the [`MultiScheduler`] sized at `runtime::max_procs()`
//!    and an `OsPoller` (mio epoll/kqueue/IOCP).
//! 2. A dedicated `gos-netpoller` OS thread starts and, in a tight
//!    loop, blocks on `OsPoller::poll`, then dispatches every
//!    delivered `Readiness` by `unpark`-ing the goroutine that
//!    registered for that event.
//! 3. Compiled `go fn(args)` lands here through [`spawn`], which
//!    constructs a real [`gossamer_coro::Goroutine`] (stackful
//!    coroutine) and pushes it onto the work-stealing pool.
//!
//! Goroutines are stackful coroutines. When user code blocks on a
//! channel, mutex, sleep, or socket, [`park`] suspends the
//! coroutine — the worker thread immediately picks up the next
//! runnable goroutine instead of being held hostage by the OS-level
//! block. The wakeup source (poller, channel queue, mutex release)
//! calls [`MultiScheduler::unpark`] when ready.

use std::io;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::sched::{Gid, MultiScheduler, OsPoller, ParkReason, Poller, Readiness, Step};

/// Wakers registered with the global poller. Keyed by [`Gid`], the
/// closure invoked when the poller delivers a readiness for that gid.
type WakerMap = std::collections::HashMap<Gid, Box<dyn Fn() + Send + Sync>>;

struct Globals {
    scheduler: MultiScheduler,
    poller: Mutex<OsPoller>,
    /// Shared interrupt handle for the netpoller thread. Held
    /// outside the `poller` mutex so registration paths can wake
    /// the running `poll()` without contending for the lock. Set
    /// once during `globals()` init; never replaced.
    poller_interrupt: std::sync::Arc<mio::Waker>,
    wakers: Mutex<WakerMap>,
    /// Monotonic gid allocator handed out for park/unpark purposes
    /// outside of `MultiScheduler::spawn` (timers, signal handlers,
    /// blocking thread pool callbacks).
    gid_alloc: AtomicU64,
    /// Set after the poller thread has been started.
    poller_started: AtomicBool,
    /// Set by [`request_shutdown`] to ask the poller loop to exit
    /// cleanly so the runtime can flush in-flight I/O before
    /// `gos_rt_exit`. Without this signal the poller thread was
    /// killed mid-`poll()` by `std::process::exit`, sending RST
    /// (not FIN) on any TCP connections in the OS kernel's send
    /// buffer.
    poller_shutdown: AtomicBool,
}

static GLOBALS: OnceLock<Globals> = OnceLock::new();

fn globals() -> &'static Globals {
    GLOBALS.get_or_init(|| {
        let workers = default_workers();
        let scheduler = MultiScheduler::new(workers);
        let os_poller = OsPoller::new().expect("OsPoller::new");
        let poller_interrupt = os_poller.interrupt_handle();
        let poller = Mutex::new(os_poller);
        scheduler.start();
        Globals {
            scheduler,
            poller,
            poller_interrupt,
            wakers: Mutex::new(WakerMap::new()),
            gid_alloc: AtomicU64::new(1_000_000),
            poller_started: AtomicBool::new(false),
            poller_shutdown: AtomicBool::new(false),
        }
    })
}

fn default_workers() -> usize {
    if let Ok(s) = std::env::var("GOSSAMER_MAX_PROCS") {
        if let Ok(n) = s.parse::<usize>() {
            if n > 0 {
                return n;
            }
        }
    }
    thread::available_parallelism().map_or(1, std::num::NonZero::get)
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

/// Short poll cycle for the netpoller. Bounds the worst-case
/// time a registering goroutine waits for `g.poller.lock()`:
/// any registration arriving while the netpoller is mid-syscall
/// waits at most `POLL_TICK_MS` ms before the mutex unlocks.
/// The mio waker (fired by [`with_poller`]) is the first line
/// of defence — most cycles end instantly when a registration
/// fires it — but the ceiling keeps idle CPU bounded if the
/// waker mechanism is ever broken or bypassed.
const POLL_TICK_MS: u64 = 1;

fn poller_loop() {
    let g = globals();
    loop {
        if g.poller_shutdown.load(Ordering::Acquire) {
            break;
        }
        let events = {
            let mut poller = g.poller.lock();
            poller
                .poll(Some(Duration::from_millis(POLL_TICK_MS)))
                .unwrap_or_default()
        };
        for ev in events {
            deliver_event(ev);
        }
    }
}

/// Signals the netpoller thread to exit on its next tick. Called
/// from `gos_rt_exit` so in-flight I/O drains cleanly instead of
/// being interrupted mid-syscall by `std::process::exit`. Idempotent.
pub fn request_shutdown() {
    // The observable flag is a bare process global: setting it must
    // not depend on (or boot) the runtime — `gos_rt_exit` runs on the
    // exit path of every program and booting a worker pool there just
    // to flag it down cost ~150 ms per short-lived process.
    SHUTDOWN_REQUESTED.store(true, Ordering::Release);
    // Only an already-booted runtime has a poller to wake.
    let Some(g) = GLOBALS.get() else { return };
    g.poller_shutdown.store(true, Ordering::Release);
    // Wake the poller so it doesn't sit on a 1ms tick before
    // observing the flag.
    let _ = g.poller_interrupt.wake();
}

/// Process-wide shutdown flag, independent of runtime boot state so
/// `request_shutdown` / `is_shutdown_requested` keep their contract
/// for programs that never started the scheduler.
static SHUTDOWN_REQUESTED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Blocks until every goroutine spawned via `go` has finished, bounded
/// at five seconds so a permanently blocked goroutine cannot wedge
/// process exit. No-op — and crucially, does not boot the runtime —
/// when nothing ever started the scheduler: a program with no
/// concurrency has nothing to drain, and booting a worker pool on the
/// exit path just to observe it idle cost ~150 ms per process.
pub fn drain_goroutines_for_exit() {
    if let Some(g) = GLOBALS.get() {
        let _ = g
            .scheduler
            .wait_quiescent(std::time::Duration::from_secs(5));
    }
}

/// True if [`request_shutdown`] has been called. Long-running
/// runtime loops (HTTP accept, M:N worker idle, …) poll this
/// flag at safepoints so `gos_rt_exit` doesn't interrupt them
/// mid-iteration.
#[must_use]
pub fn is_shutdown_requested() -> bool {
    SHUTDOWN_REQUESTED.load(Ordering::Acquire)
}

/// Wakes the netpoller thread early so it re-enters `poll()` with
/// up-to-date registrations and timer entries. Called by paths
/// that just inserted into the poller (registration, timer add)
/// to avoid waiting up to one poll cycle for the change to take
/// effect. Cheap and idempotent — safe to call from any thread.
pub fn wake_poller() {
    let _ = globals().poller_interrupt.wake();
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
/// handle (timers, blocking pool callbacks, I/O readiness). These ids
/// do not correspond to user-spawned goroutines.
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

/// Borrows the netpoller for a closure. Used by the I/O bridge code
/// in `gossamer-std::net` and the runtime's own HTTP plumbing.
pub fn with_poller<R>(f: impl FnOnce(&mut OsPoller) -> R) -> R {
    let _ = scheduler();
    let g = globals();
    let result = {
        let mut poller = g.poller.lock();
        f(&mut poller)
    };
    // The closure may have registered a new I/O source or
    // dropped a timer in the wheel; wake the netpoller thread so
    // it re-polls with the up-to-date state instead of finishing
    // out its current 1-second poll cycle.
    let _ = g.poller_interrupt.wake();
    result
}

/// Convenience wrapper around `Poller::poll(0)` that returns any
/// already-ready events without blocking.
///
/// # Errors
///
/// Returns `io::Error` when the poller's underlying `epoll`/`kqueue`
/// rejects the call.
pub fn drain_ready() -> io::Result<Vec<Readiness>> {
    globals().poller.lock().poll(Some(Duration::ZERO))
}

// ---------------------------------------------------------------
// Goroutine plumbing
// ---------------------------------------------------------------

thread_local! {
    /// Gid of the goroutine currently running on this OS worker
    /// thread, biased by `+1` so that `0` reliably means
    /// "no goroutine on this thread" (a goroutine with raw gid 0
    /// is a real value the scheduler hands out and must be
    /// distinguishable from the unset sentinel).
    /// Set by the worker loop immediately before each resume,
    /// cleared after.
    static CURRENT_GID: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Returns the gid of the goroutine currently running on this OS
/// thread, or `None` if the calling thread is not a scheduler worker
/// driving a goroutine.
#[must_use]
pub fn current_gid() -> Option<Gid> {
    let raw = CURRENT_GID.with(std::cell::Cell::get);
    if raw == 0 { None } else { Some(Gid(raw - 1)) }
}

pub(crate) fn set_current_gid(gid: Gid) {
    CURRENT_GID.with(|c| c.set(gid.as_u32().wrapping_add(1)));
}

pub(crate) fn clear_current_gid() {
    CURRENT_GID.with(|c| c.set(0));
}

/// Parker handle handed to the closure in [`park`]. Carries the gid
/// of the goroutine that is about to suspend; the closure is expected
/// to register the gid with whatever wakeup source it wants to wait
/// on (a poller waker, a channel parked-receivers list, a mutex
/// queue) so that source can later call
/// [`MultiScheduler::unpark`].
#[derive(Debug, Clone, Copy)]
pub struct Parker {
    /// Identifier of the goroutine being parked. Pass to
    /// [`MultiScheduler::unpark`] when the wait is satisfied.
    pub gid: Gid,
    /// Reason the goroutine is parking. Carried alongside for
    /// diagnostics; the scheduler uses it to populate goroutine
    /// state in SIGQUIT dumps.
    pub reason: ParkReason,
}

/// Suspends the calling goroutine after invoking `arm` to register
/// the wakeup source. The `arm` callback runs *before* the suspend
/// so the wakeup source already knows the gid when this function
/// transfers control back to the scheduler.
///
/// Wakeup race window: if the wake fires *between* `arm()`
/// returning and the worker loop moving this task into the parked
/// map, the scheduler's `unpark(gid)` can't find the gid in
/// `parked` yet. The scheduler handles this with a side
/// `pre_unpark` set — the worker checks it just after parking
/// and immediately re-ejects the task if its gid is in `pre_unpark`.
///
/// # Panics
///
/// Panics if the calling thread is not currently driving a
/// goroutine. Stdlib code that may be invoked from non-goroutine
/// contexts must check [`gossamer_coro::in_goroutine`] and fall
/// back to OS-thread blocking when off.
pub fn park(reason: ParkReason, arm: impl FnOnce(&Parker)) {
    let gid = current_gid().expect("park called outside a goroutine");
    let parker = Parker { gid, reason };
    arm(&parker);
    // Publish the park request to the worker M, which reads it
    // after the coroutine suspends.
    PENDING_PARK.with(|cell| cell.set(Some((gid, reason))));
    gossamer_coro::suspend();
}

thread_local! {
    /// Set by [`park`] just before suspending; read-and-cleared by
    /// the worker M's `Step::Yield` handler. When `Some`, the
    /// scheduler moves the task into the parked map keyed by gid
    /// instead of re-enqueueing onto the local deque.
    static PENDING_PARK: std::cell::Cell<Option<(Gid, ParkReason)>> =
        const { std::cell::Cell::new(None) };
}

/// Returns the most recent `(gid, reason)` published by [`park`],
/// reads-and-clears.
pub(crate) fn take_pending_park() -> Option<(Gid, ParkReason)> {
    PENDING_PARK.with(std::cell::Cell::take)
}

/// Suspends the current goroutine on `io`'s readiness for the given
/// `interest`. Wires the netpoller registration, the waker, the
/// park, and the cleanup into one call.
///
/// Falls back to a brief OS-thread sleep when called outside a
/// goroutine context (e.g. from tooling code that hits the same
/// helper). Real goroutine code should never trigger that path.
///
/// # Errors
///
/// Returns the underlying `io::Error` if mio refuses the
/// registration (e.g. file descriptor closed).
pub fn wait_io<S: mio::event::Source + ?Sized>(
    io: &mut S,
    interest: crate::sched::Interest,
) -> io::Result<()> {
    if !gossamer_coro::in_goroutine() {
        std::thread::sleep(Duration::from_millis(1));
        return Ok(());
    }
    let mut result: io::Result<()> = Ok(());
    park(ParkReason::Io, |parker| {
        let gid = parker.gid;
        register_waker(
            gid,
            Box::new(move || {
                scheduler().unpark(gid);
            }),
        );
        if let Err(e) = with_poller(|p| p.register_io(io, interest, gid)).map(|_| ()) {
            result = Err(e);
        }
    });
    if let Some(gid) = current_gid() {
        forget_waker(gid);
    }
    result
}

/// Suspends the current goroutine until `deadline` by registering a
/// one-shot timer with the netpoller. Falls back to
/// [`thread::sleep`] when called outside a goroutine context (e.g.
/// from synchronous tooling code).
pub fn sleep_until(deadline: Instant) {
    let now = Instant::now();
    if deadline <= now {
        return;
    }
    if !gossamer_coro::in_goroutine() {
        // No goroutine to park — fall back to OS-thread sleep.
        std::thread::sleep(deadline - now);
        return;
    }
    park(ParkReason::Timer, |parker| {
        let gid = parker.gid;
        register_waker(
            gid,
            Box::new(move || {
                scheduler().unpark(gid);
            }),
        );
        with_poller(|p| p.add_timer(deadline, gid));
    });
    // Cleanup on resume — the waker entry was consumed by
    // `deliver_event`, but if the wait timed out before delivery
    // (poll loop's 50 ms tick), the waker may still be registered.
    if let Some(gid) = current_gid() {
        forget_waker(gid);
    }
}

/// Spawns `task` on the M:N pool. Returns `None` when the
/// scheduler's live-goroutine cap would be exceeded; the caller
/// should surface the refusal to user code instead of silently
/// overcommitting kernel resources.
#[must_use]
pub fn try_spawn(task: Box<dyn FnOnce() + Send + 'static>) -> Option<Gid> {
    let coro = gossamer_coro::Goroutine::new(task);
    scheduler().try_spawn(GoroutineTask { coro })
}

/// Spawns `task` on the M:N pool. Panics if the live-goroutine cap
/// would be exceeded. Use [`try_spawn`] for graceful refusal.
///
/// The returned [`Gid`] is informational; fire-and-forget is the
/// common shape, so the result is intentionally not `#[must_use]`.
#[allow(
    clippy::must_use_candidate,
    reason = "fire-and-forget spawn is the common shape; Gid is informational"
)]
pub fn spawn(task: Box<dyn FnOnce() + Send + 'static>) -> Gid {
    let coro = gossamer_coro::Goroutine::new(task);
    scheduler().spawn(GoroutineTask { coro })
}

/// Adapts a [`gossamer_coro::Goroutine`] into the scheduler's
/// [`crate::sched::Task`] trait. Each `step()` call resumes the
/// coroutine; if the coroutine completes, returns [`Step::Done`].
/// If the coroutine called [`gossamer_coro::suspend`], returns
/// [`Step::Yield`] — the worker loop further consults
/// [`take_pending_park`] to decide whether to re-enqueue the task
/// or move it to the parked map.
struct GoroutineTask {
    coro: gossamer_coro::Goroutine,
}

impl crate::sched::Task for GoroutineTask {
    fn step(&mut self) -> Step {
        // The closure inside the coroutine's first `resume()` sets
        // the worker's TLS yielder. Subsequent steps need the
        // worker to re-set it from the slot the closure published.
        let yielder_ptr = self.coro.yielder_ptr();
        if !yielder_ptr.is_null() {
            gossamer_coro::set_current_yielder(yielder_ptr);
        }
        let done = self.coro.resume();
        gossamer_coro::clear_current_yielder();
        if done { Step::Done } else { Step::Yield }
    }
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

    #[test]
    fn spawn_runs_on_pool() {
        use std::sync::atomic::AtomicUsize;
        let counter = std::sync::Arc::new(AtomicUsize::new(0));
        let c2 = std::sync::Arc::clone(&counter);
        let _ = spawn(Box::new(move || {
            c2.fetch_add(7, Ordering::Relaxed);
        }));
        for _ in 0..200 {
            if counter.load(Ordering::Relaxed) == 7 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("spawned closure did not run within deadline");
    }

    #[test]
    fn goroutine_can_sleep_via_park() {
        use std::sync::atomic::AtomicUsize;
        let counter = std::sync::Arc::new(AtomicUsize::new(0));
        let c2 = std::sync::Arc::clone(&counter);
        let start = Instant::now();
        let _ = spawn(Box::new(move || {
            sleep_until(Instant::now() + Duration::from_millis(20));
            c2.fetch_add(1, Ordering::Relaxed);
        }));
        for _ in 0..200 {
            if counter.load(Ordering::Relaxed) == 1 {
                let elapsed = start.elapsed();
                assert!(elapsed >= Duration::from_millis(15));
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("goroutine sleep did not return within deadline");
    }
}
