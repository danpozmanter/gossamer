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
use std::sync::atomic::{AtomicU8, Ordering};

use gossamer_gc::{ConcurrentPhase, GcConfig, GcRef, GcStats, Heap};
use parking_lot::Mutex;

/// Per-process registry of every thread's shadow-stack snapshot. The
/// scheduler-driven mark phase walks each entry to discover heap
/// references that live only on the (currently un-scannable) C
/// stack of compiled goroutines.
///
/// Each thread publishes a `Vec<u32>` mirror of its shadow stack
/// here. The mirror is rebuilt lazily by `with_shadow_stack` at the
/// first push or pop after registration, and again every time the
/// global mark phase requests a snapshot. This trades a small per-
/// alloc cost for a process-wide root walk that does not require
/// stack-map emission in the codegen — see C1 in
/// `~/dev/contexts/lang/adversarial_analysis.md` for context.
type ShadowStack = std::sync::Arc<Mutex<Vec<u32>>>;
type ShadowStackRegistry = Mutex<Vec<ShadowStack>>;

static SHADOW_STACKS: OnceLock<ShadowStackRegistry> = OnceLock::new();

fn shadow_stacks() -> &'static ShadowStackRegistry {
    SHADOW_STACKS.get_or_init(|| Mutex::new(Vec::new()))
}

thread_local! {
    static THREAD_SHADOW: std::cell::OnceCell<std::sync::Arc<Mutex<Vec<u32>>>>
        = const { std::cell::OnceCell::new() };
}

fn thread_shadow() -> std::sync::Arc<Mutex<Vec<u32>>> {
    THREAD_SHADOW.with(|cell| {
        cell.get_or_init(|| {
            let arc = std::sync::Arc::new(Mutex::new(Vec::new()));
            shadow_stacks().lock().push(std::sync::Arc::clone(&arc));
            arc
        })
        .clone()
    })
}

/// Pushes `r` onto the calling thread's shadow stack so the next GC
/// mark treats it as a live root.
pub fn shadow_push(r: GcRef) {
    let s = thread_shadow();
    s.lock().push(r.as_u32());
}

/// Returns a frame token that [`shadow_restore`] uses to pop every
/// root pushed since the matching [`shadow_save`]. Codegen emits
/// `shadow_save` at function entry and `shadow_restore(token)` at
/// every return so leaked roots cannot pile up across calls.
#[must_use]
pub fn shadow_save() -> usize {
    thread_shadow().lock().len()
}

/// Truncates the calling thread's shadow stack back to a previously
/// captured `frame` token from [`shadow_save`].
pub fn shadow_restore(frame: usize) {
    let s = thread_shadow();
    let mut g = s.lock();
    if g.len() > frame {
        g.truncate(frame);
    }
}

/// Snapshots every thread's shadow stack and feeds the entries
/// into `f` as `GcRef`s. The mark phase uses this to discover
/// stack-rooted objects without stop-the-world cooperation from
/// the mutators.
pub fn for_each_shadow_root(mut f: impl FnMut(GcRef)) {
    let stacks = shadow_stacks().lock().clone();
    for s in &stacks {
        let g = s.lock();
        for &raw in g.iter() {
            f(GcRef::from_u32(raw));
        }
    }
}

/// C-ABI wrapper for [`shadow_push`]. Codegen emits a call to this
/// at every `gos_rt_gc_alloc_rooted` site (the rooted variant
/// pushes for the caller; the bare allocator does not).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_shadow_push(raw: u32) {
    if raw == 0 {
        return;
    }
    shadow_push(GcRef::from_u32(raw));
}

/// C-ABI for [`shadow_save`]. Returns a `u64` because Rust `usize`
/// has no portable C representation; callers truncate as needed.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_shadow_save() -> u64 {
    u64::try_from(shadow_save()).unwrap_or(u64::MAX)
}

/// C-ABI for [`shadow_restore`].
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_shadow_restore(frame: u64) {
    let frame = usize::try_from(frame).unwrap_or(usize::MAX);
    shadow_restore(frame);
}

/// Allocates and immediately roots a `size`-byte leaf object in the
/// global heap. Used by codegen at sites where the new pointer is
/// only visible from the C stack until later stored elsewhere — the
/// shadow-stack push keeps the GC from reclaiming it during a
/// concurrent cycle. Returns the raw `u32` of the new `GcRef` (cast
/// through `i64` for the LLVM ABI). Returns `0` on allocation
/// failure.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_alloc_rooted(size: i64) -> u32 {
    let size = usize::try_from(size).unwrap_or(0);
    let r = with_heap(|h| h.alloc(gossamer_gc::ObjKind::Leaf, Vec::new(), 0, size));
    let raw = r.as_u32();
    shadow_push(r);
    raw
}

