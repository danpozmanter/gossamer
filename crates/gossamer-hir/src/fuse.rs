//! Fuses `iter::` free-form combinator pipelines over integer ranges
//! into a single streaming loop, with each stage/terminal closure
//! inlined at its use site. A chain such as
//!
//! ```text
//! iter::range_inclusive(1, n) |> iter::filter(|k| k % 2 == 0, $) |> iter::sum_by(|k| k * k, $)
//! ```
//!
//! otherwise materialises the whole range as a `Vec`, then a second
//! `Vec` of survivors, then reduces - three passes and two large
//! allocations. This pass rewrites it to the same tight accumulator loop
//! a hand-written `for` would produce: no intermediate `Vec`, no
//! per-element indirect closure call. Because it runs on the shared HIR
//! before closure lifting and before every backend lowers, all three
//! tiers (bytecode VM, Cranelift JIT, LLVM AOT) get the fused loop.
//!
//! Recognition is conservative: only integer-range sources, `filter` /
//! `map` stages, and a fixed set of terminals, with inline single-shape
//! closures throughout. Anything else is left untouched and lowered by
//! the existing combinator path - correct, just unfused.

use gossamer_ast::Ident;
use gossamer_lex::Span;
use gossamer_types::{IntTy, Ty, TyCtxt, TyKind};

use crate::ids::HirIdGenerator;
use crate::tree::{
    HirArrayExpr, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirParam, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};

/// Rewrites every recognised `iter::` range pipeline in `program` into a
/// fused loop.
pub fn fuse_iter_pipelines(program: &mut HirProgram, tcx: &mut TyCtxt, ids: &mut HirIdGenerator) {
    let mut fuser = Fuser { tcx, ids, temp: 0 };
    for item in &mut program.items {
        fuser.visit_item(item);
    }
}

struct Fuser<'a> {
    tcx: &'a mut TyCtxt,
    ids: &'a mut HirIdGenerator,
    temp: u32,
}

/// A `filter` / `map` stage between the range source and the terminal.
enum Stage {
    /// `iter::filter(pred)` - keep the element when the predicate holds.
    Filter(HirExpr),
    /// `iter::map(f)` - replace the element with `f(element)`.
    Map(HirExpr),
}

/// The reducing operation that ends a pipeline.
enum Terminal {
    Sum,
    SumBy(HirExpr),
    Count,
    Product,
    ProductBy(HirExpr),
    Fold(HirExpr, HirExpr),
    ForEach(HirExpr),
    Any(HirExpr),
    All(HirExpr),
}

/// A recognised, fusable pipeline. Every `HirExpr` is a clone owned by
/// the plan so the builder can move it into the loop.
struct Plan {
    start: HirExpr,
    end: HirExpr,
    inclusive: bool,
    stages: Vec<Stage>,
    terminal: Terminal,
    /// Accumulator / result type of the whole pipeline.
    result_ty: Ty,
}

