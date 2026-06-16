#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Register-based bytecode VM dispatch loop.
//!
//! This crate otherwise forbids `unsafe`. The exception is the
//! inner dispatch loop: register files and const pools are
//! sized at compile time from the `FnChunk`'s `register_count`,
//! `float_count`, `int_count`, and `consts.len()`, so every
//! `get_unchecked` / `get_unchecked_mut` call in this file is
//! covered by the compiler-established bound. Skipping those
//! bounds checks is the difference between a 60-second run
//! and "slower than the VM was before typed opcodes landed".
use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gossamer_ast::Ident;
use gossamer_codegen_cranelift::{JitArtifact, JitFn};
use gossamer_hir::{HirItem, HirItemKind, HirProgram};
use gossamer_mir::Body;
use gossamer_types::TyCtxt;

use crate::builtins;
use crate::bytecode;
use crate::bytecode::{FnChunk, Op};
use crate::compile::compile_fn;
use crate::jit_call;
use crate::value::{MapKey, RuntimeError, RuntimeResult, SmolStr, Value};

/// Linked program: every global the VM needs to execute a call.
///
/// The VM compiles HIR directly to bytecode and lowers every
/// construct natively; there is no fallback evaluator. The global
/// table holds the built-in intrinsics plus every compiled function,
/// const, and static.
pub struct Vm {
    /// Per-Vm overlay holding user-defined functions, consts, and
    /// statics. Lookups consult this first; on miss they fall back
    /// to [`Self::prelude`]. Behind `Arc` so spawned worker `Vm`s
    /// share one immutable copy. Keys are `&'static str` interned
    /// via [`intern_type_name`] / [`intern_qualified`].
    pub(crate) globals: Arc<rustc_hash::FxHashMap<&'static str, Global>>,
    /// Process-shared prelude of built-in callables - built once
    /// from a `OnceLock` and `Arc::clone`d into every Vm at
    /// construction. Pre-lazy: every `Vm::new` cloned all ~330
    /// entries into its own HashMap. Post-lazy: a refcount bump.
    pub(crate) prelude: Arc<rustc_hash::FxHashMap<&'static str, Global>>,
    /// Frame pool: reused register-file storage handed out at
    /// `run()` entry and returned on exit. Eliminates the per-
    /// call `Vec<Value>` / `Vec<f64>` / `Vec<i64>` malloc storm
    /// that dominates call-heavy programs. Stack-discipline:
    /// nested calls each pop their own buffers off the free
    /// list and push them back on return.
    pub(crate) pool: RefCell<FramePool>,
    /// Lowered MIR for the program, shared across goroutines via
    /// `Arc` so a child `Vm` can drive its own deferred JIT
    /// compile without reflowing HIR → MIR. `None` when the JIT
    /// is disabled (`gos run --no-jit` / `GOS_JIT=0`).
    pub(crate) mir_bodies: Option<Arc<Vec<Body>>>,
    /// DefId.local -> native shape index for heap enums whose values
    /// may cross the JIT boundary as raw pointers.
    pub(crate) enum_shape_defs: Option<Arc<std::collections::HashMap<u32, u32>>>,
    /// Snapshot of the type context as it stood when MIR was
    /// lowered. Cranelift's `compile_to_jit` only needs `&TyCtxt`.
    /// `Arc` so spawned goroutines reuse the parent's snapshot
    /// rather than re-lowering it.
    pub(crate) tcx_snapshot: Option<Arc<TyCtxt>>,
    /// JIT artifact + override map filled by
    /// [`Vm::try_compile_jit_lazy`] the first time any chunk's hot
    /// counter trips on this `Vm`. Per-`Vm` (not shared across
    /// goroutines) because Cranelift's `JITModule` carries raw
    /// pointers and `dyn Fn` boxes that aren't `Send + Sync`.
    /// Goroutines spawned via [`Op::Spawn`] start with an empty
    /// JIT and stay on bytecode unless their own per-`Vm` hot
    /// counter trips - which only happens for genuinely long-lived
    /// child VMs, where the per-thread compile cost amortises.
    pub(crate) jit: parking_lot::RwLock<JitState>,
    /// Hot-path fast flag: number of installed JIT overrides. When
    /// zero, `apply()` skips the `jit.read()` `RwLock` probe entirely -
    /// every call is a bytecode dispatch, and probing the `RwLock`
    /// per call costs ~6-8 ns of atomic CAS that adds up across
    /// tight recursive workloads. Updated by
    /// `try_compile_jit_lazy` once the deferred compile installs
    /// entries; only ever monotonically increases.
    pub(crate) jit_override_count: AtomicUsize,
    /// Per-`Vm` cache state pinned in a never-shrinking arena. The
    /// hot dispatch loop reaches into `chunk_state_for(chunk)` once
    /// per call entry and threads `&ChunkState` through the
    /// dispatch arms. Replacing the prior shared
    /// `parking_lot::Mutex<Vec<CacheSlot>>` on `FnChunk` removes
    /// cross-goroutine cache-line bouncing while keeping the
    /// single-thread fast path lock-free (`RefCell` borrow check
    /// only). The `Box` is load-bearing: `chunk_state_for` hands
    /// out `&ChunkState` references that outlive the arena's
    /// reallocations, which only the `Box` indirection survives
    /// (a bare `Vec<ChunkState>` would move its elements on grow
    /// and invalidate every reference).
    #[allow(
        clippy::vec_box,
        reason = "Box keeps each ChunkState pinned; Vec<ChunkState> would invalidate stored &ChunkState references on grow"
    )]
    pub(crate) chunk_state_arena: RefCell<Vec<Box<ChunkState>>>,
    /// Side index into [`Self::chunk_state_arena`] keyed by
    /// `Arc::as_ptr(chunk) as usize`. The map's `&'static` lifetime
    /// is a stand-in: `chunk_state_for` casts each lookup to a
    /// borrow tied to `&self` (the arena outlives every reference
    /// it hands out). See `chunk_state_for` for the full safety
    /// argument.
    pub(crate) chunk_state_map: RefCell<HashMap<usize, &'static ChunkState>>,
    /// Single-slot last-seen-chunk cache, populated on every
    /// `chunk_state_for` lookup. Recursive / self-call patterns
    /// hit this slot before the `HashMap` probe, saving ~10 ns of
    /// hash + comparison per `apply()` call.
    pub(crate) chunk_state_last: Cell<Option<(usize, &'static ChunkState)>>,
    /// Monotonically-increasing version of [`Self::globals`]. Bumped
    /// whenever the globals map is mutated (today: only inside
    /// [`Self::load_item`] during program load; reserved for any
    /// future op that reassigns a global). Inline-cache slots stamp
    /// their resolved entry with the generation they observed and
    /// re-validate on hit so a stale slot from before a reassignment
    /// is treated as a miss instead of dispatching to the prior
    /// target. Per-`Vm`: spawned goroutines start at 1 and climb
    /// independently, mirroring the per-`Vm` `ChunkState` ownership.
    pub(crate) globals_generation: Cell<u32>,
    /// Call-stack snapshot for runtime-error diagnostics. Push on
    /// chunk entry, pop on success - on error the frame stays so
    /// `call_stack_snapshot` reports the failing chain.
    /// Names are interned `&'static str`; recursive programs do not
    /// allocate a heap String per frame.
    pub(crate) call_stack: RefCell<Vec<&'static str>>,
    /// Current Gossamer call depth for this goroutine's VM. Incremented
    /// on every `apply` entry, decremented on return. When it reaches
    /// `MAX_CALL_DEPTH` the call is refused with `RuntimeError::StackOverflow`
    /// so unbounded mutual or direct recursion aborts instead of spinning
    /// the CPU indefinitely through heap-allocated frame allocation.
    pub(crate) call_depth: Cell<usize>,
    /// Source map published by `gos test --coverage` (via
    /// [`Vm::set_source_map`]) before [`Vm::load`]. When set and
    /// `gossamer_runtime::coverage` is enabled, the compiler resolves
    /// each statement's span to `(file, line)` and emits an
    /// [`crate::bytecode::Op::CovHit`] against a pre-registered counter
    /// slot, so the bytecode tier records line coverage into the same
    /// global table the LLVM AOT tier instruments. `None` for every
    /// non-coverage path (`gos run`, plain `gos test`).
    pub(crate) source_map: Option<Arc<gossamer_lex::SourceMap>>,
}

