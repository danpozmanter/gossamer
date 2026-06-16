#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl Vm {
    /// Builds a VM pre-populated with the built-in intrinsics.
    #[must_use]
    pub fn new() -> Self {
        let mut vm = Self {
            globals: Arc::new(rustc_hash::FxHashMap::default()),
            prelude: builtins::prelude_globals(),
            pool: RefCell::new(FramePool::default()),
            mir_bodies: None,
            tcx_snapshot: None,
            enum_shape_defs: None,
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
            chunk_state_arena: RefCell::new(Vec::new()),
            chunk_state_map: RefCell::new(HashMap::new()),
            chunk_state_last: Cell::new(None),
            globals_generation: Cell::new(1),
            call_stack: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
            source_map: None,
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
    ) -> Self {
        Self {
            globals,
            prelude: builtins::prelude_globals(),
            pool: RefCell::new(FramePool::default()),
            mir_bodies,
            tcx_snapshot,
            enum_shape_defs,
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
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
        // Clear this goroutine-worker's traceback frames so the next
        // task starts with an empty call stack rather than inheriting
        // stale frames from the one that just finished.
        let mut call_stack = self.call_stack.borrow_mut();
        call_stack.clear();
        call_stack.shrink_to_fit();
    }

    /// Frees MIR bodies and the `TyCtxt` snapshot retained for deferred JIT.
    /// After JIT compilation fires (or is skipped), these are never read
    /// again on the main `Vm` - goroutines have already cloned their own Arcs.
    /// Call once after `vm.call()` returns to reclaim the per-program MIR
    /// allocation before the goroutine-join phase.
    pub fn release_jit_prelude(&mut self) {
        self.mir_bodies = None;
        self.tcx_snapshot = None;
        // The chunk-state arena (per-call IC slots, hot counters)
        // can grow large for big programs. Trim it back to the
        // steady-state floor while goroutines drain.
        self.chunk_state_arena.borrow_mut().shrink_to_fit();
        self.chunk_state_map.borrow_mut().shrink_to_fit();
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
        mut tcx: TyCtxt,
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
                    let value = self.eval_initializer(
                        &decl.value,
                        &tcx,
                        &def_layouts,
                        &wrappers,
                        &inline_fns,
                        &module_consts,
                        &method_muts,
                        &mut_statics,
                    )?;
                    self.register_item_value(
                        module_prefix.as_deref(),
                        &decl.name.name,
                        Global::Value(value.clone()),
                    );
                    module_consts.insert(decl.name.name.clone(), value);
                }
                HirItemKind::Static(decl) => {
                    let value = self.eval_initializer(
                        &decl.value,
                        &tcx,
                        &def_layouts,
                        &wrappers,
                        &inline_fns,
                        &module_consts,
                        &method_muts,
                        &mut_statics,
                    )?;
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
        // Tier D2 - deferred JIT. Lower MIR up front so the
        // tier-up trigger (in `apply`) can dispatch a compile via
        // `&self`, but don't compile yet: short-running programs
        // (`hello.gos`, REPL one-liners) never trip the per-chunk
        // hot counter and skip the cranelift cost entirely.
        // `--no-jit` / `GOS_JIT=0` skips the MIR lower too.
        //
        // Programs with no JIT-eligible helper functions (only
        // `main`) also skip the MIR lower: `main` is always run on
        // the bytecode path (see the `if name == "main" { continue; }`
        // skip in `try_compile_jit_lazy`) so lowering MIR for it
        // would never be consumed. `hello.gos` lands in this
        // bucket, shaving the lower + the tcx clone.
        // Coverage runs stay on the bytecode path: the cranelift JIT
        // lowers from MIR and never sees the `Op::CovHit` markers, so a
        // promoted function would silently stop recording line hits.
        if jit_call::jit_enabled() && has_jit_eligible_fn(program) && !self.coverage_active() {
            self.enum_shape_defs = Some(Arc::new(build_native_enum_shapes(program, &tcx)));
            let mut bodies = gossamer_mir::lower_program(program, &mut tcx);
            gossamer_mir::inline_trivial_wrappers(&mut bodies);
            gossamer_mir::inline_small_callees(&mut bodies);
            gossamer_mir::inline_general(&mut bodies);
            self.mir_bodies = Some(Arc::new(bodies));
            // Move the owned type context into the snapshot - no clone.
            // `load` takes `tcx` by value precisely so this hand-off is
            // a move: it duplicated the entire `TyCtxt` at load time
            // (the high-water allocation that set a small program's
            // MaxRSS) when the snapshot cloned, and stranded the caller
            // with an empty interner when it used `mem::take` on a
            // borrow. By-value ownership makes the consume explicit and
            // costs neither.
            self.tcx_snapshot = Some(Arc::new(tcx));
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
            state.compiled = JitCompileState::InProgress;
        }
        if !jit_call::jit_enabled() {
            self.jit.write().compiled = JitCompileState::Failed;
            return;
        }
        let Some(bodies) = self.mir_bodies.as_ref() else {
            self.jit.write().compiled = JitCompileState::Failed;
            return;
        };
        let Some(tcx) = self.tcx_snapshot.as_ref() else {
            self.jit.write().compiled = JitCompileState::Failed;
            return;
        };
        let trace = jit_call::jit_trace();
        let started = std::time::Instant::now();
        let empty = std::collections::HashMap::new();
        let shape_defs: &std::collections::HashMap<u32, u32> =
            self.enum_shape_defs.as_deref().unwrap_or(&empty);
        let artifact = match gossamer_codegen_cranelift::compile_to_jit(bodies, tcx, shape_defs) {
            Ok(art) => art,
            Err(err) => {
                if trace {
                    eprintln!("jit: compile_to_jit failed: {err}");
                }
                self.jit.write().compiled = JitCompileState::Failed;
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
        // The codegen's `println` dispatch routes per-arg through
        // the right runtime helper, so the historical
        // `println(<i64>)` segfault no longer applies. We do still
        // skip `main` because the cranelift intrinsic table
        // doesn't cover every stdlib call wired through the
        // interp's builtins (slog::info, exec::run,
        // compress::gzip::*, bufio::read_lines, etc. - anything
        // newly registered via `install_module` in `builtins.rs`).
        // When a JIT-compiled `main` hits one of those, the
        // codegen silently emits a no-op call instead of routing
        // back to the bytecode builtin, so the program runs but
        // produces no output. Keep `main` on the bytecode path so
        // those builtins fire reliably; helper functions still
        // get the native lowering, which is where the perf win
        // actually matters.
        let mut state = self.jit.write();
        for (name, jit_fn) in &artifact.functions {
            if name == "main" {
                continue;
            }
            // Only register an override for names the bytecode VM
            // actually has chunks for. Closure bodies and other
            // synthesised functions live only in the MIR; the VM
            // calls them through different paths.
            let Some(Global::Fn(chunk)) = self.lookup_global_ref(name.as_str()) else {
                continue;
            };
            // Skip promotion of any chunk that calls `panic`.
            // The cranelift codegen lowers `panic(...)` into a
            // `gos_rt_panic` call that aborts the process directly,
            // bypassing the bytecode VM's call-stack capture for the
            // user-facing diagnostic. Keeping panicking helpers on the
            // bytecode path preserves the call-stack render.
            //
            // Admitting these would also be unsound for side-effecting
            // helpers: the trampoline catches the unwind and falls back
            // to the bytecode chunk, which re-runs the body from the
            // start - any effect performed before the panic in the
            // native body would happen twice. The bytecode path renders
            // the same trace (exit 101, `main` -> helper) with neither
            // hazard, so the exclusion stays.
            if chunk.globals.iter().any(|g| g == "panic") {
                continue;
            }
            if trace {
                eprintln!("jit: promote {name}");
            }
            // `JitFn` carries a raw `*const u8` so it isn't
            // `Send + Sync`. The VM is single-threaded today, so
            // an `Arc` is the right shape for the override map's
            // shared ownership semantics - a `Rc` would prevent
            // the artifact's `Drop` from waiting for outstanding
            // override references on shutdown.
            #[allow(
                clippy::arc_with_non_send_sync,
                reason = "JitFn carries non-Sync raw fn ptrs; Arc shape needed for shared-ownership across the override map"
            )]
            let jit_arc = Arc::new(jit_fn.clone());
            state.insert_override(name.clone(), jit_arc);
        }
        self.jit_override_count
            .store(state.overrides.len(), Ordering::Release);
        state.artifact = Some(artifact);
        state.compiled = JitCompileState::Done;
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
        let chunk = crate::compile::compile_initializer(
            expr,
            tcx,
            layouts,
            wrappers,
            inline_fns,
            module_consts,
            method_muts,
            mut_statics,
            cov_map.as_deref(),
        )?;
        debug_validate_chunk(&chunk)?;
        // Run with a fresh, local `ChunkState` rather than the
        // address-keyed `chunk_state_for` cache. An initializer chunk is
        // dropped as soon as it returns, so caching its heap address
        // would alias a later real chunk allocated at the same slot.
        let jit_disabled = !jit_call::jit_enabled();
        let state = ChunkState::new(
            chunk.call_cache_count,
            chunk.arith_cache_count,
            chunk.field_cache_count,
            chunk.instrs.len(),
            jit_disabled,
        );
        self.run(&chunk, &state, Vec::new())
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
        let globals = Arc::make_mut(&mut self.globals);
        let module_prefix = if item.module_path.is_empty() {
            None
        } else {
            Some(item.module_path.join("::"))
        };
        let intern = crate::value::intern_type_name;
        match &item.kind {
            HirItemKind::Fn(decl) => {
                let chunk = compile_fn(
                    decl,
                    tcx,
                    layouts,
                    wrappers,
                    inline_fns,
                    module_consts,
                    method_muts,
                    mut_statics,
                    cov_map.as_deref(),
                )?;
                debug_validate_chunk(&chunk)?;
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
                    debug_validate_chunk(&chunk)?;
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
                    globals.insert(intern(&method.name.name), Global::Fn(shared));
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
                        debug_validate_chunk(&chunk)?;
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

/// Builds native shape descriptors for every heap enum whose values
/// can cross the JIT boundary as raw pointers (all variant fields are
/// scalars, strings, or other supported heap enums), registers them in
/// the process-global shape table, and returns `DefId.local -> shape
/// index` for the cranelift eligibility check.
fn build_native_enum_shapes(
    program: &HirProgram,
    tcx: &TyCtxt,
) -> std::collections::HashMap<u32, u32> {
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
    // Fixpoint: drop enums with unsupported fields (or fields of
    // dropped enums) until stable.
    let mut supported: std::collections::HashSet<u32> = in_set.clone();
    loop {
        let mut changed = false;
        for c in &cands {
            if !supported.contains(&c.def_local) {
                continue;
            }
            let ok = c.variants.iter().all(|(_, tys)| {
                tys.iter().all(|t| match tcx.kind_of(*t) {
                    TyKind::Int(_)
                    | TyKind::Float(_)
                    | TyKind::Bool
                    | TyKind::Char
                    | TyKind::String => true,
                    TyKind::Adt { def, .. } => supported.contains(&def.local),
                    _ => false,
                })
            });
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
        return std::collections::HashMap::new();
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
        let shapes: Vec<&'static NativeEnumShape> = kept
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
                                _ => unreachable!("filtered by the supported fixpoint"),
                            })
                            .collect(),
                    })
                    .collect();
                let shape: &'static NativeEnumShape = Box::leak(Box::new(NativeEnumShape {
                    enum_name: intern_type_name(c.name),
                    index: idx_of[&c.def_local],
                    tagged,
                    variants,
                }));
                shape
            })
            .collect();
        (shapes, idx_of)
    })
}
