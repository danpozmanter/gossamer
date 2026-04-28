//! Tree-walking evaluator over the HIR.

#![forbid(unsafe_code)]
#![allow(
    clippy::unnecessary_wraps,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::float_cmp,
    clippy::match_same_arms,
    clippy::if_same_then_else,
    clippy::map_unwrap_or,
    clippy::too_many_lines
)]

use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::thread::JoinHandle;

thread_local! {
    /// Goroutines spawned via the legacy tree-walker `go expr`
    /// path that still spins up a fresh OS thread (rare — the
    /// VM's `Op::Spawn` instead enqueues onto [`pool()`]).
    static GOROUTINE_HANDLES: RefCell<Vec<JoinHandle<()>>> = const { RefCell::new(Vec::new()) };
}

/// Joins every goroutine spawned from the current thread and
/// returns after all of them finish. Called by the CLI
/// entrypoint after `main` returns so asynchronous work has a
/// chance to land. Drains both the tree-walker handle list and
/// the bytecode VM's pool counter.
pub fn join_outstanding_goroutines() {
    let handles: Vec<JoinHandle<()>> =
        GOROUTINE_HANDLES.with(|cell| std::mem::take(&mut *cell.borrow_mut()));
    for handle in handles {
        let _ = handle.join();
    }
    pool().drain();
}

/// Goroutine task: a closure to run on a pool worker.
type GoroutineTask = Box<dyn FnOnce() + Send + 'static>;

/// Fixed-size worker pool that runs goroutines spawned via
/// [`Op::Spawn`]. Replaces the prior one-OS-thread-per-`go`
/// shape — which leaked `JoinHandle`s into a thread-local list
/// (~15 KB each) and cold-started a fresh `Vm` per goroutine.
///
/// Pool size: `num_cpus()`. Tasks queue when all workers are
/// busy; workers park on a `Condvar` when the queue is empty.
/// `outstanding` tracks queued + in-flight tasks so
/// [`join_outstanding_goroutines`] can wait for completion.
pub(crate) struct GoroutinePool {
    inner: parking_lot::Mutex<PoolInner>,
    cv: parking_lot::Condvar,
    /// Wake-up condition for `drain()` to learn that the
    /// counter has reached zero.
    drain_cv: parking_lot::Condvar,
    /// Total tasks that have not yet completed (queued +
    /// running). Used by `drain()` for completion wait.
    outstanding: AtomicU64,
    /// Total number of worker threads spawned. Capped at
    /// initialisation; never grows.
    workers: AtomicUsize,
}

struct PoolInner {
    queue: VecDeque<GoroutineTask>,
    /// `true` once the runtime is shutting down; workers exit
    /// once `queue` drains. Currently never set in practice
    /// (the process exits right after `drain()` returns), but
    /// kept for cleanliness.
    shutting_down: bool,
}

impl GoroutinePool {
    fn new(num_workers: usize) -> Arc<Self> {
        let pool = Arc::new(Self {
            inner: parking_lot::Mutex::new(PoolInner {
                queue: VecDeque::new(),
                shutting_down: false,
            }),
            cv: parking_lot::Condvar::new(),
            drain_cv: parking_lot::Condvar::new(),
            outstanding: AtomicU64::new(0),
            workers: AtomicUsize::new(0),
        });
        for _ in 0..num_workers {
            let p = Arc::clone(&pool);
            let _ = std::thread::Builder::new()
                .name("gossamer-worker".to_string())
                .spawn(move || {
                    p.workers.fetch_add(1, Ordering::Relaxed);
                    loop {
                        let task = {
                            let mut inner = p.inner.lock();
                            loop {
                                if let Some(task) = inner.queue.pop_front() {
                                    break Some(task);
                                }
                                if inner.shutting_down {
                                    break None;
                                }
                                p.cv.wait(&mut inner);
                            }
                        };
                        match task {
                            Some(task) => {
                                task();
                                let prev = p.outstanding.fetch_sub(1, Ordering::AcqRel);
                                if prev == 1 {
                                    // Last in-flight task settled —
                                    // wake any drain() waiter.
                                    p.drain_cv.notify_all();
                                }
                            }
                            None => break,
                        }
                    }
                });
        }
        pool
    }

    /// Enqueues a task. Wakes one parked worker.
    pub(crate) fn spawn(&self, task: GoroutineTask) {
        self.outstanding.fetch_add(1, Ordering::AcqRel);
        let mut inner = self.inner.lock();
        inner.queue.push_back(task);
        self.cv.notify_one();
    }

    /// Blocks until every queued / in-flight task has finished.
    /// Called by [`join_outstanding_goroutines`] at program exit.
    pub(crate) fn drain(&self) {
        let mut inner = self.inner.lock();
        while self.outstanding.load(Ordering::Acquire) > 0 {
            self.drain_cv.wait(&mut inner);
        }
    }

    #[allow(dead_code)]
    pub(crate) fn worker_count(&self) -> usize {
        self.workers.load(Ordering::Relaxed)
    }
}

static POOL: OnceLock<Arc<GoroutinePool>> = OnceLock::new();

/// Lazily-initialised process-wide goroutine pool. First call
/// builds the pool with `num_cpus()` workers.
pub(crate) fn pool() -> &'static Arc<GoroutinePool> {
    POOL.get_or_init(|| {
        // Conservative default: physical cores via `available_parallelism`.
        // Fall back to 4 when the platform refuses to report.
        let n = std::thread::available_parallelism()
            .map(std::num::NonZeroUsize::get)
            .unwrap_or(4)
            .min(64);
        GoroutinePool::new(n)
    })
}

use gossamer_ast::Ident;
use gossamer_hir::{
    HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind, HirLiteral,
    HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};

use crate::builtins;
use crate::env::Env;
use crate::value::{
    Channel, Closure, Flow, NativeDispatch, RuntimeError, RuntimeResult, SmolStr, Value,
};

/// Interpreter state. Owns the set of installed top-level functions
/// and a name table used by top-level path resolution.
#[derive(Clone)]
pub struct Interpreter {
    globals: HashMap<String, Value>,
    /// Names of Gossamer functions currently on the call stack, used
    /// to render a best-effort traceback on panic (Stream H.7).
    call_stack: Vec<String>,
}

impl Interpreter {
    /// Builds an interpreter seeded with the built-in functions.
    #[must_use]
    pub fn new() -> Self {
        let mut globals = HashMap::new();
        let mut builtin_list = Vec::new();
        builtins::install(&mut builtin_list);
        for (name, value) in builtin_list {
            globals.insert(name.to_string(), value);
        }
        Self {
            globals,
            call_stack: Vec::new(),
        }
    }

    /// Returns a snapshot of the call stack: outermost function
    /// first, currently-executing function last.
    #[must_use]
    pub fn call_stack(&self) -> Vec<String> {
        self.call_stack.clone()
    }

    /// Trims the per-task tracebacks so a worker tree-walker
    /// re-used across many goroutines does not accumulate stack
    /// frames from prior tasks. Pairs with `Vm::reset_after_task`.
    pub(crate) fn reset_after_task(&mut self) {
        self.call_stack.clear();
        self.call_stack.shrink_to_fit();
    }

    /// Installs the top-level functions from an HIR program. Structs,
    /// enums, impls, and traits are ignored by the interpreter for
    /// now — only free functions and constants are callable.
    pub fn load(&mut self, program: &HirProgram) {
        for item in &program.items {
            self.load_item(item);
        }
    }

