//! The resolver walks a parsed [`SourceFile`] and produces a
//! [`Resolutions`] side table plus a list of [`ResolveDiagnostic`]s.

#![forbid(unsafe_code)]

use gossamer_ast::{
    ArrayExpr, Block, ClosureParam, EnumDecl, Expr, ExprKind, FieldPattern, FnDecl, FnParam,
    GenericArg, GenericParam, Generics, Ident, ImplDecl, ImplItem, Item, ItemKind, Literal,
    MatchArm, ModulePath, NodeId, PathExpr, Pattern, PatternKind, SelectArm, SelectOp, SourceFile,
    Stmt, StmtKind, StructBody, StructDecl, StructExprField, StructField, TraitBound, TraitDecl,
    TraitItem, TupleField, Type, TypeAliasDecl, TypeKind, TypePath, UseDecl, UseListEntry,
    UseTarget, WhereClause,
};
use gossamer_lex::Span;

use crate::def_id::{DefId, DefIdGenerator, DefKind};
use crate::diagnostic::{ResolveDiagnostic, ResolveError};
use crate::resolutions::{Resolution, Resolutions};
use crate::scope::{Binding, ScopeStack};

/// Runs name resolution on a parsed source file and returns the resolved
/// side-table plus any diagnostics surfaced along the way.
#[must_use]
pub fn resolve_source_file(source: &SourceFile) -> (Resolutions, Vec<ResolveDiagnostic>) {
    let mut resolver = Resolver::new();
    resolver.run(source);
    (resolver.resolutions, resolver.diagnostics)
}

struct Resolver {
    resolutions: Resolutions,
    diagnostics: Vec<ResolveDiagnostic>,
    scopes: ScopeStack,
    defs: DefIdGenerator,
}

impl Resolver {
    fn new() -> Self {
        Self {
            resolutions: Resolutions::new(),
            diagnostics: Vec::new(),
            scopes: ScopeStack::with_prelude(),
            defs: DefIdGenerator::new(),
        }
    }

    fn run(&mut self, source: &SourceFile) {
        self.collect_imports(&source.uses);
        self.collect_items(&source.items);
        for item in &source.items {
            if !crate::cfg::item_is_active(&item.attrs) {
                continue;
            }
            self.resolve_item(item);
        }
    }

    fn emit(&mut self, error: ResolveError, span: Span) {
        self.diagnostics.push(ResolveDiagnostic::new(error, span));
    }

    fn alloc_def(&mut self, node: NodeId, kind: DefKind) -> DefId {
        let def = self.defs.next();
        self.resolutions.insert_definition(node, def, kind);
        def
    }

    fn collect_imports(&mut self, uses: &[UseDecl]) {
        for use_decl in uses {
            match &use_decl.list {
                Some(list) => self.register_use_list(use_decl, list),
                None => self.register_use_simple(use_decl),
            }
        }
    }

    fn register_use_simple(&mut self, use_decl: &UseDecl) {
        let name = use_decl.alias.as_ref().map_or_else(
            || tail_name(&use_decl.target),
            |alias| Some(alias.name.clone()),
        );
        let Some(name) = name else {
            return;
        };
        self.define_import(&name, use_decl.id, use_decl.span);
    }

    fn register_use_list(&mut self, use_decl: &UseDecl, list: &[UseListEntry]) {
        for entry in list {
            let imported = entry
                .alias
                .as_ref()
                .map_or_else(|| entry.name.name.clone(), |alias| alias.name.clone());
            self.define_import(&imported, use_decl.id, use_decl.span);
        }
    }

    fn define_import(&mut self, name: &str, use_id: NodeId, span: Span) {
        let module = self.scopes.module_mut();
        if module.lookup_type(name).is_some() || module.lookup_value(name).is_some() {
            self.emit(
                ResolveError::DuplicateImport {
                    name: name.to_string(),
                },
                span,
            );
            return;
        }
        let binding = Binding::import(use_id);
        module.insert_type(name, binding);
        self.scopes.module_mut().insert_value(name, binding);
    }

    fn collect_items(&mut self, items: &[Item]) {
        for item in items {
            self.collect_item(item);
        }
    }

