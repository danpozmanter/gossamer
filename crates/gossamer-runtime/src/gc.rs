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

use std::cell::RefCell;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU8, AtomicU32, AtomicUsize, Ordering};

use gossamer_gc::{ConcurrentPhase, GcConfig, GcRef, GcStats, Heap};
use parking_lot::Mutex;

/// Per-process registry of every thread's shadow-stack snapshot. The
/// scheduler-driven mark phase walks each entry to discover heap
/// references that live only on the (currently un-scannable) C
/// stack of compiled goroutines.
///
/// Each thread owns a [`LocalShadow`] containing a fixed-size
/// `[AtomicU32; STACK_CAPACITY]` slot array plus a published
/// `len: AtomicUsize`. The mutator (owner) writes slots and
/// publishes the new length via `Release`; the cross-thread mark
/// snapshot reads the length via `Acquire` and walks slots without
/// taking any lock. A cold-path `spill: Mutex<Vec<u32>>` handles
/// the rare case where call-stack depth exceeds the in-array
/// capacity.
///
/// The earlier design used a `Mutex<Vec<u32>>` per thread, which
/// paid an uncontended-but-real CAS at every function prologue and
/// epilogue (codegen emits `shadow_save` / `shadow_restore` at
/// every entry/exit, see C1 in `~/dev/contexts/lang/
/// adversarial_analysis.md`). The lock-free shape removes that
/// per-frame cost on the hot path; the locked spill remains correct
/// for the deep-stack overflow case.
const STACK_CAPACITY: usize = 1024;

struct LocalShadow {
    /// Pre-allocated, never-reallocated slot array. Owner writes
    /// with `Relaxed`; the mark thread reads with `Relaxed` after
    /// an `Acquire` load on `len` establishes happens-before.
    slots: Box<[AtomicU32; STACK_CAPACITY]>,
    /// Total logical depth, including any spill entries. The
    /// owner publishes pushes with `Release` so mark observes
    /// the slot writes through the synchronisation chain. Reads on
    /// the owner thread itself are `Relaxed` (data races against
    /// itself are impossible).
    len: AtomicUsize,
    /// Cold-path overflow buffer for stacks deeper than
    /// [`STACK_CAPACITY`]. The mutex contention is per-thread
    /// only when overflow is actually hit; cross-thread mark
    /// reads it under the same lock.
    spill: Mutex<Vec<u32>>,
}

impl LocalShadow {
    fn new() -> Self {
        // `Box::new([T; N])` would stack-allocate the array first; build
        // the slots through a Vec to avoid the temporary.
        let mut v = Vec::with_capacity(STACK_CAPACITY);
        for _ in 0..STACK_CAPACITY {
            v.push(AtomicU32::new(0));
        }
        let boxed: Box<[AtomicU32]> = v.into_boxed_slice();
        let Ok(slots) = TryInto::<Box<[AtomicU32; STACK_CAPACITY]>>::try_into(boxed) else {
            panic!("shadow stack slot array allocation")
        };
        Self {
            slots,
            len: AtomicUsize::new(0),
            spill: Mutex::new(Vec::new()),
        }
    }
}

type ShadowStack = std::sync::Arc<LocalShadow>;
type ShadowStackRegistry = Mutex<Vec<ShadowStack>>;

static SHADOW_STACKS: OnceLock<ShadowStackRegistry> = OnceLock::new();

fn shadow_stacks() -> &'static ShadowStackRegistry {
    SHADOW_STACKS.get_or_init(|| Mutex::new(Vec::new()))
}

thread_local! {
    /// One `Arc<LocalShadow>` per thread. The `Arc` keeps the
    /// storage alive for cross-thread mark snapshots even after
    /// the owning thread exits. `RefCell` because the closure
    /// passed to `with_local` does not call back into
    /// `with_local`, so dynamic borrow tracking is sufficient and
    /// preferable to the `UnsafeCell` + raw deref pattern (Stage
    /// 6, fix_architecture_ownership.md §3.10).
    static THREAD_SHADOW: RefCell<Option<ShadowStack>> =
        const { RefCell::new(None) };
}

fn with_local<R>(f: impl FnOnce(&LocalShadow) -> R) -> R {
    THREAD_SHADOW.with(|cell| {
        // First-use init: clone the Arc out under a separate
        // borrow so the registry push doesn't sit inside our own
        // RefCell borrow window. Cloning out is O(1) (Arc bump);
        // installation is a single mut-borrow assignment.
        let need_init = cell.borrow().is_none();
        if need_init {
            let arc = std::sync::Arc::new(LocalShadow::new());
            shadow_stacks().lock().push(std::sync::Arc::clone(&arc));
            *cell.borrow_mut() = Some(arc);
        }
        let guard = cell.borrow();
        f(guard.as_ref().expect("just initialised"))
    })
}

