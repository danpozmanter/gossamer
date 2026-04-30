//! AST → HIR lowering.

#![forbid(unsafe_code)]

use gossamer_ast::{
    ArrayExpr as AstArrayExpr, AssignOp, BinaryOp as AstBinOp, Block as AstBlock,
    ClosureParam as AstClosureParam, EnumDecl, Expr as AstExpr, ExprKind as AstExprKind,
    FieldPattern as AstFieldPat, FnDecl as AstFnDecl, FnParam as AstFnParam, Ident, ImplDecl,
    ImplItem, Item as AstItem, ItemKind as AstItemKind, Literal as AstLiteral, MatchArm,
    Mutability, NodeId, Pattern as AstPat, PatternKind as AstPatKind, SourceFile, Stmt as AstStmt,
    StmtKind as AstStmtKind, StructDecl, TraitDecl, TraitItem, UnaryOp,
};
use gossamer_lex::Span;
use gossamer_resolve::{Resolution, Resolutions};
use gossamer_types::{TyCtxt, TypeTable};

use crate::ids::{HirId, HirIdGenerator};
use crate::tree::{
    HirAdt, HirAdtKind, HirArrayExpr, HirBinaryOp, HirBlock, HirBody, HirConst, HirExpr,
    HirExprKind, HirFieldPat, HirFn, HirImpl, HirItem, HirItemKind, HirLiteral, HirMatchArm,
    HirParam, HirPat, HirPatKind, HirProgram, HirStatic, HirStmt, HirStmtKind, HirTrait,
    HirUnaryOp,
};

/// Lowers a resolved AST source file into HIR. The provided type table
/// annotates expression nodes with their inferred types; entries
/// missing from the table default to `TyCtxt::error_ty()`.
#[must_use]
pub fn lower_source_file(
    source: &SourceFile,
    resolutions: &Resolutions,
    table: &TypeTable,
    tcx: &mut TyCtxt,
) -> HirProgram {
    let mut lowerer = Lowerer {
        resolutions,
        table,
        tcx,
        ids: HirIdGenerator::new(),
    };
    let mut items = Vec::new();
    lower_items(&mut lowerer, &source.items, &mut items);
    HirProgram { items }
}

/// Flattens items in source order, descending into inline modules so
/// that `#[test]`-annotated functions inside `mod tests { ... }` reach
/// HIR (and thus the interpreter + test runner) the same way they
/// would if declared at the top level.
fn lower_items(lowerer: &mut Lowerer<'_>, items: &[AstItem], out: &mut Vec<HirItem>) {
    for item in items {
        if !gossamer_resolve::item_is_active(&item.attrs) {
            continue;
        }
        if let AstItemKind::Mod(decl) = &item.kind {
            if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                lower_items(lowerer, inner, out);
            }
            continue;
        }
        if let Some(lowered) = lowerer.lower_item(item) {
            out.push(lowered);
        }
    }
}

struct Lowerer<'a> {
    resolutions: &'a Resolutions,
    table: &'a TypeTable,
    tcx: &'a mut TyCtxt,
    ids: HirIdGenerator,
}

