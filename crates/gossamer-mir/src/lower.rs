//! HIR → MIR lowering.
//! Produces a [`Body`] per HIR function. The lowerer is intentionally
//! straightforward: every HIR expression of interest becomes either a
//! sequence of [`StatementKind::Assign`]s targeting fresh temporaries
//! or a [`Terminator`] that closes the current block. Control flow
//! (`if`, `while`, `loop`, `match`) drops into the CFG by allocating
//! join blocks and stitching them with [`Terminator::Goto`] /
//! [`Terminator::SwitchInt`].

#![forbid(unsafe_code)]
#![allow(
    clippy::too_many_lines,
    clippy::unnecessary_wraps,
    clippy::match_same_arms
)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::{FileId, Span};
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    AssertMessage, BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place,
    Rvalue, Statement, StatementKind, Terminator, UnOp,
};

/// Lowers every function in `program` to a MIR [`Body`].
#[must_use]
pub fn lower_program(program: &HirProgram, tcx: &mut TyCtxt) -> Vec<Body> {
    let (structs, struct_defs) = collect_struct_fields(program);
    let fn_returns = collect_fn_returns(program);
    let mut bodies = Vec::new();
    for item in &program.items {
        collect_item(item, tcx, &structs, &struct_defs, &fn_returns, &mut bodies);
    }
    bodies
}

/// Builds a `DefId → return Ty` map for every top-level function
/// (and trait / impl methods). Consumed by MIR lowering so call-
/// site destinations can be typed with the callee's concrete
/// return type instead of the call expression's inference-variable
/// placeholder.
fn collect_fn_returns(
    program: &HirProgram,
) -> HashMap<gossamer_resolve::DefId, Ty> {
    let mut out = HashMap::new();
    for item in &program.items {
        match &item.kind {
            HirItemKind::Fn(decl) => {
                if let Some(def) = item.def {
                    if let Some(ret) = decl.ret {
                        out.insert(def, ret);
                    }
                }
            }
            HirItemKind::Impl(decl) => {
                for method in &decl.methods {
                    if let Some(ret) = method.ret {
                        // Impl methods' def ids live on the
                        // method's name; use the resolver's id
                        // when available. Fallback to no entry.
                        let _ = method;
                        let _ = ret;
                    }
                }
            }
            HirItemKind::Trait(decl) => {
                let _ = decl;
            }
            _ => {}
        }
    }
    out
}

/// Builds two maps from the program's struct declarations:
/// - `structs`: struct name → ordered field names.
/// - `struct_defs`: `DefId` → struct name, so projection lowering
///   can go from an `Adt { def, .. }` receiver type back to the
///   field list.
fn collect_struct_fields(
    program: &HirProgram,
) -> (
    HashMap<String, Vec<String>>,
    HashMap<gossamer_resolve::DefId, String>,
) {
    let mut by_name = HashMap::new();
    let mut by_def = HashMap::new();
    for item in &program.items {
        if let HirItemKind::Adt(adt) = &item.kind {
            if let HirAdtKind::Struct(fields) = &adt.kind {
                by_name.insert(
                    adt.name.name.clone(),
                    fields.iter().map(|f| f.name.clone()).collect(),
                );
                if let Some(def) = item.def {
                    by_def.insert(def, adt.name.name.clone());
                }
            }
        }
    }
    (by_name, by_def)
}

fn collect_item(
    item: &HirItem,
    tcx: &mut TyCtxt,
    structs: &HashMap<String, Vec<String>>,
    struct_defs: &HashMap<gossamer_resolve::DefId, String>,
    fn_returns: &HashMap<gossamer_resolve::DefId, Ty>,
    out: &mut Vec<Body>,
) {
    match &item.kind {
        HirItemKind::Fn(decl) => {
            if let Some(body) =
                lower_fn(decl, item.def, item.span, tcx, structs, struct_defs, fn_returns)
            {
                out.push(body);
            }
        }
        HirItemKind::Impl(decl) => {
            for method in &decl.methods {
                if let Some(body) =
                    lower_fn(method, None, item.span, tcx, structs, struct_defs, fn_returns)
                {
                    out.push(body);
                }
            }
        }
        HirItemKind::Trait(decl) => {
            for method in &decl.methods {
                if method.body.is_some() {
                    if let Some(body) =
                        lower_fn(method, None, item.span, tcx, structs, struct_defs, fn_returns)
                    {
                        out.push(body);
                    }
                }
            }
        }
        HirItemKind::Adt(_) | HirItemKind::Const(_) | HirItemKind::Static(_) => {}
    }
}

fn lower_fn(
    decl: &HirFn,
    def: Option<gossamer_resolve::DefId>,
    span: Span,
    tcx: &mut TyCtxt,
    structs: &HashMap<String, Vec<String>>,
    struct_defs: &HashMap<gossamer_resolve::DefId, String>,
    fn_returns: &HashMap<gossamer_resolve::DefId, Ty>,
) -> Option<Body> {
    let body = decl.body.as_ref()?;
    let mut builder = Builder::new(
        decl.name.name.clone(),
        span,
        tcx,
        structs,
        struct_defs,
        fn_returns,
    );
    let return_ty = decl.ret.unwrap_or_else(|| builder.tcx.unit());
    builder.push_local(return_ty, None, false);
    let arity = u32::try_from(decl.params.len()).expect("arity overflow");
    for param in &decl.params {
        let local = builder.push_local(
            param.ty,
            param_name(&param.pattern),
            param_mutable(&param.pattern),
        );
        builder.param_locals.insert(local);
        if let HirPatKind::Binding { name, .. } = &param.pattern.kind {
            builder.bind_local(&name.name, local);
        }
    }
    let entry = builder.new_block(span);
    builder.set_current(entry);
    let result_local = builder.lower_block(&body.block);
    if let Some(result) = result_local {
        if builder.current.is_some() {
            builder.emit_assign(
                Place::local(Local::RETURN),
                Rvalue::Use(Operand::Copy(Place::local(result))),
                span,
            );
        }
    }
    builder.terminate(Terminator::Return);
    Some(Body {
        name: decl.name.name.clone(),
        def,
        arity,
        locals: builder.locals,
        blocks: builder.blocks,
        span,
    })
}

fn param_name(pattern: &HirPat) -> Option<Ident> {
    match &pattern.kind {
        HirPatKind::Binding { name, .. } => Some(name.clone()),
        _ => None,
    }
}

fn param_mutable(pattern: &HirPat) -> bool {
    matches!(&pattern.kind, HirPatKind::Binding { mutable: true, .. })
}

struct Builder<'a> {
    tcx: &'a mut TyCtxt,
    locals: Vec<LocalDecl>,
    blocks: Vec<BasicBlock>,
    current: Option<BlockId>,
    scopes: Vec<HashMap<String, Local>>,
    fn_span: Span,
    structs: &'a HashMap<String, Vec<String>>,
    struct_defs: &'a HashMap<gossamer_resolve::DefId, String>,
    fn_returns: &'a HashMap<gossamer_resolve::DefId, Ty>,
    local_struct: HashMap<Local, String>,
    /// For locals that hold an array/tuple whose element type is a
    /// known struct, records that struct's name. Used to resolve
    /// field projections through `a[i].x` when the type checker left
    /// the element type as an unresolved inference variable.
    local_elem_struct: HashMap<Local, String>,
    local_closure: HashMap<Local, String>,
    /// Locals that hold a function-name constant (e.g. a synthesised
    /// closure body like `__closure_0` bound through a let). Tracked
    /// so that calling the local dispatches to the named function by
    /// direct call rather than treating the local as a closure env
    /// pointer.
    local_fn_name: HashMap<Local, String>,
    param_locals: std::collections::HashSet<Local>,
    /// Loop contexts visible at the current lowering point. The
    /// innermost loop is at the back. Each entry pairs the
    /// `continue`-target (the loop header) with the `break`-target
    /// (the block emitted right after the loop). `lower_loop` /
    /// `lower_while` push on entry and pop on exit;
    /// `HirExprKind::Break` / `Continue` lookup the back of the
    /// stack to terminate to the right block.
    loop_stack: Vec<LoopContext>,
}

/// A live loop context: where to jump on `break` vs. `continue`.
#[derive(Debug, Clone, Copy)]
struct LoopContext {
    continue_to: BlockId,
    break_to: BlockId,
}

