//! Post-lowering HIR rewrite that lifts non-capturing closures to
//! top-level functions.
//! A closure with no free variables is equivalent to a regular named
//! function. Lifting those closures gives the native backend a real
//! function pointer it can emit a direct call to, instead of
//! bailing out to the interpreter for every `map` / `filter` / etc.
//! Closures that genuinely capture variables are left alone and
//! continue to route through the tree-walker.

#![allow(clippy::too_many_lines)]

use std::collections::HashSet;

use gossamer_ast::Ident;
use gossamer_lex::Span;

use crate::ids::HirIdGenerator;
use crate::tree::{
    HirArrayExpr, HirBlock, HirBody, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind, HirParam,
    HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind,
};

/// Walks `program` and lifts every closure with no free variables
/// into a top-level [`HirItemKind::Fn`] item with a synthetic name,
/// replacing the original closure expression with a
/// [`HirExprKind::Path`] that points at it. Closures that capture
/// outer bindings are left untouched.
#[must_use]
pub fn lift_closures(mut program: HirProgram) -> HirProgram {
    let mut lifter = Lifter {
        next_id: 0,
        lifted: Vec::new(),
        ids: HirIdGenerator::new(),
    };
    for item in &mut program.items {
        if let HirItemKind::Fn(decl) = &mut item.kind {
            if let Some(body) = &mut decl.body {
                lifter.visit_block(&mut body.block);
            }
        }
    }
    let mut items = program.items;
    items.extend(lifter.lifted);
    HirProgram { items }
}

struct Lifter {
    next_id: u32,
    lifted: Vec<HirItem>,
    ids: HirIdGenerator,
}

impl Lifter {
    fn fresh_name(&mut self) -> Ident {
        let idx = self.next_id;
        self.next_id += 1;
        Ident::new(format!("__closure_{idx}"))
    }

    fn visit_block(&mut self, block: &mut HirBlock) {
        for stmt in &mut block.stmts {
            self.visit_stmt(stmt);
        }
        if let Some(tail) = &mut block.tail {
            self.visit_expr(tail);
        }
    }

    fn visit_stmt(&mut self, stmt: &mut HirStmt) {
        match &mut stmt.kind {
            HirStmtKind::Let {
                init: Some(expr), ..
            } => self.visit_expr(expr),
            HirStmtKind::Let { init: None, .. } => {}
            HirStmtKind::Expr { expr, .. } => self.visit_expr(expr),
            HirStmtKind::Go(inner) => self.visit_expr(inner),
            HirStmtKind::Defer(inner) => self.visit_expr(inner),
            HirStmtKind::Item(_) => {}
        }
    }

