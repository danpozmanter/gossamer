#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

// The JIT backend: real Cranelift on native, a no-op stub on wasm32
// (Cranelift has no wasm target). The stub's `has_worthy_jit_body`
// always returns `false`, so the compile path below never runs there.
#[cfg(target_arch = "wasm32")]
use crate::jit_stub as jit_backend;
#[cfg(not(target_arch = "wasm32"))]
use gossamer_codegen_cranelift as jit_backend;

impl Vm {
    /// Builds a VM pre-populated with the built-in intrinsics.
    #[must_use]
    pub fn new() -> Self {
        let mut vm = Self {
            globals: Arc::new(rustc_hash::FxHashMap::default()),
            prelude: builtins::prelude_globals(),
            qualified_names: RefCell::new(Vec::new()),
            pool: RefCell::new(FramePool::default()),
            mir_bodies: RefCell::new(None),
            tcx_snapshot: RefCell::new(None),
            enum_shape_defs: RefCell::new(None),
            enum_shape_handles: RefCell::new(None),
            struct_shape_defs: RefCell::new(None),
            struct_shape_handles: RefCell::new(None),
            jit_eager_names: RefCell::new(Arc::new(std::collections::HashSet::new())),
            jit_cache_key: RefCell::new(None),
            jit_droppable: Cell::new(false),
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
            jit_graph_cache: crate::jit_call::GraphCache::default(),
            jit_counters: JitCounters::default(),
            chunk_state_arena: RefCell::new(Vec::new()),
            chunk_state_map: RefCell::new(HashMap::new()),
            chunk_state_last: Cell::new(None),
            globals_generation: Cell::new(1),
            call_stack: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
            source_map: None,
            collect_comptime: Cell::new(false),
            comptime_folds: RefCell::new(Vec::new()),
        };
        // Late-registered binding natives go into the per-Vm overlay
        // because they can be added between Vm constructions; the
        // OnceLock prelude is frozen at first access.
        let overlay = Arc::get_mut(&mut vm.globals).expect("fresh Vm globals are uniquely owned");
        for (name, value) in crate::external_natives::external_natives_snapshot() {
            overlay.insert(name, Global::Value(value));
        }
        vm
    }

    /// Builds a VM from a pre-populated `globals` map. Used by
    /// `Op::Spawn` so a freshly spawned goroutine runs the callee
    /// through the bytecode VM with the parent's `Arc<FnChunk>`
    /// graph shared (chunks are immutable + `Sync`). The child has
    /// its own per-`Vm` cache state and JIT slot - see [`Self::jit`]
    /// for why JIT state can't cross threads.
    #[must_use]
    pub(crate) fn with_globals(
        globals: Arc<rustc_hash::FxHashMap<&'static str, Global>>,
        mir_bodies: Option<Arc<Vec<Body>>>,
        tcx_snapshot: Option<Arc<TyCtxt>>,
        enum_shape_defs: Option<Arc<std::collections::HashMap<u32, u32>>>,
        enum_shape_handles: Option<Arc<Vec<Arc<crate::value::NativeEnumShape>>>>,
        struct_shape_defs: Option<Arc<std::collections::HashMap<u32, u32>>>,
        struct_shape_handles: Option<Arc<Vec<Arc<crate::value::NativeStructShape>>>>,
        jit_eager_names: Arc<std::collections::HashSet<String>>,
        jit_cache_key: Option<Arc<str>>,
    ) -> Self {
        Self {
            globals,
            prelude: builtins::prelude_globals(),
            qualified_names: RefCell::new(Vec::new()),
            pool: RefCell::new(FramePool::default()),
            mir_bodies: RefCell::new(mir_bodies),
            tcx_snapshot: RefCell::new(tcx_snapshot),
            enum_shape_defs: RefCell::new(enum_shape_defs),
            enum_shape_handles: RefCell::new(enum_shape_handles),
            struct_shape_defs: RefCell::new(struct_shape_defs),
            struct_shape_handles: RefCell::new(struct_shape_handles),
            jit_eager_names: RefCell::new(jit_eager_names),
            jit_cache_key: RefCell::new(jit_cache_key),
            // Worker VMs run pool tasks back-to-back; `reset_after_task`
            // manages their MIR lifetime, so they never self-drop.
            jit_droppable: Cell::new(false),
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
            jit_graph_cache: crate::jit_call::GraphCache::default(),
            jit_counters: JitCounters::default(),
            chunk_state_arena: RefCell::new(Vec::new()),
            chunk_state_map: RefCell::new(HashMap::new()),
            chunk_state_last: Cell::new(None),
            globals_generation: Cell::new(1),
            call_stack: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
            // Worker VMs run already-compiled chunks; the source map is
            // a compile-time input only, and `Op::CovHit` bumps the
            // global table regardless of which Vm executes the chunk.
            source_map: None,
            // Comptime folding is a main-VM compile-time concern only.
            collect_comptime: Cell::new(false),
            comptime_folds: RefCell::new(Vec::new()),
        }
    }

    /// Returns deferred-JIT counters and the current native dispatch footprint.
    /// Reading this snapshot does not compile, promote, or otherwise alter the
    /// VM's execution policy.
    #[must_use]
    pub fn jit_metrics(&self) -> JitMetrics {
        let state = self.jit.read();
        JitMetrics {
            tier_up_requests: self.jit_counters.tier_up_requests.get(),
            work_floor_deferrals: self.jit_counters.work_floor_deferrals.get(),
            compile_attempts: self.jit_counters.compile_attempts.get(),
            successful_compiles: self.jit_counters.successful_compiles.get(),
            resident_functions: state.overrides.len(),
            discarded_artifacts: self.jit_counters.discarded_artifacts.get(),
            released_snapshots: self.jit_counters.released_snapshots.get(),
            reused_artifacts: self.jit_counters.reused_artifacts.get(),
            ram_skipped_compiles: self.jit_counters.ram_skipped_compiles.get(),
            last_observed_rss_bytes: self.jit_counters.last_observed_rss_bytes.get(),
        }
    }

    /// Publishes the source map `gos test --coverage` uses to resolve
    /// statement spans to `(file, line)` for line-coverage recording.
    /// Must be called before [`Self::load`] so the compiler can emit
    /// [`crate::bytecode::Op::CovHit`] at each statement boundary;
    /// leaving it unset (the default) compiles without any coverage
    /// instrumentation.
    pub fn set_source_map(&mut self, map: Arc<gossamer_lex::SourceMap>) {
        self.source_map = Some(map);
    }

    /// True when coverage recording should be instrumented for this
    /// load: the runtime flag is on and a source map is published.
    /// Gates both the `Op::CovHit` emission and the JIT (native code
    /// carries no coverage ops, so coverage runs stay on bytecode).
    #[must_use]
    pub(crate) fn coverage_active(&self) -> bool {
        gossamer_runtime::coverage::enabled() && self.source_map.is_some()
    }