    /// Evaluates a single standalone HIR expression in a fresh
    /// local environment seeded with `bindings`. Used by the VM's
    /// `Op::EvalDeferred` to delegate expression kinds the VM
    /// compiler doesn't native-lower. The interpreter shares the
    /// globals already loaded via [`Self::load`].
    /// Invokes a callable `Value` (closure, builtin, native
    /// dispatch, or function-name string) with `args`. Used by
    /// the VM's `dispatch_call` to handle every runtime callable
    /// shape the tree-walker knows about.
    pub fn invoke_callable_value(
        &mut self,
        callable: Value,
        args: Vec<Value>,
    ) -> RuntimeResult<Value> {
        match callable {
            Value::Builtin(inner) => (inner.call)(&args),
            Value::Native(inner) => (inner.call)(self, &args),
            Value::Closure(closure) => self.apply_closure(&closure, args),
            Value::String(name) => self.call(&name, args),
            Value::Variant(inner) if inner.fields.is_empty() => {
                Ok(Value::variant(inner.name, Arc::new(args)))
            }
            other => Err(RuntimeError::Type(format!(
                "value of kind `{other}` is not callable"
            ))),
        }
    }

    /// Evaluates `expr` with `bindings` as the initial env, then
    /// returns both the computed value and the final env values
    /// (so callers can propagate in-place mutations the walker
    /// made through local bindings). Used by the VM's
    /// `Op::EvalDeferred` delegation path.
    pub fn eval_standalone(
        &mut self,
        expr: &gossamer_hir::HirExpr,
        bindings: &[(String, Value)],
    ) -> RuntimeResult<(Value, Vec<Value>)> {
        let mut env = Env::new();
        for (name, value) in bindings {
            env.bind(name.clone(), value.clone());
        }
        let result = self.eval_expr_to_value(expr, &mut env)?;
        // Read back each binding — the walker may have mutated
        // them in place (e.g. `bodies[0].vx = x`). The caller
        // uses the returned vec to sync the VM's registers.
        let updated: Vec<Value> = bindings
            .iter()
            .map(|(name, original)| {
                env.lookup(name)
                    .cloned()
                    .unwrap_or_else(|| original.clone())
            })
            .collect();
        Ok((result, updated))
    }

    fn load_item(&mut self, item: &HirItem) {
        match &item.kind {
            HirItemKind::Fn(decl) => self.load_fn(decl),
            HirItemKind::Const(decl) => {
                let value = match self.eval_expr(&decl.value, &mut Env::new()) {
                    Ok(Flow::Value(value)) => value,
                    _ => Value::Void,
                };
                self.globals.insert(decl.name.name.clone(), value);
            }
            HirItemKind::Static(decl) => {
                let value = match self.eval_expr(&decl.value, &mut Env::new()) {
                    Ok(Flow::Value(value)) => value,
                    _ => Value::Void,
                };
                self.globals.insert(decl.name.name.clone(), value);
            }
            HirItemKind::Impl(decl) => {
                for fn_decl in &decl.methods {
                    self.load_fn(fn_decl);
                    if let Some(type_name) = &decl.self_name {
                        self.load_impl_fn(type_name, fn_decl);
                    }
                }
            }
            HirItemKind::Trait(decl) => {
                for fn_decl in &decl.methods {
                    self.load_fn(fn_decl);
                }
            }
            HirItemKind::Adt(decl) => self.load_adt(decl),
        }
    }

    /// Registers constructors for each variant of an `enum` so that
    /// `Shape::Line` and `Shape::Circle(r)` resolve to the appropriate
    /// [`Value::Variant`] at the expression level. Unit variants are
    /// bound directly; tuple and struct variants are bound to a
    /// builtin that collects its arguments into the variant payload.
    ///
    /// Variants are keyed under both the unqualified name (`Line`)
    /// and the type-qualified name (`Shape::Line`) so that either
    /// spelling at the call site resolves correctly.
    fn load_adt(&mut self, decl: &gossamer_hir::HirAdt) {
        let gossamer_hir::HirAdtKind::Enum(variants) = &decl.kind else {
            return;
        };
        let type_name = decl.name.name.clone();
        for variant in variants {
            let variant_name = variant.name.name.clone();
            let qualified = format!("{type_name}::{variant_name}");
            let sentinel = Value::variant(variant_name.clone(), crate::value::empty_value_arc());
            self.globals.insert(variant_name, sentinel.clone());
            self.globals.insert(qualified, sentinel);
        }
    }

    fn load_fn(&mut self, decl: &HirFn) {
        let Some(closure) = build_closure(decl) else {
            return;
        };
        self.globals
            .insert(decl.name.name.clone(), Value::Closure(Arc::new(closure)));
    }

    /// Registers `decl` under the fully-qualified key
    /// `TypeName::method_name` so that method dispatch on a
    /// [`Value::Struct`] with that type name can resolve unambiguously
    /// even when another impl defines a same-named method.
    fn load_impl_fn(&mut self, type_name: &Ident, decl: &HirFn) {
        let Some(closure) = build_closure(decl) else {
            return;
        };
        let key = format!("{}::{}", type_name.name, decl.name.name);
        self.globals.insert(key, Value::Closure(Arc::new(closure)));
    }

    /// Invokes a top-level function by name with the given arguments.
    pub fn call(&mut self, name: &str, args: Vec<Value>) -> RuntimeResult<Value> {
        let callee = self
            .globals
            .get(name)
            .cloned()
            .ok_or_else(|| RuntimeError::UnresolvedName(name.to_string()))?;
        self.call_stack.push(name.to_string());
        let result = self.apply(&callee, args);
        self.call_stack.pop();
        result
    }

    fn apply(&mut self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
        match callee {
            Value::Builtin(inner) => (inner.call)(&args),
            Value::Native(inner) => (inner.call)(self, &args),
            Value::Closure(closure) => self.apply_closure(closure, args),
            // Calling a zero-field variant value acts as that
            // variant's constructor: `Circle(1.5)` produces
            // `Value::variant("Circle", [1.5])`,
            // and `None()` round-trips to `None` because its args
            // vector is empty.
            Value::Variant(inner) if inner.fields.is_empty() => {
                Ok(Value::variant(inner.name, Arc::new(args)))
            }
            // Calling through any other stub-shaped value — treat as
            // a no-op so programs that thread partially-unresolved
            // paths through calls still terminate.
            _ => Ok(Value::Unit),
        }
    }

    fn apply_closure(&mut self, closure: &Closure, args: Vec<Value>) -> RuntimeResult<Value> {
        if closure.params.len() != args.len() {
            return Err(RuntimeError::Arity {
                expected: closure.params.len(),
                found: args.len(),
            });
        }
        let mut env = Env::new();
        env.push();
        for (name, value) in &closure.captures {
            env.bind(name, value.clone());
        }
        for (param, arg) in closure.params.iter().zip(args) {
            bind_pattern(&mut env, &param.pattern, arg)?;
        }
        match self.eval_expr(&closure.body, &mut env)? {
            Flow::Value(value) | Flow::Return(value) => Ok(value),
            Flow::Break(_) => Err(RuntimeError::Panic("break outside of loop".to_string())),
            Flow::Continue => Err(RuntimeError::Panic("continue outside of loop".to_string())),
        }
    }