/// Per-`Vm` per-chunk dispatch caches. Pinned inside
/// [`Vm::chunk_state_arena`]; references handed out by
/// [`Vm::chunk_state_for`] are valid for the lifetime of the
/// owning `Vm`.
/// Memoised JIT-override resolution for a chunk. `Unresolved` until
/// the first call after the one-shot JIT install; then fixed (the
/// override map only shrinks afterward, so a cached resolution - even
/// an `Arc`-held evicted entry - stays valid).
#[derive(Clone, Default)]
pub(crate) enum JitResolve {
    /// Not yet resolved against the installed override map.
    #[default]
    Unresolved,
    /// Resolved: this chunk has no native override (stays bytecode).
    None,
    /// Resolved: pre-computed dispatch data to call through.
    Some(std::sync::Arc<crate::jit_call::Prepared>),
}

pub(crate) struct ChunkState {
    /// Inline-cache slots for `Op::Call` / `Op::MethodCall` sites.
    /// One slot per `cache_idx`. `RefCell` because the dispatch
    /// arms read the slot, then on miss take a brief `borrow_mut`
    /// to refill it - never held across a sub-call.
    pub(crate) call_caches: RefCell<Vec<crate::bytecode::CacheSlot>>,
    /// Adaptive-arith cache slots. The outer `Vec` is fixed at
    /// chunk-construction time so no outer cell is needed; each
    /// slot's `Cell<u8>` handles the per-shape transition.
    pub(crate) arith_caches: Vec<crate::bytecode::ArithCacheSlot>,
    /// PEP 659-style field-access cache. Indexed by the
    /// `cache_idx` field on `Op::FieldGet`. Each slot remembers
    /// the receiver's interned-type-name pointer + the offset
    /// the named field resolved to, so hot-path field reads
    /// skip the linear name scan.
    pub(crate) field_caches: Vec<crate::bytecode::FieldCacheSlot>,
    /// Tier-D2 hot counter - decremented on every call into the
    /// chunk; trips a deferred whole-program JIT compile at zero.
    /// `Cell<i32>` (single-thread mutation only - each `Vm` owns
    /// its own counter, so cross-thread atomicity is unneeded).
    pub(crate) hot_counter: Cell<i32>,
    /// Per-chunk memoised JIT override (see [`JitResolve`]). Lets the
    /// hot path skip the shared `RwLock<JitState>` probe and the
    /// `HashMap<String>` name lookup after the first post-install call.
    pub(crate) jit_resolve: RefCell<JitResolve>,
}

impl ChunkState {
    fn new(
        call_cache_count: u16,
        arith_cache_count: u16,
        field_cache_count: u16,
        instr_count: usize,
        jit_disabled: bool,
    ) -> Self {
        let initial = if jit_disabled {
            crate::bytecode::HOT_DISABLED
        } else {
            crate::bytecode::hot_threshold_for(instr_count)
        };
        Self {
            call_caches: RefCell::new(vec![
                crate::bytecode::CacheSlot::default();
                call_cache_count as usize
            ]),
            arith_caches: (0..arith_cache_count)
                .map(|_| crate::bytecode::ArithCacheSlot::default())
                .collect(),
            field_caches: (0..field_cache_count)
                .map(|_| crate::bytecode::FieldCacheSlot::default())
                .collect(),
            hot_counter: Cell::new(initial),
            jit_resolve: RefCell::new(JitResolve::Unresolved),
        }
    }
}

/// Owns the cranelift JIT state once the deferred compile has
/// run. The `artifact` keeps every code page alive; the
/// `overrides` map lets `apply` route a `Global::Fn(chunk)` call
/// through native dispatch by name. `compiled` collapses the
/// previous `jit_attempted` flag so two goroutines tripping the
/// hot counter concurrently can't both kick a compile - the
/// first transitions `Pending → InProgress`, the others see
/// `InProgress` / `Done` / `Failed` and skip.
#[derive(Default)]
pub(crate) struct JitState {
    /// Owns the finalised `JITModule`; dropped along with the Vm so
    /// the code pages outlive every reachable `JitFn` handle.
    pub(crate) artifact: Option<JitArtifact>,
    /// Map from chunk name to the JIT entry the deferred compile
    /// produced. Populated together with `artifact`. Skips entries
    /// for `main` (see vm.rs:343 comment) and any function the
    /// cranelift backend rejected. Soft-bounded by
    /// [`Self::insertion_order`] + [`JIT_OVERRIDE_CAP`] so a
    /// long-running daemon that JITs new functions over time
    /// doesn't grow this map without bound.
    pub(crate) overrides: HashMap<String, Arc<JitFn>>,
    /// FIFO record of insertion order for `overrides`. On every
    /// insert that pushes the map past [`JIT_OVERRIDE_CAP`] entries
    /// the front name is popped and its entry dropped - releasing
    /// the `Arc<JitFn>`. Cheaper to maintain than a true LRU
    /// (no per-hit reordering on the dispatch hot path) and
    /// sufficient for the long-running-daemon shape that motivates
    /// the cap.
    pub(crate) insertion_order: std::collections::VecDeque<String>,
    /// State machine for the one-shot deferred compile. Once it
    /// reaches `Done` or `Failed` no thread retries.
    pub(crate) compiled: JitCompileState,
}