    /// The source map to thread into the compiler for coverage, or
    /// `None` when coverage is not active for this load. Returns an
    /// `Arc` clone (a refcount bump) so the caller holds an owned
    /// handle that doesn't borrow `self` across the `&mut self`
    /// compilation passes.
    fn coverage_source_map(&self) -> Option<Arc<gossamer_lex::SourceMap>> {
        if self.coverage_active() {
            self.source_map.clone()
        } else {
            None
        }
    }

    /// Two-tier global lookup: per-Vm overlay first, then shared
    /// prelude on miss. Returns a cloned [`Global`] (Arc-clone for
    /// the heavy variants; refcount bump only).
    #[inline]
    #[must_use]
    pub(crate) fn lookup_global(&self, name: &str) -> Option<Global> {
        if let Some(g) = self.globals.get(name) {
            return Some(g.clone());
        }
        self.prelude.get(name).cloned()
    }

    /// Borrowed two-tier lookup - caller doesn't need a clone.
    #[inline]
    #[must_use]
    pub(crate) fn lookup_global_ref(&self, name: &str) -> Option<&Global> {
        if let Some(g) = self.globals.get(name) {
            return Some(g);
        }
        self.prelude.get(name)
    }

    /// Bumps the `globals_generation` counter and returns the new
    /// value. Call from any code path that mutates `globals` after
    /// `Vm::new` / `Vm::with_globals` have returned. Inline caches
    /// stamped with an older value will be treated as misses and
    /// re-resolved against the new map.
    pub fn bump_globals_generation(&self) -> u32 {
        let next = self.globals_generation.get().wrapping_add(1);
        // Skip 0 on wrap so the empty-slot sentinel stays distinct
        // from a real generation. Wrapping after 4 billion mutations
        // is purely defensive - we never expect to get there in a
        // single program run.
        let next = if next == 0 { 1 } else { next };
        self.globals_generation.set(next);
        next
    }

    /// Snapshot of the current globals generation. IC slot writers
    /// stamp the value they observed; readers re-validate against
    /// the live counter.
    #[inline]
    #[must_use]
    pub fn globals_generation(&self) -> u32 {
        self.globals_generation.get()
    }

    /// Test-only override that lets the test suite drive the
    /// generation counter to a specific value (e.g. `u32::MAX` to
    /// force the wrap-skips-zero path). Production code never needs
    /// this - it always uses [`Self::bump_globals_generation`].
    #[doc(hidden)]
    pub fn set_globals_generation_for_test(&self, value: u32) {
        self.globals_generation.set(value);
    }

    /// Trims per-`Vm` mutable buffers back toward a steady-state
    /// floor after a goroutine task completes. Without this, a
    /// worker `Vm` that handled one large goroutine carries that
    /// goroutine's high-water mark for the rest of the program;
    /// Short-lived goroutines would otherwise leave every worker
    /// holding the union of every register file they ever saw.
    /// Cheap to call between tasks: a few `Vec` truncations and
    /// `shrink_to_fit` calls.
    pub(crate) fn reset_after_task(&mut self) {
        self.pool.borrow_mut().shrink_to(4);
        // Free any marshalled graphs cached for the task that just finished:
        // a worker VM is reused across tasks, so the next task's graph Arcs
        // must not alias a prior task's native marshalling.
        self.jit_graph_cache.clear();
        // Clear this goroutine-worker's traceback frames so the next
        // task starts with an empty call stack rather than inheriting
        // stale frames from the one that just finished.
        let mut call_stack = self.call_stack.borrow_mut();
        call_stack.clear();
        call_stack.shrink_to_fit();
        // A worker task is an execution boundary. Its JIT snapshot is only a
        // deferred compile input, never bytecode runtime state, so retaining
        // it in a thread-local worker would pin a whole program after the
        // task completed. A worker that already compiled retains its artifact;
        // a later task safely stays on bytecode if it did not tier up here.
        self.release_deferred_jit_snapshots();
    }

    /// Frees MIR bodies and the `TyCtxt` snapshot retained for deferred JIT.
    /// After JIT compilation fires (or is skipped), these are never read
    /// again on the main `Vm` - goroutines have already cloned their own Arcs.
    /// Call once after `vm.call()` returns to reclaim the per-program MIR
    /// allocation before the goroutine-join phase.
    pub fn release_jit_prelude(&mut self) {
        self.release_deferred_jit_snapshots();
        // The chunk-state arena (per-call IC slots, hot counters)
        // can grow large for big programs. Trim it back to the
        // steady-state floor while goroutines drain.
        self.chunk_state_arena.borrow_mut().shrink_to_fit();
        self.chunk_state_map.borrow_mut().shrink_to_fit();
    }

    /// Drops the compiler-only state retained for deferred JIT. This takes
    /// `&self` because each component is independently owned behind a
    /// `RefCell`; execution only ever reads these before a tier-up starts.
    /// Returns whether it released anything, which keeps metrics idempotent.
    fn release_deferred_jit_snapshots(&self) -> bool {
        let had_snapshot = self.mir_bodies.borrow().is_some()
            || self.tcx_snapshot.borrow().is_some()
            || self.enum_shape_defs.borrow().is_some()
            || self.struct_shape_defs.borrow().is_some()
            || !self.jit_eager_names.borrow().is_empty()
            || self.jit_cache_key.borrow().is_some();
        *self.mir_bodies.borrow_mut() = None;
        *self.tcx_snapshot.borrow_mut() = None;
        *self.enum_shape_defs.borrow_mut() = None;
        *self.struct_shape_defs.borrow_mut() = None;
        *self.jit_eager_names.borrow_mut() = Arc::new(std::collections::HashSet::new());
        *self.jit_cache_key.borrow_mut() = None;
        if had_snapshot {
            self.jit_counters.snapshots_released();
        }
        had_snapshot
    }

    /// Enables comptime folding: the next [`Self::load`] evaluates every
    /// `comptime { ... }` block and `comptime fn` call and records the
    /// results, drainable with [`Self::take_comptime_folds`].
    pub fn set_collect_comptime(&self, on: bool) {
        self.collect_comptime.set(on);
    }

    /// Drains the comptime evaluation results gathered by the last
    /// [`Self::load`] (empty unless [`Self::set_collect_comptime`] was
    /// set). Each entry is `(span, raw, Ok(value) | Err(message))`, where
    /// `raw` marks a `codegen!` region spliced as source.
    pub fn take_comptime_folds(&self) -> Vec<crate::vm::ComptimeFold> {
        std::mem::take(&mut self.comptime_folds.borrow_mut())
    }