impl Lowerer<'_> {
    fn fresh(&mut self) -> HirId {
        self.ids.next()
    }

    fn ty_of(&mut self, node: NodeId) -> gossamer_types::Ty {
        self.table.get(node).unwrap_or_else(|| self.tcx.error_ty())
    }

    fn unit(&mut self) -> gossamer_types::Ty {
        self.tcx.unit()
    }

    fn error_ty(&mut self) -> gossamer_types::Ty {
        self.tcx.error_ty()
    }

    fn lower_item(&mut self, item: &AstItem) -> Option<HirItem> {
        let def = self.resolutions.definition_of(item.id);
        let kind = match &item.kind {
            AstItemKind::Fn(decl) => HirItemKind::Fn(self.lower_fn(decl, item.span)),
            AstItemKind::Const(decl) => HirItemKind::Const(HirConst {
                name: decl.name.clone(),
                ty: self.ty_of(decl.value.id),
                value: self.lower_expr(&decl.value),
            }),
            AstItemKind::Static(decl) => HirItemKind::Static(HirStatic {
                name: decl.name.clone(),
                ty: self.ty_of(decl.value.id),
                mutable: matches!(decl.mutability, Mutability::Mutable),
                value: self.lower_expr(&decl.value),
            }),
            AstItemKind::Struct(decl) => HirItemKind::Adt(self.lower_struct(decl)),
            AstItemKind::Enum(decl) => HirItemKind::Adt(self.lower_enum(decl)),
            AstItemKind::Impl(decl) => HirItemKind::Impl(self.lower_impl(decl, item.span)),
            AstItemKind::Trait(decl) => HirItemKind::Trait(self.lower_trait(decl, item.span)),
            AstItemKind::TypeAlias(_) | AstItemKind::Mod(_) | AstItemKind::AttrItem(_) => {
                return None;
            }
        };
        Some(HirItem {
            id: self.fresh(),
            span: item.span,
            def,
            kind,
        })
    }

    fn lower_fn(&mut self, decl: &AstFnDecl, span: Span) -> HirFn {
        self.lower_fn_with_self(decl, span, None)
    }

    /// Lowers an impl-method body with the impl's `Self` type
    /// applied to the `self` receiver. Lets MIR field-access
    /// lowering find the struct name on `self.field` reads
    /// without falling through to the unsupported placeholder.
    fn lower_fn_with_self(
        &mut self,
        decl: &AstFnDecl,
        span: Span,
        self_ty: Option<gossamer_types::Ty>,
    ) -> HirFn {
        let mut params = Vec::new();
        let mut has_self = false;
        for param in &decl.params {
            match param {
                AstFnParam::Receiver(_) => {
                    has_self = true;
                    let id = self.fresh();
                    let ty = self_ty.unwrap_or_else(|| self.error_ty());
                    params.push(HirParam {
                        pattern: HirPat {
                            id,
                            span,
                            ty,
                            kind: HirPatKind::Binding {
                                name: Ident::new("self"),
                                mutable: false,
                            },
                        },
                        ty,
                    });
                }
                AstFnParam::Typed {
                    pattern,
                    ty: ast_ty,
                } => {
                    let ty = self.ty_of(ast_ty.id);
                    let pattern = self.lower_pat_with_ty(pattern, ty);
                    params.push(HirParam { pattern, ty });
                }
            }
        }
        let ret = decl.ret.as_ref().map(|ty| self.ty_of(ty.id));
        let body = decl.body.as_ref().map(|body| HirBody {
            block: self.lower_expr_as_block(body),
        });
        HirFn {
            name: decl.name.clone(),
            params,
            ret,
            body,
            is_unsafe: decl.is_unsafe,
            has_self,
        }
    }

    fn lower_struct(&mut self, decl: &StructDecl) -> HirAdt {
        let ty = self.error_ty();
        let fields = match &decl.body {
            gossamer_ast::StructBody::Named(named) => {
                named.iter().map(|f| f.name.clone()).collect()
            }
            gossamer_ast::StructBody::Tuple(_) | gossamer_ast::StructBody::Unit => Vec::new(),
        };
        HirAdt {
            name: decl.name.clone(),
            kind: HirAdtKind::Struct(fields),
            self_ty: ty,
        }
    }

    fn lower_enum(&mut self, decl: &EnumDecl) -> HirAdt {
        let variants = decl
            .variants
            .iter()
            .map(|variant| {
                let (struct_fields, struct_field_tys) = match &variant.body {
                    gossamer_ast::StructBody::Named(fields) => {
                        let names: Vec<_> = fields.iter().map(|f| f.name.clone()).collect();
                        let tys: Vec<_> = fields.iter().map(|f| self.ty_of(f.ty.id)).collect();
                        (Some(names), Some(tys))
                    }
                    gossamer_ast::StructBody::Tuple(_) | gossamer_ast::StructBody::Unit => {
                        (None, None)
                    }
                };
                crate::tree::HirEnumVariant {
                    name: variant.name.clone(),
                    struct_fields,
                    struct_field_tys,
                }
            })
            .collect();
        let ty = self.error_ty();
        HirAdt {
            name: decl.name.clone(),
            kind: HirAdtKind::Enum(variants),
            self_ty: ty,
        }
    }

    fn lower_impl(&mut self, decl: &ImplDecl, span: Span) -> HirImpl {
        let self_ty = self.ty_of(decl.self_ty.id);
        let self_name = match &decl.self_ty.kind {
            gossamer_ast::TypeKind::Path(path) => path.segments.last().map(|seg| seg.name.clone()),
            _ => None,
        };
        let trait_name = decl
            .trait_ref
            .as_ref()
            .and_then(|bound| bound.path.segments.last())
            .map(|seg| seg.name.clone());
        let methods = decl
            .items
            .iter()
            .filter_map(|item| match item {
                ImplItem::Fn(fn_decl) => {
                    Some(self.lower_fn_with_self(fn_decl, span, Some(self_ty)))
                }
                ImplItem::Const { .. } | ImplItem::Type { .. } => None,
            })
            .collect();
        HirImpl {
            self_ty,
            self_name,
            trait_name,
            methods,
        }
    }

    fn lower_trait(&mut self, decl: &TraitDecl, span: Span) -> HirTrait {
        let methods = decl
            .items
            .iter()
            .filter_map(|item| match item {
                TraitItem::Fn(fn_decl) => Some(self.lower_fn(fn_decl, span)),
                TraitItem::Type { .. } | TraitItem::Const { .. } => None,
            })
            .collect();
        HirTrait {
            name: decl.name.clone(),
            methods,
        }
    }

    fn lower_expr(&mut self, expr: &AstExpr) -> HirExpr {
        use gossamer_types::TyKind;
        let mut ty = self.ty_of(expr.id);
        let span = expr.span;
        let kind = self.lower_expr_kind(expr);
        // `?`-unwrap leaves the typechecker's assigned type for the
        // outer Match unresolved when the inner Result wasn't
        // pinned. Pull the Ok-arm body's type up so any binding
        // bound to the `?`-expression carries something concrete
        // (typically String). Without this, a `let s = fs::
        // read_to_string(...)?; s.len()` lands on the generic
        // `gos_rt_len` instead of `gos_rt_str_len` and reads garbage.
        if matches!(self.tcx.kind(ty), Some(TyKind::Error | TyKind::Var(_))) {
            if let HirExprKind::Match { arms, .. } = &kind {
                if let Some(first) = arms.first() {
                    let arm_ty = first.body.ty;
                    if !matches!(self.tcx.kind(arm_ty), Some(TyKind::Error | TyKind::Var(_))) {
                        ty = arm_ty;
                    }
                }
            }
        }
        HirExpr {
            id: self.fresh(),
            span,
            ty,
            kind,
        }
    }

    fn lower_expr_kind(&mut self, expr: &AstExpr) -> HirExprKind {
        match &expr.kind {
            AstExprKind::Literal(lit) => HirExprKind::Literal(lower_literal(lit)),
            AstExprKind::Path(path) => self.lower_path_expr(expr.id, path),
            AstExprKind::Call { callee, args } => HirExprKind::Call {
                callee: Box::new(self.lower_expr(callee)),
                args: args.iter().map(|a| self.lower_expr(a)).collect(),
            },
            AstExprKind::MethodCall {
                receiver,
                name,
                args,
                ..
            } => HirExprKind::MethodCall {
                receiver: Box::new(self.lower_expr(receiver)),
                name: name.clone(),
                args: args.iter().map(|a| self.lower_expr(a)).collect(),
            },
            AstExprKind::FieldAccess { receiver, field } => self.lower_field(receiver, field),
            AstExprKind::Index { base, index } => HirExprKind::Index {
                base: Box::new(self.lower_expr(base)),
                index: Box::new(self.lower_expr(index)),
            },
            AstExprKind::Unary { op, operand } => HirExprKind::Unary {
                op: lower_unary_op(*op),
                operand: Box::new(self.lower_expr(operand)),
            },
            AstExprKind::Binary { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs),
            AstExprKind::Assign { op, place, value } => self.lower_assign(*op, place, value, expr),
            AstExprKind::Cast { value, ty: ast_ty } => HirExprKind::Cast {
                value: Box::new(self.lower_expr(value)),
                ty: self.ty_of(ast_ty.id),
            },
            AstExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => HirExprKind::If {
                condition: Box::new(self.lower_expr(condition)),
                then_branch: Box::new(self.lower_expr(then_branch)),
                else_branch: else_branch.as_ref().map(|e| Box::new(self.lower_expr(e))),
            },
            AstExprKind::Match { scrutinee, arms } => HirExprKind::Match {
                scrutinee: Box::new(self.lower_expr(scrutinee)),
                arms: arms.iter().map(|arm| self.lower_match_arm(arm)).collect(),
            },
            AstExprKind::Loop { body, .. } => HirExprKind::Loop {
                body: Box::new(self.lower_expr(body)),
            },
            AstExprKind::While {
                condition, body, ..
            } => HirExprKind::While {
                condition: Box::new(self.lower_expr(condition)),
                body: Box::new(self.lower_expr(body)),
            },
            AstExprKind::For {
                pattern,
                iter,
                body,
                ..
            } => self.lower_for(pattern, iter, body, expr.span),
            AstExprKind::Block(block) | AstExprKind::Unsafe(block) => {
                HirExprKind::Block(self.lower_block(block, expr.span))
            }
            AstExprKind::Closure { params, ret, body } => HirExprKind::Closure {
                params: self.lower_closure_params(params),
                ret: ret.as_ref().map(|ty| self.ty_of(ty.id)),
                body: Box::new(self.lower_expr(body)),
            },
            AstExprKind::Return(value) => {
                HirExprKind::Return(value.as_ref().map(|v| Box::new(self.lower_expr(v))))
            }
            AstExprKind::Break { value, .. } => {
                HirExprKind::Break(value.as_ref().map(|v| Box::new(self.lower_expr(v))))
            }
            AstExprKind::Continue { .. } => HirExprKind::Continue,
            AstExprKind::Tuple(elems) => {
                HirExprKind::Tuple(elems.iter().map(|e| self.lower_expr(e)).collect())
            }
            AstExprKind::Select(arms) => self.lower_select(arms),
            AstExprKind::Struct { path, fields, .. } => {
                self.lower_struct_literal(path, fields, expr.span)
            }
            AstExprKind::MacroCall(_) => HirExprKind::Placeholder,
            AstExprKind::Array(arr) => HirExprKind::Array(self.lower_array(arr)),
            AstExprKind::Range {
                start, end, kind, ..
            } => HirExprKind::Range {
                start: start.as_ref().map(|s| Box::new(self.lower_expr(s))),
                end: end.as_ref().map(|e| Box::new(self.lower_expr(e))),
                inclusive: matches!(kind, gossamer_ast::RangeKind::Inclusive),
            },
            AstExprKind::Try(inner) => self.lower_try(inner, expr.span),
            AstExprKind::Go(inner) => HirExprKind::Go(Box::new(self.lower_expr(inner))),
        }
    }

    fn lower_binary(&mut self, op: AstBinOp, lhs: &AstExpr, rhs: &AstExpr) -> HirExprKind {
        if matches!(op, AstBinOp::PipeGt) {
            return self.lower_pipe(lhs, rhs);
        }
        HirExprKind::Binary {
            op: lower_binary_op(op),
            lhs: Box::new(self.lower_expr(lhs)),
            rhs: Box::new(self.lower_expr(rhs)),
        }
    }

    fn lower_pipe(&mut self, lhs: &AstExpr, rhs: &AstExpr) -> HirExprKind {
        let piped = self.lower_expr(lhs);
        match &rhs.kind {
            AstExprKind::Call { callee, args } => {
                let mut new_args: Vec<HirExpr> = args.iter().map(|a| self.lower_expr(a)).collect();
                new_args.push(piped);
                HirExprKind::Call {
                    callee: Box::new(self.lower_expr(callee)),
                    args: new_args,
                }
            }
            AstExprKind::MethodCall {
                receiver,
                name,
                args,
                ..
            } => {
                let mut new_args: Vec<HirExpr> = args.iter().map(|a| self.lower_expr(a)).collect();
                new_args.push(piped);
                HirExprKind::MethodCall {
                    receiver: Box::new(self.lower_expr(receiver)),
                    name: name.clone(),
                    args: new_args,
                }
            }
            AstExprKind::Path(_) => HirExprKind::Call {
                callee: Box::new(self.lower_expr(rhs)),
                args: vec![piped],
            },
            _ => HirExprKind::Placeholder,
        }
    }

    fn lower_assign(
        &mut self,
        op: AssignOp,
        place: &AstExpr,
        value: &AstExpr,
        outer: &AstExpr,
    ) -> HirExprKind {
        if matches!(op, AssignOp::Assign) {
            return HirExprKind::Assign {
                place: Box::new(self.lower_expr(place)),
                value: Box::new(self.lower_expr(value)),
            };
        }
        let lowered_place = self.lower_expr(place);
        let lowered_value = self.lower_expr(value);
        let bin_op = compound_assign_to_binary(op);
        let place_ty = lowered_place.ty;
        let value_ty = lowered_value.ty;
        let bin_expr = HirExpr {
            id: self.fresh(),
            span: outer.span,
            ty: place_ty,
            kind: HirExprKind::Binary {
                op: bin_op,
                lhs: Box::new(lowered_place.clone()),
                rhs: Box::new(HirExpr {
                    ty: value_ty,
                    ..lowered_value
                }),
            },
        };
        HirExprKind::Assign {
            place: Box::new(lowered_place),
            value: Box::new(bin_expr),
        }
    }

    fn lower_field(
        &mut self,
        receiver: &AstExpr,
        field: &gossamer_ast::FieldSelector,
    ) -> HirExprKind {
        let lowered = self.lower_expr(receiver);
        match field {
            gossamer_ast::FieldSelector::Named(name) => HirExprKind::Field {
                receiver: Box::new(lowered),
                name: name.clone(),
            },
            gossamer_ast::FieldSelector::Index(idx) => HirExprKind::TupleIndex {
                receiver: Box::new(lowered),
                index: *idx,
            },
        }
    }

    fn lower_match_arm(&mut self, arm: &MatchArm) -> HirMatchArm {
        HirMatchArm {
            pattern: self.lower_pat(&arm.pattern),
            guard: arm.guard.as_ref().map(|g| self.lower_expr(g)),
            body: self.lower_expr(&arm.body),
        }
    }

    fn lower_for(
        &mut self,
        pattern: &AstPat,
        iter: &AstExpr,
        body: &AstExpr,
        span: Span,
    ) -> HirExprKind {
        let iter_expr = self.lower_expr(iter);
        let iter_ty = iter_expr.ty;
        let next_call = HirExpr {
            id: self.fresh(),
            span,
            ty: self.error_ty(),
            kind: HirExprKind::MethodCall {
                receiver: Box::new(iter_expr),
                name: Ident::new("next"),
                args: Vec::new(),
            },
        };
        let loop_pat = self.lower_pat(pattern);
        let pat_ty = loop_pat.ty;
        let some_pat = HirPat {
            id: self.fresh(),
            span,
            ty: pat_ty,
            kind: HirPatKind::Variant {
                name: Ident::new("Some"),
                fields: vec![loop_pat],
            },
        };
        let none_pat = HirPat {
            id: self.fresh(),
            span,
            ty: pat_ty,
            kind: HirPatKind::Variant {
                name: Ident::new("None"),
                fields: Vec::new(),
            },
        };
        let body_expr = self.lower_expr(body);
        let unit_ty = self.unit();
        let break_expr = HirExpr {
            id: self.fresh(),
            span,
            ty: self.tcx.never(),
            kind: HirExprKind::Break(None),
        };
        let match_expr = HirExpr {
            id: self.fresh(),
            span,
            ty: unit_ty,
            kind: HirExprKind::Match {
                scrutinee: Box::new(next_call),
                arms: vec![
                    HirMatchArm {
                        pattern: some_pat,
                        guard: None,
                        body: body_expr,
                    },
                    HirMatchArm {
                        pattern: none_pat,
                        guard: None,
                        body: break_expr,
                    },
                ],
            },
        };
        let block = HirBlock {
            id: self.fresh(),
            span,
            stmts: Vec::new(),
            tail: Some(Box::new(match_expr)),
            ty: unit_ty,
        };
        let body_block = HirExpr {
            id: self.fresh(),
            span,
            ty: unit_ty,
            kind: HirExprKind::Block(block),
        };
        let _ = iter_ty;
        HirExprKind::Loop {
            body: Box::new(body_block),
        }
    }

    /// Lowers a `select { … }` expression into a
    /// [`HirExprKind::Select`] that preserves each arm's channel and
    /// body. The interpreter polls channels for readiness at runtime
    /// and picks the first ready arm, falling back to the `default`
    /// arm when none are ready.
    fn lower_select(&mut self, arms: &[gossamer_ast::SelectArm]) -> HirExprKind {
        if arms.is_empty() {
            return HirExprKind::Literal(HirLiteral::Unit);
        }
        let lowered = arms
            .iter()
            .map(|arm| {
                let op = match &arm.op {
                    gossamer_ast::SelectOp::Recv { pattern, channel } => {
                        crate::tree::HirSelectOp::Recv {
                            pattern: self.lower_pat(pattern),
                            channel: self.lower_expr(channel),
                        }
                    }
                    gossamer_ast::SelectOp::Send { channel, value } => {
                        crate::tree::HirSelectOp::Send {
                            channel: self.lower_expr(channel),
                            value: self.lower_expr(value),
                        }
                    }
                    gossamer_ast::SelectOp::Default => crate::tree::HirSelectOp::Default,
                };
                crate::tree::HirSelectArm {
                    op,
                    body: self.lower_expr(&arm.body),
                }
            })
            .collect();
        HirExprKind::Select { arms: lowered }
    }

    /// Returns the `T` payload type when `ty` is a `Result<T, E>`
    /// (or a `&Result<T, E>`), `None` otherwise. Used by `lower_try`
    /// so a `?`-unwrapped binding inherits a real type instead of
    /// the `Error` sentinel.
    fn try_ok_payload_ty(&self, ty: gossamer_types::Ty) -> Option<gossamer_types::Ty> {
        use gossamer_types::TyKind;
        let mut peeled = ty;
        loop {
            match self.tcx.kind(peeled)? {
                TyKind::Ref { inner, .. } => peeled = *inner,
                TyKind::Adt { substs, .. } => {
                    let args = substs.as_slice();
                    if args.is_empty() {
                        return None;
                    }
                    if let Some(gossamer_types::GenericArg::Type(t)) = args.first() {
                        return Some(*t);
                    }
                    return None;
                }
                _ => return None,
            }
        }
    }

    /// Heuristic fallback for `?` operator's `__try_value` type
    /// when the inner expression's HIR type is unresolved. Walks
    /// chained method calls — `fs::read_to_string(...)
    /// .map_err(...)` is a common shape — and returns `String`
    /// for stdlib helpers whose runtime return is a c-string. The
    /// MIR-side `pinned_ret` table is the authoritative source of
    /// truth; this list mirrors its String entries so the HIR layer
    /// can ground a `let s = ...?` binding even when the
    /// typechecker leaks a Var through `?`.
    fn try_ok_payload_ty_heuristic(&mut self, inner: &AstExpr) -> Option<gossamer_types::Ty> {
        let mut cur = inner;
        loop {
            match &cur.kind {
                AstExprKind::MethodCall { receiver, name, .. }
                    if matches!(name.name.as_str(), "map_err" | "map" | "ok" | "err") =>
                {
                    cur = receiver;
                }
                AstExprKind::Call { callee, .. } => {
                    if let AstExprKind::Path(path) = &callee.kind {
                        let joined: Vec<&str> =
                            path.segments.iter().map(|s| s.name.name.as_str()).collect();
                        let last = *joined.last()?;
                        // Match the same names the parse-side
                        // resolves to gos_rt_*_-returning helpers
                        // whose c-string return is logically a
                        // String. If the MIR pin gets it right we
                        // never reach here; this is the last-ditch
                        // path for when the typechecker hasn't
                        // resolved through `?`.
                        if matches!(
                            last,
                            "read_to_string"
                                | "read_line"
                                | "trim"
                                | "to_lowercase"
                                | "to_uppercase"
                                | "replace"
                                | "format"
                                | "join"
                        ) {
                            return Some(self.tcx.string_ty());
                        }
                    }
                    return None;
                }
                _ => return None,
            }
        }
    }

    fn lower_try(&mut self, inner: &AstExpr, span: Span) -> HirExprKind {
        let value = self.lower_expr(inner);
        let value_ty = value.ty;
        // Recover the Ok-payload type from `value_ty` so the
        // `__try_value` binding (and thus any downstream `let` bound
        // to the `?`-expression) carries a real type rather than the
        // `Error` sentinel. Without this `let s = fs::read_to_string
        // (...)?` leaves `s` typed as Error/Var and `s.len()` falls
        // off the dispatch table into `gos_rt_len` instead of the
        // String-shaped `gos_rt_str_len`. We peek through the
        // dispatch table for `Result<T, E>` ADTs and any aliasing
        // refs; everything else falls back to the original
        // `error_ty` so behaviour is unchanged for unresolved
        // shapes.
        let ok_payload_ty = self
            .try_ok_payload_ty(value_ty)
            .or_else(|| self.try_ok_payload_ty_heuristic(inner));
        let try_value_ty = ok_payload_ty.unwrap_or_else(|| self.error_ty());
        let ok_binding_id = self.fresh();
        let err_binding_id = self.fresh();
        let ok_pat = HirPat {
            id: self.fresh(),
            span,
            ty: value_ty,
            kind: HirPatKind::Variant {
                name: Ident::new("Ok"),
                fields: vec![HirPat {
                    id: ok_binding_id,
                    span,
                    ty: try_value_ty,
                    kind: HirPatKind::Binding {
                        name: Ident::new("__try_value"),
                        mutable: false,
                    },
                }],
            },
        };
        let err_pat = HirPat {
            id: self.fresh(),
            span,
            ty: value_ty,
            kind: HirPatKind::Variant {
                name: Ident::new("Err"),
                fields: vec![HirPat {
                    id: err_binding_id,
                    span,
                    ty: self.error_ty(),
                    kind: HirPatKind::Binding {
                        name: Ident::new("__try_err"),
                        mutable: false,
                    },
                }],
            },
        };
        let ok_body = HirExpr {
            id: self.fresh(),
            span,
            ty: try_value_ty,
            kind: HirExprKind::Path {
                segments: vec![Ident::new("__try_value")],
                def: None,
            },
        };
        let err_value = HirExpr {
            id: self.fresh(),
            span,
            ty: self.error_ty(),
            kind: HirExprKind::Path {
                segments: vec![Ident::new("__try_err")],
                def: None,
            },
        };
        let err_wrap = HirExpr {
            id: self.fresh(),
            span,
            ty: self.error_ty(),
            kind: HirExprKind::Call {
                callee: Box::new(HirExpr {
                    id: self.fresh(),
                    span,
                    ty: self.error_ty(),
                    kind: HirExprKind::Path {
                        segments: vec![Ident::new("Err")],
                        def: None,
                    },
                }),
                args: vec![err_value],
            },
        };
        let err_body = HirExpr {
            id: self.fresh(),
            span,
            ty: self.tcx.never(),
            kind: HirExprKind::Return(Some(Box::new(err_wrap))),
        };
        HirExprKind::Match {
            scrutinee: Box::new(value),
            arms: vec![
                HirMatchArm {
                    pattern: ok_pat,
                    guard: None,
                    body: ok_body,
                },
                HirMatchArm {
                    pattern: err_pat,
                    guard: None,
                    body: err_body,
                },
            ],
        }
    }

    /// Lowers `Path { field: value, … }` into a call to the synthetic
    /// `__struct` builtin. The resulting argument list interleaves
    /// field-name strings with their lowered value expressions:
    ///
    /// `Shape::Rect { w: 2.0, h: 4.0 }` → `__struct("Rect", "w", 2.0, "h", 4.0)`.
    ///
    /// Interpreter and codegen layers can recognise `__struct` as the
    /// canonical struct-literal constructor without needing a new HIR
    /// node variant.
    fn lower_struct_literal(
        &mut self,
        path: &gossamer_ast::PathExpr,
        fields: &[gossamer_ast::StructExprField],
        span: Span,
    ) -> HirExprKind {
        let name = path
            .segments
            .last()
            .map(|seg| seg.name.name.clone())
            .unwrap_or_default();
        let error_ty = self.error_ty();
        let string_ty = self.error_ty();
        let mut args = Vec::with_capacity(1 + fields.len() * 2);
        args.push(HirExpr {
            id: self.fresh(),
            span,
            ty: string_ty,
            kind: HirExprKind::Literal(HirLiteral::String(name)),
        });
        for field in fields {
            args.push(HirExpr {
                id: self.fresh(),
                span,
                ty: string_ty,
                kind: HirExprKind::Literal(HirLiteral::String(field.name.name.clone())),
            });
            let value = match &field.value {
                Some(expr) => self.lower_expr(expr),
                None => HirExpr {
                    id: self.fresh(),
                    span,
                    ty: error_ty,
                    kind: HirExprKind::Path {
                        segments: vec![field.name.clone()],
                        def: None,
                    },
                },
            };
            args.push(value);
        }
        HirExprKind::Call {
            callee: Box::new(HirExpr {
                id: self.fresh(),
                span,
                ty: error_ty,
                kind: HirExprKind::Path {
                    segments: vec![Ident::new("__struct")],
                    def: None,
                },
            }),
            args,
        }
    }

    fn lower_array(&mut self, arr: &AstArrayExpr) -> HirArrayExpr {
        match arr {
            AstArrayExpr::List(elems) => {
                HirArrayExpr::List(elems.iter().map(|e| self.lower_expr(e)).collect())
            }
            AstArrayExpr::Repeat { value, count } => HirArrayExpr::Repeat {
                value: Box::new(self.lower_expr(value)),
                count: Box::new(self.lower_expr(count)),
            },
        }
    }

    fn lower_closure_params(&mut self, params: &[AstClosureParam]) -> Vec<HirParam> {
        params
            .iter()
            .map(|param| {
                let ty = match &param.ty {
                    Some(ast_ty) => self.ty_of(ast_ty.id),
                    None => self.error_ty(),
                };
                let pattern = self.lower_pat_with_ty(&param.pattern, ty);
                HirParam { pattern, ty }
            })
            .collect()
    }

    fn lower_path_expr(&mut self, node: NodeId, path: &gossamer_ast::PathExpr) -> HirExprKind {
        let segments: Vec<Ident> = path.segments.iter().map(|s| s.name.clone()).collect();
        let def = match self.resolutions.get(node) {
            Some(Resolution::Def { def, .. }) => Some(def),
            _ => None,
        };
        HirExprKind::Path { segments, def }
    }

    fn lower_block(&mut self, block: &AstBlock, span: Span) -> HirBlock {
        let mut stmts = Vec::new();
        for stmt in &block.stmts {
            stmts.push(self.lower_stmt(stmt));
        }
        let tail = block
            .tail
            .as_ref()
            .map(|tail| Box::new(self.lower_expr(tail)));
        let ty = match tail.as_ref() {
            Some(expr) => expr.ty,
            None => self.unit(),
        };
        HirBlock {
            id: self.fresh(),
            span,
            stmts,
            tail,
            ty,
        }
    }

    fn lower_expr_as_block(&mut self, expr: &AstExpr) -> HirBlock {
        if let AstExprKind::Block(block) = &expr.kind {
            return self.lower_block(block, expr.span);
        }
        let lowered = self.lower_expr(expr);
        let ty = lowered.ty;
        HirBlock {
            id: self.fresh(),
            span: expr.span,
            stmts: Vec::new(),
            tail: Some(Box::new(lowered)),
            ty,
        }
    }

    fn lower_stmt(&mut self, stmt: &AstStmt) -> HirStmt {
        let kind = match &stmt.kind {
            AstStmtKind::Let { pattern, ty, init } => {
                let declared_ty = match ty.as_ref() {
                    Some(ast_ty) => self.ty_of(ast_ty.id),
                    None => self.error_ty(),
                };
                let init = init.as_ref().map(|expr| self.lower_expr(expr));
                // Prefer the user-written annotation over the
                // initialiser's inferred type — the annotation is
                // already what the typechecker unified the init
                // expression against, and a concrete `Result<T, E>`
                // / `Option<T>` annotation is much more useful to
                // MIR's downstream `Adt` substs lookups than the
                // raw `Var(_)` an inference variable would carry
                // through. Falls back to the init's type when no
                // annotation was written.
                let pattern_ty =
                    if matches!(self.tcx.kind_of(declared_ty), gossamer_types::TyKind::Error) {
                        init.as_ref().map_or(declared_ty, |expr| expr.ty)
                    } else {
                        declared_ty
                    };
                let pattern = self.lower_pat_with_ty(pattern, pattern_ty);
                HirStmtKind::Let {
                    pattern,
                    ty: pattern_ty,
                    init,
                }
            }
            AstStmtKind::Expr { expr, has_semi } => HirStmtKind::Expr {
                expr: self.lower_expr(expr),
                has_semi: *has_semi,
            },
            AstStmtKind::Item(item) => {
                if let Some(lowered) = self.lower_item(item) {
                    HirStmtKind::Item(Box::new(lowered))
                } else {
                    HirStmtKind::Expr {
                        expr: self.placeholder_expr(stmt.span),
                        has_semi: false,
                    }
                }
            }
            AstStmtKind::Defer(inner) => HirStmtKind::Defer(self.lower_expr(inner)),
            AstStmtKind::Go(inner) => HirStmtKind::Go(self.lower_expr(inner)),
        };
        HirStmt {
            id: self.fresh(),
            span: stmt.span,
            kind,
        }
    }

    fn placeholder_expr(&mut self, span: Span) -> HirExpr {
        let ty = self.unit();
        HirExpr {
            id: self.fresh(),
            span,
            ty,
            kind: HirExprKind::Placeholder,
        }
    }

    fn lower_pat(&mut self, pattern: &AstPat) -> HirPat {
        let ty = self.ty_of(pattern.id);
        self.lower_pat_with_ty(pattern, ty)
    }

    fn lower_pat_with_ty(&mut self, pattern: &AstPat, ty: gossamer_types::Ty) -> HirPat {
        let kind = self.lower_pat_kind(pattern, ty);
        HirPat {
            id: self.fresh(),
            span: pattern.span,
            ty,
            kind,
        }
    }

    fn lower_pat_kind(&mut self, pattern: &AstPat, ty: gossamer_types::Ty) -> HirPatKind {
        match &pattern.kind {
            AstPatKind::Wildcard => HirPatKind::Wildcard,
            AstPatKind::Rest => HirPatKind::Rest,
            AstPatKind::Ident {
                name, mutability, ..
            } => HirPatKind::Binding {
                name: name.clone(),
                mutable: matches!(mutability, Mutability::Mutable),
            },
            AstPatKind::Literal(lit) => HirPatKind::Literal(lower_literal(lit)),
            AstPatKind::Path(path) => HirPatKind::Variant {
                name: path
                    .segments
                    .last()
                    .map_or_else(|| Ident::new("<error>"), |seg| seg.name.clone()),
                fields: Vec::new(),
            },
            AstPatKind::TupleStruct { path, elems } => HirPatKind::Variant {
                name: path
                    .segments
                    .last()
                    .map_or_else(|| Ident::new("<error>"), |seg| seg.name.clone()),
                fields: elems.iter().map(|p| self.lower_pat(p)).collect(),
            },
            AstPatKind::Struct { path, fields, rest } => HirPatKind::Struct {
                name: path
                    .segments
                    .last()
                    .map_or_else(|| Ident::new("<error>"), |seg| seg.name.clone()),
                fields: fields.iter().map(|f| self.lower_field_pat(f)).collect(),
                rest: *rest,
            },
            AstPatKind::Tuple(parts) => {
                HirPatKind::Tuple(parts.iter().map(|p| self.lower_pat(p)).collect())
            }
            AstPatKind::Or(alts) => {
                HirPatKind::Or(alts.iter().map(|p| self.lower_pat(p)).collect())
            }
            AstPatKind::Ref { inner, mutability } => HirPatKind::Ref {
                inner: Box::new(self.lower_pat(inner)),
                mutable: matches!(mutability, Mutability::Mutable),
            },
            AstPatKind::Range { lo, hi, kind } => HirPatKind::Range {
                lo: lower_literal(lo),
                hi: lower_literal(hi),
                inclusive: matches!(kind, gossamer_ast::RangeKind::Inclusive),
            },
        }
        .erase_unused(ty)
    }

    fn lower_field_pat(&mut self, field: &AstFieldPat) -> HirFieldPat {
        HirFieldPat {
            name: field.name.clone(),
            pattern: field.pattern.as_ref().map(|p| self.lower_pat(p)),
        }
    }
}