/// Soft cap on the size of [`JitState::overrides`]. Picked an
/// order of magnitude above any realistic single-program function
/// count so steady-state programs never trip the eviction path
/// while a daemon that synthesises new functions stays bounded.
const JIT_OVERRIDE_CAP: usize = 1024;

impl JitState {
    /// Inserts a JIT entry, evicting the oldest entry when the map
    /// is at capacity. Cheap (one `pop_front` + one `HashMap::remove`)
    /// since eviction only fires past the cap.
    fn insert_override(&mut self, name: String, jit: Arc<JitFn>) {
        if self.overrides.len() >= JIT_OVERRIDE_CAP
            && let Some(old) = self.insertion_order.pop_front()
        {
            self.overrides.remove(&old);
        }
        self.insertion_order.push_back(name.clone());
        self.overrides.insert(name, jit);
    }
}

#[derive(Default, Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum JitCompileState {
    /// No tier-up trigger has fired on this `Vm` yet.
    #[default]
    Pending,
    /// `compile_to_jit` is mid-flight on this thread. Other
    /// hot-counter trips on the same `Vm` skip; child VMs in
    /// other goroutines run their own per-`Vm` compile.
    InProgress,
    /// JIT artefact installed on this `Vm`.
    Done,
    /// `compile_to_jit` returned `Err`, or the JIT is disabled
    /// outright. Future hot-counter trips short-circuit.
    Failed,
}

/// Per-VM free list of register-file `Vec`s. Stack-discipline:
/// `take_*` pops a Vec sized to the requested length (or
/// allocates a fresh one when the list is empty); `give_*`
/// pushes it back on return so the next call at this depth
/// reuses the capacity.
#[derive(Default)]
pub(crate) struct FramePool {
    pub(crate) values: Vec<Vec<Value>>,
    pub(crate) floats: Vec<Vec<f64>>,
    pub(crate) ints: Vec<Vec<i64>>,
    /// Pool of `Vec<Value>` reused for `Op::Call` argument
    /// marshaling. Each call grabs one to collect args, hands
    /// it to `apply`, and the callee's `run` returns it to
    /// the pool when the args have been moved into the new
    /// frame's register file.
    pub(crate) args: Vec<Vec<Value>>,
    /// Highest `n` requested from each `take_*` since the last
    /// `shrink_to`. Used to drive exponential hysteresis: once
    /// several consecutive tasks have observed `< 50%` capacity
    /// utilisation we reclaim the excess so a single large
    /// goroutine can't permanently fatten every subsequent task
    /// on the same worker.
    pub(crate) peak_value: usize,
    pub(crate) peak_float: usize,
    pub(crate) peak_int: usize,
    /// Count of consecutive `shrink_to` calls with sub-50%
    /// utilisation across all three kinds. Once it crosses
    /// [`HYSTERESIS_RUNS`], the next `shrink_to` reclaims excess
    /// per-buffer capacity to `max(peak * 2, SHRINK_FLOOR)`.
    pub(crate) low_util_runs: u32,
}

/// Threshold of consecutive low-utilisation `shrink_to` calls
/// before per-buffer capacity reclamation kicks in. Picked low
/// enough that a worker that just handled one big task gets its
/// buffers back to size within a few subsequent tasks, high
/// enough that a noisy mix of small and big tasks doesn't
/// thrash on the shrink path.
const HYSTERESIS_RUNS: u32 = 4;

/// Floor under which `shrink_to` will not reclaim a buffer's
/// capacity. Sized so that small short-lived tasks (think a
/// handful of arguments and a tiny number of locals) keep the
/// re-use win without the next `take_*` having to grow.
const SHRINK_FLOOR: usize = 16;

impl FramePool {
    fn take_values(&mut self, n: usize) -> Vec<Value> {
        // Fast path: pool hit. We rely on the prior owner's
        // `give_values` to have already cleared the buffer, so
        // the pop is constant-time. `resize` to the requested
        // length re-fills with `Value::Void`.
        if n > self.peak_value {
            self.peak_value = n;
        }
        let mut v = self.values.pop().unwrap_or_default();
        v.resize(n, Value::Void);
        v
    }
    fn give_values(&mut self, mut v: Vec<Value>) {
        // Drop Arc-payload registers eagerly - otherwise the
        // pool would hold strings, arrays, and structs captive
        // for the lifetime of the VM, defeating ref-count
        // collection. clear() iterates dropping each; for a
        // 32-byte enum that's a tag dispatch + per-variant
        // Arc decrement, fast in the common Void/Int/Float case.
        v.clear();
        self.values.push(v);
    }
    fn take_floats(&mut self, n: usize) -> Vec<f64> {
        if n > self.peak_float {
            self.peak_float = n;
        }
        let mut v = self.floats.pop().unwrap_or_default();
        v.reserve(n);
        // SAFETY: `f64` is `Copy` with no `Drop`. We ensured
        // capacity ≥ n; the bytes left behind in the backing
        // buffer are valid `f64` patterns from the prior owner.
        // The compiler emits a `LoadConstF64` or arithmetic-
        // result write to every float reg before any read (the
        // typed register allocator gives every result a fresh
        // slot), so reading uninitialised garbage is never
        // observable.
        #[allow(clippy::uninit_vec)]
        unsafe {
            v.set_len(n);
        }
        v
    }
    fn give_floats(&mut self, mut v: Vec<f64>) {
        // No `Drop` to run; len-reset is just a u-word write,
        // cheaper than `clear()`'s iteration.
        unsafe {
            v.set_len(0);
        }
        self.floats.push(v);
    }
    fn take_ints(&mut self, n: usize) -> Vec<i64> {
        if n > self.peak_int {
            self.peak_int = n;
        }
        let mut v = self.ints.pop().unwrap_or_default();
        v.reserve(n);
        // SAFETY: see `take_floats`. `i64` is `Copy` with no
        // `Drop`; every int reg is written before read by the
        // compile-time register allocator.
        #[allow(clippy::uninit_vec)]
        unsafe {
            v.set_len(n);
        }
        v
    }
    fn give_ints(&mut self, mut v: Vec<i64>) {
        unsafe {
            v.set_len(0);
        }
        self.ints.push(v);
    }
    fn take_args(&mut self, capacity: usize) -> Vec<Value> {
        let mut v = self.args.pop().unwrap_or_default();
        // `clear()` drops any leftovers (paranoia - `give_args`
        // already empties), then reserve so the upcoming pushes
        // don't reallocate.
        v.clear();
        v.reserve(capacity);
        v
    }
    fn give_args(&mut self, mut v: Vec<Value>) {
        v.clear();
        self.args.push(v);
    }