    /// Compiles and registers every `fn`/`const`/`static`/impl item in
    /// `program`. ADT constructors register first, then each
    /// `const`/`static` initializer is evaluated by compiling it to a
    /// synthetic nullary chunk and running it on the VM, then functions
    /// compile last (so they inline the now-known const values). Items
    /// the VM can't lower produce a runtime error.
    ///
    /// `tcx` is taken by value: the JIT prepass drives
    /// [`gossamer_mir::lower_program`] (which interns inferred types
    /// during lowering) and the resulting interner is then moved into
    /// the deferred-JIT snapshot. Ownership transfer avoids both a
    /// clone and the earlier `mem::take`-on-a-borrow footgun (which
    /// silently emptied the caller's interner, breaking any second
    /// `load` on the same `tcx`). Callers that need the `tcx`
    /// afterwards must clone before calling.
    pub fn load(
        &mut self,
        program: &HirProgram,
        tcx: TyCtxt,
        enable_inlining: bool,
    ) -> RuntimeResult<()> {
        // Prepass: collect struct field orderings so `__struct`
        // can place literal fields in declaration order and the
        // VM compiler can emit compile-time offset reads.
        // Two maps: `name_layouts` (by struct name) for the
        // runtime `__struct` reorder, and `def_layouts` (by
        // DefId) for compile-time offset resolution.
        let mut name_layouts: HashMap<String, Vec<String>> = HashMap::new();
        let mut def_layouts: HashMap<gossamer_resolve::DefId, Vec<String>> = HashMap::new();
        // Trivial-wrapper table. `fn fsqrt(x: f64) -> f64 { math::sqrt(x) }`
        // and similar single-expression passthroughs get recorded
        // so the compiler can emit the intrinsic directly at
        // every call site, skipping an entire function frame per
        // call.
        let mut wrappers: HashMap<String, Vec<String>> = HashMap::new();
        // User-function inlining table. A free function whose body is a
        // single side-effect-transparent tail expression (`fn mat_a(i, j)
        // -> f64 { … }`) is re-compiled directly at each call site,
        // skipping the per-call frame. Built once here; consulted while
        // compiling every function body in pass C.
        let mut inline_fns = crate::compile::InlinableFns::new();
        for item in &program.items {
            match &item.kind {
                HirItemKind::Adt(adt) => {
                    if let gossamer_hir::HirAdtKind::Struct(fields) = &adt.kind {
                        let names: Vec<String> = fields.iter().map(|f| f.name.clone()).collect();
                        name_layouts.insert(adt.name.name.clone(), names.clone());
                        if let Some(def) = item.def {
                            def_layouts.insert(def, names);
                        }
                    }
                }
                HirItemKind::Fn(decl) => {
                    if let Some(target) = detect_trivial_wrapper(decl) {
                        wrappers.insert(decl.name.name.clone(), target);
                    }
                    // The user-function inliner is a performance optimization
                    // for `gos run` / `gos build` / `gos bench`. `gos test`
                    // loads with `enable_inlining == false` so a failed test's
                    // call-chain traceback preserves every intermediate frame.
                    let inlinable = if enable_inlining {
                        crate::compile::detect_inlinable_fn(decl, &tcx)
                    } else {
                        None
                    };
                    if let Some(info) = inlinable {
                        inline_fns.insert(decl.name.name.clone(), info);
                    }
                }
                _ => {}
            }
        }
        crate::builtins::set_struct_layouts(name_layouts);
        // Qualified names of every user `&mut self` method, so a call on
        // a place receiver routes through the write-back cell protocol
        // and the receiver's mutation persists (the `for x in <custom
        // iterator>` / stateful-method mechanism). See
        // `compile::collect_mut_self_methods`.
        let method_muts = crate::compile::collect_mut_self_methods(program);
        // Names of `static mut` items. The compiler lowers an
        // assignment rooted at one of these into an `Op::StoreStatic`
        // against the shared `Global::MutStatic` cell.
        let mut_statics = crate::compile::collect_mut_statics(program);
        // (Previously: a per-program JSON struct-schema registry was
        // built here so the VM tier could intercept
        // `<Type>::from_json` calls. 0.7.0 replaces that with
        // compile-time codegen in `gossamer-parse::autoderive`; the
        // synthesized methods are real Gossamer code and need no
        // VM-side bookkeeping.)
        //
        // Pre-evaluated values for top-level `const` items (and immutable
        // `static`s). A path that resolves to one of these inlines as a
        // `LoadConst` instead of a string-keyed `LoadGlobal` lookup.
        // Filled by pass B below and consumed when compiling functions in
        // pass C.
        let mut module_consts: HashMap<String, Value> = HashMap::new();

        // Pass A: register ADT constructors so const/static initializers
        // and function bodies can resolve enum variants.
        for item in &program.items {
            if matches!(&item.kind, HirItemKind::Adt(_)) {
                self.load_item(
                    item,
                    &tcx,
                    &def_layouts,
                    &wrappers,
                    &inline_fns,
                    &module_consts,
                    &method_muts,
                    &mut_statics,
                )?;
            }
        }

        // Pass B: evaluate every `const`/`static` initializer on the VM.
        // Each initializer compiles to a synthetic nullary chunk and runs
        // through `apply`; the resulting value registers in `globals`. A
        // `static mut` gets a shared `Global::MutStatic` cell so writes
        // from `Op::StoreStatic` persist and are observable. Immutable
        // consts/statics also feed `module_consts` for the inline path -
        // mutable statics are deliberately excluded: their reads flow
        // through `LoadGlobal` to the live cell so every store is seen.
        for item in &program.items {
            let module_prefix = if item.module_path.is_empty() {
                None
            } else {
                Some(item.module_path.join("::"))
            };
            match &item.kind {
                HirItemKind::Const(decl) => {
                    let value = match self.eval_initializer(
                        &decl.value,
                        &tcx,
                        &def_layouts,
                        &wrappers,
                        &inline_fns,
                        &module_consts,
                        &method_muts,
                        &mut_statics,
                    ) {
                        Ok(value) => value,
                        // While collecting comptime folds, functions are
                        // not yet loaded (pass C), so an initializer that
                        // calls a non-inlinable function cannot evaluate
                        // here. Its comptime regions are evaluated in
                        // pass D regardless, so skip rather than abort.
                        Err(_) if self.collect_comptime.get() => continue,
                        Err(err) => return Err(err),
                    };
                    self.register_item_value(
                        module_prefix.as_deref(),
                        &decl.name.name,
                        Global::Value(value.clone()),
                    );
                    module_consts.insert(decl.name.name.clone(), value);
                }
                HirItemKind::Static(decl) => {
                    let value = match self.eval_initializer(
                        &decl.value,
                        &tcx,
                        &def_layouts,
                        &wrappers,
                        &inline_fns,
                        &module_consts,
                        &method_muts,
                        &mut_statics,
                    ) {
                        Ok(value) => value,
                        Err(_) if self.collect_comptime.get() => continue,
                        Err(err) => return Err(err),
                    };
                    let global = if decl.mutable {
                        Global::MutStatic(Arc::new(parking_lot::Mutex::new(value.clone())))
                    } else {
                        Global::Value(value.clone())
                    };
                    self.register_item_value(module_prefix.as_deref(), &decl.name.name, global);
                    if !decl.mutable {
                        module_consts.insert(decl.name.name.clone(), value);
                    }
                }
                _ => {}
            }
        }

        // Pass C: compile and register every function / impl / trait
        // method, inlining the const values gathered in pass B.
        for item in &program.items {
            if matches!(
                &item.kind,
                HirItemKind::Fn(_) | HirItemKind::Impl(_) | HirItemKind::Trait(_)
            ) {
                self.load_item(
                    item,
                    &tcx,
                    &def_layouts,
                    &wrappers,
                    &inline_fns,
                    &module_consts,
                    &method_muts,
                    &mut_statics,
                )?;
            }
        }
        // Pass D: comptime folding. When enabled, evaluate every
        // `comptime { ... }` block and `comptime fn` call now that
        // every function and const is compiled, recording each result
        // span -> value so the CLI can splice the literal back into the
        // source. Gated so normal runs never pay the walk.
        if self.collect_comptime.get() {
            let comptime_info = crate::comptime::ComptimeInfo::collect(program);
            let regions = crate::comptime::collect_regions(program, &comptime_info);
            let mut folds = Vec::with_capacity(regions.len());
            for region in regions {
                let outcome = self
                    .eval_initializer(
                        region.expr,
                        &tcx,
                        &def_layouts,
                        &wrappers,
                        &inline_fns,
                        &module_consts,
                        &method_muts,
                        &mut_statics,
                    )
                    .map_err(|err| err.to_string());
                folds.push((region.span, region.raw, outcome));
            }
            *self.comptime_folds.borrow_mut() = folds;
        }

        // Tier D2 - deferred JIT. Lower MIR up front so the
        // tier-up trigger (in `apply`) can dispatch a compile via
        // `&self`, but don't compile yet: short-running programs never
        // trip the per-chunk hot counter and skip the cranelift cost
        // entirely.
        // `--no-jit` / `GOS_JIT=0` skips the MIR lower too.
        //
        // Scripts with no recursive helper functions also skip the MIR
        // lower: there is no body that can amortize native compilation across
        // repeated bytecode-to-native entries.
        // Coverage runs stay on the bytecode path: the cranelift JIT
        // lowers from MIR and never sees the `Op::CovHit` markers, so a
        // promoted function would silently stop recording line hits.
        if jit_call::jit_enabled() && has_jit_eligible_fn(program) && !self.coverage_active() {
            let mut jit_tcx = tcx.clone();
            let (shapes, enum_shape_handles) = build_native_enum_shapes(program, &jit_tcx);
            let (struct_shapes, struct_shape_handles) =
                build_native_struct_shapes(program, &jit_tcx);
            let mut bodies = gossamer_mir::lower_program(program, &mut jit_tcx);
            // Monomorphise before the JIT sees the bodies, exactly as the LLVM
            // AOT pipeline does. A generic function / method / struct
            // instantiated with a concrete type must reach the JIT as a
            // specialised body with concrete field and return types; left as a
            // `Param T` template, the JIT statically under-sizes a multi-slot
            // generic-struct aggregate (`Wrapper<Point>` lays out one slot
            // instead of two) and overflows its stack slot. The bytecode VM is
            // unaffected - it runs the separately-compiled chunks, not these
            // MIR bodies, which are JIT-only.
            gossamer_mir::monomorphise(&mut bodies, &mut jit_tcx);
            // The in-process JIT's win is eliding repeated bytecode dispatch
            // inside native recursion. Gate the compile snapshot on that
            // shape so programs that cannot promote a useful body do not
            // prepare the native compiler.
            if jit_backend::has_worthy_jit_body(&bodies, &jit_tcx, &shapes, &struct_shapes) {
                gossamer_mir::inline_trivial_wrappers(&mut bodies);
                gossamer_mir::inline_small_callees(&mut bodies);
                gossamer_mir::inline_general(&mut bodies);
                // Same post-inline cleanup the AOT pipeline runs: the
                // cranelift lowering is shared with `gos build`, so the
                // JIT must hand it the same MIR shape or the tiers can
                // diverge on constructs only one shape exercises.
                for body in &mut bodies {
                    gossamer_mir::optimise(body, &jit_tcx);
                }
                // Compute the eager-compile set from the post-inlining
                // bodies now, while they are still in hand: the deferred
                // compile below releases `mir_bodies` for spawn-free
                // programs, so this is the last point the set is derivable.
                let mut eager_names: std::collections::HashSet<String> =
                    jit_backend::jit_eager_loop_bodies(&bodies, &jit_tcx, &shapes, &struct_shapes)
                        .into_iter()
                        .collect();
                if !eager_names.is_empty() {
                    eager_names.insert("main".to_string());
                }
                *self.jit_eager_names.borrow_mut() = Arc::new(eager_names);
                // Keep the full, collision-free compiler description rather
                // than a lossy hash: an accidental cache hit could dispatch a
                // function compiled for a different type layout. This string
                // exists only while deferred tier-up remains possible and is
                // released together with the MIR/type snapshot.
                *self.jit_cache_key.borrow_mut() = Some(Arc::from(jit_artifact_key(
                    &bodies,
                    &jit_tcx,
                    &shapes,
                    &struct_shapes,
                )));
                *self.enum_shape_defs.borrow_mut() = Some(Arc::new(shapes));
                *self.enum_shape_handles.borrow_mut() = Some(enum_shape_handles);
                *self.struct_shape_defs.borrow_mut() = Some(Arc::new(struct_shapes));
                *self.struct_shape_handles.borrow_mut() = Some(struct_shape_handles);
                *self.mir_bodies.borrow_mut() = Some(Arc::new(bodies));
                // Store the JIT-local type context. MIR lowering and
                // monomorphisation intern additional types, so they must not
                // mutate the `tcx` the bytecode VM compiled its chunks
                // against; otherwise an empty promotion set can still perturb
                // VM-only execution.
                *self.tcx_snapshot.borrow_mut() = Some(Arc::new(jit_tcx));
                // A spawn-free program never hands its MIR to a child Vm, so
                // the deferred compile can free it the moment it lands - well
                // before the program's allocation peak.
                self.jit_droppable
                    .set(!program_has_spawn_sites(&self.globals));
            } else {
                self.jit.write().compiled = JitCompileState::Failed;
            }
        } else {
            self.jit.write().compiled = JitCompileState::Failed;
            // No JIT snapshot to retain. Every chunk compiled against the
            // borrowed `&tcx` during the passes above, so the owned `tcx`
            // is no longer needed and drops here.
        }
        // End-of-load compaction: every item is registered, so the
        // overlay HashMap has reached its steady-state size. Release
        // hashbrown's growth-by-doubling slack.
        if let Some(globals) = Arc::get_mut(&mut self.globals) {
            globals.shrink_to_fit();
        }
        Ok(())
    }