    pub(crate) fn eval_expr(&mut self, expr: &HirExpr, env: &mut Env) -> RuntimeResult<Flow> {
        match &expr.kind {
            HirExprKind::Literal(lit) => Ok(Flow::Value(eval_literal(lit))),
            HirExprKind::Path { segments, .. } => self.eval_path(segments, env),
            HirExprKind::Call { callee, args } => self.eval_call(callee, args, env),
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
            } => self.eval_method_call(receiver, name, args, env),
            HirExprKind::Field { receiver, name } => self.eval_field(receiver, name, env),
            HirExprKind::TupleIndex { receiver, index } => {
                self.eval_tuple_index(receiver, *index, env)
            }
            HirExprKind::Index { base, index } => self.eval_index(base, index, env),
            HirExprKind::Unary { op, operand } => self.eval_unary(*op, operand, env),
            HirExprKind::Binary { op, lhs, rhs } => self.eval_binary(*op, lhs, rhs, env),
            HirExprKind::Assign { place, value } => self.eval_assign(place, value, env),
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.eval_if(condition, then_branch, else_branch.as_deref(), env),
            HirExprKind::Match { scrutinee, arms } => self.eval_match(scrutinee, arms, env),
            HirExprKind::Loop { body } => self.eval_loop(body, env),
            HirExprKind::While { condition, body } => self.eval_while(condition, body, env),
            HirExprKind::Block(block) => self.eval_block(block, env),
            HirExprKind::Closure { params, body, .. } => {
                Ok(Flow::Value(Value::Closure(Arc::new(Closure {
                    params: params.clone(),
                    body: (**body).clone(),
                    captures: env.capture_all(),
                }))))
            }
            HirExprKind::LiftedClosure { .. } => {
                // The interpreter pipeline skips `lift_closures`, so
                // this variant should never appear here in practice.
                // Return Unit defensively so a misrouted HIR program
                // does not crash the process.
                Ok(Flow::Value(Value::Unit))
            }
            HirExprKind::Select { arms } => self.eval_select(arms, env),
            HirExprKind::Return(value) => {
                let inner = match value {
                    Some(value) => self.eval_expr_to_value(value, env)?,
                    None => Value::Unit,
                };
                Ok(Flow::Return(inner))
            }
            HirExprKind::Break(value) => {
                let inner = match value {
                    Some(value) => self.eval_expr_to_value(value, env)?,
                    None => Value::Unit,
                };
                Ok(Flow::Break(inner))
            }
            HirExprKind::Continue => Ok(Flow::Continue),
            HirExprKind::Tuple(elems) => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.eval_expr_to_value(elem, env)?);
                }
                Ok(Flow::Value(Value::Tuple(Arc::new(parts))))
            }
            HirExprKind::Array(arr) => self.eval_array(arr, env),
            HirExprKind::Cast { value, .. } => {
                Ok(Flow::Value(self.eval_expr_to_value(value, env)?))
            }
            HirExprKind::Range { start, end, .. } => {
                self.eval_range(start.as_deref(), end.as_deref(), env)
            }
            HirExprKind::Go(inner) => {
                // `go expr` spawns a real OS thread. We capture the
                // current env into a clone-owned vector and clone
                // the interpreter's globals so the worker is fully
                // independent of the caller's state. A runtime
                // error inside the goroutine body is caught by the
                // worker itself and logged; it does not propagate
                // to the spawning thread.
                let captured_env = env.capture_all();
                let body = (**inner).clone();
                let mut worker = self.clone();
                let handle = std::thread::Builder::new()
                    .name("gossamer-goroutine".to_string())
                    .spawn(move || {
                        let mut env = Env::new();
                        env.push();
                        for (name, value) in captured_env {
                            env.bind(name, value);
                        }
                        if let Err(err) = worker.eval_expr_to_value(&body, &mut env) {
                            eprintln!("goroutine panic (isolated): {err}");
                        }
                    })
                    .map_err(|e| RuntimeError::Panic(format!("spawn goroutine: {e}")))?;
                // Detach: a goroutine runs to completion independently
                // of the spawning thread. We hand the JoinHandle to
                // the main-thread queue so the process doesn't exit
                // before outstanding goroutines finish when `main`
                // returns.
                GOROUTINE_HANDLES.with(|cell| cell.borrow_mut().push(handle));
                Ok(Flow::Value(Value::Unit))
            }
            HirExprKind::Placeholder => {
                // Construct a synthetic struct value so downstream
                // code that immediately field-accesses it can read
                // back stub fields without crashing.
                Ok(Flow::Value(Value::struct_(
                    "<stub>",
                    crate::value::empty_struct_fields(),
                )))
            }
        }
    }

    fn eval_range(
        &mut self,
        start: Option<&HirExpr>,
        end: Option<&HirExpr>,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let start_val = match start {
            Some(expr) => match self.eval_expr_to_value(expr, env)? {
                Value::Int(n) => n,
                _ => 0,
            },
            None => 0,
        };
        let end_val = match end {
            Some(expr) => match self.eval_expr_to_value(expr, env)? {
                Value::Int(n) => n,
                _ => start_val,
            },
            None => start_val,
        };
        let elems: Vec<Value> = if end_val > start_val {
            (start_val..end_val).map(Value::Int).collect()
        } else {
            Vec::new()
        };
        Ok(Flow::Value(Value::Array(Arc::new(elems))))
    }

    pub(crate) fn eval_expr_to_value(
        &mut self,
        expr: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Value> {
        match self.eval_expr(expr, env)? {
            Flow::Value(value) => Ok(value),
            Flow::Return(value) | Flow::Break(value) => Ok(value),
            Flow::Continue => Ok(Value::Unit),
        }
    }

    fn eval_path(&self, segments: &[Ident], env: &Env) -> RuntimeResult<Flow> {
        if let Some(first) = segments.first() {
            if let Some(value) = env.lookup(&first.name) {
                return Ok(Flow::Value(value.clone()));
            }
            // Try the fully-qualified join first so stdlib builtins
            // registered under `module::name` win over same-named
            // user-defined functions.
            if segments.len() > 1 {
                let joined: String = segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if let Some(value) = self.globals.get(&joined) {
                    return Ok(Flow::Value(value.clone()));
                }
            }
            if let Some(value) = self.globals.get(&first.name) {
                return Ok(Flow::Value(value.clone()));
            }
            // Try resolving a stdlib-style alias for the full path's
            // tail (e.g. `fmt::println` → `println`). The frontend
            // treats imported namespaces opaquely, so this is how the
            // tree-walker bridges to its built-in table.
            if segments.len() > 1 {
                if let Some(last) = segments.last() {
                    if let Some(value) = self.globals.get(&last.name) {
                        return Ok(Flow::Value(value.clone()));
                    }
                }
                // Multi-segment path whose head and tail are both
                // unknown — typical for stdlib constants like
                // `Ordering::Relaxed`. Degrade to Unit so programs
                // using them as opaque arguments keep running.
                return Ok(Flow::Value(Value::Unit));
            }
            return Err(RuntimeError::UnresolvedName(first.name.clone()));
        }
        Err(RuntimeError::UnresolvedName(String::new()))
    }

    fn eval_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let callee_value = self.eval_expr_to_value(callee, env)?;
        let mut arg_values = Vec::with_capacity(args.len());
        for arg in args {
            arg_values.push(self.eval_expr_to_value(arg, env)?);
        }
        let label = callee_label(callee);
        if let Some(name) = &label {
            self.call_stack.push(name.clone());
        }
        let result = self.apply(&callee_value, arg_values);
        match result {
            Ok(value) => {
                if label.is_some() {
                    self.call_stack.pop();
                }
                Ok(Flow::Value(value))
            }
            Err(err) => Err(err),
        }
    }

    fn eval_method_call(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let receiver_value = self.eval_expr_to_value(receiver, env)?;
        let mut arg_values = Vec::with_capacity(args.len() + 1);
        for arg in args {
            arg_values.push(self.eval_expr_to_value(arg, env)?);
        }
        // Qualified-first dispatch. Try `TypeName::method` before
        // the bare method name so a user impl never collides with
        // a same-named global builtin: `tx.send(v)` always lands
        // on `Channel::send`, and a user `impl Foo { fn send(...) }`
        // resolves to `Foo::send` instead of leaking through to a
        // stdlib `send`. The bare lookup remains the fallback for
        // primitive receivers (`s.len()`, `v.iter()`) whose runtime
        // value doesn't form a qualified key.
        if let Some(qualified) = qualified_method_key(&receiver_value, &name.name) {
            if let Some(method) = self.globals.get(&qualified).cloned() {
                let mut call_args = Vec::with_capacity(arg_values.len() + 1);
                call_args.push(receiver_value);
                call_args.extend(arg_values);
                let result = self.apply(&method, call_args)?;
                return self.maybe_writeback(receiver, &name.name, result, env);
            }
        }
        if let Some(method) = self.globals.get(name.name.as_str()).cloned() {
            let mut call_args = Vec::with_capacity(arg_values.len() + 1);
            call_args.push(receiver_value);
            call_args.extend(arg_values);
            let result = self.apply(&method, call_args)?;
            return self.maybe_writeback(receiver, &name.name, result, env);
        }
        Ok(Flow::Value(receiver_value))
    }

    /// Threads the result of a method call back through the
    /// receiver place when the method is one of the in-place
    /// mutators (`push`, `pop`, `insert`, `remove`, `clear`,
    /// `extend`, `truncate`, `sort`, `reverse`, `retain`, `drain`).
    ///
    /// The interpreter implements these as pure functions today
    /// — they return the new aggregate. Without this writeback
    /// `xs.push(v)` would compute the new array and throw it
    /// away. Instead we assign the result back into the
    /// receiver's slot and surface `Value::Unit` as the call
    /// expression's value, matching the documented `xs.push(_)
    /// -> ()` shape.
    fn maybe_writeback(
        &mut self,
        receiver: &HirExpr,
        method: &str,
        result: Value,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        if !is_mutating_method(method) {
            return Ok(Flow::Value(result));
        }
        // Only writes back to a path/field/index place. Receivers
        // that are themselves call results (`get_buf().push(v)`)
        // can't be written to anyway — the user's mistake to
        // catch later.
        if !matches!(
            receiver.kind,
            HirExprKind::Path { .. }
                | HirExprKind::Field { .. }
                | HirExprKind::Index { .. }
                | HirExprKind::TupleIndex { .. }
        ) {
            return Ok(Flow::Value(Value::Unit));
        }
        // Some mutating methods (`pop`, `remove`) legitimately
        // return a value — for those we still need to write the
        // residual aggregate back, but should surface the call's
        // return value, not `()`. Today every interp builtin in
        // the mutator list returns the *whole new aggregate*, so
        // we always return Unit and write the aggregate back.
        let _ = self.write_back(receiver, result, env);
        Ok(Flow::Value(Value::Unit))
    }

    fn eval_field(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let value = self.eval_expr_to_value(receiver, env)?;
        if let Value::Struct(inner) = &value {
            if let Some((_, v)) = inner
                .fields
                .iter()
                .find(|(ident, _)| ident.name == name.name)
            {
                return Ok(Flow::Value(v.clone()));
            }
        }
        // Unknown field access — degrade to unit rather than crash so
        // partially-typed programs keep running.
        Ok(Flow::Value(Value::Unit))
    }

    #[allow(dead_code)]
    fn eval_field_strict(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let value = self.eval_expr_to_value(receiver, env)?;
        match value {
            Value::Struct(inner) => {
                let found = inner
                    .fields
                    .iter()
                    .find(|(ident, _)| ident.name == name.name)
                    .map(|(_, v)| v.clone())
                    .ok_or_else(|| RuntimeError::Type(format!("no field `{}`", name.name)))?;
                Ok(Flow::Value(found))
            }
            other => Err(RuntimeError::Type(format!(
                "field access on non-struct `{:?}`",
                classify(&other)
            ))),
        }
    }

    fn eval_tuple_index(
        &mut self,
        receiver: &HirExpr,
        index: u32,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let value = self.eval_expr_to_value(receiver, env)?;
        match value {
            Value::Tuple(parts) => parts
                .get(index as usize)
                .cloned()
                .map(Flow::Value)
                .ok_or(RuntimeError::Type("tuple index out of bounds".to_string())),
            other => Err(RuntimeError::Type(format!(
                "tuple index on non-tuple `{:?}`",
                classify(&other)
            ))),
        }
    }

    fn eval_index(
        &mut self,
        base: &HirExpr,
        index: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let base_value = self.eval_expr_to_value(base, env)?;
        let index_value = self.eval_expr_to_value(index, env)?;
        match (base_value, index_value) {
            (Value::Array(parts), Value::Int(idx)) => {
                let idx = usize::try_from(idx)
                    .map_err(|_| RuntimeError::Type("negative array index".to_string()))?;
                parts
                    .get(idx)
                    .cloned()
                    .map(Flow::Value)
                    .ok_or(RuntimeError::Type("array index out of bounds".to_string()))
            }
            (Value::String(text), Value::Int(idx)) => {
                let idx = usize::try_from(idx)
                    .map_err(|_| RuntimeError::Type("negative string index".to_string()))?;
                let byte = text
                    .as_bytes()
                    .get(idx)
                    .copied()
                    .ok_or(RuntimeError::Type("string index out of bounds".to_string()))?;
                Ok(Flow::Value(Value::Int(i64::from(byte))))
            }
            (base, _) => Err(RuntimeError::Type(format!(
                "index on non-indexable `{:?}`",
                classify(&base)
            ))),
        }
    }

    fn eval_unary(
        &mut self,
        op: HirUnaryOp,
        operand: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let value = self.eval_expr_to_value(operand, env)?;
        let result = match (op, value) {
            (HirUnaryOp::Neg, Value::Int(i)) => Value::Int(-i),
            (HirUnaryOp::Neg, Value::Float(f)) => Value::Float(-f),
            (HirUnaryOp::Not, Value::Bool(b)) => Value::Bool(!b),
            (HirUnaryOp::RefShared | HirUnaryOp::RefMut, other) => other,
            (HirUnaryOp::Deref, Value::Struct(inner)) if inner.name == "__Cell" => {
                let set_id = inner
                    .fields
                    .iter()
                    .find(|(ident, _)| ident.name == "__set_id")
                    .and_then(|(_, v)| match v {
                        Value::Int(n) => Some(*n as u64),
                        _ => None,
                    })
                    .unwrap_or(0);
                let flag_name = inner
                    .fields
                    .iter()
                    .find(|(ident, _)| ident.name == "__flag_name")
                    .and_then(|(_, v)| match v {
                        Value::String(s) => Some(s.as_str()),
                        _ => None,
                    })
                    .unwrap_or("");
                crate::builtins::resolve_cell(set_id, flag_name).unwrap_or(Value::Unit)
            }
            (HirUnaryOp::Deref, other) => other,
            (op, other) => {
                return Err(RuntimeError::Type(format!(
                    "cannot apply `{op:?}` to `{:?}`",
                    classify(&other)
                )));
            }
        };
        Ok(Flow::Value(result))
    }

    fn eval_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
            return self.eval_short_circuit(op, lhs, rhs, env);
        }
        let a = self.eval_expr_to_value(lhs, env)?;
        let b = self.eval_expr_to_value(rhs, env)?;
        // Strict typing: any operator applied to operands of
        // incompatible kinds is a runtime error, never a silent
        // coercion. Equality (`==`, `!=`) is handled inside
        // `apply_binary` via `values_equal`, which returns `false`
        // for cross-kind operands (legal, not an error).
        let result = apply_binary(op, a, b)?;
        Ok(Flow::Value(result))
    }

    fn eval_short_circuit(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let lhs_value = self.eval_expr_to_value(lhs, env)?;
        let Value::Bool(lhs_bool) = lhs_value else {
            return Err(RuntimeError::Type("logical op on non-bool".to_string()));
        };
        let short = match op {
            HirBinaryOp::And => !lhs_bool,
            HirBinaryOp::Or => lhs_bool,
            _ => unreachable!("eval_short_circuit only fires for And/Or"),
        };
        if short {
            return Ok(Flow::Value(Value::Bool(lhs_bool)));
        }
        let rhs_value = self.eval_expr_to_value(rhs, env)?;
        if !matches!(rhs_value, Value::Bool(_)) {
            return Err(RuntimeError::Type("logical op on non-bool".to_string()));
        }
        Ok(Flow::Value(rhs_value))
    }

    fn eval_assign(
        &mut self,
        place: &HirExpr,
        value: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let rhs = self.eval_expr_to_value(value, env)?;
        match &place.kind {
            HirExprKind::Path { segments, .. } => {
                let Some(first) = segments.first() else {
                    return Err(RuntimeError::Unsupported("assignment to empty path"));
                };
                if !env.assign(&first.name, rhs) {
                    return Err(RuntimeError::UnresolvedName(first.name.clone()));
                }
                Ok(Flow::Value(Value::Unit))
            }
            HirExprKind::Field { receiver, name } => {
                let current = self.eval_expr_to_value(receiver, env)?;
                let updated = update_struct_field(&current, &name.name, rhs)?;
                self.write_back(receiver, updated, env)?;
                Ok(Flow::Value(Value::Unit))
            }
            HirExprKind::Index { base, index } => {
                let current = self.eval_expr_to_value(base, env)?;
                let idx_value = self.eval_expr_to_value(index, env)?;
                let updated = update_array_index(&current, &idx_value, rhs)?;
                self.write_back(base, updated, env)?;
                Ok(Flow::Value(Value::Unit))
            }
            _ => Err(RuntimeError::Unsupported("assignment to non-local place")),
        }
    }

    /// Writes `new_value` back into the binding named by `place`.
    ///
    /// Supports nested field/index paths: `a.b.c = x` recurses so that
    /// each step of the path is rebuilt with a fresh `Rc`, preventing
    /// cross-alias mutation when multiple live values share the same
    /// aggregate.
    fn write_back(
        &mut self,
        place: &HirExpr,
        new_value: Value,
        env: &mut Env,
    ) -> RuntimeResult<()> {
        match &place.kind {
            HirExprKind::Path { segments, .. } => {
                let Some(first) = segments.first() else {
                    return Err(RuntimeError::Unsupported("write-back to empty path"));
                };
                if !env.assign(&first.name, new_value) {
                    return Err(RuntimeError::UnresolvedName(first.name.clone()));
                }
                Ok(())
            }
            HirExprKind::Field { receiver, name } => {
                let current = self.eval_expr_to_value(receiver, env)?;
                let updated = update_struct_field(&current, &name.name, new_value)?;
                self.write_back(receiver, updated, env)
            }
            HirExprKind::Index { base, index } => {
                let current = self.eval_expr_to_value(base, env)?;
                let idx_value = self.eval_expr_to_value(index, env)?;
                let updated = update_array_index(&current, &idx_value, new_value)?;
                self.write_back(base, updated, env)
            }
            _ => Err(RuntimeError::Unsupported(
                "write-back to non-place expression",
            )),
        }
    }

    fn eval_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let cond = self.eval_expr_to_value(condition, env)?;
        let Value::Bool(cond) = cond else {
            return Err(RuntimeError::Type("if condition must be bool".to_string()));
        };
        if cond {
            self.eval_expr(then_branch, env)
        } else if let Some(else_branch) = else_branch {
            self.eval_expr(else_branch, env)
        } else {
            Ok(Flow::Value(Value::Unit))
        }
    }

    fn eval_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        let value = self.eval_expr_to_value(scrutinee, env)?;
        for arm in arms {
            env.push();
            let matched = match_pattern(env, &arm.pattern, &value);
            if matched {
                if let Some(guard) = &arm.guard {
                    let guard_value = self.eval_expr_to_value(guard, env)?;
                    if !matches!(guard_value, Value::Bool(true)) {
                        env.pop();
                        continue;
                    }
                }
                let result = self.eval_expr(&arm.body, env);
                env.pop();
                return result;
            }
            env.pop();
        }
        // No arm matched. In strict mode this is a hard error, but
        // the tree-walker currently runs against programs that use
        // stub-shaped stdlib returns (e.g. Unit in place of
        // `Result<T, E>`), so degrading to Unit keeps simple demos
        // running. Exhaustiveness is already checked up-front by
        // so this fallback cannot mask a real mistake.
        Ok(Flow::Value(Value::Unit))
    }

    /// Evaluates a `select { … }` expression by polling each arm's
    /// channel in turn and running the first one that is ready. When
    /// no arm is ready, the `default` arm runs if present; otherwise
    /// the call spins on a 1ms sleep until a channel becomes ready.
    fn eval_select(
        &mut self,
        arms: &[gossamer_hir::HirSelectArm],
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        use gossamer_hir::HirSelectOp;
        if arms.is_empty() {
            return Ok(Flow::Value(Value::Unit));
        }
        let mut resolved = Vec::with_capacity(arms.len());
        for arm in arms {
            let r = match &arm.op {
                HirSelectOp::Recv { pattern, channel } => ResolvedSelectArm::Recv {
                    channel: self.eval_expr_to_value(channel, env)?,
                    pattern: pattern.clone(),
                },
                HirSelectOp::Send { channel, value } => ResolvedSelectArm::Send {
                    channel: self.eval_expr_to_value(channel, env)?,
                    value: self.eval_expr_to_value(value, env)?,
                },
                HirSelectOp::Default => ResolvedSelectArm::Default,
            };
            resolved.push(r);
        }
        // First non-blocking pass: handles the common case where an
        // arm is already ready (or a `default` arm is reachable) in
        // a single check.
        if let Some(result) = self.try_select_once(&resolved, arms, env) {
            return result;
        }
        // No arm ready and no default. Park on the receive arms'
        // channels via Condvar — `Channel::send` notifies on every
        // push, so the first push wakes us. Send arms today are
        // best-effort (the channel is unbounded) so a send arm can
        // proceed any time; we re-check both kinds on every wake.
        let recv_channels: Vec<Channel> = resolved
            .iter()
            .filter_map(|r| match r {
                ResolvedSelectArm::Recv {
                    channel: Value::Channel(ch),
                    ..
                } => Some(ch.clone()),
                _ => None,
            })
            .collect();
        loop {
            if let Some(result) = self.try_select_once(&resolved, arms, env) {
                return result;
            }
            if recv_channels.is_empty() {
                // No receivers to park on: the spec disallows a
                // `select` with only send arms and no default, but
                // tolerate it by yielding briefly.
                std::thread::sleep(std::time::Duration::from_millis(1));
                continue;
            }
            // Park on the first receive arm's channel (any wake-up
            // re-runs `try_select_once`, which scans every arm —
            // ordering of the wait isn't meaningful). Bounded so a
            // missed notify doesn't strand the goroutine.
            let _ = recv_channels[0].wait_for(std::time::Duration::from_millis(50));
        }
    }

    /// One poll pass over every resolved arm: returns a flow when a
    /// recv arm finds data, a send arm completes, or a default arm
    /// is reachable.
    fn try_select_once(
        &mut self,
        resolved: &[ResolvedSelectArm],
        arms: &[gossamer_hir::HirSelectArm],
        env: &mut Env,
    ) -> Option<RuntimeResult<Flow>> {
        for (i, r) in resolved.iter().enumerate() {
            match r {
                ResolvedSelectArm::Recv { channel, pattern } => {
                    if let Value::Channel(ch) = channel {
                        if let Some(value) = ch.try_recv() {
                            env.push();
                            if bind_pattern(env, pattern, value).is_err() {
                                env.pop();
                                continue;
                            }
                            let result = self.eval_expr(&arms[i].body, env);
                            env.pop();
                            return Some(result);
                        }
                    }
                }
                ResolvedSelectArm::Send { channel, value } => {
                    if let Value::Channel(ch) = channel {
                        ch.send(value.clone());
                        return Some(self.eval_expr(&arms[i].body, env));
                    }
                }
                ResolvedSelectArm::Default => {}
            }
        }
        for (i, r) in resolved.iter().enumerate() {
            if matches!(r, ResolvedSelectArm::Default) {
                return Some(self.eval_expr(&arms[i].body, env));
            }
        }
        None
    }

    fn eval_loop(&mut self, body: &HirExpr, env: &mut Env) -> RuntimeResult<Flow> {
        if let Some(for_loop) = detect_for_loop(body) {
            return self.eval_for_loop(&for_loop, env);
        }
        loop {
            match self.eval_expr(body, env)? {
                Flow::Break(value) => return Ok(Flow::Value(value)),
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Continue | Flow::Value(_) => {}
            }
        }
    }

    /// Iterates the array or range produced by `for_loop.iter_expr`,
    /// binding `for_loop.loop_pat` to each element before evaluating
    /// `for_loop.body`.
    ///
    /// This short-circuits the tree-walker around the HIR's
    /// `loop { match iter.next() { Some(x) => body, None => break } }`
    /// desugaring, which would otherwise re-evaluate `iter` on every
    /// iteration and spin forever on stateless receivers like range
    /// expressions.
    fn eval_for_loop(&mut self, for_loop: &ForLoop<'_>, env: &mut Env) -> RuntimeResult<Flow> {
        let iter_value = self.eval_expr_to_value(for_loop.iter_expr, env)?;
        let Value::Array(items) = iter_value else {
            return Err(RuntimeError::Type(format!(
                "`for` loop expected an array or range, got `{}`",
                classify(&iter_value)
            )));
        };
        for item in items.iter() {
            env.push();
            bind_pattern(env, for_loop.loop_pat, item.clone())?;
            let result = self.eval_expr(for_loop.body, env);
            env.pop();
            match result? {
                Flow::Break(value) => return Ok(Flow::Value(value)),
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Continue | Flow::Value(_) => {}
            }
        }
        Ok(Flow::Value(Value::Unit))
    }

    fn eval_while(
        &mut self,
        condition: &HirExpr,
        body: &HirExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        loop {
            let cond = self.eval_expr_to_value(condition, env)?;
            let Value::Bool(cond) = cond else {
                return Err(RuntimeError::Type(
                    "while condition must be bool".to_string(),
                ));
            };
            if !cond {
                return Ok(Flow::Value(Value::Unit));
            }
            match self.eval_expr(body, env)? {
                Flow::Break(value) => return Ok(Flow::Value(value)),
                Flow::Return(value) => return Ok(Flow::Return(value)),
                Flow::Continue | Flow::Value(_) => {}
            }
        }
    }

    fn eval_block(&mut self, block: &HirBlock, env: &mut Env) -> RuntimeResult<Flow> {
        env.push();
        let result = self.eval_block_inner(block, env);
        env.pop();
        result
    }

    fn eval_block_inner(&mut self, block: &HirBlock, env: &mut Env) -> RuntimeResult<Flow> {
        for stmt in &block.stmts {
            if let Some(flow) = self.eval_stmt(stmt, env)? {
                return Ok(flow);
            }
        }
        if let Some(tail) = &block.tail {
            return self.eval_expr(tail, env);
        }
        Ok(Flow::Value(Value::Unit))
    }

    fn eval_stmt(&mut self, stmt: &HirStmt, env: &mut Env) -> RuntimeResult<Option<Flow>> {
        match &stmt.kind {
            HirStmtKind::Let { pattern, init, .. } => {
                let value = match init {
                    Some(init) => match self.eval_expr(init, env)? {
                        Flow::Value(v) => v,
                        early @ (Flow::Return(_) | Flow::Break(_) | Flow::Continue) => {
                            return Ok(Some(early));
                        }
                    },
                    None => Value::Void,
                };
                bind_pattern(env, pattern, value)?;
                Ok(None)
            }
            HirStmtKind::Expr { expr, has_semi } => {
                let flow = self.eval_expr(expr, env)?;
                match flow {
                    Flow::Value(_) => {
                        if *has_semi {
                            Ok(None)
                        } else {
                            Ok(None)
                        }
                    }
                    Flow::Return(_) | Flow::Break(_) | Flow::Continue => Ok(Some(flow)),
                }
            }
            HirStmtKind::Go(inner) => {
                // Lift statement-level `go expr;` into the same
                // spawn path the expression-level `go expr` uses.
                // Borrow-checker asks for an owned copy.
                let go_expr = HirExpr {
                    id: gossamer_hir::HirId(0),
                    span: inner.span,
                    ty: inner.ty,
                    kind: HirExprKind::Go(Box::new(inner.clone())),
                };
                let _ = self.eval_expr(&go_expr, env)?;
                Ok(None)
            }
            HirStmtKind::Defer(_) => Ok(None),
            HirStmtKind::Item(item) => {
                self.load_item(item);
                Ok(None)
            }
        }
    }

    fn eval_array(
        &mut self,
        arr: &gossamer_hir::HirArrayExpr,
        env: &mut Env,
    ) -> RuntimeResult<Flow> {
        match arr {
            gossamer_hir::HirArrayExpr::List(elems) => {
                let mut parts = Vec::with_capacity(elems.len());
                for elem in elems {
                    parts.push(self.eval_expr_to_value(elem, env)?);
                }
                Ok(Flow::Value(Value::Array(Arc::new(parts))))
            }
            gossamer_hir::HirArrayExpr::Repeat { value, count } => {
                let v = self.eval_expr_to_value(value, env)?;
                let count_value = self.eval_expr_to_value(count, env)?;
                let Value::Int(count) = count_value else {
                    return Err(RuntimeError::Type("repeat count must be int".to_string()));
                };
                let count = usize::try_from(count)
                    .map_err(|_| RuntimeError::Type("negative repeat count".to_string()))?;
                Ok(Flow::Value(Value::Array(Arc::new(vec![v; count]))))
            }
        }
    }
}

