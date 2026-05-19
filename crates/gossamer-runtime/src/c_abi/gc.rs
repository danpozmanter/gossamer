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

use std::alloc::{Layout, alloc_zeroed, dealloc};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

// ---------------------------------------------------------------
// GC — bump allocator with safepoint reset
// ---------------------------------------------------------------
//
// Thread-local arena. `gos_rt_gc_alloc(size)` bumps a pointer;
// when the arena fills, a new one is allocated and the old one
// stays alive until `gos_rt_gc_reset()` discards every arena on
// the current thread. Call reset at well-defined safepoints
// (end of main, between benchmark iterations, etc.).
//
// Arena buffers are capped at `MAX_ARENA_CAP` so the geometric
// growth path (2× per fresh arena) plateaus instead of running
// away. Without the cap, after K arenas total capacity was
// `ARENA_BYTES * (2^K - 1)`; with the cap it's at most
// `MAX_ARENA_CAP * K`. For long-running format-heavy programs
// this turns "exponential blowup of slack space at the tail of
// each arena" into "linear in the number of arenas needed".
//
// `gos_rt_arena_save() -> u64` / `gos_rt_arena_restore(saved)`
// expose a checkpoint/rewind primitive so codegen can wrap
// scope-bounded allocations (e.g. ephemeral format!() output that
// is consumed before the surrounding function returns) without
// permanently leaking the slack. The semantics are "undo every
// allocation made since the matching save"; callers must
// guarantee no live pointer into the saved range escapes the
// scope, since restore makes those pointers dangling.
//
// A real tri-color GC replaces this without changing the ABI.

// Bump arena retired (fix_architecture_ownership.md Stage 4). The
// `Arena` / `ARENAS` types and the `try_extend_last_cstring` fast
// path used to live here; they're gone in favour of Box-leak
// allocation. Constants kept zeroed-out as documentation of what
// the previous limits were if anyone wonders about the historical
// allocator.

// ---------------------------------------------------------------
// GC allocation registry
// ---------------------------------------------------------------
//
// `gos_rt_gc_alloc` is the sole entry point for user-struct heap
// allocation in compiled Gossamer (Cranelift + LLVM tiers).
//
// Default mode (GOS_GC_TRACK unset): allocate via the global
// allocator with 8-byte alignment; no tracking. `gos_rt_gc_reset()`
// is a no-op. This path has zero overhead vs the old Box-leak shape.
//
// Tracking mode (GOS_GC_TRACK=1): every allocation is registered in
// a process-wide Mutex-protected list. `gos_rt_gc_reset()` sweeps
// the full list and deallocates. Used for leak detection (valgrind),
// memory profiling, and as the hook point for future safepoint GC.
// NOT safe to call mid-execution when cross-goroutine pointers exist
// — see the safety contract on `gos_rt_gc_reset`.
//
// `gos_rt_gc_deregister(ptr)` removes a pointer from the registry
// when ownership transfers to a runtime structure that manages its
// own lifetime (e.g. GosVec's data buffer after Vec::from_raw_parts).

// ---------------------------------------------------------------
// Raw-pointer tracing GC for compiled-tier aggregates.
//
// Every `gos_rt_gc_alloc` / `gos_rt_aggr_alloc` allocation is
// registered in a process-wide HashMap<ptr → (size, mark)>. The
// drop pass remains the deterministic fast path: it emits
// `gos_rt_aggr_free` at scope exit, which deregisters + deallocates
// in O(1). Aggregates that escape their constructing scope, or
// participate in cycles, stay in the registry until a tracing
// `gos_rt_gc_collect()` reclaims them.
//
// Tracing model (conservative, à la Boehm):
// - Each thread maintains a raw-pointer shadow stack of live roots.
//   Codegen emits `gos_rt_gc_root_push` after every aggregate-typed
//   local assignment, plus `gos_rt_gc_root_save` at function entry
//   and `gos_rt_gc_root_restore` at every return / scope exit.
// - `gos_rt_gc_collect()` snapshots every thread's shadow stack,
//   clears every mark bit, then transitively marks each rooted
//   allocation. The transitive scan walks each marked allocation's
//   payload in pointer-sized words and treats any word whose value
//   matches a registered pointer as a reference. This is
//   conservative — it can keep dead allocations alive when an
//   integer happens to alias a heap pointer — but it does not need
//   precise per-type pointer-offset metadata and collects cycles
//   that the drop pass cannot.
// - Sweep walks the registry; every unmarked entry is deallocated.
//
// `gos_rt_gc_safepoint()` triggers a collect when the bytes
// allocated since the last collection cross a threshold. The
// existing concurrent-GC `gc.rs` machinery layers on top: STW
// remains the production path here.
//
// `gos_rt_gc_reset()` retains its semantics — drain every
// registered allocation. Used at program teardown and from tests.
//
// `GOS_GC=leak` disables tracking entirely (allocator-only mode,
// for benchmarks).
// ---------------------------------------------------------------

// ---------------------------------------------------------------
// GC error type, fail-closed Layout helper, generation counter.
// ---------------------------------------------------------------

/// Errors the raw-pointer tracing GC can surface across the FFI
/// boundary. All variants are recovered to a null-pointer return
/// for `gos_rt_gc_alloc` or a silent no-op for `gos_rt_aggr_free`
/// — the runtime never panics across `extern "C"`.
#[derive(Debug, Clone, Copy)]
enum GcError {
    /// `Layout::from_size_align` rejected the size + alignment
    /// pair. Either `size` was zero (handled separately by the
    /// public entry points), `align` was not a power of two, or
    /// the rounded-up size exceeded `isize::MAX`.
    LayoutOverflow,
}

/// Word size on the supported targets (x86_64, aarch64). The
/// runtime ABI hard-codes 8-byte alignment for every aggregate
/// allocation; the marker depends on this for word-granular
/// payload scans.
const WORD_BYTES: usize = std::mem::size_of::<usize>();

/// Hard ceiling on a single aggregate allocation (1 GiB). Any
/// `gos_rt_gc_alloc(size)` call with `size > MAX_AGGR_BYTES`
/// returns null; the registry integrity check refuses to ratify
/// an entry whose stored size exceeds this. Generous enough that
/// no real user program will hit it; tight enough to catch
/// corruption-induced size drift before the marker reads out
/// of bounds.
const MAX_AGGR_BYTES: usize = 1 << 30;

/// Hard ceiling on the live aggregate count. The integrity check
/// fires when the registry grows past this; production code will
/// see a clean abort rather than slow degradation into swap.
const MAX_REGISTRY_ENTRIES: usize = 1 << 26;

/// Per-thread shadow-stack capacity. Pushes past the cap trigger
/// an immediate stop-the-world collect to bound the live heap
/// (the cap itself is not lifted by the collect — function
/// returns lift it). Tunable via `GOS_GC_SHADOW_MAX`; default
/// `1 << 20` entries (~8 MiB at 8 bytes/entry).
fn shadow_stack_cap() -> usize {
    static CAP: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *CAP.get_or_init(|| {
        std::env::var("GOS_GC_SHADOW_MAX")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(1 << 20)
    })
}