    /// Compiles the saved MIR through cranelift and fills the JIT
    /// override map. Called the first time any chunk's tier-up
    /// counter trips. The state machine on `JitState::compiled`
    /// short-circuits concurrent goroutine trips so `compile_to_jit`
    /// runs at most once per `Arc<RwLock<JitState>>`. Failures
    /// transition to `Failed` and stay there - no observable
    /// behaviour change for the bytecode path.
    pub(crate) fn try_compile_jit_lazy(&self) {
        // Fast read-only check first: avoids exclusive locks once
        // the compile has settled (Done / Failed). The hot
        // counter at the call site already got us here, so the
        // common case after the first goroutine wins is `Done`.
        {
            let state = self.jit.read();
            if matches!(
                state.compiled,
                JitCompileState::Done | JitCompileState::InProgress | JitCompileState::Failed
            ) {
                return;
            }
        }
        // Take an exclusive lock to flip Pending → InProgress.
        {
            let mut state = self.jit.write();
            if state.compiled != JitCompileState::Pending {
                return;
            }
            if let Some((rss_bytes, cap_bytes)) = jit_rss_sample_and_cap() {
                self.jit_counters.observed_rss(rss_bytes);
                if rss_bytes >= cap_bytes {
                    state.compiled = JitCompileState::Failed;
                    self.jit_counters.ram_skipped_compile(rss_bytes);
                    if jit_call::jit_trace() {
                        eprintln!(
                            "jit: skip compile at rss={rss_bytes} bytes cap={cap_bytes} bytes"
                        );
                    }
                    drop(state);
                    self.release_terminal_jit_snapshot();
                    return;
                }
            }
            state.compiled = JitCompileState::InProgress;
        }
        if !jit_call::jit_enabled() {
            self.jit.write().compiled = JitCompileState::Failed;
            self.release_terminal_jit_snapshot();
            return;
        }
        // Clone the Arcs out so the `RefCell` borrows release before the
        // compile, leaving the fields free to be dropped afterwards.
        let Some(bodies) = self.mir_bodies.borrow().clone() else {
            self.jit.write().compiled = JitCompileState::Failed;
            self.release_terminal_jit_snapshot();
            return;
        };
        let Some(tcx) = self.tcx_snapshot.borrow().clone() else {
            self.jit.write().compiled = JitCompileState::Failed;
            self.release_terminal_jit_snapshot();
            return;
        };
        let bodies = &bodies;
        let tcx = &tcx;
        let trace = jit_call::jit_trace();
        let started = std::time::Instant::now();
        let empty = std::collections::HashMap::new();
        let shape_defs_arc = self.enum_shape_defs.borrow().clone();
        let shape_defs: &std::collections::HashMap<u32, u32> =
            shape_defs_arc.as_deref().unwrap_or(&empty);
        let struct_shape_defs_arc = self.struct_shape_defs.borrow().clone();
        let struct_shape_defs: &std::collections::HashMap<u32, u32> =
            struct_shape_defs_arc.as_deref().unwrap_or(&empty);
        let cache_key = self.jit_cache_key.borrow().clone();
        if let Some(cache_key) = cache_key.as_deref()
            && let Some(artifact) = thread_jit_artifact(cache_key)
        {
            if self.install_jit_artifact(artifact) {
                self.jit_counters.artifact_reused();
            }
            self.release_terminal_jit_snapshot();
            return;
        }

        self.jit_counters.compile_started();
        let artifact = match jit_backend::compile_to_jit_for_promotion(
            bodies,
            tcx,
            shape_defs,
            struct_shape_defs,
        ) {
            Ok(art) => art,
            Err(err) => {
                if trace {
                    eprintln!("jit: compile_to_jit_for_promotion failed: {err}");
                }
                self.jit.write().compiled = JitCompileState::Failed;
                self.release_terminal_jit_snapshot();
                return;
            }
        };
        let compile_ms = started.elapsed().as_millis();
        if trace {
            eprintln!(
                "jit: compiled {} functions in {compile_ms} ms",
                artifact.functions.len()
            );
        }
        if artifact.functions.is_empty() {
            self.jit.write().compiled = JitCompileState::Failed;
            self.release_terminal_jit_snapshot();
            return;
        }
        let artifact = Rc::new(artifact);
        if let Some(cache_key) = cache_key {
            cache_thread_jit_artifact(cache_key, &artifact);
        }
        if self.install_jit_artifact(artifact) {
            self.jit_counters.compile_succeeded();
        } else {
            self.jit_counters.artifact_discarded();
        }
        self.release_terminal_jit_snapshot();
        gossamer_runtime::collect_process_allocator(true);
    }

