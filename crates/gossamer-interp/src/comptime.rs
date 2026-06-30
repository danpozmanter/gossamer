//! Compile-time evaluation ("comptime") region discovery.
//!
//! A `comptime { ... }` block and every call to a `comptime fn` are
//! evaluated on the bytecode VM during compilation and replaced in the
//! source with their result literal, so the bytecode VM, the Cranelift
//! JIT, and the LLVM AOT backend all see a constant rather than the
//! original computation. This module finds the outermost such regions;
//! the VM evaluates them in [`crate::vm`], and the CLI splices the
//! results back into the source.

use std::collections::{HashMap, HashSet};

use gossamer_hir::{
    HirArrayExpr, HirExpr, HirExprKind, HirItemKind, HirProgram, HirSelectOp, HirStmt, HirStmtKind,
};
use gossamer_lex::Span;

/// One comptime region: the expression to evaluate and the source span
/// it occupies (so the result literal can be spliced over it).
pub(crate) struct Region<'a> {
    /// Source range covering the whole region (`comptime { ... }`, a
    /// `comptime fn` call, or a `comptime` parameter's argument).
    pub span: Span,
    /// Expression handed to the VM evaluator.
    pub expr: &'a HirExpr,
    /// When set, the region's `String` result is spliced as raw source
    /// (code emission via `codegen!`) rather than rendered as a quoted
    /// literal. A non-`String` result for a raw region is an error.
    pub raw: bool,
}

/// Comptime metadata gathered from the program: which free functions
/// are `comptime fn` (whole-call folding) and which functions have
/// `comptime` parameters (per-argument folding by position).
pub(crate) struct ComptimeInfo {
    fns: HashSet<String>,
    params: HashMap<String, Vec<usize>>,
}

impl ComptimeInfo {
    /// Collects comptime metadata from every function in the program.
    pub(crate) fn collect(program: &HirProgram) -> Self {
        let mut fns = HashSet::new();
        let mut params = HashMap::new();
        for item in &program.items {
            if let HirItemKind::Fn(f) = &item.kind {
                if f.is_comptime {
                    fns.insert(f.name.name.clone());
                }
                let indices: Vec<usize> = f
                    .params
                    .iter()
                    .enumerate()
                    .filter(|(_, p)| p.is_comptime)
                    .map(|(i, _)| i)
                    .collect();
                if !indices.is_empty() {
                    params.insert(f.name.name.clone(), indices);
                }
            }
        }
        Self { fns, params }
    }

    /// `true` when a call to `name` folds in its entirety (a `comptime fn`).
    fn is_comptime_fn(&self, name: &str) -> bool {
        self.fns.contains(name)
    }
}

/// Collects the outermost comptime regions across every non-comptime
/// function body, impl method, and const/static initializer. Regions
/// nested inside another comptime region are not collected separately:
/// the enclosing region's evaluation already covers them.
pub(crate) fn collect_regions<'a>(program: &'a HirProgram, info: &ComptimeInfo) -> Vec<Region<'a>> {
    let mut out = Vec::new();
    for item in &program.items {
        match &item.kind {
            HirItemKind::Fn(f) => {
                // A comptime fn's own body is folded at its call sites,
                // not in isolation (its body may reference parameters
                // that are only comptime-known per call).
                if !f.is_comptime
                    && let Some(body) = &f.body
                {
                    walk_block_stmts(&body.block.stmts, info, &mut out);
                    if let Some(tail) = &body.block.tail {
                        walk(tail, info, &mut out);
                    }
                }
            }
            HirItemKind::Impl(imp) => {
                for m in &imp.methods {
                    if !m.is_comptime
                        && let Some(body) = &m.body
                    {
                        walk_block_stmts(&body.block.stmts, info, &mut out);
                        if let Some(tail) = &body.block.tail {
                            walk(tail, info, &mut out);
                        }
                    }
                }
            }
            HirItemKind::Const(c) => walk(&c.value, info, &mut out),
            HirItemKind::Static(s) => walk(&s.value, info, &mut out),
            _ => {}
        }
    }
    out
}

/// Returns the callee's leaf path name when `expr` is a direct call.
fn call_callee_name(expr: &HirExpr) -> Option<&str> {
    let HirExprKind::Call { callee, .. } = &expr.kind else {
        return None;
    };
    let HirExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    segments.last().map(|s| s.name.as_str())
}