    fn visit_expr(&mut self, expr: &mut HirExpr) {
        // Recurse into children first so inner closures are lifted
        // before we process the outer one.
        match &mut expr.kind {
            HirExprKind::Call { callee, args } => {
                self.visit_expr(callee);
                for arg in args {
                    self.visit_expr(arg);
                }
            }
            HirExprKind::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver);
                for arg in args {
                    self.visit_expr(arg);
                }
            }
            HirExprKind::Field { receiver, .. } => self.visit_expr(receiver),
            HirExprKind::TupleIndex { receiver, .. } => self.visit_expr(receiver),
            HirExprKind::Index { base, index } => {
                self.visit_expr(base);
                self.visit_expr(index);
            }
            HirExprKind::Unary { operand, .. } => self.visit_expr(operand),
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.visit_expr(lhs);
                self.visit_expr(rhs);
            }
            HirExprKind::Assign { place, value } => {
                self.visit_expr(place);
                self.visit_expr(value);
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.visit_expr(condition);
                self.visit_expr(then_branch);
                if let Some(else_branch) = else_branch {
                    self.visit_expr(else_branch);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee);
                for arm in arms {
                    if let Some(guard) = &mut arm.guard {
                        self.visit_expr(guard);
                    }
                    self.visit_expr(&mut arm.body);
                }
            }
            HirExprKind::Loop { body } | HirExprKind::While { body, .. } => {
                self.visit_expr(body);
            }
            HirExprKind::Block(block) => self.visit_block(block),
            HirExprKind::Return(Some(inner))
            | HirExprKind::Break(Some(inner))
            | HirExprKind::Cast { value: inner, .. }
            | HirExprKind::Go(inner) => self.visit_expr(inner),
            HirExprKind::Return(None) | HirExprKind::Break(None) => {}
            HirExprKind::Tuple(elems) => {
                for e in elems {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Array(HirArrayExpr::List(elems)) => {
                for e in elems {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Array(HirArrayExpr::Repeat { value, count }) => {
                self.visit_expr(value);
                self.visit_expr(count);
            }
            HirExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.visit_expr(s);
                }
                if let Some(e) = end {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Closure { body, .. } => self.visit_expr(body),
            HirExprKind::LiftedClosure { captures, .. } => {
                for c in captures {
                    self.visit_expr(c);
                }
            }
            HirExprKind::Select { arms } => {
                for arm in arms {
                    match &mut arm.op {
                        crate::tree::HirSelectOp::Recv { channel, .. } => {
                            self.visit_expr(channel);
                        }
                        crate::tree::HirSelectOp::Send { channel, value } => {
                            self.visit_expr(channel);
                            self.visit_expr(value);
                        }
                        crate::tree::HirSelectOp::Default => {}
                    }
                    self.visit_expr(&mut arm.body);
                }
            }
            HirExprKind::Literal(_)
            | HirExprKind::Path { .. }
            | HirExprKind::Continue
            | HirExprKind::Placeholder => {}
        }

        if let HirExprKind::Closure { params, ret, body } = &expr.kind {
            let mut bound: HashSet<String> = HashSet::new();
            for param in params {
                collect_pattern_names(&param.pattern, &mut bound);
            }
            if is_closed(body, &bound) {
                let lifted_name = self.lift_closed(params, *ret, body, expr.span);
                expr.kind = HirExprKind::Path {
                    segments: vec![lifted_name],
                    def: None,
                };
            } else {
                // Capturing closure: collect free vars, generate a
                // `__closure_N(env, params…)` lifted function that
                // reads captures via `gos_load`, and rewrite the
                // closure expression into a `LiftedClosure` node
                // that the MIR lowerer expands into the heap-alloc
                // sequence.
                let captures = collect_free_vars(body, &bound);
                if !captures.is_empty() {
                    let (name, capture_exprs) =
                        self.lift_capturing(params, *ret, body, &captures, expr.span);
                    expr.kind = HirExprKind::LiftedClosure {
                        name,
                        captures: capture_exprs,
                    };
                }
            }
        }
    }

    fn lift_closed(
        &mut self,
        params: &[HirParam],
        ret: Option<gossamer_types::Ty>,
        body: &HirExpr,
        span: Span,
    ) -> Ident {
        let name = self.fresh_name();
        let hir_body = HirBody {
            block: HirBlock {
                id: self.ids.next(),
                span,
                stmts: Vec::new(),
                tail: Some(Box::new(body.clone())),
                ty: body.ty,
            },
        };
        let decl = HirFn {
            name: name.clone(),
            params: params.to_vec(),
            ret,
            body: Some(hir_body),
            is_unsafe: false,
            has_self: false,
        };
        self.lifted.push(HirItem {
            id: self.ids.next(),
            span,
            def: None,
            kind: HirItemKind::Fn(decl),
        });
        name
    }

    fn lift_capturing(
        &mut self,
        params: &[HirParam],
        ret: Option<gossamer_types::Ty>,
        body: &HirExpr,
        captures: &[String],
        span: Span,
    ) -> (Ident, Vec<HirExpr>) {
        let name = self.fresh_name();
        // The lifted function's body wraps the original body in a
        // block that first pulls each capture out of the env pointer
        // via `gos_load(env, offset)`, binds it to a local of the
        // same name, then evaluates the original body.
        let mut stmts: Vec<HirStmt> = Vec::with_capacity(captures.len());
        for (i, cap) in captures.iter().enumerate() {
            let offset = (i as i64 + 1) * 8;
            let load_call = self.make_env_load("__env", offset, body.span, body.ty);
            stmts.push(HirStmt {
                id: self.ids.next(),
                span: body.span,
                kind: HirStmtKind::Let {
                    pattern: HirPat {
                        id: self.ids.next(),
                        span: body.span,
                        ty: body.ty,
                        kind: HirPatKind::Binding {
                            name: Ident::new(cap),
                            mutable: false,
                        },
                    },
                    ty: body.ty,
                    init: Some(load_call),
                },
            });
        }
        let wrapper_block = HirBlock {
            id: self.ids.next(),
            span,
            stmts,
            tail: Some(Box::new(body.clone())),
            ty: body.ty,
        };
        let env_param = HirParam {
            pattern: HirPat {
                id: self.ids.next(),
                span,
                ty: body.ty,
                kind: HirPatKind::Binding {
                    name: Ident::new("__env"),
                    mutable: false,
                },
            },
            ty: body.ty,
        };
        let mut new_params = vec![env_param];
        new_params.extend(params.iter().cloned());
        let decl = HirFn {
            name: name.clone(),
            params: new_params,
            ret,
            body: Some(HirBody {
                block: wrapper_block,
            }),
            is_unsafe: false,
            has_self: false,
        };
        self.lifted.push(HirItem {
            id: self.ids.next(),
            span,
            def: None,
            kind: HirItemKind::Fn(decl),
        });
        let capture_exprs: Vec<HirExpr> = captures
            .iter()
            .map(|n| HirExpr {
                id: self.ids.next(),
                span,
                ty: body.ty,
                kind: HirExprKind::Path {
                    segments: vec![Ident::new(n)],
                    def: None,
                },
            })
            .collect();
        (name, capture_exprs)
    }

    /// Builds `gos_load(env, offset)` as a HIR call expression.
    fn make_env_load(
        &mut self,
        env_name: &str,
        offset: i64,
        span: Span,
        ty: gossamer_types::Ty,
    ) -> HirExpr {
        let env_ref = HirExpr {
            id: self.ids.next(),
            span,
            ty,
            kind: HirExprKind::Path {
                segments: vec![Ident::new(env_name)],
                def: None,
            },
        };
        let offset_lit = HirExpr {
            id: self.ids.next(),
            span,
            ty,
            kind: HirExprKind::Literal(crate::tree::HirLiteral::Int(offset.to_string())),
        };
        let callee = HirExpr {
            id: self.ids.next(),
            span,
            ty,
            kind: HirExprKind::Path {
                segments: vec![Ident::new("gos_load")],
                def: None,
            },
        };
        HirExpr {
            id: self.ids.next(),
            span,
            ty,
            kind: HirExprKind::Call {
                callee: Box::new(callee),
                args: vec![env_ref, offset_lit],
            },
        }
    }
}

/// Collects every binding name introduced by `pat` into `out`.
///
/// Used by free-variable analysis: a name is "bound" if it's
/// introduced by an enclosing pattern (parameter, `let`, match arm),
/// so collecting pattern names builds the bound-set passed into
/// [`collect_free_vars`].
pub fn collect_pattern_names<S: std::hash::BuildHasher + Clone>(
    pat: &HirPat,
    out: &mut HashSet<String, S>,
) {
    match &pat.kind {
        HirPatKind::Binding { name, .. } => {
            out.insert(name.name.clone());
        }
        HirPatKind::Tuple(subs) | HirPatKind::Variant { fields: subs, .. } => {
            for sub in subs {
                collect_pattern_names(sub, out);
            }
        }
        HirPatKind::Struct { fields, .. } => {
            for f in fields {
                if let Some(sub) = &f.pattern {
                    collect_pattern_names(sub, out);
                } else {
                    out.insert(f.name.name.clone());
                }
            }
        }
        HirPatKind::Or(alts) => {
            for alt in alts {
                collect_pattern_names(alt, out);
            }
        }
        HirPatKind::Ref { inner, .. } => collect_pattern_names(inner, out),
        HirPatKind::Literal(_)
        | HirPatKind::Wildcard
        | HirPatKind::Rest
        | HirPatKind::Range { .. } => {}
    }
}

fn is_closed<S: std::hash::BuildHasher + Clone>(
    expr: &HirExpr,
    bound: &HashSet<String, S>,
) -> bool {
    match &expr.kind {
        HirExprKind::Path { segments, def } => {
            // Fully-qualified paths and resolved DefIds point to top-
            // level items — treat those as "closed" (not captures).
            if def.is_some() || segments.len() > 1 {
                return true;
            }
            if let Some(first) = segments.first() {
                return bound.contains(&first.name);
            }
            true
        }
        HirExprKind::Literal(_) | HirExprKind::Continue | HirExprKind::Placeholder => true,
        HirExprKind::Return(inner) | HirExprKind::Break(inner) => {
            inner.as_ref().is_none_or(|e| is_closed(e, bound))
        }
        HirExprKind::Call { callee, args } => {
            is_closed(callee, bound) && args.iter().all(|a| is_closed(a, bound))
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            is_closed(receiver, bound) && args.iter().all(|a| is_closed(a, bound))
        }
        HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
            is_closed(receiver, bound)
        }
        HirExprKind::Index { base, index } => is_closed(base, bound) && is_closed(index, bound),
        HirExprKind::Unary { operand, .. } => is_closed(operand, bound),
        HirExprKind::Binary { lhs, rhs, .. } => is_closed(lhs, bound) && is_closed(rhs, bound),
        HirExprKind::Assign { place, value } => is_closed(place, bound) && is_closed(value, bound),
        HirExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            is_closed(condition, bound)
                && is_closed(then_branch, bound)
                && else_branch.as_ref().is_none_or(|e| is_closed(e, bound))
        }
        HirExprKind::Match { scrutinee, arms } => {
            if !is_closed(scrutinee, bound) {
                return false;
            }
            for arm in arms {
                let mut arm_bound = bound.clone();
                collect_pattern_names(&arm.pattern, &mut arm_bound);
                if let Some(guard) = &arm.guard {
                    if !is_closed(guard, &arm_bound) {
                        return false;
                    }
                }
                if !is_closed(&arm.body, &arm_bound) {
                    return false;
                }
            }
            true
        }
        HirExprKind::Loop { body } | HirExprKind::While { body, .. } => is_closed(body, bound),
        HirExprKind::Block(block) => is_closed_block(block, bound),
        HirExprKind::Closure { params, body, .. } => {
            let mut inner_bound = bound.clone();
            for param in params {
                collect_pattern_names(&param.pattern, &mut inner_bound);
            }
            is_closed(body, &inner_bound)
        }
        HirExprKind::LiftedClosure { captures, .. } => captures.iter().all(|c| is_closed(c, bound)),
        HirExprKind::Select { arms } => arms.iter().all(|arm| {
            let ops_closed = match &arm.op {
                crate::tree::HirSelectOp::Recv { channel, .. } => is_closed(channel, bound),
                crate::tree::HirSelectOp::Send { channel, value } => {
                    is_closed(channel, bound) && is_closed(value, bound)
                }
                crate::tree::HirSelectOp::Default => true,
            };
            ops_closed && is_closed(&arm.body, bound)
        }),
        HirExprKind::Tuple(elems) => elems.iter().all(|e| is_closed(e, bound)),
        HirExprKind::Array(HirArrayExpr::List(elems)) => elems.iter().all(|e| is_closed(e, bound)),
        HirExprKind::Array(HirArrayExpr::Repeat { value, count }) => {
            is_closed(value, bound) && is_closed(count, bound)
        }
        HirExprKind::Cast { value, .. } => is_closed(value, bound),
        HirExprKind::Range { start, end, .. } => {
            start.as_ref().is_none_or(|s| is_closed(s, bound))
                && end.as_ref().is_none_or(|e| is_closed(e, bound))
        }
        HirExprKind::Go(inner) => is_closed(inner, bound),
    }
}

