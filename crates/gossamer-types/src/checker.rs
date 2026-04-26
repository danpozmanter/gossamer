//! Type checker and inference driver.
//! Walks a parsed and name-resolved [`SourceFile`], assigns a [`Ty`]
//! handle to every expression and pattern, and records obvious
//! type-equality mismatches as diagnostics.
//! The implementation is deliberately lenient where later phases will
//! add strength: unresolved methods, operators on non-primitive types,
//! and external stdlib references fall back to fresh inference
//! variables instead of emitting diagnostics. Only conflicts between
//! two known-concrete types are reported. This keeps the checker
//! quiet on programs that reach heavily into the stdlib before the
//! trait solver arrives.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_ast::{
    ArrayExpr, BinaryOp, Block, ClosureParam, Expr, ExprKind, FieldPattern, FnDecl, FnParam,
    GenericArg as AstGenericArg, ImplDecl, ImplItem, Item, ItemKind, Literal, MatchArm, NodeId,
    Pattern, PatternKind, SourceFile, Stmt, StmtKind, StructBody, TraitItem, Type as AstType,
    TypeKind as AstTypeKind, TypePath, UnaryOp,
};
use gossamer_lex::Span;
use gossamer_resolve::{FloatWidth, IntWidth, PrimitiveTy, Resolution, Resolutions};

use crate::context::TyCtxt;
use crate::error::{TypeDiagnostic, TypeError};
use crate::infer::{InferCtxt, UnifyError};
use crate::printer::render_ty;
use crate::table::TypeTable;
use crate::ty::{FloatTy, FnSig, IntTy, Mutbl, Ty, TyKind};

/// Runs type inference on `source` using the name-resolution output in
/// `resolutions` and the shared type interner `tcx`.
#[must_use]
pub fn typecheck_source_file(
    source: &SourceFile,
    resolutions: &Resolutions,
    tcx: &mut TyCtxt,
) -> (TypeTable, Vec<TypeDiagnostic>) {
    let mut checker = TypeChecker::new(tcx, resolutions);
    checker.collect_signatures(&source.items);
    for item in &source.items {
        checker.check_item(item);
    }
    // Default any integer-constrained inference variables that
    // remain unresolved to `i64`. This gives unsuffixed literals
    // (`let x = 42`) a concrete type when no use-site forced the
    // width.
    checker.infer.default_unresolved_int_vars(checker.tcx);
    checker.resolve_table();
    (checker.table, checker.diagnostics)
}

struct TypeChecker<'a> {
    tcx: &'a mut TyCtxt,
    infer: InferCtxt,
    table: TypeTable,
    diagnostics: Vec<TypeDiagnostic>,
    resolutions: &'a Resolutions,
    scopes: Vec<HashMap<String, Ty>>,
    binding_types: HashMap<NodeId, Ty>,
    /// Ordered field name + type for every named struct, keyed by
    /// the struct's `DefId`. Built during `collect_signatures` so
    /// field-access and struct-literal expressions can resolve leaf
    /// types without having to look up the original AST.
    struct_fields: HashMap<gossamer_resolve::DefId, Vec<(String, Ty)>>,
}

impl<'a> TypeChecker<'a> {
    fn new(tcx: &'a mut TyCtxt, resolutions: &'a Resolutions) -> Self {
        Self {
            tcx,
            infer: InferCtxt::new(),
            table: TypeTable::new(),
            diagnostics: Vec::new(),
            resolutions,
            scopes: vec![HashMap::new()],
            binding_types: HashMap::new(),
            struct_fields: HashMap::new(),
        }
    }

    fn fresh(&mut self) -> Ty {
        self.infer.fresh_var(self.tcx)
    }

    fn emit(&mut self, error: TypeError, span: Span) {
        self.diagnostics.push(TypeDiagnostic::new(error, span));
    }

    fn record(&mut self, node: NodeId, ty: Ty) -> Ty {
        self.table.insert(node, ty);
        ty
    }

    fn resolve_table(&mut self) {
        let pairs: Vec<(NodeId, Ty)> = self.table.sorted_entries();
        for (node, ty) in pairs {
            let resolved = self.infer.resolve(self.tcx, ty);
            if resolved != ty {
                self.table.insert(node, resolved);
            }
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(HashMap::new());
    }

    fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    fn bind_local(&mut self, name: &str, ty: Ty) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.insert(name.to_string(), ty);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<Ty> {
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(name) {
                return Some(*ty);
            }
        }
        None
    }

    fn unify(&mut self, lhs: Ty, rhs: Ty, span: Span) {
        match self.infer.unify(self.tcx, lhs, rhs) {
            Ok(()) => {}
            Err(err) => self.report_unify(err, lhs, rhs, span),
        }
    }

    fn report_unify(&mut self, err: UnifyError, lhs: Ty, rhs: Ty, span: Span) {
        match err {
            UnifyError::Mismatch => {
                let lhs = self.infer.resolve(self.tcx, lhs);
                let rhs = self.infer.resolve(self.tcx, rhs);
                if !self.is_concrete(lhs) || !self.is_concrete(rhs) {
                    return;
                }
                let expected = render_ty(self.tcx, lhs);
                let found = render_ty(self.tcx, rhs);
                self.emit(TypeError::TypeMismatch { expected, found }, span);
            }
            UnifyError::IntegerConstraint => {
                // The other side is concrete and non-integer (the
                // unifier only raises this when it has a concrete
                // target). Render the mismatch as a regular type
                // error against `i64`, which is the shape the user
                // would see if they had written `42i64`.
                let lhs = self.infer.resolve(self.tcx, lhs);
                let rhs = self.infer.resolve(self.tcx, rhs);
                let (literal_side, target_side) =
                    if matches!(self.tcx.kind(lhs), Some(TyKind::Var(_))) {
                        (lhs, rhs)
                    } else {
                        (rhs, lhs)
                    };
                let _ = literal_side;
                let expected = render_ty(self.tcx, target_side);
                self.emit(
                    TypeError::TypeMismatch {
                        expected,
                        found: "{integer}".to_string(),
                    },
                    span,
                );
            }
            UnifyError::Occurs { .. } => {}
        }
    }

