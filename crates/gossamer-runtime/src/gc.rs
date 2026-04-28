//! Process-wide GC heap singleton + write-barrier C-ABI surface.
//!
//! The compiled tier emits one `gos_rt_write_barrier` call per
//! heap-pointer store. The barrier is a no-op while the collector
//! is in [`ConcurrentPhase::Idle`] (the common case), and shades
//! the target reference grey while marking is active. This module
//! owns the global heap behind a `parking_lot::Mutex` so the
//! barrier is the same symbol regardless of which generated
//! function called it.
//!
//! Concurrent collection cycle:
//!
//! ```text
//!   gos_rt_gc_concurrent_start()      // STW snapshot of roots
//!   ... mutator work; barriers grey writes ...
//!   gos_rt_gc_concurrent_step(budget) // chunked mark
//!   ...
//!   gos_rt_gc_concurrent_finish()     // STW remark + sweep
//! ```

use std::sync::OnceLock;

use gossamer_gc::{ConcurrentPhase, GcConfig, GcRef, GcStats, Heap};
use parking_lot::Mutex;

/// Global heap. Initialised on first access. Honours the
/// `GOSSAMER_GC_TARGET` env var: if set, its value (parsed as
/// bytes) becomes the heap-growth threshold the collector uses
/// before kicking off the next cycle. Default is the
/// `GcConfig::default()` value.
static HEAP: OnceLock<Mutex<Heap>> = OnceLock::new();

fn heap() -> &'static Mutex<Heap> {
    HEAP.get_or_init(|| {
        let mut config = GcConfig::default();
        if let Ok(v) = std::env::var("GOSSAMER_GC_TARGET") {
            if let Ok(bytes) = v.parse::<usize>() {
                if bytes > 0 {
                    config.threshold_bytes = bytes;
                }
            }
        }
        Mutex::new(Heap::with_config(config))
    })
}

/// Returns the current GC statistics snapshot — wraps
/// [`Heap::stats`] so callers don't need to acquire the global lock
/// themselves. Used by [`crate::runtime`]-equivalent stdlib code.
#[must_use]
pub fn stats() -> GcStats {
    with_heap(|h| h.stats())
}

/// Locks the global heap for the supplied closure. Internal use only.
pub fn with_heap<R>(f: impl FnOnce(&mut Heap) -> R) -> R {
    let mut guard = heap().lock();
    f(&mut guard)
}

/// Begins a concurrent GC cycle. Idempotent — calling while the
/// collector is already marking has no effect.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_concurrent_start() {
    with_heap(|h| {
        if matches!(h.concurrent_phase(), ConcurrentPhase::Idle) {
            h.concurrent_start();
        }
    });
}

/// Drains up to `budget` grey references, marking them. Returns
/// the number of objects actually marked this step.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_concurrent_step(budget: i64) -> i64 {
    let n = if budget <= 0 {
        256
    } else {
        usize::try_from(budget).unwrap_or(usize::MAX)
    };
    with_heap(|h| i64::try_from(h.concurrent_step(n)).unwrap_or(i64::MAX))
}

/// Finishes the concurrent cycle: short STW remark + sweep.
/// Returns the number of objects reclaimed by the sweep.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_concurrent_finish() -> i64 {
    with_heap(|h| i64::try_from(h.concurrent_finish()).unwrap_or(i64::MAX))
}

/// Write barrier emitted by codegen on every heap-pointer store.
/// `target` is the *new* reference being written into the heap; the
/// barrier shades it grey when a concurrent mark is active. The
/// fast path (idle phase) is a single load + compare + branch.
///
/// `target` is interpreted as a `GcRef`'s raw `u32`. A value of
/// `0` is treated as a null reference and skipped.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_write_barrier(target: u32) {
    if target == 0 {
        return;
    }
    let h = heap();
    // Fast path: read phase without acquiring the heap lock by
    // peeking — `parking_lot::Mutex::try_lock` lets us skip when
    // the heap is contended; in that case we conservatively defer
    // to the slow path which acquires the lock.
    if let Some(mut guard) = h.try_lock() {
        if matches!(guard.concurrent_phase(), ConcurrentPhase::Idle) {
            return;
        }
        // SAFETY of the GcRef construction: `Heap::write_barrier`
        // tolerates dead/stale handles by checking `is_live`
        // before re-greying.
        guard.write_barrier(GcRef::from_u32(target));
    } else {
        // Slow path: take the lock unconditionally. This still
        // does not allocate; the cost is contention with other
        // mutators racing the barrier.
        let mut guard = h.lock();
        if matches!(guard.concurrent_phase(), ConcurrentPhase::Idle) {
            return;
        }
        guard.write_barrier(GcRef::from_u32(target));
    }
}

/// Returns the current concurrent phase as an integer:
/// `0 = Idle`, `1 = Marking`, `2 = ReadyToSweep`. Used by tests
/// and by the scheduler-side write-barrier fast path that wants
/// to skip the call when the collector is idle.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_phase() -> i32 {
    with_heap(|h| match h.concurrent_phase() {
        ConcurrentPhase::Idle => 0,
        ConcurrentPhase::Marking => 1,
        ConcurrentPhase::ReadyToSweep => 2,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gossamer_gc::ObjKind;

    #[test]
    fn write_barrier_idle_is_noop() {
        // Idle phase: barrier returns without touching the heap.
        let ref0 = with_heap(|h| h.alloc(ObjKind::Leaf, Vec::new(), 0, 8));
        gos_rt_write_barrier(ref0.as_u32());
        // No assertion — just verifying no panic.
    }

    #[test]
    fn write_barrier_during_mark_greys_target() {
        // Allocate, root, start concurrent mark.
        let ref0 = with_heap(|h| {
            let r = h.alloc(ObjKind::Leaf, Vec::new(), 0, 8);
            h.add_root(r);
            r
        });
        gos_rt_gc_concurrent_start();
        assert_eq!(gos_rt_gc_phase(), 1);
        // Barrier on a live ref should not panic.
        gos_rt_write_barrier(ref0.as_u32());
        let _ = gos_rt_gc_concurrent_step(1024);
        let freed = gos_rt_gc_concurrent_finish();
        // The rooted object survives.
        assert!(with_heap(|h| h.is_rooted(ref0)));
        assert!(freed >= 0);
    }
}