/// Collects the free variables referenced by `expr` that are not in
/// `bound`. Variables appear in first-use order (each distinct name
/// shows up exactly once). Used by the lifter to produce a stable
/// capture ordering, and by the tree-walking interpreter so closures
/// capture only the bindings they actually reference (instead of
/// the full enclosing scope).
#[must_use]
pub fn collect_free_vars<S: std::hash::BuildHasher + Clone>(
    expr: &HirExpr,
    bound: &HashSet<String, S>,
) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    walk_free(expr, bound, &mut out, &mut seen);
    out
}

fn walk_free<S: std::hash::BuildHasher + Clone>(
    expr: &HirExpr,
    bound: &HashSet<String, S>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    match &expr.kind {
        HirExprKind::Path { segments, def } => {
            if def.is_some() || segments.len() > 1 {
                return;
            }
            if let Some(first) = segments.first() {
                if !bound.contains(&first.name) && seen.insert(first.name.clone()) {
                    out.push(first.name.clone());
                }
            }
        }
        HirExprKind::Literal(_) | HirExprKind::Continue | HirExprKind::Placeholder => {}
        HirExprKind::Return(inner) | HirExprKind::Break(inner) => {
            if let Some(e) = inner {
                walk_free(e, bound, out, seen);
            }
        }
        HirExprKind::Call { callee, args } => {
            walk_free(callee, bound, out, seen);
            for a in args {
                walk_free(a, bound, out, seen);
            }
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            walk_free(receiver, bound, out, seen);
            for a in args {
                walk_free(a, bound, out, seen);
            }
        }
        HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
            walk_free(receiver, bound, out, seen);
        }
        HirExprKind::Index { base, index } => {
            walk_free(base, bound, out, seen);
            walk_free(index, bound, out, seen);
        }
        HirExprKind::Unary { operand, .. } => walk_free(operand, bound, out, seen),
        HirExprKind::Binary { lhs, rhs, .. } => {
            walk_free(lhs, bound, out, seen);
            walk_free(rhs, bound, out, seen);
        }
        HirExprKind::Assign { place, value } => {
            walk_free(place, bound, out, seen);
            walk_free(value, bound, out, seen);
        }
        HirExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            walk_free(condition, bound, out, seen);
            walk_free(then_branch, bound, out, seen);
            if let Some(e) = else_branch {
                walk_free(e, bound, out, seen);
            }
        }
        HirExprKind::Match { scrutinee, arms } => {
            walk_free(scrutinee, bound, out, seen);
            for arm in arms {
                let mut arm_bound = bound.clone();
                collect_pattern_names(&arm.pattern, &mut arm_bound);
                if let Some(g) = &arm.guard {
                    walk_free_with(g, &arm_bound, out, seen);
                }
                walk_free_with(&arm.body, &arm_bound, out, seen);
            }
        }
        HirExprKind::Loop { body } | HirExprKind::While { body, .. } => {
            walk_free(body, bound, out, seen);
        }
        HirExprKind::Block(block) => walk_free_block(block, bound, out, seen),
        HirExprKind::Closure { params, body, .. } => {
            let mut inner_bound = bound.clone();
            for p in params {
                collect_pattern_names(&p.pattern, &mut inner_bound);
            }
            walk_free_with(body, &inner_bound, out, seen);
        }
        HirExprKind::LiftedClosure { captures, .. } => {
            for c in captures {
                walk_free(c, bound, out, seen);
            }
        }
        HirExprKind::Select { arms } => {
            for arm in arms {
                match &arm.op {
                    crate::tree::HirSelectOp::Recv { channel, .. } => {
                        walk_free(channel, bound, out, seen);
                    }
                    crate::tree::HirSelectOp::Send { channel, value } => {
                        walk_free(channel, bound, out, seen);
                        walk_free(value, bound, out, seen);
                    }
                    crate::tree::HirSelectOp::Default => {}
                }
                walk_free(&arm.body, bound, out, seen);
            }
        }
        HirExprKind::Tuple(elems) | HirExprKind::Array(HirArrayExpr::List(elems)) => {
            for e in elems {
                walk_free(e, bound, out, seen);
            }
        }
        HirExprKind::Array(HirArrayExpr::Repeat { value, count }) => {
            walk_free(value, bound, out, seen);
            walk_free(count, bound, out, seen);
        }
        HirExprKind::Cast { value, .. } => walk_free(value, bound, out, seen),
        HirExprKind::Range { start, end, .. } => {
            if let Some(s) = start {
                walk_free(s, bound, out, seen);
            }
            if let Some(e) = end {
                walk_free(e, bound, out, seen);
            }
        }
        HirExprKind::Go(inner) => walk_free(inner, bound, out, seen),
    }
}

