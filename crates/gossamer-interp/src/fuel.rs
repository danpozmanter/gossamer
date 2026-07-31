//! Execution budget for the bytecode VM.
//!
//! Compiled in only under the `fuel` feature (the wasm playground enables
//! it). Native `gos` builds without the feature, so the dispatch loop
//! carries none of this - no per-instruction tracking, no budget check.
//!
//! The budget counts loop back-edges (one per loop iteration). A thread-local
//! holds it so every goroutine running on this thread shares one budget; an
//! unbounded loop drains it and the VM aborts with
//! [`crate::value::RuntimeError::FuelExhausted`] instead of hanging the tab.

use std::cell::Cell;

thread_local! {
    static FUEL: Cell<u64> = const { Cell::new(u64::MAX) };
}

/// Sets the per-thread execution budget (loop iterations allowed before the
/// VM aborts). `u64::MAX` is effectively unlimited.
pub fn set_fuel(budget: u64) {
    FUEL.with(|f| f.set(budget));
}

/// Remaining budget, for reporting how much was consumed.
#[must_use]
pub fn fuel_remaining() -> u64 {
    FUEL.with(Cell::get)
}

/// Consumes one unit. Returns `true` when the budget is exhausted, so the
/// dispatch loop aborts.
#[inline]
pub(crate) fn consume() -> bool {
    FUEL.with(|f| {
        let v = f.get();
        if v == 0 {
            return true;
        }
        f.set(v - 1);
        false
    })
}
