#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::alloc::{Layout, alloc, alloc_zeroed, dealloc};
use std::sync::atomic::{AtomicUsize, Ordering};

// ---------------------------------------------------------------
// Intrusive reference counting for compiled-tier heap objects.
// ---------------------------------------------------------------
//
// Every RC-managed heap object is laid out as `[ RcHeader | payload ]`.
// The pointer the compiled program holds points at the *payload*; the
// header sits `RC_HEADER_SIZE` bytes before it. There is no global
// allocation registry — lifetime is owned entirely by the strong
// refcount in each object's header. This replaces the raw-pointer
// tracing GC, which could not discover live roots precisely under
// optimized LLVM (see 0100_GC.md §0).
//
// retain = +1 strong; release = -1 strong, and at zero the object's
// RC-pointer children are released (iteratively) and the payload is
// destroyed. This matches the interpreter tier's `Arc`-payload
// semantics, which is the semantic oracle.
//
// Weak references (Swift-ARC model): a `Weak<T>` does not contribute to
// the strong count. The *payload* (the user's value and its child
// releases) is destroyed when `strong` hits 0; the *allocation* is freed
// only when both `strong` and `weak` hit 0, so a dangling `Weak` can
// still safely read "is the referent alive?" (`strong > 0`). See the
// hybrid memory model design (RC + cycle collector + weak refs).

/// Intrusive header prefixed to every RC-managed allocation.
///
/// The compiled program never computes this offset itself: it holds a
/// payload pointer and passes it to `gos_rt_rc_retain` / `_release` /
/// `_downgrade` / `_weak_*`, which recover the header internally. The
/// exact header size is thus a runtime-private detail.
#[repr(C)]
pub struct RcHeader {
    /// Strong reference count. Starts at 1 on allocation. `u32` (≥4 billion
    /// live refs is unreachable) to keep the header at 16 bytes — every heap
    /// object pays this, so a node `Node(i64, Box, Box)` is 16 + 24 = 40
    /// bytes instead of 48.
    pub strong: u32,
    /// Weak reference count (saturating). The allocation outlives `strong ==
    /// 0` whenever this is non-zero, so a `Weak` can probe liveness without
    /// reading freed memory. `u16` is ample (65535 simultaneous weak handles
    /// to one object) and keeps the header at 16 bytes.
    pub weak: u16,
    /// Allocation size in [`SIZE_UNIT`]-byte units (header + payload),
    /// recovered at deallocation since the release site is type-erased.
    /// [`SIZE_OVERSIZED`] is a sentinel meaning "real byte size lives in the
    /// oversized side table" (only blocks larger than ~1 MiB, effectively
    /// never for ADTs). Storing units rather than raw bytes frees 16 bits
    /// for the weak count without growing the header.
    pub size_u: u16,
    /// Child-layout descriptor blob for recursive release. Null for leaf
    /// objects with no RC-pointer children. See the blob format below.
    pub meta: *const i64,
}

/// 8-byte alignment is hard-coded across the runtime ABI; all payload
/// fields are word-sized and word-aligned.
pub const RC_ALIGN: usize = 8;

/// Size of [`RcHeader`], rounded to the runtime alignment. The payload
/// begins this many bytes after the allocation base.
pub const RC_HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();

// The header must stay 16 bytes: every heap object pays it, so growth is a
// direct per-object RAM regression. The hybrid weak/cycle fields are packed
// into the existing 16 bytes (see the field docs), never added on top.
const _: () = assert!(RC_HEADER_SIZE == 16, "RcHeader must remain 16 bytes");

// ---------------------------------------------------------------
// Type-meta blob format (a flat, self-describing `[i64]`).
// ---------------------------------------------------------------
//
// Codegen emits one such blob per RC-managed user ADT as a single
// contiguous module constant (trivial to lower in both LLVM and
// Cranelift, unlike a nested pointer-laden descriptor). The header's
// `meta` points at word 0.
//
//   [0] kind            — RC_KIND_*
//   [1] variant_count V
//   then V variant records, each variable-length:
//       disc            — discriminant value this record describes
//       child_count C   — number of RC-pointer child words
//       off_0 .. off_C  — payload WORD indices (byte offset / 8) holding
//                         RC-managed child pointers to release
//
// For an enum, `release_children` reads the live discriminant from
// payload word 0 and releases the children of the matching record. For
// a struct/tuple there is a single record and the discriminant is
// ignored.

// `meta[0]` kind discriminants live in `gossamer-abi` (the single
// source shared with the MIR lowerer that emits these blobs). Only
// `Enum` and `Struct` carry child layouts today; the heap builtins
// (string/vec/map/closure) are wired in a later phase.
pub use gossamer_abi::rc::{
    RC_KIND_CLOSURE, RC_KIND_ENUM, RC_KIND_MAP, RC_KIND_STRING, RC_KIND_STRUCT, RC_KIND_VEC,
};

/// Count of live RC objects (allocated minus freed). Diagnostic only —
/// a single relaxed counter, negligible against allocation cost — used
/// by tests and available for future leak reporting.
static RC_LIVE: AtomicUsize = AtomicUsize::new(0);

/// Number of RC-managed objects currently alive. Test/diagnostic hook.
pub fn rc_live_count() -> usize {
    RC_LIVE.load(Ordering::Relaxed)
}

// ---------------------------------------------------------------
// Cycle-collector header bits (synchronous trial deletion).
// ---------------------------------------------------------------
//
// Reference counting cannot reclaim cycles (`A -> B -> A` never reaches
// count 0). A synchronous Bacon-Rajan trial-deletion collector reclaims
// cyclic RC garbage by tracing the object graph from a buffer of
// *candidate roots* — objects whose strong count was decremented to a
// non-zero value (the only objects that can start a cycle). It needs no
// stack scanning and no compiler root map, so it is sound under `-O3` by
// construction (it never inspects a register or spill slot), and it stays
// a no-op for the acyclic 99%: acyclic objects free immediately at count
// 0 exactly as before, and the collector runs only when the candidate
// buffer crosses a threshold.
//
// The four collector flags live in the high bits of `strong`, leaving the
// low 28 bits for the count (268M live refs is unreachable). The hot
// retain/release paths mask the count portion; the flag bits are touched
// only on the cold release-to-non-zero path and during collection, so the
// acyclic fast path pays only a mask.

/// Low 28 bits of `strong`: the actual strong reference count.
const STRONG_COUNT_MASK: u32 = 0x0FFF_FFFF;
/// Bit 31: the object sits in the cycle-collector candidate buffer.
const BUFFERED_BIT: u32 = 1 << 31;
/// Bits 28-29: trial-deletion color.
const COLOR_SHIFT: u32 = 28;
const COLOR_MASK: u32 = 0b11 << COLOR_SHIFT;
/// Bit 30: the object was bump-allocated inside an arena region. Its
/// lifetime is the region's, so retain/release are no-ops and it is freed
/// wholesale (no per-node teardown walk) when the region is popped. This is
/// the bit that lets a `region { … }` block sidestep RC's per-node
/// reclamation cost on short-lived allocation churn.
const REGION_BIT: u32 = 1 << 30;

#[inline]
unsafe fn is_region(h: *const RcHeader) -> bool {
    (unsafe { (*h).strong }) & REGION_BIT != 0
}

/// In active use, or already freed. The default (zeroed) color.
const COLOR_BLACK: u32 = 0;
/// Possible member of a garbage cycle (being traced).
const COLOR_GRAY: u32 = 1;
/// Confirmed member of a garbage cycle (to be collected).
const COLOR_WHITE: u32 = 2;
/// Possible root of a garbage cycle (decremented to non-zero).
const COLOR_PURPLE: u32 = 3;

#[inline]
unsafe fn strong_count(h: *const RcHeader) -> u32 {
    (unsafe { (*h).strong }) & STRONG_COUNT_MASK
}