/// Validated 8-byte-aligned layout for a `size`-byte aggregate.
/// Failure modes:
/// - `size == 0` → `LayoutOverflow` (callers handle zero
///   separately; this helper assumes a meaningful payload).
/// - `size > MAX_AGGR_BYTES` → `LayoutOverflow`.
/// - rounded-up size exceeds `isize::MAX` → `LayoutOverflow`.
///
/// `Layout::from_size_align_unchecked` is gone from the GC code
/// path — every call site routes through this helper so a
/// pathological size (attacker-controlled or codegen drift)
/// cannot reach the allocator with a malformed layout.
fn aggregate_layout(size: usize) -> Result<Layout, GcError> {
    if size == 0 || size > MAX_AGGR_BYTES {
        return Err(GcError::LayoutOverflow);
    }
    Layout::from_size_align(size, WORD_BYTES).map_err(|_| GcError::LayoutOverflow)
}

/// Monotonically-increasing generation counter. Every allocation
/// is stamped at `insert` time; every removal at sweep / free
/// time bumps it. The marker uses the (address, generation) pair
/// when deciding whether a candidate pointer is still the entry
/// it captured — ABA protection without per-allocation
/// `AtomicU64`s.
static GENERATION_COUNTER: AtomicU64 = AtomicU64::new(1);

fn next_generation() -> u64 {
    let g = GENERATION_COUNTER.fetch_add(1, Ordering::Relaxed);
    g.wrapping_add(1)
}

// ---------------------------------------------------------------
// Raw-pointer tracing GC for compiled-tier aggregates.
// ---------------------------------------------------------------

static GC_TRACK_ENABLED: AtomicBool = AtomicBool::new(false);
static GC_TRACK_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();
static GC_BYTES_SINCE_LAST_COLLECT: AtomicUsize = AtomicUsize::new(0);

/// Bytes allocated between safepoint-driven collects. Tunable via
/// `GOS_GC_THRESHOLD=<bytes>` (default 4 MiB).
static GC_COLLECT_THRESHOLD: std::sync::OnceLock<usize> = std::sync::OnceLock::new();

fn gc_collect_threshold() -> usize {
    *GC_COLLECT_THRESHOLD.get_or_init(|| {
        std::env::var("GOS_GC_THRESHOLD")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(4 * 1024 * 1024)
    })
}

/// True when the raw-pointer tracing GC is active. Default: ON.
/// `GOS_GC=leak` opts out (used by benchmarks that measure raw
/// allocator cost). The legacy `GOS_GC_TRACK=1` flag stays
/// recognised so existing scripts continue to work; the only
/// observable difference now is that tracking is also on by
/// default.
fn gc_track_enabled() -> bool {
    GC_TRACK_INIT.get_or_init(|| {
        let leak = std::env::var_os("GOS_GC").is_some_and(|v| v == "leak");
        let on = !leak;
        GC_TRACK_ENABLED.store(on, Ordering::Relaxed);
    });
    GC_TRACK_ENABLED.load(Ordering::Relaxed)
}

/// One entry in the per-aggregate registry. `mark` is the
/// current cycle's reachability bit; `generation` is the ABA
/// stamp the marker compares against snapshotted roots.
///
/// `pointer_mask` is optional precise-trace metadata: when
/// present, only the word offsets it lists are read as
/// candidate pointers during the mark phase. This eliminates
/// the false-retention hazard of the conservative payload-word
/// scan (where any `i64` payload field that numerically matches
/// a live allocation pins that allocation). When `None`, the
/// marker falls back to the conservative scan — preserving the
/// existing behaviour for allocations that codegen hasn't yet
/// produced a layout description for.
#[derive(Debug, Clone)]
struct AllocEntry {
    size: usize,
    mark: bool,
    generation: u64,
    /// Word offsets in the payload that are pointer slots.
    /// `Some(vec)` engages precise tracing; `None` falls back
    /// to conservative.
    pointer_mask: Option<Vec<u32>>,
}

/// Newtype around the raw allocation address. Stored as `usize`
/// so the registry's `HashMap` is structurally `Send + Sync`
/// without a bespoke `unsafe impl`. The marker is the only code
/// path that converts a `PtrKey` back to a pointer, and only
/// inside `with_audited_ptr` after the registry lookup has
/// confirmed the address + generation match.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(transparent)]
struct PtrKey(usize);

impl PtrKey {
    fn from_raw(ptr: *mut u8) -> Self {
        PtrKey(ptr as usize)
    }
    #[cfg(test)]
    fn as_addr(self) -> usize {
        self.0
    }
}

type AllocRegistry = std::collections::HashMap<PtrKey, AllocEntry>;

static GC_ALLOC_REGISTRY: std::sync::OnceLock<parking_lot::Mutex<AllocRegistry>> =
    std::sync::OnceLock::new();

fn gc_registry() -> &'static parking_lot::Mutex<AllocRegistry> {
    GC_ALLOC_REGISTRY.get_or_init(|| parking_lot::Mutex::new(AllocRegistry::new()))
}

/// Per-thread shadow stack of raw-pointer GC roots. Stored as
/// `usize` so the wrapping `ThreadRoots` struct is structurally
/// `Send + Sync` (the underlying `parking_lot::Mutex<Vec<usize>>`
/// is `Send + Sync` by composition). The marker converts back to
/// `*mut u8` only through `with_audited_ptr`, which validates
/// the address against the registry under the registry lock.
struct ThreadRoots {
    stack: parking_lot::Mutex<Vec<usize>>,
}

type ThreadRootsRegistry = parking_lot::Mutex<Vec<std::sync::Arc<ThreadRoots>>>;
static GC_THREAD_ROOTS: std::sync::OnceLock<ThreadRootsRegistry> = std::sync::OnceLock::new();

fn gc_thread_roots_registry() -> &'static ThreadRootsRegistry {
    GC_THREAD_ROOTS.get_or_init(|| parking_lot::Mutex::new(Vec::new()))
}

// ---------------------------------------------------------------
// HashMap-as-container roots
// ---------------------------------------------------------------
//
// `GosMap` is allocated via `Box::into_raw` (in `gos_rt_map_new` /
// `_with_capacity`) and lives outside the GC registry — its storage
// buckets are managed by Rust's allocator, not ours. That means
// the conservative `scan_payload_words` walk cannot reach the
// heap-allocated struct values the user inserted into the map: it
// only sees the bytes of the `GosMap` struct itself, never the
// Rust-side `FxHashMap<K, V>` buckets.
//
// To keep HashMap-stored aggregate values reachable, every live
// `GosMap` registers its address here at construction. The
// tracing collector's mark phase performs a second pass after
// draining the normal worklist: for each registered map, it locks
// the storage and treats every 8-byte value as a candidate root.
// `scan_payload_words`'s registry-presence check filters
// non-pointer values (raw i64, char codes), so the trace is
// conservative without overestimating reachability for primitive
// maps (HashMap<_, i64>).
type GosMapRegistry = parking_lot::Mutex<std::collections::HashSet<usize>>;
static GOS_MAP_REGISTRY: std::sync::OnceLock<GosMapRegistry> = std::sync::OnceLock::new();

fn gos_map_registry() -> &'static GosMapRegistry {
    GOS_MAP_REGISTRY.get_or_init(|| parking_lot::Mutex::new(std::collections::HashSet::new()))
}