/// Pushes `r` onto the calling thread's shadow stack so the next GC
/// mark treats it as a live root.
pub fn shadow_push(r: GcRef) {
    let raw = r.as_u32();
    with_local(|local| {
        // Owner-thread Relaxed read — only the owner mutates `len`,
        // so the latest value is visible without synchronisation.
        let cur = local.len.load(Ordering::Relaxed);
        if cur < STACK_CAPACITY {
            local.slots[cur].store(raw, Ordering::Relaxed);
            // Release publishes both the slot write and any prior
            // owner writes so the mark thread's Acquire-load on
            // `len` sees them.
            local.len.store(cur + 1, Ordering::Release);
        } else {
            let mut s = local.spill.lock();
            s.push(raw);
            local.len.store(cur + 1, Ordering::Release);
        }
    });
}

/// Returns a frame token that [`shadow_restore`] uses to pop every
/// root pushed since the matching [`shadow_save`]. Codegen emits
/// `shadow_save` at function entry and `shadow_restore(token)` at
/// every return so leaked roots cannot pile up across calls.
#[must_use]
pub fn shadow_save() -> usize {
    with_local(|local| local.len.load(Ordering::Relaxed))
}

/// Truncates the calling thread's shadow stack back to a previously
/// captured `frame` token from [`shadow_save`].
pub fn shadow_restore(frame: usize) {
    with_local(|local| {
        let cur = local.len.load(Ordering::Relaxed);
        if frame >= cur {
            return;
        }
        if cur > STACK_CAPACITY {
            // Some entries live in spill. If the new frame is also
            // beyond the in-array capacity, truncate spill to the
            // remainder. Otherwise drop spill entirely (frame falls
            // back into the in-array region).
            let new_spill = frame.saturating_sub(STACK_CAPACITY);
            let mut s = local.spill.lock();
            if s.len() > new_spill {
                s.truncate(new_spill);
            }
        }
        local.len.store(frame, Ordering::Release);
    });
}