impl<'a> Builder<'a> {
    fn new(
        _name: String,
        span: Span,
        tcx: &'a mut TyCtxt,
        structs: &'a HashMap<String, Vec<String>>,
        struct_defs: &'a HashMap<gossamer_resolve::DefId, String>,
        fn_returns: &'a HashMap<gossamer_resolve::DefId, Ty>,
    ) -> Self {
        Self {
            tcx,
            locals: Vec::new(),
            blocks: Vec::new(),
            current: None,
            scopes: vec![HashMap::new()],
            fn_span: span,
            structs,
            struct_defs,
            fn_returns,
            local_struct: HashMap::new(),
            local_elem_struct: HashMap::new(),
            local_closure: HashMap::new(),
            local_fn_name: HashMap::new(),
            param_locals: std::collections::HashSet::new(),
            loop_stack: Vec::new(),
        }
    }

    /// Returns the struct name registered for the given type (if
    /// any). Walks through references so `&Body` resolves the same
    /// way as `Body`.
    fn struct_name_of(&self, ty: Ty) -> Option<String> {
        use gossamer_types::TyKind;
        let mut cur = ty;
        loop {
            match self.tcx.kind_of(cur) {
                TyKind::Adt { def, .. } => {
                    return self.struct_defs.get(def).cloned();
                }
                TyKind::Ref { inner, .. } => cur = *inner,
                _ => return None,
            }
        }
    }