/// Records `addr` as a live `GosMap` whose stored values must be
/// scanned by the tracing collector. Called from `gos_rt_map_new`
/// and `gos_rt_map_new_with_capacity`. No-op when GC tracking is
/// disabled (`GOS_GC=leak`).
pub(super) fn gos_map_register(addr: *mut u8) {
    if !gc_track_enabled() || addr.is_null() {
        return;
    }
    gos_map_registry().lock().insert(addr as usize);
}

/// Deregisters a `GosMap` when the user code drops the map.
/// Idempotent on a never-registered address. Called from
/// `gos_rt_map_free`.
pub(super) fn gos_map_deregister(addr: *mut u8) {
    if addr.is_null() {
        return;
    }
    if let Some(reg) = GOS_MAP_REGISTRY.get() {
        reg.lock().remove(&(addr as usize));
    }
}

// Lock-holding scan_all_gos_maps retired by the snapshot-based
// `scan_all_gos_maps_snapshot` (see below); the GC now releases
// the registry lock for the entire mark phase.

thread_local! {
    static LOCAL_ROOTS: std::cell::RefCell<Option<std::sync::Arc<ThreadRoots>>> =
        const { std::cell::RefCell::new(None) };
}

fn with_local_roots<R>(f: impl FnOnce(&ThreadRoots) -> R) -> R {
    LOCAL_ROOTS.with(|cell| {
        if cell.borrow().is_none() {
            let arc = std::sync::Arc::new(ThreadRoots {
                stack: parking_lot::Mutex::new(Vec::new()),
            });
            gc_thread_roots_registry()
                .lock()
                .push(std::sync::Arc::clone(&arc));
            *cell.borrow_mut() = Some(arc);
        }
        let borrow = cell.borrow();
        let arc = borrow.as_ref().expect("LOCAL_ROOTS just initialised");
        f(arc)
    })
}

/// Pushes a single raw-pointer root onto the current thread's
/// shadow stack. Idempotent on null (a null root is recorded
/// verbatim and skipped by the marker). Codegen emits one of
/// these immediately after every aggregate-typed local
/// assignment.
///
/// When the per-thread stack reaches [`shadow_stack_cap`], the
/// helper runs a stop-the-world collect before pushing. The cap
/// itself is not lifted (function returns do that), but the
/// collect bounds the live heap so adversarial inputs that
/// inflate the stack between returns cannot OOM.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_root_push(ptr: *mut u8) {
    ffi_entry!((), {
        if !gc_track_enabled() {
            return;
        }
        let addr = ptr as usize;
        let need_collect = with_local_roots(|r| {
            let mut stack = r.stack.lock();
            let at_cap = stack.len() >= shadow_stack_cap();
            stack.push(addr);
            at_cap
        });
        if need_collect {
            let _ = gos_rt_gc_collect();
        }
    });
}

/// Returns the current depth of the calling thread's shadow
/// stack. Codegen emits this at function entry and stores the
/// returned token in a frame-local slot; the matching
/// `gos_rt_gc_root_restore(token)` at every return / scope exit
/// truncates the stack back to the saved depth.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_root_save() -> u64 {
    ffi_entry!(0, {
        if !gc_track_enabled() {
            return 0;
        }
        with_local_roots(|r| u64::try_from(r.stack.lock().len()).unwrap_or(u64::MAX))
    })
}

/// Truncates the calling thread's shadow stack to `frame` entries.
/// Cheap O(1); the underlying Vec keeps its capacity so the next
/// function call avoids reallocation.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_root_restore(frame: u64) {
    ffi_entry!((), {
        if !gc_track_enabled() {
            return;
        }
        with_local_roots(|r| {
            let target = usize::try_from(frame).unwrap_or(usize::MAX);
            let mut stack = r.stack.lock();
            if target < stack.len() {
                stack.truncate(target);
            }
        });
    });
}

// The previous `scan_payload_words(addr, &AllocRegistry, ...)`
// helper has been retired in favour of the snapshot-based
// `scan_payload_words_snapshot` below. The GC's mark phase
// no longer touches the live registry; the brief sweep window
// is the only place the lock is held.

/// Conservative payload-word scan against a frozen snapshot
/// `{addr -> (size, generation)}` rather than the live registry.
/// The entire transitive walk can run without holding the
/// cross-thread allocation lock. Mutators may allocate while we
/// walk; those new allocations are absent from the snapshot and
/// therefore never reclaimed in this cycle (correct — they are
/// at most one safepoint old).
/// Per-allocation snapshot row: size, generation, and optional
/// pointer-offset mask (word indices). Carrying the mask in the
/// snapshot keeps the mark phase entirely lock-free — neither
/// the registry nor the pointer-mask buffers are dereferenced
/// off-snapshot.
#[derive(Clone, Debug)]
struct AllocSnapshot {
    size: usize,
    generation: u64,
    pointer_mask: Option<Vec<u32>>,
}

fn scan_payload_words_snapshot(
    addr: usize,
    snapshot: &std::collections::HashMap<usize, AllocSnapshot>,
    worklist: &mut Vec<(usize, u64)>,
) {
    let Some(entry) = snapshot.get(&addr) else {
        return;
    };
    let size = entry.size;
    // Precise tracing path: walk only the recorded pointer
    // offsets. Eliminates the false-retention hazard of the
    // conservative scan (where an `i64` payload field that
    // numerically matches a live allocation address pins it).
    if let Some(mask) = entry.pointer_mask.as_ref() {
        for &word_idx in mask {
            let byte_off = (word_idx as usize).saturating_mul(WORD_BYTES);
            if byte_off
                .checked_add(WORD_BYTES)
                .is_none_or(|end| end > size)
            {
                continue;
            }
            let word_ptr = (addr + byte_off) as *const usize;
            // SAFETY: `addr` was registered with `size`; the
            // bounds check above keeps the read inside the
            // allocation.
            let candidate = unsafe { core::ptr::read_unaligned(word_ptr) };
            if candidate != 0 {
                if let Some(child) = snapshot.get(&candidate) {
                    worklist.push((candidate, child.generation));
                }
            }
        }
        return;
    }
    // Conservative fallback path: walk every 8-byte word.
    let mut byte_off: usize = 0;
    while byte_off
        .checked_add(WORD_BYTES)
        .is_some_and(|end| end <= size)
    {
        let word_ptr = (addr + byte_off) as *const usize;
        // SAFETY: as above; `addr` registered with `size`.
        let candidate = unsafe { core::ptr::read_unaligned(word_ptr) };
        if candidate != 0 {
            if let Some(child) = snapshot.get(&candidate) {
                worklist.push((candidate, child.generation));
            }
        }
        byte_off += WORD_BYTES;
    }
}

/// Snapshot variant of [`scan_all_gos_maps`]. Looks up candidate
/// addresses in the snapshot rather than the live registry so the
/// caller can stay outside the registry critical section.
fn scan_all_gos_maps_snapshot(
    snapshot: &std::collections::HashMap<usize, AllocSnapshot>,
    worklist: &mut Vec<(usize, u64)>,
) {
    let Some(map_reg) = GOS_MAP_REGISTRY.get() else {
        return;
    };
    let map_addrs: Vec<usize> = map_reg.lock().iter().copied().collect();
    for addr in map_addrs {
        if addr == 0 {
            continue;
        }
        // SAFETY: same provenance contract as `scan_all_gos_maps`;
        // a registered map cannot be freed mid-cycle because the
        // free path itself runs at a safepoint.
        let map = unsafe { &*(addr as *const super::map::GosMap) };
        for value in super::map::storage_values_for_gc(map) {
            if value == 0 {
                continue;
            }
            let candidate = value as usize;
            if let Some(child) = snapshot.get(&candidate) {
                worklist.push((candidate, child.generation));
            }
        }
    }
}