    /// Drains pool buffers above `keep_per_kind`, dropping the
    /// excess to release backing capacity. Called after each
    /// goroutine task completes so a worker `Vm` does not ratchet
    /// to high-water and stay there for the rest of the program.
    ///
    /// Also applies exponential hysteresis on each surviving
    /// buffer's capacity: when several consecutive tasks have run
    /// with sub-50% utilisation across every kind, surviving
    /// buffers are shrunk to `max(peak * 2, SHRINK_FLOOR)`. Without
    /// this, one large goroutine permanently fattens every
    /// subsequent task on this worker - the per-buffer capacity
    /// stays at the high-water mark even though the task that
    /// needed it has long since returned.
    fn shrink_to(&mut self, keep_per_kind: usize) {
        if self.values.len() > keep_per_kind {
            self.values.truncate(keep_per_kind);
        }
        if self.floats.len() > keep_per_kind {
            self.floats.truncate(keep_per_kind);
        }
        if self.ints.len() > keep_per_kind {
            self.ints.truncate(keep_per_kind);
        }
        if self.args.len() > keep_per_kind {
            self.args.truncate(keep_per_kind);
        }

        let cap_v = self.values.iter().map(Vec::capacity).max().unwrap_or(0);
        let cap_f = self.floats.iter().map(Vec::capacity).max().unwrap_or(0);
        let cap_i = self.ints.iter().map(Vec::capacity).max().unwrap_or(0);
        let low_util = self.peak_value.saturating_mul(2) < cap_v
            && self.peak_float.saturating_mul(2) < cap_f
            && self.peak_int.saturating_mul(2) < cap_i;
        if low_util {
            self.low_util_runs = self.low_util_runs.saturating_add(1);
        } else {
            self.low_util_runs = 0;
        }
        if self.low_util_runs >= HYSTERESIS_RUNS {
            let target_v = self.peak_value.saturating_mul(2).max(SHRINK_FLOOR);
            let target_f = self.peak_float.saturating_mul(2).max(SHRINK_FLOOR);
            let target_i = self.peak_int.saturating_mul(2).max(SHRINK_FLOOR);
            for buf in &mut self.values {
                if buf.capacity() > target_v {
                    buf.shrink_to(target_v);
                }
            }
            for buf in &mut self.floats {
                if buf.capacity() > target_f {
                    buf.shrink_to(target_f);
                }
            }
            for buf in &mut self.ints {
                if buf.capacity() > target_i {
                    buf.shrink_to(target_i);
                }
            }
            self.low_util_runs = 0;
        }
        self.peak_value = 0;
        self.peak_float = 0;
        self.peak_int = 0;

        // Free the trailing capacity that pop()/push() rounds up
        // over time so `Vec` headers do not pin allocations
        // larger than the steady-state high-water mark.
        self.values.shrink_to_fit();
        self.floats.shrink_to_fit();
        self.ints.shrink_to_fit();
        self.args.shrink_to_fit();
    }
}

/// RAII guard that lends three register-file `Vec`s out of the
/// pool for the duration of one `run()` call. On `Drop`, the
/// buffers go back to the pool - including on early returns or
/// `?` propagation from inside the dispatch loop. Without this,
/// every `?` in the loop body would have to be hand-rewritten
/// to reunite with the buffers before bubbling out.
pub(crate) struct FrameGuard<'a> {
    pub(crate) pool: &'a RefCell<FramePool>,
    pub(crate) registers: std::mem::ManuallyDrop<Vec<Value>>,
    pub(crate) floats: std::mem::ManuallyDrop<Vec<f64>>,
    pub(crate) ints: std::mem::ManuallyDrop<Vec<i64>>,
}

impl<'a> FrameGuard<'a> {
    fn take(pool: &'a RefCell<FramePool>, n_val: usize, n_float: usize, n_int: usize) -> Self {
        let (registers, floats, ints) = {
            let mut p = pool.borrow_mut();
            (
                p.take_values(n_val),
                p.take_floats(n_float),
                p.take_ints(n_int),
            )
        };
        Self {
            pool,
            registers: std::mem::ManuallyDrop::new(registers),
            floats: std::mem::ManuallyDrop::new(floats),
            ints: std::mem::ManuallyDrop::new(ints),
        }
    }
}

impl Drop for FrameGuard<'_> {
    fn drop(&mut self) {
        // SAFETY: `take` runs exactly once at construction and
        // `Drop` runs exactly once at end-of-scope; the inner
        // `ManuallyDrop`s are never observed empty by anyone.
        let registers = unsafe { std::mem::ManuallyDrop::take(&mut self.registers) };
        let floats = unsafe { std::mem::ManuallyDrop::take(&mut self.floats) };
        let ints = unsafe { std::mem::ManuallyDrop::take(&mut self.ints) };
        let mut p = self.pool.borrow_mut();
        p.give_values(registers);
        p.give_floats(floats);
        p.give_ints(ints);
    }
}

