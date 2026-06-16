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

#[cfg(any(tsan, miri, fuzzing))]
use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::sync::atomic::{AtomicU32, AtomicUsize, Ordering, fence};

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
// optimized LLVM.
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
    /// Strong reference count (low 28 bits) plus the collector flag bits.
    /// Starts at 1 on allocation.
    pub strong: u32,
    /// Weak reference count (saturating). The allocation outlives `strong ==
    /// 0` whenever this is non-zero, so a `Weak` can probe liveness without
    /// reading freed memory. `u8`: an object observed by 255 simultaneous
    /// weak handles saturates and its allocation is never reclaimed — a
    /// leak, never a corruption — which frees the byte the enum
    /// discriminant now occupies.
    pub weak: u8,
    /// Enum discriminant. Lives in the header (codegen reads/writes the
    /// byte at `payload - 3`) so the payload holds only the variant's
    /// fields: a `Node(i64, Box, Box)` is 8 + 24 = 32 bytes and a
    /// two-pointer `Node(Box, Box)` is 8 + 16 = 24. Enums are capped at
    /// 256 variants by the type checker. Zero (and unread) for
    /// non-enum RC objects.
    pub disc: u8,
    /// Interned id of the child-layout descriptor blob (see
    /// `meta_intern` / `meta_of`); 0 for leaf objects with no
    /// RC-pointer children. The allocation size is not recorded at all —
    /// blocks are freed with `mi_free`, which needs only the base
    /// pointer.
    pub meta_id: u16,
}

/// 8-byte alignment is hard-coded across the runtime ABI; all payload
/// fields are word-sized and word-aligned.
pub const RC_ALIGN: usize = 8;

/// Size of [`RcHeader`], rounded to the runtime alignment. The payload
/// begins this many bytes after the allocation base.
pub const RC_HEADER_SIZE: usize = std::mem::size_of::<RcHeader>();

// The header must stay 8 bytes: every heap object pays it, so growth is a
// direct per-object RAM regression. The weak/cycle/meta fields are packed
// into the existing 8 bytes (see the field docs), never added on top.
const _: () = assert!(RC_HEADER_SIZE == 8, "RcHeader must remain 8 bytes");

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
    RC_KIND_CLOSURE, RC_KIND_ENUM, RC_KIND_MAP, RC_KIND_STRING, RC_KIND_STRUCT,
    RC_KIND_STRUCT_GUARDED, RC_KIND_VEC,
};

/// Count of live RC objects (allocated minus freed). Two relaxed atomic
/// RMWs per object lifecycle are measurable on tree workloads (~134M
/// increments for a binary-trees run), so production counting is gated:
/// always on in this crate's test build (the unit tests assert on it),
/// otherwise only when `GOS_RC_DEBUG` is set (the flag that also prints
/// `RC_LIVE_AT_EXIT`).
static RC_LIVE: AtomicUsize = AtomicUsize::new(0);

#[cfg(not(test))]
static RC_LIVE_ENABLED: std::sync::LazyLock<bool> =
    std::sync::LazyLock::new(|| std::env::var_os("GOS_RC_DEBUG").is_some());

#[inline]
fn rc_live_enabled() -> bool {
    #[cfg(test)]
    {
        true
    }
    #[cfg(not(test))]
    {
        *RC_LIVE_ENABLED
    }
}

/// Number of RC-managed objects currently alive. Diagnostic hook;
/// meaningful only when counting is enabled (tests / `GOS_RC_DEBUG`).
pub fn rc_live_count() -> usize {
    RC_LIVE.load(Ordering::Relaxed)
}

#[inline]
fn rc_live_inc() {
    if rc_live_enabled() {
        RC_LIVE.fetch_add(1, Ordering::Relaxed);
    }
}

#[inline]
fn rc_live_dec() {
    if rc_live_enabled() {
        RC_LIVE.fetch_sub(1, Ordering::Relaxed);
    }
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

/// Low 27 bits of `strong`: the actual strong reference count (134M
/// live refs is unreachable). Bit 27 is [`SHARED_BIT`]; bits 28-31 are
/// the collector flags.
const STRONG_COUNT_MASK: u32 = 0x07FF_FFFF;

/// Bit 27: the object has escaped to another goroutine (sent on a
/// channel, captured by a spawned goroutine, or passed to `go f(...)`).
/// Its strong count is then mutated with **atomic** retain/release so
/// concurrent workers can't tear the count (lost decrement → UAF /
/// double-free). Shared objects are excluded from the per-thread cycle
/// collector — their cycles leak, exactly like Rust's `Arc` (break with
/// weak refs). Set transitively over the reachable RC subgraph at the
/// escape point by [`gos_rt_rc_mark_shared`], before the value is
/// published, and never cleared. Because shared objects skip the
/// collector, the non-atomic accessors (`color_of`, `set_strong_count`,
/// …) only ever run on thread-local (non-shared) objects, so they need
/// no atomics.
const SHARED_BIT: u32 = 1 << 27;

/// Pinned strong count for process-immortal objects (unit-variant
/// singletons). Retain and release skip the count entirely: inside an
/// arena the balancing releases never run (bulk free, no walk), so a
/// counted singleton would grow monotonically and overflow the 28-bit
/// field into the collector flag bits on big workloads.
const STRONG_IMMORTAL: u32 = STRONG_COUNT_MASK;
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

// Only the test asserts read this now — the hot retain/release paths
// check `REGION_BIT` inline off their single atomic `strong` load
// (`inc_strong` / `dec_strong`) to avoid a second read.
#[cfg(test)]
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
    let cur = unsafe { (*h).strong };
    // Immortal pin (unit-variant singletons): the count is never
    // mutated — not by retain/release, not by the cycle collector's
    // trial deletion, not by the release walk's child decrements.
    if cur & STRONG_COUNT_MASK == STRONG_IMMORTAL {
        return;
    }
    let flags = cur & !STRONG_COUNT_MASK;
    unsafe { (*h).strong = flags | (count & STRONG_COUNT_MASK) };
}

// ---------------------------------------------------------------
// Escape-aware (atomic-on-share) strong-count operations.
// ---------------------------------------------------------------
//
// An RC object shared across goroutines (see `SHARED_BIT`) must have
// its strong count mutated atomically, or two workers releasing it
// concurrently tear the count and double-free / leak. Thread-local
// objects keep the cheap non-atomic path. Every read/write below that
// can run on a *shared* object goes through these helpers; the
// non-atomic accessors only ever see thread-local objects (shared ones
// are excluded from the cycle collector, the only other writer).

/// Relaxed atomic load of `strong` — safe to call on shared objects.
#[inline]
unsafe fn load_strong(h: *const RcHeader) -> u32 {
    let a = unsafe { AtomicU32::from_ptr(std::ptr::addr_of!((*h).strong).cast_mut()) };
    a.load(Ordering::Relaxed)
}

