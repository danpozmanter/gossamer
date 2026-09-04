//! The wall clock a program reads, and the test control over it.
//!
//! `time::now_ms` and everything built on it answer the real clock until a
//! test freezes one. A frozen clock is process-global because a program has
//! one notion of "now", and it is the same state on every tier: the
//! bytecode VM's builtins and the compiled tiers' shims both read it here.
//!
//! Only the wall clock is affected. The monotonic clock and `sleep` are
//! not: this pins what the program is told the time is, not how long
//! anything takes.

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::time::UNIX_EPOCH;

/// Whether the wall clock is pinned.
static FROZEN: AtomicBool = AtomicBool::new(false);

/// The reading a frozen clock answers, in milliseconds since the epoch.
static FROZEN_MS: AtomicI64 = AtomicI64::new(0);

/// The real wall clock in milliseconds since the epoch.
fn real_ms() -> i64 {
    crate::platform::system_time_now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

/// The wall clock a program reads: the frozen reading when one is pinned,
/// the real clock otherwise.
#[must_use]
pub fn wall_ms() -> i64 {
    if FROZEN.load(Ordering::Acquire) {
        FROZEN_MS.load(Ordering::Acquire)
    } else {
        real_ms()
    }
}

/// The wall clock in nanoseconds, from the same source as [`wall_ms`].
#[must_use]
pub fn wall_nanos() -> i64 {
    if FROZEN.load(Ordering::Acquire) {
        FROZEN_MS.load(Ordering::Acquire).saturating_mul(1_000_000)
    } else {
        crate::platform::system_time_now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos() as i64)
    }
}

/// Pins the wall clock at `ms` since the epoch.
pub fn freeze(ms: i64) {
    FROZEN_MS.store(ms, Ordering::Release);
    FROZEN.store(true, Ordering::Release);
}

/// Moves a frozen clock forward by `ms` and answers the new reading.
/// Freezes at the current reading first when it is not already frozen.
pub fn advance(ms: i64) -> i64 {
    if !FROZEN.load(Ordering::Acquire) {
        freeze(real_ms());
    }
    let now = FROZEN_MS.fetch_add(ms, Ordering::AcqRel);
    now.saturating_add(ms)
}

/// Returns to the real wall clock.
pub fn unfreeze() {
    FROZEN.store(false, Ordering::Release);
}

/// Whether the wall clock is pinned.
#[must_use]
pub fn is_frozen() -> bool {
    FROZEN.load(Ordering::Acquire)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn advancing_a_frozen_clock_moves_only_the_wall_reading() {
        freeze(1_000);
        assert_eq!(wall_ms(), 1_000);
        assert_eq!(advance(500), 1_500);
        assert_eq!(wall_ms(), 1_500);
        unfreeze();
        assert!(!is_frozen());
        // Back on the real clock, which is far past a thousand milliseconds
        // after the epoch.
        assert!(wall_ms() > 1_500);
    }
}
