#![allow(clippy::wildcard_imports)]
use super::*;

/// Tail-node ceiling for an inlinable function: a callee whose tail
/// expression weighs more than this stays a real `Op::Call`. Bounds the
/// per-call-site code growth. One unit per expression node.
const INLINE_TAIL_COST_LIMIT: usize = 24;

/// Total inlined-node budget one caller may accrue before further
/// inlines into it fall back to real calls. Caps code blow-up when a hot
/// function inlines many small helpers (transitively).
const INLINE_CALLER_BUDGET: usize = 96;

/// Returns `false` when `GOSSAMER_INLINE=0` (or `false`) is set, so the
/// differential harness can compare a program's output with the
/// bytecode inliner on and off. Read live (not memoised) so a test can
/// flip it between runs. Mirrors the MIR tier's `inlining_enabled`.
fn inlining_enabled() -> bool {
    !matches!(
        std::env::var("GOSSAMER_INLINE").ok().as_deref(),
        Some("0" | "false")
    )
}

/// True when `callee` names an explicit diagnostic builtin (`panic` /
/// `assert` / `assert_eq`). A function that calls one is a traceback
/// boundary: inlining it would drop its frame from the panic call-stack
/// snapshot, so such a function stays a real `Op::Call`. Hot-path
/// numeric helpers never call these, so the inline win is unaffected.
fn callee_is_diagnostic(callee: &HirExpr) -> bool {
    let HirExprKind::Path { segments, .. } = &callee.kind else {
        return false;
    };
    matches!(
        segments.as_slice(),
        [seg] if matches!(seg.name.as_str(), "panic" | "assert" | "assert_eq")
    )
}

/// Weighted node count of `expr`, or `None` when `expr` contains a
/// construct that is unsafe to re-compile at a call site. The whitelist
/// admits only side-effect-transparent, control-flow-free shapes:
/// literals, name reads, arithmetic, casts, indexing, field / tuple
/// access, and nested calls (which may themselves inline). Everything
/// else — control flow (`if` / `match` / loops / `return` / `break` /
/// `continue`), closures, `go` / `select`, assignment, and method calls
/// — rejects the function, keeping the MVP correctness-first.
fn tail_inline_cost(expr: &HirExpr) -> Option<usize> {
    use HirExprKind as K;
    let children = match &expr.kind {
        K::Literal(_) | K::Path { .. } => 0,
        K::Binary { lhs, rhs, .. } => tail_inline_cost(lhs)? + tail_inline_cost(rhs)?,
        K::Unary { operand, .. } => tail_inline_cost(operand)?,
        K::Cast { value, .. } => tail_inline_cost(value)?,
        K::Index { base, index } => tail_inline_cost(base)? + tail_inline_cost(index)?,
        K::Field { receiver, .. } | K::TupleIndex { receiver, .. } => tail_inline_cost(receiver)?,
        K::Call { callee, args } => {
            if callee_is_diagnostic(callee) {
                return None;
            }
            let mut sum = tail_inline_cost(callee)?;
            for arg in args {
                sum += tail_inline_cost(arg)?;
            }
            sum
        }
        _ => return None,
    };
    Some(1 + children)
}

/// Recognises a user function the bytecode compiler may inline at its
/// call sites: a free function (no `self` receiver) whose body is a
/// single tail expression (no statements), every parameter a plain
/// by-value binding (no `&mut` write-back shape), and whose tail passes
/// [`tail_inline_cost`] under [`INLINE_TAIL_COST_LIMIT`]. Returns an
/// owned snapshot so the inliner never re-borrows the `HirProgram`.
pub(crate) fn detect_inlinable_fn(decl: &HirFn, tcx: &TyCtxt) -> Option<InlinableFn> {
    if decl.has_self {
        return None;
    }
    let body = decl.body.as_ref()?;
    // Single-tail-expression body: matches `fn mat_a(i, j) -> f64 { … }`.
    if !body.block.stmts.is_empty() {
        return None;
    }
    let tail = body.block.tail.as_deref()?;
    for param in &decl.params {
        if !matches!(param.pattern.kind, HirPatKind::Binding { .. }) {
            return None;
        }
        // A `&mut Vec` / `&mut [T]` / `&mut <scalar>` parameter rides the
        // write-back cell protocol; inlining its body would drop the
        // caller-visible mutation, so leave it a real call.
        if is_mut_ref_writeback(tcx, param.ty) {
            return None;
        }
    }
    let cost = tail_inline_cost(tail)?;
    if cost > INLINE_TAIL_COST_LIMIT {
        return None;
    }
    Some(InlinableFn {
        params: decl.params.iter().map(|p| p.pattern.clone()).collect(),
        tail: tail.clone(),
        cost,
    })
}