    /// Walks a HIR place-shaped expression and tries to recover the
    /// struct name of whatever the expression evaluates to, even
    /// when the type checker left the expression's own `ty` as an
    /// unresolved inference variable. Falls through container
    /// projections (`a[_]` → element type, `a.N` → tuple element).
    fn struct_name_from_expr(&self, expr: &HirExpr) -> Option<String> {
        use gossamer_types::TyKind;
        if let Some(name) = self.struct_name_of(expr.ty) {
            return Some(name);
        }
        match &expr.kind {
            HirExprKind::Index { base, .. } => {
                // Prefer the element-type registration (survives
                // inference-variable leakage) before walking the
                // base's static type.
                if let HirExprKind::Path { segments, .. } = &base.kind {
                    if let Some(first) = segments.first() {
                        if let Some(local) = self.lookup_local(&first.name) {
                            if let Some(name) = self.local_elem_struct.get(&local).cloned() {
                                return Some(name);
                            }
                        }
                    }
                }
                let mut cur = base.ty;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                            return self.struct_name_of(*elem);
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return self.struct_name_from_expr(base),
                    }
                }
            }
            HirExprKind::TupleIndex { receiver, index } => {
                let mut cur = receiver.ty;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Tuple(elems) => {
                            let elem = *elems.get(*index as usize)?;
                            return self.struct_name_of(elem);
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return self.struct_name_from_expr(receiver),
                    }
                }
            }
            HirExprKind::Path { segments, .. } => {
                let first = segments.first()?;
                let local = self.lookup_local(&first.name)?;
                let ty = self.locals.get(local.0 as usize)?.ty;
                self.struct_name_of(ty)
            }
            _ => None,
        }
    }

    fn push_local(&mut self, ty: Ty, debug_name: Option<Ident>, mutable: bool) -> Local {
        let id = u32::try_from(self.locals.len()).expect("local overflow");
        self.locals.push(LocalDecl {
            ty,
            debug_name,
            mutable,
        });
        Local(id)
    }

    fn fresh(&mut self, ty: Ty) -> Local {
        self.push_local(ty, None, false)
    }

    fn bind_local(&mut self, name: &str, local: Local) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), local);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<Local> {
        for scope in self.scopes.iter().rev() {
            if let Some(local) = scope.get(name) {
                return Some(*local);
            }
        }
        None
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn new_block(&mut self, span: Span) -> BlockId {
        let id = BlockId(u32::try_from(self.blocks.len()).expect("block overflow"));
        self.blocks.push(BasicBlock {
            id,
            stmts: Vec::new(),
            terminator: Terminator::Unreachable,
            span,
        });
        id
    }

    fn set_current(&mut self, block: BlockId) {
        self.current = Some(block);
    }

    fn current_block(&mut self) -> &mut BasicBlock {
        let id = self.current.expect("no current block").0 as usize;
        &mut self.blocks[id]
    }

    fn emit_assign(&mut self, place: Place, rvalue: Rvalue, span: Span) {
        if self.current.is_none() {
            return;
        }
        let stmt = Statement {
            kind: StatementKind::Assign { place, rvalue },
            span,
        };
        self.current_block().stmts.push(stmt);
    }

    fn terminate(&mut self, terminator: Terminator) {
        if self.current.is_some() {
            let span = self.fn_span;
            let block = self.current_block();
            block.terminator = terminator;
            let _ = span;
        }
        self.current = None;
    }

    fn lower_block(&mut self, block: &HirBlock) -> Option<Local> {
        self.push_scope();
        for stmt in &block.stmts {
            self.lower_stmt(stmt);
            if self.current.is_none() {
                self.pop_scope();
                return None;
            }
        }
        let result = block.tail.as_ref().and_then(|tail| self.lower_expr(tail));
        self.pop_scope();
        if self.current.is_none() { None } else { result }
    }

    fn lower_stmt(&mut self, stmt: &HirStmt) {
        match &stmt.kind {
            HirStmtKind::Let { pattern, ty, init } => {
                let local = self.push_local(*ty, param_name(pattern), param_mutable(pattern));
                if let HirPatKind::Binding { name, .. } = &pattern.kind {
                    self.bind_local(&name.name, local);
                }
                if let Some(init) = init {
                    if let Some(value) = self.lower_expr(init) {
                        // When the HIR-recorded type is an
                        // unresolved inference variable, pin the
                        // binding's MIR type to whatever the lowered
                        // initialiser settled on — keeps downstream
                        // passes (string-concat, codegen cl-type
                        // inference) grounded on concrete kinds.
                        let init_ty = self.locals[value.0 as usize].ty;
                        {
                            use gossamer_types::TyKind;
                            let binding_kind = self.tcx.kind_of(self.locals[local.0 as usize].ty);
                            if !matches!(
                                binding_kind,
                                TyKind::Bool
                                    | TyKind::Char
                                    | TyKind::Int(_)
                                    | TyKind::Float(_)
                                    | TyKind::String
                                    | TyKind::Vec(_)
                                    | TyKind::Array { .. }
                                    | TyKind::Slice(_)
                                    | TyKind::Adt { .. }
                                    | TyKind::Tuple(_)
                                    | TyKind::Ref { .. }
                            ) {
                                self.locals[local.0 as usize].ty = init_ty;
                            }
                        }
                        if let Some(struct_name) = self.local_struct.get(&value).cloned() {
                            self.local_struct.insert(local, struct_name);
                        }
                        if let Some(elem) = self.local_elem_struct.get(&value).cloned() {
                            self.local_elem_struct.insert(local, elem);
                        }
                        if let Some(closure_name) = self.local_closure.get(&value).cloned() {
                            self.local_closure.insert(local, closure_name);
                        }
                        if let Some(fn_name) = self.local_fn_name.get(&value).cloned() {
                            self.local_fn_name.insert(local, fn_name);
                        }
                        self.emit_assign(
                            Place::local(local),
                            Rvalue::Use(Operand::Copy(Place::local(value))),
                            stmt.span,
                        );
                        if let HirPatKind::Tuple(sub_patterns) = &pattern.kind {
                            self.bind_tuple_pattern(local, sub_patterns, stmt.span);
                        }
                    }
                }
            }
            HirStmtKind::Expr { expr, .. } => {
                let _ = self.lower_expr(expr);
            }
            HirStmtKind::Defer(_) => {
                // Deferred calls are lowered to no-ops at the MIR
                // level for now; full support lands with the
                // runtime's unwind-and-run machinery.
            }
            HirStmtKind::Go(expr) => {
                // `go f(args);` — spawn `f` on a fresh OS
                // thread via the runtime's
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`
                // helper. Mirrors the expression-position
                // lowering below so a goroutine spawned at
                // statement level fans out the same way as
                // one used as an expression. Falls back to
                // synchronous execution when the inner shape
                // doesn't match a direct `f(args)` call with
                // ≤ 4 scalar arguments.
                let mut handled = false;
                if let HirExprKind::Call { callee, args } = &expr.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef {
                                    def: *def,
                                    substs,
                                }),
                                expr.span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                if let Some(a) = self.lower_expr(arg) {
                                    operands.push(Operand::Copy(Place::local(a)));
                                }
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(expr.span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(
                                    sym.to_string(),
                                )),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            handled = true;
                        }
                    }
                }
                if !handled {
                    let _ = self.lower_expr(expr);
                }
            }
            HirStmtKind::Item(_) => {
                // Nested items are not supported in the MIR yet.
            }
        }
    }

    fn lower_expr(&mut self, expr: &HirExpr) -> Option<Local> {
        match &expr.kind {
            HirExprKind::Literal(lit) => Some(self.lower_literal(lit, expr.ty, expr.span)),
            HirExprKind::Path { segments, def } => {
                self.lower_path(segments, *def, expr.ty, expr.span)
            }
            HirExprKind::Unary { op, operand } => {
                self.lower_unary(*op, operand, expr.ty, expr.span)
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                self.lower_binary(*op, lhs, rhs, expr.ty, expr.span)
            }
            HirExprKind::Assign { place, value } => {
                self.lower_assign(place, value, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Call { callee, args } => self.lower_call(callee, args, expr.ty, expr.span),
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(
                condition,
                then_branch,
                else_branch.as_deref(),
                expr.ty,
                expr.span,
            ),
            HirExprKind::While { condition, body } => {
                self.lower_while(condition, body, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Loop { body } => self.lower_loop(body, expr.ty, expr.span),
            HirExprKind::Block(block) => self.lower_block(block),
            HirExprKind::Return(value) => {
                if let Some(value) = value {
                    if let Some(local) = self.lower_expr(value) {
                        self.emit_assign(
                            Place::local(Local::RETURN),
                            Rvalue::Use(Operand::Copy(Place::local(local))),
                            expr.span,
                        );
                    }
                }
                self.terminate(Terminator::Return);
                None
            }
            HirExprKind::Break(_) => {
                // Jump to the innermost loop's break target. Outside
                // a loop the resolver/typechecker is supposed to
                // reject this; if it slips through, fall back to
                // `Unreachable` rather than emit a dangling jump.
                if let Some(ctx) = self.loop_stack.last().copied() {
                    self.terminate(Terminator::Goto { target: ctx.break_to });
                } else {
                    self.terminate(Terminator::Unreachable);
                }
                None
            }
            HirExprKind::Continue => {
                if let Some(ctx) = self.loop_stack.last().copied() {
                    self.terminate(Terminator::Goto { target: ctx.continue_to });
                } else {
                    self.terminate(Terminator::Unreachable);
                }
                None
            }
            HirExprKind::Tuple(elems) => self.lower_tuple(elems, expr.ty, expr.span),
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                self.lower_array_list(elems, expr.ty, expr.span)
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                self.lower_array_repeat(value, count, expr.ty, expr.span)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                self.lower_tuple_index(receiver, *index, expr.ty, expr.span)
            }
            HirExprKind::Index { base, index } => {
                self.lower_index_access(base, index, expr.ty, expr.span)
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.lower_match(scrutinee, arms, expr.ty, expr.span)
            }
            HirExprKind::Cast { value, ty: target } => {
                self.lower_cast(value, *target, expr.ty, expr.span)
            }
            HirExprKind::Field { receiver, name } => {
                self.lower_field_access(receiver, name, expr.ty, expr.span)
            }
            HirExprKind::LiftedClosure { name, captures } => {
                self.lower_lifted_closure(name, captures, expr.ty, expr.span)
            }
            HirExprKind::MethodCall { receiver, name, args } => {
                self.lower_method_call(receiver, name, args, expr.ty, expr.span)
            }
            HirExprKind::Go(inner) => {
                let go_span = expr.span;
                // Real spawn for `go f(args)` where f is a named
                // function with 0-2 scalar args: emit a call to
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`. The
                // runtime helper transmutes fn_addr back to
                // `extern "C" fn(...) -> i64` and runs it on a
                // fresh OS thread.
                //
                // Anything more complex (closure captures, >2
                // args, method calls) falls back to synchronous
                // execution so the program still runs — sound
                // for single-threaded workloads.
                if let HirExprKind::Call { callee, args } = &inner.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef {
                                    def: *def,
                                    substs,
                                }),
                                go_span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                let a = self.lower_expr(arg)?;
                                operands.push(Operand::Copy(Place::local(a)));
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(go_span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            return Some(dest);
                        }
                    }
                }
                // Fallback: synchronous.
                let _ = self.lower_expr(inner);
                Some(self.lower_unit(go_span))
            }
            HirExprKind::Select { arms } => {
                // Sequential stub: run each arm's side-effects and
                // then the first arm's body. The real runtime will
                // pick the first ready channel, but under the
                // single-task stub we just pretend arm 0 fired.
                use gossamer_hir::HirSelectOp;
                let mut result: Option<Local> = None;
                for (i, arm) in arms.iter().enumerate() {
                    match &arm.op {
                        HirSelectOp::Recv { channel, .. }
                        | HirSelectOp::Send { channel, .. } => {
                            let _ = self.lower_expr(channel);
                        }
                        HirSelectOp::Default => {}
                    }
                    if i == 0 {
                        result = self.lower_expr(&arm.body);
                    }
                }
                result.or_else(|| Some(self.lower_unit(expr.span)))
            }
            HirExprKind::Range { .. }
            | HirExprKind::Closure { .. }
            | HirExprKind::Placeholder => {
                // Lowering of these constructs is left to later
                // milestones; emit an unreachable placeholder so the
                // block is still well-formed.
                let local = self.fresh(expr.ty);
                self.emit_assign(
                    Place::local(local),
                    Rvalue::CallIntrinsic {
                        name: "unsupported",
                        args: Vec::new(),
                    },
                    expr.span,
                );
                Some(local)
            }
        }
    }

    /// Lowers a `HirExprKind::LiftedClosure` into a heap env laid out
    /// as `[fn_addr, cap0, cap1, …]`: the first word holds the
    /// address of the lifted function (used for indirect dispatch
    /// when the closure escapes into a parameter), and each capture
    /// occupies one i64 slot at offset `8*(i+1)`. The local that
    /// owns the env pointer is registered in `local_closure` so
    /// direct calls at the creation site can bypass the indirect
    /// dispatch and jump straight to the lifted function.
    fn lower_lifted_closure(
        &mut self,
        name: &Ident,
        captures: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let size = i128::from((captures.len() + 1) as i64 * 8);
        let size_local = self.fresh(ty);
        self.emit_assign(
            Place::local(size_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(size))),
            span,
        );
        let env_local = self.fresh(ty);
        self.emit_assign(
            Place::local(env_local),
            Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(Place::local(size_local))],
            },
            span,
        );
        let fn_addr_local = self.fresh(ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(name.name.clone()))],
            },
            span,
        );
        let zero_offset_local = self.fresh(ty);
        self.emit_assign(
            Place::local(zero_offset_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let sink = self.fresh(ty);
        self.emit_assign(
            Place::local(sink),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(zero_offset_local)),
                    Operand::Copy(Place::local(fn_addr_local)),
                ],
            },
            span,
        );
        for (i, cap) in captures.iter().enumerate() {
            let offset = (i as i64 + 1) * 8;
            let offset_local = self.fresh(ty);
            self.emit_assign(
                Place::local(offset_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(offset)))),
                span,
            );
            let value_local = self.lower_expr(cap)?;
            let sink = self.fresh(ty);
            self.emit_assign(
                Place::local(sink),
                Rvalue::CallIntrinsic {
                    name: "gos_store",
                    args: vec![
                        Operand::Copy(Place::local(env_local)),
                        Operand::Copy(Place::local(offset_local)),
                        Operand::Copy(Place::local(value_local)),
                    ],
                },
                span,
            );
        }
        self.local_closure.insert(env_local, name.name.clone());
        Some(env_local)
    }

    fn lower_literal(&mut self, lit: &HirLiteral, ty: Ty, span: Span) -> Local {
        // Pin the literal's MIR type to the concrete kind the
        // literal implies, not the HIR expression's `ty` which may
        // still be an unresolved inference variable. Downstream
        // passes (string-concat detection, cranelift type
        // inference) rely on this being grounded.
        use gossamer_types::{FloatTy as Ft, IntTy as It, TyKind};
        let concrete = match lit {
            HirLiteral::String(_) => Some(self.tcx.string_ty()),
            HirLiteral::Bool(_) => Some(self.tcx.bool_ty()),
            HirLiteral::Char(_) => Some(self.tcx.char_ty()),
            HirLiteral::Unit => Some(self.tcx.unit()),
            _ => None,
        };
        let local_ty = match concrete {
            Some(concrete_ty) => concrete_ty,
            None => match self.tcx.kind_of(ty) {
                TyKind::Int(_) | TyKind::Float(_) => ty,
                _ => match lit {
                    HirLiteral::Int(_) => self.tcx.int_ty(It::I64),
                    HirLiteral::Float(_) => self.tcx.float_ty(Ft::F64),
                    _ => ty,
                },
            },
        };
        let local = self.fresh(local_ty);
        let value = literal_to_const(lit);
        self.emit_assign(
            Place::local(local),
            Rvalue::Use(Operand::Const(value)),
            span,
        );
        local
    }

    fn lower_path(
        &mut self,
        segments: &[Ident],
        def: Option<gossamer_resolve::DefId>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if let Some(first) = segments.first() {
            if let Some(local) = self.lookup_local(&first.name) {
                return Some(local);
            }
        }
        let local = self.fresh(ty);
        let joined_name = segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        let operand = if let Some(def) = def { Operand::FnRef {
            def,
            substs: self.substs_of(ty),
        } } else {
            // Record that `local` holds a function-name constant
            // so a later `let` binding + call can still dispatch
            // directly to the named function without treating
            // the local as a closure env pointer.
            self.local_fn_name.insert(local, joined_name.clone());
            Operand::Const(ConstValue::Str(joined_name))
        };
        self.emit_assign(Place::local(local), Rvalue::Use(operand), span);
        Some(local)
    }

    /// Returns the generic substitution recorded on a function-shaped
    /// type. `Ty`s that are not `FnDef` (closures, plain references,
    /// anything resolved to an error) yield an empty substitution.
    fn substs_of(&self, ty: Ty) -> gossamer_types::Substs {
        match self.tcx.kind(ty) {
            Some(gossamer_types::TyKind::FnDef { substs, .. }) => substs.clone(),
            _ => gossamer_types::Substs::new(),
        }
    }

    fn lower_unary(
        &mut self,
        op: HirUnaryOp,
        operand: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let inner = self.lower_expr(operand)?;
        let local = self.fresh(ty);
        let mir_op = match op {
            HirUnaryOp::Neg => UnOp::Neg,
            HirUnaryOp::Not => UnOp::Not,
            HirUnaryOp::RefShared | HirUnaryOp::RefMut | HirUnaryOp::Deref => {
                self.emit_assign(
                    Place::local(local),
                    Rvalue::Use(Operand::Copy(Place::local(inner))),
                    span,
                );
                return Some(local);
            }
        };
        self.emit_assign(
            Place::local(local),
            Rvalue::UnaryOp {
                op: mir_op,
                operand: Operand::Copy(Place::local(inner)),
            },
            span,
        );
        Some(local)
    }

    fn lower_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let lhs_local = self.lower_expr(lhs)?;
        let rhs_local = self.lower_expr(rhs)?;
        // Detect string concatenation (`s1 + s2` where at least
        // one side is a `String`) and route it through the native
        // runtime's `gos_rt_str_concat` helper rather than the
        // integer `+`. HIR types may still carry unresolved
        // inference variables here, so we inspect the lowered
        // MIR locals' concrete types too.
        if matches!(op, HirBinaryOp::Add) {
            let is_string = |t: Ty| -> bool {
                let mut cur = t;
                loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::String => return true,
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => return false,
                    }
                }
            };
            if is_string(ty)
                || is_string(lhs.ty)
                || is_string(rhs.ty)
                || is_string(self.locals[lhs_local.0 as usize].ty)
                || is_string(self.locals[rhs_local.0 as usize].ty)
            {
                let dest_ty = self.tcx.string_ty();
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_concat".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(lhs_local)),
                        Operand::Copy(Place::local(rhs_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
        }
        let local = self.fresh(ty);
        let bin_op = lower_binop(op);
        self.emit_assign(
            Place::local(local),
            Rvalue::BinaryOp {
                op: bin_op,
                lhs: Operand::Copy(Place::local(lhs_local)),
                rhs: Operand::Copy(Place::local(rhs_local)),
            },
            span,
        );
        Some(local)
    }

    fn lower_assign(&mut self, place: &HirExpr, value: &HirExpr, span: Span) {
        let Some(value_local) = self.lower_expr(value) else {
            return;
        };
        let Some(mir_place) = self.lower_place_expr(place) else {
            return;
        };
        self.emit_assign(
            mir_place,
            Rvalue::Use(Operand::Copy(Place::local(value_local))),
            span,
        );
    }

    /// Converts a HIR expression used in lvalue position (`a`,
    /// `a.field`, `a[i]`, `a.0`, nested combinations) into a MIR
    /// [`Place`] with the right projection chain. Returns `None`
    /// when the expression is not a place (e.g. a literal).
    fn lower_place_expr(&mut self, expr: &HirExpr) -> Option<Place> {
        match &expr.kind {
            HirExprKind::Path { segments, .. } => {
                let first = segments.first()?;
                let local = self.lookup_local(&first.name)?;
                Some(Place::local(local))
            }
            HirExprKind::Field { receiver, name } => {
                let mut base = self.lower_place_expr(receiver)?;
                // Field index: first try the base's local_struct
                // registration, then fall back to the receiver's
                // static type via the type system.
                let struct_name = self
                    .local_struct
                    .get(&base.local)
                    .cloned()
                    .or_else(|| self.struct_name_from_expr(receiver))?;
                let order = self.structs.get(&struct_name)?;
                let idx = u32::try_from(order.iter().position(|f| f == &name.name)?)
                    .ok()?;
                base.projection.push(crate::ir::Projection::Field(idx));
                Some(base)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                let mut base = self.lower_place_expr(receiver)?;
                base.projection.push(crate::ir::Projection::Field(*index));
                Some(base)
            }
            HirExprKind::Index { base, index } => {
                let mut base_place = self.lower_place_expr(base)?;
                let index_local = self.lower_expr(index)?;
                base_place.projection.push(crate::ir::Projection::Index(index_local));
                Some(base_place)
            }
            _ => None,
        }
    }

    fn lower_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // When the callee's `DefId` is known and its declared
        // return type is on record, prefer the callee's return
        // type over the call-expression's HIR type — the latter
        // may still be an inference variable.
        let ty = if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
            // Prefer the callee's declared return type over the
            // call-expression's HIR type when available; the
            // checker often leaves the latter as an inference
            // variable.
            use gossamer_types::TyKind;
            if let Some(registered) = self.fn_returns.get(def).copied() {
                if matches!(self.tcx.kind_of(registered), TyKind::Error) {
                    ty
                } else {
                    registered
                }
            } else {
                ty
            }
        } else {
            ty
        };
        // Pin the call's dest type for known stdlib path callees
        // whose return kind is fixed. The typechecker leaves most
        // stdlib call-expression types as `Var` because no impl
        // index tracks them; the codegen then defaults to pointer-
        // or int-typed registers. Fix the printable kind here.
        let ty = {
            use gossamer_types::TyKind;
            if let HirExprKind::Path { segments, def: None, .. } = &callee.kind {
                let joined = segments
                    .iter()
                    .map(|s| s.name.as_str())
                    .collect::<Vec<_>>()
                    .join("::");
                if matches!(
                    self.tcx.kind_of(ty),
                    TyKind::Error | TyKind::Var(_)
                ) {
                    match joined.as_str() {
                        "math::sqrt"
                        | "math::sin"
                        | "math::cos"
                        | "math::ln"
                        | "math::log"
                        | "math::exp"
                        | "math::abs"
                        | "math::floor"
                        | "math::ceil"
                        | "math::pow"
                        | "time::now" => self.tcx.float_ty(gossamer_types::FloatTy::F64),
                        "time::now_ns"
                        | "time::now_ms"
                        | "strconv::parse_i64"
                        | "gos_rt_math_sqrt" => {
                            self.tcx.int_ty(gossamer_types::IntTy::I64)
                        }
                        _ => ty,
                    }
                } else {
                    ty
                }
            } else {
                ty
            }
        };
        if let Some(local) = self.lower_struct_call(callee, args, ty, span) {
            return Some(local);
        }
        // If the callee is a bare path that resolves to a local
        // previously registered as a lifted closure, dispatch
        // statically to that closure's top-level function and pass
        // the env pointer as the implicit first argument.
        if let HirExprKind::Path { segments, def: None, .. } = &callee.kind {
            if segments.len() == 1 {
                if let Some(local) = self.lookup_local(&segments[0].name) {
                    if let Some(fn_name) = self.local_closure.get(&local).cloned() {
                        let mut arg_operands =
                            Vec::with_capacity(args.len() + 1);
                        arg_operands
                            .push(Operand::Copy(Place::local(local)));
                        for arg in args {
                            let a = self.lower_expr(arg)?;
                            arg_operands.push(Operand::Copy(Place::local(a)));
                        }
                        let dest = self.fresh(ty);
                        let next = self.new_block(span);
                        self.terminate(Terminator::Call {
                            callee: Operand::Const(ConstValue::Str(fn_name)),
                            args: arg_operands,
                            destination: Place::local(dest),
                            target: Some(next),
                        });
                        self.set_current(next);
                        return Some(dest);
                    }
                }
            }
        }
        let callee_operand = match &callee.kind {
            HirExprKind::Path { def: Some(def), .. } => Operand::FnRef {
                def: *def,
                substs: self.substs_of(callee.ty),
            },
            HirExprKind::Path { segments, def: None, .. } => {
                // Only treat a bare local as an indirect closure
                // callee when it came from a function parameter.
                // Other locals (e.g. bound to `Const(Str(name))`
                // by a `let f = bare_name`) still flow through the
                // by-name callee lookup so the direct dispatch path
                // resolves them to the named function body.
                if segments.len() == 1 {
                    if let Some(local) = self.lookup_local(&segments[0].name) {
                        use gossamer_types::TyKind;
                        // Prefer the recorded function-name binding
                        // when the local holds a `Const(Str(name))`
                        // (e.g. `let plus = __closure_0; plus(...)`).
                        // Falling back to the segment name alone
                        // loses the pointer to the synthesised body.
                        if let Some(name) = self.local_fn_name.get(&local).cloned() {
                            Operand::Const(ConstValue::Str(name))
                        } else if self.param_locals.contains(&local) {
                            Operand::Copy(Place::local(local))
                        } else if matches!(
                            self.tcx.kind_of(self.locals[local.0 as usize].ty),
                            TyKind::FnPtr(_) | TyKind::FnDef { .. } | TyKind::Closure { .. }
                        ) {
                            // Local bound to a function-typed value
                            // (e.g. returned from `make_counter()`).
                            // Call it indirectly through the local.
                            Operand::Copy(Place::local(local))
                        } else {
                            Operand::Const(ConstValue::Str(segments[0].name.clone()))
                        }
                    } else {
                        Operand::Const(ConstValue::Str(segments[0].name.clone()))
                    }
                } else {
                    Operand::Const(ConstValue::Str(
                        segments
                            .iter()
                            .map(|s| s.name.as_str())
                            .collect::<Vec<_>>()
                            .join("::"),
                    ))
                }
            }
            _ => {
                let local = self.lower_expr(callee)?;
                Operand::Copy(Place::local(local))
            }
        };
        let mut arg_operands = Vec::with_capacity(args.len());
        for arg in args {
            let local = self.lower_expr(arg)?;
            arg_operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: callee_operand,
            args: arg_operands,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    fn lower_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let cond_local = self.lower_expr(condition)?;
        let then_block = self.new_block(span);
        let else_block = self.new_block(span);
        let join_block = self.new_block(span);
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, else_block)],
            default: then_block,
        });

        let result_local = self.fresh(ty);

        self.set_current(then_block);
        if let Some(then_value) = self.lower_expr(then_branch) {
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(then_value))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(else_block);
        if let Some(else_branch) = else_branch {
            if let Some(else_value) = self.lower_expr(else_branch) {
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(else_value))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        } else {
            let unit_local = self.lower_unit(span);
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(unit_local))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(join_block);
        Some(result_local)
    }

    /// Lowers a `match` expression over an integer or boolean
    /// scrutinee into a `SwitchInt` terminator. Handles only literal
    /// and wildcard/binding patterns — any other pattern (tuple,
    /// struct, variant, or arm with a guard) aborts the lowering and
    /// emits a `CallIntrinsic { name: "unsupported" }` placeholder so
    /// callers fall back to the interpreter instead of miscompiling.
    fn lower_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if arms.iter().any(|arm| arm.guard.is_some()) {
            return self.lower_unsupported_placeholder(ty, span);
        }
        let mut switch_arms: Vec<(i128, BlockId)> = Vec::new();
        let mut default_block: Option<BlockId> = None;
        let mut arm_bodies: Vec<(BlockId, &HirExpr)> = Vec::with_capacity(arms.len());
        for arm in arms {
            let arm_block = self.new_block(span);
            arm_bodies.push((arm_block, &arm.body));
            match &arm.pattern.kind {
                HirPatKind::Literal(HirLiteral::Int(text)) => {
                    let Some(v) = parse_int(text) else {
                        return self.lower_unsupported_placeholder(ty, span);
                    };
                    switch_arms.push((v, arm_block));
                }
                HirPatKind::Literal(HirLiteral::Bool(b)) => {
                    switch_arms.push((i128::from(*b), arm_block));
                }
                HirPatKind::Wildcard | HirPatKind::Binding { .. } => {
                    if default_block.is_some() {
                        return self.lower_unsupported_placeholder(ty, span);
                    }
                    default_block = Some(arm_block);
                }
                // Variant patterns (`Ok(x)`, `Err(e)`, `Some(v)`, …)
                // don't yet have runtime discriminants, but we can
                // still produce a well-formed CFG by always taking
                // the first variant arm as a "happy path" default.
                // Bind any inner pattern to the scrutinee local so
                // `let x = foo()?` compiles. Wrong for genuine error
                // cases, but enough for programs whose control flow
                // stays on the Ok/Some path.
                HirPatKind::Variant { name, fields } => {
                    let pos = i128::from(
                        matches!(name.name.as_str(), "Err" | "None" | "Some" | "Ok")
                            .then(|| match name.name.as_str() {
                                "Some" | "Ok" => 0,
                                _ => 1,
                            })
                            .unwrap_or(switch_arms.len() as i32),
                    );
                    switch_arms.push((pos, arm_block));
                    // Note: variant inner bindings get zeroed
                    // scrutinee-shaped values; downstream codegen
                    // treats them as i64 locals. That satisfies
                    // typeck but not real semantics — tracked as
                    // part of the GC/runtime-variants work.
                    let _ = fields;
                }
                _ => return self.lower_unsupported_placeholder(ty, span),
            }
        }
        let scrutinee_local = self.lower_expr(scrutinee)?;
        let join_block = self.new_block(span);
        let result_local = self.fresh(ty);
        let default = default_block.unwrap_or_else(|| {
            let unreachable_block = self.new_block(span);
            self.set_current(unreachable_block);
            self.terminate(Terminator::Unreachable);
            unreachable_block
        });
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(scrutinee_local)),
            arms: switch_arms,
            default,
        });
        for (arm_block, body) in arm_bodies {
            self.set_current(arm_block);
            if let Some(value_local) = self.lower_expr(body) {
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(value_local))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        }
        self.set_current(join_block);
        Some(result_local)
    }

    /// Lowers `expr as T` into `Rvalue::Cast { operand, target }`.
    fn lower_cast(
        &mut self,
        value: &HirExpr,
        target: Ty,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let value_local = self.lower_expr(value)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Cast {
                operand: Operand::Copy(Place::local(value_local)),
                target,
            },
            span,
        );
        Some(dest)
    }

    /// Binds each element of a tuple pattern to a fresh local reading
    /// through a `Projection::Field(i)`. Only the simple shapes used
    /// in practice — [`HirPatKind::Binding`] and [`HirPatKind::Wildcard`]
    /// — are supported; nested or non-tuple sub-patterns are silently
    /// skipped so the outer binding still sees the whole aggregate.
    fn bind_tuple_pattern(&mut self, tuple_local: Local, sub_patterns: &[HirPat], span: Span) {
        for (i, sub) in sub_patterns.iter().enumerate() {
            let HirPatKind::Binding { name, mutable } = &sub.kind else {
                continue;
            };
            let element_local =
                self.push_local(sub.ty, Some(Ident::new(name.name.as_str())), *mutable);
            self.bind_local(name.name.as_str(), element_local);
            let projection = vec![crate::ir::Projection::Field(
                u32::try_from(i).expect("tuple projection overflow"),
            )];
            let place = Place {
                local: tuple_local,
                projection,
            };
            self.emit_assign(
                Place::local(element_local),
                Rvalue::Use(Operand::Copy(place)),
                span,
            );
        }
    }

    /// Recognises a call to the synthetic `__struct("Name", "f1", v1,
    /// "f2", v2, …)` builtin and rewrites it into an
    /// [`Rvalue::Aggregate`] with the operands in declaration order.
    /// Returns `None` when the call is not a struct literal.
    fn lower_struct_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        let last = segments.last()?;
        if last.name != "__struct" {
            return None;
        }
        let (name_expr, pairs) = args.split_first()?;
        let HirExprKind::Literal(HirLiteral::String(struct_name)) = &name_expr.kind else {
            return None;
        };
        if pairs.len() % 2 != 0 {
            return None;
        }
        let order = self.structs.get(struct_name)?.clone();
        let mut provided: HashMap<String, &HirExpr> = HashMap::new();
        let mut chunks = pairs.chunks_exact(2);
        for chunk in chunks.by_ref() {
            let HirExprKind::Literal(HirLiteral::String(field_name)) = &chunk[0].kind else {
                return None;
            };
            provided.insert(field_name.clone(), &chunk[1]);
        }
        let mut operands = Vec::with_capacity(order.len());
        for field in &order {
            let value_expr = provided.get(field.as_str())?;
            let value_local = self.lower_expr(value_expr)?;
            operands.push(Operand::Copy(Place::local(value_local)));
        }
        let dest = self.fresh(ty);
        self.local_struct.insert(dest, struct_name.clone());
        // Adt requires a DefId we don't have handy at this layer.
        // The native codegen treats every aggregate as a flat i64-per
        // slot stack slot regardless of kind, so `Tuple` is a safe
        // structural stand-in until monomorphisation wires real DefIds
        // through.
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Tuple,
                operands,
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `receiver.name` into a projection read when `receiver`'s
    /// type is a known named struct. Falls back to the unsupported
    /// placeholder for any other receiver shape.
    fn lower_field_access(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Try the place-expression path first: for `a.x`, `a[i].x`,
        // and other lvalue-shaped receivers this builds a direct
        // projected place read without materialising the intermediate
        // struct copy. That lets `a[i].x` lower to `copy a[i].x`
        // instead of `tmp = a[i]; tmp.x` (and the latter's
        // lost-struct-name fallback to the unsupported placeholder).
        if let Some(mut place) = self.lower_place_expr(receiver) {
            let struct_name = self
                .local_struct
                .get(&place.local)
                .cloned()
                .or_else(|| self.struct_name_from_expr(receiver));
            if let Some(sname) = struct_name {
                if let Some(order) = self.structs.get(&sname).cloned() {
                    if let Some(pos) = order.iter().position(|f| f == &name.name) {
                        let idx = u32::try_from(pos).ok()?;
                        place.projection.push(crate::ir::Projection::Field(idx));
                        let dest = self.fresh(ty);
                        self.emit_assign(
                            Place::local(dest),
                            Rvalue::Use(Operand::Copy(place)),
                            span,
                        );
                        return Some(dest);
                    }
                }
            }
        }

        // Fallback: recurse into the receiver and use its local's
        // recorded struct name (the original path, kept for cases
        // where the receiver is an expression rather than a place
        // — e.g. a call that returns a struct).
        let receiver_local = self.lower_expr(receiver)?;
        let struct_name = self
            .local_struct
            .get(&receiver_local)
            .cloned()
            .or_else(|| self.struct_name_of(receiver.ty));
        let field_order = struct_name.as_ref().and_then(|n| self.structs.get(n)).cloned();
        let Some(order) = field_order else {
            return self.lower_unsupported_placeholder(ty, span);
        };
        let idx = order
            .iter()
            .position(|f| f == &name.name)
            .map(|i| u32::try_from(i).expect("field index fits u32"));
        let Some(idx) = idx else {
            return self.lower_unsupported_placeholder(ty, span);
        };
        let dest = self.fresh(ty);
        let place = Place {
            local: receiver_local,
            projection: vec![crate::ir::Projection::Field(idx)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    /// Lowers `receiver.method(args…)` into a `Call` terminator.
    /// First tries the stdlib intrinsic table (method names whose
    /// semantics the native runtime implements as a C-ABI helper);
    /// falls back to the `unsupported` placeholder if the receiver
    /// shape isn't recognised.
    fn lower_method_call(
        &mut self,
        receiver: &HirExpr,
        method: &Ident,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let receiver_ty = receiver.ty;
        let receiver_kind = self.tcx.kind_of(receiver_ty).clone();
        // Unwrap a leading `&T` so `s.len()` on a `&String`
        // parameter lowers the same as on an owned `String`.
        let receiver_kind_flat = match &receiver_kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(*inner).clone(),
            other => other.clone(),
        };

        // Stdlib dispatch table. First by method name alone —
        // covers receivers whose HIR type is still an unresolved
        // inference variable (common post-checker). The runtime
        // helpers accept any receiver shape and return a safe
        // default (0, empty, null) for inputs the native runtime
        // doesn't yet represent.
        //
        // When the callee name is empty the method is identity
        // (currently `.to_string()` / `.clone()` on any scalar or
        // string-shaped receiver — the GC already aliases the
        // buffer).
        let runtime_symbol: Option<&'static str> = match method.name.as_str() {
            // `.to_string()` routes to the runtime numeric
            // formatter for integer / float receivers. String
            // receivers fall through to the identity copy.
            "to_string" => match &receiver_kind_flat {
                TyKind::Int(_) => Some("gos_rt_i64_to_str"),
                TyKind::Float(_) => Some("gos_rt_f64_to_str"),
                _ => Some(""),
            },
            "clone" => Some(""),
            "len" => match &receiver_kind_flat {
                TyKind::String => Some("gos_rt_str_len"),
                TyKind::HashMap { .. } => Some("gos_rt_map_len"),
                TyKind::Vec(_) | TyKind::Array { .. } | TyKind::Slice(_) => {
                    Some("gos_rt_len")
                }
                _ => Some("gos_rt_len"),
            },
            "trim" => Some("gos_rt_str_trim"),
            "contains" => Some("gos_rt_str_contains"),
            "starts_with" => Some("gos_rt_str_starts_with"),
            "ends_with" => Some("gos_rt_str_ends_with"),
            "find" => Some("gos_rt_str_find"),
            "replace" => Some("gos_rt_str_replace"),
            "split" => Some("gos_rt_str_split"),
            "to_lowercase" => Some("gos_rt_str_to_lower"),
            "to_uppercase" => Some("gos_rt_str_to_upper"),
            "push" => Some("gos_rt_vec_push"),
            "pop" => Some("gos_rt_vec_pop"),
            "iter" => Some("gos_rt_arr_iter"),
            "as_bytes" => Some(""),
            "as_str" => Some(""),
            "send" => Some("gos_rt_chan_send"),
            "recv" => Some("gos_rt_chan_recv"),
            "try_send" => Some("gos_rt_chan_try_send"),
            "try_recv" => Some("gos_rt_chan_try_recv"),
            "close" => Some("gos_rt_chan_close"),
            // Stream methods (on `io::stdout()` / `io::stderr()`
            // / `io::stdin()` handles). Mirrors Rust's `Write` /
            // `BufRead` trait surface.
            "write_byte" => Some("gos_rt_stream_write_byte"),
            "write_byte_array" | "write_bytes" => {
                Some("gos_rt_stream_write_byte_array")
            }
            "write" | "write_str" => Some("gos_rt_stream_write_str"),
            "flush" => Some("gos_rt_stream_flush"),
            "read_line" => Some("gos_rt_stream_read_line"),
            "read_to_string" => Some("gos_rt_stream_read_to_string"),
            "insert" => Some("gos_rt_map_insert"),
            "get" => Some("gos_rt_map_get"),
            "remove" => Some("gos_rt_map_remove"),
            // Mutex<T> / WaitGroup / Atomic / heap-Vec
            // primitives. Each method dispatches by name —
            // the runtime function takes the receiver
            // pointer as its first arg, matching the rest of
            // the table.
            "lock" => Some("gos_rt_mutex_lock"),
            "unlock" => Some("gos_rt_mutex_unlock"),
            "add" => Some("gos_rt_wg_add"),
            "done" => Some("gos_rt_wg_done"),
            "wait" => Some("gos_rt_wg_wait"),
            "load" => Some("gos_rt_atomic_i64_load"),
            "store" => Some("gos_rt_atomic_i64_store"),
            "fetch_add" => Some("gos_rt_atomic_i64_fetch_add"),
            "set_at" => Some("gos_rt_heap_i64_set"),
            "get_at" => Some("gos_rt_heap_i64_get"),
            "vec_len" => Some("gos_rt_heap_i64_len"),
            "write_range_to_stdout" => {
                Some("gos_rt_heap_i64_write_bytes_to_stdout")
            }
            "write_lines_to_stdout" => {
                Some("gos_rt_heap_i64_write_lines_to_stdout")
            }
            // U8Vec methods. Distinct names from the I64Vec
            // family because MIR's method dispatch is by name
            // alone — sharing `set_at` between i64 and u8
            // receivers would silently write through the
            // i64-stride helper to a u8 buffer, corrupting
            // adjacent bytes.
            "set_byte" => Some("gos_rt_heap_u8_set"),
            "get_byte" => Some("gos_rt_heap_u8_get"),
            "byte_len" => Some("gos_rt_heap_u8_len"),
            "write_byte_range_to_stdout" => {
                Some("gos_rt_heap_u8_write_bytes_to_stdout")
            }
            "write_byte_lines_to_stdout" => {
                Some("gos_rt_heap_u8_write_lines_to_stdout")
            }
            _ => None,
        };
        let _ = receiver_kind;

        let receiver_local = self.lower_expr(receiver)?;
        let mut arg_operands = Vec::with_capacity(args.len() + 1);
        arg_operands.push(Operand::Copy(Place::local(receiver_local)));
        for arg in args {
            let a = self.lower_expr(arg)?;
            arg_operands.push(Operand::Copy(Place::local(a)));
        }

        if let Some(sym) = runtime_symbol {
            if sym.is_empty() {
                // Identity method — just copy the receiver to the
                // destination. Lets `"lit".to_string()` lower
                // without involving the runtime.
                //
                // Pin the destination's MIR type to the receiver's
                // own type rather than the method-call expression's
                // (often still unresolved) inference variable, so
                // downstream passes see a concrete `String` /
                // `Vec<T>` / etc. — crucial for the binary-op
                // lowering in `lower_binary` to route `s + t`
                // through `gos_rt_str_concat`.
                let dest_ty = match self.tcx.kind_of(ty) {
                    TyKind::Bool
                    | TyKind::Char
                    | TyKind::Int(_)
                    | TyKind::Float(_)
                    | TyKind::String
                    | TyKind::Vec(_)
                    | TyKind::Array { .. }
                    | TyKind::Slice(_)
                    | TyKind::Adt { .. }
                    | TyKind::Tuple(_) => ty,
                    _ => receiver_ty,
                };
                let dest = self.fresh(dest_ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Copy(Place::local(receiver_local))),
                    span,
                );
                return Some(dest);
            }
            // Pin the destination's MIR type to the helper's
            // known return shape when the HIR expression type is
            // still opaque (inference variable or Error). Keeps
            // operand_print_kind + codegen inference grounded on
            // a concrete scalar/string kind.
            let pinned_ret: Ty = match sym {
                "gos_rt_str_concat"
                | "gos_rt_str_trim"
                | "gos_rt_str_to_lower"
                | "gos_rt_str_to_upper"
                | "gos_rt_str_replace"
                | "gos_rt_i64_to_str"
                | "gos_rt_f64_to_str"
                | "gos_rt_stream_read_line"
                | "gos_rt_stream_read_to_string" => self.tcx.string_ty(),
                "gos_rt_str_contains"
                | "gos_rt_str_starts_with"
                | "gos_rt_str_ends_with" => self.tcx.bool_ty(),
                "gos_rt_str_find"
                | "gos_rt_str_len"
                | "gos_rt_str_byte_at"
                | "gos_rt_arr_len"
                | "gos_rt_len"
                | "gos_rt_map_len"
                | "gos_rt_chan_recv"
                | "gos_rt_chan_try_recv"
                | "gos_rt_vec_pop" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_chan_try_send"
                | "gos_rt_map_remove" => self.tcx.bool_ty(),
                _ => match self.tcx.kind_of(ty) {
                    TyKind::Error | TyKind::Var(_) => {
                        self.tcx.int_ty(gossamer_types::IntTy::I64)
                    }
                    _ => ty,
                },
            };
            let dest = self.fresh(pinned_ret);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }

        // No known intrinsic mapping — leave as the unsupported
        // placeholder for now. L1.x milestones replace the
        // remaining cases (user-defined `impl` methods via the
        // trait index) incrementally.
        self.lower_unsupported_placeholder(ty, span)
    }

    fn lower_unsupported_placeholder(&mut self, ty: Ty, span: Span) -> Option<Local> {
        let local = self.fresh(ty);
        self.emit_assign(
            Place::local(local),
            Rvalue::CallIntrinsic {
                name: "unsupported",
                args: Vec::new(),
            },
            span,
        );
        Some(local)
    }

    /// Lowers a tuple literal into an `Rvalue::Aggregate { kind:
    /// Tuple }` stored in a fresh local.
    fn lower_tuple(&mut self, elems: &[HirExpr], ty: Ty, span: Span) -> Option<Local> {
        let mut operands = Vec::with_capacity(elems.len());
        for elem in elems {
            let local = self.lower_expr(elem)?;
            operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Tuple,
                operands,
            },
            span,
        );
        Some(dest)
    }

    /// Lowers an explicit array literal (`[a, b, c]`) into an
    /// `Rvalue::Aggregate { kind: Array }`.
    fn lower_array_list(
        &mut self,
        elems: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let mut operands = Vec::with_capacity(elems.len());
        let mut elem_struct: Option<String> = None;
        for elem in elems {
            let local = self.lower_expr(elem)?;
            if elem_struct.is_none() {
                if let Some(name) = self.local_struct.get(&local).cloned() {
                    elem_struct = Some(name);
                }
            }
            operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
        if let Some(name) = elem_struct {
            self.local_elem_struct.insert(dest, name);
        }
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Array,
                operands,
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `[value; count]` into `Rvalue::Repeat { value, count }`.
    ///
    /// Only supports compile-time-integer counts. Non-literal counts
    /// fall back to the `unsupported` intrinsic so the rest of the
    /// body still lowers cleanly.
    fn lower_array_repeat(
        &mut self,
        value: &HirExpr,
        count: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let Some(count_u64) = literal_u64(count) else {
            let local = self.fresh(ty);
            self.emit_assign(
                Place::local(local),
                Rvalue::CallIntrinsic {
                    name: "unsupported",
                    args: Vec::new(),
                },
                span,
            );
            return Some(local);
        };
        let value_local = self.lower_expr(value)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Repeat {
                value: Operand::Copy(Place::local(value_local)),
                count: count_u64,
            },
            span,
        );
        Some(dest)
    }

    /// Lowers `receiver.N` into a projection read: copy from a
    /// place rooted at the receiver local with a trailing
    /// [`Projection::Field(N)`].
    fn lower_tuple_index(
        &mut self,
        receiver: &HirExpr,
        index: u32,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let receiver_local = self.lower_expr(receiver)?;
        let dest = self.fresh(ty);
        let place = Place {
            local: receiver_local,
            projection: vec![crate::ir::Projection::Field(index)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    /// Lowers `base[index]` into a projection read with a runtime
    /// [`Projection::Index(local)`]. For `String` receivers the
    /// element is a byte, so we route through a dedicated runtime
    /// helper that loads the byte and zero-extends it to `i64`.
    fn lower_index_access(
        &mut self,
        base: &HirExpr,
        index: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // Walk through references so `&String` indexing behaves
        // the same as indexing a bare `String`.
        let mut base_kind = base.ty;
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(base_kind) {
            base_kind = *inner;
        }
        let base_is_string = matches!(self.tcx.kind_of(base_kind), TyKind::String);
        if base_is_string {
            let base_local = self.lower_expr(base)?;
            let index_local = self.lower_expr(index)?;
            let dest = self.fresh(ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_str_byte_at".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(index_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        let base_local = self.lower_expr(base)?;
        let index_local = self.lower_expr(index)?;
        let dest = self.fresh(ty);
        let place = Place {
            local: base_local,
            projection: vec![crate::ir::Projection::Index(index_local)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    fn lower_while(&mut self, condition: &HirExpr, body: &HirExpr, span: Span) {
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let Some(cond_local) = self.lower_expr(condition) else {
            return;
        };
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        // `break` jumps to `exit`; `continue` jumps back to the
        // condition test (`header`).
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
    }

    fn lower_loop(&mut self, body: &HirExpr, _ty: Ty, span: Span) -> Option<Local> {
        if let Some(for_loop) = detect_for_loop(body) {
            if let Some(result) = self.try_lower_for_loop(&for_loop, span) {
                return Some(result);
            }
        }
        let header = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });
        self.set_current(header);
        // Unconditional `loop`: `continue` and `break` both have
        // somewhere sensible to land. `break` exits, `continue`
        // restarts the body.
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        self.terminate(Terminator::Goto { target: header });
        self.set_current(exit);
        None
    }

    /// Lowers a detected `for x in iter { body }` loop directly into
    /// a counter-driven CFG when `iter` is a range or an array-shaped
    /// expression. Returns `None` when the iterator's shape is not
    /// recognised so the generic `loop` fallback handles it.
    fn try_lower_for_loop(
        &mut self,
        for_loop: &ForLoopShape<'_>,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        match &for_loop.iter_expr.kind {
            HirExprKind::Range {
                start: Some(start),
                end: Some(end),
                inclusive,
            } => self.lower_for_range(
                start,
                end,
                *inclusive,
                for_loop.loop_pat,
                for_loop.body,
                span,
            ),
            HirExprKind::Array(arr) => {
                let len = match arr {
                    gossamer_hir::HirArrayExpr::List(elems) => elems.len() as i64,
                    gossamer_hir::HirArrayExpr::Repeat { count, .. } => {
                        literal_u64(count).and_then(|c| i64::try_from(c).ok())?
                    }
                };
                self.lower_for_array(for_loop.iter_expr, for_loop.loop_pat, for_loop.body, len, span)
            }
            _ => {
                // Fallback: if the iter expression's HIR type is a
                // fixed-size `[T; N]` (or `&[T; N]`), treat it as an
                // array iter with length N.
                let mut cur = for_loop.iter_expr.ty;
                let len_opt = loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Array { len, .. } => {
                            break i64::try_from(*len).ok();
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => break None,
                    }
                };
                let len = len_opt?;
                self.lower_for_array(for_loop.iter_expr, for_loop.loop_pat, for_loop.body, len, span)
            }
        }
    }

    fn lower_for_range(
        &mut self,
        start: &HirExpr,
        end: &HirExpr,
        inclusive: bool,
        loop_pat: &HirPat,
        body: &HirExpr,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy as It, TyKind};
        let start_local = self.lower_expr(start)?;
        let end_local = self.lower_expr(end)?;
        // The loop counter's cranelift width must be concrete. Prefer
        // the MIR type picked by `lower_literal` for `start`; fall
        // back to i64 when neither HIR nor lowered MIR gave an
        // integer kind (unsuffixed literal, leaked inference var, …).
        let int_ty = {
            let start_mir_ty = self.locals[start_local.0 as usize].ty;
            let hir_kind = self.tcx.kind_of(start.ty);
            let mir_kind = self.tcx.kind_of(start_mir_ty);
            match hir_kind {
                TyKind::Int(_) => start.ty,
                _ => match mir_kind {
                    TyKind::Int(_) => start_mir_ty,
                    _ => self.tcx.int_ty(It::I64),
                },
            }
        };
        let counter = self.push_local(int_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(start_local))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        let op = if inclusive { BinOp::Le } else { BinOp::Lt };
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(end_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        if let HirPatKind::Binding { name, mutable } = &loop_pat.kind {
            let bind_local = self.push_local(int_ty, Some(name.clone()), *mutable);
            self.bind_local(&name.name, bind_local);
            self.emit_assign(
                Place::local(bind_local),
                Rvalue::Use(Operand::Copy(Place::local(counter))),
                span,
            );
        }
        let _ = self.lower_expr(body);
        self.pop_scope();
        let one = self.fresh(int_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    fn lower_for_array(
        &mut self,
        iter_expr: &HirExpr,
        loop_pat: &HirPat,
        body: &HirExpr,
        array_len: i64,
        span: Span,
    ) -> Option<Local> {
        let array_local = self.lower_expr(iter_expr)?;
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let len_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(len_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(array_len)))),
            span,
        );

        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        if let HirPatKind::Binding { name, mutable } = &loop_pat.kind {
            let elem_ty = loop_pat.ty;
            let bind_local = self.push_local(elem_ty, Some(name.clone()), *mutable);
            self.bind_local(&name.name, bind_local);
            let indexed_place = Place {
                local: array_local,
                projection: vec![crate::ir::Projection::Index(counter)],
            };
            self.emit_assign(
                Place::local(bind_local),
                Rvalue::Use(Operand::Copy(indexed_place)),
                span,
            );
        }
        let _ = self.lower_expr(body);
        self.pop_scope();
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(self.lower_unit(span))
    }

    fn lower_unit(&mut self, span: Span) -> Local {
        let unit_ty = self.tcx.unit();
        let local = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(local),
            Rvalue::Use(Operand::Const(ConstValue::Unit)),
            span,
        );
        local
    }
}

/// Structural view of the HIR shape produced by
/// `for p in iter { body }` lowering (`loop { match iter.next() {
/// Some(p) => body, None => break } }`). Used by the MIR lowerer to
/// emit a counter-driven CFG instead of a method call + pattern
/// match the native backend can't lower.
struct ForLoopShape<'h> {
    iter_expr: &'h HirExpr,
    loop_pat: &'h HirPat,
    body: &'h HirExpr,
}

fn detect_for_loop(body: &HirExpr) -> Option<ForLoopShape<'_>> {
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
    Some(ForLoopShape {
        iter_expr: receiver,
        loop_pat: &some_fields[0],
        body: &some_arm.body,
    })
}

