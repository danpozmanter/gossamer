//! Allocation ledger: per-family live-object counters, printed at process exit
//! when `GOS_LEAK_LEDGER` is set. Deterministic leak detection — a family whose
//! live count grows with the workload size N (instead of staying O(1)) is
//! leaking. Used to lock leak targets and prove fixes (see
//! `~/dev/contexts/gos/leaks.md`).
//!
//! The counters are `Relaxed` atomics (cheap); the at-exit hook is armed once.

use std::sync::atomic::{AtomicI64, Ordering};

pub static AGGR_LIVE: AtomicI64 = AtomicI64::new(0);
pub static RC_LIVE: AtomicI64 = AtomicI64::new(0);
pub static STR_LIVE: AtomicI64 = AtomicI64::new(0);
pub static VEC_LIVE: AtomicI64 = AtomicI64::new(0);
pub static MAP_LIVE: AtomicI64 = AtomicI64::new(0);

#[cfg(all(unix, not(miri)))]
static ARMED: std::sync::Once = std::sync::Once::new();

#[cfg(all(unix, not(miri)))]
extern "C" fn report() {
    if std::env::var("GOS_LEAK_LEDGER").is_ok() {
        eprintln!(
            "LEAK LEDGER (live at exit): aggr={} rc={} str={} vec={} map={}",
            AGGR_LIVE.load(Ordering::SeqCst),
            RC_LIVE.load(Ordering::SeqCst),
            STR_LIVE.load(Ordering::SeqCst),
            VEC_LIVE.load(Ordering::SeqCst),
            MAP_LIVE.load(Ordering::SeqCst),
        );
    }
}

// At-exit auto-print of the ledger is unix-only (`libc::atexit`) and is
// additionally skipped under Miri, which cannot execute `atexit` (and runs
// with `-Zmiri-ignore-leaks`, so the report is moot there anyway). On other
// targets the counters still tally but the report is read via a debugger /
// explicit query rather than printed at exit. This keeps the Windows build —
// where `libc` is not a dependency — and the Miri job compiling and running.
#[inline]
fn arm() {
    #[cfg(all(unix, not(miri)))]
    ARMED.call_once(|| unsafe {
        libc::atexit(report);
    });
}

#[inline]
pub fn aggr_inc() {
    arm();
    AGGR_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn aggr_dec() {
    AGGR_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn rc_inc() {
    arm();
    RC_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn rc_dec() {
    RC_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn str_inc() {
    arm();
    STR_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn str_dec() {
    STR_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn vec_inc() {
    arm();
    VEC_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn vec_dec() {
    VEC_LIVE.fetch_sub(1, Ordering::Relaxed);
}
#[inline]
pub fn map_inc() {
    arm();
    MAP_LIVE.fetch_add(1, Ordering::Relaxed);
}
#[inline]
pub fn map_dec() {
    MAP_LIVE.fetch_sub(1, Ordering::Relaxed);
}