/// Overwrite the count portion of `strong`, preserving the flag bits.
#[inline]
unsafe fn set_strong_count(h: *mut RcHeader, count: u32) {
    let flags = unsafe { (*h).strong } & !STRONG_COUNT_MASK;
    unsafe { (*h).strong = flags | (count & STRONG_COUNT_MASK) };
}

#[inline]
unsafe fn color_of(h: *const RcHeader) -> u32 {
    ((unsafe { (*h).strong }) & COLOR_MASK) >> COLOR_SHIFT
}

#[inline]
unsafe fn set_color(h: *mut RcHeader, color: u32) {
    let rest = unsafe { (*h).strong } & !COLOR_MASK;
    unsafe { (*h).strong = rest | (color << COLOR_SHIFT) };
}

#[inline]
unsafe fn is_buffered(h: *const RcHeader) -> bool {
    (unsafe { (*h).strong }) & BUFFERED_BIT != 0
}

#[inline]
unsafe fn set_buffered(h: *mut RcHeader, on: bool) {
    if on {
        unsafe { (*h).strong |= BUFFERED_BIT };
    } else {
        unsafe { (*h).strong &= !BUFFERED_BIT };
    }
}

/// Whether `payload`'s *live* shape holds at least one RC-pointer child —
/// the precondition for being a cycle member. An object with no current
/// children (a leaf, an empty-variant enum, a struct of scalars) can never
/// start a cycle, so it is never buffered as a candidate and frees
/// immediately at count 0 like any acyclic object.
#[inline]
unsafe fn has_rc_children(payload: *mut u8) -> bool {
    let mut found = false;
    unsafe { visit_rc_children(payload, |_| found = true) };
    found
}

/// Default candidate-buffer size that auto-triggers a collection. Tuned so
/// the collector runs rarely and only when cyclic garbage is plausibly
/// accumulating; acyclic workloads never fill it (nothing is buffered).
const DEFAULT_COLLECT_THRESHOLD: usize = 10_000;

