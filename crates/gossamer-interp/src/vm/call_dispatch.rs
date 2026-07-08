#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl Vm {
    /// Invokes a top-level function by name.
    pub fn call(&self, name: &str, args: Vec<Value>) -> RuntimeResult<Value> {
        let callee = self
            .lookup_global(name)
            .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
        let interned = crate::value::intern_type_name(name);
        self.call_stack.borrow_mut().clear();
        self.call_stack.borrow_mut().push(interned);
        let result = self.apply(callee, args);
        if result.is_ok() {
            self.call_stack.borrow_mut().pop();
        }
        result
    }

    /// Snapshot of the in-flight (or last failing) call stack.
    /// Outermost frame first. Returns owned `String`s for API
    /// stability; underlying storage is `&'static str`.
    #[must_use]
    pub fn call_stack_snapshot(&self) -> Vec<String> {
        self.call_stack
            .borrow()
            .iter()
            .map(|s| (*s).to_string())
            .collect()
    }

    /// Public entry for invoking a function VALUE (not a name): used by
    /// the CLI runner to deliver the main-goroutine panic message to a
    /// `runtime::set_panic_hook` hook.
    pub fn apply_value(&self, callee: Value, args: Vec<Value>) -> RuntimeResult<Value> {
        self.apply(Global::Value(callee), args)
    }

    /// Deliver a panic message to the registered hook, if any. The
    /// stored hook may be a function value or (bytecode tier) the bare
    /// function NAME - resolve through globals in that case. Returns
    /// false when no hook is set or the call failed (caller prints the
    /// default report).
    pub fn invoke_panic_hook(&self, msg: &str) -> bool {
        let Some(hook) = crate::panic_hook_value() else {
            return false;
        };
        let arg = Value::String(SmolStr::from(msg));
        let resolved = match &hook {
            Value::String(name) => match self.lookup_global(name.as_str()) {
                Some(g) => g,
                None => return false,
            },
            other => Global::Value(other.clone()),
        };
        self.apply(resolved, vec![arg]).is_ok()
    }

    pub(crate) fn apply(&self, global: Global, args: Vec<Value>) -> RuntimeResult<Value> {
        match global {
            Global::Fn(chunk) => {
                // Byte-precise native-stack guard, consulted before the frame
                // count. A JIT-compiled body recurses on the real OS stack
                // (not the heap frame pool `MAX_CALL_DEPTH` bounds), so a
                // frame count alone cannot stop it before the guard page; the
                // armed byte budget catches that recursion at the boundary and
                // raises a clean stack-overflow, ending only the current
                // goroutine instead of aborting the whole process. No-op when
                // unarmed.
                if gossamer_coro::stack_guard_tripped() {
                    return Err(RuntimeError::StackOverflow(MAX_CALL_DEPTH));
                }
                // Refuse calls beyond the goroutine call-depth cap. The VM
                // allocates Gossamer frames on the heap (not on the OS stack),
                // so without this check a program like `fn f(n) { f(n+1) }`
                // would spin indefinitely consuming CPU and heap rather than
                // crashing with a clear message.
                let depth = self.call_depth.get();
                if depth >= MAX_CALL_DEPTH {
                    return Err(RuntimeError::StackOverflow(MAX_CALL_DEPTH));
                }
                self.call_depth.set(depth + 1);

                // Push a frame on the call stack so a runtime error
                // mid-body reports the chain. Pop on success only;
                // a propagating error keeps the frame so callers
                // can read it via `call_stack_snapshot`.
                let pushed_frame = self
                    .call_stack
                    .borrow()
                    .last()
                    .is_none_or(|top| *top != chunk.name);
                if pushed_frame {
                    self.call_stack.borrow_mut().push(chunk.name);
                }
                let state = self.chunk_state_for(&chunk);
                // Tier D2 - decrement the per-`Vm` hot counter and
                // trigger a deferred JIT compile when the budget is
                // spent. The counter is per-thread (in `ChunkState`),
                // so each goroutine independently warms up. `main`
                // participates like any body: sret covers every
                // aggregate-return shape, so a hot loop living
                // directly in the implicit top-level `main` promotes.
                // A loop-bearing `main` sits in the eager set (counter
                // starts at 1) and compiles on its single call; a
                // loop-free `main` never trips its size-scaled
                // threshold on one call, keeping short scripts off
                // the Cranelift compile pass entirely.
                let hot = state.hot_counter.get();
                if hot > 0 && hot != crate::bytecode::HOT_DISABLED {
                    state.jit_observed_work.set(
                        state
                            .jit_observed_work
                            .get()
                            .saturating_add(state.instr_count.max(1)),
                    );
                    let next = hot - 1;
                    state.hot_counter.set(next);
                    if next == 0 {
                        if state.jit_observed_work.get() >= state.jit_min_work {
                            self.try_compile_jit_lazy();
                        } else {
                            state.hot_counter.set(1);
                        }
                    }
                }
                // Tier D1 - if the deferred compile produced a
                // native entry for this chunk, route through the
                // trampoline first. The override map is shared
                // across goroutines via `Arc<RwLock<JitState>>`, so
                // a child goroutine that tripped the hot counter
                // installs entries every other thread sees.
                //
                // Fast path: skip the RwLock probe entirely when no
                // overrides are installed. The atomic load is a
                // single ~1 ns instruction; the RwLock read costs
                // ~6-8 ns of CAS that compounds across recursive
                // call chains where every leaf fires through `apply`.
                let jit_opt = if self.jit_override_count.load(Ordering::Acquire) == 0 {
                    None
                } else {
                    // Resolve once per ChunkState, then read a plain
                    // field - no lock, no string hash. Sound because
                    // JIT install is one-shot and the map only shrinks.
                    let mut slot = state.jit_resolve.borrow_mut();
                    if matches!(&*slot, crate::vm::JitResolve::Unresolved) {
                        // `Prepared` carries the non-Sync raw JIT fn ptr
                        // (and a per-entry verification `Cell`); the VM is
                        // single-threaded, so the `Arc` never crosses a
                        // thread. `Arc` (not `Rc`) keeps the resolution
                        // valid even after the override map evicts it.
                        #[allow(
                            clippy::arc_with_non_send_sync,
                            reason = "Prepared carries non-Sync raw fn ptr; per-ChunkState cache stays on one thread"
                        )]
                        let resolved = match self.jit.read().overrides.get(chunk.name).cloned() {
                            Some(j) => match jit_call::prepare(j) {
                                Some(p) => crate::vm::JitResolve::Some(std::sync::Arc::new(p)),
                                None => crate::vm::JitResolve::None,
                            },
                            None => crate::vm::JitResolve::None,
                        };
                        *slot = resolved;
                    }
                    match &*slot {
                        crate::vm::JitResolve::Some(p) => Some(p.clone()),
                        _ => None,
                    }
                };
                if let Some(prepared) = jit_opt {
                    match jit_call::invoke_prepared(&prepared, &args, &self.jit_graph_cache) {
                        jit_call::Dispatch::Ok(value) => {
                            prepared.record_hit();
                            if jit_call::jit_trace() {
                                eprintln!("jit: native hit {}", prepared.jit.name);
                            }
                            self.call_depth.set(self.call_depth.get().saturating_sub(1));
                            if pushed_frame {
                                self.call_stack.borrow_mut().pop();
                            }
                            return Ok(value);
                        }
                        jit_call::Dispatch::Fallback => {
                            if jit_call::jit_trace() {
                                eprintln!("jit: fallback to bytecode for {}", prepared.jit.name);
                            }
                            // A body whose args never marshal (enum values
                            // arriving as bytecode `Value::Variant`) wastes a
                            // marshal attempt on every call. After enough
                            // consecutive misses with no native hit, demote
                            // the ChunkState slot to bytecode-only so the
                            // attempt stops.
                            if prepared.record_fallback_should_demote() {
                                *state.jit_resolve.borrow_mut() = crate::vm::JitResolve::None;
                                if jit_call::jit_trace() {
                                    eprintln!("jit: demote {} (bytecode-only)", prepared.jit.name);
                                }
                            }
                        }
                    }
                }
                let result = self.run(&chunk, state, args);
                // Decrement depth unconditionally on return so mutual
                // recursion and early-return paths both release their slot.
                self.call_depth.set(self.call_depth.get().saturating_sub(1));
                if result.is_ok() && pushed_frame {
                    self.call_stack.borrow_mut().pop();
                }
                result
            }
            Global::Value(value) => match value {
                // Builtins / natives take aggregates by value -
                // unwrap any `&mut` write-back cell so they see the
                // plain aggregate (the unchanged value flows back to
                // the caller through the cell afterwards).
                Value::Builtin(inner) => {
                    let args = crate::value::unwrap_mut_cells(args);
                    (inner.call)(&args)
                }
                Value::Native(inner) => {
                    let args = crate::value::unwrap_mut_cells(args);
                    let mut dispatch = super::native_dispatch::VmDispatch::new(self);
                    (inner.call)(&mut dispatch, &args)
                }
                Value::Closure(closure) => self.invoke_closure(&closure, args),
                Value::Variant(inner) if inner.fields.is_empty() => Ok(Value::variant(
                    inner.name,
                    Arc::unwrap_or_clone(std::sync::Arc::new(args)),
                )),
                _ => Err(RuntimeError::Type(
                    "global is not callable at this call site".to_string(),
                )),
            },
            // A `static mut` holding a callable value - load the current
            // value and dispatch it as a plain value.
            Global::MutStatic(cell) => {
                let value = cell.lock().clone();
                self.apply(Global::Value(value), args)
            }
        }
    }

    /// Enqueues `task` on the process-wide [`crate::vm::goroutine::pool`],
    /// running it against the worker thread's reused `Vm`. The pool
    /// keeps `num_cpus()` worker threads, each owning a thread-local
    /// `Vm` lazily built on first task and reused across every
    /// goroutine that lands on it - chunk caches stay warm, the frame
    /// pool stays populated, and there is no per-spawn `HashMap::clone`
    /// of globals. After `task` returns, the worker `Vm` is trimmed
    /// back toward steady state so bursty workloads do not leave every
    /// worker holding the union of every task's high-water mark.
    ///
    /// Tasks queue if every worker is busy; the spawning thread does
    /// not block. Programs that mix `gos run` invocations within one
    /// process would see stale per-worker state here; the bench-game
    /// shape (one program per process) does not.
    fn spawn_on_pool<F>(&self, task: F)
    where
        F: FnOnce(&mut Vm) + Send + 'static,
    {
        let globals = Arc::clone(&self.globals);
        let mir_bodies = self.mir_bodies.borrow().clone();
        let tcx_snapshot = self.tcx_snapshot.borrow().clone();
        let enum_shape_defs = self.enum_shape_defs.borrow().clone();
        let struct_shape_defs = self.struct_shape_defs.borrow().clone();
        crate::vm::goroutine::pool().spawn(Box::new(move || {
            thread_local! {
                static THREAD_VM: std::cell::OnceCell<std::cell::RefCell<Option<Vm>>> =
                    const { std::cell::OnceCell::new() };
            }
            THREAD_VM.with(|cell| {
                let vm_cell = cell.get_or_init(|| std::cell::RefCell::new(None));
                let mut slot = vm_cell.borrow_mut();
                // The cached `Vm` is only valid for the program whose
                // globals it was built from. A thread can outlive one
                // program (wasm runs every task on the main thread; an
                // embedding may load several programs in one process),
                // so key reuse on the globals `Arc` identity.
                let reusable = slot
                    .as_ref()
                    .is_some_and(|vm| Arc::ptr_eq(&vm.globals, &globals));
                if !reusable {
                    // The worker `Vm` shares the parent's loaded globals
                    // (user fns, consts, statics, ADT ctors) via the
                    // `Arc`, so every callable a Native builtin resolves
                    // off-main is already present.
                    *slot = Some(Vm::with_globals(
                        globals,
                        mir_bodies,
                        tcx_snapshot,
                        enum_shape_defs,
                        struct_shape_defs,
                    ));
                }
                let vm = slot.as_mut().expect("THREAD_VM init");
                task(vm);
                vm.reset_after_task();
            });
        }));
    }

    /// Spawns a goroutine that runs `callee(args)` through the
    /// bytecode VM. A panic in the spawned callee is isolated to its
    /// worker: it is delivered to the panic hook (if any) or reported
    /// on stderr, and never propagates to the spawning thread.
    pub(crate) fn spawn_goroutine_native(&self, callee: Value, args: Vec<Value>) {
        self.spawn_on_pool(move |vm| {
            if let Err(err) = vm.dispatch_call(&callee, args) {
                if !vm.invoke_panic_hook(&crate::panic_message(&err)) {
                    // Report an unobserved goroutine panic with the same single
                    // `error[GX0005]: panic: ...` line the compiled runtime
                    // emits (`gos_rt_panic`), so stderr is identical whether the
                    // goroutine ran on the VM or native code.
                    eprintln!("{err}");
                }
            }
        });
    }

    /// Spawns `callee(args)` through the bytecode VM and returns a
    /// one-shot channel handle that `.join()` blocks on for the
    /// outcome. Backs `spawn(f)`: the outcome rides the channel as the
    /// final `Result<T, String>` variant - `Ok(value)`, or
    /// `Err(message)` carrying the bare panic text, matching the
    /// compiled tier's `gos_rt_join`.
    pub(crate) fn spawn_join_native(
        &self,
        callee: Value,
        args: Vec<Value>,
    ) -> RuntimeResult<Value> {
        let channel = crate::value::Channel::new();
        let worker_channel = channel.clone();
        self.spawn_on_pool(move |vm| {
            let outcome = match vm.dispatch_call(&callee, args) {
                Ok(v) => Value::variant("Ok", vec![v]),
                Err(RuntimeError::Panic(msg)) => {
                    Value::variant("Err", vec![Value::String(msg.into())])
                }
                Err(other) => Value::variant("Err", vec![Value::String(format!("{other}").into())]),
            };
            worker_channel.send(outcome);
        });
        Ok(Value::Channel(channel))
    }

    pub(crate) fn dispatch_call(&self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
        match callee {
            Value::Builtin(inner) => {
                let args = crate::value::unwrap_mut_cells(args);
                (inner.call)(&args)
            }
            Value::String(name) => {
                let entry = self
                    .globals
                    .get(name.as_str())
                    .cloned()
                    .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
                self.apply(entry, args)
            }
            // Closures run natively via their compiled body chunk.
            Value::Closure(closure) => self.invoke_closure(closure, args),
            // Native-dispatch hooks (`&mut self` builtins) re-enter the
            // VM's own call machinery through `VmDispatch`, so the user
            // callables they invoke run on the VM.
            Value::Native(inner) => {
                let args = crate::value::unwrap_mut_cells(args);
                let mut dispatch = super::native_dispatch::VmDispatch::new(self);
                (inner.call)(&mut dispatch, &args)
            }
            // Calling a zero-field variant value acts as that variant's
            // constructor: `Circle(1.5)` produces
            // `Value::variant("Circle", [1.5])`.
            Value::Variant(inner) if inner.fields.is_empty() => Ok(Value::variant(
                inner.name,
                Arc::unwrap_or_clone(std::sync::Arc::new(args)),
            )),
            other => Err(RuntimeError::Type(format!(
                "value of kind `{other}` is not callable"
            ))),
        }
    }

    /// Invokes a [`Value::Closure`] by running its native body chunk
    /// with `capture_values ++ args` in the leading registers. Every
    /// closure the VM builds carries a compiled chunk; a missing one is
    /// an internal lowering invariant violation.
    pub(crate) fn invoke_closure(
        &self,
        closure: &Arc<crate::value::Closure>,
        args: Vec<Value>,
    ) -> RuntimeResult<Value> {
        let chunk = &closure.chunk;
        // The chunk's leading parameters are the captured upvalues, so
        // its declared-parameter count is the arity minus the captures.
        let expected = chunk.arity as usize - closure.capture_values.len();
        if expected != args.len() {
            return Err(RuntimeError::Arity {
                expected,
                found: args.len(),
            });
        }
        let mut full = Vec::with_capacity(closure.capture_values.len() + args.len());
        full.extend(closure.capture_values.iter().cloned());
        full.extend(args);
        self.apply(Global::Fn(Arc::clone(chunk)), full)
    }
}