/// Whether the object has escaped to another goroutine.
#[inline]
unsafe fn is_shared(h: *const RcHeader) -> bool {
    (unsafe { load_strong(h) }) & SHARED_BIT != 0
}

/// Escape-aware strong increment (the `retain` core). Atomic for shared
/// objects, a plain RMW otherwise. Region / immortal objects untouched.
#[inline]
unsafe fn inc_strong(h: *mut RcHeader) {
    let s = unsafe { load_strong(h) };
    if s & REGION_BIT != 0 || s & STRONG_COUNT_MASK == STRONG_IMMORTAL {
        return;
    }
    if s & SHARED_BIT != 0 {
        // Count is the low 27 bits; +1 cannot reach SHARED_BIT before
        // 134M live refs (unreachable). Relaxed: a retain needs
        // atomicity, not ordering.
        let a = unsafe { AtomicU32::from_ptr(std::ptr::addr_of_mut!((*h).strong)) };
        a.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let bumped = (s & STRONG_COUNT_MASK)
        .saturating_add(1)
        .min(STRONG_COUNT_MASK);
    unsafe { (*h).strong = (s & BUFFERED_BIT) | bumped };
}

/// Result of [`dec_strong`].
struct DecOutcome {
    /// Strong count after the decrement.
    next: u32,
    /// The object had escaped to another goroutine.
    shared: bool,
    /// Region / immortal object — no accounting happened, never reclaim.
    skip: bool,
}

/// Escape-aware strong decrement (the `release` core). Atomic (Release)
/// for shared objects so the worker that drops the last reference
/// synchronises with the others before reclaiming; plain RMW otherwise.
#[inline]
unsafe fn dec_strong(h: *mut RcHeader) -> DecOutcome {
    let s = unsafe { load_strong(h) };
    if s & REGION_BIT != 0 || s & STRONG_COUNT_MASK == STRONG_IMMORTAL {
        return DecOutcome {
            next: 1,
            shared: false,
            skip: true,
        };
    }
    if s & SHARED_BIT != 0 {
        let a = unsafe { AtomicU32::from_ptr(std::ptr::addr_of_mut!((*h).strong)) };
        let prev = a.fetch_sub(1, Ordering::Release);
        return DecOutcome {
            next: (prev & STRONG_COUNT_MASK).saturating_sub(1),
            shared: true,
            skip: false,
        };
    }
    let next = (s & STRONG_COUNT_MASK).saturating_sub(1);
    unsafe { (*h).strong = (s & !STRONG_COUNT_MASK) | (next & STRONG_COUNT_MASK) };
    DecOutcome {
        next,
        shared: false,
        skip: false,
    }
}

/// Marks `payload` and its reachable RC subgraph as shared (escaped to
/// another goroutine), so subsequent retains/releases use atomics and
/// the cycle collector leaves them alone. Idempotent and cycle-safe
/// (stops at already-shared nodes). Called at escape points on the
/// owning thread *before* the value is published, so the walk here
/// races with no one.
unsafe fn mark_shared(payload: *mut u8) {
    let base = untag_rc(payload);
    if base.is_null() || unsafe { in_region_arena(base) } {
        return;
    }
    if unsafe { crate::c_abi::string::is_gos_string(base.cast()) } {
        return;
    }
    let mut work: Vec<*mut u8> = vec![base];
    while let Some(p) = work.pop() {
        if p.is_null() || unsafe { in_region_arena(p) } {
            continue;
        }
        if unsafe { crate::c_abi::string::is_gos_string(p.cast()) } {
            continue;
        }
        let h = unsafe { header_ptr(p) };
        let s = unsafe { load_strong(h) };
        if s & SHARED_BIT != 0 || s & REGION_BIT != 0 || s & STRONG_COUNT_MASK == STRONG_IMMORTAL {
            continue;
        }
        let a = unsafe { AtomicU32::from_ptr(std::ptr::addr_of_mut!((*h).strong)) };
        a.fetch_or(SHARED_BIT, Ordering::Relaxed);
        unsafe {
            visit_children_raw(p, |c| work.push(c));
        }
    }
}

/// Marks the reachable RC subgraph of `payload` as shared across
/// goroutines. Called from the channel-send / goroutine-spawn lowering
/// so escaped objects switch to atomic reference counting. The codegen
/// gates the call on the static type (RC-managed only), so scalars
/// never reach here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_mark_shared(payload: *mut u8) {
    unsafe { mark_shared(payload) };
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
    // Objects that have escaped to another goroutine are excluded from
    // the per-thread cycle collector — touching their flag bits here is a
    // non-atomic write that would race a concurrent worker's atomic
    // retain/release. Their cycles leak (like `Arc`); break with weak refs.
    if unsafe { is_shared(h) } {
        return;
    }
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

/// Rounds `total` bytes to its size class. Returns `(rounded_bytes,
/// Some(class_index))` when poolable, or `(rounded, None)` for oversized
/// allocations that bypass the pool. The rounded byte size equals
/// `units * SIZE_UNIT`.
/// Allocate `total` zeroed bytes for an RC block. Calls mimalloc's plain
/// `mi_zalloc` directly instead of going through the Rust global-allocator
/// facade: the facade routes every allocation through
/// `mi_zalloc_aligned(size, align)`, and mimalloc v3's aligned entry pads
/// the request by 8-16 bytes — a 48-byte RC node then occupies a 64-byte
/// block, a flat ~25% RAM tax on every RC object. Plain `mi_zalloc`
/// returns exactly the requested bin and guarantees 16-byte alignment,
/// which covers `RC_ALIGN`. Under ThreadSanitizer the global allocator is
/// the system one, so the facade is kept (mixing would free across
/// allocators).
#[inline]
fn rc_block_alloc_zeroed(total: usize) -> *mut u8 {
    #[cfg(not(any(tsan, miri, fuzzing)))]
    {
        unsafe { libmimalloc_sys::mi_zalloc(total).cast() }
    }
    #[cfg(any(tsan, miri, fuzzing))]
    {
        let Ok(layout) = Layout::from_size_align(total, RC_ALIGN) else {
            return std::ptr::null_mut();
        };
        let base = unsafe { alloc_zeroed(layout) };
        if !base.is_null() {
            tsan_sizes().lock().insert(base as usize, total);
        }
        base
    }
}

/// Like [`rc_block_alloc_zeroed`] without the zero fill, for callers
/// that provably write every byte.
#[inline]
fn rc_block_alloc_unzeroed(total: usize) -> *mut u8 {
    #[cfg(not(any(tsan, miri, fuzzing)))]
    {
        unsafe { libmimalloc_sys::mi_malloc(total).cast() }
    }
    #[cfg(any(tsan, miri, fuzzing))]
    {
        rc_block_alloc_zeroed(total)
    }
}

/// Free an RC block allocated by [`rc_block_alloc_zeroed`].
#[inline]
unsafe fn rc_block_free(base: *mut u8) {
    #[cfg(not(any(tsan, miri, fuzzing)))]
    {
        unsafe { libmimalloc_sys::mi_free(base.cast()) };
    }
    #[cfg(any(tsan, miri, fuzzing))]
    {
        // The system allocator needs the original layout back; recover
        // the size from the tsan-only side map populated at allocation.
        let total = tsan_sizes()
            .lock()
            .remove(&(base as usize))
            .unwrap_or(RC_HEADER_SIZE);
        if let Ok(layout) = Layout::from_size_align(total, RC_ALIGN) {
            unsafe { dealloc(base, layout) };
        }
    }
}

/// Byte sizes of live RC blocks, ThreadSanitizer builds only (the
/// system allocator's `dealloc` needs the original layout; production
/// builds free through `mi_free`, which doesn't).
#[cfg(any(tsan, miri, fuzzing))]
fn tsan_sizes() -> &'static parking_lot::Mutex<std::collections::HashMap<usize, usize>> {
    static SIZES: std::sync::OnceLock<parking_lot::Mutex<std::collections::HashMap<usize, usize>>> =
        std::sync::OnceLock::new();
    SIZES.get_or_init(|| parking_lot::Mutex::new(std::collections::HashMap::new()))
}

