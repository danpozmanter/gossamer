//! Register-based bytecode VM dispatch loop.
//!
//! This crate otherwise forbids `unsafe`. The exception is the
//! inner dispatch loop: register files and const pools are
//! sized at compile time from the `FnChunk`'s `register_count`,
//! `float_count`, `int_count`, and `consts.len()`, so every
//! `get_unchecked` / `get_unchecked_mut` call in this file is
//! covered by the compiler-established bound. Skipping those
//! bounds checks is the difference between ~60-second nbody
//! and "slower than the VM was before typed opcodes landed".
#![allow(unsafe_code)]
#![allow(
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    clippy::too_many_lines,
    clippy::many_single_char_names
)]

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::sync::Arc;

use gossamer_ast::Ident;
use gossamer_codegen_cranelift::{JitArtifact, JitFn};
use gossamer_hir::{HirItem, HirItemKind, HirProgram};
use gossamer_mir::Body;
use gossamer_types::TyCtxt;

use crate::builtins;
use crate::bytecode;
use crate::bytecode::{FnChunk, Op};
use crate::compile::compile_fn;
use crate::interp::Interpreter;
use crate::jit_call;
use crate::value::{MapKey, RuntimeError, RuntimeResult, SmolStr, Value};

/// Linked program: every global the VM needs to execute a call.
///
/// The VM bundles a tree-walker `Interpreter` so that
/// `Op::EvalDeferred` can hand off expression kinds the VM
/// compiler doesn't native-lower yet. The shared interpreter
/// receives the same global table (built-ins + compiled
/// functions) and is consulted only for the delegated
/// expression's subtree — the VM keeps driving everything else.
///
/// The walker sits behind a `RefCell` rather than a `Mutex`:
/// `Vm::run` is the single writer and runs fully on the calling
/// thread, so there's no concurrent-access to guard against. A
/// mutex's per-call atomic swap showed up as the #1 hot spot in
/// tight-loop programs that go through `Op::EvalDeferred`.
pub struct Vm {
    globals: HashMap<String, Global>,
    walker: RefCell<Interpreter>,
    /// Frame pool: reused register-file storage handed out at
    /// `run()` entry and returned on exit. Eliminates the per-
    /// call `Vec<Value>` / `Vec<f64>` / `Vec<i64>` malloc storm
    /// that dominates call-heavy programs (recursive `fib`, the
    /// inner loops of `nbody` / `fasta`). Stack-discipline:
    /// nested calls each pop their own buffers off the free
    /// list and push them back on return.
    pool: RefCell<FramePool>,
    /// Lowered MIR for the program, captured at `load` time so the
    /// deferred Tier-D2 JIT compile can run later through `&self`
    /// without needing to re-lower from HIR. `None` when the JIT
    /// is disabled (`gos run --no-jit` / `GOS_JIT=0`) — we skip the
    /// MIR-lower work entirely in that case.
    mir_bodies: Option<Vec<Body>>,
    /// Snapshot of the type context as it stood when MIR was
    /// lowered. Cranelift's `compile_to_jit` only needs `&TyCtxt`,
    /// so the clone keeps the deferred-compile path off the
    /// caller's `&mut TyCtxt`.
    tcx_snapshot: Option<TyCtxt>,
    /// JIT artifact + override map filled by [`Vm::try_compile_jit_lazy`]
    /// the first time any chunk's hot counter trips. The artifact
    /// owns the `JITModule`'s code pages; the override map gives
    /// `apply` an O(1) lookup from chunk name to JIT entry without
    /// having to swap entries in `globals`.
    jit: RefCell<JitState>,
    /// Process-wide hint that the deferred compile has either
    /// already happened or has been declined (--no-jit,
    /// `compile_to_jit` returned `Err`, …). Once `true`, no future
    /// hot-counter trip retries — so a pathological program
    /// can't burn CPU re-attempting a compile that's known to fail.
    jit_attempted: Cell<bool>,
}

/// Owns the cranelift JIT state once the deferred compile has
/// run. The `artifact` keeps every code page alive; the
/// `overrides` map lets `apply` route a `Global::Fn(chunk)` call
/// through native dispatch by name.
#[derive(Default)]
struct JitState {
    /// Owns the finalised `JITModule`; dropped along with the Vm so
    /// the code pages outlive every reachable `JitFn` handle.
    artifact: Option<JitArtifact>,
    /// Map from chunk name to the JIT entry the deferred compile
    /// produced. Populated together with `artifact`. Skips entries
    /// for `main` (see vm.rs:343 comment) and any function the
    /// cranelift backend rejected.
    overrides: HashMap<String, Arc<JitFn>>,
}

/// Per-VM free list of register-file `Vec`s. Stack-discipline:
/// `take_*` pops a Vec sized to the requested length (or
/// allocates a fresh one when the list is empty); `give_*`
/// pushes it back on return so the next call at this depth
/// reuses the capacity.
#[derive(Default)]
struct FramePool {
    values: Vec<Vec<Value>>,
    floats: Vec<Vec<f64>>,
    ints: Vec<Vec<i64>>,
    /// Pool of `Vec<Value>` reused for `Op::Call` argument
    /// marshaling. Each call grabs one to collect args, hands
    /// it to `apply`, and the callee's `run` returns it to
    /// the pool when the args have been moved into the new
    /// frame's register file.
    args: Vec<Vec<Value>>,
}