thread_local! {
    /// Candidate roots: objects whose strong count was decremented to a
    /// non-zero value. Deduplicated by the `BUFFERED_BIT`. Thread-local so
    /// buffering needs no lock on the release hot path; the collector runs
    /// on the same thread over its own candidates.
    static ROOTS: std::cell::RefCell<Vec<*mut u8>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Total cyclic objects reclaimed by the collector. Diagnostic / test hook.
static CYCLES_FREED: AtomicUsize = AtomicUsize::new(0);

/// Number of cyclic RC objects reclaimed so far. Test/diagnostic hook.
pub fn rc_cycles_freed() -> usize {
    CYCLES_FREED.load(Ordering::Relaxed)
}

/// Record `payload` as a possible cycle root: strong count was just
/// decremented but is still non-zero, so it may be part of a cycle whose
/// only remaining references are internal. Buffered once (deduplicated by
/// the header bit); the buffer auto-collects when it crosses the threshold.
unsafe fn possible_root(payload: *mut u8) {
    if !unsafe { has_rc_children(payload) } {
        return;
    }
    let h = unsafe { header_ptr(payload) };
    // Color purple marks a candidate; skip if already a tracked root.
    if unsafe { color_of(h) } == COLOR_PURPLE {
        return;
    }
    unsafe { set_color(h, COLOR_PURPLE) };
    if unsafe { is_buffered(h) } {
        return;
    }
    unsafe { set_buffered(h, true) };
    let over = ROOTS.with(|r| {
        let mut roots = r.borrow_mut();
        roots.push(payload);
        roots.len() >= DEFAULT_COLLECT_THRESHOLD
    });
    if over {
        unsafe { collect_cycles() };
    }
}

// ---------------------------------------------------------------
// Size-class free-list (recycling slab) allocator.
// ---------------------------------------------------------------
//
// RC objects are small and overwhelmingly uniform-sized (a tree's nodes,
// a graph's adjacency cells), allocated and freed in tight churn. Routing
// every node through libc `malloc`/`free` is the dominant cost on
// allocation-heavy workloads; Rust's reference implementations of the
// same programs use a bump arena. This caches freed blocks per size class
// and hands them straight back on the next allocation of that class — a
// pop/push instead of a malloc/free round-trip.
//
// Blocks are recycled by *byte size* (rounded to `CLASS_STEP`), never by
// type: a freed block of N bytes can back any later N-byte allocation
// regardless of which ADT it held, because the allocator only manages raw
// storage (the header + payload are fully rewritten on reuse).
//
// Soundness: RC objects migrate across threads (channels), so a freed
// block may be returned on a different thread than it was taken from. The
// free-lists are therefore global, sharded to bound lock contention. Each
// shard holds raw addresses as `usize` (Send + Sync); the per-class cap
// returns surplus blocks to the OS so a one-time large burst cannot pin
// memory forever.

/// Granularity of a size class, in bytes. Allocations round up to the next
/// multiple; same-class blocks are interchangeable.
const CLASS_STEP: usize = 16;

/// Byte granularity the header's `size_u` counts in. Every allocation is
/// rounded up to this so the rounded byte size divides evenly into units.
const SIZE_UNIT: usize = CLASS_STEP;

/// `size_u` sentinel: the block is larger than `(u16::MAX - 1) * SIZE_UNIT`
/// (~1 MiB) and its real byte size is recorded in the oversized side table.
/// Only reachable for pathologically large single allocations.
const SIZE_OVERSIZED: u16 = u16::MAX;

/// Byte size of oversized (`> ~1 MiB`) blocks, keyed by base address. These
/// bypass the size-class allocator entirely; the map exists only to recover
/// the `Layout` at deallocation, never for liveness or roots. Effectively
/// always empty for ADT workloads.
static OVERSIZED: parking_lot::Mutex<Vec<(usize, usize)>> = parking_lot::Mutex::new(Vec::new());

fn oversized_register(base: *mut u8, total: usize) {
    OVERSIZED.lock().push((base as usize, total));
}

/// Remove and return the recorded byte size for an oversized `base`.
fn oversized_take(base: *mut u8) -> usize {
    let key = base as usize;
    let mut map = OVERSIZED.lock();
    if let Some(i) = map.iter().position(|&(k, _)| k == key) {
        map.swap_remove(i).1
    } else {
        0
    }
}

/// Number of size classes. Class `c` covers `c * CLASS_STEP` bytes; the
/// largest pooled block is `NUM_CLASSES * CLASS_STEP` (1 KiB). Larger
/// allocations bypass the pool and use the global allocator directly.
const NUM_CLASSES: usize = 64;

/// Rounds `total` bytes to its size class. Returns `(rounded_bytes,
/// Some(class_index))` when poolable, or `(rounded, None)` for oversized
/// allocations that bypass the pool. The rounded byte size equals
/// `units * SIZE_UNIT`.
#[inline]
fn size_class(total: usize) -> (usize, Option<usize>) {
    let rounded = total.div_ceil(CLASS_STEP) * CLASS_STEP;
    (rounded, class_of_units(rounded / CLASS_STEP))
}

/// The pool class for a block of `units` size-units, or `None` when the
/// block is too large (or too small) to recycle.
#[inline]
fn class_of_units(units: usize) -> Option<usize> {
    if (1..NUM_CLASSES).contains(&units) {
        Some(units)
    } else {
        None
    }
}

#[inline]
unsafe fn header_ptr(payload: *mut u8) -> *mut RcHeader {
    unsafe { payload.sub(RC_HEADER_SIZE) as *mut RcHeader }
}

// ---------------------------------------------------------------
// Arena regions (`region { … }`).
// ---------------------------------------------------------------
//
// A region is a bump allocator on a stack of large slabs. While a region
// is active, `gos_rt_rc_alloc` allocates from it and tags the object with
// `REGION_BIT`; retain/release on such objects are no-ops, and the whole
// region is freed in O(slabs) at `gos_rt_region_pop` — never a per-node
// teardown walk. The compiler guarantees no region object outlives the
// pop (region-block results are RC-free and region values cannot be
// assigned to outer bindings), so the bulk free is sound.

/// Default slab size; one `mmap`-backed glibc allocation amortised over
/// many node allocations. A single oversized object gets its own slab.
const REGION_SLAB_BYTES: usize = 1 << 20;

struct RegionSlabs {
    /// `(base, layout_size)` for each slab, freed at pop.
    slabs: Vec<(*mut u8, usize)>,
    /// Saved bump state (base, cur offset, slab end) for this region while it
    /// is NOT the innermost one. The innermost region's live bump lives in the
    /// `BUMP` cache instead, so the hot allocation path touches no `RefCell`.
    saved: BumpState,
    /// Objects committed to this region before it was suspended (the live
    /// innermost region's uncommitted count is in `BUMP_OBJS`).
    objs: usize,
}

/// Thread-local bump-pointer cache: `(base, cur, end)` of the innermost
/// region's current slab. A null `base` means "allocate a slab on next use".
/// This is what makes a region allocation a handful of inline instructions
/// (compare + add) rather than a `RefCell` borrow + `Vec` walk.
#[derive(Clone, Copy)]
struct BumpState {
    base: *mut u8,
    cur: usize,
    end: usize,
}

impl BumpState {
    const EMPTY: BumpState = BumpState {
        base: std::ptr::null_mut(),
        cur: 0,
        end: 0,
    };
}

/// Max recycled standard-size slabs kept per thread. Auto-regions on a
/// fine-grained loop (millions of tiny iterations) push/pop a region every
/// iteration; recycling the backing slab through this pool turns each
/// `region_push` into a bump-pointer reset instead of a 1 MiB `mmap`.
const FREE_SLAB_CAP: usize = 64;

thread_local! {
    /// Stack of suspended regions on this thread (the innermost region's live
    /// bump is in `BUMP`). Only touched on push/pop/slab-exhaustion.
    static REGIONS: std::cell::RefCell<Vec<RegionSlabs>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Pool of freed standard-size (`REGION_SLAB_BYTES`) slabs, reused by the
    /// next `region_push` instead of re-`mmap`ing. Bounded by `FREE_SLAB_CAP`.
    static FREE_SLABS: std::cell::RefCell<Vec<*mut u8>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Nesting depth of active regions. `> 0` ⇒ a region is open; checked once
    /// per allocation (a `Cell` read) to route to the region bump.
    static REGION_DEPTH: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
    /// Live bump state of the innermost region (see `BumpState`).
    static BUMP: std::cell::Cell<BumpState> = const { std::cell::Cell::new(BumpState::EMPTY) };
    /// RC objects bump-allocated into the innermost region since it became
    /// innermost (reconciled into `RegionSlabs::objs` at push, into `RC_LIVE`
    /// at pop).
    static BUMP_OBJS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Acquire a slab of `slab_size` bytes. Standard-size slabs come from the
/// recycling pool when available (no `mmap`); oversized ones are always
/// freshly allocated. Bytes are NOT zeroed — `region_alloc_inner` zeroes
/// each handed-out allocation, so a recycled slab needs no bulk clear.
fn acquire_slab(slab_size: usize) -> *mut u8 {
    if slab_size == REGION_SLAB_BYTES {
        if let Some(s) = FREE_SLABS.with(|p| p.borrow_mut().pop()) {
            return s;
        }
    }
    match Layout::from_size_align(slab_size, RC_ALIGN) {
        // SAFETY: validated non-zero layout; bytes are zeroed per-allocation.
        Ok(layout) => unsafe { alloc(layout) },
        Err(_) => std::ptr::null_mut(),
    }
}

#[inline]
fn region_active() -> bool {
    REGION_DEPTH.with(|d| d.get() > 0)
}

/// Public: is an arena region active on this thread? Used by the Vec/String
/// allocators to route their backing storage through the region so it is
/// freed wholesale at pop (and so their `free` becomes a no-op).
#[must_use]
pub fn region_is_active() -> bool {
    region_active()
}

/// Public: bump `n` zeroed, `RC_ALIGN`-aligned bytes from the active region,
/// or null if no region is active. The bytes are freed wholesale at
/// `region_pop` — callers must NOT individually free them.
#[must_use]
pub fn region_alloc_bytes(n: usize) -> *mut u8 {
    if n == 0 || !region_active() {
        return std::ptr::null_mut();
    }
    // Raw bytes (Vec/String backing) are not RC_LIVE-counted, so don't bump
    // the region's RC-object tally — doing so underflows RC_LIVE at pop.
    region_alloc_inner(n, false)
}

/// Bump `total` zeroed, `RC_ALIGN`-aligned bytes from the innermost active
/// region. `count_obj` increments the region's RC-object tally (used to
/// reconcile `RC_LIVE` at pop) — true for RC payloads, false for raw
/// Vec/String backing bytes (which are not `RC_LIVE`-counted). Returns null
/// only on allocation failure. Caller guarantees a region is active.
fn region_alloc_inner(total: usize, count_obj: bool) -> *mut u8 {
    let need = (total + RC_ALIGN - 1) & !(RC_ALIGN - 1);
    // Hot path: bump within the innermost region's current slab — no RefCell,
    // no Vec walk, just a compare and an add on a thread-local cache.
    let ptr = BUMP.with(|b| {
        let st = b.get();
        if !st.base.is_null() && st.cur + need <= st.end {
            let p = unsafe { st.base.add(st.cur) };
            b.set(BumpState {
                cur: st.cur + need,
                ..st
            });
            p
        } else {
            std::ptr::null_mut()
        }
    });
    let ptr = if ptr.is_null() {
        let p = region_alloc_slow(need);
        if p.is_null() {
            return std::ptr::null_mut();
        }
        p
    } else {
        ptr
    };
    // Zero the handed-out bytes: slabs may be recycled or freshly (un-zeroed)
    // allocated, and codegen relies on every allocation starting zeroed.
    unsafe { std::ptr::write_bytes(ptr, 0, need) };
    if count_obj {
        BUMP_OBJS.with(|o| o.set(o.get() + 1));
    }
    ptr
}

/// Cold path: the current slab can't fit `need`. Acquire a fresh slab, record
/// it on the innermost region, point the bump cache at it, and carve `need`.
#[cold]
fn region_alloc_slow(need: usize) -> *mut u8 {
    let slab_size = need.max(REGION_SLAB_BYTES);
    let base = acquire_slab(slab_size);
    if base.is_null() {
        return std::ptr::null_mut();
    }
    REGIONS.with(|r| {
        let mut regions = r.borrow_mut();
        let region = regions.last_mut().expect("region_alloc with no region");
        region.slabs.push((base, slab_size));
    });
    BUMP.with(|b| {
        b.set(BumpState {
            base,
            cur: need,
            end: slab_size,
        });
    });
    base
}

/// Region bump for an RC payload (counted against `RC_LIVE`).
fn region_alloc(total: usize) -> *mut u8 {
    region_alloc_inner(total, true)
}

/// Open a new arena region. Allocations until the matching
/// [`gos_rt_region_pop`] are bump-allocated and freed wholesale.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_region_push() {
    // Suspend the current innermost region's live bump into its `RegionSlabs`
    // entry, then open a fresh region with an empty bump.
    let saved = BUMP.with(std::cell::Cell::get);
    let pending_objs = BUMP_OBJS.with(|o| o.replace(0));
    REGIONS.with(|r| {
        let mut regions = r.borrow_mut();
        if let Some(top) = regions.last_mut() {
            top.saved = saved;
            top.objs += pending_objs;
        }
        regions.push(RegionSlabs {
            slabs: Vec::new(),
            saved: BumpState::EMPTY,
            objs: 0,
        });
    });
    BUMP.with(|b| b.set(BumpState::EMPTY));
    REGION_DEPTH.with(|d| d.set(d.get() + 1));
}

/// Close the innermost region: free/recycle every slab in O(slabs). No
/// per-object teardown walk runs — the escape analysis guarantees nothing in
/// the region is referenced after pop.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_region_pop() {
    let pending_objs = BUMP_OBJS.with(|o| o.replace(0));
    let restored = REGIONS.with(|r| {
        let mut regions = r.borrow_mut();
        let region = regions.pop()?;
        RC_LIVE.fetch_sub(region.objs + pending_objs, Ordering::Relaxed);
        for (base, size) in region.slabs {
            // Recycle standard-size slabs into the thread-local pool (up to the
            // cap) so the next region reuses them without an mmap.
            if size == REGION_SLAB_BYTES {
                let kept = FREE_SLABS.with(|p| {
                    let mut pool = p.borrow_mut();
                    if pool.len() < FREE_SLAB_CAP {
                        pool.push(base);
                        true
                    } else {
                        false
                    }
                });
                if kept {
                    continue;
                }
            }
            if let Ok(layout) = Layout::from_size_align(size, RC_ALIGN) {
                // SAFETY: `base` came from `acquire_slab` with this layout.
                unsafe { dealloc(base, layout) };
            }
        }
        // Resume the parent region's suspended bump (empty if none remains).
        Some(regions.last().map_or(BumpState::EMPTY, |top| top.saved))
    });
    if let Some(bump) = restored {
        BUMP.with(|b| b.set(bump));
    }
    REGION_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
}