/// Tracing collect — stop-the-world conservative mark + sweep
/// over the raw-pointer aggregate registry. Reclaims allocations
/// that escaped their constructing scope and any cycles between
/// them.
///
/// Implementation notes:
/// - Snapshot all threads' shadow stacks (as `(addr, expected_gen)`
///   pairs) under the registry lock so mutator pushes are
///   serialised behind the snapshot.
/// - Mark transitively via `scan_payload_words`, which validates
///   bounds, alignment, and generation per candidate.
/// - Sweep: walk the registry, dealloc unmarked entries, bump
///   their generation so any stale shadow-stack entry referring
///   to the reclaimed address is skipped on the next cycle.
///
/// Returns the number of bytes reclaimed.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_collect() -> u64 {
    ffi_entry!(0, {
        if !gc_track_enabled() {
            return 0;
        }
        gc_collect_with_buffers()
    })
}

/// Per-thread reusable buffers for the lock-free mark phase.
/// Allocating a fresh `HashMap` / `HashSet` / `Vec` on every
/// `gos_rt_gc_collect` cycle is a measurable allocator pressure
/// source for HashMap-heavy workloads (k-nucleotide, …) — every
/// collect was N×(40 B per snapshot entry) + the HashSet's
/// bucket overhead in transient memory. The thread_local holds
/// the buffers across cycles; each collect calls `.clear()` and
/// repopulates without freeing the bucket arrays.
struct CollectBuffers {
    snapshot: std::collections::HashMap<usize, AllocSnapshot>,
    marked: std::collections::HashSet<usize>,
    worklist: Vec<(usize, u64)>,
}

impl CollectBuffers {
    fn new() -> Self {
        Self {
            snapshot: std::collections::HashMap::new(),
            marked: std::collections::HashSet::new(),
            worklist: Vec::new(),
        }
    }

    fn reset_for_cycle(&mut self) {
        self.snapshot.clear();
        self.marked.clear();
        self.worklist.clear();
    }
}

thread_local! {
    static COLLECT_BUFFERS: std::cell::RefCell<CollectBuffers> =
        std::cell::RefCell::new(CollectBuffers::new());
}

fn gc_collect_with_buffers() -> u64 {
    COLLECT_BUFFERS.with(|cell| {
        let mut buffers = cell.borrow_mut();
        buffers.reset_for_cycle();
        let CollectBuffers {
            snapshot,
            marked,
            worklist,
        } = &mut *buffers;
        gc_collect_inner(snapshot, marked, worklist)
    })
}

fn gc_collect_inner(
    snapshot: &mut std::collections::HashMap<usize, AllocSnapshot>,
    marked: &mut std::collections::HashSet<usize>,
    worklist: &mut Vec<(usize, u64)>,
) -> u64 {
    {
        // Phase 0: snapshot {addr -> (size, generation)} under a
        // brief registry lock. Mutator allocations after this
        // point are absent from the snapshot and are therefore
        // never reclaimed in this cycle — that's correct (they
        // are at most one safepoint old). This replaces holding
        // the registry mutex across the entire mark+sweep, which
        // serialised every concurrent `gos_rt_gc_alloc` behind
        // the collector's full pause.
        let registry = gc_registry().lock();
        snapshot.reserve(registry.len());
        for (k, v) in registry.iter() {
            snapshot.insert(
                k.0,
                AllocSnapshot {
                    size: v.size,
                    generation: v.generation,
                    pointer_mask: v.pointer_mask.clone(),
                },
            );
        }
    }
    // Pre-size the marked set against the snapshot so the bucket
    // arrays don't grow during the mark phase.
    marked.reserve(snapshot.len());

    // Phase 1: seed the worklist from every thread's shadow
    // stack. Walked lock-free against the snapshot — pushes
    // that arrive concurrently land on the live registry, not
    // on the snapshot, and are therefore ineligible for
    // reclaim in this cycle.
    {
        let threads = gc_thread_roots_registry().lock();
        for t in threads.iter() {
            let stack = t.stack.lock();
            for &addr in stack.iter() {
                if addr == 0 {
                    continue;
                }
                if let Some(entry) = snapshot.get(&addr) {
                    worklist.push((addr, entry.generation));
                }
            }
        }
    }

    // Phase 2: transitive mark, lock-free against the snapshot.
    while let Some((addr, expected_gen)) = worklist.pop() {
        let Some(entry) = snapshot.get(&addr) else {
            continue;
        };
        if entry.generation != expected_gen {
            continue;
        }
        if !marked.insert(addr) {
            continue;
        }
        scan_payload_words_snapshot(addr, snapshot, worklist);
    }

    // Phase 2b: HashMap-as-container roots. Maps store their
    // values in Rust-owned buckets the conservative word-scan
    // cannot see through; emit each value as a candidate and
    // re-drain the worklist. Lock-free against the snapshot.
    scan_all_gos_maps_snapshot(snapshot, worklist);
    while let Some((addr, expected_gen)) = worklist.pop() {
        let Some(entry) = snapshot.get(&addr) else {
            continue;
        };
        if entry.generation != expected_gen {
            continue;
        }
        if !marked.insert(addr) {
            continue;
        }
        scan_payload_words_snapshot(addr, snapshot, worklist);
    }

    // Phase 3: sweep — under a fresh registry lock, dealloc
    // every entry in the snapshot that was *not* marked. The
    // generation check rejects addresses whose alloc has been
    // freed-and-reused between snapshot and sweep (so we
    // never dealloc a stranger's allocation).
    let mut bytes_reclaimed: u64 = 0;
    {
        let mut registry = gc_registry().lock();
        for (addr, snap_entry) in snapshot.iter() {
            if marked.contains(addr) {
                continue;
            }
            let Some(entry) = registry.get(&PtrKey(*addr)) else {
                continue;
            };
            if entry.size != snap_entry.size || entry.generation != snap_entry.generation {
                // Reused since snapshot; skip — the new owner
                // is a live, ineligible-this-cycle allocation.
                continue;
            }
            let reclaimed_size = snap_entry.size;
            registry.remove(&PtrKey(*addr));
            let Ok(layout) = aggregate_layout(reclaimed_size) else {
                continue;
            };
            // SAFETY:
            // - Provenance: `addr as *mut u8` came from
            //   `alloc_zeroed(layout)` in `gos_rt_gc_alloc`.
            // - Aliasing: we hold the registry Mutex; no other
            //   code path is currently dereferencing this
            //   allocation (it's unmarked, so no root pointed
            //   at it after the mark phase) and the generation
            //   check guarantees the alloc hasn't been reused.
            // - Synchronization: registry Mutex.
            // - Failure mode: a corrupted (addr, size) pair
            //   would produce dealloc UB. The integrity check
            //   rejects such pairs at insert time under
            //   debug_assertions.
            unsafe { dealloc(*addr as *mut u8, layout) };
            let _ = next_generation();
            bytes_reclaimed = bytes_reclaimed.saturating_add(reclaimed_size as u64);
        }

        // Clear surviving entries' `mark` bits (held over from
        // older single-lock implementations) so the integrity
        // walker invariant `!entry.mark` still holds. With
        // marks now tracked in the local `marked` HashSet, the
        // registry's `mark` field is reset state only.
        for entry in registry.values_mut() {
            entry.mark = false;
        }

        #[cfg(debug_assertions)]
        {
            assert_registry_consistent_locked(&registry);
        }
    }

    GC_BYTES_SINCE_LAST_COLLECT.store(0, Ordering::Relaxed);

    bytes_reclaimed
}