// ---------------------------------------------------------------
// Meta interning: blob pointer <-> u16 id.
// ---------------------------------------------------------------
//
// Metas are per-TYPE module constants — a program has a handful of
// distinct ones — so the header stores a 16-bit id instead of the
// 8-byte pointer. Reads (`meta_of`, on every release walk) are a single
// relaxed load from an append-only table; writes intern through a map
// with a per-thread single-entry memo, which hits ~always because
// allocation sites repeat the same type.

const META_TABLE_CAP: usize = 1 << 16;

/// Append-only id -> blob-pointer table. Slot 0 is permanently null.
static META_TABLE: [std::sync::atomic::AtomicUsize; META_TABLE_CAP] =
    [const { std::sync::atomic::AtomicUsize::new(0) }; META_TABLE_CAP];

static META_IDS: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashMap<usize, u16>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));
static META_NEXT: AtomicUsize = AtomicUsize::new(1);

thread_local! {
    /// Last (pointer, id) pair interned on this thread.
    static META_MEMO: std::cell::Cell<(usize, u16)> = const { std::cell::Cell::new((0, 0)) };
}

fn meta_intern(meta: *const i64) -> u16 {
    if meta.is_null() {
        return 0;
    }
    let key = meta as usize;
    let memo = META_MEMO.with(std::cell::Cell::get);
    if memo.0 == key {
        return memo.1;
    }
    let mut ids = META_IDS.lock();
    let id = if let Some(&id) = ids.get(&key) {
        id
    } else {
        let next = META_NEXT.fetch_add(1, Ordering::Relaxed);
        if next >= META_TABLE_CAP {
            // Table exhausted (65535 distinct metas): treat the object as
            // a leaf. Its children are never released — a leak, never a
            // corruption — and no realistic program has this many ADTs.
            return 0;
        }
        let id = next as u16;
        META_TABLE[next].store(key, Ordering::Release);
        ids.insert(key, id);
        id
    };
    drop(ids);
    META_MEMO.with(|m| m.set((key, id)));
    id
}

/// The child-layout blob for a header, or null for leaves.
#[inline]
unsafe fn meta_of(h: *const RcHeader) -> *const i64 {
    let id = unsafe { (*h).meta_id } as usize;
    META_TABLE[id].load(Ordering::Acquire) as *const i64
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
// region is freed in O(slabs) at `gos_rt_arena_pop` — never a per-node
// teardown walk. The compiler guarantees no region object outlives the
// pop (region-block results are RC-free and region values cannot be
// assigned to outer bindings), so the bulk free is sound.

/// Default slab size; one `mmap`-backed glibc allocation amortised over
/// many node allocations. A single oversized object gets its own slab.
const REGION_SLAB_BYTES: usize = 1 << 20;

// ---------------------------------------------------------------
// Region arena: one reserved virtual range for every region slab.
// ---------------------------------------------------------------
//
// Region objects can be HEADERLESS (16-byte tree nodes), so the
// accounting entries cannot read a header bit to decide "no-op".
// Instead every region slab is carved out of a single reserved
// virtual range, and `in_region(ptr)` is a subtract + compare against
// a cached global — no memory access into the object. If the reserve
// fails (exotic environment), regions disable themselves
// (`gos_rt_arena_push` no-ops) and everything stays reference
// counted with headers — slower, never unsound.

/// Virtual reservation size. Address space only; pages are committed
/// slab-by-slab as regions actually allocate.
const REGION_ARENA_BYTES: usize = 1 << 36;

/// Base address of the reserved range. 0 = not yet initialised;
/// `usize::MAX` = reservation failed (regions disabled).
static REGION_ARENA_BASE: AtomicUsize = AtomicUsize::new(0);
/// Bump offset of the next never-used slab within the reserve.
static REGION_ARENA_NEXT: AtomicUsize = AtomicUsize::new(0);
/// Decommitted standard-size slabs available for re-commit, as arena
/// offsets. (Thread-local `FREE_SLABS` recycling keeps slabs
/// committed; this list holds overflow beyond `FREE_SLAB_CAP`.)
static REGION_ARENA_FREE: parking_lot::Mutex<Vec<usize>> = parking_lot::Mutex::new(Vec::new());

/// True when `ptr` points into region-arena memory. One cached global
/// load plus the pure [`addr_in_region_arena`] range test.
#[inline]
pub(crate) fn in_region_arena(ptr: *const u8) -> bool {
    addr_in_region_arena(ptr as usize, REGION_ARENA_BASE.load(Ordering::Relaxed))
}

/// Pure range test for [`in_region_arena`], split out so the sentinel
/// handling is unit-testable without mutating the global base.
///
/// `base` carries two sentinels that are NOT live reservations: `0`
/// (no arena reserved yet) and `usize::MAX` (reservation failed). In
/// either state no pointer is region memory, so the range check must be
/// skipped entirely. The check is only meaningful once `base` is a real
/// reservation: with `base == 0` the subtraction is the identity, so the
/// bare range test would classify every pointer below `REGION_ARENA_BYTES`
/// (64 GiB) as in-region. That holds for the low heap addresses Windows
/// hands out, which silently turned `gos_rt_rc_retain`/`release` into
/// no-ops there — a use-after-free, since structural frees still ran.
#[inline]
fn addr_in_region_arena(addr: usize, base: usize) -> bool {
    if base == 0 || base == usize::MAX {
        return false;
    }
    addr.wrapping_sub(base) < REGION_ARENA_BYTES
}

/// Strip a tagged-repr enum's discriminant bits (pointer bits 1-2)
/// from an RC pointer. String bodies are deliberately ODD pointers and
/// pass through untouched; every other heap pointer is 8-aligned, so
/// the mask is a no-op for untagged values.
#[inline]
pub(crate) fn untag_rc(p: *mut u8) -> *mut u8 {
    if p as usize & 1 == 0 {
        (p as usize & !7) as *mut u8
    } else {
        p
    }
}

/// Reserve the arena on first use. Returns the base, or `usize::MAX`
/// when virtual reservation is unavailable.
fn region_arena_base() -> usize {
    let cur = REGION_ARENA_BASE.load(Ordering::Acquire);
    if cur != 0 {
        return cur;
    }
    let reserved = arena_reserve(REGION_ARENA_BYTES);
    let val = if reserved.is_null() {
        usize::MAX
    } else {
        reserved as usize
    };
    match REGION_ARENA_BASE.compare_exchange(0, val, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => val,
        Err(winner) => {
            // Lost the race; release our reservation.
            if !reserved.is_null() {
                arena_release(reserved, REGION_ARENA_BYTES);
            }
            winner
        }
    }
}

#[cfg(unix)]
fn arena_reserve(len: usize) -> *mut u8 {
    // SAFETY: anonymous PROT_NONE reservation; no file, no aliasing.
    let p = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            len,
            libc::PROT_NONE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
            -1,
            0,
        )
    };
    if p == libc::MAP_FAILED {
        std::ptr::null_mut()
    } else {
        p.cast()
    }
}