fn walk_free_with<S: std::hash::BuildHasher + Clone>(
    expr: &HirExpr,
    bound: &HashSet<String, S>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    walk_free(expr, bound, out, seen);
}

fn walk_free_block<S: std::hash::BuildHasher + Clone>(
    block: &HirBlock,
    bound: &HashSet<String, S>,
    out: &mut Vec<String>,
    seen: &mut HashSet<String>,
) {
    let mut local = bound.clone();
    for stmt in &block.stmts {
        match &stmt.kind {
            HirStmtKind::Let { pattern, init, .. } => {
                if let Some(e) = init {
                    walk_free(e, &local, out, seen);
                }
                collect_pattern_names(pattern, &mut local);
            }
            HirStmtKind::Expr { expr, .. } | HirStmtKind::Go(expr) | HirStmtKind::Defer(expr) => {
                walk_free(expr, &local, out, seen);
            }
            HirStmtKind::Item(_) => {}
        }
    }
    if let Some(tail) = &block.tail {
        walk_free(tail, &local, out, seen);
    }
}

fn is_closed_block<S: std::hash::BuildHasher + Clone>(
    block: &HirBlock,
    bound: &HashSet<String, S>,
) -> bool {
    let mut local = bound.clone();
    for stmt in &block.stmts {
        match &stmt.kind {
            HirStmtKind::Let { pattern, init, .. } => {
                if let Some(init) = init {
                    if !is_closed(init, &local) {
                        return false;
                    }
                }
                collect_pattern_names(pattern, &mut local);
            }
            HirStmtKind::Expr { expr, .. } | HirStmtKind::Go(expr) | HirStmtKind::Defer(expr) => {
                if !is_closed(expr, &local) {
                    return false;
                }
            }
            HirStmtKind::Item(_) => {}
        }
    }
    block
        .tail
        .as_ref()
        .is_none_or(|tail| is_closed(tail, &local))
}
