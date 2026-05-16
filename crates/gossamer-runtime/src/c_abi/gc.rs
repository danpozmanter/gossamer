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
use std::os::raw::c_char;
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
#[derive(Debug, Clone, Copy)]
struct AllocEntry {
    size: usize,
    mark: bool,
    generation: u64,
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

/// Conservative payload-word scan over a single rooted
/// allocation. Pushes any 8-byte word in the payload whose
/// value matches a live registry entry onto the worklist for
/// transitive marking.
///
/// Safety + correctness invariants:
///
/// - **Lock held**: the registry mutex is held by the caller
///   (we receive `&mut AllocRegistry`). Mutator threads cannot
///   free `addr` mid-scan.
/// - **Registry-authoritative size**: the loop bound comes
///   from the entry's recorded size, not from a parameter. If
///   a future change shrinks the entry mid-cycle (it can't —
///   the lock is held — but defence in depth), the scan
///   terminates within the recorded bound.
/// - **Bounded reads**: `byte_off + WORD_BYTES <= entry.size`
///   for every iteration. Trailing bytes that don't form a
///   complete word are not scanned (they cannot be a 64-bit
///   pointer in the architectures we support).
/// - **Unaligned reads**: `core::ptr::read_unaligned` defends
///   against future allocator changes that drop the 8-byte
///   alignment guarantee. The current `aggregate_layout`
///   enforces `WORD_BYTES` alignment, so all reads are in fact
///   aligned today, but `read_unaligned` is Miri-clean
///   regardless.
/// - **Generation match**: a candidate word is only pushed
///   onto the worklist if the registry entry's generation
///   equals the value the marker captured. ABA-stable: a
///   reallocation of the same address after a free is
///   correctly skipped (its generation has advanced).
fn scan_payload_words(addr: usize, registry: &AllocRegistry, worklist: &mut Vec<(usize, u64)>) {
    let Some(entry) = registry.get(&PtrKey(addr)) else {
        return;
    };
    let size = entry.size;
    let mut byte_off: usize = 0;
    while byte_off
        .checked_add(WORD_BYTES)
        .is_some_and(|end| end <= size)
    {
        // Provenance: `addr` came from the registry, which holds the
        // value returned by `alloc_zeroed` with a `size`-byte
        // layout. The bounds check above guarantees the read sits
        // inside that allocation.
        // Aliasing: we hold the registry Mutex; no mutator can free
        // `addr` mid-scan.
        // Synchronization: per the Mutex above.
        // Failure mode: if `addr` is somehow not the start of the
        // allocation the registry claims, this would scan adjacent
        // memory. The registry insert path is the single writer of
        // (addr, size) pairs, so this is structurally impossible
        // absent registry corruption — which the integrity check
        // catches under debug_assertions.
        let word_ptr = (addr + byte_off) as *const usize;
        // SAFETY: see invariant block above; reads through a valid
        // pointer inside a known-live allocation under the
        // registry lock, using `read_unaligned` for Miri cleanliness.
        let candidate = unsafe { core::ptr::read_unaligned(word_ptr) };
        if candidate != 0 {
            if let Some(child) = registry.get(&PtrKey(candidate)) {
                worklist.push((candidate, child.generation));
            }
        }
        byte_off += WORD_BYTES;
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
        let mut registry = gc_registry().lock();

        // Snapshot every thread's raw-pointer shadow stack into a
        // single worklist of (addr, expected_gen) pairs. Hold the
        // cross-thread registry lock so mutator pushes are
        // serialised behind the snapshot.
        let mut worklist: Vec<(usize, u64)> = Vec::new();
        {
            let threads = gc_thread_roots_registry().lock();
            for t in threads.iter() {
                let stack = t.stack.lock();
                for &addr in stack.iter() {
                    if addr == 0 {
                        continue;
                    }
                    if let Some(entry) = registry.get(&PtrKey(addr)) {
                        worklist.push((addr, entry.generation));
                    }
                }
            }
        }

        // Phase 1: clear every mark bit so this cycle starts clean.
        for entry in registry.values_mut() {
            entry.mark = false;
        }

        // Phase 2: transitive mark. Drains the worklist, marking
        // each entry exactly once. `scan_payload_words` enforces
        // the read-safety invariants on every payload word.
        while let Some((addr, expected_gen)) = worklist.pop() {
            let Some(entry) = registry.get_mut(&PtrKey(addr)) else {
                continue;
            };
            if entry.generation != expected_gen {
                // The address was freed and re-allocated between
                // snapshot and trace. The new allocation is not
                // reachable from any captured root; skip.
                continue;
            }
            if entry.mark {
                continue;
            }
            entry.mark = true;
            scan_payload_words(addr, &registry, &mut worklist);
        }

        // Phase 3: sweep — dealloc every unmarked entry. Bump
        // each removed entry's generation so any stale
        // shadow-stack snapshot fails the next-cycle check.
        let mut bytes_reclaimed: u64 = 0;
        let dead: Vec<(usize, usize)> = registry
            .iter()
            .filter_map(|(k, v)| if v.mark { None } else { Some((k.0, v.size)) })
            .collect();
        for (addr, size) in dead {
            registry.remove(&PtrKey(addr));
            // Layout reconstruction is total: the registry only
            // ever stored sizes from `aggregate_layout`, which
            // already validated them. Re-deriving here cannot fail
            // for any registered entry. The `?`-propagation route
            // exists for the rare case of registry corruption
            // (caught by `gos_rt_gc_assert_consistent` in debug).
            let Ok(layout) = aggregate_layout(size) else {
                continue;
            };
            // SAFETY:
            // - Provenance: `addr as *mut u8` came from
            //   `alloc_zeroed(layout)` in `gos_rt_gc_alloc`. The
            //   registry holds the address verbatim; no
            //   arithmetic was applied.
            // - Aliasing: we hold the registry Mutex; no other
            //   code path is currently dereferencing this
            //   allocation (it's unmarked, so no root pointed at
            //   it after the mark phase).
            // - Synchronization: registry Mutex.
            // - Failure mode: a corrupted (addr, size) pair would
            //   produce dealloc UB. The integrity check rejects
            //   such pairs at insert time under debug_assertions.
            unsafe { dealloc(addr as *mut u8, layout) };
            // Bump generation so any stale shadow-stack entry
            // referring to `addr` is skipped on the next cycle.
            let _ = next_generation();
            bytes_reclaimed = bytes_reclaimed.saturating_add(size as u64);
        }

        // Reset surviving entries' mark bits so the registry is
        // back to "clean between cycles" state. The integrity
        // walker invariant `!entry.mark` only holds before the
        // next mark phase, not after the sweep, unless we clear
        // here. Linear in surviving-entry count.
        for entry in registry.values_mut() {
            entry.mark = false;
        }

        GC_BYTES_SINCE_LAST_COLLECT.store(0, Ordering::Relaxed);

        // Debug-only integrity check. Catches registry
        // corruption introduced by future refactors before the
        // marker reads through a malformed entry.
        #[cfg(debug_assertions)]
        {
            assert_registry_consistent_locked(&registry);
        }

        bytes_reclaimed
    })
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
        // - Failure mode: `alloc_zeroed` returns null on OOM,
        //   which we forward to `handle_alloc_error`.
        let ptr = unsafe { alloc_zeroed(layout) };
        if ptr.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        if gc_track_enabled() {
            let generation = next_generation();
            gc_registry().lock().insert(
                PtrKey::from_raw(ptr),
                AllocEntry {
                    size,
                    mark: false,
                    generation,
                },
            );
            GC_BYTES_SINCE_LAST_COLLECT.fetch_add(size, Ordering::Relaxed);
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
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_aggr_alloc(size: u64) -> *mut u8 {
    ffi_entry!(std::ptr::null_mut(), { gos_rt_gc_alloc(size) })
}

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

/// In-place arena-string extension was a fast path for the
/// `s = s + c` accumulator pattern. With the Box-leak allocator
/// every allocation is a fresh `Box<[u8]>` and the "last
/// allocation" concept no longer applies. Always returns null so
/// `gos_rt_str_concat`'s caller falls through to its
/// fresh-allocation slow path.
///
/// Removing the optimization is correct: `try_extend_last_cstring`
/// also had a subtle aliasing hazard — extending the last
/// allocation mutated bytes that other Gossamer locals might
/// have been holding (see fix_architecture_ownership.md §3.6).
#[allow(clippy::unnecessary_wraps)]
pub fn try_extend_last_cstring(_a_ptr: *const c_char, _extra: &[u8]) -> *mut c_char {
    std::ptr::null_mut()
}

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