#[cfg(unix)]
fn arena_release(p: *mut u8, len: usize) {
    // SAFETY: releasing exactly the mapping created in arena_reserve.
    unsafe { libc::munmap(p.cast(), len) };
}

#[cfg(unix)]
fn arena_commit(p: *mut u8, len: usize) -> bool {
    // SAFETY: p..p+len lies inside our reservation.
    unsafe { libc::mprotect(p.cast(), len, libc::PROT_READ | libc::PROT_WRITE) == 0 }
}

#[cfg(unix)]
fn arena_decommit(p: *mut u8, len: usize) {
    // Return the physical pages; keep the address range reserved.
    // SAFETY: range lies inside our reservation.
    unsafe {
        libc::madvise(p.cast(), len, libc::MADV_DONTNEED);
    }
}

#[cfg(windows)]
fn arena_reserve(len: usize) -> *mut u8 {
    use windows_sys::Win32::System::Memory::{MEM_RESERVE, PAGE_NOACCESS, VirtualAlloc};
    // SAFETY: plain reservation, no aliasing.
    unsafe { VirtualAlloc(std::ptr::null(), len, MEM_RESERVE, PAGE_NOACCESS).cast() }
}

#[cfg(windows)]
fn arena_release(p: *mut u8, _len: usize) {
    use windows_sys::Win32::System::Memory::{MEM_RELEASE, VirtualFree};
    // SAFETY: releasing exactly the reservation from arena_reserve.
    unsafe { VirtualFree(p.cast(), 0, MEM_RELEASE) };
}

#[cfg(windows)]
fn arena_commit(p: *mut u8, len: usize) -> bool {
    use windows_sys::Win32::System::Memory::{MEM_COMMIT, PAGE_READWRITE, VirtualAlloc};
    // SAFETY: committing pages inside our reservation.
    !unsafe { VirtualAlloc(p.cast(), len, MEM_COMMIT, PAGE_READWRITE) }.is_null()
}

#[cfg(windows)]
fn arena_decommit(p: *mut u8, len: usize) {
    use windows_sys::Win32::System::Memory::{MEM_DECOMMIT, VirtualFree};
    // SAFETY: decommitting pages inside our reservation.
    unsafe { VirtualFree(p.cast(), len, MEM_DECOMMIT) };
}

/// Carve (or re-commit) a slab of `slab_size` bytes from the arena.
/// Null when the arena is unavailable or exhausted — callers fall back
/// to headered global allocation (sound, just unoptimised).
/// Host page size, queried once. Slab offsets inside the reserved
/// arena must be page-multiples or `mprotect` / `VirtualAlloc`
/// rejects the commit — and the size is NOT universally 4 KiB
/// (macOS arm64 and some aarch64 Linux kernels use 16 KiB or 64 KiB
/// pages).
fn os_page_size() -> usize {
    static PAGE: AtomicUsize = AtomicUsize::new(0);
    let cached = PAGE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    #[cfg(unix)]
    // SAFETY: sysconf is async-signal-safe and has no preconditions.
    let size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(4096) as usize;
    #[cfg(windows)]
    let size = {
        use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
        let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: GetSystemInfo fills the struct; no preconditions.
        unsafe { GetSystemInfo(&mut info) };
        (info.dwPageSize as usize).max(4096)
    };
    PAGE.store(size, Ordering::Relaxed);
    size
}

fn arena_acquire(slab_size: usize) -> *mut u8 {
    let base = region_arena_base();
    if base == usize::MAX {
        return std::ptr::null_mut();
    }
    if slab_size == REGION_SLAB_BYTES {
        if let Some(off) = REGION_ARENA_FREE.lock().pop() {
            let p = (base + off) as *mut u8;
            if arena_commit(p, slab_size) {
                return p;
            }
            return std::ptr::null_mut();
        }
    }
    let page_mask = os_page_size() - 1;
    let rounded = (slab_size + page_mask) & !page_mask;
    let off = REGION_ARENA_NEXT.fetch_add(rounded, Ordering::Relaxed);
    if off + rounded > REGION_ARENA_BYTES {
        return std::ptr::null_mut();
    }
    let p = (base + off) as *mut u8;
    if arena_commit(p, rounded) {
        p
    } else {
        std::ptr::null_mut()
    }
}

/// Decommit a no-longer-needed standard slab and remember its offset
/// for re-commit. Oversized slabs are decommitted and their address
/// range retired (rare; bounded by peak oversized use).
fn arena_retire(p: *mut u8, slab_size: usize) {
    let base = REGION_ARENA_BASE.load(Ordering::Relaxed);
    arena_decommit(p, slab_size);
    if slab_size == REGION_SLAB_BYTES {
        REGION_ARENA_FREE.lock().push(p as usize - base);
    }
}

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
/// `arena_push` into a bump-pointer reset instead of a 1 MiB `mmap`.
const FREE_SLAB_CAP: usize = 64;