    /// Installs callable entries from an immutable artifact. The artifact is
    /// held by this VM before its raw entry pointers become reachable through
    /// `overrides`, so a cache eviction can never invalidate a dispatch.
    fn install_jit_artifact(&self, artifact: Rc<JitArtifact>) -> bool {
        let trace = jit_call::jit_trace();
        let mut state = self.jit.write();
        for (name, jit_fn) in &artifact.functions {
            let Some(Global::Fn(chunk)) = self.lookup_global_ref(name.as_str()) else {
                continue;
            };
            // Keep panic-capable code on bytecode so the VM's diagnostic and
            // side-effect semantics remain the single execution path.
            if chunk.globals.iter().any(|g| g == "panic") {
                continue;
            }
            if trace {
                eprintln!("jit: promote {name}");
            }
            #[allow(
                clippy::arc_with_non_send_sync,
                reason = "JitFn is immutable but intentionally not Send/Sync; artifacts are cached per thread"
            )]
            let jit_arc = Arc::clone(jit_fn);
            state.insert_override(name.clone(), jit_arc.clone());
            state
                .chunk_overrides
                .insert(Arc::as_ptr(chunk) as usize, jit_arc);
        }
        let installed = !state.overrides.is_empty();
        if installed {
            self.jit_override_count
                .store(state.overrides.len(), Ordering::Release);
            state.artifact = Some(artifact);
            state.compiled = JitCompileState::Done;
        } else {
            state.compiled = JitCompileState::Failed;
        }
        drop(state);
        for chunk_state in self.chunk_state_map.borrow().values() {
            *chunk_state.jit_resolve.borrow_mut() = crate::vm::JitResolve::Unresolved;
        }
        installed
    }

    /// A terminal tier-up result has no future use for compiler snapshots in
    /// spawn-free programs, including RAM skips and compilation failures.
    fn release_terminal_jit_snapshot(&self) {
        if self.jit_droppable.get() {
            self.release_deferred_jit_snapshots();
        }
    }

    /// Evaluates a `const`/`static` initializer by compiling `expr` into
    /// a synthetic nullary chunk and running it on the VM. `module_consts`
    /// lets the initializer inline any earlier const it references.
    fn eval_initializer(
        &self,
        expr: &gossamer_hir::HirExpr,
        tcx: &TyCtxt,
        layouts: &HashMap<gossamer_resolve::DefId, Vec<String>>,
        wrappers: &HashMap<String, Vec<String>>,
        inline_fns: &crate::compile::InlinableFns,
        module_consts: &HashMap<String, Value>,
        method_muts: &crate::compile::MutSelfMethods,
        mut_statics: &crate::compile::MutStatics,
    ) -> RuntimeResult<Value> {
        let cov_map = self.coverage_source_map();
        let chunk = Arc::new(crate::compile::compile_initializer(
            expr,
            tcx,
            layouts,
            wrappers,
            inline_fns,
            module_consts,
            method_muts,
            mut_statics,
            cov_map.as_deref(),
        )?);
        validate_chunk_for_execution(&chunk)?;
        // Run with a fresh, local `ChunkState` rather than the
        // address-keyed `chunk_state_for` cache. An initializer chunk is
        // dropped as soon as it returns, so caching its heap address
        // would alias a later real chunk allocated at the same slot.
        let jit_disabled = !jit_call::jit_enabled();
        // A one-shot initializer chunk is dropped as soon as it returns, so
        // there is never a second call to amortize a compile - keep it on
        // the bytecode path regardless of shape.
        let state = ChunkState::new(
            chunk.call_cache_count,
            chunk.arith_cache_count,
            chunk.field_cache_count,
            chunk.instrs.len(),
            jit_disabled,
            false,
        );
        self.run_local(Arc::clone(&chunk), &state, Vec::new())
    }

    /// Registers a `const`/`static` value in `globals` under both its
    /// bare name and (if the item lives in a module) its qualified name.
    /// Bumps the globals generation so any inline cache stamped against
    /// the prior map re-validates.
    fn register_item_value(&mut self, prefix: Option<&str>, name: &str, global: Global) {
        self.bump_globals_generation();
        let globals = Arc::make_mut(&mut self.globals);
        let intern = crate::value::intern_type_name;
        if let Some(prefix) = prefix {
            globals.insert(intern(&format!("{prefix}::{name}")), global.clone());
        }
        globals.insert(intern(name), global);
    }

    pub(crate) fn load_item(
        &mut self,
        item: &HirItem,
        tcx: &TyCtxt,
        layouts: &HashMap<gossamer_resolve::DefId, Vec<String>>,
        wrappers: &HashMap<String, Vec<String>>,
        inline_fns: &crate::compile::InlinableFns,
        module_consts: &HashMap<String, Value>,
        method_muts: &crate::compile::MutSelfMethods,
        mut_statics: &crate::compile::MutStatics,
    ) -> RuntimeResult<()> {
        // Resolve the coverage source map before borrowing `globals`
        // (the helper takes `&self`, which would conflict with the
        // `&mut self.globals` borrow held below). An owned `Arc` clone
        // keeps no borrow of `self` outstanding.
        let cov_map = self.coverage_source_map();
        // Loading an item mutates the globals map. Bump the
        // generation so any IC slots already populated against an
        // earlier snapshot of `globals` re-validate. Today every
        // `load_item` call happens before the dispatch loop runs
        // for the current chunk, but the bump is correctness-by-
        // construction: any future op that calls `load_item` mid-
        // run still gets a valid invalidation signal.
        self.bump_globals_generation();
        // A method whose bare name matches a prelude builtin (`clone`,
        // `len`, `push`, ...) must not shadow it in the per-Vm overlay:
        // overlay entries win over the prelude, so a bare-name insert
        // would reroute every `String`/`Vec`/enum receiver - which falls
        // back to the bare name when no `Type::method` key matches - into
        // a type-specific impl that only understands its own shape.
        let prelude = Arc::clone(&self.prelude);
        let globals = Arc::make_mut(&mut self.globals);
        let module_prefix = if item.module_path.is_empty() {
            None
        } else {
            Some(item.module_path.join("::"))
        };
        let intern = crate::value::intern_type_name;
        match &item.kind {
            HirItemKind::Fn(decl) => {
                // The chunk's identity carries the canonical
                // `mod::name` so JIT promotion and stack traces
                // distinguish same-named functions across modules;
                // call sites reference the qualified spelling.
                let compiled: gossamer_hir::HirFn = if let Some(prefix) = &module_prefix {
                    let mut renamed = decl.clone();
                    renamed.name =
                        gossamer_ast::Ident::new(format!("{prefix}::{}", decl.name.name));
                    renamed
                } else {
                    decl.clone()
                };
                let chunk = compile_fn(
                    &compiled,
                    tcx,
                    layouts,
                    wrappers,
                    inline_fns,
                    module_consts,
                    method_muts,
                    mut_statics,
                    cov_map.as_deref(),
                )?;
                validate_chunk_for_execution(&chunk)?;
                let shared = chunk.into_shared();
                if let Some(prefix) = &module_prefix {
                    let qualified = format!("{prefix}::{}", decl.name.name);
                    globals.insert(intern(&qualified), Global::Fn(shared.clone()));
                }
                globals.insert(intern(&decl.name.name), Global::Fn(shared));
            }
            HirItemKind::Impl(decl) => {
                for method in &decl.methods {
                    let chunk = compile_fn(
                        method,
                        tcx,
                        layouts,
                        wrappers,
                        inline_fns,
                        module_consts,
                        method_muts,
                        mut_statics,
                        cov_map.as_deref(),
                    )?;
                    validate_chunk_for_execution(&chunk)?;
                    let shared = chunk.into_shared();
                    if let Some(type_name) = &decl.self_name {
                        let qualified = format!("{}::{}", type_name.name, method.name.name);
                        globals.insert(intern(&qualified), Global::Fn(shared.clone()));
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                intern(&format!("{prefix}::{qualified}")),
                                Global::Fn(shared.clone()),
                            );
                        }
                    }
                    if !prelude.contains_key(method.name.name.as_str()) {
                        globals.insert(intern(&method.name.name), Global::Fn(shared));
                    }
                }
            }
            HirItemKind::Trait(decl) => {
                for method in &decl.methods {
                    if method.body.is_some() {
                        let chunk = compile_fn(
                            method,
                            tcx,
                            layouts,
                            wrappers,
                            inline_fns,
                            module_consts,
                            method_muts,
                            mut_statics,
                            cov_map.as_deref(),
                        )?;
                        validate_chunk_for_execution(&chunk)?;
                        let shared = chunk.into_shared();
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                intern(&format!("{prefix}::{}", method.name.name)),
                                Global::Fn(shared.clone()),
                            );
                        }
                        globals.insert(intern(&method.name.name), Global::Fn(shared));
                    }
                }
            }
            // `const` / `static` items are evaluated and registered by
            // `load`'s dedicated pass (see [`Self::eval_initializer`] /
            // [`Self::register_item_value`]) before any function compiles,
            // so there is nothing to do here.
            HirItemKind::Const(_) | HirItemKind::Static(_) => {}
            HirItemKind::Adt(decl) => {
                if let gossamer_hir::HirAdtKind::Enum(variants) = &decl.kind {
                    let type_name = decl.name.name.as_str();
                    for variant in variants {
                        let variant_name = variant.name.name.as_str();
                        let qualified = format!("{type_name}::{variant_name}");
                        let sentinel = Value::variant(variant_name, Vec::new());
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                intern(&format!("{prefix}::{qualified}")),
                                Global::Value(sentinel.clone()),
                            );
                        }
                        globals.insert(intern(variant_name), Global::Value(sentinel.clone()));
                        globals.insert(intern(&qualified), Global::Value(sentinel));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Returns a live artifact for `key` from this OS thread. Pruning dead weak
/// entries here keeps the cache metadata bounded even when many short-lived
/// programs execute on one worker.
fn thread_jit_artifact(key: &str) -> Option<Rc<JitArtifact>> {
    THREAD_JIT_ARTIFACTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|entry| entry.artifact.strong_count() != 0);
        cache
            .iter()
            .find(|entry| entry.key.as_ref() == key)
            .and_then(|entry| entry.artifact.upgrade())
    })
}