impl std::fmt::Debug for Vm {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Intentionally non-exhaustive: the FramePool, JIT
        // artifact, and tcx snapshot are gnarly to render and add
        // no debugging signal beyond the global names.
        f.debug_struct("Vm")
            .field("globals", &self.globals.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Entries in the global table. Visible to `bytecode::CacheSlot`
/// so inline-cache slots can hold a resolved dispatch target
/// directly - no downcast on the hit path.
#[derive(Debug, Clone)]
pub(crate) enum Global {
    Fn(Arc<FnChunk>),
    Value(Value),
    /// A `static mut` cell. The shared `Arc<Mutex<Value>>` is cloned
    /// into every spawned goroutine's `globals` map (which itself is an
    /// `Arc`), so writes from one goroutine are observable in all
    /// others; the `Mutex` makes the concurrent access sound. Reads
    /// (`LoadGlobal`) load the current value; writes (`Op::StoreStatic`)
    /// replace it.
    MutStatic(Arc<parking_lot::Mutex<Value>>),
}

/// Maximum Gossamer call frames per goroutine before `StackOverflow`.
///
/// Each Gossamer call adds one `apply()` + `run()` pair to the Rust call
/// stack. `run()` is a 2 000-line match function; in debug builds the
/// compiler keeps every arm's locals live simultaneously, making each
/// pair cost ~160 KB of Rust stack. The default OS thread stack is 8 MB,
/// leaving roughly 40 safe levels after process and CLI startup.
///
/// Debug builds use a conservative 40-frame cap (each VM frame holds
/// several KB of `Value` slots in the register pool). Release builds
/// have ~10× smaller frames in practice; the cap is raised to 512 so
/// typical recursive shapes (mergesort over moderate inputs, parser
/// combinators, recursive-descent traversals) run without hitting the
/// limit. For
/// deeply recursive programs use `gos build`, where the native codegen
/// produces standard call instructions the OS can grow to handle.
#[cfg(debug_assertions)]
const MAX_CALL_DEPTH: usize = 40;
#[cfg(not(debug_assertions))]
const MAX_CALL_DEPTH: usize = 512;

mod call_dispatch;
pub(crate) mod goroutine;
mod lifecycle;
mod native_dispatch;
mod resolve;
mod run;

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

/// Runs [`crate::validate::validate_chunk`] in debug builds; no-op
/// in release. Catches `compile_fn` regressions before they reach
/// the unsafe dispatch loop in [`Vm::run`].
#[cfg(debug_assertions)]
fn debug_validate_chunk(chunk: &FnChunk) -> RuntimeResult<()> {
    crate::validate::validate_chunk(chunk)
        .map_err(|e| RuntimeError::Type(format!("invalid bytecode for `{}`: {e}", chunk.name)))
}

/// Release-build stub - production execution trusts the unverified
/// "compiler emits in-bounds indices" invariant for speed.
#[cfg(not(debug_assertions))]
#[inline]
fn debug_validate_chunk(_chunk: &FnChunk) -> RuntimeResult<()> {
    Ok(())
}

/// Recognises `fn name(p) { intrinsic_path(p) }` (a single
/// parameter, no other statements, body is exactly one call
/// forwarding the parameter) and returns the intrinsic's
/// path segments so the VM compiler can fold `name(x)` into
/// a direct intrinsic op at every call site.
fn detect_trivial_wrapper(decl: &gossamer_hir::HirFn) -> Option<Vec<String>> {
    if decl.params.len() != 1 {
        return None;
    }
    let body = decl.body.as_ref()?;
    if !body.block.stmts.is_empty() {
        return None;
    }
    let tail = body.block.tail.as_deref()?;
    // The tail may be the call itself, or a block whose tail
    // is the call. We only inline the former shape to keep
    // the matcher simple and the wrapper table small.
    let call_expr = match &tail.kind {
        gossamer_hir::HirExprKind::Call { .. } => tail,
        gossamer_hir::HirExprKind::Block(inner) if inner.stmts.is_empty() => {
            inner.tail.as_deref()?
        }
        _ => return None,
    };
    let gossamer_hir::HirExprKind::Call { callee, args } = &call_expr.kind else {
        return None;
    };
    if args.len() != 1 {
        return None;
    }
    let gossamer_hir::HirExprKind::Path {
        segments: arg_segments,
        ..
    } = &args[0].kind
    else {
        return None;
    };
    if arg_segments.len() != 1 {
        return None;
    }
    let param_name = match &decl.params[0].pattern.kind {
        gossamer_hir::HirPatKind::Binding { name, .. } => &name.name,
        _ => return None,
    };
    if arg_segments[0].name != *param_name {
        return None;
    }
    let gossamer_hir::HirExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    Some(segments.iter().map(|s| s.name.clone()).collect())
}

/// Native indexed read: `base[i]` over arrays, strings, tuples, vecs,
/// and structs, producing the element type's value (or its zero value
/// for an out-of-range index).
fn index_get(base: &Value, idx: &Value) -> RuntimeResult<Value> {
    let raw = match idx {
        Value::Int(n) => *n,
        _ => return Err(RuntimeError::Type("index must be integer".to_string())),
    };
    // Lenient indexing, matching the compiled tier (the canonical
    // behaviour): any index outside `[0, len)` - negative or past the end -
    // yields the element zero value rather than aborting, exactly as the
    // runtime `gos_rt_vec_get_*` helpers do. This keeps `gos run`
    // bit-identical to `gos build` on out-of-bounds access.
    let len = match base {
        Value::Array(items) | Value::Tuple(items) => items.len(),
        Value::IntArray(d) => d.len(),
        Value::FloatVec(d) => d.len(),
        Value::String(s) => s.len(),
        Value::FloatArray(fa) if fa.stride > 0 => fa.data.len() / fa.stride as usize,
        Value::FloatArray(_) => 0,
        _ => {
            return Err(RuntimeError::Type(format!(
                "value of kind `{base}` is not indexable"
            )));
        }
    };
    if raw < 0 || raw as usize >= len {
        return Ok(match base {
            Value::FloatVec(_) | Value::FloatArray(_) => Value::Float(0.0),
            _ => Value::Int(0),
        });
    }
    let i = raw as usize;
    match base {
        Value::Array(items) | Value::Tuple(items) => Ok(items[i].clone()),
        // Rehydrate a single element into `Value::Struct` so generic
        // indexed-access code keeps working when the array was compiled to
        // flat f64 storage.
        Value::FloatArray(fa_inner) => {
            let stride = fa_inner.stride as usize;
            let base_idx = i * stride;
            let mut fields: Vec<(Ident, Value)> = Vec::with_capacity(fa_inner.field_names.len());
            for (j, fname) in fa_inner.field_names.iter().enumerate() {
                fields.push((
                    Ident::new(fname.as_str()),
                    Value::Float(fa_inner.data[base_idx + j]),
                ));
            }
            Ok(Value::struct_(
                fa_inner.name,
                Arc::unwrap_or_clone(Arc::new(fields)),
            ))
        }
        Value::String(s) => Ok(Value::Int(i64::from(s.as_bytes()[i]))),
        Value::IntArray(data) => Ok(Value::Int(data[i])),
        Value::FloatVec(data) => Ok(Value::Float(data[i])),
        _ => unreachable!("len computed above for this variant"),
    }
}

/// Builds the `TypeName::method` global-table key for a
/// nominal receiver. Used as the fallback when the bare
/// method-name lookup misses.
fn qualified_key(receiver: &Value, method: &str) -> Option<&'static str> {
    match receiver {
        Value::Struct(inner) => Some(intern_qualified(inner.name, method)),
        Value::Channel(_) => Some(intern_qualified("Channel", method)),
        Value::String(_) => Some(intern_qualified("String", method)),
        // `Vec`-receiver methods resolve by type so a bare name shared with
        // another module's free function (`path::join` vs `strings::join`)
        // dispatches correctly. Only names registered under `Vec::` reroute;
        // the rest fall back to the bare lookup.
        Value::Array(_) => Some(intern_qualified("Vec", method)),
        _ => None,
    }
}

/// A stable identity for a method-call receiver, used as the IC's
/// guard. Two calls with the same `TypeToken` resolve to the same
/// `Global`. Token `0` (`TAG_NONE`) means "no stable identity, do
/// not cache".
///
/// For struct / variant receivers the token is the interned
/// type-name pointer in the low bits OR'd with a per-variant tag in
/// the high byte. `intern_type_name` returns a `&'static str` whose
/// `as_ptr()` is stable across every `Value::clone` of a struct
/// with the same name, so the cache hit path is one u64 compare.
pub(crate) fn type_token(v: &Value) -> u64 {
    const TAG_NONE: u64 = 0;
    const TAG_STRUCT: u64 = 1 << 56;
    const TAG_CHANNEL: u64 = 2 << 56;
    const TAG_STRING: u64 = 3 << 56;
    const TAG_ARRAY: u64 = 4 << 56;
    const TAG_TUPLE: u64 = 5 << 56;
    const TAG_VARIANT: u64 = 6 << 56;
    match v {
        Value::Struct(inner) => {
            // `inner.name` is already a globally-interned `&'static str`
            // (every `Value::struct_` routes the name through
            // `value::intern_type_name`), so its pointer is canonical and
            // stable across every clone of any instance of this type - the
            // same identity `Op::StructIs` relies on via `ptr::eq`. Use it
            // directly instead of re-hashing through a second pool.
            TAG_STRUCT | (inner.name.as_ptr() as u64 & 0x00FF_FFFF_FFFF_FFFF)
        }
        Value::Channel(_) => TAG_CHANNEL,
        Value::String(_) => TAG_STRING,
        Value::Array(_) | Value::FloatArray(_) | Value::IntArray(_) | Value::FloatVec(_) => {
            TAG_ARRAY
        }
        Value::Tuple(_) => TAG_TUPLE,
        Value::Variant(inner) => {
            // Globally-interned canonical pointer (see the `Struct` arm).
            TAG_VARIANT | (inner.name.as_ptr() as u64 & 0x00FF_FFFF_FFFF_FFFF)
        }
        // Primitives + non-cacheable receivers fall through to the
        // slow path on every call. The IC slot stores token=0 and
        // never matches a non-zero `type_token` result.
        _ => TAG_NONE,
    }
}

/// Builds a fresh inline-cache slot from a resolved [`Global`].
/// Pulls out the raw builtin fn pointer when the global is a
/// `Value::Builtin` so the steady-state dispatch is a direct
/// indirect call rather than `match Global::Value(Value::Builtin
/// { call, .. })`. Mirrors `CPython` 3.11's specialisation of
/// `LOAD_METHOD_NO_DICT` (where the resolved `__call__` is cached
/// alongside the type-version guard).
fn fill_cache_slot(token: u64, generation: u32, g: &Global) -> crate::bytecode::CacheSlot {
    let builtin_fn = match g {
        Global::Value(Value::Builtin(inner)) => Some(inner.call),
        _ => None,
    };
    let fn_chunk = match g {
        Global::Fn(chunk) => Some(Arc::clone(chunk)),
        Global::Value(_) | Global::MutStatic(_) => None,
    };
    crate::bytecode::CacheSlot {
        type_token: token,
        generation,
        builtin_fn,
        fn_chunk,
    }
}

/// Stable identity for an `Op::Call` callee - keyed by the
/// resolved-name string for `Value::String` callees (the bytecode
/// VM's idiom for "named global function"). Other callee shapes
/// (closures, builtins-passed-as-values, etc.) return `0` so the
/// IC slot stays cold and the slow path is taken every time -
/// those receivers don't have a stable identity worth caching.
pub(crate) fn call_token(v: &Value) -> u64 {
    const TAG_NAMED: u64 = 1 << 56;
    match v {
        Value::String(s) => {
            // Intern once per program - the leaked `&'static str`
            // is identity-stable across the run, so the cache hit
            // path is one u64 compare.
            let interned = intern_type_name(s);
            TAG_NAMED | (interned.as_ptr() as u64 & 0x00FF_FFFF_FFFF_FFFF)
        }
        _ => 0,
    }
}

/// Returns a `&'static str` for `name`, allocating only the first
/// time a given byte sequence is seen on this thread. Used by
/// [`type_token`] so receivers of "the same struct" produce the
/// same token across `Value::clone` boundaries.
fn intern_type_name(name: &str) -> &'static str {
    use std::cell::RefCell;
    thread_local! {
        static TYPE_NAMES: RefCell<rustc_hash::FxHashSet<&'static str>> =
            RefCell::new(rustc_hash::FxHashSet::default());
    }
    TYPE_NAMES.with(|cell| {
        if let Some(&interned) = cell.borrow().get(name) {
            return interned;
        }
        let interned: &'static str = Box::leak(name.to_string().into_boxed_str());
        cell.borrow_mut().insert(interned);
        interned
    })
}