thread_local! {
    /// Stack of suspended regions on this thread (the innermost region's live
    /// bump is in `BUMP`). Only touched on push/pop/slab-exhaustion.
    static REGIONS: std::cell::RefCell<Vec<RegionSlabs>> =
        const { std::cell::RefCell::new(Vec::new()) };
    /// Pool of freed standard-size (`REGION_SLAB_BYTES`) slabs, reused by the
    /// next `arena_push` instead of re-`mmap`ing. Bounded by `FREE_SLAB_CAP`.
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
    // Slabs come exclusively from the reserved arena so `in_region_arena`
    // can identify region memory without touching the object. Null
    // (arena unavailable/exhausted) makes the region allocation fail and
    // the caller fall back to headered global allocation.
    arena_acquire(slab_size)
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
/// `arena_pop` — callers must NOT individually free them.
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
    region_alloc_inner_impl(total, count_obj, true)
}

/// Like [`region_alloc_inner`] but lets fully-initializing callers skip
/// the zero fill (a tagged-repr enum constructor stores every payload
/// slot, so pre-zeroing doubles its write traffic for nothing).
fn region_alloc_inner_unzeroed(total: usize, count_obj: bool) -> *mut u8 {
    region_alloc_inner_impl(total, count_obj, false)
}

fn region_alloc_inner_impl(total: usize, count_obj: bool, zero: bool) -> *mut u8 {
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
    // allocated, and codegen relies on every allocation starting zeroed —
    // except for callers that provably overwrite every byte.
    if zero {
        unsafe { std::ptr::write_bytes(ptr, 0, need) };
    }
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
/// [`gos_rt_arena_pop`] are bump-allocated and freed wholesale.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_arena_push() {
    if region_arena_base() == usize::MAX {
        // Virtual reservation unavailable: regions disable themselves and
        // every allocation stays reference counted (headered). The matching
        // pop no-ops via the empty REGIONS stack.
        return;
    }
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
pub extern "C" fn gos_rt_arena_pop() {
    let pending_objs = BUMP_OBJS.with(|o| o.replace(0));
    let restored = REGIONS.with(|r| {
        let mut regions = r.borrow_mut();
        let region = regions.pop()?;
        if rc_live_enabled() {
            RC_LIVE.fetch_sub(region.objs + pending_objs, Ordering::Relaxed);
        }
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
            arena_retire(base, size);
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
/// Allocate a TAGGED-repr enum node (discriminant in pointer bits, no
/// header byte consulted at match time). Inside an active region the
/// node is completely HEADERLESS — `size` payload bytes, bump-allocated,
/// bulk-freed at pop, identified by the arena range check (never by a
/// header) — a two-pointer tree node costs exactly 16 bytes. Outside a
/// region this is a normal reference-counted allocation (the header
/// carries counts; the disc bits still live in the pointer).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_alloc_tagged(size: u64, meta: *const i64) -> *mut u8 {
    if region_active() {
        let p = region_alloc_inner_unzeroed(size as usize, false);
        if !p.is_null() {
            return p;
        }
        // Arena unavailable: fall through to the headered global path.
    }
    // Tagged-enum constructors store every payload slot, so the global
    // path also skips the payload memset (the header is written field
    // by field below).
    let total = (size as usize).saturating_add(RC_HEADER_SIZE);
    let in_region = false;
    let _ = in_region;
    let base = rc_block_alloc_unzeroed(total);
    if base.is_null() {
        return unsafe { gos_rt_rc_alloc(size, meta) };
    }
    let h = base as *mut RcHeader;
    unsafe {
        (*h).strong = 1;
        (*h).weak = 0;
        (*h).disc = 0;
        (*h).meta_id = meta_intern(meta);
    }
    rc_live_inc();
    unsafe { base.add(RC_HEADER_SIZE) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_alloc(size: u64, meta: *const i64) -> *mut u8 {
    // Exact-size request: mimalloc's bins serve it without padding and
    // `mi_free` recovers everything from the pointer, so neither a size
    // field nor class rounding is needed.
    let total = (size as usize).saturating_add(RC_HEADER_SIZE);
    // Inside a `region { … }` the object is bump-allocated and freed
    // wholesale at pop — tag it so retain/release stay no-ops and the
    // teardown walk never touches it.
    let in_region = region_active();
    let base = if in_region {
        region_alloc(total)
    } else {
        rc_block_alloc_zeroed(total)
    };
    if base.is_null() {
        return std::ptr::null_mut();
    }
    let h = base as *mut RcHeader;
    unsafe {
        (*h).strong = if in_region { 1 | REGION_BIT } else { 1 };
        (*h).weak = 0;
        (*h).disc = 0;
        (*h).meta_id = meta_intern(meta);
    }
    rc_live_inc();
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
        let total = 8usize.saturating_add(RC_HEADER_SIZE);
        let base = rc_block_alloc_zeroed(total);
        if base.is_null() {
            return std::ptr::null_mut();
        }
        unsafe {
            let h = base as *mut RcHeader;
            (*h).strong = STRONG_IMMORTAL;
            (*h).weak = 0;
            // Unit-variant singleton: the discriminant lives in the
            // header byte the compiled match reads. The 8-byte payload
            // stays zeroed spare space.
            (*h).disc = u8::try_from(tag).unwrap_or(0);
            (*h).meta_id = 0;
            let payload = base.add(RC_HEADER_SIZE);
            rc_live_inc();
            payload
        }
    };

    if !(0..N as i64).contains(&tag) {
        // Out-of-range discriminant: fall back to a fresh global node.
        // Uncached, so the caller's single ownership reclaims it.
        return alloc_global(tag);
    }
    // Every return hands the caller an OWNED share (+1): the drop pass
    // treats the destination local as owned (release at death) exactly
    // like any other constructor result, and a bare `let x = Enum::Unit`
    // binding must not strip the cache's pin when it dies. The cache
    // insert itself holds the initial strong=1 pin, so the singleton's
    // count never reaches zero.
    let slot = &SINGLETONS[tag as usize];
    let existing = slot.load(Ordering::Acquire);
    if !existing.is_null() {
        unsafe { gos_rt_rc_retain(existing) };
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
        Ok(_) => {
            unsafe { gos_rt_rc_retain(fresh) };
            fresh
        }
        Err(winner) => {
            // Lost the race — drop the redundant node, share the winner's.
            unsafe { gos_rt_rc_release(fresh) };
            unsafe { gos_rt_rc_retain(winner) };
            winner
        }
    }
}

/// Increment the strong count of an RC object. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_retain(payload: *mut u8) {
    let payload = untag_rc(payload);
    // Region-arena objects are bulk-freed at pop and may be HEADERLESS:
    // never touch their memory from the accounting paths.
    if in_region_arena(payload) {
        return;
    }
    if payload.is_null() {
        return;
    }
    if unsafe { crate::c_abi::string::is_gos_string(payload.cast()) } {
        unsafe { crate::c_abi::string::gos_rt_str_retain(payload.cast()) };
        return;
    }
    let h = unsafe { header_ptr(payload) };
    // `inc_strong` reads `strong` atomically and dispatches: region /
    // immortal objects are no-ops; escaped (shared) objects bump the
    // count atomically; thread-local objects take the cheap non-atomic
    // bump (count up, color back to black, buffered bit preserved).
    unsafe { inc_strong(h) };
}

/// Decrement the strong count; at zero, release RC-pointer children
/// (iteratively, to bound stack depth on deep structures) and free the
/// block (unless a weak ref still observes it). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_release(payload: *mut u8) {
    let payload = untag_rc(payload);
    // Region-arena objects are bulk-freed at pop and may be HEADERLESS:
    // never touch their memory from the accounting paths.
    if in_region_arena(payload) {
        return;
    }
    unsafe { rc_release_impl(payload) };
}

/// Create a weak reference from a strong-held payload: increment the weak
/// count and return the same pointer (now carrying weak ownership). Does not
/// touch the strong count. Null-safe (returns null).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_downgrade(payload: *mut u8) -> *mut u8 {
    // Preserve the caller's pointer bit-for-bit (a tagged-repr enum's
    // disc lives in it; the weak round-trip must hand it back). Mask
    // only for header access.
    let base = untag_rc(payload);
    // Region-arena objects are bulk-freed at pop and may be HEADERLESS:
    // never touch their memory from the accounting paths.
    if in_region_arena(base) {
        return std::ptr::null_mut();
    }
    if base.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { header_ptr(base) };
    unsafe { (*h).weak = (*h).weak.saturating_add(1) };
    payload
}

/// Increment the weak count (copying a `Weak`). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_weak_retain(payload: *mut u8) {
    let payload = untag_rc(payload);
    // Region-arena objects are bulk-freed at pop and may be HEADERLESS:
    // never touch their memory from the accounting paths.
    if in_region_arena(payload) {
        return;
    }
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
    let payload = untag_rc(payload);
    // Region-arena objects are bulk-freed at pop and may be HEADERLESS:
    // never touch their memory from the accounting paths.
    if in_region_arena(payload) {
        return;
    }
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
    // Hand back the caller's pointer verbatim (tag bits included);
    // mask only for header access.
    let base = untag_rc(payload);
    if in_region_arena(base) {
        return std::ptr::null_mut();
    }
    if base.is_null() {
        return std::ptr::null_mut();
    }
    let h = unsafe { header_ptr(base) };
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
    // Mask tag bits for the header probe; the Some payload keeps the
    // caller's pointer (and its disc tag) verbatim.
    let base = untag_rc(payload);
    let alive = if base.is_null() || in_region_arena(base) {
        false
    } else {
        let h = unsafe { header_ptr(base) };
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
    // `dec_strong` reads `strong` atomically and dispatches: region /
    // immortal objects are no-ops; escaped (shared) objects decrement
    // atomically (Release); thread-local objects take the cheap
    // non-atomic decrement.
    let d = unsafe { dec_strong(h) };
    if d.skip {
        return;
    }
    if d.next != 0 {
        // Survived the decrement. Thread-local objects become cycle
        // candidates; shared objects are excluded from the per-thread
        // collector (their cycles leak, like `Arc` — break with weak refs).
        if !d.shared {
            unsafe { possible_root(root) };
        }
        return;
    }
    // Last reference. For a shared object an Acquire fence pairs with the
    // other workers' Release decrements so this thread sees all their
    // writes before tearing it down (now exclusively owned — count 0).
    if d.shared {
        fence(Ordering::Acquire);
    }
    unsafe { set_color(h, COLOR_BLACK) };
    let meta = unsafe { meta_of(h) };
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
        // Single fused pass over the meta: string-tagged children free
        // through the tag-checking string path, RC children join the
        // release worklist. (Two separate walks here doubled the
        // per-free meta traversal on every internal node.)
        unsafe {
            visit_children_raw(root, |c| {
                if crate::c_abi::string::is_gos_string(c.cast()) {
                    crate::c_abi::string::gos_rt_str_free(c.cast());
                } else {
                    worklist.push(c);
                }
            });
        }
        let _ = meta;
        unsafe { try_reclaim(root) };
        while let Some(payload) = worklist.pop() {
            if payload.is_null() {
                continue;
            }
            let h = unsafe { header_ptr(payload) };
            let d = unsafe { dec_strong(h) };
            if d.skip {
                continue;
            }
            if d.next != 0 {
                if !d.shared {
                    unsafe { possible_root(payload) };
                }
                continue;
            }
            if d.shared {
                fence(Ordering::Acquire);
            }
            unsafe { set_color(h, COLOR_BLACK) };
            unsafe {
                visit_children_raw_buffered(payload, &mut worklist);
            }
            unsafe { try_reclaim(payload) };
        }
    });
}

