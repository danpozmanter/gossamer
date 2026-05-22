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

    pub(crate) fn apply(&self, global: Global, args: Vec<Value>) -> RuntimeResult<Value> {
        match global {
            Global::Fn(chunk) => {
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
                // Tier D2 — decrement the per-`Vm` hot counter and
                // trigger a deferred JIT compile when the budget is
                // spent. The counter is per-thread (in `ChunkState`),
                // so each goroutine independently warms up.
                // `main` is always executed on the bytecode path (the
                // JIT compiler skips it); its hot counter is irrelevant.
                // We rely purely on per-function counters so short-lived
                // scripts that never call any function 16+ times skip the
                // Cranelift compile pass entirely — a ~3 MB RSS saving for
                // programs that don't benefit from JIT.
                if chunk.name != "main" {
                    let hot = state.hot_counter.get();
                    if hot > 0 && hot != crate::bytecode::HOT_DISABLED {
                        let next = hot - 1;
                        state.hot_counter.set(next);
                        if next == 0 {
                            self.try_compile_jit_lazy();
                        }
                    }
                }
                // Tier D1 — if the deferred compile produced a
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
                    self.jit.read().overrides.get(chunk.name).cloned()
                };
                if let Some(jit) = jit_opt {
                    match jit_call::invoke(&jit, &args) {
                        jit_call::Dispatch::Ok(value) => {
                            self.call_depth.set(self.call_depth.get().saturating_sub(1));
                            return Ok(value);
                        }
                        jit_call::Dispatch::Fallback => {
                            if jit_call::jit_trace() {
                                eprintln!("jit: fallback to bytecode for {}", jit.name);
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
                Value::Builtin(inner) => (inner.call)(&args),
                Value::Native(inner) => {
                    let mut walker = self.walker.borrow_mut();
                    (inner.call)(&mut *walker, &args)
                }
                Value::Closure(closure) => self
                    .walker
                    .borrow_mut()
                    .invoke_callable_value(Value::Closure(closure), args),
                Value::Variant(inner) if inner.fields.is_empty() => {
                    Ok(Value::variant(inner.name, std::sync::Arc::new(args)))
                }
                _ => Err(RuntimeError::Type(
                    "global is not callable at this call site".to_string(),
                )),
            },
        }
    }

    /// Spawns a goroutine that runs `callee(args)` through the
    /// bytecode VM via the process-wide [`crate::interp::pool`].
    /// The pool keeps `num_cpus()` worker threads, each owning
    /// a thread-local `Vm` reused across many goroutines. This
    /// replaces the prior one-OS-thread-per-`go` shape, which
    /// burned ~140 µs of CPU and ~15 KB of leaked `JoinHandle`
    /// state per goroutine. Tasks queue if every worker is
    /// busy; the spawning thread does not block.
    pub(crate) fn spawn_goroutine_native(&self, callee: Value, args: Vec<Value>) {
        let globals = Arc::clone(&self.globals);
        let mir_bodies = self.mir_bodies.clone();
        let tcx_snapshot = self.tcx_snapshot.clone();
        crate::interp::pool().spawn(Box::new(move || {
            // Per-worker `Vm`, lazily built on first task. Reused
            // across every subsequent goroutine landing on this
            // worker — chunk caches stay warm, frame pool stays
            // populated, no per-spawn `HashMap::clone` of globals.
            // Programs that mix `gos run` invocations within one
            // process would see stale state here; the bench-game
            // shape (one program per process) doesn't.
            thread_local! {
                static THREAD_VM: std::cell::OnceCell<std::cell::RefCell<Option<Vm>>> =
                    const { std::cell::OnceCell::new() };
            }
            THREAD_VM.with(|cell| {
                let vm_cell = cell.get_or_init(|| std::cell::RefCell::new(None));
                let mut slot = vm_cell.borrow_mut();
                if slot.is_none() {
                    *slot = Some(Vm::with_globals(globals, mir_bodies, tcx_snapshot));
                }
                let vm = slot.as_mut().expect("THREAD_VM init");
                if let Err(err) = vm.dispatch_call(&callee, args) {
                    eprintln!("goroutine panic (isolated): {err}");
                }
                // Trim per-task buffers back toward steady-state so
                // bursty goroutine workloads do not leave every worker
                // holding the union of every task's high-water mark.
                vm.reset_after_task();
            });
        }));
    }

    pub(crate) fn dispatch_call(&self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
        match callee {
            Value::Builtin(inner) => (inner.call)(&args),
            Value::String(name) => {
                let entry = self
                    .globals
                    .get(name.as_str())
                    .cloned()
                    .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
                self.apply(entry, args)
            }
            // Any other callable shape (closure, native dispatch
            // with `&mut self` hooks, zero-field-variant
            // constructor) delegates to the bundled tree-walker
            // which already knows how to extend envs, bind
            // params, and evaluate the body.
            Value::Closure(_) | Value::Native(_) | Value::Variant(_) => self
                .walker
                .borrow_mut()
                .invoke_callable_value(callee.clone(), args),
            other => Err(RuntimeError::Type(format!(
                "value of kind `{other}` is not callable"
            ))),
        }
    }
}