/// Returns the canonical `"<type>::<method>"` key, allocating only
/// the first time a given (type, method) pair is seen on this
/// thread.
fn intern_qualified(type_name: &str, method: &str) -> &'static str {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<rustc_hash::FxHashSet<&'static str>> =
            RefCell::new(rustc_hash::FxHashSet::default());
    }
    let mut buf = String::with_capacity(type_name.len() + 2 + method.len());
    buf.push_str(type_name);
    buf.push_str("::");
    buf.push_str(method);
    CACHE.with(|cell| {
        if let Some(&interned) = cell.borrow().get(buf.as_str()) {
            return interned;
        }
        let interned: &'static str = Box::leak(buf.into_boxed_str());
        cell.borrow_mut().insert(interned);
        interned
    })
}

/// Binary arithmetic that dispatches on operand kind. Ints use
/// `int_fn`; floats use `float_fn`; mixed kinds promote to
/// float. String concat (Add on two strings) is handled at the
/// caller before this runs.
fn bin_arith(
    a: &Value,
    b: &Value,
    int_fn: fn(i64, i64) -> i64,
    float_fn: fn(f64, f64) -> f64,
    label: &str,
) -> RuntimeResult<Value> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => Ok(Value::Int(int_fn(*x, *y))),
        (Value::Float(x), Value::Float(y)) => Ok(Value::Float(float_fn(*x, *y))),
        (Value::Int(x), Value::Float(y)) => Ok(Value::Float(float_fn(*x as f64, *y))),
        (Value::Float(x), Value::Int(y)) => Ok(Value::Float(float_fn(*x, *y as f64))),
        // String concat is handled separately in the dispatch
        // loop (Add on two strings).
        _ => Err(RuntimeError::Type(format!(
            "{label} on unsupported value kinds"
        ))),
    }
}