/// Fused child dispatch for the worklist loop: strings are freed
/// immediately, RC children are appended to `worklist`.
unsafe fn visit_children_raw_buffered(payload: *mut u8, worklist: &mut Vec<*mut u8>) {
    unsafe {
        visit_children_raw(payload, |c| {
            if crate::c_abi::string::is_gos_string(c.cast()) {
                crate::c_abi::string::gos_rt_str_free(c.cast());
            } else {
                worklist.push(c);
            }
        });
    }
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
    // Type metas list every heap child a node owns, and an enum or
    // struct can own a *String* child — whose allocation carries the
    // string tag header, not an `RcHeader`. Feeding one to the count /
    // color machinery reads garbage, so the RC-graph walk yields only
    // RC-headered children; the release path reclaims string children
    // through [`visit_string_children`].
    unsafe {
        visit_children_raw(payload, |child| {
            if !crate::c_abi::string::is_gos_string(child.cast()) {
                f(child);
            }
        });
    }
}

unsafe fn visit_children_raw(payload: *mut u8, mut raw_f: impl FnMut(*mut u8)) {
    // Child words may carry tagged-repr enum pointers; consumers work
    // on payload bases (strings stay odd and untouched).
    let mut f = |c: *mut u8| raw_f(untag_rc(c));
    let meta = unsafe { meta_of(header_ptr(payload)) };
    if meta.is_null() {
        return;
    }
    let kind = unsafe { *meta };
    let variant_count = unsafe { *meta.add(1) };
    if kind == RC_KIND_STRUCT_GUARDED {
        unsafe { visit_guarded_children(payload, meta, f) };
        return;
    }
    // Only Enum and Struct carry child layouts today. String / Vec / Map
    // / Closure layouts are wired in a later phase and never reach here.
    if kind != RC_KIND_ENUM && kind != RC_KIND_STRUCT {
        return;
    }
    let target_disc = if kind == RC_KIND_ENUM {
        i64::from(unsafe { (*header_ptr(payload)).disc })
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

/// Free an RC block's underlying allocation. Called when the block is no
/// longer observed by any strong *or* weak reference. The payload's children
/// must already have been released (at the strong→0 transition). The byte
/// size is recovered from the header's `size_u` (or the oversized side table).
unsafe fn free_block(payload: *mut u8) {
    let h = unsafe { header_ptr(payload) };
    // Copy-blobs leave the provenance set exactly here, so a reused
    // address can never inherit membership. One meta-word compare for
    // every other RC object.
    let meta = unsafe { meta_of(h) };
    if !meta.is_null() && unsafe { *meta } == RC_KIND_STRUCT_GUARDED {
        copy_blob_remove(payload);
    }
    let base = h as *mut u8;
    rc_live_dec();
    // Straight back to mimalloc — see `gos_rt_rc_alloc` for why a custom
    // slab/pool is not used (measured net-neutral), and
    // `rc_block_alloc_zeroed` for why the call is direct.
    unsafe { rc_block_free(base) };
}

// ---------------------------------------------------------------
// Escaped value-aggregate copy-blobs (deterministic reclamation).
// ---------------------------------------------------------------
//
// When a multi-slot struct value flows into a `Some(..)`/`Ok(..)` payload
// on the LLVM tier, the backend makes a heap copy of its flat slots.
// Those copies are single-owner value snapshots; `gos_rt_rc_alloc_copy`
// puts them under reference counting with an `RC_KIND_STRUCT_GUARDED`
// meta so the MIR drop pass can release them deterministically when the
// owning aggregate slot dies.
//
// The provenance set below is the soundness backstop: a guarded payload
// word can also hold pointers this system did NOT allocate (a map-get
// result, an interior borrow, the Cranelift tier's construction-allocated
// aggregates). Every guarded retain/release first checks membership and
// leaves foreign pointers untouched — an unknown pointer can be leaked,
// never corrupted. Entries are removed exactly at `free_block`, so a
// reused address can never inherit stale membership.
static COPY_BLOBS: std::sync::LazyLock<parking_lot::Mutex<std::collections::HashSet<usize>>> =
    std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashSet::new()));