impl Fuser<'_> {
    fn visit_item(&mut self, item: &mut HirItem) {
        match &mut item.kind {
            HirItemKind::Fn(f) => self.visit_fn(f),
            HirItemKind::Impl(imp) => {
                for m in &mut imp.methods {
                    self.visit_fn(m);
                }
            }
            HirItemKind::Trait(t) => {
                for m in &mut t.methods {
                    self.visit_fn(m);
                }
            }
            HirItemKind::Const(c) => self.visit_expr(&mut c.value),
            HirItemKind::Static(s) => self.visit_expr(&mut s.value),
            HirItemKind::Adt(_) => {}
        }
    }

    fn visit_fn(&mut self, f: &mut HirFn) {
        if let Some(body) = &mut f.body {
            self.walk_block(&mut body.block);
        }
    }

    /// Post-order walk: fuse nested pipelines (including inside closure
    /// bodies) first, then attempt to fuse this node.
    fn visit_expr(&mut self, expr: &mut HirExpr) {
        self.walk_children(expr);
        if let Some(plan) = self.plan(expr) {
            let span = expr.span;
            *expr = self.build(plan, span);
        }
    }

    // One arm per HIR expression variant; the length is the variant count.
    // Splitting it would scatter the exhaustive-visitor structure that keeps
    // every child edge visible in one place.
    #[allow(clippy::too_many_lines)]
    fn walk_children(&mut self, expr: &mut HirExpr) {
        match &mut expr.kind {
            HirExprKind::Literal(_)
            | HirExprKind::Path { .. }
            | HirExprKind::LiftedClosure { .. }
            | HirExprKind::Continue { .. }
            | HirExprKind::Placeholder => {}
            HirExprKind::Call { callee, args } => {
                self.visit_expr(callee);
                for a in args {
                    self.visit_expr(a);
                }
            }
            HirExprKind::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver);
                for a in args {
                    self.visit_expr(a);
                }
            }
            HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
                self.visit_expr(receiver);
            }
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
                if let Some(e) = else_branch {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.visit_expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &mut arm.guard {
                        self.visit_expr(g);
                    }
                    self.visit_expr(&mut arm.body);
                }
            }
            HirExprKind::Loop { body, .. } | HirExprKind::Go(body) => self.visit_expr(body),
            HirExprKind::While {
                condition, body, ..
            } => {
                self.visit_expr(condition);
                self.visit_expr(body);
            }
            HirExprKind::Block(block) => self.walk_block(block),
            HirExprKind::Closure { body, .. } => self.visit_expr(body),
            HirExprKind::Select { arms } => {
                for arm in arms {
                    self.visit_expr(&mut arm.body);
                }
            }
            HirExprKind::Return(v) => {
                if let Some(e) = v {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Break { value, .. } => {
                if let Some(e) = value {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Tuple(items) => {
                for e in items {
                    self.visit_expr(e);
                }
            }
            HirExprKind::Array(arr) => match arr {
                HirArrayExpr::List(items) => {
                    for e in items {
                        self.visit_expr(e);
                    }
                }
                HirArrayExpr::Repeat { value, count } => {
                    self.visit_expr(value);
                    self.visit_expr(count);
                }
            },
            HirExprKind::Cast { value, .. } => self.visit_expr(value),
            HirExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.visit_expr(s);
                }
                if let Some(e) = end {
                    self.visit_expr(e);
                }
            }
        }
    }

    fn walk_block(&mut self, block: &mut HirBlock) {
        for stmt in &mut block.stmts {
            match &mut stmt.kind {
                HirStmtKind::Let { init, .. } => {
                    if let Some(e) = init {
                        self.visit_expr(e);
                    }
                }
                HirStmtKind::Expr { expr, .. }
                | HirStmtKind::Defer(expr)
                | HirStmtKind::Go(expr) => self.visit_expr(expr),
                HirStmtKind::Item(item) => self.visit_item(item),
            }
        }
        if let Some(tail) = &mut block.tail {
            self.visit_expr(tail);
        }
    }

    // ----- recognition -----

    fn plan(&mut self, expr: &HirExpr) -> Option<Plan> {
        let (name, args) = as_iter_call(expr)?;
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let bool_ty = self.tcx.bool_ty();
        let unit_ty = self.tcx.unit_interned()?;

        // Terminal + its source arg (always the last argument - the
        // combinators are data-last).
        let (terminal, source) = match (name, args.len()) {
            ("sum", 1) => (Terminal::Sum, &args[0]),
            ("count", 1) => (Terminal::Count, &args[0]),
            ("product", 1) => (Terminal::Product, &args[0]),
            ("sum_by", 2) => (Terminal::SumBy(one_closure(&args[0])?.clone()), &args[1]),
            ("product_by", 2) => (
                Terminal::ProductBy(one_closure(&args[0])?.clone()),
                &args[1],
            ),
            ("for_each", 2) => (Terminal::ForEach(one_closure(&args[0])?.clone()), &args[1]),
            ("any", 2) => (Terminal::Any(one_closure(&args[0])?.clone()), &args[1]),
            ("all", 2) => (Terminal::All(one_closure(&args[0])?.clone()), &args[1]),
            ("fold", 3) => (
                Terminal::Fold(args[0].clone(), n_closure(&args[1], 2)?.clone()),
                &args[2],
            ),
            _ => return None,
        };

        // The result type fixes the accumulator's type; only the i64 /
        // bool / unit shapes are fused.
        let result_ty = match &terminal {
            Terminal::Any(_) | Terminal::All(_) => bool_ty,
            Terminal::ForEach(_) => unit_ty,
            _ => {
                if !is_i64(self.tcx, expr.ty) {
                    return None;
                }
                i64_ty
            }
        };

        // Peel `filter` / `map` stages down to the range source.
        let mut stages_rev = Vec::new();
        let mut cur = source;
        let (start, end, inclusive) = loop {
            let (sname, sargs) = as_iter_call(cur)?;
            match (sname, sargs.len()) {
                ("filter", 2) => {
                    stages_rev.push(Stage::Filter(one_closure(&sargs[0])?.clone()));
                    cur = &sargs[1];
                }
                ("map", 2) => {
                    if !map_closure_returns_i64(self.tcx, &sargs[0]) {
                        return None;
                    }
                    stages_rev.push(Stage::Map(one_closure(&sargs[0])?.clone()));
                    cur = &sargs[1];
                }
                ("range", 2) => break (sargs[0].clone(), sargs[1].clone(), false),
                ("range_inclusive", 2) => break (sargs[0].clone(), sargs[1].clone(), true),
                _ => return None,
            }
        };

        if !is_i64(self.tcx, start.ty) || !is_i64(self.tcx, end.ty) {
            return None;
        }

        stages_rev.reverse();
        Some(Plan {
            start,
            end,
            inclusive,
            stages: stages_rev,
            terminal,
            result_ty,
        })
    }

    // ----- construction -----

    fn build(&mut self, plan: Plan, span: Span) -> HirExpr {
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let unit_ty = self.tcx.unit();
        let counter = self.temp_name("i");
        let end_name = self.temp_name("end");
        let acc = self.temp_name("acc");
        let has_acc = !matches!(plan.terminal, Terminal::ForEach(_));

        let Plan {
            start,
            end,
            inclusive,
            stages,
            terminal,
            result_ty,
        } = plan;

        let mut stmts: Vec<HirStmt> = Vec::new();
        if has_acc {
            let init = self.acc_init(&terminal, result_ty, span);
            let s = self.let_stmt(&acc, true, result_ty, init, span);
            stmts.push(s);
        }
        let s = self.let_stmt(&counter, true, i64_ty, start, span);
        stmts.push(s);
        let s = self.let_stmt(&end_name, false, i64_ty, end, span);
        stmts.push(s);

        let while_expr = self.build_while(
            &counter, &end_name, inclusive, &stages, &terminal, &acc, span,
        );
        let s = self.expr_stmt(while_expr, span);
        stmts.push(s);

        let (tail, block_ty) = if has_acc {
            (Some(self.path(&acc, result_ty, span)), result_ty)
        } else {
            (None, unit_ty)
        };
        self.block(stmts, tail, block_ty, span)
    }

    /// Builds `while <counter> <=/< <end> { <stages/terminal>; counter += 1 }`.
    #[allow(clippy::too_many_arguments)]
    fn build_while(
        &mut self,
        counter: &str,
        end_name: &str,
        inclusive: bool,
        stages: &[Stage],
        terminal: &Terminal,
        acc: &str,
        span: Span,
    ) -> HirExpr {
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let unit_ty = self.tcx.unit();
        let bool_ty = self.tcx.bool_ty();

        let elem = self.path(counter, i64_ty, span);
        let mut body_stmts = self.build_body(stages, 0, elem, terminal, acc, span);
        let ctr_path = self.path(counter, i64_ty, span);
        let one = self.int_lit(1, i64_ty, span);
        let inc = self.binary(HirBinaryOp::Add, ctr_path, one, i64_ty, span);
        let ctr_place = self.path(counter, i64_ty, span);
        let bump = self.assign_stmt(ctr_place, inc, span);
        body_stmts.push(bump);
        let body_block = self.block(body_stmts, None, unit_ty, span);

        let cmp_op = if inclusive {
            HirBinaryOp::Le
        } else {
            HirBinaryOp::Lt
        };
        let lhs = self.path(counter, i64_ty, span);
        let rhs = self.path(end_name, i64_ty, span);
        let cond = self.binary(cmp_op, lhs, rhs, bool_ty, span);
        self.expr(
            unit_ty,
            span,
            HirExprKind::While {
                condition: Box::new(cond),
                body: Box::new(body_block),
                label: None,
            },
        )
    }

    fn build_body(
        &mut self,
        stages: &[Stage],
        i: usize,
        elem: HirExpr,
        terminal: &Terminal,
        acc: &str,
        span: Span,
    ) -> Vec<HirStmt> {
        if i == stages.len() {
            return self.terminal_stmts(terminal, elem, acc, span);
        }
        match &stages[i] {
            Stage::Filter(pred) => {
                let elem_for_pred = self.reclone(&elem);
                let cond = self.inline1(pred, elem_for_pred, span);
                let rest = self.build_body(stages, i + 1, elem, terminal, acc, span);
                let unit_ty = self.tcx.unit();
                let rest_block = self.block(rest, None, unit_ty, span);
                let if_expr = self.expr(
                    unit_ty,
                    span,
                    HirExprKind::If {
                        condition: Box::new(cond),
                        then_branch: Box::new(rest_block),
                        else_branch: None,
                    },
                );
                let stmt = self.expr_stmt(if_expr, span);
                vec![stmt]
            }
            Stage::Map(f) => {
                let i64_ty = self.tcx.int_ty(IntTy::I64);
                let vname = self.temp_name("v");
                let mapped = self.inline1(f, elem, span);
                let let_v = self.let_stmt(&vname, false, i64_ty, mapped, span);
                let next_elem = self.path(&vname, i64_ty, span);
                let mut out = vec![let_v];
                out.extend(self.build_body(stages, i + 1, next_elem, terminal, acc, span));
                out
            }
        }
    }

    fn terminal_stmts(
        &mut self,
        terminal: &Terminal,
        elem: HirExpr,
        acc: &str,
        span: Span,
    ) -> Vec<HirStmt> {
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        match terminal {
            Terminal::Sum => vec![self.acc_op(acc, elem, HirBinaryOp::Add, i64_ty, span)],
            Terminal::Product => vec![self.acc_op(acc, elem, HirBinaryOp::Mul, i64_ty, span)],
            Terminal::Count => {
                let one = self.int_lit(1, i64_ty, span);
                vec![self.acc_op(acc, one, HirBinaryOp::Add, i64_ty, span)]
            }
            Terminal::SumBy(f) => {
                let mapped = self.inline1(f, elem, span);
                vec![self.acc_op(acc, mapped, HirBinaryOp::Add, i64_ty, span)]
            }
            Terminal::ProductBy(f) => {
                let mapped = self.inline1(f, elem, span);
                vec![self.acc_op(acc, mapped, HirBinaryOp::Mul, i64_ty, span)]
            }
            Terminal::Fold(_, f) => {
                let acc_expr = self.path(acc, i64_ty, span);
                let folded = self.inline2(f, acc_expr, elem, span);
                let place = self.path(acc, i64_ty, span);
                vec![self.assign_stmt(place, folded, span)]
            }
            Terminal::ForEach(f) => {
                let call = self.inline1(f, elem, span);
                vec![self.expr_stmt(call, span)]
            }
            Terminal::Any(p) => self.short_circuit(p, elem, acc, true, span),
            Terminal::All(p) => self.short_circuit(p, elem, acc, false, span),
        }
    }

    /// `if [!]pred(elem) { acc = <target>; break }` for `any` / `all`.
    fn short_circuit(
        &mut self,
        pred: &HirExpr,
        elem: HirExpr,
        acc: &str,
        is_any: bool,
        span: Span,
    ) -> Vec<HirStmt> {
        let bool_ty = self.tcx.bool_ty();
        let unit_ty = self.tcx.unit();
        let mut cond = self.inline1(pred, elem, span);
        if !is_any {
            cond = self.expr(
                bool_ty,
                span,
                HirExprKind::Unary {
                    op: HirUnaryOp::Not,
                    operand: Box::new(cond),
                },
            );
        }
        let target = self.bool_lit(is_any, span);
        let place = self.path(acc, bool_ty, span);
        let set = self.assign_stmt(place, target, span);
        let brk_expr = self.expr(
            unit_ty,
            span,
            HirExprKind::Break {
                value: None,
                label: None,
            },
        );
        let brk = self.expr_stmt(brk_expr, span);
        let then = self.block(vec![set, brk], None, unit_ty, span);
        let if_expr = self.expr(
            unit_ty,
            span,
            HirExprKind::If {
                condition: Box::new(cond),
                then_branch: Box::new(then),
                else_branch: None,
            },
        );
        vec![self.expr_stmt(if_expr, span)]
    }

    fn acc_init(&mut self, terminal: &Terminal, ty: Ty, span: Span) -> HirExpr {
        match terminal {
            Terminal::Any(_) => self.bool_lit(false, span),
            Terminal::All(_) => self.bool_lit(true, span),
            Terminal::Product | Terminal::ProductBy(_) => self.int_lit(1, ty, span),
            Terminal::Fold(init, _) => init.clone(),
            _ => self.int_lit(0, ty, span),
        }
    }

    /// `acc = acc <op> value`
    fn acc_op(
        &mut self,
        acc: &str,
        value: HirExpr,
        op: HirBinaryOp,
        ty: Ty,
        span: Span,
    ) -> HirStmt {
        let acc_read = self.path(acc, ty, span);
        let new = self.binary(op, acc_read, value, ty, span);
        let place = self.path(acc, ty, span);
        self.assign_stmt(place, new, span)
    }

    /// Inlines a single-argument closure applied to `arg` as
    /// `{ let <param> = arg; <body> }`. `lower_path` resolves locals by
    /// name, so the cloned body's references to the parameter bind to the
    /// `let`; captured names stay resolved in the enclosing scope.
    fn inline1(&mut self, closure: &HirExpr, arg: HirExpr, span: Span) -> HirExpr {
        let HirExprKind::Closure { params, body, .. } = &closure.kind else {
            return arg;
        };
        let body = (**body).clone();
        let ty = body.ty;
        let stmt = self.param_let(&params[0], arg, span);
        self.block(vec![stmt], Some(body), ty, span)
    }

    fn inline2(&mut self, closure: &HirExpr, a: HirExpr, b: HirExpr, span: Span) -> HirExpr {
        let HirExprKind::Closure { params, body, .. } = &closure.kind else {
            return b;
        };
        let body = (**body).clone();
        let ty = body.ty;
        let s0 = self.param_let(&params[0], a, span);
        let s1 = self.param_let(&params[1], b, span);
        self.block(vec![s0, s1], Some(body), ty, span)
    }

    fn param_let(&mut self, param: &HirParam, init: HirExpr, span: Span) -> HirStmt {
        let name = match &param.pattern.kind {
            HirPatKind::Binding { name, .. } => name.name.clone(),
            _ => "_".to_string(),
        };
        self.let_stmt(&name, false, param.ty, init, span)
    }

    // ----- node constructors -----

    fn expr(&mut self, ty: Ty, span: Span, kind: HirExprKind) -> HirExpr {
        HirExpr {
            id: self.ids.next(),
            span,
            ty,
            kind,
        }
    }

    fn int_lit(&mut self, v: i64, ty: Ty, span: Span) -> HirExpr {
        self.expr(
            ty,
            span,
            HirExprKind::Literal(HirLiteral::Int(v.to_string())),
        )
    }

    fn bool_lit(&mut self, v: bool, span: Span) -> HirExpr {
        let ty = self.tcx.bool_ty();
        self.expr(ty, span, HirExprKind::Literal(HirLiteral::Bool(v)))
    }

    fn path(&mut self, name: &str, ty: Ty, span: Span) -> HirExpr {
        self.expr(
            ty,
            span,
            HirExprKind::Path {
                segments: vec![Ident::new(name)],
                def: None,
            },
        )
    }

    fn binary(
        &mut self,
        op: HirBinaryOp,
        lhs: HirExpr,
        rhs: HirExpr,
        ty: Ty,
        span: Span,
    ) -> HirExpr {
        self.expr(
            ty,
            span,
            HirExprKind::Binary {
                op,
                lhs: Box::new(lhs),
                rhs: Box::new(rhs),
            },
        )
    }

    fn assign_stmt(&mut self, place: HirExpr, value: HirExpr, span: Span) -> HirStmt {
        let unit = self.tcx.unit();
        let assign = self.expr(
            unit,
            span,
            HirExprKind::Assign {
                place: Box::new(place),
                value: Box::new(value),
            },
        );
        self.expr_stmt(assign, span)
    }

    fn expr_stmt(&mut self, expr: HirExpr, span: Span) -> HirStmt {
        HirStmt {
            id: self.ids.next(),
            span,
            kind: HirStmtKind::Expr {
                expr,
                has_semi: true,
            },
        }
    }

    fn let_stmt(
        &mut self,
        name: &str,
        mutable: bool,
        ty: Ty,
        init: HirExpr,
        span: Span,
    ) -> HirStmt {
        let pat = HirPat {
            id: self.ids.next(),
            span,
            ty,
            kind: HirPatKind::Binding {
                name: Ident::new(name),
                mutable,
            },
        };
        HirStmt {
            id: self.ids.next(),
            span,
            kind: HirStmtKind::Let {
                pattern: pat,
                ty,
                init: Some(init),
            },
        }
    }

    fn block(&mut self, stmts: Vec<HirStmt>, tail: Option<HirExpr>, ty: Ty, span: Span) -> HirExpr {
        let block = HirBlock {
            id: self.ids.next(),
            span,
            stmts,
            tail: tail.map(Box::new),
            ty,
            is_comptime: false,
        };
        self.expr(ty, span, HirExprKind::Block(block))
    }

    fn temp_name(&mut self, tag: &str) -> String {
        let n = self.temp;
        self.temp += 1;
        format!("__fuse_{tag}_{n}")
    }

    /// A fresh clone of an element path so it can be used at more than one
    /// site.
    fn reclone(&mut self, e: &HirExpr) -> HirExpr {
        let mut c = e.clone();
        c.id = self.ids.next();
        c
    }
}