trait PatKindExt {
    fn erase_unused(self, ty: gossamer_types::Ty) -> Self;
}

impl PatKindExt for HirPatKind {
    fn erase_unused(self, _ty: gossamer_types::Ty) -> Self {
        self
    }
}

fn lower_literal(lit: &AstLiteral) -> HirLiteral {
    match lit {
        AstLiteral::Int(text) => HirLiteral::Int(text.clone()),
        AstLiteral::Float(text) => HirLiteral::Float(text.clone()),
        AstLiteral::String(text) => HirLiteral::String(text.clone()),
        AstLiteral::RawString { value, .. } => HirLiteral::String(value.clone()),
        AstLiteral::Char(c) => HirLiteral::Char(*c),
        AstLiteral::Byte(b) => HirLiteral::Byte(*b),
        AstLiteral::ByteString(bytes) => HirLiteral::ByteString(bytes.clone()),
        AstLiteral::RawByteString { value, .. } => HirLiteral::ByteString(value.clone()),
        AstLiteral::Bool(b) => HirLiteral::Bool(*b),
        AstLiteral::Unit => HirLiteral::Unit,
    }
}

fn lower_unary_op(op: UnaryOp) -> HirUnaryOp {
    match op {
        UnaryOp::Neg => HirUnaryOp::Neg,
        UnaryOp::Not => HirUnaryOp::Not,
        UnaryOp::RefShared => HirUnaryOp::RefShared,
        UnaryOp::RefMut => HirUnaryOp::RefMut,
        UnaryOp::Deref => HirUnaryOp::Deref,
    }
}