/// Allocate an RC-managed object with `size` payload bytes and the given
/// child-layout `meta` (may be null for leaves). Returns a pointer to the
/// zeroed payload with strong count 1, or null on allocation failure.
// The RC primitives are deliberately NOT wrapped in `ffi_entry!`
// (catch_unwind): they are called once per allocation / copy / drop and
// the per-call unwind-guard setup dominates their cost. They are also
// panic-free across the FFI boundary — pointer arithmetic and atomics
// never unwind, and the only allocator failure paths (`alloc_zeroed`
// returning null, `Vec` growth) `abort` rather than unwind. Keeping them
// bare is what makes RC-managed code fast.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_alloc(size: u64, meta: *const i64) -> *mut u8 {
    let raw = (size as usize).saturating_add(RC_HEADER_SIZE);
    let (total, class) = size_class(raw);
    // Inside a `region { … }` the object is bump-allocated and freed
    // wholesale at pop — tag it so retain/release stay no-ops and the
    // teardown walk never touches it.
    let in_region = region_active();
    // The system allocator (glibc tcache / equivalent) is the RC node
    // allocator: a measured A/B against a custom thread-local slab AND the
    // earlier recycling pool both showed zero speed/RAM benefit — per-node
    // alloc/free is not the bottleneck. The wins come from regions (bulk-free
    // whole iterations) and inlining, not from swapping the allocator.
    let _ = class;
    let base = if in_region {
        region_alloc(total)
    } else {
        let Ok(layout) = Layout::from_size_align(total, RC_ALIGN) else {
            return std::ptr::null_mut();
        };
        unsafe { alloc_zeroed(layout) }
    };
    if base.is_null() {
        return std::ptr::null_mut();
    }
    let h = base as *mut RcHeader;
    let units = total / SIZE_UNIT;
    unsafe {
        (*h).strong = if in_region { 1 | REGION_BIT } else { 1 };
        (*h).weak = 0;
        // Store the rounded size in units so release recovers the exact
        // byte size (and pool class). Pathologically large blocks store a
        // sentinel and record their byte size in the side table. Region
        // objects are freed by slab, so they never consult `size_u`, but
        // recording it keeps the header uniform.
        if units < SIZE_OVERSIZED as usize {
            (*h).size_u = units as u16;
        } else {
            (*h).size_u = SIZE_OVERSIZED;
            if !in_region {
                oversized_register(base, total);
            }
        }
        (*h).meta = meta;
    }
    RC_LIVE.fetch_add(1, Ordering::Relaxed);
    unsafe { base.add(RC_HEADER_SIZE) }
}

/// Shared, pinned singleton for a payload-less enum variant with discriminant
/// `tag`. Unit variants carry no fields and are only read (the match reads the
/// tag at offset 0), so every `Tree::Leaf`-style construction shares one heap
/// node instead of allocating per use — a large RAM win for recursive enums
/// (full binary trees are ~half leaves). The node is allocated GLOBALLY (never
/// in an arena region, which would free it wholesale at pop and leave the
/// cached pointer dangling), and its base reference pins it for the process
/// lifetime; callers treat the pointer as a borrow, so the enclosing
/// aggregate's store retains it and teardown releases it (balanced).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_enum_unit(tag: i64) -> *mut u8 {
    use std::sync::atomic::{AtomicPtr, Ordering};
    const N: usize = 256;
    static SINGLETONS: [AtomicPtr<u8>; N] = [const { AtomicPtr::new(std::ptr::null_mut()) }; N];

    // Global (non-region) allocation of a tag-only RC node, pinned at strong=1.
    let alloc_global = |tag: i64| -> *mut u8 {
        let total = size_class(8usize.saturating_add(RC_HEADER_SIZE)).0;
        let Ok(layout) = Layout::from_size_align(total, RC_ALIGN) else {
            return std::ptr::null_mut();
        };
        let base = unsafe { alloc_zeroed(layout) };
        if base.is_null() {
            return std::ptr::null_mut();
        }
        let units = total / SIZE_UNIT;
        unsafe {
            let h = base as *mut RcHeader;
            (*h).strong = 1;
            (*h).weak = 0;
            (*h).size_u = if units < SIZE_OVERSIZED as usize {
                units as u16
            } else {
                SIZE_OVERSIZED
            };
            (*h).meta = std::ptr::null();
            let payload = base.add(RC_HEADER_SIZE);
            (payload as *mut i64).write(tag);
            RC_LIVE.fetch_add(1, Ordering::Relaxed);
            payload
        }
    };

    if !(0..N as i64).contains(&tag) {
        // Out-of-range discriminant: fall back to a fresh global node.
        return alloc_global(tag);
    }
    let slot = &SINGLETONS[tag as usize];
    let existing = slot.load(Ordering::Acquire);
    if !existing.is_null() {
        return existing;
    }
    let fresh = alloc_global(tag);
    if fresh.is_null() {
        return fresh;
    }
    match slot.compare_exchange(
        std::ptr::null_mut(),
        fresh,
        Ordering::AcqRel,
        Ordering::Acquire,
    ) {
        Ok(_) => fresh,
        Err(winner) => {
            // Lost the race — drop the redundant node, share the winner's.
            unsafe { gos_rt_rc_release(fresh) };
            winner
        }
    }
}

/// Increment the strong count of an RC object. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_retain(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    if unsafe { crate::c_abi::string::is_gos_string(payload.cast()) } {
        unsafe { crate::c_abi::string::gos_rt_str_retain(payload.cast()) };
        return;
    }
    let h = unsafe { header_ptr(payload) };
    // Region objects are owned by their arena and freed wholesale at pop;
    // their count is meaningless, so retain is a no-op (and must not run
    // the mask below, which would clobber REGION_BIT).
    if unsafe { is_region(h) } {
        return;
    }
    // Bump the count in the low 28 bits, leaving the collector flag bits
    // intact; a new reference also colors the object black ("in use"),
    // cancelling any pending purple/gray mark from a prior decrement.
    let s = unsafe { (*h).strong };
    let bumped = (s & STRONG_COUNT_MASK)
        .saturating_add(1)
        .min(STRONG_COUNT_MASK);
    // Preserve the buffered bit, clear the color to black, set the new count.
    unsafe { (*h).strong = (s & BUFFERED_BIT) | bumped };
}

/// Decrement the strong count; at zero, release RC-pointer children
/// (iteratively, to bound stack depth on deep structures) and free the
/// block (unless a weak ref still observes it). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_release(payload: *mut u8) {
    unsafe { rc_release_impl(payload) };
}