    fn is_concrete(&self, ty: Ty) -> bool {
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind(resolved) {
            Some(kind) => kind_is_concrete(self, kind),
            None => false,
        }
    }

    fn collect_signatures(&mut self, items: &[Item]) {
        for item in items {
            match &item.kind {
                ItemKind::Fn(decl) => self.register_fn_sig(item.id, decl),
                ItemKind::Impl(decl) => self.collect_impl_signatures(decl),
                ItemKind::Trait(decl) => self.collect_trait_signatures(decl),
                ItemKind::Struct(decl) => self.register_struct(item.id, &decl.body),
                _ => {}
            }
        }
    }

    fn register_struct(&mut self, item_id: NodeId, body: &StructBody) {
        let Some(def) = self.resolutions.definition_of(item_id) else {
            return;
        };
        if let StructBody::Named(fields) = body {
            let list: Vec<(String, Ty)> = fields
                .iter()
                .map(|f| (f.name.name.clone(), self.type_from_ast(&f.ty)))
                .collect();
            let tys: Vec<Ty> = list.iter().map(|(_, t)| *t).collect();
            self.tcx.register_struct_fields(def, tys);
            self.struct_fields.insert(def, list);
        }
    }

    /// Resolves `receiver_ty.field_name` to the leaf field type.
    /// Auto-dereferences through `&T`/`&mut T` wrappers. Returns
    /// `None` when the receiver does not name a known struct or the
    /// field is not declared on it.
    fn lookup_field_ty(&mut self, receiver_ty: Ty, field_name: &str) -> Option<Ty> {
        let resolved = self.infer.resolve(self.tcx, receiver_ty);
        let mut cur = resolved;
        loop {
            match self.tcx.kind_of(cur).clone() {
                TyKind::Ref { inner, .. } => cur = inner,
                TyKind::Adt { def, .. } => {
                    let fields = self.struct_fields.get(&def)?;
                    for (name, ty) in fields {
                        if name == field_name {
                            return Some(*ty);
                        }
                    }
                    return None;
                }
                _ => return None,
            }
        }
    }

    fn collect_impl_signatures(&mut self, decl: &ImplDecl) {
        for item in &decl.items {
            if let ImplItem::Fn(fn_decl) = item {
                let id = NodeId::DUMMY;
                let _ = id;
                self.register_fn_sig_anonymous(fn_decl);
            }
        }
    }

    fn collect_trait_signatures(&mut self, decl: &gossamer_ast::TraitDecl) {
        for item in &decl.items {
            if let TraitItem::Fn(fn_decl) = item {
                self.register_fn_sig_anonymous(fn_decl);
            }
        }
    }

    fn register_fn_sig(&mut self, _node: NodeId, decl: &FnDecl) {
        self.fn_sig_of(decl);
    }

    fn register_fn_sig_anonymous(&mut self, decl: &FnDecl) {
        self.fn_sig_of(decl);
    }

    fn fn_sig_of(&mut self, decl: &FnDecl) -> FnSig {
        let inputs: Vec<Ty> = decl
            .params
            .iter()
            .map(|param| self.param_ty(param))
            .collect();
        let output = match decl.ret.as_ref() {
            Some(ty) => self.type_from_ast(ty),
            None => self.tcx.unit(),
        };
        FnSig { inputs, output }
    }

    fn param_ty(&mut self, param: &FnParam) -> Ty {
        match param {
            FnParam::Typed { ty, .. } => self.type_from_ast(ty),
            FnParam::Receiver(_) => self.fresh(),
        }
    }