impl Default for Interpreter {
    fn default() -> Self {
        Self::new()
    }
}

/// Methods that mutate the receiver in place and (in the
/// interpreter's pure-function builtins) return the new
/// aggregate. The method-call dispatcher writes the result
/// back into the receiver's place for any name on this list.
fn is_mutating_method(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "pop"
            | "insert"
            | "remove"
            | "clear"
            | "extend"
            | "append"
            | "truncate"
            | "sort"
            | "reverse"
            | "retain"
            | "drain"
            | "swap"
    )
}

fn callee_label(expr: &HirExpr) -> Option<String> {
    match &expr.kind {
        HirExprKind::Path { segments, .. } => segments.last().map(|ident| ident.name.clone()),
        _ => None,
    }
}

impl NativeDispatch for Interpreter {
    fn call_fn(&mut self, name: &str, args: Vec<Value>) -> RuntimeResult<Value> {
        self.call(name, args)
    }
    fn call_value(&mut self, callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
        self.apply(callee, args)
    }
    fn spawn_callable(&mut self, callable: Value, args: Vec<Value>) -> RuntimeResult<()> {
        let mut worker = self.clone();
        let handle = std::thread::Builder::new()
            .name("gossamer-spawn".to_string())
            .spawn(move || {
                if let Err(err) = worker.apply(&callable, args) {
                    eprintln!("spawn panic (isolated): {err}");
                }
            })
            .map_err(|e| RuntimeError::Panic(format!("spawn worker: {e}")))?;
        GOROUTINE_HANDLES.with(|cell| cell.borrow_mut().push(handle));
        Ok(())
    }
}