/// Create a weak reference from a strong-held payload: increment the weak
/// count and return the same pointer (now carrying weak ownership). Does not
/// touch the strong count. Null-safe (returns null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_downgrade(payload: *mut u8) -> *mut u8 {
    if payload.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { header_ptr(payload) };
    unsafe { (*h).weak = (*h).weak.saturating_add(1) };
    payload
}

/// Increment the weak count (copying a `Weak`). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_weak_retain(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    let h = unsafe { header_ptr(payload) };
    unsafe { (*h).weak = (*h).weak.saturating_add(1) };
}

/// Decrement the weak count; if both strong and weak counts are now zero,
/// free the (already payload-destroyed) allocation. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_weak_release(payload: *mut u8) {
    if payload.is_null() {
        return;
    }
    let h = unsafe { header_ptr(payload) };
    let next = unsafe { (*h).weak }.saturating_sub(1);
    unsafe { (*h).weak = next };
    if next == 0 {
        unsafe { try_reclaim(payload) };
    }
}

/// Attempt to obtain a strong reference from a weak one. If the referent is
/// still alive (`strong > 0`), increment the strong count and return the
/// payload; otherwise return null (the `None` shape). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_weak_upgrade(payload: *mut u8) -> *mut u8 {
    if payload.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { header_ptr(payload) };
    let count = unsafe { strong_count(h) };
    if count == 0 {
        return std::ptr::null_mut();
    }
    // A new strong reference revives the object: bump the count and color it
    // black so a concurrent cycle scan treats it as live.
    unsafe { set_strong_count(h, count.saturating_add(1)) };
    unsafe { set_color(h, COLOR_BLACK) };
    payload
}

/// Upgrade a weak reference to `Option<T>` for the language-level
/// `w.upgrade()`. Returns a boxed `GosResult` discriminated as `Some`
/// (disc 0) carrying the payload pointer when the referent is still
/// alive (`strong > 0`), or `None` (disc 1) once it has been reclaimed.
///
/// Unlike [`gos_rt_rc_weak_upgrade`] this does NOT bump the strong
/// count: the `Some` payload is an interior borrow valid for the
/// duration of the caller's match arm (whose own live strong reference
/// keeps the object alive). Bumping the count here would keep dead-once
/// objects alive across later `upgrade()` calls — silently turning a
/// `None` into a `Some` — so the borrow model is the sound one for the
/// synchronous match/if-let idiom. Null-safe (returns `None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_weak_upgrade_opt(payload: *mut u8) -> i128 {
    let alive = if payload.is_null() {
        false
    } else {
        let h = unsafe { header_ptr(payload) };
        (unsafe { strong_count(h) }) > 0
    };
    if alive {
        crate::c_abi::vec::pack_result(0, payload as i64)
    } else {
        crate::c_abi::vec::pack_result(1, 0)
    }
}

/// Free a block's allocation only when nothing pins it: no strong refs, no
/// weak refs, and not awaiting the cycle collector. The single funnel every
/// release path goes through, so each block is freed exactly once.
unsafe fn try_reclaim(payload: *mut u8) {
    let h = unsafe { header_ptr(payload) };
    if unsafe { strong_count(h) } == 0 && unsafe { (*h).weak } == 0 && !unsafe { is_buffered(h) } {
        unsafe { free_block(payload) };
    }
}

/// Iterative release: maintain an explicit worklist of payloads whose
/// strong count must be decremented. When a count reaches zero, release its
/// RC-pointer children and reclaim the block. A non-zero result may be a
/// cycle root, so it is buffered for the collector. Iterative (not
/// recursive) so a deep tree/list cannot overflow the runtime's own stack.
unsafe fn rc_release_impl(root: *mut u8) {
    if root.is_null() {
        return;
    }
    if unsafe { crate::c_abi::string::is_gos_string(root.cast()) } {
        unsafe { crate::c_abi::string::gos_rt_str_free(root.cast()) };
        return;
    }
    let h = unsafe { header_ptr(root) };
    // Region objects are freed wholesale at region pop — never individually.
    // Skipping the decrement-and-walk here is exactly what eliminates the
    // per-node teardown cost for `region { … }` allocations.
    if unsafe { is_region(h) } {
        return;
    }
    let count = unsafe { strong_count(h) };
    let next = count.saturating_sub(1);
    unsafe { set_strong_count(h, next) };
    if next != 0 {
        // Survived the decrement: a possible cycle root.
        unsafe { possible_root(root) };
        return;
    }
    unsafe { set_color(h, COLOR_BLACK) };
    let meta = unsafe { (*h).meta };
    // Leaf fast path: a childless object (no RC-pointer children, the
    // overwhelming common case — every enum payload-free variant, every
    // leaf node) is reclaimed directly. This avoids touching the worklist
    // at all, so the dominant release shape never allocates or recurses.
    if meta.is_null() {
        unsafe { try_reclaim(root) };
        return;
    }
    // Internal node: walk children iteratively (bounds stack depth on deep
    // structures). Reuse a thread-local worklist buffer — allocating a
    // fresh `Vec` per release call was a malloc/free on every node teardown
    // (millions, for tree workloads), dwarfing the actual reclamation.
    RELEASE_WORKLIST.with(|cell| {
        let mut worklist = cell.borrow_mut();
        worklist.clear();
        unsafe { collect_children(root, meta, &mut worklist) };
        unsafe { try_reclaim(root) };
        while let Some(payload) = worklist.pop() {
            if payload.is_null() {
                continue;
            }
            let h = unsafe { header_ptr(payload) };
            let count = unsafe { strong_count(h) };
            let next = count.saturating_sub(1);
            unsafe { set_strong_count(h, next) };
            if next != 0 {
                unsafe { possible_root(payload) };
                continue;
            }
            unsafe { set_color(h, COLOR_BLACK) };
            let meta = unsafe { (*h).meta };
            unsafe { collect_children(payload, meta, &mut worklist) };
            unsafe { try_reclaim(payload) };
        }
    });
}

thread_local! {
    /// Reused scratch buffer for the iterative release walk. A fresh `Vec`
    /// per `rc_release_impl` call was a malloc/free on every node teardown.
    /// Not re-entered: the walk calls no user code.
    static RELEASE_WORKLIST: std::cell::RefCell<Vec<*mut u8>> =
        std::cell::RefCell::new(Vec::with_capacity(64));
}

/// Call `f` for each non-null RC-pointer child of `payload`, per its
/// type-meta blob. Walks the flat `[i64]` blob documented above. The single
/// edge-traversal primitive shared by the RC release walk, the cycle
/// collector's trial-deletion, and the GC mark — one edge map, three
/// consumers.
unsafe fn visit_rc_children(payload: *mut u8, mut f: impl FnMut(*mut u8)) {
    let meta = unsafe { (*header_ptr(payload)).meta };
    if meta.is_null() {
        return;
    }
    let kind = unsafe { *meta };
    let variant_count = unsafe { *meta.add(1) };
    // Only Enum and Struct carry child layouts today. String / Vec / Map
    // / Closure layouts are wired in a later phase and never reach here.
    if kind != RC_KIND_ENUM && kind != RC_KIND_STRUCT {
        return;
    }
    let target_disc = if kind == RC_KIND_ENUM {
        unsafe { *(payload as *const i64) }
    } else {
        0
    };
    let mut idx: usize = 2;
    for _ in 0..variant_count.max(0) {
        let disc = unsafe { *meta.add(idx) };
        let child_count = unsafe { *meta.add(idx + 1) };
        let matches = kind == RC_KIND_STRUCT || disc == target_disc;
        if matches {
            for j in 0..child_count.max(0) {
                let word = unsafe { *meta.add(idx + 2 + j as usize) };
                let slot = unsafe { payload.add((word as usize) * 8) as *const *mut u8 };
                let child = unsafe { *slot };
                if !child.is_null() {
                    f(child);
                }
            }
            return;
        }
        idx += 2 + child_count.max(0) as usize;
    }
}