impl FramePool {
    fn take_values(&mut self, n: usize) -> Vec<Value> {
        // Fast path: pool hit. We rely on the prior owner's
        // `give_values` to have already cleared the buffer, so
        // the pop is constant-time. `resize` to the requested
        // length re-fills with `Value::Void`.
        let mut v = self.values.pop().unwrap_or_default();
        v.resize(n, Value::Void);
        v
    }
    fn give_values(&mut self, mut v: Vec<Value>) {
        // Drop Arc-payload registers eagerly — otherwise the
        // pool would hold strings, arrays, and structs captive
        // for the lifetime of the VM, defeating ref-count
        // collection. clear() iterates dropping each; for a
        // 32-byte enum that's a tag dispatch + per-variant
        // Arc decrement, fast in the common Void/Int/Float case.
        v.clear();
        self.values.push(v);
    }
    fn take_floats(&mut self, n: usize) -> Vec<f64> {
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
        // `clear()` drops any leftovers (paranoia — `give_args`
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
}

/// RAII guard that lends three register-file `Vec`s out of the
/// pool for the duration of one `run()` call. On `Drop`, the
/// buffers go back to the pool — including on early returns or
/// `?` propagation from inside the dispatch loop. Without this,
/// every `?` in the loop body would have to be hand-rewritten
/// to reunite with the buffers before bubbling out.
struct FrameGuard<'a> {
    pool: &'a RefCell<FramePool>,
    registers: std::mem::ManuallyDrop<Vec<Value>>,
    floats: std::mem::ManuallyDrop<Vec<f64>>,
    ints: std::mem::ManuallyDrop<Vec<i64>>,
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
        // Intentionally non-exhaustive: the FramePool, walker, JIT
        // artifact, and tcx snapshot are gnarly to render and add
        // no debugging signal beyond the global names.
        f.debug_struct("Vm")
            .field("globals", &self.globals.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

/// Entries in the global table. Visible to `bytecode::CacheSlot`
/// so inline-cache slots can hold a resolved dispatch target
/// directly — no downcast on the hit path.
#[derive(Debug, Clone)]
pub(crate) enum Global {
    Fn(Arc<FnChunk>),
    Value(Value),
}

impl Vm {
    /// Builds a VM pre-populated with the built-in intrinsics.
    #[must_use]
    pub fn new() -> Self {
        let mut vm = Self {
            globals: HashMap::new(),
            walker: RefCell::new(Interpreter::new()),
            pool: RefCell::new(FramePool::default()),
            mir_bodies: None,
            tcx_snapshot: None,
            jit: RefCell::new(JitState::default()),
            jit_attempted: Cell::new(false),
        };
        let mut list = Vec::new();
        builtins::install(&mut list);
        for (name, value) in list {
            vm.globals.insert(name.to_string(), Global::Value(value));
        }
        vm
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
        self.walker.borrow_mut().load(program);
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
        for item in &program.items {
            self.load_item(item, tcx, &def_layouts, &wrappers)?;
        }
        // Tier D2 — deferred JIT. Lower MIR up front so the
        // tier-up trigger (in `apply`) can dispatch a compile via
        // `&self`, but don't compile yet: short-running programs
        // (`hello.gos`, REPL one-liners) never trip the per-chunk
        // hot counter and skip the cranelift cost entirely.
        // `--no-jit` / `GOS_JIT=0` skips the MIR lower too.
        if jit_call::jit_enabled() {
            let bodies = gossamer_mir::lower_program(program, tcx);
            self.mir_bodies = Some(bodies);
            self.tcx_snapshot = Some(tcx.clone());
        } else {
            self.jit_attempted.set(true);
        }
        Ok(())
    }

    /// Compiles the saved MIR through cranelift and fills the JIT
    /// override map. Called the first time any chunk's tier-up
    /// counter trips. Subsequent calls are short-circuited by
    /// `jit_attempted`. Failures (codegen rejection, MIR lowering
    /// surprises, …) leave the VM on pure bytecode — no
    /// propagation, no observable behaviour change.
    ///
    /// The eager-promotion path (swapping `Global::Fn` for
    /// `Global::Jit` in `globals`) is gone: we keep `globals`
    /// unchanged and route native dispatch through the override
    /// map in `apply`. That keeps the mutation off `globals` so
    /// the tier-up trigger doesn't need `&mut self`.
    fn try_compile_jit_lazy(&self) {
        if self.jit_attempted.get() {
            return;
        }
        self.jit_attempted.set(true);
        if !jit_call::jit_enabled() {
            return;
        }
        let Some(bodies) = self.mir_bodies.as_ref() else {
            return;
        };
        let Some(tcx) = self.tcx_snapshot.as_ref() else {
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
        let mut state = self.jit.borrow_mut();
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
            #[allow(clippy::arc_with_non_send_sync)]
            let jit_arc = Arc::new(jit_fn.clone());
            state.overrides.insert(name.clone(), jit_arc);
        }
        state.artifact = Some(artifact);
    }

    fn load_item(
        &mut self,
        item: &HirItem,
        tcx: &TyCtxt,
        layouts: &HashMap<gossamer_resolve::DefId, Vec<String>>,
        wrappers: &HashMap<String, Vec<String>>,
    ) -> RuntimeResult<()> {
        match &item.kind {
            HirItemKind::Fn(decl) => {
                let chunk = compile_fn(decl, tcx, layouts, wrappers)?;
                self.globals
                    .insert(decl.name.name.clone(), Global::Fn(chunk.into_shared()));
            }
            HirItemKind::Impl(decl) => {
                for method in &decl.methods {
                    let chunk = compile_fn(method, tcx, layouts, wrappers)?;
                    let shared = chunk.into_shared();
                    // Register both the short name and the
                    // `TypeName::method` qualified key so runtime
                    // dispatch (`recv.method(...)`) routed through
                    // the tree-walker finds the same chunk the VM
                    // sees under its short name.
                    if let Some(type_name) = &decl.self_name {
                        let qualified = format!("{}::{}", type_name.name, method.name.name);
                        self.globals.insert(qualified, Global::Fn(shared.clone()));
                    }
                    self.globals
                        .insert(method.name.name.clone(), Global::Fn(shared));
                }
            }
            HirItemKind::Trait(decl) => {
                for method in &decl.methods {
                    if method.body.is_some() {
                        let chunk = compile_fn(method, tcx, layouts, wrappers)?;
                        self.globals
                            .insert(method.name.name.clone(), Global::Fn(chunk.into_shared()));
                    }
                }
            }
            HirItemKind::Adt(_) | HirItemKind::Const(_) | HirItemKind::Static(_) => {}
        }
        Ok(())
    }

    /// Invokes a top-level function by name.
    pub fn call(&self, name: &str, args: Vec<Value>) -> RuntimeResult<Value> {
        let callee = self
            .globals
            .get(name)
            .cloned()
            .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
        self.apply(callee, args)
    }

    fn apply(&self, global: Global, args: Vec<Value>) -> RuntimeResult<Value> {
        match global {
            Global::Fn(chunk) => {
                // Tier D2 — decrement the per-chunk hot counter
                // and trigger a deferred JIT compile when the
                // budget is spent. A saturated counter (sentinel
                // `HOT_DISABLED`) skips both the decrement and
                // the trigger so non-runnable chunks (extern
                // declarations, etc.) never burn the budget.
                //
                // Special case: `main` is called exactly once for
                // the typical single-fn program (`fasta`, `nbody`,
                // any benchmark with a hot inner loop). The
                // counter would never trip from a single call, so
                // the entire program would run on bytecode. Force
                // tier-up on the first invocation of `main` so
                // JIT compilation kicks in before the inner loop
                // starts spinning.
                if chunk.name.as_str() == "main" {
                    self.try_compile_jit_lazy();
                } else {
                    let hot = chunk.hot_counter.get();
                    if hot > 0 && hot != crate::bytecode::HOT_DISABLED {
                        let next = hot - 1;
                        chunk.hot_counter.set(next);
                        if next == 0 {
                            self.try_compile_jit_lazy();
                        }
                    }
                }
                // Tier D1 — if the deferred compile produced a
                // native entry for this chunk, route through the
                // trampoline first. Any unsupported shape (rare,
                // since the JIT's typing reflects the signature
                // the bytecode chunk also sees) falls through to
                // the bytecode body so the call still completes.
                let jit_opt = self
                    .jit
                    .borrow()
                    .overrides
                    .get(chunk.name.as_str())
                    .cloned();
                if let Some(jit) = jit_opt {
                    match jit_call::invoke(&jit, &args) {
                        jit_call::Dispatch::Ok(value) => return Ok(value),
                        jit_call::Dispatch::Err(err) => return Err(err),
                        jit_call::Dispatch::Fallback => {
                            if jit_call::jit_trace() {
                                eprintln!("jit: fallback to bytecode for {}", jit.name);
                            }
                        }
                    }
                }
                self.run(&chunk, args)
            }
            Global::Value(value) => match value {
                Value::Builtin(inner) => (inner.call)(&args),
                Value::Closure(_) => Err(RuntimeError::Unsupported(
                    "tree-walker closures invoked from the VM",
                )),
                _ => Err(RuntimeError::Type(
                    "global is not callable at this call site".to_string(),
                )),
            },
        }
    }

    fn resolve_global(&self, name: &str) -> RuntimeResult<Value> {
        let entry = self
            .globals
            .get(name)
            .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
        match entry {
            Global::Value(value) => Ok(value.clone()),
            Global::Fn(_) => {
                // Bytecode chunk (possibly with a deferred JIT
                // override) — surface as the function's name
                // string so `dispatch_call` looks the entry up
                // again and routes through `apply`, which checks
                // the JIT override map before falling back to
                // bytecode.
                Ok(Value::String(SmolStr::from(name.to_string())))
            }
        }
    }

    // Cognitive-complexity is intentionally high: this is the
    // single dispatch loop covering every `Op` variant (~80
    // arms today). Splitting into per-op handler fns is the
    // Tier-A3 work in `interp_wow_plan.md` and will land
    // separately. The `items_after_statements` allow covers
    // per-arm `type` and `const` definitions (e.g. `BuiltinFn`
    // in `Op::MethodCall`); hoisting them out of their match
    // arm scope would obscure the dispatch shape.
    #[allow(clippy::cognitive_complexity, clippy::items_after_statements)]
    fn run(&self, chunk: &FnChunk, args: Vec<Value>) -> RuntimeResult<Value> {
        if chunk.arity as usize != args.len() {
            return Err(RuntimeError::Arity {
                expected: chunk.arity as usize,
                found: args.len(),
            });
        }
        // Pool guard: takes the three register-file `Vec`s on
        // entry and returns them on Drop, so `?` and early
        // returns inside the dispatch loop don't leak buffers.
        let mut guard = FrameGuard::take(
            &self.pool,
            chunk.register_count as usize,
            chunk.float_count as usize,
            chunk.int_count as usize,
        );
        let registers = &mut guard.registers;
        let floats = &mut guard.floats;
        let ints = &mut guard.ints;
        // Drain (not consume) so the empty Vec can go back to
        // the pool's `args` free list — most arg Vecs are
        // pool-borrowed in `Op::Call`, and reclaiming them here
        // closes the loop without an extra allocation per call.
        let mut args = args;
        for (i, arg) in args.drain(..).enumerate() {
            registers[i] = arg;
        }
        self.pool.borrow_mut().give_args(args);
        let mut pc: u32 = 0;
        let instrs: &[Op] = &chunk.instrs;
        let instr_count = instrs.len();
        loop {
            // SAFETY: every chunk emitted by `compile.rs` ends
            // with a `Return` / `ReturnUnit`, and every jump /
            // branch target is computed from the same emit-
            // counter that placed the op — so `pc` can never
            // exceed `instr_count` at this point. We keep a
            // `debug_assert!` so a corrupted chunk fails loudly
            // in debug builds, but skip the runtime branch in
            // release. `Op` is `Copy`, so dereferencing gives
            // us a by-value copy of the enum for destructuring
            // without invoking `<Op as Clone>::clone`.
            debug_assert!((pc as usize) < instr_count, "fell off end of bytecode");
            let _ = instr_count;
            let op = unsafe { *instrs.get_unchecked(pc as usize) };
            pc += 1;
            match op {
                Op::LoadConst { dst, idx } => {
                    registers[dst as usize] = chunk.consts[idx as usize].clone();
                }
                Op::LoadGlobal { dst, idx } => {
                    let name = &chunk.globals[idx as usize];
                    let value = match self.globals.get(name) {
                        Some(Global::Value(v)) => v.clone(),
                        Some(Global::Fn(_)) => Value::String(SmolStr::from(name.clone())),
                        None => return Err(RuntimeError::UnresolvedName(name.clone())),
                    };
                    let _ = self.resolve_global(name)?;
                    registers[dst as usize] = value;
                }
                Op::Move { dst, src } => {
                    registers[dst as usize] = registers[src as usize].clone();
                }
                Op::AddInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = chunk.arith_caches.borrow()[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_add(chunk, cache_idx, shape, a, b)?;
                }
                Op::SubInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = chunk.arith_caches.borrow()[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_arith(
                        chunk,
                        cache_idx,
                        shape,
                        a,
                        b,
                        i64::wrapping_sub,
                        |x, y| x - y,
                        "subtraction",
                    )?;
                }
                Op::MulInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = chunk.arith_caches.borrow()[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_arith(
                        chunk,
                        cache_idx,
                        shape,
                        a,
                        b,
                        i64::wrapping_mul,
                        |x, y| x * y,
                        "multiplication",
                    )?;
                }
                Op::DivInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = chunk.arith_caches.borrow()[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_div(chunk, cache_idx, shape, a, b)?;
                }
                Op::RemInt {
                    dst,
                    lhs,
                    rhs,
                    cache_idx,
                } => {
                    let a = &registers[lhs as usize];
                    let b = &registers[rhs as usize];
                    let shape = chunk.arith_caches.borrow()[cache_idx as usize].shape.get();
                    registers[dst as usize] = adaptive_rem(chunk, cache_idx, shape, a, b)?;
                }
                Op::Neg { dst, operand } => {
                    registers[dst as usize] = neg(&registers[operand as usize])?;
                }
                Op::Not { dst, operand } => {
                    registers[dst as usize] = not(&registers[operand as usize])?;
                }
                Op::Eq { dst, lhs, rhs } => {
                    registers[dst as usize] = Value::Bool(values_equal(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                    ));
                }
                Op::Ne { dst, lhs, rhs } => {
                    registers[dst as usize] = Value::Bool(!values_equal(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                    ));
                }
                Op::Lt { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Less,
                        false,
                    )?;
                }
                Op::Le { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Less,
                        true,
                    )?;
                }
                Op::Gt { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Greater,
                        false,
                    )?;
                }
                Op::Ge { dst, lhs, rhs } => {
                    registers[dst as usize] = compare(
                        &registers[lhs as usize],
                        &registers[rhs as usize],
                        std::cmp::Ordering::Greater,
                        true,
                    )?;
                }
                Op::Jump { target } => pc = target,
                Op::BranchIf { cond, target } => {
                    if truthy(&registers[cond as usize])? {
                        pc = target;
                    }
                }
                Op::BranchIfNot { cond, target } => {
                    if !truthy(&registers[cond as usize])? {
                        pc = target;
                    }
                }
                Op::Call {
                    dst,
                    callee,
                    args,
                    argc,
                    cache_idx,
                } => {
                    let argc_usz = argc as usize;
                    let mut arg_values = self.pool.borrow_mut().take_args(argc_usz);
                    for i in 0..argc_usz {
                        arg_values.push(registers[args as usize + i].clone());
                    }
                    let callee_val = &registers[callee as usize];
                    // Inline-cache probe. The slot is keyed by the
                    // *callee* identity (the resolved name for a
                    // `Value::String(SmolStr::from("foo"))` callee). Cache hit
                    // skips the `self.globals.get(name)` HashMap
                    // probe — typically the dominant cost in tight
                    // loops calling small helper functions.
                    let token = call_token(callee_val);
                    let cached: Option<Global> = if token != 0 {
                        let cache = chunk.call_caches.borrow();
                        let slot = &cache[cache_idx as usize];
                        if slot.type_token == token {
                            slot.resolved.clone()
                        } else {
                            None
                        }
                    } else {
                        None
                    };
                    let result = if let Some(g) = cached {
                        self.apply(g, arg_values)?
                    } else if token != 0 {
                        // Miss: do the full dispatch and write back.
                        let resolved_global = match callee_val {
                            Value::String(name) => self.globals.get(name.as_str()).cloned(),
                            _ => None,
                        };
                        if let Some(ref g) = resolved_global {
                            let mut cache = chunk.call_caches.borrow_mut();
                            cache[cache_idx as usize] = fill_cache_slot(token, g);
                        }
                        match resolved_global {
                            Some(g) => self.apply(g, arg_values)?,
                            None => self.dispatch_call(callee_val, arg_values)?,
                        }
                    } else {
                        // Non-cacheable callee shape (Builtin,
                        // Closure, Native, …): straight to the
                        // existing slow-path dispatcher.
                        self.dispatch_call(callee_val, arg_values)?
                    };
                    registers[dst as usize] = result;
                }
                Op::Return { value } => return Ok(registers[value as usize].clone()),
                Op::ReturnUnit => return Ok(Value::Unit),
                Op::MethodCall {
                    dst,
                    receiver,
                    name_idx,
                    args,
                    argc,
                    cache_idx,
                } => {
                    // Inline-cache probe. We key the slot on the
                    // *receiver* type (interned struct-name pointer
                    // or a per-variant constant). Hit returns the
                    // resolved `Global` directly, skipping the
                    // qualified-key build + double `HashMap::get`
                    // lookup chain that dominated fasta's per-byte
                    // `out.write_byte(b)` cost.
                    let name = &chunk.globals[name_idx as usize];
                    let argc_usz = argc as usize;
                    let total = argc_usz + 1;
                    let recv_token = type_token(&registers[receiver as usize]);
                    // Two-tier IC probe. The hottest hit is the
                    // builtin fn-pointer fast path: the slot's
                    // `builtin_fn` is the resolved
                    // `fn(&[Value]) -> RuntimeResult<Value>`,
                    // called directly with no `match Global { … }`
                    // chain. Closures / JIT-promoted bodies fall to
                    // the slower `resolved` field.
                    type BuiltinFn = fn(&[Value]) -> RuntimeResult<Value>;
                    let (cached_builtin, cached_general): (Option<BuiltinFn>, Option<Global>) =
                        if recv_token != 0 {
                            let cache = chunk.call_caches.borrow();
                            let slot = &cache[cache_idx as usize];
                            if slot.type_token == recv_token {
                                (slot.builtin_fn, slot.resolved.clone())
                            } else {
                                (None, None)
                            }
                        } else {
                            (None, None)
                        };
                    let cached = cached_general;

                    // Materialise call args. Stack buffer for argc
                    // ≤ 7 (recv + 7 args fits 8 slots) — fasta's
                    // hot path is argc=1.
                    const SMALL: usize = 8;
                    let result = if total <= SMALL {
                        let mut buf: [Value; SMALL] = [
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                            Value::Void,
                        ];
                        buf[0] = registers[receiver as usize].clone();
                        for i in 0..argc_usz {
                            buf[i + 1] = registers[args as usize + i].clone();
                        }
                        if let Some(call_fn) = cached_builtin {
                            // Hottest hit path: direct fn ptr call,
                            // no enum match.
                            call_fn(&buf[..total])?
                        } else if let Some(g) = cached {
                            // Cached non-builtin (closure / JIT).
                            let v: Vec<Value> = buf[..total].to_vec();
                            self.apply(g, v)?
                        } else {
                            // Miss: full resolution + cache fill.
                            let r = qualified_key(&buf[0], name)
                                .and_then(|qual: &str| self.globals.get(qual).cloned())
                                .or_else(|| self.globals.get(name.as_str()).cloned());
                            if recv_token != 0 {
                                if let Some(ref g) = r {
                                    let mut cache = chunk.call_caches.borrow_mut();
                                    cache[cache_idx as usize] = fill_cache_slot(recv_token, g);
                                }
                            }
                            match r {
                                Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                    (builtin_inner.call)(&buf[..total])?
                                }
                                Some(g) => {
                                    let v: Vec<Value> = buf[..total].to_vec();
                                    self.apply(g, v)?
                                }
                                None => {
                                    return Err(RuntimeError::UnresolvedName(name.clone()));
                                }
                            }
                        }
                    } else {
                        let recv = registers[receiver as usize].clone();
                        let mut call_args: Vec<Value> = Vec::with_capacity(total);
                        call_args.push(recv);
                        for i in 0..argc_usz {
                            call_args.push(registers[args as usize + i].clone());
                        }
                        if let Some(call_fn) = cached_builtin {
                            call_fn(&call_args)?
                        } else if let Some(g) = cached {
                            self.apply(g, call_args)?
                        } else {
                            let r = qualified_key(&call_args[0], name)
                                .and_then(|qual: &str| self.globals.get(qual).cloned())
                                .or_else(|| self.globals.get(name.as_str()).cloned());
                            if recv_token != 0 {
                                if let Some(ref g) = r {
                                    let mut cache = chunk.call_caches.borrow_mut();
                                    cache[cache_idx as usize] = fill_cache_slot(recv_token, g);
                                }
                            }
                            match r {
                                Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                    (builtin_inner.call)(&call_args)?
                                }
                                Some(g) => self.apply(g, call_args)?,
                                None => {
                                    return Err(RuntimeError::UnresolvedName(name.clone()));
                                }
                            }
                        }
                    };
                    registers[dst as usize] = result;
                }
                Op::StreamWriteByte {
                    dst,
                    stream_reg,
                    byte_reg,
                } => {
                    // Super-instruction for `<stream>.write_byte(<b>)`.
                    // Hot path: receiver is a `Value::Struct{name="Stream",
                    // fields=[("fd", Int(fd))]}`, byte is a
                    // `Value::Int`. Inline the same work
                    // `builtins::builtin_stream_write_byte` does
                    // but without going through the
                    // MethodCall + IC + Vec-args path.
                    let recv = &registers[stream_reg as usize];
                    let byte_val = &registers[byte_reg as usize];
                    let stream_match = matches!(
                        recv,
                        Value::Struct(inner) if inner.name == "Stream"
                    );
                    let byte_match = matches!(byte_val, Value::Int(_));
                    if stream_match && byte_match {
                        let fd = match recv {
                            Value::Struct(inner) => {
                                let mut fd = 1i64;
                                for (n, v) in inner.fields.iter() {
                                    if n.name == "fd" {
                                        if let Value::Int(f) = v {
                                            fd = *f;
                                            break;
                                        }
                                    }
                                }
                                fd
                            }
                            _ => 1,
                        };
                        let b = match byte_val {
                            Value::Int(n) => *n,
                            _ => unreachable!(),
                        };
                        crate::builtins::stream_write_one_byte(fd, b);
                        registers[dst as usize] = Value::Unit;
                    } else {
                        // Fallback: full method dispatch through
                        // the regular qualified-key path. Keeps
                        // the op correct for any user-defined
                        // `write_byte` method on a non-Stream
                        // receiver, at the cost of one extra
                        // hash lookup per call (uncached for the
                        // miss case, since this op doesn't carry
                        // an IC slot).
                        let recv_clone = recv.clone();
                        let byte_clone = byte_val.clone();
                        let resolved = match &recv_clone {
                            Value::Struct(_) | Value::Channel(_) => {
                                qualified_key(&recv_clone, "write_byte")
                                    .and_then(|q| self.globals.get(q).cloned())
                            }
                            _ => None,
                        }
                        .or_else(|| self.globals.get("write_byte").cloned());
                        let args = vec![recv_clone, byte_clone];
                        let result = match resolved {
                            Some(Global::Value(Value::Builtin(builtin_inner))) => {
                                (builtin_inner.call)(&args)?
                            }
                            Some(g) => self.apply(g, args)?,
                            None => {
                                return Err(RuntimeError::UnresolvedName("write_byte".to_string()));
                            }
                        };
                        registers[dst as usize] = result;
                    }
                }
                Op::MapInc {
                    dst,
                    map_reg,
                    key_reg,
                    by_reg,
                } => {
                    // Fused `m.insert(k, m.get_or(k, 0) + by)`. The
                    // compiler only emits this op for receivers
                    // statically typed `HashMap`, so the fast arm
                    // is the only one that runs in practice. The
                    // generic arm handles polymorphic-by-promotion
                    // value shapes (i.e. a slot already holding
                    // something other than `Value::Int`) by going
                    // through the normal `bin_arith` path.
                    let result = if let Value::Map(map) = &registers[map_reg as usize] {
                        let key = MapKey::from_value(&registers[key_reg as usize]);
                        let by_val = &registers[by_reg as usize];
                        let mut guard = map.lock();
                        let entry = guard.entry(key).or_insert(Value::Int(0));
                        match (&*entry, by_val) {
                            (Value::Int(cur), Value::Int(b)) => {
                                *entry = Value::Int(*cur + *b);
                            }
                            _ => {
                                let cur = entry.clone();
                                let sum = bin_arith(
                                    &cur,
                                    by_val,
                                    i64::wrapping_add,
                                    |a, b| a + b,
                                    "+",
                                )?;
                                *entry = sum;
                            }
                        }
                        let cloned = Arc::clone(map);
                        drop(guard);
                        Value::Map(cloned)
                    } else {
                        // Receiver isn't a Map (shouldn't happen for
                        // a HashMap-typed receiver, but stay total).
                        registers[map_reg as usize].clone()
                    };
                    registers[dst as usize] = result;
                }
                Op::IndexGet { dst, base, index } => {
                    let b = &registers[base as usize];
                    let i = &registers[index as usize];
                    registers[dst as usize] = index_get(b, i)?;
                }
                Op::IndexSet { base, index, value } => {
                    let new_value = registers[value as usize].clone();
                    let i = &registers[index as usize];
                    let idx = match i {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = &mut registers[base as usize];
                    match b {
                        Value::Array(items) | Value::Tuple(items) => {
                            let v = Arc::make_mut(items);
                            if idx >= v.len() {
                                return Err(RuntimeError::Arithmetic(
                                    "index out of bounds".to_string(),
                                ));
                            }
                            v[idx] = new_value;
                        }
                        Value::IntArray(data) => {
                            // Mutate the underlying `Vec<i64>` in place.
                            // `Arc::make_mut` clones if shared (rare —
                            // a fresh `BuildIntArray` usually leaves
                            // the array uniquely owned).
                            let v = Arc::make_mut(data);
                            if idx >= v.len() {
                                return Err(RuntimeError::Arithmetic(
                                    "index out of bounds".to_string(),
                                ));
                            }
                            match new_value {
                                Value::Int(n) => v[idx] = n,
                                _ => {
                                    return Err(RuntimeError::Type(
                                        "IndexSet on IntArray expects i64 value".to_string(),
                                    ));
                                }
                            }
                        }
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        }
                    }
                }
                Op::FieldGet {
                    dst,
                    receiver,
                    name_idx,
                } => {
                    let field_name = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "FieldGet: name must be string const".to_string(),
                            ));
                        }
                    };
                    let recv = &registers[receiver as usize];
                    let v = field_get(recv, field_name.as_str())?;
                    registers[dst as usize] = v;
                }
                Op::FieldSet {
                    receiver,
                    name_idx,
                    value,
                } => {
                    let field_name = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "FieldSet: name must be string const".to_string(),
                            ));
                        }
                    };
                    let new_value = registers[value as usize].clone();
                    let recv = &mut registers[receiver as usize];
                    field_set(recv, field_name.as_str(), new_value)?;
                }
                Op::TupleIndex {
                    dst,
                    receiver,
                    index,
                } => {
                    let recv = &registers[receiver as usize];
                    let idx = index as usize;
                    registers[dst as usize] = match recv {
                        Value::Tuple(items) | Value::Array(items) => {
                            items.get(idx).cloned().ok_or_else(|| {
                                RuntimeError::Arithmetic("tuple index out of bounds".to_string())
                            })?
                        }
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{recv}` has no tuple fields"
                            )));
                        }
                    };
                }
                Op::IndexedFieldSet {
                    base,
                    index,
                    name_idx,
                    value,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let field_name_arc = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "IndexedFieldSet: name must be string const".to_string(),
                            ));
                        }
                    };
                    let field_name: &str = &field_name_arc;
                    let new_value = registers[value as usize].clone();
                    let b = &mut registers[base as usize];
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slots = Arc::make_mut(items);
                    let slot = slots.get_mut(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = Arc::make_mut(&mut struct_inner.fields);
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| ident.name == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        field_slots.push((Ident::new(field_name), new_value));
                    }
                }
                Op::EvalDeferred { dst, expr_idx } => {
                    let idx = expr_idx as usize;
                    let expr = &chunk.deferred_exprs[idx];
                    let names = &chunk.deferred_envs[idx];
                    let regs = &chunk.deferred_env_regs[idx];
                    let mut env_values: Vec<(String, Value)> = Vec::with_capacity(regs.len());
                    for (i, reg) in regs.iter().enumerate() {
                        let value = registers[*reg as usize].clone();
                        let name = names.get(i).cloned().unwrap_or_default();
                        env_values.push((name, value));
                    }
                    let (result, updated) = self
                        .walker
                        .borrow_mut()
                        .eval_standalone(expr, &env_values)?;
                    // Sync mutations back into the original
                    // register slots so `bodies[i].vx = x` in a
                    // delegated expression persists across the
                    // rest of the VM's execution.
                    for (reg, value) in regs.iter().zip(updated) {
                        registers[*reg as usize] = value;
                    }
                    registers[dst as usize] = result;
                }

                // ----- Phase 1 typed ops -----
                //
                // All float/int register accesses use
                // `get_unchecked` — the register slot index is
                // always less than `chunk.float_count` /
                // `chunk.int_count` by construction of the
                // bytecode (the compiler emits a fresh index for
                // every destination and carries it through
                // compile_expr_ex).
                Op::LoadConstF64 { dst_f, idx } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        *chunk.f64_consts.get_unchecked(idx as usize);
                },
                Op::AddF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        + *floats.get_unchecked(rhs_f as usize);
                },
                Op::SubF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        - *floats.get_unchecked(rhs_f as usize);
                },
                Op::MulF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        * *floats.get_unchecked(rhs_f as usize);
                },
                Op::DivF64 {
                    dst_f,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) = *floats
                        .get_unchecked(lhs_f as usize)
                        / *floats.get_unchecked(rhs_f as usize);
                },
                Op::NegF64 { dst_f, src_f } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        -*floats.get_unchecked(src_f as usize);
                },
                Op::LtF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            < *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::LeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            <= *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::GtF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            > *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::GeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            >= *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::EqF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            == *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::NeF64 {
                    dst_v,
                    lhs_f,
                    rhs_f,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *floats.get_unchecked(lhs_f as usize)
                            != *floats.get_unchecked(rhs_f as usize),
                    );
                },
                Op::UnboxF64 { dst_f, src_v } => {
                    let v = &registers[src_v as usize];
                    let f = match v {
                        Value::Float(f) => *f,
                        Value::Int(n) => *n as f64,
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "expected f64 at register, got `{v}`"
                            )));
                        }
                    };
                    floats[dst_f as usize] = f;
                }
                Op::BoxF64 { dst_v, src_f } => {
                    registers[dst_v as usize] = Value::Float(floats[src_f as usize]);
                }
                Op::SqrtF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].sqrt();
                }
                Op::SinF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].sin();
                }
                Op::CosF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].cos();
                }
                Op::AbsF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].abs();
                }
                Op::FloorF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].floor();
                }
                Op::CeilF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].ceil();
                }
                Op::ExpF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].exp();
                }
                Op::LnF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize].ln();
                }
                Op::MulAddF64 {
                    dst_f,
                    a_f,
                    b_f,
                    c_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        floats.get_unchecked(a_f as usize).mul_add(
                            *floats.get_unchecked(b_f as usize),
                            *floats.get_unchecked(c_f as usize),
                        );
                },
                Op::MulSubF64 {
                    dst_f,
                    a_f,
                    b_f,
                    c_f,
                } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        floats.get_unchecked(a_f as usize).mul_add(
                            -*floats.get_unchecked(b_f as usize),
                            *floats.get_unchecked(c_f as usize),
                        );
                },

                Op::LoadConstI64 { dst_i, idx } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        *chunk.i64_consts.get_unchecked(idx as usize);
                },
                Op::AddI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_add(*ints.get_unchecked(rhs_i as usize));
                },
                Op::SubI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_sub(*ints.get_unchecked(rhs_i as usize));
                },
                Op::MulI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) = ints
                        .get_unchecked(lhs_i as usize)
                        .wrapping_mul(*ints.get_unchecked(rhs_i as usize));
                },
                Op::DivI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => {
                    let r = ints[rhs_i as usize];
                    if r == 0 {
                        return Err(RuntimeError::Arithmetic("division by zero".to_string()));
                    }
                    ints[dst_i as usize] = ints[lhs_i as usize].wrapping_div(r);
                }
                Op::RemI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => {
                    let r = ints[rhs_i as usize];
                    if r == 0 {
                        return Err(RuntimeError::Arithmetic("remainder by zero".to_string()));
                    }
                    ints[dst_i as usize] = ints[lhs_i as usize].wrapping_rem(r);
                }
                Op::NegI64 { dst_i, src_i } => {
                    ints[dst_i as usize] = ints[src_i as usize].wrapping_neg();
                }
                Op::BitAndI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) & ints.get_unchecked(rhs_i as usize);
                },
                Op::BitOrI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) | ints.get_unchecked(rhs_i as usize);
                },
                Op::BitXorI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize) ^ ints.get_unchecked(rhs_i as usize);
                },
                Op::ShlI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    let shift = (*ints.get_unchecked(rhs_i as usize) & 63) as u32;
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize).wrapping_shl(shift);
                },
                Op::ShrI64 {
                    dst_i,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    let shift = (*ints.get_unchecked(rhs_i as usize) & 63) as u32;
                    *ints.get_unchecked_mut(dst_i as usize) =
                        ints.get_unchecked(lhs_i as usize).wrapping_shr(shift);
                },
                Op::LtI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) < *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::LeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) <= *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::GtI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) > *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::GeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) >= *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::EqI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) == *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::NeI64 {
                    dst_v,
                    lhs_i,
                    rhs_i,
                } => unsafe {
                    *registers.get_unchecked_mut(dst_v as usize) = Value::Bool(
                        *ints.get_unchecked(lhs_i as usize) != *ints.get_unchecked(rhs_i as usize),
                    );
                },
                Op::UnboxI64 { dst_i, src_v } => {
                    let v = &registers[src_v as usize];
                    let n = match v {
                        Value::Int(n) => *n,
                        _ => {
                            return Err(RuntimeError::Type(format!(
                                "expected i64 at register, got `{v}`"
                            )));
                        }
                    };
                    ints[dst_i as usize] = n;
                }
                Op::BoxI64 { dst_v, src_i } => {
                    registers[dst_v as usize] = Value::Int(ints[src_i as usize]);
                }
                Op::MoveF64 { dst_f, src_f } => {
                    floats[dst_f as usize] = floats[src_f as usize];
                }
                Op::MoveI64 { dst_i, src_i } => {
                    ints[dst_i as usize] = ints[src_i as usize];
                }

                // ----- Phase 2 fused / typed field access -----
                Op::FieldGetF64 {
                    dst_f,
                    receiver,
                    name_idx,
                } => {
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "FieldGetF64: name must be string const".to_string(),
                        ));
                    };
                    let recv = &registers[receiver as usize];
                    let Value::Struct(struct_inner) = recv else {
                        return Err(RuntimeError::Type(format!(
                            "field access on non-struct `{recv}`"
                        )));
                    };
                    let mut val = 0.0f64;
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
                            val = match v {
                                Value::Float(f) => *f,
                                Value::Int(n) => *n as f64,
                                _ => 0.0,
                            };
                            break;
                        }
                    }
                    floats[dst_f as usize] = val;
                }
                Op::IndexedFieldGet {
                    dst,
                    base,
                    index,
                    name_idx,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "IndexedFieldGet: name must be string const".to_string(),
                        ));
                    };
                    let b = &registers[base as usize];
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slot = items.get(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_inner) = slot else {
                        return Err(RuntimeError::Type(
                            "value at index is not a struct".to_string(),
                        ));
                    };
                    let mut found = None;
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
                            found = Some(v);
                            break;
                        }
                    }
                    registers[dst as usize] = found.cloned().unwrap_or(Value::Unit);
                }
                Op::IndexedFieldGetF64 {
                    dst_f,
                    base,
                    index,
                    name_idx,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let Value::String(field_name) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "IndexedFieldGetF64: name must be string const".to_string(),
                        ));
                    };
                    let b = &registers[base as usize];
                    // FloatArray fast path — resolve the field
                    // name against the stored declaration order
                    // and pull the f64 directly out of flat data.
                    if let Value::FloatArray(fa_inner) = b {
                        let off = fa_inner
                            .field_names
                            .iter()
                            .position(|n| n.as_str() == field_name.as_str())
                            .unwrap_or(0);
                        let stride = fa_inner.stride as usize;
                        let pos = idx * stride + off;
                        floats[dst_f as usize] = *fa_inner.data.get(pos).unwrap_or(&0.0);
                        continue;
                    }
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slot = items.get(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_inner) = slot else {
                        return Err(RuntimeError::Type(
                            "value at index is not a struct".to_string(),
                        ));
                    };
                    let mut val = 0.0f64;
                    for (ident, v) in struct_inner.fields.iter() {
                        if ident.name == field_name.as_str() {
                            val = match v {
                                Value::Float(f) => *f,
                                Value::Int(n) => *n as f64,
                                _ => 0.0,
                            };
                            break;
                        }
                    }
                    floats[dst_f as usize] = val;
                }
                Op::IndexedFieldSetF64 {
                    base,
                    index,
                    name_idx,
                    value_f,
                } => {
                    let idx = match &registers[index as usize] {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let field_name_arc = match &chunk.consts[name_idx as usize] {
                        Value::String(s) => s.clone(),
                        _ => {
                            return Err(RuntimeError::Panic(
                                "IndexedFieldSetF64: name must be string const".to_string(),
                            ));
                        }
                    };
                    let field_name = field_name_arc.as_str();
                    let new_value = Value::Float(floats[value_f as usize]);
                    let b = &mut registers[base as usize];
                    let (Value::Array(items) | Value::Tuple(items)) = b else {
                        return Err(RuntimeError::Type(format!(
                            "value of kind `{b}` is not indexable"
                        )));
                    };
                    let slots = Arc::make_mut(items);
                    let slot = slots.get_mut(idx).ok_or_else(|| {
                        RuntimeError::Arithmetic("index out of bounds".to_string())
                    })?;
                    let Value::Struct(struct_arc) = slot else {
                        return Err(RuntimeError::Type(format!(
                            "cannot assign to field `{field_name}` on non-struct"
                        )));
                    };
                    let struct_inner = Arc::make_mut(struct_arc);
                    let field_slots = Arc::make_mut(&mut struct_inner.fields);
                    let pos = field_slots
                        .iter()
                        .position(|(ident, _)| ident.name == field_name);
                    if let Some(p) = pos {
                        field_slots[p].1 = new_value;
                    } else {
                        field_slots.push((Ident::new(field_name), new_value));
                    }
                }

                // ----- Phase 2 offset-resolved ops -----
                Op::IndexedFieldGetF64ByOffset {
                    dst_f,
                    base,
                    index,
                    offset,
                } => {
                    // SAFETY: `index`, `base`, `dst_f` are
                    // compile-time allocated register slots,
                    // so the indexed accesses into `registers`
                    // and `floats` are always in bounds.
                    let idx = match unsafe { registers.get_unchecked(index as usize) } {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = unsafe { registers.get_unchecked(base as usize) };
                    // Flat-f64 fast path: direct f64 load out
                    // of the flat data buffer, no `Value`
                    // discriminant, no `Arc::clone`.
                    if let Value::FloatArray(fa_inner) = b {
                        let pos = idx * (fa_inner.stride as usize) + offset as usize;
                        // SAFETY: the FloatArray was built with
                        // `data.len() == stride * elem_count`, and
                        // `offset < stride` by construction
                        // (compile-time checked). `idx` is the
                        // caller's responsibility; we bounds-check
                        // it once here.
                        if pos >= fa_inner.data.len() {
                            return Err(RuntimeError::Arithmetic(
                                "index out of bounds".to_string(),
                            ));
                        }
                        let f = unsafe { *fa_inner.data.get_unchecked(pos) };
                        unsafe {
                            *floats.get_unchecked_mut(dst_f as usize) = f;
                        }
                    } else {
                        let (Value::Array(items) | Value::Tuple(items)) = b else {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        };
                        let slot = items.get(idx).ok_or_else(|| {
                            RuntimeError::Arithmetic("index out of bounds".to_string())
                        })?;
                        let Value::Struct(struct_inner) = slot else {
                            return Err(RuntimeError::Type(
                                "value at index is not a struct".to_string(),
                            ));
                        };
                        let f = match struct_inner.fields.get(offset as usize).map(|(_, v)| v) {
                            Some(Value::Float(f)) => *f,
                            Some(Value::Int(n)) => *n as f64,
                            _ => 0.0,
                        };
                        floats[dst_f as usize] = f;
                    }
                }
                Op::IndexedFieldSetF64ByOffset {
                    base,
                    index,
                    offset,
                    value_f,
                } => {
                    let idx = match unsafe { registers.get_unchecked(index as usize) } {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    // SAFETY: `value_f` and `base` are
                    // compile-allocated register slots.
                    let new_f = unsafe { *floats.get_unchecked(value_f as usize) };
                    let b = unsafe { registers.get_unchecked_mut(base as usize) };
                    // Flat-f64 fast path: one `Arc::make_mut`
                    // plus a direct memory store. The common
                    // case is a refcount-1 Arc, so `make_mut`
                    // returns the inner mut ref without cloning
                    // — still one acquire-load per write, but no
                    // struct clone and no field scan.
                    if let Value::FloatArray(fa_arc) = b {
                        let fa_inner = Arc::make_mut(fa_arc);
                        let stride = fa_inner.stride as usize;
                        let pos = idx * stride + offset as usize;
                        let buf = Arc::make_mut(&mut fa_inner.data);
                        // SAFETY: `pos < stride * elem_count == buf.len()`
                        // when `idx < elem_count`; we verify that.
                        if pos < buf.len() {
                            unsafe {
                                *buf.get_unchecked_mut(pos) = new_f;
                            }
                        }
                    } else {
                        let new_value = Value::Float(new_f);
                        let (Value::Array(items) | Value::Tuple(items)) = b else {
                            return Err(RuntimeError::Type(format!(
                                "value of kind `{b}` is not indexable"
                            )));
                        };
                        let slots = Arc::make_mut(items);
                        let slot = slots.get_mut(idx).ok_or_else(|| {
                            RuntimeError::Arithmetic("index out of bounds".to_string())
                        })?;
                        let Value::Struct(struct_arc) = slot else {
                            return Err(RuntimeError::Type(
                                "cannot assign to field on non-struct".to_string(),
                            ));
                        };
                        let struct_inner = Arc::make_mut(struct_arc);
                        let field_slots = Arc::make_mut(&mut struct_inner.fields);
                        if let Some(entry) = field_slots.get_mut(offset as usize) {
                            entry.1 = new_value;
                        }
                    }
                }
                Op::BranchIfLtI64 {
                    lhs_i,
                    rhs_i,
                    target,
                } => unsafe {
                    if *ints.get_unchecked(lhs_i as usize) < *ints.get_unchecked(rhs_i as usize) {
                        pc = target;
                    }
                },
                Op::BranchIfGeI64 {
                    lhs_i,
                    rhs_i,
                    target,
                } => unsafe {
                    if *ints.get_unchecked(lhs_i as usize) >= *ints.get_unchecked(rhs_i as usize) {
                        pc = target;
                    }
                },
                Op::BranchIfLtF64 {
                    lhs_f,
                    rhs_f,
                    target,
                } => unsafe {
                    if *floats.get_unchecked(lhs_f as usize) < *floats.get_unchecked(rhs_f as usize)
                    {
                        pc = target;
                    }
                },
                Op::BranchIfGeF64 {
                    lhs_f,
                    rhs_f,
                    target,
                } => unsafe {
                    if *floats.get_unchecked(lhs_f as usize)
                        >= *floats.get_unchecked(rhs_f as usize)
                    {
                        pc = target;
                    }
                },

                Op::FieldGetF64ByOffset {
                    dst_f,
                    receiver,
                    offset,
                } => {
                    let recv = &registers[receiver as usize];
                    let Value::Struct(struct_inner) = recv else {
                        return Err(RuntimeError::Type(format!(
                            "field access on non-struct `{recv}`"
                        )));
                    };
                    let f = match struct_inner.fields.get(offset as usize).map(|(_, v)| v) {
                        Some(Value::Float(f)) => *f,
                        Some(Value::Int(n)) => *n as f64,
                        _ => 0.0,
                    };
                    floats[dst_f as usize] = f;
                }
                Op::FlatGetF64 {
                    dst_f,
                    base,
                    index,
                    stride,
                    offset,
                } => unsafe {
                    let idx = match registers.get_unchecked(index as usize) {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let b = registers.get_unchecked(base as usize);
                    // Compiler-proven FloatArray: skip discriminant match.
                    let Value::FloatArray(fa_inner) = b else {
                        return Err(RuntimeError::Type(
                            "FlatGetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let pos = idx * stride as usize + offset as usize;
                    if pos >= fa_inner.data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *floats.get_unchecked_mut(dst_f as usize) = *fa_inner.data.get_unchecked(pos);
                },
                Op::FlatSetF64 {
                    base,
                    index,
                    stride,
                    offset,
                    value_f,
                } => unsafe {
                    let idx = match registers.get_unchecked(index as usize) {
                        Value::Int(n) if *n >= 0 => *n as usize,
                        Value::Int(_) => {
                            return Err(RuntimeError::Arithmetic(
                                "negative index into sequence".to_string(),
                            ));
                        }
                        _ => {
                            return Err(RuntimeError::Type("index must be integer".to_string()));
                        }
                    };
                    let new_f = *floats.get_unchecked(value_f as usize);
                    let b = registers.get_unchecked_mut(base as usize);
                    let Value::FloatArray(fa_arc) = b else {
                        return Err(RuntimeError::Type(
                            "FlatSetF64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    let fa_inner = Arc::make_mut(fa_arc);
                    let pos = idx * stride as usize + offset as usize;
                    let buf = Arc::make_mut(&mut fa_inner.data);
                    if pos < buf.len() {
                        *buf.get_unchecked_mut(pos) = new_f;
                    }
                },

                Op::BuildFloatArray {
                    dst_v,
                    name_idx,
                    fields_idx,
                    stride,
                    elem_count,
                    first_f,
                } => {
                    let Value::String(name_arc) = &chunk.consts[name_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "BuildFloatArray: name must be string const".to_string(),
                        ));
                    };
                    let name = name_arc.as_str().to_string();
                    let Value::Array(field_names_arr) = &chunk.consts[fields_idx as usize] else {
                        return Err(RuntimeError::Panic(
                            "BuildFloatArray: fields must be array of strings".to_string(),
                        ));
                    };
                    let field_names: Vec<String> = field_names_arr
                        .iter()
                        .filter_map(|v| match v {
                            Value::String(s) => Some(s.as_str().to_string()),
                            _ => None,
                        })
                        .collect();
                    let total = stride as usize * elem_count as usize;
                    let start = first_f as usize;
                    let end = start + total;
                    let data: Vec<f64> = floats[start..end].to_vec();
                    registers[dst_v as usize] =
                        Value::float_array(name, stride, Arc::new(field_names), Arc::new(data));
                }
                Op::BuildIntArray {
                    dst_v,
                    first_i,
                    count,
                } => {
                    let start = first_i as usize;
                    let end = start + count as usize;
                    let data: Vec<i64> = ints[start..end].to_vec();
                    registers[dst_v as usize] = Value::IntArray(Arc::new(data));
                }
                Op::IntToFloatF64 { dst_f, src_i } => unsafe {
                    *floats.get_unchecked_mut(dst_f as usize) =
                        *ints.get_unchecked(src_i as usize) as f64;
                },
                Op::FloatToIntI64 { dst_i, src_f } => unsafe {
                    *ints.get_unchecked_mut(dst_i as usize) =
                        *floats.get_unchecked(src_f as usize) as i64;
                },
                Op::BuildTuple { dst, first, count } => {
                    // Native counterpart to the deferred-walker
                    // path. Clones each value register into a
                    // fresh `Vec<Value>`, wraps in Arc, drops
                    // into `Value::Tuple`. No env reconstruction,
                    // no walker re-entry.
                    let n = count as usize;
                    let start = first as usize;
                    let mut items: Vec<Value> = Vec::with_capacity(n);
                    for i in 0..n {
                        items.push(registers[start + i].clone());
                    }
                    registers[dst as usize] = Value::Tuple(Arc::new(items));
                }
                Op::IntArrayGetI64 {
                    dst_i,
                    base,
                    index_i,
                } => unsafe {
                    let idx = *ints.get_unchecked(index_i as usize);
                    if idx < 0 {
                        return Err(RuntimeError::Arithmetic(
                            "negative index into sequence".to_string(),
                        ));
                    }
                    let i = idx as usize;
                    let b = registers.get_unchecked(base as usize);
                    let Value::IntArray(data) = b else {
                        return Err(RuntimeError::Type(
                            "IntArrayGetI64: receiver lost flat invariant".to_string(),
                        ));
                    };
                    if i >= data.len() {
                        return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
                    }
                    *ints.get_unchecked_mut(dst_i as usize) = *data.get_unchecked(i);
                },
            }
        }
    }

    fn dispatch_call(&self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
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

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
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

/// Native indexed read: `base[i]`. Matches the tree-walker's
/// `eval_index` shape so both code paths produce the same
/// value for every legal `(base, i)` pair.
fn index_get(base: &Value, idx: &Value) -> RuntimeResult<Value> {
    let i = match idx {
        Value::Int(n) => *n,
        _ => return Err(RuntimeError::Type("index must be integer".to_string())),
    };
    if i < 0 {
        return Err(RuntimeError::Arithmetic(
            "negative index into sequence".to_string(),
        ));
    }
    let i = i as usize;
    match base {
        Value::Array(items) | Value::Tuple(items) => items
            .get(i)
            .cloned()
            .ok_or_else(|| RuntimeError::Arithmetic("index out of bounds".to_string())),
        // Rehydrate a single element into `Value::Struct` so
        // generic indexed-access code keeps working when the
        // array was compiled to flat f64 storage.
        Value::FloatArray(fa_inner) => {
            let stride = fa_inner.stride as usize;
            let base_idx = i * stride;
            if base_idx + stride > fa_inner.data.len() {
                return Err(RuntimeError::Arithmetic("index out of bounds".to_string()));
            }
            let mut fields: Vec<(Ident, Value)> = Vec::with_capacity(fa_inner.field_names.len());
            for (j, fname) in fa_inner.field_names.iter().enumerate() {
                fields.push((
                    Ident::new(fname.as_str()),
                    Value::Float(fa_inner.data[base_idx + j]),
                ));
            }
            Ok(Value::struct_(fa_inner.name.clone(), Arc::new(fields)))
        }
        Value::String(s) => s
            .as_bytes()
            .get(i)
            .map(|b| Value::Int(i64::from(*b)))
            .ok_or_else(|| RuntimeError::Arithmetic("index out of bounds".to_string())),
        Value::IntArray(data) => data
            .get(i)
            .copied()
            .map(Value::Int)
            .ok_or_else(|| RuntimeError::Arithmetic("index out of bounds".to_string())),
        _ => Err(RuntimeError::Type(format!(
            "value of kind `{base}` is not indexable"
        ))),
    }
}

/// Builds the `TypeName::method` global-table key for a
/// nominal receiver, mirroring the walker's
/// `qualified_method_key`. Used as the fallback when the bare
/// method-name lookup misses.
fn qualified_key(receiver: &Value, method: &str) -> Option<&'static str> {
    match receiver {
        Value::Struct(inner) => Some(intern_qualified(&inner.name, method)),
        Value::Channel(_) => Some(intern_qualified("Channel", method)),
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
            let interned = intern_type_name(&inner.name);
            TAG_STRUCT | (interned.as_ptr() as u64 & 0x00FF_FFFF_FFFF_FFFF)
        }
        Value::Channel(_) => TAG_CHANNEL,
        Value::String(_) => TAG_STRING,
        Value::Array(_) | Value::FloatArray(_) => TAG_ARRAY,
        Value::Tuple(_) => TAG_TUPLE,
        Value::Variant(inner) => {
            let interned = intern_type_name(&inner.name);
            TAG_VARIANT | (interned.as_ptr() as u64 & 0x00FF_FFFF_FFFF_FFFF)
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
fn fill_cache_slot(token: u64, g: &Global) -> crate::bytecode::CacheSlot {
    let builtin_fn = match g {
        Global::Value(Value::Builtin(inner)) => Some(inner.call),
        _ => None,
    };
    crate::bytecode::CacheSlot {
        type_token: token,
        builtin_fn,
        resolved: Some(g.clone()),
    }
}

/// Stable identity for an `Op::Call` callee — keyed by the
/// resolved-name string for `Value::String` callees (the bytecode
/// VM's idiom for "named global function"). Other callee shapes
/// (closures, builtins-passed-as-values, etc.) return `0` so the
/// IC slot stays cold and the slow path is taken every time —
/// those receivers don't have a stable identity worth caching.
pub(crate) fn call_token(v: &Value) -> u64 {
    const TAG_NAMED: u64 = 1 << 56;
    match v {
        Value::String(s) => {
            // Intern once per program — the leaked `&'static str`
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
/// same token across `Value::clone` boundaries (where `String`
/// otherwise reallocates per clone).
fn intern_type_name(name: &str) -> &'static str {
    use std::cell::RefCell;
    thread_local! {
        // Linear scan rather than HashMap: programs typically have
        // <32 distinct named receiver types in their hot path.
        static TYPE_NAMES: RefCell<Vec<(String, &'static str)>> =
            const { RefCell::new(Vec::new()) };
    }
    TYPE_NAMES.with(|cell| {
        let mut entries = cell.borrow_mut();
        for (k, interned) in entries.iter() {
            if k == name {
                return *interned;
            }
        }
        let interned: &'static str = Box::leak(name.to_string().into_boxed_str());
        entries.push((name.to_string(), interned));
        interned
    })
}

/// Returns the canonical `"<type>::<method>"` key, allocating only
/// the first time a given (type, method) pair is seen on this
/// thread. Hot-loop method dispatch (e.g. fasta's
/// `stream.write_byte(_)` per character) was burning a lot of
/// wall clock on `format!` because every call rebuilt the same
/// 17-byte string. The cache makes the repeat case a single
/// linear scan over a per-thread Vec.
///
/// The joined string is leaked into a `&'static str` so cache hits
/// return without a `String::clone`. Leak is bounded by the count
/// of distinct (type, method) pairs the program ever uses — a
/// fixed number for any given workload, typically <32.
fn intern_qualified(type_name: &str, method: &str) -> &'static str {
    use std::cell::RefCell;
    thread_local! {
        static CACHE: RefCell<Vec<(String, String, &'static str)>> =
            const { RefCell::new(Vec::new()) };
    }
    CACHE.with(|cell| {
        let mut entries = cell.borrow_mut();
        for (t, m, joined) in entries.iter() {
            if t == type_name && m == method {
                return *joined;
            }
        }
        let joined: &'static str = Box::leak(format!("{type_name}::{method}").into_boxed_str());
        entries.push((type_name.to_string(), method.to_string(), joined));
        joined
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

/// Tier C2 — classify `(a, b)` into one of the `ARITH_*` shape
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
fn record_shape(chunk: &FnChunk, cache_idx: u16, observed: u8) {
    let cache = chunk.arith_caches.borrow();
    let slot = &cache[cache_idx as usize];
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
    chunk: &FnChunk,
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
    record_shape(chunk, cache_idx, classify_pair(a, b, true));
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
#[allow(clippy::too_many_arguments)]
fn adaptive_arith(
    chunk: &FnChunk,
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
    record_shape(chunk, cache_idx, classify_pair(a, b, false));
    bin_arith(a, b, int_fn, float_fn, label)
}

/// Specialised dispatch for `Op::DivInt`. Integer divide-by-zero
/// surfaces as a runtime error, so the int-int hot path still
/// has to branch on `y == 0`. Float division never errors.
fn adaptive_div(
    chunk: &FnChunk,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                if *y == 0 {
                    return Err(RuntimeError::Arithmetic(
                        "integer divide by zero".to_string(),
                    ));
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
    record_shape(chunk, cache_idx, classify_pair(a, b, false));
    div_int(a, b)
}

/// Specialised dispatch for `Op::RemInt`. Mirrors [`adaptive_div`].
fn adaptive_rem(
    chunk: &FnChunk,
    cache_idx: u16,
    shape: u8,
    a: &Value,
    b: &Value,
) -> RuntimeResult<Value> {
    match shape {
        bytecode::ARITH_INT_INT => {
            if let (Value::Int(x), Value::Int(y)) = (a, b) {
                if *y == 0 {
                    return Err(RuntimeError::Arithmetic(
                        "integer modulo by zero".to_string(),
                    ));
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
    record_shape(chunk, cache_idx, classify_pair(a, b, false));
    rem_int(a, b)
}

fn div_int(a: &Value, b: &Value) -> RuntimeResult<Value> {
    match (a, b) {
        (Value::Int(_), Value::Int(0)) => Err(RuntimeError::Arithmetic(
            "integer divide by zero".to_string(),
        )),
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
        (Value::Int(_), Value::Int(0)) => Err(RuntimeError::Arithmetic(
            "integer modulo by zero".to_string(),
        )),
        (Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_rem(*y))),
        (Value::Float(x), Value::Float(y)) => Ok(Value::Float(x % y)),
        (Value::Int(x), Value::Float(y)) => Ok(Value::Float((*x as f64) % y)),
        (Value::Float(x), Value::Int(y)) => Ok(Value::Float(x % (*y as f64))),
        _ => Err(RuntimeError::Type("modulo on non-int values".to_string())),
    }
}

fn neg(v: &Value) -> RuntimeResult<Value> {
    match v {
        Value::Int(i) => Ok(Value::Int(-*i)),
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
    let result = match (a, b) {
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

fn truthy(v: &Value) -> RuntimeResult<bool> {
    match v {
        Value::Bool(b) => Ok(*b),
        _ => Err(RuntimeError::Type(
            "branch condition must be bool".to_string(),
        )),
    }
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        _ => false,
    }
}

/// Native struct-field read. Mirrors the walker's `eval_field`
/// behaviour: returns `Value::Unit` on unknown fields so
/// partially-typed programs keep running.
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
/// new state only if they share the same `Arc` — matching the
/// walker's `update_struct_field` semantics when the receiver
/// is a local (register) binding.
fn field_set(receiver: &mut Value, name: &str, new_value: Value) -> RuntimeResult<()> {
    let Value::Struct(struct_arc) = receiver else {
        return Err(RuntimeError::Type(format!(
            "cannot assign to field `{name}` on non-struct `{receiver}`"
        )));
    };
    let struct_inner = Arc::make_mut(struct_arc);
    let slots = Arc::make_mut(&mut struct_inner.fields);
    for (ident, slot) in slots.iter_mut() {
        if ident.name == name {
            *slot = new_value;
            return Ok(());
        }
    }
    slots.push((Ident::new(name), new_value));
    Ok(())
}