/// Publishes an artifact only to the current thread. The cache itself holds a
/// `Weak`, so it coordinates reuse but never extends executable-page lifetime.
fn cache_thread_jit_artifact(key: Arc<str>, artifact: &Rc<JitArtifact>) {
    THREAD_JIT_ARTIFACTS.with(|cache| {
        let mut cache = cache.borrow_mut();
        cache.retain(|entry| {
            entry.artifact.strong_count() != 0 && entry.key.as_ref() != key.as_ref()
        });
        while cache.len() >= THREAD_JIT_ARTIFACT_CACHE_CAP {
            cache.pop_front();
        }
        cache.push_back(ThreadJitArtifact {
            key,
            artifact: Rc::downgrade(artifact),
        });
    });
}

/// Full (rather than hashed) identity for code pages produced from a compiler
/// snapshot. Sorted map entries avoid randomized `HashMap` iteration turning
/// identical programs into a cache miss; string equality avoids collision
/// risks at this unsafe ABI boundary.
fn jit_artifact_key(
    bodies: &[Body],
    tcx: &TyCtxt,
    enum_shape_defs: &std::collections::HashMap<u32, u32>,
    struct_shape_defs: &std::collections::HashMap<u32, u32>,
) -> String {
    fn sorted_map(map: &std::collections::HashMap<u32, u32>) -> Vec<(u32, u32)> {
        let mut entries: Vec<_> = map.iter().map(|(&key, &value)| (key, value)).collect();
        entries.sort_unstable();
        entries
    }

    format!(
        "bodies={bodies:?};tcx={};enum_shapes={:?};struct_shapes={:?}",
        tcx.stable_snapshot_key(),
        sorted_map(enum_shape_defs),
        sorted_map(struct_shape_defs),
    )
}