/// Maps concrete binary operators to their HIR form. `PipeGt` is
/// lowered separately via [`Lowerer::lower_pipe`] before this helper is
/// called, so the mapping never sees it.
fn lower_binary_op(op: AstBinOp) -> HirBinaryOp {
    match op {
        AstBinOp::Add | AstBinOp::PipeGt => HirBinaryOp::Add,
        AstBinOp::Sub => HirBinaryOp::Sub,
        AstBinOp::Mul => HirBinaryOp::Mul,
        AstBinOp::Div => HirBinaryOp::Div,
        AstBinOp::Rem => HirBinaryOp::Rem,
        AstBinOp::BitAnd => HirBinaryOp::BitAnd,
        AstBinOp::BitOr => HirBinaryOp::BitOr,
        AstBinOp::BitXor => HirBinaryOp::BitXor,
        AstBinOp::Shl => HirBinaryOp::Shl,
        AstBinOp::Shr => HirBinaryOp::Shr,
        AstBinOp::Eq => HirBinaryOp::Eq,
        AstBinOp::Ne => HirBinaryOp::Ne,
        AstBinOp::Lt => HirBinaryOp::Lt,
        AstBinOp::Le => HirBinaryOp::Le,
        AstBinOp::Gt => HirBinaryOp::Gt,
        AstBinOp::Ge => HirBinaryOp::Ge,
        AstBinOp::And => HirBinaryOp::And,
        AstBinOp::Or => HirBinaryOp::Or,
    }
}

fn compound_assign_to_binary(op: AssignOp) -> HirBinaryOp {
    match op {
        AssignOp::Assign | AssignOp::AddAssign => HirBinaryOp::Add,
        AssignOp::SubAssign => HirBinaryOp::Sub,
        AssignOp::MulAssign => HirBinaryOp::Mul,
        AssignOp::DivAssign => HirBinaryOp::Div,
        AssignOp::RemAssign => HirBinaryOp::Rem,
        AssignOp::BitAndAssign => HirBinaryOp::BitAnd,
        AssignOp::BitOrAssign => HirBinaryOp::BitOr,
        AssignOp::BitXorAssign => HirBinaryOp::BitXor,
        AssignOp::ShlAssign => HirBinaryOp::Shl,
        AssignOp::ShrAssign => HirBinaryOp::Shr,
    }
}