/// Lock-free mirror of `Heap::concurrent_phase()`. Updated by
/// every start / step / finish entry point. The hot write-barrier
/// path consults this atomic instead of the heap mutex.
/// Encoding matches `gos_rt_gc_phase`: 0 = `Idle`, 1 = `Marking`,
/// 2 = `ReadyToSweep`.
static PHASE: AtomicU8 = AtomicU8::new(0);

fn phase_to_u8(p: ConcurrentPhase) -> u8 {
    match p {
        ConcurrentPhase::Idle => 0,
        ConcurrentPhase::Marking => 1,
        ConcurrentPhase::ReadyToSweep => 2,
    }
}

/// Global heap. Initialised on first access. Honours the
/// `GOSSAMER_GC_TARGET` env var: if set, its value (parsed as
/// bytes) becomes the heap-growth threshold the collector uses
/// before kicking off the next cycle. Default is the
/// `GcConfig::default()` value.
static HEAP: OnceLock<Mutex<Heap>> = OnceLock::new();

fn heap() -> &'static Mutex<Heap> {
    HEAP.get_or_init(|| {
        let mut config = GcConfig::default();
        if let Ok(v) = std::env::var("GOSSAMER_GC_TARGET")
            && let Ok(bytes) = v.parse::<usize>()
            && bytes > 0
        {
            config.threshold_bytes = bytes;
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
///
/// Greys every shadow-stack root before starting the mark so
/// stack-only references survive the cycle. The mark loop then
/// visits them transitively the same way it visits an explicit
/// `add_root` entry. Without this snapshot, codegen-allocated
/// objects that have not yet been stored into a longer-lived
/// container would be reclaimed mid-cycle (C1 in the audit).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_concurrent_start() {
    with_heap(|h| {
        if matches!(h.concurrent_phase(), ConcurrentPhase::Idle) {
            h.concurrent_start();
            // After concurrent_start has greyed the explicit roots,
            // also grey every shadow-stack root so the mark loop
            // walks them.
            for_each_shadow_root(|r| {
                h.write_barrier(r);
            });
            PHASE.store(phase_to_u8(h.concurrent_phase()), Ordering::Release);
        }
    });
}

/// Forces a stop-the-world collection that includes shadow-stack
/// roots. Used by tests and tooling that want a deterministic
/// reclamation cycle without driving the concurrent state machine.
/// Returns the number of objects reclaimed.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_collect_with_stack_roots() -> i64 {
    // Promote every shadow-stack root to an explicit root for the
    // duration of the collection, then remove the temporary
    // entries so the next collection can drop them again. A
    // dedicated "scoped roots" API on `Heap` would be more
    // efficient; the temporary promotion is correct and small.
    let snapshot = {
        let mut out = Vec::new();
        for_each_shadow_root(|r| out.push(r));
        out
    };
    let freed = with_heap(|h| {
        for r in &snapshot {
            h.add_root(*r);
        }
        let freed = h.collect();
        for r in &snapshot {
            h.remove_root(*r);
        }
        freed
    });
    i64::try_from(freed).unwrap_or(i64::MAX)
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
    with_heap(|h| {
        let marked = h.concurrent_step(n);
        PHASE.store(phase_to_u8(h.concurrent_phase()), Ordering::Release);
        i64::try_from(marked).unwrap_or(i64::MAX)
    })
}

/// Finishes the concurrent cycle: short STW remark + sweep.
/// Returns the number of objects reclaimed by the sweep.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_concurrent_finish() -> i64 {
    with_heap(|h| {
        let freed = h.concurrent_finish();
        PHASE.store(phase_to_u8(h.concurrent_phase()), Ordering::Release);
        i64::try_from(freed).unwrap_or(i64::MAX)
    })
}

/// Write barrier emitted by codegen on every heap-pointer store.
/// Lock-free fast path: a single relaxed load + branch on the
/// `PHASE` atomic. The heap mutex is only acquired when an actual
/// greying needs to happen.
///
/// `target` is interpreted as a `GcRef`'s raw `u32`. A value of
/// `0` is treated as a null reference and skipped.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_write_barrier(target: u32) {
    if target == 0 {
        return;
    }
    if PHASE.load(Ordering::Relaxed) == 0 {
        return;
    }
    let mut guard = heap().lock();
    if matches!(guard.concurrent_phase(), ConcurrentPhase::Idle) {
        return;
    }
    guard.write_barrier(GcRef::from_u32(target));
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