/// Matches `iter::<name>(args...)`, returning the trailing name and args.
fn as_iter_call(expr: &HirExpr) -> Option<(&str, &[HirExpr])> {
    let HirExprKind::Call { callee, args } = &expr.kind else {
        return None;
    };
    let HirExprKind::Path { segments, .. } = &callee.kind else {
        return None;
    };
    if segments.len() != 2 || segments[0].name != "iter" {
        return None;
    }
    Some((segments[1].name.as_str(), args.as_slice()))
}

/// Accepts an inline single-parameter closure with a plain binding
/// parameter, the only shape the inliner can splice.
fn one_closure(expr: &HirExpr) -> Option<&HirExpr> {
    n_closure(expr, 1)
}

fn n_closure(expr: &HirExpr, n: usize) -> Option<&HirExpr> {
    let HirExprKind::Closure { params, body, .. } = &expr.kind else {
        return None;
    };
    if params.len() != n {
        return None;
    }
    if params
        .iter()
        .any(|p| !matches!(p.pattern.kind, HirPatKind::Binding { .. }))
    {
        return None;
    }
    // Splicing the body into the loop must not move control flow that
    // targets the closure out to the enclosing function or the fused
    // loop: a `return` (also what `?` desugars to) or a loop-level
    // `break` / `continue` disqualifies it.
    if !inline_safe(body, 0) {
        return None;
    }
    Some(expr)
}