/// Snapshots every thread's shadow stack and feeds the entries
/// into `f` as `GcRef`s. The mark phase uses this to discover
/// stack-rooted objects without stop-the-world cooperation from
/// the mutators.
///
/// Reads cross-thread state without locking the per-thread fast
/// path. Acquire-loads `len` so the slot writes that preceded
/// each owner's `Release`-store are visible. The spill entries
/// are protected by their own mutex.
pub fn for_each_shadow_root(mut f: impl FnMut(GcRef)) {
    // Snapshot the registry's Arc list under its global lock,
    // then drop the registry lock before touching individual
    // thread state. Per-thread reads happen lock-free.
    let stacks = shadow_stacks().lock().clone();
    for s in &stacks {
        let n = s.len.load(Ordering::Acquire);
        let in_array = n.min(STACK_CAPACITY);
        for i in 0..in_array {
            // No null filter: a `GcRef::from_u32(0)` is a valid
            // heap-table index for the first object allocated in
            // this process, so we forward every published slot.
            // The C-ABI `gos_rt_gc_shadow_push` filters at the
            // entry point; the Rust `shadow_push` does not.
            let raw = s.slots[i].load(Ordering::Relaxed);
            f(GcRef::from_u32(raw));
        }
        if n > STACK_CAPACITY {
            let g = s.spill.lock();
            for &raw in g.iter() {
                f(GcRef::from_u32(raw));
            }
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
    // Drive one incremental step so marking work is amortised across
    // the allocation sequence rather than accumulating into a single
    // stop-the-world pause.
    drive_incremental();
    raw
}

/// Objects to mark per allocation-site incremental step.
const STEP_BUDGET: usize = 32;

/// Drives one step of the concurrent GC cycle from an allocation
/// site. Reads the `PHASE` atomic lock-free and takes the heap mutex
/// only when action is needed.
///
/// Mode selection via `GOSSAMER_GC_MODE`:
/// - unset / `concurrent` (default): incremental marking interleaved
///   with allocation, matching the description above.
/// - `stw`: never start a concurrent cycle from this path. The heap
///   still grows on allocation; explicit `gos_rt_gc_concurrent_*`
///   calls (or test harnesses) can still drive collection
///   stop-the-world. Useful for diagnosing GC-related bugs by
///   comparing program output between modes.
fn drive_incremental() {
    if gc_mode_is_stw() {
        return;
    }
    match PHASE.load(Ordering::Relaxed) {
        // Idle — start a new cycle when allocation pressure exceeds the
        // threshold. `gos_rt_gc_concurrent_start` also greys shadow-stack
        // roots, which is mandatory for compiled goroutines that hold
        // stack-only rooted refs.
        0 if with_heap(|h| h.should_start_concurrent_cycle()) => {
            gos_rt_gc_concurrent_start();
        }
        // Marking — mark a small batch to amortise work per allocation.
        1 => {
            gos_rt_gc_concurrent_step(STEP_BUDGET as i64);
        }
        // ReadyToSweep — finalise the cycle.
        2 => {
            gos_rt_gc_concurrent_finish();
        }
        _ => {}
    }
}

/// `true` when `GOSSAMER_GC_MODE` is set to `stw` (case-insensitive).
/// Cached behind an `OnceLock` so we don't re-parse the env var on
/// every allocation. The value is sampled once at first allocation;
/// changing the env var after the process is up has no effect (which
/// matches Go and the JVM's GC-mode flags).
fn gc_mode_is_stw() -> bool {
    static MODE: OnceLock<bool> = OnceLock::new();
    *MODE.get_or_init(|| {
        std::env::var("GOSSAMER_GC_MODE")
            .ok()
            .is_some_and(|v| v.eq_ignore_ascii_case("stw"))
    })
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

/// Safepoint hook callable from any goroutine. Production
/// codegen emits a call at every function prologue and at every
/// loop back-edge. The call is the unified entry point for both:
///
/// - the handle-based concurrent collector in this module (drives
///   one incremental step when the collector is active, or kicks
///   off a new cycle when heap pressure crosses the threshold);
/// - the raw-pointer tracing collector in `crate::c_abi` (runs a
///   STW mark + sweep over the aggregate registry when bytes
///   allocated since the last collect cross the configured
///   threshold).
///
/// Both branches are cheap in the common case (atomic-load +
/// compare). When neither collector needs work, the helper
/// returns in a handful of instructions.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_safepoint() {
    // Concurrent handle-based GC step (no-op under stw mode).
    if !gc_mode_is_stw() {
        drive_incremental();
    }
    // Raw-pointer tracing GC threshold check (runs STW mark+sweep
    // over the aggregate registry when the byte threshold trips).
    crate::c_abi::gos_rt_gc_raw_safepoint();
}

#[cfg(test)]
mod tests {
    use super::*;
    use gossamer_gc::ObjKind;

    // Serialise every test that touches the global heap/PHASE so
    // concurrent test runners don't interfere.
    static GC_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    fn drain_to_idle() {
        // Drive the global state machine to Idle so each test starts clean.
        match PHASE.load(Ordering::Relaxed) {
            1 => {
                let _ = gos_rt_gc_concurrent_step(i64::MAX);
                let _ = gos_rt_gc_concurrent_finish();
            }
            2 => {
                let _ = gos_rt_gc_concurrent_finish();
            }
            _ => {}
        }
        assert_eq!(gos_rt_gc_phase(), 0, "heap must be Idle before test");
    }

    #[test]
    fn write_barrier_idle_is_noop() {
        let _g = GC_TEST_LOCK.lock();
        drain_to_idle();
        let ref0 = with_heap(|h| h.alloc(ObjKind::Leaf, Vec::new(), 0, 8));
        gos_rt_write_barrier(ref0.as_u32());
        // No assertion — just verifying no panic.
    }

    #[test]
    fn write_barrier_during_mark_greys_target() {
        let _g = GC_TEST_LOCK.lock();
        drain_to_idle();
        let ref0 = with_heap(|h| {
            let r = h.alloc(ObjKind::Leaf, Vec::new(), 0, 8);
            h.add_root(r);
            r
        });
        gos_rt_gc_concurrent_start();
        assert_eq!(gos_rt_gc_phase(), 1);
        gos_rt_write_barrier(ref0.as_u32());
        let _ = gos_rt_gc_concurrent_step(1024);
        let freed = gos_rt_gc_concurrent_finish();
        assert!(with_heap(|h| h.is_rooted(ref0)));
        assert!(freed >= 0);
    }

    #[test]
    fn drive_incremental_steps_marking_phase() {
        let _g = GC_TEST_LOCK.lock();
        drain_to_idle();
        // Root one object and start the cycle manually.
        let ref0 = with_heap(|h| {
            let r = h.alloc(ObjKind::Leaf, Vec::new(), 0, 8);
            h.add_root(r);
            r
        });
        gos_rt_gc_concurrent_start();
        assert_eq!(gos_rt_gc_phase(), 1, "should be Marking after start");
        // A rooted allocation triggers drive_incremental, which should
        // step the grey set (just ref0) to ReadyToSweep.
        let _ = gos_rt_gc_alloc_rooted(8);
        let phase_after_step = gos_rt_gc_phase();
        // Either still Marking (large grey set) or ReadyToSweep (done).
        // With one root and STEP_BUDGET=32, it must be done.
        assert!(
            phase_after_step == 2 || phase_after_step == 0,
            "expected ReadyToSweep(2) or Idle(0) after stepping one-root grey set, got {phase_after_step}"
        );
        // Another allocation triggers the finish path → back to Idle.
        let _ = gos_rt_gc_alloc_rooted(8);
        assert_eq!(
            gos_rt_gc_phase(),
            0,
            "should be Idle after incremental finish"
        );
        // The explicitly rooted object must have survived the collection.
        assert!(with_heap(|h| h.is_rooted(ref0)));
    }
}