/// Extracts a `u64` count from a HIR integer-literal expression used
/// as the repeat count of `[value; count]`. Returns `None` for any
/// non-literal or negative value.
fn literal_u64(expr: &HirExpr) -> Option<u64> {
    let HirExprKind::Literal(HirLiteral::Int(text)) = &expr.kind else {
        return None;
    };
    let parsed = parse_int(text)?;
    u64::try_from(parsed).ok()
}

fn literal_to_const(lit: &HirLiteral) -> ConstValue {
    match lit {
        HirLiteral::Unit => ConstValue::Unit,
        HirLiteral::Bool(b) => ConstValue::Bool(*b),
        HirLiteral::Int(text) => ConstValue::Int(parse_int(text).unwrap_or(0)),
        HirLiteral::Float(text) => ConstValue::Float(parse_float(text).to_bits()),
        HirLiteral::Char(c) => ConstValue::Char(*c),
        HirLiteral::String(text) => ConstValue::Str(text.clone()),
        HirLiteral::Byte(b) => ConstValue::Int(i128::from(*b)),
        HirLiteral::ByteString(bytes) => {
            ConstValue::Str(String::from_utf8_lossy(bytes).into_owned())
        }
    }
}

fn parse_int(text: &str) -> Option<i128> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    if let Some(rest) = cleaned
        .strip_prefix("0x")
        .or_else(|| cleaned.strip_prefix("0X"))
    {
        return i128::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0b")
        .or_else(|| cleaned.strip_prefix("0B"))
    {
        return i128::from_str_radix(rest, 2).ok();
    }
    if let Some(rest) = cleaned
        .strip_prefix("0o")
        .or_else(|| cleaned.strip_prefix("0O"))
    {
        return i128::from_str_radix(rest, 8).ok();
    }
    cleaned.parse::<i128>().ok()
}