/// Read `payload`'s RC-pointer children and push them onto `worklist` for
/// release. Must be called before the block is freed.
unsafe fn collect_children(payload: *mut u8, _meta: *const i64, worklist: &mut Vec<*mut u8>) {
    unsafe { visit_rc_children(payload, |child| worklist.push(child)) };
}

/// Free an RC block's underlying allocation. Called when the block is no
/// longer observed by any strong *or* weak reference. The payload's children
/// must already have been released (at the strong→0 transition). The byte
/// size is recovered from the header's `size_u` (or the oversized side table).
unsafe fn free_block(payload: *mut u8) {
    let h = unsafe { header_ptr(payload) };
    let units = unsafe { (*h).size_u };
    let base = h as *mut u8;
    let total = if units == SIZE_OVERSIZED {
        oversized_take(base)
    } else {
        units as usize * SIZE_UNIT
    };
    RC_LIVE.fetch_sub(1, Ordering::Relaxed);
    // Straight back to the system allocator — see `gos_rt_rc_alloc` for why a
    // custom slab/pool is not used (measured net-neutral).
    if let Ok(layout) = Layout::from_size_align(total, RC_ALIGN) {
        unsafe { dealloc(base, layout) };
    }
}

// ---------------------------------------------------------------
// Trial-deletion cycle collector (Bacon-Rajan, synchronous).
// ---------------------------------------------------------------
//
// All four phases trace the RC object graph from the candidate buffer via
// `visit_rc_children` (the same meta-blob edge map the release walk uses).
// They are iterative (explicit stacks) so a large cyclic component cannot
// overflow the runtime stack. They never inspect a stack frame, register,
// or spill slot, so they are sound under `-O3`.

/// MarkGray: trial-delete internal references by decrementing the strong
/// count of every child reachable from `root`, marking the subgraph gray.
/// After this, a node's count reflects only references from *outside* the
/// traced subgraph.
unsafe fn mark_gray(root: *mut u8) {
    let mut stack = vec![root];
    while let Some(s) = stack.pop() {
        let h = unsafe { header_ptr(s) };
        if unsafe { color_of(h) } == COLOR_GRAY {
            continue;
        }
        unsafe { set_color(h, COLOR_GRAY) };
        unsafe {
            visit_rc_children(s, |t| {
                let th = header_ptr(t);
                set_strong_count(th, strong_count(th).saturating_sub(1));
                stack.push(t);
            });
        }
    }
}

/// Scan: any gray node still holding an external reference (count > 0) is
/// live — restore its subgraph to black. Gray nodes that reached count 0
/// are cyclic garbage — paint them white and recurse.
unsafe fn scan(root: *mut u8) {
    let mut stack = vec![root];
    while let Some(s) = stack.pop() {
        let h = unsafe { header_ptr(s) };
        if unsafe { color_of(h) } != COLOR_GRAY {
            continue;
        }
        if unsafe { strong_count(h) } > 0 {
            unsafe { scan_black(s) };
        } else {
            unsafe { set_color(h, COLOR_WHITE) };
            unsafe { visit_rc_children(s, |t| stack.push(t)) };
        }
    }
}

/// ScanBlack: restore the counts MarkGray trial-deleted for a live subgraph
/// and repaint it black.
unsafe fn scan_black(root: *mut u8) {
    let mut stack = vec![root];
    while let Some(s) = stack.pop() {
        let h = unsafe { header_ptr(s) };
        if unsafe { color_of(h) } == COLOR_BLACK {
            continue;
        }
        unsafe { set_color(h, COLOR_BLACK) };
        unsafe {
            visit_rc_children(s, |t| {
                let th = header_ptr(t);
                set_strong_count(th, strong_count(th).saturating_add(1));
                if color_of(th) != COLOR_BLACK {
                    stack.push(t);
                }
            });
        }
    }
}

/// CollectWhite: free the confirmed garbage cycle. White nodes are gathered
/// (repainting black to dedupe), then their allocations reclaimed — unless a
/// weak reference still pins one, in which case the payload is already dead
/// and the block lingers for the last weak release.
unsafe fn collect_white(root: *mut u8) {
    let mut stack = vec![root];
    let mut to_free: Vec<*mut u8> = Vec::new();
    while let Some(s) = stack.pop() {
        let h = unsafe { header_ptr(s) };
        if unsafe { color_of(h) } != COLOR_WHITE || unsafe { is_buffered(h) } {
            continue;
        }
        unsafe { set_color(h, COLOR_BLACK) };
        unsafe { visit_rc_children(s, |t| stack.push(t)) };
        to_free.push(s);
    }
    for s in to_free {
        let h = unsafe { header_ptr(s) };
        if unsafe { strong_count(h) } == 0
            && unsafe { (*h).weak } == 0
            && !unsafe { is_buffered(h) }
        {
            CYCLES_FREED.fetch_add(1, Ordering::Relaxed);
            unsafe { free_block(s) };
        }
    }
}

/// Run one synchronous trial-deletion collection over the current candidate
/// buffer. Reclaims unreachable cyclic RC garbage; live data and acyclic
/// garbage are untouched. Cost is proportional to the subgraph reachable
/// from the candidates, not the whole heap.
unsafe fn collect_cycles() {
    let roots: Vec<*mut u8> = ROOTS.with(|r| std::mem::take(&mut *r.borrow_mut()));
    if roots.is_empty() {
        return;
    }
    // MarkRoots: trace gray from each still-purple candidate; drop stale
    // candidates (revived to black, or already dead awaiting free).
    let mut scan_roots: Vec<*mut u8> = Vec::new();
    let mut dead: Vec<*mut u8> = Vec::new();
    for s in roots {
        let h = unsafe { header_ptr(s) };
        if unsafe { color_of(h) } == COLOR_PURPLE {
            unsafe { mark_gray(s) };
            scan_roots.push(s);
        } else {
            unsafe { set_buffered(h, false) };
            // A candidate whose count later fell to 0 *and* is black is
            // acyclic garbage whose children were already released; reclaim
            // it. A gray candidate is mid-trace (reachable from another
            // purple root) and must be left to scan/collect — never freed
            // here, or a live cycle member would be dropped.
            if unsafe { color_of(h) } == COLOR_BLACK && unsafe { strong_count(h) } == 0 {
                dead.push(s);
            }
        }
    }
    for &s in &scan_roots {
        unsafe { scan(s) };
    }
    for s in scan_roots {
        let h = unsafe { header_ptr(s) };
        unsafe { set_buffered(h, false) };
        if unsafe { color_of(h) } == COLOR_WHITE {
            unsafe { collect_white(s) };
        }
    }
    // Reclaim the dead leftovers last: count 0 means nothing references them,
    // so no MarkGray traversal touched them.
    for s in dead {
        unsafe { try_reclaim(s) };
    }
}