/// Runtime shape of one `select` arm after evaluating its channel
/// (and, for send arms, its value) once before polling.
enum ResolvedSelectArm {
    Recv {
        channel: Value,
        pattern: gossamer_hir::HirPat,
    },
    Send {
        channel: Value,
        value: Value,
    },
    Default,
}

/// Structural view of the HIR shape produced by lowering a `for p in
/// iter { body }` statement. Borrowed from the enclosing `Loop` node.
struct ForLoop<'h> {
    /// Expression whose value is iterated.
    iter_expr: &'h HirExpr,
    /// Binding pattern, taken from the `Some(p)` arm.
    loop_pat: &'h HirPat,
    /// Body expression, taken from the `Some(p)` arm.
    body: &'h HirExpr,
}

/// Matches the HIR lowering of a `for` loop against the `body` of a
/// `Loop` node and returns the extracted iterator, pattern, and body
/// if the shape matches exactly. Used by the tree-walker to iterate
/// without re-evaluating the iterator expression on every step.
fn detect_for_loop(body: &HirExpr) -> Option<ForLoop<'_>> {
    let HirExprKind::Block(block) = &body.kind else {
        return None;
    };
    if !block.stmts.is_empty() {
        return None;
    }
    let tail = block.tail.as_deref()?;
    let HirExprKind::Match { scrutinee, arms } = &tail.kind else {
        return None;
    };
    if arms.len() != 2 {
        return None;
    }
    let HirExprKind::MethodCall {
        receiver,
        name,
        args,
    } = &scrutinee.kind
    else {
        return None;
    };
    if name.name != "next" || !args.is_empty() {
        return None;
    }
    let some_arm = &arms[0];
    let none_arm = &arms[1];
    let HirPatKind::Variant {
        name: some_name,
        fields: some_fields,
    } = &some_arm.pattern.kind
    else {
        return None;
    };
    if some_name.name != "Some" || some_fields.len() != 1 {
        return None;
    }
    let HirPatKind::Variant {
        name: none_name,
        fields: none_fields,
    } = &none_arm.pattern.kind
    else {
        return None;
    };
    if none_name.name != "None" || !none_fields.is_empty() {
        return None;
    }
    if !matches!(none_arm.body.kind, HirExprKind::Break(_)) {
        return None;
    }
    Some(ForLoop {
        iter_expr: receiver,
        loop_pat: &some_fields[0],
        body: &some_arm.body,
    })
}