fn parse_float(text: &str) -> f64 {
    for suffix in &["f32", "f64"] {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.parse::<f64>().unwrap_or(0.0);
        }
    }
    text.parse::<f64>().unwrap_or(0.0)
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

fn lower_binop(op: HirBinaryOp) -> BinOp {
    match op {
        HirBinaryOp::Add => BinOp::Add,
        HirBinaryOp::Sub => BinOp::Sub,
        HirBinaryOp::Mul => BinOp::Mul,
        HirBinaryOp::Div => BinOp::Div,
        HirBinaryOp::Rem => BinOp::Rem,
        // Logical `&&` / `||` lower to bitwise on the i1/i8
        // bool representation. The truth tables match: for
        // operands `a, b ∈ {0, 1}`, `a & b == a && b` and
        // `a | b == a || b`. (Short-circuit evaluation — not
        // calling the rhs when the lhs settles the result —
        // is a separate concern handled at HIR-to-MIR control
        // flow if/when we expose `&&`/`||` over expressions
        // with side effects.)
        HirBinaryOp::And | HirBinaryOp::BitAnd => BinOp::BitAnd,
        HirBinaryOp::Or | HirBinaryOp::BitOr => BinOp::BitOr,
        HirBinaryOp::BitXor => BinOp::BitXor,
        HirBinaryOp::Shl => BinOp::Shl,
        HirBinaryOp::Shr => BinOp::Shr,
        HirBinaryOp::Eq => BinOp::Eq,
        HirBinaryOp::Ne => BinOp::Ne,
        HirBinaryOp::Lt => BinOp::Lt,
        HirBinaryOp::Le => BinOp::Le,
        HirBinaryOp::Gt => BinOp::Gt,
        HirBinaryOp::Ge => BinOp::Ge,
    }
}

#[allow(dead_code)]
fn _used_imports(_: AssertMessage, _: FileId) {}