fn jit_rss_sample_and_cap() -> Option<(u64, u64)> {
    let cap_mb = std::env::var("GOS_JIT_MAX_RSS_MB").ok()?;
    let cap_mb = cap_mb.trim().parse::<u64>().ok()?;
    if cap_mb == 0 {
        return None;
    }
    let cap_bytes = cap_mb.saturating_mul(1024 * 1024);
    current_process_rss_bytes().map(|rss| (rss, cap_bytes))
}

#[cfg(target_os = "linux")]
fn current_process_rss_bytes() -> Option<u64> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    for line in status.lines() {
        let Some(rest) = line.strip_prefix("VmRSS:") else {
            continue;
        };
        let kb = rest.split_whitespace().next()?.parse::<u64>().ok()?;
        return Some(kb.saturating_mul(1024));
    }
    None
}

#[cfg(target_os = "macos")]
fn current_process_rss_bytes() -> Option<u64> {
    // macOS reports `ru_maxrss` in bytes (unlike Linux's KiB convention).
    // It is a high-water mark, which is conservative for a JIT admission cap:
    // a process that has already exceeded the cap must not start another
    // native compilation merely because a few pages were later released.
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `usage` points to valid writable storage for the duration of
    // the call, and RUSAGE_SELF requests statistics for this process only.
    if unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: a zero return from `getrusage` initializes the complete rusage.
    let rss = unsafe { usage.assume_init() }.ru_maxrss;
    u64::try_from(rss).ok()
}

#[cfg(windows)]
fn current_process_rss_bytes() -> Option<u64> {
    use windows_sys::Win32::System::ProcessStatus::{
        GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    };
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let mut counters = PROCESS_MEMORY_COUNTERS {
        cb: u32::try_from(std::mem::size_of::<PROCESS_MEMORY_COUNTERS>()).ok()?,
        ..Default::default()
    };
    // SAFETY: `counters` is a valid PROCESS_MEMORY_COUNTERS buffer whose
    // `cb` advertises its exact size; GetCurrentProcess returns a valid
    // pseudo-handle for the current process.
    if unsafe { GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, counters.cb) } == 0 {
        return None;
    }
    u64::try_from(counters.WorkingSetSize).ok()
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn current_process_rss_bytes() -> Option<u64> {
    None
}