/// Returns the `TypeName::method` lookup key for dispatch on
/// `receiver`, or `None` if the receiver does not name a nominal type
/// the interpreter can key on.
fn qualified_method_key(receiver: &Value, method: &str) -> Option<String> {
    match receiver {
        Value::Struct(inner) => Some(format!("{}::{}", inner.name, method)),
        Value::Channel(_) => Some(format!("Channel::{method}")),
        Value::Map(_) => Some(format!("HashMap::{method}")),
        _ => None,
    }
}

/// Builds a [`Closure`] from a lowered Gossamer function, or returns
/// `None` for stub-bodied declarations (trait methods without a
/// default, extern fns, etc.).
fn build_closure(decl: &HirFn) -> Option<Closure> {
    let body = decl.body.clone()?;
    let block = body.block;
    let span = block.span;
    let ty = block.ty;
    Some(Closure {
        params: decl.params.clone(),
        body: HirExpr {
            id: gossamer_hir::HirId(0),
            span,
            ty,
            kind: HirExprKind::Block(block),
        },
        captures: Vec::new(),
    })
}

fn eval_literal(lit: &HirLiteral) -> Value {
    match lit {
        HirLiteral::Unit => Value::Unit,
        HirLiteral::Bool(b) => Value::Bool(*b),
        HirLiteral::Int(text) => parse_int(text).map(Value::Int).unwrap_or(Value::Int(0)),
        HirLiteral::Float(text) => strip_float_suffix(text)
            .parse::<f64>()
            .ok()
            .map(Value::Float)
            .unwrap_or(Value::Float(0.0)),
        HirLiteral::String(text) => Value::String(SmolStr::from(text.clone())),
        HirLiteral::Char(c) => Value::Char(*c),
        HirLiteral::Byte(b) => Value::Int(i64::from(*b)),
        HirLiteral::ByteString(bytes) => {
            let parts = bytes.iter().map(|b| Value::Int(i64::from(*b))).collect();
            Value::Array(Arc::new(parts))
        }
    }
}