/// Returns the number of currently-tracked allocations. Test /
/// diagnostic only.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_alloc_count() -> u64 {
    ffi_entry!(0, {
        if !gc_track_enabled() {
            return 0;
        }
        u64::try_from(gc_registry().lock().len()).unwrap_or(u64::MAX)
    })
}

/// Debug-only integrity check. Walks the registry asserting that
/// every entry has a well-formed size, a non-zero generation,
/// and the post-sweep invariant `mark == false`. Called
/// automatically at the end of every `gos_rt_gc_collect` under
/// `debug_assertions`; tests may call it explicitly.
///
/// In release builds this is a no-op — the assertions compile
/// away.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_assert_consistent() {
    ffi_entry!((), {
        if !gc_track_enabled() {
            return;
        }
        let registry = gc_registry().lock();
        let len = registry.len();
        debug_assert!(
            len <= MAX_REGISTRY_ENTRIES,
            "GC registry exceeded MAX_REGISTRY_ENTRIES ({len} > {MAX_REGISTRY_ENTRIES}); \
             possible leak or runaway allocation"
        );
        for (key, entry) in registry.iter() {
            let size = entry.size;
            let generation = entry.generation;
            debug_assert!(
                entry.size > 0,
                "GC registry corruption: zero-size entry at {key:?}"
            );
            debug_assert!(
                entry.size <= MAX_AGGR_BYTES,
                "GC registry corruption: oversized entry size={size} at {key:?}"
            );
            debug_assert!(
                entry.generation > 0,
                "GC registry corruption: zero generation={generation} at {key:?}"
            );
        }
    });
}

/// Internal variant of [`gos_rt_gc_assert_consistent`] that
/// borrows the already-held registry mutex. Used from inside
/// `gos_rt_gc_collect` so the consistency check runs without
/// re-acquiring the lock.
#[cfg(debug_assertions)]
fn assert_registry_consistent_locked(registry: &AllocRegistry) {
    for (key, entry) in registry {
        let size = entry.size;
        let generation = entry.generation;
        debug_assert!(
            entry.size > 0,
            "GC registry corruption: zero-size entry at {key:?}"
        );
        debug_assert!(
            entry.size <= MAX_AGGR_BYTES,
            "GC registry corruption: oversized entry size={size} at {key:?}"
        );
        debug_assert!(
            entry.generation > 0,
            "GC registry corruption: zero generation={generation} at {key:?}"
        );
        debug_assert!(
            !entry.mark,
            "GC registry corruption: mark bit set on survivor after sweep at {key:?}"
        );
    }
}

/// Write barrier for heap-pointer stores. Future concurrent-mark
/// collectors need to shade the target whenever a mutator
/// overwrites a slot during the marking phase. The current STW
/// collector has no need for this — it pauses mutators across
/// the entire mark + sweep — but the symbol exists so the
/// codegen can route every aggregate-pointer store through this
/// helper, allowing the concurrent path to be enabled later
/// with a single runtime change.
///
/// Today's implementation is a straight store. Codegen emits
/// the barrier behind `GOSSAMER_WRITE_BARRIER=1`; without the
/// flag the store is a plain `mov` and this symbol is unused.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_write_barrier_ptr(slot: *mut *mut u8, new_val: *mut u8) {
    ffi_entry!((), {
        if slot.is_null() {
            return;
        }
        // SAFETY:
        // - Provenance: `slot` is a heap-pointer slot inside an
        //   aggregate the caller owns. The codegen-emitted call
        //   site guarantees the slot is within a registered
        //   allocation.
        // - Aliasing: the store is the only access to `*slot` at
        //   this point — codegen serialises it with surrounding
        //   reads.
        // - Synchronization: under the current STW collector,
        //   the mutator owns the slot (no concurrent marker).
        // - Failure mode: a stale `slot` (registered allocation
        //   freed by sweep before the store runs) would write
        //   into reclaimed memory. The drop pass + safepoint
        //   discipline ensures `slot` is rooted via the shadow
        //   stack for the duration of the store.
        unsafe { *slot = new_val };
    });
}

/// Allocates `size` zeroed bytes for a user-struct instance.
///
/// Aggregates allocated via this entry point are registered in
/// the process-wide tracing GC registry. The MIR drop pass emits
/// `gos_rt_aggr_free` at end-of-scope for owning locals, which
/// deregisters and `dealloc`s the block in O(1). Aggregates that
/// escape their constructing scope (returned, stored in a
/// container, captured in a closure) or that form cycles are
/// reclaimed by the tracing collector — either at the next
/// safepoint-triggered `gos_rt_gc_collect` or at process exit
/// via `gos_rt_gc_reset`.
///
/// Set `GOS_GC=leak` to disable tracking (matches pre-0.6
/// Box-leak behaviour) for benchmarks that measure raw
/// allocator cost.
///
/// Eight-byte alignment satisfies all scalar fields (i64, f64, ptr).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_alloc(size: u64) -> *mut u8 {
    ffi_entry!(std::ptr::null_mut(), {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let size = size as usize;
        let Ok(layout) = aggregate_layout(size) else {
            return std::ptr::null_mut();
        };
        // SAFETY:
        // - Provenance: layout came from `aggregate_layout`,
        //   which validated size > 0, size <= MAX_AGGR_BYTES,
        //   and align is a power of two ≤ usize::MAX/2.
        // - Aliasing: this is the unique allocation site; the
        //   returned pointer is handed to a single caller.
        // - Synchronization: the global allocator is internally
        //   thread-safe; no external lock needed.
        // - Failure mode: `alloc_zeroed` returns null on OOM.
        //   We abort rather than calling `handle_alloc_error` because
        //   the latter panics, and panic-across-FFI from this `gos_rt_*`
        //   entry into compiled Gossamer code is UB.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            eprintln!(
                "gossamer runtime: OOM in gos_rt_gc_alloc (size={}, align={}); aborting",
                layout.size(),
                layout.align()
            );
            std::process::abort();
        }
        if gc_track_enabled() {
            let generation = next_generation();
            gc_registry().lock().insert(
                PtrKey::from_raw(ptr),
                AllocEntry {
                    size,
                    mark: false,
                    generation,
                    pointer_mask: None,
                },
            );
            GC_BYTES_SINCE_LAST_COLLECT.fetch_add(size, Ordering::Relaxed);
        }
        ptr
    })
}