/// Tier C2 - classify `(a, b)` into one of the `ARITH_*` shape
/// constants for the purposes of inline-cache specialisation.
/// Anything outside the four narrow shapes (II, FF, SS, II/FF
/// mixed) ends up [`bytecode::ARITH_POLYMORPHIC`].
fn classify_pair(a: &Value, b: &Value, allow_string: bool) -> u8 {
    match (a, b) {
        (Value::Int(_), Value::Int(_)) => bytecode::ARITH_INT_INT,
        (Value::Float(_), Value::Float(_)) => bytecode::ARITH_FLOAT_FLOAT,
        (Value::String(_), Value::String(_)) if allow_string => bytecode::ARITH_STRING_STRING,
        _ => bytecode::ARITH_POLYMORPHIC,
    }
}

/// Updates the shape slot for the arith op at `cache_idx` after
/// observing one operand pair. Sticky transitions: any move off
/// the initial specialised shape goes straight to
/// [`bytecode::ARITH_POLYMORPHIC`] so subsequent dispatches skip
/// the re-observation cost.
fn record_shape(state: &ChunkState, cache_idx: u16, observed: u8) {
    let slot = &state.arith_caches[cache_idx as usize];
    let cur = slot.shape.get();
    if cur == bytecode::ARITH_UNKNOWN {
        slot.shape.set(observed);
    } else if cur != observed {
        slot.shape.set(bytecode::ARITH_POLYMORPHIC);
    }
}

/// Specialised dispatch for `Op::AddInt`. The hot path is a
/// single discriminant check; the cold path observes the operand
/// shape and quickens the slot. String concatenation lives here
/// because `+` is the only Gossamer operator that overloads onto
/// `Value::String`.
fn adaptive_add(
    state: &ChunkState,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                return Ok(Value::Int(x.wrapping_add(*y)));
            }
        }
        bytecode::ARITH_FLOAT_FLOAT => {
            if let (Value::Float(x), Value::Float(y)) = (a, b) {
                return Ok(Value::Float(*x + *y));
            }
        }
        bytecode::ARITH_STRING_STRING => {
            if let (Value::String(x), Value::String(y)) = (a, b) {
                let mut s = String::with_capacity(x.len() + y.len());
                s.push_str(x);
                s.push_str(y);
                return Ok(Value::String(s.into()));
            }
        }
        _ => {}
    }
    record_shape(state, cache_idx, classify_pair(a, b, true));
    if let (Value::String(x), Value::String(y)) = (a, b) {
        let mut s = String::with_capacity(x.len() + y.len());
        s.push_str(x);
        s.push_str(y);
        return Ok(Value::String(s.into()));
    }
    bin_arith(a, b, i64::wrapping_add, |x, y| x + y, "addition")
}

/// Specialised dispatch for `Op::SubInt` / `Op::MulInt`. Sub and
/// Mul share the shape of binary numeric ops, so the helper
/// takes the int/float operations and a label for the polymorphic
/// fallback path's error message.
#[allow(
    clippy::too_many_arguments,
    reason = "lowering plumbing - every parameter is needed by the surrounding pipeline"
)]
fn adaptive_arith(
    state: &ChunkState,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
    int_fn: fn(i64, i64) -> i64,
    float_fn: fn(f64, f64) -> f64,
    label: &str,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                return Ok(Value::Int(int_fn(*x, *y)));
            }
        }
        bytecode::ARITH_FLOAT_FLOAT => {
            if let (Value::Float(x), Value::Float(y)) = (a, b) {
                return Ok(Value::Float(float_fn(*x, *y)));
            }
        }
        _ => {}
    }
    record_shape(state, cache_idx, classify_pair(a, b, false));
    bin_arith(a, b, int_fn, float_fn, label)
}

/// Specialised dispatch for `Op::DivInt`. Integer divide-by-zero
/// surfaces as a runtime error, so the int-int hot path still
/// has to branch on `y == 0`. Float division never errors.
fn adaptive_div(
    state: &ChunkState,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                if *y == 0 {
                    return Err(RuntimeError::Panic("divide by zero".to_string()));
                }
                return Ok(Value::Int(x.wrapping_div(*y)));
            }
        }
        bytecode::ARITH_FLOAT_FLOAT => {
            if let (Value::Float(x), Value::Float(y)) = (a, b) {
                return Ok(Value::Float(*x / *y));
            }
        }
        _ => {}
    }
    record_shape(state, cache_idx, classify_pair(a, b, false));
    div_int(a, b)
}

/// Specialised dispatch for `Op::RemInt`. Mirrors [`adaptive_div`].
fn adaptive_rem(
    state: &ChunkState,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                if *y == 0 {
                    return Err(RuntimeError::Panic("divide by zero".to_string()));
                }
                return Ok(Value::Int(x.wrapping_rem(*y)));
            }
        }
        bytecode::ARITH_FLOAT_FLOAT => {
            if let (Value::Float(x), Value::Float(y)) = (a, b) {
                return Ok(Value::Float(*x % *y));
            }
        }
        _ => {}
    }
    record_shape(state, cache_idx, classify_pair(a, b, false));
    rem_int(a, b)
}

fn div_int(a: &Value, b: &Value) -> RuntimeResult<Value> {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(RuntimeError::Panic("divide by zero".to_string())),
        (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_div(*y))),
        (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x / y)),
        (Value::Int(x), Value::Float(y)) => Ok(Value::Float((*x as f64) / y)),
        (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x / (*y as f64))),
        _ => Err(RuntimeError::Type(
            "division on non-numeric values".to_string(),
        )),
    }
}

fn rem_int(a: &Value, b: &Value) -> RuntimeResult<Value> {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(RuntimeError::Panic("divide by zero".to_string())),
        (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_rem(*y))),
        (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x % y)),
        (Value::Int(x), Value::Float(y)) => Ok(Value::Float((*x as f64) % y)),
        (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x % (*y as f64))),
        _ => Err(RuntimeError::Type("modulo on non-int values".to_string())),
    }
}

fn neg(v: &Value) -> RuntimeResult<Value> {
    match v {
        // `i64::MIN` negates to itself (wraps), matching the typed-int
        // negation path and the documented integer-overflow semantics.
        Value::Int(i) => Ok(Value::Int(i.wrapping_neg())),
        Value::Float(f) => Ok(Value::Float(-*f)),
        _ => Err(RuntimeError::Type("neg on non-numeric".to_string())),
    }
}

fn not(v: &Value) -> RuntimeResult<Value> {
    match v {
        Value::Bool(b) => Ok(Value::Bool(!b)),
        _ => Err(RuntimeError::Type("not on non-bool".to_string())),
    }
}