fn parse_int(text: &str) -> Option<i64> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        return i64::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return i64::from_str_radix(rest, 2).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return i64::from_str_radix(rest, 8).ok();
    }
    cleaned.parse::<i64>().ok()
}

fn strip_int_suffix(text: &str) -> String {
    const SUFFIXES: &[&str] = &[
        "i128", "u128", "isize", "usize", "i64", "u64", "i32", "u32", "i16", "u16", "i8", "u8",
    ];
    for suffix in SUFFIXES {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

fn strip_float_suffix(text: &str) -> String {
    for suffix in &["f32", "f64"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

fn apply_binary(op: HirBinaryOp, a: Value, b: Value) -> RuntimeResult<Value> {
    if let HirBinaryOp::Add = op {
        if let (Value::String(x), Value::String(y)) = (&a, &b) {
            let mut out = String::with_capacity(x.len() + y.len());
            out.push_str(x);
            out.push_str(y);
            return Ok(Value::String(out.into()));
        }
        if let (Value::String(x), rhs) = (&a, &b) {
            return Ok(Value::String(SmolStr::from(format!("{x}{rhs}"))));
        }
        if let (lhs, Value::String(y)) = (&a, &b) {
            return Ok(Value::String(SmolStr::from(format!("{lhs}{y}"))));
        }
    }
    match (op, a, b) {
        (HirBinaryOp::Add, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_add(y))),
        (HirBinaryOp::Sub, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_sub(y))),
        (HirBinaryOp::Mul, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x.wrapping_mul(y))),
        (HirBinaryOp::Div, Value::Int(x), Value::Int(y)) => {
            if y == 0 {
                return Err(RuntimeError::Arithmetic(
                    "integer divide by zero".to_string(),
                ));
            }
            Ok(Value::Int(x.wrapping_div(y)))
        }
        (HirBinaryOp::Rem, Value::Int(x), Value::Int(y)) => {
            if y == 0 {
                return Err(RuntimeError::Arithmetic(
                    "integer modulo by zero".to_string(),
                ));
            }
            Ok(Value::Int(x.wrapping_rem(y)))
        }
        (HirBinaryOp::BitAnd, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x & y)),
        (HirBinaryOp::BitOr, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x | y)),
        (HirBinaryOp::BitXor, Value::Int(x), Value::Int(y)) => Ok(Value::Int(x ^ y)),
        (HirBinaryOp::Shl, Value::Int(x), Value::Int(y)) => {
            Ok(Value::Int(x.wrapping_shl(y as u32)))
        }
        (HirBinaryOp::Shr, Value::Int(x), Value::Int(y)) => {
            Ok(Value::Int(x.wrapping_shr(y as u32)))
        }
        (HirBinaryOp::Add, Value::Float(x), Value::Float(y)) => Ok(Value::Float(x + y)),
        (HirBinaryOp::Sub, Value::Float(x), Value::Float(y)) => Ok(Value::Float(x - y)),
        (HirBinaryOp::Mul, Value::Float(x), Value::Float(y)) => Ok(Value::Float(x * y)),
        (HirBinaryOp::Div, Value::Float(x), Value::Float(y)) => Ok(Value::Float(x / y)),
        (HirBinaryOp::Rem, Value::Float(x), Value::Float(y)) => Ok(Value::Float(x % y)),
        (HirBinaryOp::Eq, a, b) => Ok(Value::Bool(values_equal(&a, &b))),
        (HirBinaryOp::Ne, a, b) => Ok(Value::Bool(!values_equal(&a, &b))),
        (HirBinaryOp::Lt, Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x < y)),
        (HirBinaryOp::Le, Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x <= y)),
        (HirBinaryOp::Gt, Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x > y)),
        (HirBinaryOp::Ge, Value::Int(x), Value::Int(y)) => Ok(Value::Bool(x >= y)),
        (HirBinaryOp::Lt, Value::Float(x), Value::Float(y)) => Ok(Value::Bool(x < y)),
        (HirBinaryOp::Le, Value::Float(x), Value::Float(y)) => Ok(Value::Bool(x <= y)),
        (HirBinaryOp::Gt, Value::Float(x), Value::Float(y)) => Ok(Value::Bool(x > y)),
        (HirBinaryOp::Ge, Value::Float(x), Value::Float(y)) => Ok(Value::Bool(x >= y)),
        (op, a, b) => Err(RuntimeError::Type(format!(
            "cannot apply `{op:?}` to `{:?}`/`{:?}`",
            classify(&a),
            classify(&b)
        ))),
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
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y.iter()).all(|(a, b)| values_equal(a, b))
        }
        _ => false,
    }
}