/// Whether `expr` can be spliced inline without changing which construct
/// its control flow targets. `loop_depth` counts loops entered *within*
/// the body, so a `break` inside a nested loop is local and safe. Nested
/// closures are opaque boundaries - their control flow stays local.
fn inline_safe(expr: &HirExpr, loop_depth: u32) -> bool {
    match &expr.kind {
        HirExprKind::Return(_) => false,
        HirExprKind::Break { .. } | HirExprKind::Continue { .. } => loop_depth > 0,
        HirExprKind::Closure { .. } | HirExprKind::LiftedClosure { .. } => true,
        HirExprKind::Literal(_) | HirExprKind::Path { .. } | HirExprKind::Placeholder => true,
        HirExprKind::Call { callee, args } => {
            inline_safe(callee, loop_depth) && args.iter().all(|a| inline_safe(a, loop_depth))
        }
        HirExprKind::MethodCall { receiver, args, .. } => {
            inline_safe(receiver, loop_depth) && args.iter().all(|a| inline_safe(a, loop_depth))
        }
        HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
            inline_safe(receiver, loop_depth)
        }
        HirExprKind::Index { base, index } => {
            inline_safe(base, loop_depth) && inline_safe(index, loop_depth)
        }
        HirExprKind::Unary { operand, .. } => inline_safe(operand, loop_depth),
        HirExprKind::Binary { lhs, rhs, .. } => {
            inline_safe(lhs, loop_depth) && inline_safe(rhs, loop_depth)
        }
        HirExprKind::Assign { place, value } => {
            inline_safe(place, loop_depth) && inline_safe(value, loop_depth)
        }
        HirExprKind::Cast { value, .. } => inline_safe(value, loop_depth),
        HirExprKind::If {
            condition,
            then_branch,
            else_branch,
        } => {
            inline_safe(condition, loop_depth)
                && inline_safe(then_branch, loop_depth)
                && else_branch
                    .as_ref()
                    .is_none_or(|e| inline_safe(e, loop_depth))
        }
        HirExprKind::Match { scrutinee, arms } => {
            inline_safe(scrutinee, loop_depth)
                && arms.iter().all(|a| {
                    a.guard.as_ref().is_none_or(|g| inline_safe(g, loop_depth))
                        && inline_safe(&a.body, loop_depth)
                })
        }
        HirExprKind::Loop { body, .. } => inline_safe(body, loop_depth + 1),
        HirExprKind::While {
            condition, body, ..
        } => inline_safe(condition, loop_depth) && inline_safe(body, loop_depth + 1),
        HirExprKind::Block(b) => {
            b.stmts.iter().all(|s| match &s.kind {
                HirStmtKind::Let { init, .. } => {
                    init.as_ref().is_none_or(|e| inline_safe(e, loop_depth))
                }
                HirStmtKind::Expr { expr, .. }
                | HirStmtKind::Defer(expr)
                | HirStmtKind::Go(expr) => inline_safe(expr, loop_depth),
                HirStmtKind::Item(_) => true,
            }) && b.tail.as_ref().is_none_or(|t| inline_safe(t, loop_depth))
        }
        HirExprKind::Tuple(items) => items.iter().all(|e| inline_safe(e, loop_depth)),
        HirExprKind::Array(HirArrayExpr::List(items)) => {
            items.iter().all(|e| inline_safe(e, loop_depth))
        }
        HirExprKind::Array(HirArrayExpr::Repeat { value, count }) => {
            inline_safe(value, loop_depth) && inline_safe(count, loop_depth)
        }
        HirExprKind::Range { start, end, .. } => {
            start.as_ref().is_none_or(|e| inline_safe(e, loop_depth))
                && end.as_ref().is_none_or(|e| inline_safe(e, loop_depth))
        }
        // Spawning / selecting inside a fused inline body is outside the
        // shapes this pass reasons about; be conservative.
        HirExprKind::Select { .. } | HirExprKind::Go(_) => false,
    }
}

fn is_i64(tcx: &TyCtxt, ty: Ty) -> bool {
    matches!(tcx.kind_of(ty), TyKind::Int(IntTy::I64))
}

fn map_closure_returns_i64(tcx: &TyCtxt, closure: &HirExpr) -> bool {
    if let HirExprKind::Closure { body, .. } = &closure.kind {
        is_i64(tcx, body.ty)
    } else {
        false
    }
}