fn copy_blob_register(p: *mut u8) {
    COPY_BLOBS.lock().insert(p as usize);
}

fn copy_blob_contains(p: *mut u8) -> bool {
    COPY_BLOBS.lock().contains(&(p as usize))
}

fn copy_blob_remove(p: *mut u8) {
    COPY_BLOBS.lock().remove(&(p as usize));
}

/// Walk the `(disc_word, payload_word)` pairs of an `RC_KIND_STRUCT_GUARDED`
/// meta over the aggregate slots at `base`, calling `f` for each child that
/// is live (negative disc word, or the disc word reads 0), non-null, and a
/// registered copy-blob. `base` may be a heap payload or a stack slot — the
/// walk only reads the flat words the meta names.
unsafe fn visit_guarded_children(base: *mut u8, meta: *const i64, mut f: impl FnMut(*mut u8)) {
    let entry_count = unsafe { *meta.add(1) };
    for i in 0..entry_count.max(0) {
        let gate = unsafe { *meta.add(2 + (i as usize) * 3) };
        let disc_word = unsafe { *meta.add(3 + (i as usize) * 3) };
        let payload_word = unsafe { *meta.add(4 + (i as usize) * 3) };
        // `gate` is the discriminant value under which the payload word
        // holds a copy-blob pointer (0 = Ok/Some side, 1 = Err side);
        // negative means unconditional (both sides are blobs).
        if gate >= 0 {
            let disc = unsafe { *(base.add(disc_word as usize * 8) as *const i64) };
            if disc != gate {
                continue;
            }
        }
        let child = unsafe { *(base.add(payload_word as usize * 8) as *const *mut u8) };
        if !child.is_null() && copy_blob_contains(child) {
            f(child);
        }
    }
}

/// Allocate an RC copy-blob, memcpy `size` bytes from `src`, retain the
/// guarded children the copy now shares with its source, and register the
/// blob in the provenance set. Inside a `region` block the bytes are
/// bump-allocated and freed wholesale at pop, so the blob is neither
/// registered nor are its children retained (region objects never run the
/// per-node teardown walk).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_rc_alloc_copy(
    size: u64,
    meta: *const i64,
    src: *const u8,
) -> *mut u8 {
    let in_region = region_active();
    let payload = unsafe { gos_rt_rc_alloc(size, if in_region { std::ptr::null() } else { meta }) };
    if payload.is_null() || src.is_null() {
        return payload;
    }
    unsafe { std::ptr::copy_nonoverlapping(src, payload, size as usize) };
    if in_region {
        return payload;
    }
    unsafe {
        visit_guarded_children(payload, meta, |child| {
            gos_rt_rc_retain(child);
        });
    }
    copy_blob_register(payload);
    payload
}

/// Release the guarded children held in the aggregate slots at `base`
/// (a stack aggregate dying or being overwritten). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_aggr_release_children(base: *mut u8, meta: *const i64) {
    if base.is_null() || meta.is_null() {
        return;
    }
    unsafe {
        visit_guarded_children(base, meta, |child| {
            gos_rt_rc_release(child);
        });
    }
}

/// Retain the guarded children held in the aggregate slots at `base`
/// (a stack aggregate that was just whole-copied). Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_aggr_retain_children(base: *mut u8, meta: *const i64) {
    if base.is_null() || meta.is_null() {
        return;
    }
    unsafe {
        visit_guarded_children(base, meta, |child| {
            gos_rt_rc_retain(child);
        });
    }
}

/// Zero the `(disc, payload)` word pairs a guarded meta names within the
/// aggregate slots at `base`. Entry-block initialisation: without it the
/// first release-before-reassignment walk would read stack garbage, and a
/// garbage word that happens to equal a live copy-blob address would be
/// spuriously released. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_aggr_zero_guarded(base: *mut u8, meta: *const i64) {
    if base.is_null() || meta.is_null() {
        return;
    }
    let entry_count = unsafe { *meta.add(1) };
    for i in 0..entry_count.max(0) {
        let gate = unsafe { *meta.add(2 + (i as usize) * 3) };
        let disc_word = unsafe { *meta.add(3 + (i as usize) * 3) };
        let payload_word = unsafe { *meta.add(4 + (i as usize) * 3) };
        if gate >= 0 && disc_word >= 0 {
            // Write a discriminant that fails every gate (no entry gates
            // on a negative disc), so an accidental read of the
            // not-yet-assigned field never sees a live payload. For the
            // Option/Result encoding -1 is no valid variant; the real
            // first assignment overwrites it.
            unsafe { *(base.add(disc_word as usize * 8) as *mut i64) = -1 };
        }
        unsafe { *(base.add(payload_word as usize * 8) as *mut i64) = 0 };
    }
}