    fn collect_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Fn, item.span);
            }
            ItemKind::Struct(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Struct, item.span);
            }
            ItemKind::Enum(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Enum, item.span);
                self.register_enum_variants(decl, item.span);
            }
            ItemKind::Trait(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Trait, item.span);
            }
            ItemKind::TypeAlias(decl) => {
                self.register_item(item.id, &decl.name, DefKind::TypeAlias, item.span);
            }
            ItemKind::Const(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Const, item.span);
            }
            ItemKind::Static(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Static, item.span);
            }
            ItemKind::Mod(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Mod, item.span);
            }
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => {}
        }
    }

    fn register_enum_variants(&mut self, decl: &EnumDecl, span: Span) {
        for variant in &decl.variants {
            let def = self.defs.next();
            let binding = Binding::def(def, DefKind::Variant);
            if !self
                .scopes
                .module_mut()
                .insert_value(&variant.name.name, binding)
            {
                self.emit(
                    ResolveError::DuplicateItem {
                        name: variant.name.name.clone(),
                    },
                    span,
                );
            }
        }
    }

    fn register_item(&mut self, node: NodeId, name: &Ident, kind: DefKind, span: Span) {
        let def = self.alloc_def(node, kind);
        let binding = Binding::def(def, kind);
        let module = self.scopes.module_mut();
        let mut inserted_any = false;
        if kind.is_type_ns() {
            inserted_any |= module.insert_type(&name.name, binding);
        }
        if kind.is_value_ns() {
            inserted_any |= module.insert_value(&name.name, binding);
        }
        if !inserted_any && (kind.is_type_ns() || kind.is_value_ns()) {
            self.emit(
                ResolveError::DuplicateItem {
                    name: name.name.clone(),
                },
                span,
            );
        }
    }

    fn resolve_item(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(decl) => self.resolve_fn(decl),
            ItemKind::Struct(decl) => self.resolve_struct(decl),
            ItemKind::Enum(decl) => self.resolve_enum(decl),
            ItemKind::Trait(decl) => self.resolve_trait(decl),
            ItemKind::Impl(decl) => self.resolve_impl(decl),
            ItemKind::TypeAlias(decl) => self.resolve_type_alias(decl),
            ItemKind::Const(decl) => {
                self.resolve_type(&decl.ty);
                self.resolve_expr(&decl.value);
            }
            ItemKind::Static(decl) => {
                self.resolve_type(&decl.ty);
                self.resolve_expr(&decl.value);
            }
            ItemKind::Mod(_) | ItemKind::AttrItem(_) => {}
        }
    }

    fn resolve_fn(&mut self, decl: &FnDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        for param in &decl.params {
            match param {
                FnParam::Typed { pattern, ty } => {
                    self.resolve_type(ty);
                    self.bind_pattern(pattern);
                }
                FnParam::Receiver(_) => {
                    self.scopes
                        .top_mut()
                        .shadow_value("self", Binding::local(NodeId::DUMMY));
                }
            }
        }
        if let Some(ret) = &decl.ret {
            self.resolve_type(ret);
        }
        self.resolve_where_clause(&decl.where_clause);
        if let Some(body) = &decl.body {
            self.resolve_expr(body);
        }
        self.scopes.pop();
    }

    fn resolve_struct(&mut self, decl: &StructDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        self.resolve_where_clause(&decl.where_clause);
        self.resolve_struct_body(&decl.body);
        self.scopes.pop();
    }

    fn resolve_enum(&mut self, decl: &EnumDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        self.resolve_where_clause(&decl.where_clause);
        for variant in &decl.variants {
            self.resolve_struct_body(&variant.body);
            if let Some(disc) = &variant.discriminant {
                self.resolve_expr(disc);
            }
        }
        self.scopes.pop();
    }

    fn resolve_struct_body(&mut self, body: &StructBody) {
        match body {
            StructBody::Named(fields) => {
                for field in fields {
                    self.resolve_struct_field(field);
                }
            }
            StructBody::Tuple(fields) => {
                for field in fields {
                    self.resolve_tuple_field(field);
                }
            }
            StructBody::Unit => {}
        }
    }

    fn resolve_trait(&mut self, decl: &TraitDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        for bound in &decl.supertraits {
            self.resolve_trait_bound(bound);
        }
        self.resolve_where_clause(&decl.where_clause);
        for item in &decl.items {
            self.resolve_trait_item(item);
        }
        self.scopes.pop();
    }

    fn resolve_trait_item(&mut self, item: &TraitItem) {
        match item {
            TraitItem::Fn(decl) => self.resolve_fn(decl),
            TraitItem::Type {
                bounds, default, ..
            } => {
                for bound in bounds {
                    self.resolve_trait_bound(bound);
                }
                if let Some(default) = default {
                    self.resolve_type(default);
                }
            }
            TraitItem::Const { ty, default, .. } => {
                self.resolve_type(ty);
                if let Some(default) = default {
                    self.resolve_expr(default);
                }
            }
        }
    }

    fn resolve_impl(&mut self, decl: &ImplDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        if let Some(bound) = &decl.trait_ref {
            self.resolve_trait_bound(bound);
        }
        self.resolve_type(&decl.self_ty);
        self.resolve_where_clause(&decl.where_clause);
        for item in &decl.items {
            self.resolve_impl_item(item);
        }
        self.scopes.pop();
    }

    fn resolve_impl_item(&mut self, item: &ImplItem) {
        match item {
            ImplItem::Fn(decl) => self.resolve_fn(decl),
            ImplItem::Type { ty, .. } => self.resolve_type(ty),
            ImplItem::Const { ty, value, .. } => {
                self.resolve_type(ty);
                self.resolve_expr(value);
            }
        }
    }

    fn resolve_type_alias(&mut self, decl: &TypeAliasDecl) {
        self.scopes.push();
        self.bind_generics(&decl.generics);
        self.resolve_type(&decl.ty);
        self.scopes.pop();
    }

    fn resolve_struct_field(&mut self, field: &StructField) {
        self.resolve_type(&field.ty);
    }

    fn resolve_tuple_field(&mut self, field: &TupleField) {
        self.resolve_type(&field.ty);
    }

    fn resolve_trait_bound(&mut self, bound: &TraitBound) {
        self.resolve_type_path_in(&bound.path, None, None);
    }

    fn resolve_where_clause(&mut self, clause: &WhereClause) {
        for predicate in &clause.predicates {
            self.resolve_type(&predicate.bounded);
            for bound in &predicate.bounds {
                self.resolve_trait_bound(bound);
            }
        }
    }

    fn bind_generics(&mut self, generics: &Generics) {
        for param in &generics.params {
            match param {
                GenericParam::Type { name, bounds, .. } => {
                    let def = self.defs.next();
                    let binding = Binding::def(def, DefKind::TypeParam);
                    self.scopes.top_mut().insert_type(&name.name, binding);
                    for bound in bounds {
                        self.resolve_trait_bound(bound);
                    }
                }
                GenericParam::Const { name, ty, default } => {
                    self.resolve_type(ty);
                    let def = self.defs.next();
                    let binding = Binding::def(def, DefKind::Const);
                    self.scopes.top_mut().insert_value(&name.name, binding);
                    if let Some(default) = default {
                        self.resolve_expr(default);
                    }
                }
                GenericParam::Lifetime { .. } => {}
            }
        }
    }

    fn resolve_type(&mut self, ty: &Type) {
        match &ty.kind {
            TypeKind::Unit | TypeKind::Never | TypeKind::Infer => {}
            TypeKind::Path(path) => self.resolve_type_path_in(path, Some(ty.id), Some(ty.span)),
            TypeKind::Tuple(elems) => {
                for elem in elems {
                    self.resolve_type(elem);
                }
            }
            TypeKind::Array { elem, len } => {
                self.resolve_type(elem);
                self.resolve_expr(len);
            }
            TypeKind::Slice(inner) | TypeKind::Ref { inner, .. } => self.resolve_type(inner),
            TypeKind::Fn { params, ret, .. } => {
                for param in params {
                    self.resolve_type(param);
                }
                if let Some(ret) = ret {
                    self.resolve_type(ret);
                }
            }
        }
    }

    fn resolve_pattern_path(&mut self, path: &TypePath, anchor: NodeId, span: Span) {
        let Some(head) = path.segments.first() else {
            return;
        };
        let name = &head.name.name;
        let resolution = self
            .scopes
            .lookup_value(name)
            .or_else(|| self.scopes.lookup_type(name))
            .map_or(Resolution::Err, |b| b.resolution);
        if matches!(resolution, Resolution::Err) {
            self.emit(ResolveError::UnresolvedName { name: name.clone() }, span);
        }
        self.resolutions.insert(anchor, resolution);
        for segment in &path.segments {
            self.resolve_generic_args(&segment.generics);
        }
    }

    fn resolve_type_path_in(
        &mut self,
        path: &TypePath,
        anchor: Option<NodeId>,
        span: Option<Span>,
    ) {
        let Some(head) = path.segments.first() else {
            return;
        };
        let name = &head.name.name;
        let resolution = if is_self_type(name) {
            Resolution::Err
        } else {
            self.scopes
                .lookup_type(name)
                .map_or(Resolution::Err, |binding| binding.resolution)
        };
        if matches!(resolution, Resolution::Err) && !is_self_type(name) {
            if let Some(span) = span {
                self.emit(ResolveError::UnresolvedName { name: name.clone() }, span);
            }
        }
        if let Some(anchor) = anchor {
            self.resolutions.insert(anchor, resolution);
        }
        for segment in &path.segments {
            self.resolve_generic_args(&segment.generics);
        }
    }

    fn resolve_generic_args(&mut self, args: &[GenericArg]) {
        for arg in args {
            match arg {
                GenericArg::Type(ty) => self.resolve_type(ty),
                GenericArg::Const(expr) => self.resolve_expr(expr),
            }
        }
    }

    fn resolve_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Literal(lit) => self.resolve_literal(lit),
            ExprKind::Path(path) => self.resolve_value_path(path, expr.id, expr.span),
            ExprKind::Call { callee, args } => self.resolve_call(callee, args),
            ExprKind::MethodCall {
                receiver,
                generics,
                args,
                ..
            } => self.resolve_method_call(receiver, generics, args),
            ExprKind::FieldAccess { receiver, .. }
            | ExprKind::Unary {
                operand: receiver, ..
            } => {
                self.resolve_expr(receiver);
            }
            ExprKind::Index { base, index } => {
                self.resolve_expr(base);
                self.resolve_expr(index);
            }
            ExprKind::Binary { lhs, rhs, .. }
            | ExprKind::Assign {
                place: lhs,
                value: rhs,
                ..
            } => {
                self.resolve_expr(lhs);
                self.resolve_expr(rhs);
            }
            ExprKind::Cast { value, ty } => {
                self.resolve_expr(value);
                self.resolve_type(ty);
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.resolve_if(condition, then_branch, else_branch.as_deref()),
            ExprKind::Match { scrutinee, arms } => self.resolve_match(scrutinee, arms),
            ExprKind::Loop { body, .. } => self.resolve_expr(body),
            ExprKind::While {
                condition, body, ..
            } => {
                self.resolve_expr(condition);
                self.resolve_expr(body);
            }
            ExprKind::For {
                pattern,
                iter,
                body,
                ..
            } => self.resolve_for(pattern, iter, body),
            ExprKind::Block(block) | ExprKind::Unsafe(block) => self.resolve_block(block),
            ExprKind::Closure { params, ret, body } => {
                self.resolve_closure(params, ret.as_ref(), body);
            }
            ExprKind::Return(value) | ExprKind::Break { value, .. } => {
                self.resolve_optional_expr(value.as_deref());
            }
            ExprKind::Continue { .. } | ExprKind::MacroCall(_) => {}
            ExprKind::Tuple(elems) => self.resolve_exprs(elems),
            ExprKind::Struct { path, fields, base } => {
                self.resolve_struct_expr(path, fields, base.as_deref(), expr.id, expr.span);
            }
            ExprKind::Array(arr) => self.resolve_array_expr(arr),
            ExprKind::Range { start, end, .. } => {
                self.resolve_optional_expr(start.as_deref());
                self.resolve_optional_expr(end.as_deref());
            }
            ExprKind::Try(inner) | ExprKind::Go(inner) => self.resolve_expr(inner),
            ExprKind::Select(arms) => {
                for arm in arms {
                    self.resolve_select_arm(arm);
                }
            }
        }
    }

    fn resolve_call(&mut self, callee: &Expr, args: &[Expr]) {
        self.resolve_expr(callee);
        self.resolve_exprs(args);
    }

    fn resolve_method_call(&mut self, receiver: &Expr, generics: &[GenericArg], args: &[Expr]) {
        self.resolve_expr(receiver);
        self.resolve_generic_args(generics);
        self.resolve_exprs(args);
    }

    fn resolve_if(&mut self, condition: &Expr, then_branch: &Expr, else_branch: Option<&Expr>) {
        self.resolve_expr(condition);
        self.resolve_expr(then_branch);
        self.resolve_optional_expr(else_branch);
    }

    fn resolve_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) {
        self.resolve_expr(scrutinee);
        for arm in arms {
            self.resolve_match_arm(arm);
        }
    }

    fn resolve_for(&mut self, pattern: &Pattern, iter: &Expr, body: &Expr) {
        self.resolve_expr(iter);
        self.scopes.push();
        self.bind_pattern(pattern);
        self.resolve_expr(body);
        self.scopes.pop();
    }

    fn resolve_struct_expr(
        &mut self,
        path: &PathExpr,
        fields: &[StructExprField],
        base: Option<&Expr>,
        anchor: NodeId,
        span: Span,
    ) {
        self.resolve_struct_literal(path, anchor, span);
        for field in fields {
            self.resolve_struct_expr_field(field);
        }
        self.resolve_optional_expr(base);
    }

    fn resolve_exprs(&mut self, exprs: &[Expr]) {
        for expr in exprs {
            self.resolve_expr(expr);
        }
    }

    fn resolve_optional_expr(&mut self, expr: Option<&Expr>) {
        if let Some(expr) = expr {
            self.resolve_expr(expr);
        }
    }

    fn resolve_array_expr(&mut self, arr: &ArrayExpr) {
        match arr {
            ArrayExpr::List(elems) => {
                for elem in elems {
                    self.resolve_expr(elem);
                }
            }
            ArrayExpr::Repeat { value, count } => {
                self.resolve_expr(value);
                self.resolve_expr(count);
            }
        }
    }

    #[allow(clippy::unused_self)]
    fn resolve_literal(&self, _lit: &Literal) {}

    fn resolve_value_path(&mut self, path: &PathExpr, anchor: NodeId, span: Span) {
        let Some(head) = path.segments.first() else {
            return;
        };
        let name = &head.name.name;
        let resolution = self.lookup_value_or_type(name).unwrap_or_else(|| {
            self.emit(ResolveError::UnresolvedName { name: name.clone() }, span);
            Resolution::Err
        });
        self.resolutions.insert(anchor, resolution);
        for segment in &path.segments {
            self.resolve_generic_args(&segment.generics);
        }
    }

    fn resolve_struct_literal(&mut self, path: &PathExpr, anchor: NodeId, span: Span) {
        let Some(head) = path.segments.first() else {
            return;
        };
        let name = &head.name.name;
        let resolution = self.scopes.lookup_type(&head.name.name).map_or_else(
            || {
                self.emit(ResolveError::UnresolvedName { name: name.clone() }, span);
                Resolution::Err
            },
            |binding| binding.resolution,
        );
        self.resolutions.insert(anchor, resolution);
        for segment in &path.segments {
            self.resolve_generic_args(&segment.generics);
        }
    }

    fn lookup_value_or_type(&self, name: &str) -> Option<Resolution> {
        if let Some(binding) = self.scopes.lookup_value(name) {
            return Some(binding.resolution);
        }
        self.scopes.lookup_type(name).map(|b| b.resolution)
    }

    fn resolve_struct_expr_field(&mut self, field: &StructExprField) {
        if let Some(value) = &field.value {
            self.resolve_expr(value);
        }
    }

    fn resolve_block(&mut self, block: &Block) {
        self.scopes.push();
        for stmt in &block.stmts {
            self.resolve_stmt(stmt);
        }
        if let Some(tail) = &block.tail {
            self.resolve_expr(tail);
        }
        self.scopes.pop();
    }

    fn resolve_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, ty, init } => {
                if let Some(ty) = ty {
                    self.resolve_type(ty);
                }
                if let Some(init) = init {
                    self.resolve_expr(init);
                }
                self.bind_pattern(pattern);
            }
            StmtKind::Expr { expr, .. } => self.resolve_expr(expr),
            StmtKind::Item(item) => {
                self.collect_item_nested(item);
                self.resolve_item(item);
            }
            StmtKind::Defer(inner) | StmtKind::Go(inner) => self.resolve_expr(inner),
        }
    }

    fn collect_item_nested(&mut self, item: &Item) {
        match &item.kind {
            ItemKind::Fn(decl) => {
                let def = self.alloc_def(item.id, DefKind::Fn);
                self.scopes
                    .top_mut()
                    .insert_value(&decl.name.name, Binding::def(def, DefKind::Fn));
            }
            ItemKind::Const(decl) => {
                let def = self.alloc_def(item.id, DefKind::Const);
                self.scopes
                    .top_mut()
                    .insert_value(&decl.name.name, Binding::def(def, DefKind::Const));
            }
            ItemKind::Static(decl) => {
                let def = self.alloc_def(item.id, DefKind::Static);
                self.scopes
                    .top_mut()
                    .insert_value(&decl.name.name, Binding::def(def, DefKind::Static));
            }
            ItemKind::Struct(decl) => {
                let def = self.alloc_def(item.id, DefKind::Struct);
                self.scopes
                    .top_mut()
                    .insert_type(&decl.name.name, Binding::def(def, DefKind::Struct));
            }
            ItemKind::Enum(decl) => {
                let def = self.alloc_def(item.id, DefKind::Enum);
                self.scopes
                    .top_mut()
                    .insert_type(&decl.name.name, Binding::def(def, DefKind::Enum));
            }
            ItemKind::TypeAlias(decl) => {
                let def = self.alloc_def(item.id, DefKind::TypeAlias);
                self.scopes
                    .top_mut()
                    .insert_type(&decl.name.name, Binding::def(def, DefKind::TypeAlias));
            }
            ItemKind::Trait(decl) => {
                let def = self.alloc_def(item.id, DefKind::Trait);
                self.scopes
                    .top_mut()
                    .insert_type(&decl.name.name, Binding::def(def, DefKind::Trait));
            }
            ItemKind::Mod(decl) => {
                let def = self.alloc_def(item.id, DefKind::Mod);
                self.scopes
                    .top_mut()
                    .insert_type(&decl.name.name, Binding::def(def, DefKind::Mod));
            }
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => {}
        }
    }

    fn resolve_closure(&mut self, params: &[ClosureParam], ret: Option<&Type>, body: &Expr) {
        self.scopes.push();
        for param in params {
            if let Some(ty) = &param.ty {
                self.resolve_type(ty);
            }
            self.bind_pattern(&param.pattern);
        }
        if let Some(ret) = ret {
            self.resolve_type(ret);
        }
        self.resolve_expr(body);
        self.scopes.pop();
    }

    fn resolve_match_arm(&mut self, arm: &MatchArm) {
        self.scopes.push();
        self.bind_pattern(&arm.pattern);
        if let Some(guard) = &arm.guard {
            self.resolve_expr(guard);
        }
        self.resolve_expr(&arm.body);
        self.scopes.pop();
    }

    fn resolve_select_arm(&mut self, arm: &SelectArm) {
        self.scopes.push();
        match &arm.op {
            SelectOp::Recv { pattern, channel } => {
                self.resolve_expr(channel);
                self.bind_pattern(pattern);
            }
            SelectOp::Send { channel, value } => {
                self.resolve_expr(channel);
                self.resolve_expr(value);
            }
            SelectOp::Default => {}
        }
        self.resolve_expr(&arm.body);
        self.scopes.pop();
    }

    fn bind_pattern(&mut self, pattern: &Pattern) {
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Literal(_)
            | PatternKind::Rest
            | PatternKind::Range { .. } => {}
            PatternKind::Ident {
                name, subpattern, ..
            } => {
                self.scopes
                    .top_mut()
                    .shadow_value(name.name.clone(), Binding::local(pattern.id));
                if let Some(subpattern) = subpattern {
                    self.bind_pattern(subpattern);
                }
            }
            PatternKind::Path(path) => {
                self.resolve_pattern_path(path, pattern.id, pattern.span);
            }
            PatternKind::Tuple(parts) => {
                for part in parts {
                    self.bind_pattern(part);
                }
            }
            PatternKind::Struct { path, fields, .. } => {
                self.resolve_pattern_path(path, pattern.id, pattern.span);
                for field in fields {
                    self.bind_field_pattern(field);
                }
            }
            PatternKind::TupleStruct { path, elems } => {
                self.resolve_pattern_path(path, pattern.id, pattern.span);
                for elem in elems {
                    self.bind_pattern(elem);
                }
            }
            PatternKind::Or(alts) => {
                for alt in alts {
                    self.bind_pattern(alt);
                }
            }
            PatternKind::Ref { inner, .. } => self.bind_pattern(inner),
        }
    }

    fn bind_field_pattern(&mut self, field: &FieldPattern) {
        match &field.pattern {
            Some(pattern) => self.bind_pattern(pattern),
            None => {
                self.scopes
                    .top_mut()
                    .shadow_value(field.name.name.clone(), Binding::local(NodeId::DUMMY));
            }
        }
    }
}

fn tail_name(target: &UseTarget) -> Option<String> {
    match target {
        UseTarget::Module(path)
        | UseTarget::Project {
            module: Some(path), ..
        } => path_tail(path),
        UseTarget::Project { id, module: None } => Some(id.clone()),
    }
}

fn path_tail(path: &ModulePath) -> Option<String> {
    path.segments.last().map(|ident| ident.name.clone())
}

fn is_self_type(name: &str) -> bool {
    name == "Self" || name == "self"
}