/// Like [`gos_rt_gc_alloc`] but records a precise pointer-offset
/// bitmap so the tracing collector reads only the pointer slots
/// in the payload during mark, eliminating the false-retention
/// hazard of the conservative word scan. `mask_words` is a
/// pointer to a contiguous `u32` array of `mask_len` word
/// offsets (each offset multiplied by `WORD_BYTES` gives the
/// byte offset into the payload). MIR codegen emits this from
/// the per-type layout description.
///
/// Returns `null` on OOM (same contract as `gos_rt_gc_alloc`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_alloc_traced(
    size: u64,
    mask_words: *const u32,
    mask_len: u64,
) -> *mut u8 {
    ffi_entry!(std::ptr::null_mut(), {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let size_usize = size as usize;
        let Ok(layout) = aggregate_layout(size_usize) else {
            return std::ptr::null_mut();
        };
        // SAFETY: same allocator invariants as `gos_rt_gc_alloc`.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            eprintln!(
                "gossamer runtime: OOM in gos_rt_gc_alloc_traced (size={}, align={}); aborting",
                layout.size(),
                layout.align()
            );
            std::process::abort();
        }
        let mask = if mask_words.is_null() || mask_len == 0 {
            None
        } else {
            let len_usize = mask_len.min(u64::from(u32::MAX)) as usize;
            // SAFETY: caller asserts `mask_words` points at a
            // contiguous `len_usize`-element `u32` array. The
            // values are read-only and the slice is consumed
            // immediately (copied into the `Vec`), so caller's
            // buffer needn't outlive this call.
            let words: &[u32] = unsafe { core::slice::from_raw_parts(mask_words, len_usize) };
            Some(words.to_vec())
        };
        if gc_track_enabled() {
            let generation = next_generation();
            gc_registry().lock().insert(
                PtrKey::from_raw(ptr),
                AllocEntry {
                    size: size_usize,
                    mark: false,
                    generation,
                    pointer_mask: mask,
                },
            );
            GC_BYTES_SINCE_LAST_COLLECT.fetch_add(size_usize, Ordering::Relaxed);
        }
        ptr
    })
}

/// Allocates `size` zeroed bytes for a user-aggregate (struct,
/// tuple, enum payload) whose lifetime is tied to a MIR local.
/// Routes through `gos_rt_gc_alloc` so allocation tracking and
/// alignment match.
///
/// Symmetric with [`gos_rt_aggr_free`]: every allocation made via
/// this function is reclaimed by either an explicit `gos_rt_aggr_free`
/// (emitted by the MIR drop pass at scope exit) or by the
/// tracing collector at the next `gos_rt_gc_collect`.
/// Load-bearing distinctness: a bare wrapper around
/// `gos_rt_gc_alloc` is ICF-folded by the linker into the same
/// function. Once the symbol collapses, the user-side LLVM IR's
/// `call @gos_rt_aggr_alloc` resolves to the same address as
/// `gos_rt_gc_alloc`, the linker can prove the heap return is
/// only used to memcpy a stack alloca, and removes the entire
/// heap-copy + memcpy chain — leaving HashMap inserts storing
/// stack pointers that go dangling on the inserter's return.
///
/// `#[inline(never)]` keeps the wrapper out of the caller's
/// body. A `compiler_fence(SeqCst)` plus a distinct atomic-load
/// keeps ICF from folding the wrapper into `gos_rt_gc_alloc`:
/// the bodies are no longer instruction-identical and the
/// `compiler_fence` blocks LLVM from reasoning across the call
/// boundary.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn gos_rt_aggr_alloc(size: u64) -> *mut u8 {
    // ICF-anchor: distinct atomic + fence so the linker does
    // not identify this function with `gos_rt_gc_alloc` and
    // the optimiser cannot prove the heap return is dead.
    static ANCHOR: AtomicUsize = AtomicUsize::new(0);
    ANCHOR.fetch_add(1, Ordering::SeqCst);
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    ffi_entry!(std::ptr::null_mut(), { gos_rt_gc_alloc(size) })
}

// `#[used]` anchor — without this, `--gc-sections` strips the
// distinct `gos_rt_aggr_alloc` symbol once ICF folds its body
// into `gos_rt_gc_alloc`, then dead-strip kills the original
// because nothing in the runtime staticlib references it
// directly. The reference here pins the symbol so user
// LLVM IR's `call @gos_rt_aggr_alloc` keeps resolving to
// our distinct wrapper.
#[used]
static GOS_RT_AGGR_ALLOC_KEEP: extern "C" fn(u64) -> *mut u8 = gos_rt_aggr_alloc;

/// Allocates `size` zeroed bytes for an aggregate whose only
/// surviving handle is going to escape the GC's reachability
/// graph — typically a struct value being stored as an i64 in
/// a HashMap, where the runtime can't currently teach the
/// tracing collector to walk through the MapStorage's
/// Rust-managed buckets.
///
/// Skips the GC registry entirely so the tracing collector
/// can't reclaim the block; the cost is that the allocation
/// leaks until process exit (when `gos_rt_gc_reset` walks the
/// global allocator's reserve). Use only when the alternative
/// is a silent miscompile via a dangling stack pointer; the
/// general aggregate path stays on `gos_rt_aggr_alloc`.
/// Same ICF / inlining concerns as `gos_rt_aggr_alloc`; see that
/// function's comment block. The distinct anchor here uses a
/// separate static so the two wrappers remain non-foldable.
#[unsafe(no_mangle)]
#[inline(never)]
pub extern "C" fn gos_rt_aggr_alloc_leak(size: u64) -> *mut u8 {
    static ANCHOR: AtomicUsize = AtomicUsize::new(0);
    ANCHOR.fetch_add(1, Ordering::SeqCst);
    std::sync::atomic::compiler_fence(Ordering::SeqCst);
    ffi_entry!(std::ptr::null_mut(), {
        if size == 0 {
            return std::ptr::null_mut();
        }
        let size = size as usize;
        let Ok(layout) = aggregate_layout(size) else {
            return std::ptr::null_mut();
        };
        // SAFETY: same shape as `gos_rt_gc_alloc`'s `alloc_zeroed`
        // call (size validated by `aggregate_layout`, allocator
        // is thread-safe). The deliberate omission is the
        // registry insert — the caller has accepted leak
        // semantics in exchange for indefinite lifetime.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            eprintln!(
                "gossamer runtime: OOM in gos_rt_aggr_alloc_leak (size={}, align={}); aborting",
                layout.size(),
                layout.align()
            );
            std::process::abort();
        }
        ptr
    })
}

/// Companion `#[used]` anchor for the leak variant — same
/// rationale as `GOS_RT_AGGR_ALLOC_KEEP`.
#[used]
static GOS_RT_AGGR_ALLOC_LEAK_KEEP: extern "C" fn(u64) -> *mut u8 = gos_rt_aggr_alloc_leak;