    fn check_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(decl) => self.check_fn(decl),
            ItemKind::Impl(decl) => {
                for impl_item in &decl.items {
                    if let ImplItem::Fn(fn_decl) = impl_item {
                        self.check_fn(fn_decl);
                    } else if let ImplItem::Const { value, .. } = impl_item {
                        self.check_expr(value);
                    }
                }
            }
            ItemKind::Trait(decl) => {
                for trait_item in &decl.items {
                    if let TraitItem::Fn(fn_decl) = trait_item {
                        self.check_fn(fn_decl);
                    }
                }
            }
            ItemKind::Const(decl) => {
                let annotated = self.type_from_ast(&decl.ty);
                let init = self.check_expr(&decl.value);
                self.unify(annotated, init, decl.value.span);
            }
            ItemKind::Static(decl) => {
                let annotated = self.type_from_ast(&decl.ty);
                let init = self.check_expr(&decl.value);
                self.unify(annotated, init, decl.value.span);
            }
            ItemKind::Struct(decl) => self.check_struct_body(&decl.body),
            ItemKind::Enum(decl) => {
                for variant in &decl.variants {
                    self.check_struct_body(&variant.body);
                }
            }
            ItemKind::TypeAlias(decl) => {
                let _ = self.type_from_ast(&decl.ty);
            }
            ItemKind::Mod(_) | ItemKind::AttrItem(_) => {}
        }
    }

    fn check_struct_body(&mut self, body: &StructBody) {
        match body {
            StructBody::Named(fields) => {
                for field in fields {
                    let _ = self.type_from_ast(&field.ty);
                }
            }
            StructBody::Tuple(fields) => {
                for field in fields {
                    let _ = self.type_from_ast(&field.ty);
                }
            }
            StructBody::Unit => {}
        }
    }

    fn check_fn(&mut self, decl: &FnDecl) {
        self.push_scope();
        for param in &decl.params {
            self.bind_fn_param(param);
        }
        let ret = match decl.ret.as_ref() {
            Some(ty) => self.type_from_ast(ty),
            None => self.tcx.unit(),
        };
        if let Some(body) = &decl.body {
            let body_ty = self.check_expr(body);
            self.unify(ret, body_ty, body.span);
        }
        self.pop_scope();
    }

    fn bind_fn_param(&mut self, param: &FnParam) {
        match param {
            FnParam::Typed { pattern, ty } => {
                let param_ty = self.type_from_ast(ty);
                self.bind_pattern(pattern, param_ty);
            }
            FnParam::Receiver(_) => {
                let ty = self.fresh();
                self.bind_local("self", ty);
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr) -> Ty {
        let ty = self.check_expr_kind(expr);
        self.record(expr.id, ty)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "expression dispatch — arms map 1:1 to ExprKind variants; splitting hides the dispatch table"
    )]
    fn check_expr_kind(&mut self, expr: &Expr) -> Ty {
        match &expr.kind {
            ExprKind::Literal(lit) => self.type_of_literal(lit),
            ExprKind::Path(path) => self.check_path_expr(expr.id, path, expr.span),
            ExprKind::Call { callee, args } => self.check_call(callee, args),
            ExprKind::MethodCall { receiver, args, .. } => self.check_method_call(receiver, args),
            ExprKind::FieldAccess { receiver, field } => {
                let receiver_ty = self.check_expr(receiver);
                match field {
                    gossamer_ast::FieldSelector::Named(name) => self
                        .lookup_field_ty(receiver_ty, &name.name)
                        .unwrap_or_else(|| self.fresh()),
                    gossamer_ast::FieldSelector::Index(idx) => {
                        let resolved = self.infer.resolve(self.tcx, receiver_ty);
                        if let TyKind::Tuple(elems) = self.tcx.kind_of(resolved).clone() {
                            elems
                                .get(*idx as usize)
                                .copied()
                                .unwrap_or_else(|| self.fresh())
                        } else {
                            self.fresh()
                        }
                    }
                }
            }
            ExprKind::Unary { op, operand } => self.check_unary(*op, operand, expr.span),
            ExprKind::Index { base, index } => {
                let base_ty = self.check_expr(base);
                self.check_expr(index);
                let mut cur = self.infer.resolve(self.tcx, base_ty);
                loop {
                    match self.tcx.kind_of(cur).clone() {
                        TyKind::Ref { inner, .. } => cur = inner,
                        TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                            return elem;
                        }
                        TyKind::String => {
                            return self.tcx.int_ty(IntTy::I64);
                        }
                        _ => return self.fresh(),
                    }
                }
            }
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, expr.span),
            ExprKind::Assign { place, value, .. } => self.check_assign(place, value),
            ExprKind::Cast { value, ty } => {
                let from = self.check_expr(value);
                let to = self.type_from_ast(ty);
                self.check_cast(from, to, expr.span);
                to
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.check_if(condition, then_branch, else_branch.as_deref()),
            ExprKind::Match { scrutinee, arms } => self.check_match(scrutinee, arms),
            ExprKind::Loop { body, .. } => {
                self.check_expr(body);
                self.tcx.never()
            }
            ExprKind::While {
                condition, body, ..
            } => {
                let bool_ty = self.tcx.bool_ty();
                let cond_ty = self.check_expr(condition);
                self.unify(bool_ty, cond_ty, condition.span);
                self.check_expr(body);
                self.tcx.unit()
            }
            ExprKind::For {
                pattern,
                iter,
                body,
                ..
            } => self.check_for(pattern, iter, body),
            ExprKind::Block(block) | ExprKind::Unsafe(block) => self.check_block(block),
            ExprKind::Closure { params, ret, body } => {
                self.check_closure(params, ret.as_ref(), body)
            }
            ExprKind::Return(value) | ExprKind::Break { value, .. } => {
                if let Some(value) = value {
                    self.check_expr(value);
                }
                self.tcx.never()
            }
            ExprKind::Continue { .. } => self.tcx.never(),
            ExprKind::Tuple(elems) => {
                let tys: Vec<Ty> = elems.iter().map(|e| self.check_expr(e)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            ExprKind::Struct { path, fields, base } => {
                // Resolve the header path to an Adt type. `head` is
                // the struct's NodeId in the path's last segment.
                // Unifying named field values with the declared
                // field types lets downstream field-access nodes
                // see concrete leaf types.
                let head_node = expr.id;
                let struct_ty = if let Some(res) = self.resolutions.get(head_node) {
                    match res {
                        Resolution::Def {
                            def,
                            kind:
                                gossamer_resolve::DefKind::Struct | gossamer_resolve::DefKind::Enum,
                        } => self.tcx.intern(TyKind::Adt {
                            def,
                            substs: crate::Substs::new(),
                        }),
                        _ => self.fresh(),
                    }
                } else {
                    self.fresh()
                };
                let _ = path;
                let resolved = self.infer.resolve(self.tcx, struct_ty);
                let declared: Option<Vec<(String, Ty)>> = match self.tcx.kind_of(resolved) {
                    TyKind::Adt { def, .. } => self.struct_fields.get(def).cloned(),
                    _ => None,
                };
                for field in fields {
                    if let Some(value) = &field.value {
                        let val_ty = self.check_expr(value);
                        if let Some(declared_fields) = declared.as_ref() {
                            if let Some((_, dty)) =
                                declared_fields.iter().find(|(n, _)| n == &field.name.name)
                            {
                                self.unify(*dty, val_ty, value.span);
                            }
                        }
                    }
                }
                if let Some(base) = base {
                    self.check_expr(base);
                }
                struct_ty
            }
            ExprKind::Array(arr) => self.check_array(arr),
            ExprKind::Range { start, end, .. } => {
                if let Some(start) = start {
                    self.check_expr(start);
                }
                if let Some(end) = end {
                    self.check_expr(end);
                }
                self.fresh()
            }
            ExprKind::Try(inner) | ExprKind::Go(inner) => {
                self.check_expr(inner);
                self.fresh()
            }
            ExprKind::Select(_) | ExprKind::MacroCall(_) => self.fresh(),
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) -> Ty {
        let callee_ty = self.check_expr(callee);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.check_expr(a)).collect();
        let resolved = self.infer.resolve(self.tcx, callee_ty);
        if let Some(TyKind::FnPtr(sig)) = self.tcx.kind(resolved).cloned() {
            if sig.inputs.len() == arg_tys.len() {
                for (param, (arg_ty, arg_expr)) in sig.inputs.iter().zip(arg_tys.iter().zip(args)) {
                    self.unify(*param, *arg_ty, arg_expr.span);
                }
                return sig.output;
            }
        }
        self.fresh()
    }

    fn check_method_call(&mut self, receiver: &Expr, args: &[Expr]) -> Ty {
        self.check_expr(receiver);
        for arg in args {
            self.check_expr(arg);
        }
        self.fresh()
    }

    fn check_unary(&mut self, op: UnaryOp, operand: &Expr, span: Span) -> Ty {
        let operand_ty = self.check_expr(operand);
        let resolved = self.infer.resolve(self.tcx, operand_ty);
        match op {
            UnaryOp::Not => {
                if matches!(self.tcx.kind(resolved), Some(TyKind::Bool)) {
                    self.tcx.bool_ty()
                } else if self.is_concrete(resolved) && !self.is_integer(resolved) {
                    self.emit(
                        TypeError::UnresolvedOp {
                            op: "!".to_string(),
                            lhs: render_ty(self.tcx, resolved),
                            rhs: String::new(),
                        },
                        span,
                    );
                    self.tcx.error_ty()
                } else {
                    operand_ty
                }
            }
            UnaryOp::Neg => operand_ty,
            UnaryOp::RefShared => self.tcx.intern(TyKind::Ref {
                mutability: Mutbl::Not,
                inner: operand_ty,
            }),
            UnaryOp::RefMut => self.tcx.intern(TyKind::Ref {
                mutability: Mutbl::Mut,
                inner: operand_ty,
            }),
            UnaryOp::Deref => self.fresh(),
        }
    }

    fn check_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        let lhs_ty = self.check_expr(lhs);
        let rhs_ty = self.check_expr(rhs);
        match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                self.unify(lhs_ty, rhs_ty, span);
                self.tcx.bool_ty()
            }
            BinaryOp::And | BinaryOp::Or => {
                let bool_ty = self.tcx.bool_ty();
                self.unify(bool_ty, lhs_ty, lhs.span);
                self.unify(bool_ty, rhs_ty, rhs.span);
                bool_ty
            }
            BinaryOp::PipeGt => rhs_ty,
            _ => {
                self.unify(lhs_ty, rhs_ty, span);
                lhs_ty
            }
        }
    }

    fn check_assign(&mut self, place: &Expr, value: &Expr) -> Ty {
        let place_ty = self.check_expr(place);
        let value_ty = self.check_expr(value);
        self.unify(place_ty, value_ty, value.span);
        self.tcx.unit()
    }

    /// Validates an `as` cast against the whitelist of permitted
    /// conversions: numeric ↔ numeric, `bool`/`char` → integer,
    /// `u8` → `char`, and same-type no-ops. Matches Rust's RFC 401.
    /// Fails soft when either side is still an inference variable —
    /// the unification pass will resolve it, and a later run can
    /// recheck; inventing an error on a not-yet-known type would
    /// cascade into noise.
    fn check_cast(&mut self, from: Ty, to: Ty, span: Span) {
        let resolved_from = self.infer.resolve(self.tcx, from);
        let resolved_to = self.infer.resolve(self.tcx, to);
        let Some(from_kind) = self.tcx.kind(resolved_from).cloned() else {
            return;
        };
        let Some(to_kind) = self.tcx.kind(resolved_to).cloned() else {
            return;
        };
        if matches!(from_kind, TyKind::Var(_) | TyKind::Error)
            || matches!(to_kind, TyKind::Var(_) | TyKind::Error)
        {
            return;
        }
        if cast_allowed(&from_kind, &to_kind) {
            return;
        }
        self.diagnostics.push(TypeDiagnostic::new(
            TypeError::InvalidCast {
                from: render_ty(self.tcx, resolved_from),
                to: render_ty(self.tcx, resolved_to),
            },
            span,
        ));
    }

    fn check_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: Option<&Expr>) -> Ty {
        let cond_ty = self.check_expr(condition);
        let bool_ty = self.tcx.bool_ty();
        self.unify(bool_ty, cond_ty, condition.span);
        let then_ty = self.check_expr(then_branch);
        if let Some(else_branch) = else_branch {
            let else_ty = self.check_expr(else_branch);
            self.unify(then_ty, else_ty, else_branch.span);
            then_ty
        } else {
            self.tcx.unit()
        }
    }

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Ty {
        let scrut_ty = self.check_expr(scrutinee);
        let result_ty = self.fresh();
        for arm in arms {
            self.push_scope();
            let pat_ty = self.type_of_pattern(&arm.pattern);
            self.bind_pattern(&arm.pattern, pat_ty);
            self.unify(scrut_ty, pat_ty, arm.pattern.span);
            if let Some(guard) = &arm.guard {
                let guard_ty = self.check_expr(guard);
                let bool_ty = self.tcx.bool_ty();
                self.unify(bool_ty, guard_ty, guard.span);
            }
            let body_ty = self.check_expr(&arm.body);
            self.unify(result_ty, body_ty, arm.body.span);
            self.pop_scope();
        }
        result_ty
    }

    fn check_for(&mut self, pattern: &Pattern, iter: &Expr, body: &Expr) -> Ty {
        let iter_ty = self.check_expr(iter);
        self.push_scope();
        // Derive the pattern's type from the iterator: arrays/slices
        // yield their element type, ranges over integers yield the
        // integer type.
        let derived = {
            let mut cur = self.infer.resolve(self.tcx, iter_ty);
            loop {
                match self.tcx.kind_of(cur).clone() {
                    TyKind::Ref { inner, .. } => cur = inner,
                    TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                        break Some(elem);
                    }
                    _ => break None,
                }
            }
        };
        let pat_ty = match derived {
            Some(t) => {
                let p = self.type_of_pattern(pattern);
                self.unify(p, t, pattern.span);
                t
            }
            None => self.type_of_pattern(pattern),
        };
        self.bind_pattern(pattern, pat_ty);
        self.check_expr(body);
        self.pop_scope();
        self.tcx.unit()
    }

    fn check_block(&mut self, block: &Block) -> Ty {
        self.push_scope();
        for stmt in &block.stmts {
            self.check_stmt(stmt);
        }
        let ty = if let Some(tail) = &block.tail {
            self.check_expr(tail)
        } else {
            self.tcx.unit()
        };
        self.pop_scope();
        ty
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, ty, init } => {
                let binding_ty = match ty {
                    Some(ty) => self.type_from_ast(ty),
                    None => self.fresh(),
                };
                if let Some(init) = init {
                    let init_ty = self.check_expr(init);
                    self.unify(binding_ty, init_ty, init.span);
                }
                self.bind_pattern(pattern, binding_ty);
            }
            StmtKind::Expr { expr, .. } => {
                self.check_expr(expr);
            }
            StmtKind::Item(item) => self.check_item(item),
            StmtKind::Defer(inner) | StmtKind::Go(inner) => {
                self.check_expr(inner);
            }
        }
    }

    fn check_closure(&mut self, params: &[ClosureParam], ret: Option<&AstType>, body: &Expr) -> Ty {
        self.push_scope();
        let inputs: Vec<Ty> = params
            .iter()
            .map(|param| {
                let ty = match param.ty.as_ref() {
                    Some(ty) => self.type_from_ast(ty),
                    None => self.fresh(),
                };
                self.bind_pattern(&param.pattern, ty);
                ty
            })
            .collect();
        let output = match ret {
            Some(ty) => self.type_from_ast(ty),
            None => self.fresh(),
        };
        let body_ty = self.check_expr(body);
        self.unify(output, body_ty, body.span);
        self.pop_scope();
        self.tcx.intern(TyKind::FnPtr(FnSig { inputs, output }))
    }

    fn check_array(&mut self, arr: &ArrayExpr) -> Ty {
        match arr {
            ArrayExpr::List(elems) => {
                let elem_ty = if let Some(first) = elems.first() {
                    self.check_expr(first)
                } else {
                    self.fresh()
                };
                for elem in elems.iter().skip(1) {
                    let ty = self.check_expr(elem);
                    self.unify(elem_ty, ty, elem.span);
                }
                self.tcx.intern(TyKind::Array {
                    elem: elem_ty,
                    len: elems.len(),
                })
            }
            ArrayExpr::Repeat { value, count } => {
                let elem_ty = self.check_expr(value);
                self.check_expr(count);
                let len = evaluate_const_int(count).unwrap_or(0);
                self.tcx.intern(TyKind::Array { elem: elem_ty, len })
            }
        }
    }

    fn check_path_expr(&mut self, node: NodeId, path: &gossamer_ast::PathExpr, _span: Span) -> Ty {
        let Some(resolution) = self.resolutions.get(node) else {
            return self.fresh();
        };
        match resolution {
            Resolution::Local(binding_id) => {
                if let Some(ty) = self.binding_types.get(&binding_id).copied() {
                    return ty;
                }
                if let Some(first) = path.segments.first() {
                    if let Some(ty) = self.lookup_local(&first.name.name) {
                        return ty;
                    }
                }
                self.fresh()
            }
            Resolution::Primitive(prim) => self.type_from_primitive(prim),
            Resolution::Def { def, kind } => match kind {
                gossamer_resolve::DefKind::Enum | gossamer_resolve::DefKind::Struct => {
                    self.tcx.intern(TyKind::Adt {
                        def,
                        substs: crate::Substs::new(),
                    })
                }
                gossamer_resolve::DefKind::Fn => {
                    // Pull turbofish args (`ident::<i64, bool>`) off
                    // the last path segment, resolve each to a
                    // concrete [`Ty`], and stamp the callee's type as
                    // `TyKind::FnDef { def, substs }` so that the MIR
                    // lowerer reads the real substitution instead of
                    // deriving one heuristically from argument types.
                    let substs = self.substs_from_path(path);
                    self.tcx.intern(TyKind::FnDef { def, substs })
                }
                _ => self.fresh(),
            },
            Resolution::Import { .. } | Resolution::Err => self.fresh(),
        }
    }

    fn substs_from_path(&mut self, path: &gossamer_ast::PathExpr) -> crate::Substs {
        let generics = match path.segments.last() {
            Some(seg) => &seg.generics,
            None => return crate::Substs::new(),
        };
        let args: Vec<crate::GenericArg> = generics
            .iter()
            .map(|arg| match arg {
                gossamer_ast::GenericArg::Type(t) => crate::GenericArg::Type(self.type_from_ast(t)),
                gossamer_ast::GenericArg::Const(_) => crate::GenericArg::Const(0),
            })
            .collect();
        crate::Substs::from_args(args)
    }

    fn type_of_literal(&mut self, lit: &Literal) -> Ty {
        match lit {
            Literal::Int(text) => self.type_of_int_literal(text),
            Literal::Float(text) => self.type_of_float_literal(text),
            Literal::String(_) | Literal::RawString { .. } => self.tcx.string_ty(),
            Literal::Char(_) => self.tcx.char_ty(),
            Literal::Byte(_) => self.tcx.int_ty(IntTy::U8),
            Literal::ByteString(_) | Literal::RawByteString { .. } => {
                let u8_ty = self.tcx.int_ty(IntTy::U8);
                self.tcx.intern(TyKind::Slice(u8_ty))
            }
            Literal::Bool(_) => self.tcx.bool_ty(),
            Literal::Unit => self.tcx.unit(),
        }
    }

    fn type_of_int_literal(&mut self, text: &str) -> Ty {
        for (suffix, int_ty) in INT_SUFFIXES {
            if text.ends_with(suffix) {
                return self.tcx.int_ty(*int_ty);
            }
        }
        for (suffix, float_ty) in FLOAT_SUFFIXES {
            if text.ends_with(suffix) {
                return self.tcx.float_ty(*float_ty);
            }
        }
        // Unsuffixed integer literal — Go-style untyped constant.
        // The fresh var is integer-constrained so it can only
        // unify with concrete integer types; if no use-site
        // constraints arise it defaults to `i64` at the end of
        // typechecking.
        self.infer.fresh_int_var(self.tcx)
    }

    fn type_of_float_literal(&mut self, text: &str) -> Ty {
        for (suffix, float_ty) in FLOAT_SUFFIXES {
            if text.ends_with(suffix) {
                return self.tcx.float_ty(*float_ty);
            }
        }
        self.fresh()
    }

    fn type_from_primitive(&mut self, prim: PrimitiveTy) -> Ty {
        match prim {
            PrimitiveTy::Bool => self.tcx.bool_ty(),
            PrimitiveTy::Char => self.tcx.char_ty(),
            PrimitiveTy::String => self.tcx.string_ty(),
            PrimitiveTy::Int(width) => self.tcx.int_ty(int_ty_from_width(width, true)),
            PrimitiveTy::UInt(width) => self.tcx.int_ty(int_ty_from_width(width, false)),
            PrimitiveTy::Float(FloatWidth::W32) => self.tcx.float_ty(FloatTy::F32),
            PrimitiveTy::Float(FloatWidth::W64) => self.tcx.float_ty(FloatTy::F64),
            PrimitiveTy::Never => self.tcx.never(),
            PrimitiveTy::Unit => self.tcx.unit(),
        }
    }

    fn type_from_ast(&mut self, ast_ty: &AstType) -> Ty {
        let ty = match &ast_ty.kind {
            AstTypeKind::Unit => self.tcx.unit(),
            AstTypeKind::Never => self.tcx.never(),
            AstTypeKind::Infer => self.fresh(),
            AstTypeKind::Path(path) => self.type_from_ast_path(ast_ty.id, path),
            AstTypeKind::Tuple(elems) => {
                let tys: Vec<Ty> = elems.iter().map(|e| self.type_from_ast(e)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            AstTypeKind::Array { elem, len } => {
                let elem_ty = self.type_from_ast(elem);
                let count = evaluate_const_int(len).unwrap_or(0);
                self.tcx.intern(TyKind::Array {
                    elem: elem_ty,
                    len: count,
                })
            }
            AstTypeKind::Slice(inner) => {
                let inner_ty = self.type_from_ast(inner);
                self.tcx.intern(TyKind::Slice(inner_ty))
            }
            AstTypeKind::Ref { mutability, inner } => {
                let inner_ty = self.type_from_ast(inner);
                let mutability = match mutability {
                    gossamer_ast::Mutability::Immutable => Mutbl::Not,
                    gossamer_ast::Mutability::Mutable => Mutbl::Mut,
                };
                self.tcx.intern(TyKind::Ref {
                    mutability,
                    inner: inner_ty,
                })
            }
            AstTypeKind::Fn { kind, params, ret } => {
                let inputs: Vec<Ty> = params.iter().map(|p| self.type_from_ast(p)).collect();
                let output = match ret.as_ref() {
                    Some(ty) => self.type_from_ast(ty),
                    None => self.tcx.unit(),
                };
                let sig = FnSig { inputs, output };
                match kind {
                    gossamer_ast::FnTypeKind::Fn => self.tcx.intern(TyKind::FnPtr(sig)),
                    // `Fn` / `FnMut` / `FnOnce` all map to the single
                    // `FnTrait` callable shape. The MIR / codegen
                    // machinery uses one fat-pointer ABI for all
                    // three; the borrow-style distinctions Rust
                    // makes are unnecessary in a fully GC'd world.
                    gossamer_ast::FnTypeKind::ClosureFn
                    | gossamer_ast::FnTypeKind::ClosureFnMut
                    | gossamer_ast::FnTypeKind::ClosureFnOnce => {
                        self.tcx.intern(TyKind::FnTrait(sig))
                    }
                }
            }
        };
        self.record(ast_ty.id, ty)
    }

    fn type_from_ast_path(&mut self, node: NodeId, path: &TypePath) -> Ty {
        let head_name = path
            .segments
            .first()
            .map_or("", |seg| seg.name.name.as_str());
        if let Some(prim) = primitive_from_name(head_name) {
            return prim_to_ty(self.tcx, prim);
        }
        if let Some(resolution) = self.resolutions.get(node) {
            match resolution {
                Resolution::Primitive(prim) => return self.type_from_primitive(prim),
                Resolution::Def { def, .. } => {
                    let substs = self.substs_from_ast(path);
                    return self.tcx.intern(TyKind::Adt { def, substs });
                }
                Resolution::Import { .. } | Resolution::Err | Resolution::Local(_) => {}
            }
        }
        self.fresh()
    }

    fn substs_from_ast(&mut self, path: &TypePath) -> crate::Substs {
        let mut args = Vec::new();
        for segment in &path.segments {
            for arg in &segment.generics {
                match arg {
                    AstGenericArg::Type(ast_ty) => {
                        args.push(crate::GenericArg::Type(self.type_from_ast(ast_ty)));
                    }
                    AstGenericArg::Const(expr) => {
                        let raw = evaluate_const_int_from_expr(expr).unwrap_or(0);
                        let value = i128::try_from(raw).unwrap_or(0);
                        args.push(crate::GenericArg::Const(value));
                    }
                }
            }
        }
        crate::Substs::from_args(args)
    }

    fn is_integer(&self, ty: Ty) -> bool {
        matches!(self.tcx.kind(ty), Some(TyKind::Int(_)))
    }

    fn type_of_pattern(&mut self, pattern: &Pattern) -> Ty {
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Ident { .. }
            | PatternKind::Path(_)
            | PatternKind::Struct { .. }
            | PatternKind::TupleStruct { .. }
            | PatternKind::Rest => self.fresh(),
            PatternKind::Literal(lit) => self.type_of_literal(lit),
            PatternKind::Tuple(parts) => {
                let tys: Vec<Ty> = parts.iter().map(|p| self.type_of_pattern(p)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            PatternKind::Range { lo, .. } => self.type_of_literal(lo),
            PatternKind::Or(alts) => match alts.first() {
                Some(first) => self.type_of_pattern(first),
                None => self.fresh(),
            },
            PatternKind::Ref { inner, mutability } => {
                let inner_ty = self.type_of_pattern(inner);
                let mutability = match mutability {
                    gossamer_ast::Mutability::Immutable => Mutbl::Not,
                    gossamer_ast::Mutability::Mutable => Mutbl::Mut,
                };
                self.tcx.intern(TyKind::Ref {
                    mutability,
                    inner: inner_ty,
                })
            }
        }
    }

    fn bind_pattern(&mut self, pattern: &Pattern, ty: Ty) {
        self.binding_types.insert(pattern.id, ty);
        self.table.insert(pattern.id, ty);
        match &pattern.kind {
            PatternKind::Ident {
                name, subpattern, ..
            } => {
                self.bind_local(&name.name, ty);
                if let Some(subpattern) = subpattern {
                    self.bind_pattern(subpattern, ty);
                }
            }
            PatternKind::Tuple(parts) => {
                let resolved = self.infer.resolve(self.tcx, ty);
                let element_tys: Vec<Ty> =
                    if let Some(TyKind::Tuple(elems)) = self.tcx.kind(resolved).cloned() {
                        elems
                    } else {
                        (0..parts.len()).map(|_| self.fresh()).collect()
                    };
                for (i, part) in parts.iter().enumerate() {
                    let elem_ty = element_tys.get(i).copied().unwrap_or_else(|| self.fresh());
                    self.bind_pattern(part, elem_ty);
                }
            }
            PatternKind::Struct { fields, .. } => {
                for field in fields {
                    self.bind_field_pattern(field);
                }
            }
            PatternKind::TupleStruct { elems, .. } => {
                for elem in elems {
                    let elem_ty = self.fresh();
                    self.bind_pattern(elem, elem_ty);
                }
            }
            PatternKind::Or(alts) => {
                for alt in alts {
                    self.bind_pattern(alt, ty);
                }
            }
            PatternKind::Ref { inner, .. } => {
                let inner_ty = self.fresh();
                self.bind_pattern(inner, inner_ty);
            }
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::Path(_)
            | PatternKind::Range { .. }
            | PatternKind::Rest => {}
        }
    }

    fn bind_field_pattern(&mut self, field: &FieldPattern) {
        let ty = self.fresh();
        if let Some(pattern) = &field.pattern {
            self.bind_pattern(pattern, ty);
        } else {
            self.bind_local(&field.name.name, ty);
        }
    }
}

fn evaluate_const_int(expr: &Expr) -> Option<usize> {
    evaluate_const_int_from_expr(expr).map(|v| v as usize)
}

fn evaluate_const_int_from_expr(expr: &Expr) -> Option<u128> {
    if let ExprKind::Literal(Literal::Int(text)) = &expr.kind {
        let cleaned = strip_int_suffix(text).replace('_', "");
        return parse_int(&cleaned);
    }
    None
}

fn parse_int(text: &str) -> Option<u128> {
    if let Some(rest) = text.strip_prefix("0x").or_else(|| text.strip_prefix("0X")) {
        return u128::from_str_radix(rest, 16).ok();
    }
    if let Some(rest) = text.strip_prefix("0b").or_else(|| text.strip_prefix("0B")) {
        return u128::from_str_radix(rest, 2).ok();
    }
    if let Some(rest) = text.strip_prefix("0o").or_else(|| text.strip_prefix("0O")) {
        return u128::from_str_radix(rest, 8).ok();
    }
    text.parse::<u128>().ok()
}

fn strip_int_suffix(text: &str) -> String {
    for (suffix, _) in INT_SUFFIXES {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    for (suffix, _) in FLOAT_SUFFIXES {
        if let Some(stripped) = text.strip_suffix(suffix) {
            return stripped.to_string();
        }
    }
    text.to_string()
}

fn kind_is_concrete(checker: &TypeChecker<'_>, kind: &TyKind) -> bool {
    match kind {
        TyKind::Var(_) | TyKind::Error => false,
        TyKind::Bool
        | TyKind::Char
        | TyKind::String
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Unit
        | TyKind::Never
        | TyKind::Param { .. } => true,
        TyKind::Tuple(parts) => parts.iter().all(|t| checker.is_concrete(*t)),
        TyKind::Array { elem, .. }
        | TyKind::Slice(elem)
        | TyKind::Vec(elem)
        | TyKind::Sender(elem)
        | TyKind::Receiver(elem)
        | TyKind::Ref { inner: elem, .. } => checker.is_concrete(*elem),
        TyKind::HashMap { key, value } => checker.is_concrete(*key) && checker.is_concrete(*value),
        TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => {
            sig.inputs.iter().all(|t| checker.is_concrete(*t)) && checker.is_concrete(sig.output)
        }
        TyKind::FnDef { substs, .. }
        | TyKind::Adt { substs, .. }
        | TyKind::Alias { substs, .. }
        | TyKind::Closure { substs, .. } => substs.as_slice().iter().all(|arg| match arg {
            crate::GenericArg::Type(ty) => checker.is_concrete(*ty),
            crate::GenericArg::Const(_) => true,
        }),
        TyKind::Dyn(trait_ref) => trait_ref.substs.as_slice().iter().all(|arg| match arg {
            crate::GenericArg::Type(ty) => checker.is_concrete(*ty),
            crate::GenericArg::Const(_) => true,
        }),
    }
}

const INT_SUFFIXES: &[(&str, IntTy)] = &[
    ("i128", IntTy::I128),
    ("u128", IntTy::U128),
    ("isize", IntTy::Isize),
    ("usize", IntTy::Usize),
    ("i64", IntTy::I64),
    ("u64", IntTy::U64),
    ("i32", IntTy::I32),
    ("u32", IntTy::U32),
    ("i16", IntTy::I16),
    ("u16", IntTy::U16),
    ("i8", IntTy::I8),
    ("u8", IntTy::U8),
];

const FLOAT_SUFFIXES: &[(&str, FloatTy)] = &[("f32", FloatTy::F32), ("f64", FloatTy::F64)];

fn int_ty_from_width(width: IntWidth, signed: bool) -> IntTy {
    match (signed, width) {
        (true, IntWidth::W8) => IntTy::I8,
        (true, IntWidth::W16) => IntTy::I16,
        (true, IntWidth::W32) => IntTy::I32,
        (true, IntWidth::W64) => IntTy::I64,
        (true, IntWidth::W128) => IntTy::I128,
        (true, IntWidth::Size) => IntTy::Isize,
        (false, IntWidth::W8) => IntTy::U8,
        (false, IntWidth::W16) => IntTy::U16,
        (false, IntWidth::W32) => IntTy::U32,
        (false, IntWidth::W64) => IntTy::U64,
        (false, IntWidth::W128) => IntTy::U128,
        (false, IntWidth::Size) => IntTy::Usize,
    }
}

fn primitive_from_name(name: &str) -> Option<PrimitiveTy> {
    Some(match name {
        "bool" => PrimitiveTy::Bool,
        "char" => PrimitiveTy::Char,
        "String" => PrimitiveTy::String,
        "i8" => PrimitiveTy::Int(IntWidth::W8),
        "i16" => PrimitiveTy::Int(IntWidth::W16),
        "i32" => PrimitiveTy::Int(IntWidth::W32),
        "i64" => PrimitiveTy::Int(IntWidth::W64),
        "i128" => PrimitiveTy::Int(IntWidth::W128),
        "isize" => PrimitiveTy::Int(IntWidth::Size),
        "u8" => PrimitiveTy::UInt(IntWidth::W8),
        "u16" => PrimitiveTy::UInt(IntWidth::W16),
        "u32" => PrimitiveTy::UInt(IntWidth::W32),
        "u64" => PrimitiveTy::UInt(IntWidth::W64),
        "u128" => PrimitiveTy::UInt(IntWidth::W128),
        "usize" => PrimitiveTy::UInt(IntWidth::Size),
        "f32" => PrimitiveTy::Float(FloatWidth::W32),
        "f64" => PrimitiveTy::Float(FloatWidth::W64),
        _ => return None,
    })
}

fn prim_to_ty(tcx: &mut TyCtxt, prim: PrimitiveTy) -> Ty {
    match prim {
        PrimitiveTy::Bool => tcx.bool_ty(),
        PrimitiveTy::Char => tcx.char_ty(),
        PrimitiveTy::String => tcx.string_ty(),
        PrimitiveTy::Int(width) => tcx.int_ty(int_ty_from_width(width, true)),
        PrimitiveTy::UInt(width) => tcx.int_ty(int_ty_from_width(width, false)),
        PrimitiveTy::Float(FloatWidth::W32) => tcx.float_ty(FloatTy::F32),
        PrimitiveTy::Float(FloatWidth::W64) => tcx.float_ty(FloatTy::F64),
        PrimitiveTy::Never => tcx.never(),
        PrimitiveTy::Unit => tcx.unit(),
    }
}

/// Returns `true` when `from as to` is in the permitted cast set.
///
/// Mirrors Rust RFC 401:
/// - numeric ↔ numeric (every pair of `Int(_)` / `Float(_)`)
/// - `bool` → integer
/// - `char` → integer
/// - `u8` → `char`
/// - same-type cast (no-op, always allowed)
fn cast_allowed(from: &TyKind, to: &TyKind) -> bool {
    if from == to {
        return true;
    }
    let from_is_num = matches!(from, TyKind::Int(_) | TyKind::Float(_));
    let to_is_num = matches!(to, TyKind::Int(_) | TyKind::Float(_));
    if from_is_num && to_is_num {
        return true;
    }
    matches!(
        (from, to),
        (TyKind::Bool | TyKind::Char, TyKind::Int(_)) | (TyKind::Int(IntTy::U8), TyKind::Char),
    )
}