/// True when `expr` folds in its entirety: a `comptime { ... }` block or
/// a call to a `comptime fn`.
fn is_region_root(expr: &HirExpr, info: &ComptimeInfo) -> bool {
    match &expr.kind {
        HirExprKind::Block(block) => block.is_comptime,
        HirExprKind::Call { .. } => call_callee_name(expr).is_some_and(|n| info.is_comptime_fn(n)),
        _ => false,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one exhaustive match over every HIR expression kind; splitting the traversal would scatter it"
)]
fn walk<'a>(expr: &'a HirExpr, info: &ComptimeInfo, out: &mut Vec<Region<'a>>) {
    if is_region_root(expr, info) {
        let raw = call_callee_name(expr) == Some("__gos_codegen");
        out.push(Region {
            span: expr.span,
            expr,
            raw,
        });
        return;
    }
    match &expr.kind {
        HirExprKind::Call { callee, args } => {
            walk(callee, info, out);
            // A `comptime` parameter folds just its argument at the call
            // site: push the argument as a region, and recurse into the
            // remaining (runtime) arguments normally.
            let comptime_indices = call_callee_name(expr).and_then(|n| info.params.get(n));
            for (i, arg) in args.iter().enumerate() {
                if comptime_indices.is_some_and(|idx| idx.contains(&i)) {
                    out.push(Region {
                        span: arg.span,
                        expr: arg,
                        raw: false,
                    });
                } else {
                    walk(arg, info, out);
                }
            }
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            walk(receiver, info, out);
            for arg in args {
                walk(arg, info, out);
            }
        }
        HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
            walk(receiver, info, out);
        }
        HirExprKind::Index { base, index } => {
            walk(base, info, out);
            walk(index, info, out);
        }
        HirExprKind::Unary { operand, .. } => walk(operand, info, out),
        HirExprKind::Binary { lhs, rhs, .. } => {
            walk(lhs, info, out);
            walk(rhs, info, out);
        }
        HirExprKind::Assign { place, value } => {
            walk(place, info, out);
            walk(value, info, out);
        }
        HirExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk(condition, info, out);
            walk(then_branch, info, out);
            if let Some(e) = else_branch {
                walk(e, info, out);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            walk(scrutinee, info, out);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    walk(guard, info, out);
                }
                walk(&arm.body, info, out);
            }
        }
        HirExprKind::Loop { body, .. } | HirExprKind::While { body, .. } => {
            walk(body, info, out);
        }
        HirExprKind::Block(block) => {
            walk_block_stmts(&block.stmts, info, out);
            if let Some(tail) = &block.tail {
                walk(tail, info, out);
            }
        }
        HirExprKind::Return(Some(inner))
        | HirExprKind::Break {
            value: Some(inner), ..
        }
        | HirExprKind::Cast { value: inner, .. }
        | HirExprKind::Go(inner) => walk(inner, info, out),
        HirExprKind::Tuple(elems) | HirExprKind::Array(HirArrayExpr::List(elems)) => {
            for e in elems {
                walk(e, info, out);
            }
        }
        HirExprKind::Array(HirArrayExpr::Repeat { value, count }) => {
            walk(value, info, out);
            walk(count, info, out);
        }
        HirExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk(s, info, out);
            }
            if let Some(e) = end {
                walk(e, info, out);
            }
        }
        HirExprKind::Closure { body, .. } => walk(body, info, out),
        HirExprKind::LiftedClosure { captures, .. } => {
            for c in captures {
                walk(c, info, out);
            }
        }
        HirExprKind::Select { arms } => {
            for arm in arms {
                match &arm.op {
                    HirSelectOp::Recv { channel, .. } => walk(channel, info, out),
                    HirSelectOp::Send { channel, value } => {
                        walk(channel, info, out);
                        walk(value, info, out);
                    }
                    HirSelectOp::Default => {}
                }
                walk(&arm.body, info, out);
            }
        }
        HirExprKind::Literal(_)
        | HirExprKind::Path { .. }
        | HirExprKind::Continue { .. }
        | HirExprKind::Placeholder
        | HirExprKind::Return(None)
        | HirExprKind::Break { value: None, .. } => {}
    }
}

fn walk_block_stmts<'a>(stmts: &'a [HirStmt], info: &ComptimeInfo, out: &mut Vec<Region<'a>>) {
    for stmt in stmts {
        match &stmt.kind {
            HirStmtKind::Let {
                init: Some(expr), ..
            } => walk(expr, info, out),
            HirStmtKind::Expr { expr, .. } => walk(expr, info, out),
            HirStmtKind::Go(inner) | HirStmtKind::Defer(inner) => walk(inner, info, out),
            HirStmtKind::Let { init: None, .. } | HirStmtKind::Item(_) => {}
        }
    }
}