/// Release the payload of the by-value `{disc, payload}` Option/Result at
/// `slot` when it is a registered copy-blob. Companion to
/// [`gos_rt_option_slot_retain`]; used when an option holder dies, is
/// overwritten, or an owning field slot is replaced. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_slot_release(slot: *const i64) {
    if slot.is_null() {
        return;
    }
    let payload = unsafe { *(slot.add(1) as *const *mut u8) };
    if !payload.is_null() && copy_blob_contains(payload) {
        unsafe { gos_rt_rc_release(payload) };
        // Null the payload word so a second release of the same slot
        // (consumption-site release + the unconditional return-sweep)
        // is a no-op instead of a double-free — the same null-out
        // discipline the local-release pass uses. The address may be
        // reused and re-registered in the provenance set, so "the set
        // no longer contains it" is not a safe second-release guard.
        unsafe { *slot.add(1).cast_mut() = 0 };
    }
}

/// Retain the payload of the by-value `{disc, payload}` Option/Result at
/// `slot` when the discriminant reads 0 (`Some`/`Ok`) and the payload is a
/// registered copy-blob. Used when an aliased option value is stored into
/// an owning aggregate slot. Null-safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_option_slot_retain(slot: *const i64) {
    if slot.is_null() {
        return;
    }
    let payload = unsafe { *(slot.add(1) as *const *mut u8) };
    if !payload.is_null() && copy_blob_contains(payload) {
        unsafe { gos_rt_rc_retain(payload) };
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
        // A candidate that escaped to another goroutine after being
        // buffered: drop it without touching its flag bits (a concurrent
        // worker may be mutating them atomically). Shared objects never
        // re-enter the collector, so the stale buffered bit is harmless.
        if unsafe { is_shared(h) } {
            continue;
        }
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

    #[test]
    #[cfg_attr(miri, ignore)] // arena uses mmap with non-RW protections; Miri can't model it
    fn region_arena_rejects_every_pointer_when_no_arena_reserved() {
        // Regression: with the uninitialised base (0) the range test must
        // report NO pointer as region memory — even ones below the 64 GiB
        // reserve size. The old bare `wrapping_sub` classified every low
        // heap pointer as in-region, which neutralised RC retain/release on
        // platforms whose allocator hands out low addresses (Windows),
        // producing use-after-free in any handler that retains a heap value.
        assert!(!addr_in_region_arena(0x1000, 0));
        assert!(!addr_in_region_arena(REGION_ARENA_BYTES - 1, 0));
        assert!(!addr_in_region_arena(0xdead_beef, 0));
        // Failed-reservation sentinel disables regions just the same.
        assert!(!addr_in_region_arena(0x2000, usize::MAX));
    }

    #[test]
    #[cfg_attr(miri, ignore)] // arena uses mmap with non-RW protections; Miri can't model it
    fn region_arena_matches_only_the_reserved_window() {
        let base = 0x7000_0000_0000usize;
        assert!(addr_in_region_arena(base, base));
        assert!(addr_in_region_arena(base + REGION_ARENA_BYTES - 1, base));
        assert!(!addr_in_region_arena(base - 1, base));
        assert!(!addr_in_region_arena(base + REGION_ARENA_BYTES, base));
        // A pointer below the reserve (the Windows-heap shape) is outside it.
        assert!(!addr_in_region_arena(0x1_0000, base));
    }

    // `RC_LIVE` is process-global; tests that assert exact live-count
    // deltas must not run concurrently with each other's allocations.
    static COUNT_LOCK: Mutex<()> = Mutex::new(());

    fn count_guard() -> std::sync::MutexGuard<'static, ()> {
        COUNT_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Allocate via the runtime entry and write the discriminant into
    /// the header byte (mirroring `gos_enum_set_disc`).
    unsafe fn alloc_with_disc(payload_words: usize, disc: i64, meta: *const i64) -> *mut u8 {
        let p = unsafe { gos_rt_rc_alloc((payload_words * 8) as u64, meta) };
        assert!(!p.is_null());
        unsafe { (*header_ptr(p)).disc = u8::try_from(disc).unwrap_or(0) };
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
    #[cfg_attr(miri, ignore)] // arena uses mmap with non-RW protections; Miri can't model it
    fn region_allocs_are_freed_wholesale_at_pop() {
        let _g = count_guard();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            gos_rt_arena_push();
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
            gos_rt_arena_pop();
            assert!(!region_active());
        }
        assert_eq!(rc_live_count(), base, "pop frees the whole region");
    }

    #[test]
    #[cfg_attr(miri, ignore)] // arena uses mmap with non-RW protections; Miri can't model it
    fn region_tree_freed_without_per_node_teardown() {
        let _g = count_guard();
        fresh_cycle_state();
        let base = rc_live_count();
        let meta = node_meta();
        unsafe {
            gos_rt_arena_push();
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
            gos_rt_arena_pop();
        }
        assert_eq!(
            rc_live_count(),
            base,
            "pop reclaims parent + child together"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // arena uses mmap with non-RW protections; Miri can't model it
    fn region_oversized_alloc_gets_its_own_slab() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            gos_rt_arena_push();
            // Larger than the default slab — must still allocate, on its own slab.
            let big = gos_rt_rc_alloc((REGION_SLAB_BYTES as u64) * 2, std::ptr::null());
            assert!(!big.is_null());
            assert!(is_region(header_ptr(big)));
            gos_rt_arena_pop();
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
            let node = alloc_with_disc(2, 1, meta.as_ptr());
            set_child(node, 0, l0);
            set_child(node, 1, l1);
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
    fn oversized_block_round_trips() {
        let _g = count_guard();
        let base = rc_live_count();
        unsafe {
            // A multi-MiB payload: no size is recorded anywhere any more
            // (mi_free recovers the block from the pointer), so this pins
            // that large blocks still alloc, retain state, and free.
            let big = (u16::MAX as u64) * 16 + 4096;
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
            0,
            1,
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
            let node = alloc_with_disc(2, 1, meta.as_ptr());
            set_child(node, 0, l0);
            set_child(node, 1, l1);
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
            let node = alloc_with_disc(2, 1, meta.as_ptr());
            set_child(node, 0, shared);
            set_child(node, 1, l1);
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
        // Miri interprets every node alloc/release, so the 1M-node
        // native stress would take hours; a shallower list still
        // exercises the iterative (non-recursive) release path there.
        let depth = if cfg!(miri) {
            10_000usize
        } else {
            1_000_000usize
        };
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