fn bind_pattern(env: &mut Env, pattern: &HirPat, value: Value) -> RuntimeResult<()> {
    match &pattern.kind {
        HirPatKind::Wildcard | HirPatKind::Rest => Ok(()),
        HirPatKind::Binding { name, .. } => {
            env.bind(name.name.clone(), value);
            Ok(())
        }
        HirPatKind::Literal(_) => Ok(()),
        HirPatKind::Tuple(parts) => match value {
            Value::Tuple(vals) if vals.len() == parts.len() => {
                for (pat, v) in parts.iter().zip(vals.iter()) {
                    bind_pattern(env, pat, v.clone())?;
                }
                Ok(())
            }
            // Shape-mismatched tuple destructuring is common when a
            // stub-shaped call returns Unit or Array in place of the
            // real tuple (e.g. `channel::<T>()`). Bind every pattern
            // slot to Unit and keep going rather than failing the
            // program.
            _other => {
                for pat in parts {
                    bind_pattern(env, pat, Value::Unit)?;
                }
                Ok(())
            }
        },
        HirPatKind::Variant { fields, .. } => match value {
            Value::Variant(var_inner) if var_inner.fields.len() == fields.len() => {
                for (pat, v) in fields.iter().zip(var_inner.fields.iter()) {
                    bind_pattern(env, pat, v.clone())?;
                }
                Ok(())
            }
            _ => Ok(()),
        },
        HirPatKind::Struct { fields, .. } => match value {
            Value::Struct(struct_inner) => {
                for field in fields {
                    let found = struct_inner
                        .fields
                        .iter()
                        .find(|(ident, _)| ident.name == field.name.name)
                        .map(|(_, v)| v.clone());
                    if let Some(value) = found {
                        if let Some(pat) = &field.pattern {
                            bind_pattern(env, pat, value)?;
                        } else {
                            env.bind(field.name.name.clone(), value);
                        }
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        },
        HirPatKind::Ref {
            inner: ref_inner, ..
        } => bind_pattern(env, ref_inner, value),
        HirPatKind::Or(alts) => {
            if let Some(first) = alts.first() {
                bind_pattern(env, first, value)?;
            }
            Ok(())
        }
        // Range patterns introduce no new bindings.
        HirPatKind::Range { .. } => Ok(()),
    }
}

fn match_pattern(env: &mut Env, pattern: &HirPat, value: &Value) -> bool {
    match &pattern.kind {
        HirPatKind::Wildcard | HirPatKind::Rest => true,
        HirPatKind::Binding { name, .. } => {
            env.bind(name.name.clone(), value.clone());
            true
        }
        HirPatKind::Literal(lit) => literal_matches(lit, value),
        HirPatKind::Tuple(parts) => match value {
            Value::Tuple(vals) if vals.len() == parts.len() => parts
                .iter()
                .zip(vals.iter())
                .all(|(pat, v)| match_pattern(env, pat, v)),
            _ => false,
        },
        HirPatKind::Variant { name, fields } => match value {
            Value::Variant(var_inner)
                if var_inner.name == name.name && var_inner.fields.len() == fields.len() =>
            {
                fields
                    .iter()
                    .zip(var_inner.fields.iter())
                    .all(|(pat, v)| match_pattern(env, pat, v))
            }
            _ => false,
        },
        HirPatKind::Struct { fields, .. } => match value {
            Value::Struct(struct_inner) => {
                for field in fields {
                    let found = struct_inner
                        .fields
                        .iter()
                        .find(|(ident, _)| ident.name == field.name.name)
                        .map(|(_, v)| v.clone());
                    if let Some(value) = found {
                        if let Some(pat) = &field.pattern {
                            if !match_pattern(env, pat, &value) {
                                return false;
                            }
                        } else {
                            env.bind(field.name.name.clone(), value);
                        }
                    }
                }
                true
            }
            _ => false,
        },
        HirPatKind::Ref { inner, .. } => match_pattern(env, inner, value),
        HirPatKind::Or(alts) => alts.iter().any(|alt| match_pattern(env, alt, value)),
        HirPatKind::Range { lo, hi, inclusive } => {
            // Numeric ranges only — `1..=9 => …` arms etc. The
            // value side must be `Value::Int`; the bounds parse
            // as ints, and the comparison is `lo <= v && v <op> hi`.
            let HirLiteral::Int(lo_text) = lo else {
                return false;
            };
            let HirLiteral::Int(hi_text) = hi else {
                return false;
            };
            let Some(lo_v) = parse_int(lo_text) else {
                return false;
            };
            let Some(hi_v) = parse_int(hi_text) else {
                return false;
            };
            match value {
                Value::Int(v) => {
                    // `parse_int` already returns the bounds as
                    // i64 for the integer-literal range case;
                    // promote both sides to i128 so the comparison
                    // doesn't overflow at the i64 extremes.
                    let v = i128::from(*v);
                    let lo_v = i128::from(lo_v);
                    let hi_v = i128::from(hi_v);
                    let lower_ok = lo_v <= v;
                    let upper_ok = if *inclusive { v <= hi_v } else { v < hi_v };
                    lower_ok && upper_ok
                }
                _ => false,
            }
        }
    }
}

fn literal_matches(lit: &HirLiteral, value: &Value) -> bool {
    match (lit, value) {
        (HirLiteral::Bool(a), Value::Bool(b)) => a == b,
        (HirLiteral::Int(text), Value::Int(b)) => parse_int(text).is_some_and(|a| a == *b),
        (HirLiteral::Char(a), Value::Char(b)) => a == b,
        (HirLiteral::String(a), Value::String(b)) => a == b.as_str(),
        (HirLiteral::Unit, Value::Unit) => true,
        _ => false,
    }
}

fn classify(value: &Value) -> &'static str {
    match value {
        Value::Unit => "()",
        Value::Bool(_) => "bool",
        Value::Int(_) => "int",
        Value::Float(_) => "float",
        Value::Char(_) => "char",
        Value::String(_) => "string",
        Value::Tuple(_) => "tuple",
        Value::Array(_) => "array",
        Value::FloatArray { .. } => "array",
        Value::IntArray(_) => "array",
        Value::FloatVec(_) => "array",
        Value::Variant { .. } => "variant",
        Value::Struct { .. } => "struct",
        Value::Closure(_) => "closure",
        Value::Builtin { .. } => "builtin",
        Value::Native { .. } => "native",
        Value::Channel(_) => "channel",
        Value::Map(_) => "map",
        Value::IntMap(_) => "map",
        Value::Void => "void",
    }
}

/// Returns a fresh `Value::Struct` whose `field_name` entry carries
/// `new_value` and whose remaining fields are cloned from `current`.
///
/// Always allocates a new `Rc` for the field table, so any aliasing
/// struct value — for example, one held by another binding that was
/// cloned from the same source — observes its old field values.
fn update_struct_field(
    current: &Value,
    field_name: &str,
    new_value: Value,
) -> RuntimeResult<Value> {
    let Value::Struct(inner) = current else {
        return Err(RuntimeError::Type(format!(
            "cannot assign to field `{field_name}` on non-struct `{}`",
            classify(current)
        )));
    };
    let mut owned = inner.fields.as_ref().clone();
    let mut replaced = false;
    for (ident, slot) in &mut owned {
        if ident.name == field_name {
            *slot = new_value.clone();
            replaced = true;
            break;
        }
    }
    if !replaced {
        owned.push((Ident::new(field_name), new_value));
    }
    Ok(Value::struct_(inner.name, Arc::new(owned)))
}

/// Returns a fresh `Value::Array` with `index`-th element replaced by
/// `new_value`; the remaining elements are cloned from `current`. Any
/// aliasing array value keeps its old elements.
fn update_array_index(current: &Value, index: &Value, new_value: Value) -> RuntimeResult<Value> {
    let Value::Array(parts) = current else {
        return Err(RuntimeError::Type(format!(
            "cannot index-assign on non-array `{}`",
            classify(current)
        )));
    };
    let Value::Int(idx) = index else {
        return Err(RuntimeError::Type(format!(
            "array index must be integer, got `{}`",
            classify(index)
        )));
    };
    let idx_usize = usize::try_from(*idx)
        .map_err(|_| RuntimeError::Type("negative array index".to_string()))?;
    let mut owned = parts.as_ref().clone();
    if idx_usize >= owned.len() {
        return Err(RuntimeError::Type("array index out of bounds".to_string()));
    }
    owned[idx_usize] = new_value;
    Ok(Value::Array(Arc::new(owned)))
}