impl<'tcx> FnBuilder<'tcx> {
    /// Inlines a call to a detected [`InlinableFn`] by re-compiling the
    /// callee's tail expression directly into the caller, with the
    /// callee's parameters bound to the already-compiled argument
    /// registers. Returns `Some(tail_reg)` — preserving the tail's
    /// `RegKind` so a numeric result stays unboxed — or `None` when the
    /// call is not inlinable (unknown callee, arity mismatch, recursion,
    /// or budget exhausted), in which case the caller emits a real
    /// `Op::Call`.
    ///
    /// The callee body compiles in an isolated scope stack containing
    /// only its parameters, so a body reference to a global never
    /// accidentally resolves to a caller local of the same name
    /// (inlining hygiene). Register allocation stays shared: the inlined
    /// body's temporaries live in the caller's register file.
    pub(crate) fn try_inline_user_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
    ) -> RuntimeResult<Option<TypedReg>> {
        if !inlining_enabled() {
            return Ok(None);
        }
        // Only a bare single-segment path naming a free function inlines.
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return Ok(None);
        };
        let [seg] = segments.as_slice() else {
            return Ok(None);
        };
        // A local of the same name shadows the global function (a
        // fn-pointer binding): leave dispatch to the value in the local.
        if self.lookup_local(seg.name.as_str()).is_some() {
            return Ok(None);
        }
        // Copy the `'tcx` map reference out of `self` first so the looked
        // up `&InlinableFn` is independent of the `&mut self` borrows
        // below — no clone of the callee's HIR is needed.
        let fns: &'tcx InlinableFns = self.inline_fns;
        let Some(info) = fns.get(seg.name.as_str()) else {
            return Ok(None);
        };
        if info.params.len() != args.len() {
            return Ok(None);
        }
        // Recursion guard: never inline the function currently being
        // compiled into itself, nor any function already on the inline
        // stack (covers direct, mutual, and transitive recursion).
        let name = crate::value::intern_type_name(seg.name.as_str());
        if name == self.name || self.inlining.contains(&name) {
            return Ok(None);
        }
        // Per-caller budget: once exhausted, further calls stay real.
        if self.inlined_nodes + info.cost > INLINE_CALLER_BUDGET {
            return Ok(None);
        }
        // Evaluate every argument once, left-to-right, in the caller's
        // current scope — matching call-by-value evaluation order.
        let mut arg_regs: Vec<TypedReg> = Vec::with_capacity(args.len());
        for arg in args {
            arg_regs.push(self.compile_expr_ex(arg)?);
        }
        self.inlining.push(name);
        self.inlined_nodes += info.cost;
        // Swap in a fresh scope stack holding only the parameters; the
        // caller's locals are invisible to the callee body.
        let saved_scopes = std::mem::replace(&mut self.scopes, vec![Scope::default()]);
        for (pattern, arg) in info.params.iter().zip(arg_regs.iter()) {
            if let HirPatKind::Binding {
                name: param_name, ..
            } = &pattern.kind
            {
                self.bind_local(&param_name.name, *arg);
            }
        }
        let result = self.compile_expr_ex(&info.tail);
        // Restore the caller's scopes and pop the recursion stack on every
        // exit path, including the error path, before surfacing the result.
        self.scopes = saved_scopes;
        self.inlining.pop();
        Ok(Some(result?))
    }
}