fn compare(
    a: &Value,
    b: &Value,
    order: std::cmp::Ordering,
    or_equal: bool,
) -> RuntimeResult<Value> {
    // Auto-deref `__Cell` so `flags.count < 10` works without `*`.
    let a_deref = auto_deref_cell(a);
    let b_deref = auto_deref_cell(b);
    let a_ref = a_deref.as_ref().unwrap_or(a);
    let b_ref = b_deref.as_ref().unwrap_or(b);
    let result = match (a_ref, b_ref) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x
            .partial_cmp(y)
            .ok_or(RuntimeError::Arithmetic("NaN comparison".to_string()))?,
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.cmp(y),
        _ => {
            return Err(RuntimeError::Type(
                "comparison on unsupported kinds".to_string(),
            ));
        }
    };
    let matches = if or_equal {
        result == order || result == std::cmp::Ordering::Equal
    } else {
        result == order
    };
    Ok(Value::Bool(matches))
}

/// True when `program` declares at least one user `fn` other than
/// `main`. Used by `Vm::load` to skip MIR lowering + tcx cloning
/// on programs where the JIT could never produce a useful
/// override (the bytecode VM always runs `main` on its own path,
/// so a program whose only function is `main` never benefits from
/// the cranelift compile).
fn has_jit_eligible_fn(program: &HirProgram) -> bool {
    program
        .items
        .iter()
        .any(|item| matches!(&item.kind, HirItemKind::Fn(decl) if decl.name.name != "main"))
}

fn truthy(v: &Value) -> RuntimeResult<bool> {
    // 0.7.0 flag::Cell auto-deref so `if flags.verbose { … }`
    // works without `*`.
    let deref = auto_deref_cell(v);
    let value = deref.as_ref().unwrap_or(v);
    match value {
        Value::Bool(b) => Ok(*b),
        _ => Err(RuntimeError::Type(
            "branch condition must be bool".to_string(),
        )),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    // 0.7.0 flag::Cell auto-deref. Either side being a `__Cell`
    // struct unwraps to its current value before comparison, so
    // `flags.output == "text"` works without the user typing `*`.
    let a_deref = auto_deref_cell(a);
    let b_deref = auto_deref_cell(b);
    let a_ref = a_deref.as_ref().unwrap_or(a);
    let b_ref = b_deref.as_ref().unwrap_or(b);
    match (a_ref, b_ref) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        // Enum variants (incl. Option / Result) compare structurally: same
        // variant, same payload. The compiled tiers already do this; without
        // it `Some(5) == Some(5)` was `false` on the VM, and a derived enum's
        // `==` (which routes to a field-wise `eq`) couldn't be matched.
        (Value::Variant(va), Value::Variant(vb)) => {
            va.name == vb.name
                && va.fields.len() == vb.fields.len()
                && va
                    .fields
                    .iter()
                    .zip(vb.fields.iter())
                    .all(|(x, y)| values_equal(x, y))
        }
        // Native enum handles compare structurally through the boxed
        // representation (rare fallback; derived `==` routes through
        // match dispatch instead).
        (Value::NativeEnum(a), _) => values_equal(&crate::value::native_enum_to_variant(a), b_ref),
        (_, Value::NativeEnum(b)) => values_equal(a_ref, &crate::value::native_enum_to_variant(b)),
        _ => false,
    }
}

/// Unwraps a `__Cell` flag handle to its current backing value.
/// Returns `Some(unwrapped)` for `__Cell` structs, `None` for any
/// other shape so callers can keep the original borrow.
pub(crate) fn auto_deref_cell(v: &Value) -> Option<Value> {
    let Value::Struct(inner) = v else {
        return None;
    };
    if inner.name != "__Cell" {
        return None;
    }
    let mut set_id: u64 = 0;
    let mut flag_name = String::new();
    for (ident, val) in &inner.fields {
        if ident.name == "__set_id"
            && let Value::Int(n) = val
        {
            set_id = *n as u64;
        }
        if ident.name == "__flag_name"
            && let Value::String(s) = val
        {
            flag_name = s.as_str().to_string();
        }
    }
    crate::builtins::resolve_cell(set_id, &flag_name)
}

/// Native struct-field read. Returns `Value::Unit` on unknown fields
/// so partially-typed programs keep running.
fn field_get(receiver: &Value, name: &str) -> RuntimeResult<Value> {
    if let Value::Struct(inner) = receiver {
        if let Some((_, v)) = inner.fields.iter().find(|(ident, _)| ident.name == name) {
            return Ok(v.clone());
        }
        return Ok(Value::Unit);
    }
    Err(RuntimeError::Type(format!(
        "field access on non-struct `{receiver}`"
    )))
}

/// Native struct-field write. Mutates the register's struct
/// in place using `Arc::make_mut`, so aliasing values see the
/// new state only if they share the same `Arc` (value-aggregate
/// semantics) when the receiver is a local (register) binding.
fn field_set(receiver: &mut Value, name: &str, new_value: Value) -> RuntimeResult<()> {
    let Value::Struct(struct_arc) = receiver else {
        return Err(RuntimeError::Type(format!(
            "cannot assign to field `{name}` on non-struct `{receiver}`"
        )));
    };
    let struct_inner = Arc::make_mut(struct_arc);
    let slots = &mut struct_inner.fields;
    for (ident, slot) in slots.iter_mut() {
        if ident.name == name {
            *slot = new_value;
            return Ok(());
        }
    }
    slots.push((Ident::new(name), new_value));
    Ok(())
}

#[cfg(all(test, debug_assertions))]
mod tests {
    use super::*;
    use crate::bytecode::Op;
    use crate::validate::validate_chunk;

    fn empty_chunk(register_count: u16) -> FnChunk {
        FnChunk {
            name: "vm_test",
            arity: 0,
            register_count,
            float_count: 0,
            int_count: 0,
            instrs: Vec::new(),
            wide_ops: Vec::new(),
            consts: vec![Value::Int(0)],
            f64_consts: Vec::new(),
            i64_consts: Vec::new(),
            globals: Vec::new(),
            shape_names: Vec::new(),
            call_cache_count: 0,
            arith_cache_count: 0,
            field_cache_count: 0,
            mut_ref_params: Vec::new(),
            closure_protos: Vec::new(),
            select_arms: Vec::new(),
        }
    }

    #[test]
    fn debug_validate_chunk_accepts_well_formed_bytecode() {
        let mut chunk = empty_chunk(2);
        chunk.instrs.push(Op::LoadConst { dst: 0, idx: 0 });
        chunk.instrs.push(Op::Return { value: 0 });
        assert!(validate_chunk(&chunk).is_ok());
        assert!(debug_validate_chunk(&chunk).is_ok());
    }

    #[test]
    fn debug_validate_chunk_rejects_malformed_bytecode() {
        let mut chunk = empty_chunk(2);
        chunk.instrs.push(Op::Move { dst: 99, src: 0 });
        let err = debug_validate_chunk(&chunk).expect_err("must reject");
        match err {
            RuntimeError::Type(msg) => assert!(msg.contains("invalid bytecode")),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}