/// Run the cycle collector now, reclaiming any unreachable cyclic RC
/// garbage accumulated in the candidate buffer. Exposed to user code as
/// `runtime::collect_cycles()`; also triggered automatically when the
/// candidate buffer crosses its threshold.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_collect_cycles() {
    unsafe { collect_cycles() };
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, PoisonError};

    // `RC_LIVE` is process-global; tests that assert exact live-count
    // deltas must not run concurrently with each other's allocations.
    static COUNT_LOCK: Mutex<()> = Mutex::new(());

    fn count_guard() -> std::sync::MutexGuard<'static, ()> {
        COUNT_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocate via the runtime entry and write a discriminant into the
    /// payload's first word.
    unsafe fn alloc_with_disc(payload_words: usize, disc: i64, meta: *const i64) -> *mut u8 {
        let p = unsafe { gos_rt_rc_alloc((payload_words * 8) as u64, meta) };
        assert!(!p.is_null());
        unsafe { *(p as *mut i64) = disc };
        p
    }

    unsafe fn set_child(parent: *mut u8, word: usize, child: *mut u8) {
        let slot = unsafe { parent.add(word * 8) as *mut *mut u8 };
        unsafe { *slot = child };
    }

    unsafe fn strong_of(payload: *mut u8) -> usize {
        unsafe { strong_count(header_ptr(payload)) as usize }
    }

    // Struct node with one i64 field (word 0) and one RC-pointer child
    // (word 1): kind=STRUCT, V=1, [disc0 cc1 off1].
    fn node_meta() -> Vec<i64> {
        vec![RC_KIND_STRUCT, 1, 0, 1, 1]
    }

    /// Set `parent`'s child slot (word 1) to `child` and retain it, as the
    /// compiled tier's `gos_store` does when an object gains a child edge.
    unsafe fn link(parent: *mut u8, child: *mut u8) {
        unsafe { set_child(parent, 1, child) };
        unsafe { gos_rt_rc_retain(child) };
    }

    /// Move `child` into `parent`'s slot: set the edge without a retain,
    /// transferring the existing reference (the unique-ownership shape, like
    /// `Node { child: existing_b }` where `b` is used once and not aliased).
    unsafe fn move_child(parent: *mut u8, child: *mut u8) {
        unsafe { set_child(parent, 1, child) };
    }

    /// Drain any candidates left buffered by earlier tests on this thread so
    /// each cycle test starts from a clean candidate buffer (`ROOTS` is
    /// thread-local and tests share worker threads).
    fn fresh_cycle_state() {
        unsafe { gos_rt_collect_cycles() };
    }

    #[test]
    fn two_node_cycle_is_collected() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let freed_base = rc_cycles_freed();
        let meta = node_meta();
        unsafe {
            let a = gos_rt_rc_alloc(16, meta.as_ptr());
            let b = gos_rt_rc_alloc(16, meta.as_ptr());
            link(a, b);
            link(b, a);
            // Drop both external handles: a pure-RC heap now leaks the cycle.
            gos_rt_rc_release(a);
            gos_rt_rc_release(b);
            assert_eq!(rc_live_count(), base + 2, "cycle leaks under plain RC");
            gos_rt_collect_cycles();
        }
        assert_eq!(rc_live_count(), base, "cycle collector reclaims the cycle");
        assert_eq!(rc_cycles_freed(), freed_base + 2);
    }

    #[test]
    fn self_cycle_is_collected() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            let a = gos_rt_rc_alloc(16, meta.as_ptr());
            link(a, a);
            gos_rt_rc_release(a);
            assert_eq!(rc_live_count(), base + 1);
            gos_rt_collect_cycles();
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn externally_referenced_cycle_survives_collection() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            let a = gos_rt_rc_alloc(16, meta.as_ptr());
            let b = gos_rt_rc_alloc(16, meta.as_ptr());
            link(a, b);
            link(b, a);
            // An outside owner keeps `a` (and transitively the cycle) live.
            gos_rt_rc_retain(a);
            gos_rt_rc_release(a);
            gos_rt_rc_release(b);
            gos_rt_collect_cycles();
            assert_eq!(rc_live_count(), base + 2, "live cycle is not collected");
            assert_eq!(strong_of(a), 2, "counts restored after trial deletion");
            assert_eq!(strong_of(b), 1);
            // Drop the external owner: now it is garbage and collectable.
            gos_rt_rc_release(a);
            gos_rt_collect_cycles();
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn uniquely_owned_acyclic_release_never_buffers() {
        let _g = count_guard();
        fresh_cycle_state();
        let meta = node_meta();
        unsafe {
            let b = gos_rt_rc_alloc(16, meta.as_ptr());
            let a = gos_rt_rc_alloc(16, meta.as_ptr());
            // `a` uniquely owns `b` (the reference was moved in, not aliased):
            // dropping `a` frees both straight at count 0, so neither ever
            // survives a decrement and nothing is buffered — the collector
            // stays a no-op on the unique-ownership (benchmark) shape.
            move_child(a, b);
            gos_rt_rc_release(a);
            let buffered = ROOTS.with(|r| r.borrow().len());
            assert_eq!(buffered, 0, "unique-ownership drop must not buffer");
        }
    }

    #[test]
    fn region_allocs_are_freed_wholesale_at_pop() {
        let _g = count_guard();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            gos_rt_region_push();
            assert!(region_active());
            let mut ptrs = Vec::new();
            for _ in 0..1000 {
                let p = gos_rt_rc_alloc(16, meta.as_ptr());
                assert!(!p.is_null());
                assert!(is_region(header_ptr(p)), "region alloc must tag REGION_BIT");
                ptrs.push(p);
            }
            assert_eq!(rc_live_count(), base + 1000);
            // retain/release on region objects are no-ops: they neither free
            // nor clobber the REGION bit.
            gos_rt_rc_retain(ptrs[0]);
            gos_rt_rc_release(ptrs[0]);
            assert!(is_region(header_ptr(ptrs[0])));
            assert_eq!(
                rc_live_count(),
                base + 1000,
                "region objects not freed early"
            );
            gos_rt_region_pop();
            assert!(!region_active());
        }
        assert_eq!(rc_live_count(), base, "pop frees the whole region");
    }

    #[test]
    fn region_tree_freed_without_per_node_teardown() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            gos_rt_region_push();
            // Build a parent that owns a child entirely inside the region.
            let child = gos_rt_rc_alloc(16, meta.as_ptr());
            let parent = gos_rt_rc_alloc(16, meta.as_ptr());
            move_child(parent, child);
            // "Consume" the parent (count would hit zero for a heap object,
            // triggering a teardown walk). For a region object this is a
            // no-op — the child is NOT freed here.
            gos_rt_rc_release(parent);
            assert_eq!(rc_live_count(), base + 2, "region release must not free");
            let buffered = ROOTS.with(|r| r.borrow().len());
            assert_eq!(buffered, 0, "region objects never enter the cycle buffer");
            gos_rt_region_pop();
        }
        assert_eq!(
            rc_live_count(),
            base,
            "pop reclaims parent + child together"
        );
    }

    #[test]
    fn region_oversized_alloc_gets_its_own_slab() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            gos_rt_region_push();
            // Larger than the default slab — must still allocate, on its own slab.
            let big = gos_rt_rc_alloc((REGION_SLAB_BYTES as u64) * 2, std::ptr::null());
            assert!(!big.is_null());
            assert!(is_region(header_ptr(big)));
            gos_rt_region_pop();
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn shared_acyclic_node_is_not_collected_while_referenced() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            // Two parents share one child (a diamond, no cycle). The child is
            // owned only by the parents (its construction handle is released).
            let child = gos_rt_rc_alloc(16, meta.as_ptr());
            let p1 = gos_rt_rc_alloc(16, meta.as_ptr());
            let p2 = gos_rt_rc_alloc(16, meta.as_ptr());
            link(p1, child);
            link(p2, child);
            gos_rt_rc_release(child);
            // Dropping one parent decrements the shared child to a non-zero
            // count → buffered as a candidate, but it is live and survives.
            gos_rt_rc_release(p1);
            gos_rt_collect_cycles();
            assert_eq!(rc_live_count(), base + 2, "shared live child survives");
            assert_eq!(strong_of(child), 1);
            // Drop the last parent: child reaches 0 (deferred while buffered),
            // reclaimed at the next collection.
            gos_rt_rc_release(p2);
            gos_rt_collect_cycles();
        }
        assert_eq!(rc_live_count(), base);
    }

    unsafe fn weak_of(payload: *mut u8) -> usize {
        unsafe { (*header_ptr(payload)).weak as usize }
    }

    #[test]
    fn downgrade_increments_weak_not_strong() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            let w = gos_rt_rc_downgrade(p);
            assert_eq!(w, p, "downgrade returns the same payload pointer");
            assert_eq!(strong_of(p), 1);
            assert_eq!(weak_of(p), 1);
            // Strong release destroys the payload but the block lingers
            // because a weak ref still observes it.
            gos_rt_rc_release(p);
            assert_eq!(rc_live_count(), base + 1, "block lingers while weak > 0");
            assert_eq!(strong_of(p), 0);
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base, "block freed once weak hits 0");
    }

    #[test]
    fn upgrade_while_alive_returns_payload_and_bumps_strong() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_downgrade(p);
            let up = gos_rt_rc_weak_upgrade(p);
            assert_eq!(up, p, "upgrade of a live referent yields the payload");
            assert_eq!(strong_of(p), 2, "upgrade adds a strong ref");
            gos_rt_rc_release(p);
            gos_rt_rc_release(p);
            assert_eq!(rc_live_count(), base + 1, "lingers on the weak ref");
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn upgrade_opt_boxes_some_when_alive_without_bumping_strong() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_downgrade(p);
            let opt = gos_rt_rc_weak_upgrade_opt(p);
            assert_eq!(
                crate::c_abi::vec::gos_rt_result_disc(opt),
                0,
                "alive referent is Some"
            );
            assert_eq!(
                crate::c_abi::vec::gos_rt_result_payload(opt),
                p as i64,
                "Some carries the payload pointer"
            );
            assert_eq!(
                strong_of(p),
                1,
                "upgrade_opt does not bump the strong count"
            );
            gos_rt_rc_release(p);
            assert_eq!(rc_live_count(), base + 1, "lingers on the weak ref");
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn upgrade_opt_boxes_none_after_last_strong_release() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_downgrade(p);
            gos_rt_rc_release(p);
            let opt = gos_rt_rc_weak_upgrade_opt(p);
            assert_eq!(
                crate::c_abi::vec::gos_rt_result_disc(opt),
                1,
                "dead referent is None"
            );
            assert_eq!(crate::c_abi::vec::gos_rt_result_payload(opt), 0);
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn upgrade_after_last_strong_release_returns_null() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_downgrade(p);
            gos_rt_rc_release(p);
            assert_eq!(rc_live_count(), base + 1);
            let up = gos_rt_rc_weak_upgrade(p);
            assert!(up.is_null(), "upgrade of a dead referent yields null");
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn allocation_lingers_until_weak_count_zero() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_downgrade(p);
            gos_rt_rc_weak_retain(p);
            assert_eq!(weak_of(p), 2);
            gos_rt_rc_release(p);
            assert_eq!(rc_live_count(), base + 1);
            gos_rt_rc_weak_release(p);
            assert_eq!(rc_live_count(), base + 1, "still one outstanding weak");
            gos_rt_rc_weak_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn strong_release_with_outstanding_weak_still_releases_children() {
        let _g = count_guard();
        let base = rc_live_count();
        let meta = tree_meta();
        unsafe {
            let l0 = alloc_with_disc(1, 0, meta.as_ptr());
            let l1 = alloc_with_disc(1, 0, meta.as_ptr());
            let node = alloc_with_disc(3, 1, meta.as_ptr());
            set_child(node, 1, l0);
            set_child(node, 2, l1);
            gos_rt_rc_downgrade(node);
            assert_eq!(rc_live_count(), base + 3);
            // Last strong release frees the two children immediately; the
            // node block lingers for the weak observer.
            gos_rt_rc_release(node);
            assert_eq!(rc_live_count(), base + 1, "children freed, node lingers");
            gos_rt_rc_weak_release(node);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn weak_funcs_are_null_safe() {
        unsafe {
            assert!(gos_rt_rc_downgrade(std::ptr::null_mut()).is_null());
            assert!(gos_rt_rc_weak_upgrade(std::ptr::null_mut()).is_null());
            gos_rt_rc_weak_retain(std::ptr::null_mut());
            gos_rt_rc_weak_release(std::ptr::null_mut());
        }
    }

    #[test]
    fn oversized_block_round_trips_through_side_table() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            // Payload large enough to exceed the u16 size-unit range, forcing
            // the oversized side-table path.
            let big = (u16::MAX as u64) * CLASS_STEP as u64 + 4096;
            let p = gos_rt_rc_alloc(big, std::ptr::null());
            assert!(!p.is_null());
            assert_eq!(strong_of(p), 1);
            gos_rt_rc_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn alloc_starts_at_strong_one_and_tracks_live() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            assert!(!p.is_null());
            assert_eq!(strong_of(p), 1);
            assert_eq!(rc_live_count(), base + 1);
            gos_rt_rc_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn retain_and_release_adjust_strong_count() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_retain(p);
            assert_eq!(strong_of(p), 2);
            gos_rt_rc_release(p);
            assert_eq!(strong_of(p), 1);
            assert_eq!(rc_live_count(), base + 1);
            gos_rt_rc_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn null_safe() {
        unsafe {
            gos_rt_rc_retain(std::ptr::null_mut());
            gos_rt_rc_release(std::ptr::null_mut());
        }
    }

    #[test]
    fn leaf_with_no_meta_frees_cleanly() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            let p = gos_rt_rc_alloc(8, std::ptr::null());
            gos_rt_rc_release(p);
        }
        assert_eq!(rc_live_count(), base);
    }

    // Flat-blob meta for enum `Tree`: Leaf (disc 0, no children),
    // Node (disc 1, children at payload words 1 and 2).
    //   kind=ENUM, V=2, [disc0 cc0] [disc1 cc2 off1 off2]
    fn tree_meta() -> Vec<i64> {
        vec![
            RC_KIND_ENUM,
            2,
            /* Leaf */ 0,
            0,
            /* Node */ 1,
            2,
            1,
            2,
        ]
    }

    #[test]
    fn release_frees_recursive_tree() {
        let _g = count_guard();
        let base = rc_live_count();
        let meta = tree_meta();
        unsafe {
            let l0 = alloc_with_disc(1, 0, meta.as_ptr());
            let l1 = alloc_with_disc(1, 0, meta.as_ptr());
            let node = alloc_with_disc(3, 1, meta.as_ptr());
            set_child(node, 1, l0);
            set_child(node, 2, l1);
            assert_eq!(rc_live_count(), base + 3);
            gos_rt_rc_release(node);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn shared_child_survives_until_last_owner() {
        let _g = count_guard();
        let base = rc_live_count();
        let meta = tree_meta();
        unsafe {
            let shared = alloc_with_disc(1, 0, meta.as_ptr());
            let l1 = alloc_with_disc(1, 0, meta.as_ptr());
            let node = alloc_with_disc(3, 1, meta.as_ptr());
            set_child(node, 1, shared);
            set_child(node, 2, l1);
            // A second owner of `shared`.
            gos_rt_rc_retain(shared);
            assert_eq!(strong_of(shared), 2);

            gos_rt_rc_release(node);
            // node + l1 freed; shared survives with one owner.
            assert_eq!(rc_live_count(), base + 1);
            assert_eq!(strong_of(shared), 1);

            gos_rt_rc_release(shared);
        }
        assert_eq!(rc_live_count(), base);
    }

    #[test]
    fn deep_list_release_is_iterative() {
        // Struct list node: one child pointer at payload word 0.
        //   kind=STRUCT, V=1, [disc0 cc1 off0]
        let meta: Vec<i64> = vec![RC_KIND_STRUCT, 1, 0, 1, 0];

        let _g = count_guard();
        let base = rc_live_count();
        let depth = 1_000_000usize;
        unsafe {
            let mut head = std::ptr::null_mut::<u8>();
            for _ in 0..depth {
                let node = gos_rt_rc_alloc(8, meta.as_ptr());
                assert!(!node.is_null());
                set_child(node, 0, head);
                head = node;
            }
            assert_eq!(rc_live_count(), base + depth);
            // Recursive release would overflow the stack here.
            gos_rt_rc_release(head);
        }
        assert_eq!(rc_live_count(), base);
    }
}