/// Reclaims an aggregate allocation made by `gos_rt_aggr_alloc` /
/// `gos_rt_gc_alloc`. Idempotent on null. The MIR drop pass emits
/// this at end-of-scope and before reassignment for every
/// Adt/Tuple/Array-typed owning local that has not escaped its
/// constructing frame.
///
/// `size` must match the allocation's original size in bytes; the
/// MIR pass derives it from `type_slot_count(ty) * 8`. The fast
/// path skips the tracked-registry deregister when tracking is
/// disabled (the `GOS_GC=leak` opt-out); otherwise the helper
/// removes the entry in O(1) and frees, ensuring the next
/// tracing collect does not double-free. A short-circuit on
/// registry-miss prevents double-free when a prior tracing
/// collect already reclaimed the entry.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_aggr_free(ptr: *mut u8, size: u64) {
    ffi_entry!((), {
        if ptr.is_null() || size == 0 {
            return;
        }
        let size = size as usize;
        if gc_track_enabled() {
            // O(1) deregister via HashMap removal. If the entry is
            // missing (because a prior tracing collect already
            // reclaimed it), short-circuit so we do not double-free.
            let removed = gc_registry().lock().remove(&PtrKey::from_raw(ptr));
            if removed.is_none() {
                return;
            }
            // Bump generation so any stale shadow-stack snapshot
            // referring to this address is rejected on the next
            // mark cycle.
            let _ = next_generation();
        }
        let Ok(layout) = aggregate_layout(size) else {
            return;
        };
        // SAFETY:
        // - Provenance: `ptr` was returned by `alloc_zeroed` with
        //   this exact layout (registered in the registry under
        //   that same size; the dropper guarantees a matching
        //   call).
        // - Aliasing: registry removal happened above, so no other
        //   code path is currently using this allocation.
        // - Synchronization: registry lock released after removal;
        //   the allocation is now owned by this thread.
        // - Failure mode: a mismatched `size` from codegen drift
        //   would produce dealloc UB. The integrity check
        //   verifies stored sizes; a mismatch would also fail the
        //   `removed.is_none()` short-circuit.
        unsafe { dealloc(ptr, layout) };
    });
}

/// Frees all allocations currently in the GC registry.
///
/// Safety contract: must only be called at a safepoint where no live
/// Gossamer pointer from any goroutine was allocated via
/// `gos_rt_gc_alloc` and still reachable. The compiled tier does not
/// auto-emit calls to this symbol; callers must honour the invariant
/// manually. Violating it produces use-after-free.
///
/// A no-op when `GOS_GC=leak` is set (tracking disabled).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_reset() {
    ffi_entry!((), {
        if !gc_track_enabled() {
            return;
        }
        let mut registry = gc_registry().lock();
        let entries: Vec<(usize, usize)> = registry.drain().map(|(k, v)| (k.0, v.size)).collect();
        for (addr, size) in entries {
            let Ok(layout) = aggregate_layout(size) else {
                continue;
            };
            // SAFETY: see `gos_rt_aggr_free`'s safety block.
            unsafe { dealloc(addr as *mut u8, layout) };
            let _ = next_generation();
        }
        GC_BYTES_SINCE_LAST_COLLECT.store(0, Ordering::Relaxed);
    });
}

/// Removes `ptr` from the GC registry when ownership of the block
/// transfers to a runtime structure that manages its own lifetime.
///
/// Called after `Vec::from_raw_parts` takes over a `gos_rt_gc_alloc`
/// buffer: the Vec's drop impl will call `dealloc`; without
/// deregistering, `gos_rt_gc_reset()` would double-free.
/// A no-op when tracking is disabled.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_deregister(ptr: *mut u8) {
    ffi_entry!((), {
        if ptr.is_null() || !gc_track_enabled() {
            return;
        }
        if gc_registry()
            .lock()
            .remove(&PtrKey::from_raw(ptr))
            .is_some()
        {
            let _ = next_generation();
        }
    });
}

/// Bytes allocated since the last collection. Used by
/// `gos_rt_gc_safepoint` to decide when to trigger a collect.
fn gc_bytes_since_last_collect() -> usize {
    GC_BYTES_SINCE_LAST_COLLECT.load(Ordering::Relaxed)
}

/// Threshold-driven safepoint hook for the raw-pointer tracing
/// GC. Codegen emits a call at every function prologue and every
/// loop back-edge; the call is a cheap atomic-load + compare in
/// the common case (under threshold, no collect). When the
/// threshold is crossed, runs a full STW mark + sweep.
///
/// Separate from `crate::gc::gos_rt_gc_safepoint` which drives
/// the handle-based concurrent collector; that symbol calls this
/// one as well so a single safepoint emit reaches both
/// collectors.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_gc_raw_safepoint() {
    ffi_entry!((), {
        if !gc_track_enabled() {
            return;
        }
        if gc_bytes_since_last_collect() >= gc_collect_threshold() {
            let _ = gos_rt_gc_collect();
        }
    });
}

/// Legacy arena watermark — returns 0 (the "no checkpoint" value).
/// LLVM codegen still wraps aggregate-returning user calls with
/// `arena_save`/`arena_restore`; the calls are now no-ops.
/// Eventually the LLVM emit pass should stop generating them
/// entirely; the symbol exists so existing compiled artefacts
/// continue to link.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_arena_save() -> u64 {
    ffi_entry!(0, { 0 })
}

/// Legacy arena rewind — no-op. See `gos_rt_arena_save`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_arena_restore(_saved: u64) {
    ffi_entry!((), {});
}

// `try_extend_last_cstring` was retired with the Box-leak
// allocator (see history); `gos_rt_str_concat` now allocates in
// one round trip via `alloc_cstring_from_slices`.

#[cfg(test)]
mod tracing_gc_tests {
    use super::*;
    // Every test in this module mutates the process-wide
    // tracing-GC registry. Serialise so the cargo test runner
    // (which executes tests in parallel by default) cannot
    // interleave allocations from different tests.
    static GC_TEST_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    fn force_tracking_on() {
        // The `gc_track_enabled` OnceLock latches its decision
        // on first call. In normal binaries the default is "on";
        // tests have to make sure the env hasn't been set to
        // "leak" by a sibling test process. Rust 2024 made
        // `std::env::remove_var` unsafe — fine in a test fixture
        // that runs before any goroutine spawns.
        // SAFETY: tests serialise via GC_TEST_LOCK so no
        // concurrent goroutine spawn observes the env mutation.
        unsafe { std::env::remove_var("GOS_GC") };
    }

