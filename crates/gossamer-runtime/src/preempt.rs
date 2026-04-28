//! Async preemption + GC safepoint plumbing.
//!
//! This module owns the mechanism that lets the scheduler / GC
//! interrupt a CPU-bound goroutine so other goroutines can run and
//! the collector can mark the world. Two cooperating pieces:
//!
//! 1. A global atomic *preempt phase* counter. Application code
//!    polls [`should_yield`] at safepoints (function entry, loop
//!    back-edges, allocation sites). If it returns `true`, the
//!    caller jumps to its yield handler — interpreter calls into
//!    the scheduler, compiled code calls [`gos_rt_preempt_check`].
//!
//! 2. A real OS signal (`SIGURG` on Unix; a thread-targeted APC on
//!    Windows in a future iteration) installed by [`init`]. When the
//!    scheduler watchdog decides a worker has been running too long,
//!    it raises the signal at that worker's thread; the handler
//!    flips the atomic and the next safepoint poll observes it.
//!
//! The signal handler itself does only async-signal-safe work
//! (atomic store) — no allocations, no locks.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicUsize, Ordering};

/// Global counter incremented every time the scheduler asks all
/// goroutines to reach a safepoint (start of a GC cycle, set-max-procs
/// reduction, etc.). Application code compares its own
/// thread-local copy and yields if the global moved.
static GLOBAL_PHASE: AtomicU64 = AtomicU64::new(0);

// Per-thread "yield requested" flag set by the SIGURG handler.
// Stored thread-locally so the safepoint poll is a single relaxed
// load with no cache-line contention.
thread_local! {
    static LOCAL_YIELD: AtomicBool = const { AtomicBool::new(false) };
    static LOCAL_PHASE: AtomicU64 = const { AtomicU64::new(0) };
}

/// Number of cooperative yields recorded — exposed for tests.
static YIELD_COUNT: AtomicUsize = AtomicUsize::new(0);

/// Initialises the SIGURG handler. Idempotent.
pub fn init() {
    install_signal_handler();
}

#[cfg(unix)]
fn install_signal_handler() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let flag = global_flag();
        if signal_hook::flag::register(signal_hook::consts::SIGURG, std::sync::Arc::clone(&flag))
            .is_ok()
        {
            std::thread::Builder::new()
                .name("gos-preempt-relay".to_string())
                .spawn(relay_loop)
                .ok();
        }
    });
}

#[cfg(not(unix))]
fn install_signal_handler() {
    // Windows: APC-based preemption is a future iteration. The
    // cooperative-only path still works.
}

#[cfg(unix)]
static GLOBAL_PREEMPT_FLAG_INNER: std::sync::OnceLock<std::sync::Arc<AtomicBool>> =
    std::sync::OnceLock::new();

#[cfg(unix)]
fn global_flag() -> std::sync::Arc<AtomicBool> {
    std::sync::Arc::clone(
        GLOBAL_PREEMPT_FLAG_INNER.get_or_init(|| std::sync::Arc::new(AtomicBool::new(false))),
    )
}

#[cfg(unix)]
fn relay_loop() {
    let flag = global_flag();
    loop {
        if flag.swap(false, Ordering::AcqRel) {
            request_yield_all();
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

#[cfg(not(unix))]
fn relay_loop() {}

/// Signals every active worker to reach a safepoint. The actual
/// per-thread flag is consulted by [`should_yield`] from the
/// generated code. Increments the global phase counter so threads
/// without per-thread state can also notice.
pub fn request_yield_all() {
    GLOBAL_PHASE.fetch_add(1, Ordering::AcqRel);
}

/// Asks the calling thread to reach a safepoint at its next
/// opportunity. Hook used by the scheduler watchdog when it sends
/// SIGURG to a specific worker's thread.
pub fn request_yield_self() {
    LOCAL_YIELD.with(|f| f.store(true, Ordering::Release));
    GLOBAL_PHASE.fetch_add(1, Ordering::AcqRel);
}

/// Returns `true` when the calling thread should yield at the next
/// safepoint. Cheap fast path: a single relaxed load + comparison.
#[inline]
pub fn should_yield() -> bool {
    let global = GLOBAL_PHASE.load(Ordering::Relaxed);
    let local_phase = LOCAL_PHASE.with(|p| p.load(Ordering::Relaxed));
    if global != local_phase {
        LOCAL_PHASE.with(|p| p.store(global, Ordering::Relaxed));
        return true;
    }
    LOCAL_YIELD.with(|f| f.swap(false, Ordering::Acquire))
}

/// Total cooperative yields recorded — for tests / diagnostics.
#[must_use]
pub fn yields_observed() -> usize {
    YIELD_COUNT.load(Ordering::Relaxed)
}

/// Records a successful cooperative yield. Called by code that
/// honours [`should_yield`] and actually returns control to the
/// scheduler.
pub fn note_yield() {
    YIELD_COUNT.fetch_add(1, Ordering::Relaxed);
}

/// Number of phase changes seen by [`should_yield`] — diagnostic.
#[must_use]
pub fn current_phase() -> u64 {
    GLOBAL_PHASE.load(Ordering::Relaxed)
}

/// C-ABI safepoint poll. Compiled code emits a call to this at each
/// loop back-edge / function entry. Returns `1` if the goroutine
/// should yield, `0` otherwise. Kept as a non-mangled `extern "C"`
/// so the LLVM lowerer can call it by name.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_preempt_check() -> i32 {
    if should_yield() {
        note_yield();
        1
    } else {
        0
    }
}

/// Watchdog tick used by the scheduler. Returns the number of
/// outstanding `request_yield_*` calls a worker has not yet honoured.
/// Useful for diagnostics; the value is best-effort because workers
/// race the watchdog.
#[must_use]
pub fn pending_yield_pressure() -> u32 {
    PENDING_PRESSURE.load(Ordering::Relaxed)
}

static PENDING_PRESSURE: AtomicU32 = AtomicU32::new(0);

/// Bumps the pending-pressure counter; called by the scheduler when
/// it raises a SIGURG against a worker.
pub fn bump_pressure() {
    PENDING_PRESSURE.fetch_add(1, Ordering::Relaxed);
}

/// Decrements pending pressure. Called by the safepoint handler when
/// the yield is honoured.
pub fn drop_pressure() {
    PENDING_PRESSURE
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |v| {
            if v > 0 { Some(v - 1) } else { None }
        })
        .ok();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn yield_request_sticks_until_polled() {
        // Reset baseline.
        let baseline = current_phase();
        request_yield_all();
        assert!(current_phase() > baseline);
        // First should_yield observes the phase change.
        assert!(should_yield());
        // Second one returns false because the local phase caught up.
        assert!(!should_yield());
    }

    #[test]
    fn yield_self_flips_local_flag() {
        let _ = should_yield();
        request_yield_self();
        assert!(should_yield());
    }

    #[test]
    fn pressure_counter_round_trips() {
        let baseline = pending_yield_pressure();
        bump_pressure();
        bump_pressure();
        assert_eq!(pending_yield_pressure(), baseline + 2);
        drop_pressure();
        drop_pressure();
        assert_eq!(pending_yield_pressure(), baseline);
    }
}