/// Builds native shape descriptors for every heap enum whose values
/// can cross the JIT boundary as raw pointers (all variant fields are
/// scalars, strings, or other supported heap enums), registers them in
/// the process-global shape table, and returns `DefId.local -> shape
/// index` for the cranelift eligibility check.
fn build_native_enum_shapes(
    program: &HirProgram,
    tcx: &TyCtxt,
) -> (
    std::collections::HashMap<u32, u32>,
    Arc<Vec<Arc<crate::value::NativeEnumShape>>>,
) {
    use crate::value::{
        NativeEnumShape, NativeFieldKind, NativeVariantShape, intern_type_name,
        register_native_shapes,
    };
    use gossamer_types::TyKind;
    struct Cand<'a> {
        def_local: u32,
        name: &'a str,
        variants: Vec<(&'a str, Vec<gossamer_types::Ty>)>,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for item in &program.items {
        let HirItemKind::Adt(adt) = &item.kind else {
            continue;
        };
        let gossamer_hir::HirAdtKind::Enum(variants) = &adt.kind else {
            continue;
        };
        let Some(def) = item.def else { continue };
        let vs: Vec<(&str, Vec<gossamer_types::Ty>)> = variants
            .iter()
            .map(|v| {
                (
                    v.name.name.as_str(),
                    v.struct_field_tys.clone().unwrap_or_default(),
                )
            })
            .collect();
        cands.push(Cand {
            def_local: def.local,
            name: adt.name.name.as_str(),
            variants: vs,
        });
    }
    let in_set: std::collections::HashSet<u32> = cands.iter().map(|c| c.def_local).collect();
    // A variant field is JIT-marshallable when it is a scalar / string, a
    // supported heap enum, a `Vec<Enum>` of a supported enum, or a
    // `Vec<(String, Enum)>` of a supported enum (the recursive `Arr` / `Obj`
    // shapes). The inner-enum support is consulted through the running
    // fixpoint set so a Vec of a still-supported enum keeps the parent alive.
    let field_supported = |t: gossamer_types::Ty,
                           supported: &std::collections::HashSet<u32>|
     -> bool {
        match tcx.kind_of(t) {
            TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::String => {
                true
            }
            TyKind::Adt { def, .. } => supported.contains(&def.local),
            TyKind::Vec(inner) | TyKind::Slice(inner) => match tcx.kind_of(*inner) {
                TyKind::Adt { def, .. } => supported.contains(&def.local),
                TyKind::Tuple(elems) if elems.len() == 2 => {
                    matches!(tcx.kind_of(elems[0]), TyKind::String)
                        && matches!(
                            tcx.kind_of(elems[1]),
                            TyKind::Adt { def, .. } if supported.contains(&def.local)
                        )
                }
                _ => false,
            },
            _ => false,
        }
    };
    // Fixpoint: drop enums with unsupported fields (or fields of
    // dropped enums) until stable.
    let mut supported: std::collections::HashSet<u32> = in_set.clone();
    loop {
        let mut changed = false;
        for c in &cands {
            if !supported.contains(&c.def_local) {
                continue;
            }
            let ok = c
                .variants
                .iter()
                .all(|(_, tys)| tys.iter().all(|t| field_supported(*t, &supported)));
            if !ok {
                supported.remove(&c.def_local);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let kept: Vec<&Cand> = cands
        .iter()
        .filter(|c| supported.contains(&c.def_local))
        .collect();
    if kept.is_empty() {
        return (std::collections::HashMap::new(), Arc::new(Vec::new()));
    }
    // Two-phase index assignment so recursive/mutual references
    // resolve: indices are decided up front, shapes built after. The
    // base read and the registration run under one lock inside
    // `register_native_shapes`, so two concurrent loads can't assign
    // overlapping indices.
    register_native_shapes(|base| {
        let idx_of: std::collections::HashMap<u32, u32> = kept
            .iter()
            .enumerate()
            .map(|(i, c)| (c.def_local, base + u32::try_from(i).unwrap_or(0)))
            .collect();
        let shapes: Vec<std::sync::Arc<NativeEnumShape>> = kept
            .iter()
            .map(|c| {
                let tagged = !c.variants.is_empty() && c.variants.len() <= 4;
                let variants: Vec<NativeVariantShape> = c
                    .variants
                    .iter()
                    .map(|(vname, tys)| NativeVariantShape {
                        name: intern_type_name(vname),
                        fields: tys
                            .iter()
                            .map(|t| match tcx.kind_of(*t) {
                                TyKind::Int(_) => NativeFieldKind::I64,
                                TyKind::Float(_) => NativeFieldKind::F64,
                                TyKind::Bool => NativeFieldKind::Bool,
                                TyKind::Char => NativeFieldKind::Char,
                                TyKind::String => NativeFieldKind::Str,
                                TyKind::Adt { def, .. } => {
                                    NativeFieldKind::Enum(idx_of[&def.local])
                                }
                                TyKind::Vec(inner) | TyKind::Slice(inner) => {
                                    match tcx.kind_of(*inner) {
                                        TyKind::Adt { def, .. } => {
                                            NativeFieldKind::VecEnum(idx_of[&def.local])
                                        }
                                        TyKind::Tuple(elems) if elems.len() == 2 => {
                                            let TyKind::Adt { def, .. } = tcx.kind_of(elems[1])
                                            else {
                                                unreachable!("filtered by the supported fixpoint")
                                            };
                                            NativeFieldKind::VecStrEnumTuple(idx_of[&def.local])
                                        }
                                        _ => unreachable!("filtered by the supported fixpoint"),
                                    }
                                }
                                _ => unreachable!("filtered by the supported fixpoint"),
                            })
                            .collect(),
                    })
                    .collect();
                std::sync::Arc::new(NativeEnumShape {
                    enum_name: intern_type_name(c.name),
                    index: idx_of[&c.def_local],
                    tagged,
                    variants,
                })
            })
            .collect();
        let handles = Arc::new(shapes.clone());
        (shapes, (idx_of, handles))
    })
}

/// Builds native shape descriptors for every user struct whose fields are
/// scalars or `String`, registers them in the process-global struct-shape
/// table, and returns `DefId.local -> shape index` for the Cranelift
/// eligibility check. A registered struct is a flat field-slot block at the
/// JIT boundary - one 8-byte slot per field. String slots own temporary
/// native strings that the trampoline copies out and frees after the call.
fn build_native_struct_shapes(
    program: &HirProgram,
    tcx: &TyCtxt,
) -> (
    std::collections::HashMap<u32, u32>,
    Arc<Vec<Arc<crate::value::NativeStructShape>>>,
) {
    use crate::value::{
        NativeFieldKind, NativeStructShape, intern_type_name, register_native_struct_shapes,
    };
    use gossamer_types::TyKind;
    struct Cand<'a> {
        def_local: u32,
        name: &'a str,
        fields: Vec<(&'a str, NativeFieldKind)>,
    }
    let mut cands: Vec<Cand> = Vec::new();
    for item in &program.items {
        let HirItemKind::Adt(adt) = &item.kind else {
            continue;
        };
        let gossamer_hir::HirAdtKind::Struct(field_names) = &adt.kind else {
            continue;
        };
        let Some(def) = item.def else { continue };
        // Tuple / unit structs carry no field names here; only named-field
        // structs have the positional name list the marshaller needs. A
        // single-field struct is excluded: the compiled tier treats a
        // 1-slot aggregate as a by-pointer scalar, so a `&self` field read
        // dereferences the slot rather than indexing the block - the
        // marshalled flat block is only the right shape at >= 2 fields.
        if field_names.len() < 2 {
            continue;
        }
        let Some(field_tys) = tcx.struct_field_tys(def) else {
            continue;
        };
        if field_tys.len() != field_names.len() {
            continue;
        }
        let mut fields = Vec::with_capacity(field_names.len());
        let mut supported = true;
        for (n, t) in field_names.iter().zip(field_tys.iter()) {
            let kind = match tcx.kind_of(*t) {
                TyKind::Int(_) => NativeFieldKind::I64,
                TyKind::Float(_) => NativeFieldKind::F64,
                TyKind::Bool => NativeFieldKind::Bool,
                TyKind::Char => NativeFieldKind::Char,
                TyKind::String => NativeFieldKind::Str,
                _ => {
                    supported = false;
                    break;
                }
            };
            fields.push((n.name.as_str(), kind));
        }
        if !supported {
            continue;
        }
        cands.push(Cand {
            def_local: def.local,
            name: adt.name.name.as_str(),
            fields,
        });
    }
    if cands.is_empty() {
        return (std::collections::HashMap::new(), Arc::new(Vec::new()));
    }
    register_native_struct_shapes(|base| {
        let idx_of: std::collections::HashMap<u32, u32> = cands
            .iter()
            .enumerate()
            .map(|(i, c)| (c.def_local, base + u32::try_from(i).unwrap_or(0)))
            .collect();
        let shapes: Vec<std::sync::Arc<NativeStructShape>> = cands
            .iter()
            .map(|c| {
                let fields = c
                    .fields
                    .iter()
                    .map(|(n, k)| (intern_type_name(n), *k))
                    .collect();
                std::sync::Arc::new(NativeStructShape {
                    struct_name: intern_type_name(c.name),
                    index: idx_of[&c.def_local],
                    fields,
                })
            })
            .collect();
        let handles = Arc::new(shapes.clone());
        (shapes, (idx_of, handles))
    })
}