    #[test]
    fn collect_reclaims_unrooted_allocation() {
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let ptr = gos_rt_gc_alloc(64);
        assert!(!ptr.is_null());
        assert_eq!(gos_rt_gc_alloc_count(), 1);
        // No root pushed — collect must reclaim it.
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 64);
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn collect_keeps_rooted_allocation_alive() {
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let ptr = gos_rt_gc_alloc(64);
        gos_rt_gc_root_push(ptr);
        assert_eq!(gos_rt_gc_alloc_count(), 1);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 0);
        assert_eq!(gos_rt_gc_alloc_count(), 1);
        gos_rt_gc_root_restore(frame);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 64);
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn collect_reclaims_self_referential_cycle() {
        // Cycle that the drop pass cannot reclaim: alloc A
        // stores a pointer to B in its first slot; B stores a
        // pointer to A in its first slot. Drop the only root
        // and call collect; both should be reclaimed.
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let a = gos_rt_gc_alloc(16);
        let b = gos_rt_gc_alloc(16);
        // SAFETY: each alloc is at least 16 bytes (one pointer
        // slot); writes stay within bounds.
        unsafe {
            a.cast::<*mut u8>().write(b);
            b.cast::<*mut u8>().write(a);
        }
        // Root only `a` — the cycle keeps `b` reachable via
        // a's first slot.
        gos_rt_gc_root_push(a);
        assert_eq!(gos_rt_gc_alloc_count(), 2);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 0, "rooted cycle must survive collect");
        assert_eq!(gos_rt_gc_alloc_count(), 2);
        // Drop root — both members of the cycle become
        // unreachable. The drop pass would have leaked them;
        // the tracing collector reclaims them.
        gos_rt_gc_root_restore(frame);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 32, "unrooted cycle must be reclaimed");
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn collect_follows_transitive_chain() {
        // Chain: root → a → b → c. Drop root, collect, all gone.
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let a = gos_rt_gc_alloc(16);
        let b = gos_rt_gc_alloc(16);
        let c = gos_rt_gc_alloc(16);
        unsafe {
            a.cast::<*mut u8>().write(b);
            b.cast::<*mut u8>().write(c);
        }
        gos_rt_gc_root_push(a);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 0);
        assert_eq!(gos_rt_gc_alloc_count(), 3);
        gos_rt_gc_root_restore(frame);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 48);
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn aggr_free_short_circuits_after_collect_already_reclaimed() {
        // If the tracing collector frees an allocation that the
        // drop pass later tries to free again (because the local
        // outlived the collect), the explicit free must skip the
        // dealloc to avoid double-free. The HashMap lookup at
        // free time observes the missing key.
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let ptr = gos_rt_gc_alloc(32);
        let _freed = gos_rt_gc_collect();
        // Collector reclaimed it; the registry no longer has it.
        // gos_rt_aggr_free must short-circuit on the missing entry.
        gos_rt_aggr_free(ptr, 32);
        // Reaching here without a double-free abort is the
        // assertion.
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn generation_guard_skips_freed_root_at_snapshot() {
        // The shadow stack stores raw addresses, not (addr, gen)
        // pairs. When the collector snapshots a per-thread stack,
        // it skips entries whose address is no longer present in
        // the registry — so a freed root cannot resurrect a
        // since-freed allocation.
        //
        // Allocator-reuse note: if a subsequent allocation reuses
        // the freed address, the conservative single-snapshot
        // scanner has no way to distinguish a stale shadow entry
        // from a live one and pins the new allocation for one
        // cycle. After the stale entry is popped (function return
        // or restore), the next collect reclaims it. This test
        // covers the no-reuse case; the reuse case is documented
        // technical debt of conservative scanning (item 6 in the
        // audit).
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let a = gos_rt_gc_alloc(64);
        gos_rt_gc_root_push(a);
        gos_rt_aggr_free(a, 64);
        // No new allocation in between — the registry has no
        // entry at `a`'s address. Snapshot skips stale roots and
        // the worklist is empty; the no-op sweep follows.
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 0, "no allocations to reclaim");
        assert_eq!(gos_rt_gc_alloc_count(), 0);
        gos_rt_gc_root_restore(frame);
    }

    #[test]
    fn restored_shadow_frame_drops_stale_roots() {
        // Allocate + push, then restore the shadow stack to the
        // pre-alloc depth (simulating a function return). The
        // address is no longer in any thread's shadow stack, so
        // the next collect reclaims even if the registry still
        // holds the entry (the drop pass would have removed it,
        // but tests skip the drop pass).
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let a = gos_rt_gc_alloc(64);
        gos_rt_gc_root_push(a);
        // Restore drops the root for `a`.
        gos_rt_gc_root_restore(frame);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 64);
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn shadow_stack_cap_does_not_overflow() {
        // Push many roots, verify no panic / OOM (the cap is the
        // safeguard; the test exercises the push-with-collect
        // path).
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let mut ptrs = Vec::new();
        for _ in 0..16 {
            let p = gos_rt_gc_alloc(16);
            gos_rt_gc_root_push(p);
            ptrs.push(p);
        }
        assert_eq!(gos_rt_gc_alloc_count(), 16);
        gos_rt_gc_root_restore(frame);
        let freed = gos_rt_gc_collect();
        assert_eq!(freed, 16 * 16);
        assert_eq!(gos_rt_gc_alloc_count(), 0);
    }

    #[test]
    fn registry_consistency_check_passes() {
        // The integrity walker must not fire on a healthy registry.
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let _a = gos_rt_gc_alloc(64);
        let _b = gos_rt_gc_alloc(128);
        let _c = gos_rt_gc_alloc(256);
        gos_rt_gc_assert_consistent();
        let _ = gos_rt_gc_collect();
        // After collect with no roots, registry is empty.
        gos_rt_gc_assert_consistent();
    }

    #[test]
    fn write_barrier_ptr_stores_value() {
        // The current STW barrier is a straight store. Verify
        // the symbol is callable and writes through.
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let frame = gos_rt_gc_root_save();
        let container = gos_rt_gc_alloc(16);
        let payload = gos_rt_gc_alloc(8);
        gos_rt_gc_root_push(container);
        gos_rt_gc_root_push(payload);
        // SAFETY: `container` is a registered 16-byte allocation;
        // its first 8 bytes form a valid `*mut *mut u8` write target.
        unsafe {
            gos_rt_write_barrier_ptr(container.cast::<*mut u8>(), payload);
        }
        // SAFETY: same allocation, reading the slot we just wrote.
        let read_back = unsafe { container.cast::<*mut u8>().read() };
        assert_eq!(read_back, payload);
        gos_rt_gc_root_restore(frame);
        let _ = gos_rt_gc_collect();
    }

    #[test]
    fn aggregate_layout_rejects_oversized() {
        // Layout helper must fail closed on a size that overflows
        // the allocator's isize::MAX bound.
        let r = aggregate_layout(usize::MAX);
        assert!(matches!(r, Err(GcError::LayoutOverflow)));
        let r = aggregate_layout(MAX_AGGR_BYTES + 1);
        assert!(matches!(r, Err(GcError::LayoutOverflow)));
        let r = aggregate_layout(0);
        assert!(matches!(r, Err(GcError::LayoutOverflow)));
        // A valid size succeeds.
        let r = aggregate_layout(64);
        assert!(r.is_ok());
    }

    #[test]
    fn ptr_key_is_send_sync_via_usize() {
        // Compile-time check: PtrKey is Send + Sync without a
        // bespoke unsafe impl. If a future refactor adds a
        // non-Send field, this assertion stops compiling.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<PtrKey>();
        assert_send_sync::<AllocRegistry>();
        // Round-trip through a real allocation so Miri's strict-
        // provenance model does not flag an integer-to-pointer
        // cast. The `as_addr` accessor stays test-only —
        // production callers go through registry lookups instead.
        // The alloc + free touch the process-wide GC registry,
        // so this test must serialise against every other test in
        // this module — otherwise a concurrent reset() between
        // alloc and free races with sibling tests that snapshot
        // alloc_count or sweep, producing flaky failures
        // ("expected 1 got 0" / "expected 64 freed got 0").
        let _g = GC_TEST_LOCK.lock();
        force_tracking_on();
        gos_rt_gc_reset();
        let real = gos_rt_gc_alloc(8);
        let p = PtrKey::from_raw(real);
        assert_eq!(p.as_addr(), real as usize);
        gos_rt_aggr_free(real, 8);
    }
}
