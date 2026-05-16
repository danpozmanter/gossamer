#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl Vm {
    /// Builds a VM pre-populated with the built-in intrinsics.
    #[must_use]
    pub fn new() -> Self {
        let mut vm = Self {
            globals: Arc::new(HashMap::new()),
            walker: RefCell::new(Interpreter::new()),
            pool: RefCell::new(FramePool::default()),
            mir_bodies: None,
            tcx_snapshot: None,
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
            chunk_state_arena: RefCell::new(Vec::new()),
            chunk_state_map: RefCell::new(HashMap::new()),
            chunk_state_last: Cell::new(None),
            globals_generation: Cell::new(1),
            call_stack: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
        };
        let globals = Arc::get_mut(&mut vm.globals).expect("fresh Vm globals are uniquely owned");
        for (name, value) in builtins::cached() {
            globals.insert((*name).to_string(), Global::Value(value.clone()));
        }
        for (name, value) in crate::external_natives::external_natives_snapshot() {
            globals.insert(name.to_string(), Global::Value(value));
        }
        vm
    }

    /// Builds a VM from a pre-populated `globals` map. Used by
    /// `Op::Spawn` so a freshly spawned goroutine runs the callee
    /// through the bytecode VM with the parent's `Arc<FnChunk>`
    /// graph shared (chunks are immutable + `Sync`). The child has
    /// its own per-`Vm` cache state and JIT slot — see [`Self::jit`]
    /// for why JIT state can't cross threads.
    #[must_use]
    pub(crate) fn with_globals(
        globals: Arc<HashMap<String, Global>>,
        mir_bodies: Option<Arc<Vec<Body>>>,
        tcx_snapshot: Option<Arc<TyCtxt>>,
    ) -> Self {
        Self {
            globals,
            walker: RefCell::new(Interpreter::new()),
            pool: RefCell::new(FramePool::default()),
            mir_bodies,
            tcx_snapshot,
            jit: parking_lot::RwLock::new(JitState::default()),
            jit_override_count: AtomicUsize::new(0),
            chunk_state_arena: RefCell::new(Vec::new()),
            chunk_state_map: RefCell::new(HashMap::new()),
            chunk_state_last: Cell::new(None),
            globals_generation: Cell::new(1),
            call_stack: RefCell::new(Vec::new()),
            call_depth: Cell::new(0),
        }
    }

    /// Bumps the [`Self::globals_generation`] counter and returns
    /// the new value. Call from any code path that mutates
    /// [`Self::globals`] after `Vm::new` / `Vm::with_globals` have
    /// returned. Inline caches stamped with an older value will be
    /// treated as misses and re-resolved against the new map.
    pub fn bump_globals_generation(&self) -> u32 {
        let next = self.globals_generation.get().wrapping_add(1);
        // Skip 0 on wrap so the empty-slot sentinel stays distinct
        // from a real generation. Wrapping after 4 billion mutations
        // is purely defensive — we never expect to get there in a
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
    /// this — it always uses [`Self::bump_globals_generation`].
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
        self.walker.borrow_mut().reset_after_task();
    }

    /// Frees MIR bodies and the `TyCtxt` snapshot retained for deferred JIT.
    /// After JIT compilation fires (or is skipped), these are never read
    /// again on the main `Vm` — goroutines have already cloned their own Arcs.
    /// Call once after `vm.call()` returns to reclaim the per-program MIR
    /// allocation before the goroutine-join phase.
    pub fn release_jit_prelude(&mut self) {
        self.mir_bodies = None;
        self.tcx_snapshot = None;
    }

    /// Compiles and registers every `fn`/`const`/`static`/impl item in
    /// `program`. Items the VM can't lower yet produce a runtime error.
    /// The bundled tree-walker is loaded with the same program so
    /// `Op::EvalDeferred` can delegate anything the VM compiler
    /// falls back on.
    ///
    /// `tcx` is `&mut` so the JIT prepass can drive
    /// [`gossamer_mir::lower_program`] (which interns inferred types
    /// during lowering); the bytecode compiler still treats it as
    /// read-only.
    pub fn load(&mut self, program: &HirProgram, tcx: &mut TyCtxt) -> RuntimeResult<()> {
        // Phase 1: evaluate consts/statics and register ADT constructors.
        // Skips function body clones — those are only needed when at least
        // one bytecode chunk defers expressions back to the tree-walker.
        self.walker.borrow_mut().load_non_fns(program);
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
                }
                _ => {}
            }
        }
        crate::builtins::set_struct_layouts(name_layouts);
        // (Previously: a per-program JSON struct-schema registry was
        // built here so the VM tier could intercept
        // `<Type>::from_json` calls. 0.7.0 replaces that with
        // compile-time codegen in `gossamer-parse::autoderive`; the
        // synthesized methods are real Gossamer code and need no
        // VM-side bookkeeping.)
        // Snapshot every top-level `const NAME = ...` value the
        // tree-walker has already evaluated. Passed to `compile_fn`
        // so a path that resolves to one of these inlines as a
        // `LoadConst` instead of a string-keyed `LoadGlobal` lookup.
        let mut module_consts: HashMap<String, Value> = HashMap::new();
        {
            let walker = self.walker.borrow();
            for item in &program.items {
                let name = match &item.kind {
                    HirItemKind::Const(decl) => &decl.name.name,
                    // Immutable statics inline as constants. Mutable
                    // statics (`static mut COUNTER: i64 = 0`) are
                    // skipped: their reads must continue to flow
                    // through `LoadGlobal` so writes the tree-walker
                    // performs against the globals table are visible
                    // to subsequent reads. Inlining the initial
                    // value would shadow every store and freeze the
                    // observed value at the declaration site.
                    HirItemKind::Static(decl) if !decl.mutable => &decl.name.name,
                    _ => continue,
                };
                if let Some(value) = walker.lookup_global(name) {
                    module_consts.insert(name.clone(), value);
                }
            }
        }
        for item in &program.items {
            self.load_item(item, tcx, &def_layouts, &wrappers, &module_consts)?;
        }
        // Phase 2: load function closures into the walker. The walker
        // backs every `NativeDispatch::call_fn` / `call_value` callback
        // a Native builtin makes — http::serve dispatching `Router::serve`,
        // iter::for_each invoking a passed-in fn, etc. A bare function
        // value (e.g. `r.get("/", root)`) reaches the walker as
        // `Value::String("root")` and resolves through `globals`, so the
        // user's fn table must live there unconditionally. The cost is a
        // one-time HashMap insert per top-level fn at startup.
        self.walker.borrow_mut().load_fns(program);
        // Tier D2 — deferred JIT. Lower MIR up front so the
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
        if jit_call::jit_enabled() && has_jit_eligible_fn(program) {
            let mut bodies = gossamer_mir::lower_program(program, tcx);
            gossamer_mir::inline_trivial_wrappers(&mut bodies);
            gossamer_mir::inline_small_callees(&mut bodies);
            self.mir_bodies = Some(Arc::new(bodies));
            self.tcx_snapshot = Some(Arc::new(tcx.clone()));
        } else {
            self.jit.write().compiled = JitCompileState::Failed;
        }
        Ok(())
    }

    /// Compiles the saved MIR through cranelift and fills the JIT
    /// override map. Called the first time any chunk's tier-up
    /// counter trips. The state machine on `JitState::compiled`
    /// short-circuits concurrent goroutine trips so `compile_to_jit`
    /// runs at most once per `Arc<RwLock<JitState>>`. Failures
    /// transition to `Failed` and stay there — no observable
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
        let artifact = match gossamer_codegen_cranelift::compile_to_jit(bodies, tcx) {
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
        // compress::gzip::*, bufio::read_lines, etc. — anything
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
            let Some(Global::Fn(chunk)) = self.globals.get(name) else {
                continue;
            };
            // Skip promotion of any chunk that calls `panic`.
            // The cranelift codegen lowers `panic(...)` into a
            // `gos_rt_panic` call that aborts the process directly,
            // bypassing the bytecode VM's tree-walker fallback that
            // captures the call stack for the user-facing
            // diagnostic. Keeping panicking helpers on the
            // bytecode path preserves the call-stack render.
            if chunk.globals.iter().any(|g| g == "panic") {
                continue;
            }
            if trace {
                eprintln!("jit: promote {name}");
            }
            // `JitFn` carries a raw `*const u8` so it isn't
            // `Send + Sync`. The VM is single-threaded today, so
            // an `Arc` is the right shape for the override map's
            // shared ownership semantics — a `Rc` would prevent
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

    pub(crate) fn load_item(
        &mut self,
        item: &HirItem,
        tcx: &TyCtxt,
        layouts: &HashMap<gossamer_resolve::DefId, Vec<String>>,
        wrappers: &HashMap<String, Vec<String>>,
        module_consts: &HashMap<String, Value>,
    ) -> RuntimeResult<()> {
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
        match &item.kind {
            HirItemKind::Fn(decl) => {
                let mut chunk = compile_fn(decl, tcx, layouts, wrappers, module_consts)?;
                chunk.compact();
                debug_validate_chunk(&chunk)?;
                let shared = chunk.into_shared();
                if let Some(prefix) = &module_prefix {
                    let qualified = format!("{prefix}::{}", decl.name.name);
                    globals.insert(qualified, Global::Fn(shared.clone()));
                }
                // Bare-name registration last so it wins over the
                // qualified key on `globals.get(name)` only when the
                // bare name is unique. The qualified key is the
                // canonical lookup for cross-module callers.
                globals.insert(decl.name.name.clone(), Global::Fn(shared));
            }
            HirItemKind::Impl(decl) => {
                for method in &decl.methods {
                    let mut chunk = compile_fn(method, tcx, layouts, wrappers, module_consts)?;
                    chunk.compact();
                    debug_validate_chunk(&chunk)?;
                    let shared = chunk.into_shared();
                    // Register both the short name and the
                    // `TypeName::method` qualified key so runtime
                    // dispatch (`recv.method(...)`) routed through
                    // the tree-walker finds the same chunk the VM
                    // sees under its short name.
                    if let Some(type_name) = &decl.self_name {
                        let qualified = format!("{}::{}", type_name.name, method.name.name);
                        globals.insert(qualified.clone(), Global::Fn(shared.clone()));
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                format!("{prefix}::{qualified}"),
                                Global::Fn(shared.clone()),
                            );
                        }
                    }
                    globals.insert(method.name.name.clone(), Global::Fn(shared));
                }
            }
            HirItemKind::Trait(decl) => {
                for method in &decl.methods {
                    if method.body.is_some() {
                        let mut chunk = compile_fn(method, tcx, layouts, wrappers, module_consts)?;
                        chunk.compact();
                        debug_validate_chunk(&chunk)?;
                        let shared = chunk.into_shared();
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                format!("{prefix}::{}", method.name.name),
                                Global::Fn(shared.clone()),
                            );
                        }
                        globals.insert(method.name.name.clone(), Global::Fn(shared));
                    }
                }
            }
            HirItemKind::Const(decl) => {
                // The bundled tree-walker has already evaluated every
                // top-level `const` initializer in its own globals
                // map. Pull that value over so a bytecode
                // `Op::LoadGlobal` keyed on the const's name finds it
                // here without falling back to the walker.
                if let Some(value) = self.walker.borrow().lookup_global(&decl.name.name) {
                    if let Some(prefix) = &module_prefix {
                        globals.insert(
                            format!("{prefix}::{}", decl.name.name),
                            Global::Value(value.clone()),
                        );
                    }
                    globals.insert(decl.name.name.clone(), Global::Value(value));
                }
            }
            HirItemKind::Static(decl) => {
                if let Some(value) = self.walker.borrow().lookup_global(&decl.name.name) {
                    if let Some(prefix) = &module_prefix {
                        globals.insert(
                            format!("{prefix}::{}", decl.name.name),
                            Global::Value(value.clone()),
                        );
                    }
                    globals.insert(decl.name.name.clone(), Global::Value(value));
                }
            }
            HirItemKind::Adt(decl) => {
                // Register enum variant constructors so user code
                // like `List::Nil` and bare `Cons(v, rest)` resolves
                // through the same dispatch as a builtin call.
                if let gossamer_hir::HirAdtKind::Enum(variants) = &decl.kind {
                    let type_name = decl.name.name.clone();
                    for variant in variants {
                        let variant_name = variant.name.name.clone();
                        let qualified = format!("{type_name}::{variant_name}");
                        let sentinel =
                            Value::variant(variant_name.clone(), crate::value::empty_value_arc());
                        if let Some(prefix) = &module_prefix {
                            globals.insert(
                                format!("{prefix}::{qualified}"),
                                Global::Value(sentinel.clone()),
                            );
                        }
                        globals.insert(variant_name, Global::Value(sentinel.clone()));
                        globals.insert(qualified, Global::Value(sentinel));
                    }
                }
            }
        }
        Ok(())
    }
}
