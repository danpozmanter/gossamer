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
        self.reject_non_canonical_std_path(use_decl);
        let name = use_decl.alias.as_ref().map_or_else(
            || tail_name(&use_decl.target),
            |alias| Some(alias.name.clone()),
        );
        let Some(name) = name else {
            return;
        };
        self.define_import(&name, use_decl.id, use_decl.span);
    }

    /// Validates `use std::...` module paths against the canonical
    /// module table: every module has exactly one path, and importing
    /// a path that names no module (an alias spelling, a typo) is an
    /// error here instead of a late member-lookup failure. A path
    /// whose parent is a valid module is accepted without checking
    /// the tail - item imports (`use std::sync::channel`) name items
    /// the resolver's table does not enumerate.
    fn reject_non_canonical_std_path(&mut self, use_decl: &UseDecl) {
        let gossamer_ast::UseTarget::Module(p) = &use_decl.target else {
            return;
        };
        if p.segments.len() < 2 || p.segments[0].name != "std" {
            return;
        }
        let rest: Vec<&str> = p.segments[1..].iter().map(|s| s.name.as_str()).collect();
        let joined = rest.join("::");
        let table = crate::stdlib_exports::STDLIB_MODULE_PATHS;
        let is_module_or_namespace = |path: &str| -> bool {
            table.binary_search(&path).is_ok()
                || table.iter().any(|m| {
                    m.len() > path.len() && m.starts_with(path) && m.as_bytes()[path.len()] == b':'
                })
        };
        if is_module_or_namespace(&joined) {
            return;
        }
        if rest.len() >= 2 {
            let parent = rest[..rest.len() - 1].join("::");
            if is_module_or_namespace(&parent) {
                return;
            }
        }
        self.emit(
            ResolveError::UnknownModulePath { path: joined },
            use_decl.span,
        );
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
        // Allow imports to shadow prelude entries (Gossamer's
        // `use std::collections::{HashMap, ...}` style imports
        // re-introduce names already in the prelude). A
        // collision with a non-prelude binding stays an error.
        let existing_kind = module
            .lookup_type(name)
            .or_else(|| module.lookup_value(name))
            .map(|b| b.resolution);
        let is_prelude_only = match existing_kind {
            Some(crate::resolutions::Resolution::Import { use_id: existing })
                if existing == crate::scope::PRELUDE_SENTINEL =>
            {
                true
            }
            None => true,
            _ => false,
        };
        if !is_prelude_only {
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
        let mut module_path: Vec<String> = Vec::new();
        self.collect_items_in(items, &mut module_path);
    }

    fn collect_items_in(&mut self, items: &[Item], module_path: &mut Vec<String>) {
        for item in items {
            self.collect_item(item, module_path);
        }
    }

    fn collect_item(&mut self, item: &Item, module_path: &mut Vec<String>) {
        match &item.kind {
            ItemKind::Fn(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Fn,
                    item.span,
                    module_path,
                );
            }
            ItemKind::Struct(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Struct,
                    item.span,
                    module_path,
                );
            }
            ItemKind::Enum(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Enum,
                    item.span,
                    module_path,
                );
                self.register_enum_variants(decl, item.span);
            }
            ItemKind::Trait(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Trait,
                    item.span,
                    module_path,
                );
            }
            ItemKind::TypeAlias(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::TypeAlias,
                    item.span,
                    module_path,
                );
            }
            ItemKind::Const(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Const,
                    item.span,
                    module_path,
                );
            }
            ItemKind::Static(decl) => {
                self.register_item_with_module(
                    item.id,
                    &decl.name,
                    DefKind::Static,
                    item.span,
                    module_path,
                );
            }
            ItemKind::Mod(decl) => {
                self.register_item(item.id, &decl.name, DefKind::Mod, item.span);
                if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                    module_path.push(decl.name.name.clone());
                    self.collect_items_in(inner, module_path);
                    module_path.pop();
                }
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

    /// Like [`Self::register_item`] but also registers the item under
    /// its fully-qualified `mod1::mod2::name` path (when nested inside
    /// inline modules), so cross-module call-site lookups (`other::greet`)
    /// resolve directly to the function's `DefId` and the type checker
    /// can pick up the function's declared return type.
    fn register_item_with_module(
        &mut self,
        node: NodeId,
        name: &Ident,
        kind: DefKind,
        span: Span,
        module_path: &[String],
    ) {
        if module_path.is_empty() {
            self.register_item(node, name, kind, span);
            return;
        }
        let def = self.alloc_def(node, kind);
        let binding = Binding::def(def, kind);
        let module = self.scopes.module_mut();
        // Register the bare name so callers inside the module can
        // still write `name(...)` (HIR-flatten visibility).
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
        // Also register `mod1::mod2::name` so cross-module callers
        // resolve directly to this def. Failure to insert here is a
        // benign duplicate - another sibling module declared the
        // same fully-qualified path.
        let qualified = format!("{}::{}", module_path.join("::"), name.name);
        let module = self.scopes.module_mut();
        if kind.is_type_ns() {
            let _ = module.insert_type(&qualified, binding);
        }
        if kind.is_value_ns() {
            let _ = module.insert_value(&qualified, binding);
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
            ItemKind::Mod(decl) => {
                if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                    for nested in inner {
                        if !crate::cfg::item_is_active(&nested.attrs) {
                            continue;
                        }
                        self.resolve_item(nested);
                    }
                }
            }
            ItemKind::AttrItem(_) => {}
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
            ExprKind::Error => {}
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

    fn resolve_literal(&self, _lit: &Literal) {}

    fn resolve_value_path(&mut self, path: &PathExpr, anchor: NodeId, span: Span) {
        let Some(head) = path.segments.first() else {
            return;
        };
        // `super::name` inside an inline child module (`mod tests {}`,
        // etc.) refers to the parent scope's bare name. The resolver
        // registers parent-scope items under their bare name in the
        // same module table the child reads from, so dropping the
        // `super::` prefix lets the regular flat lookup find them.
        let effective: Vec<&str> = path
            .segments
            .iter()
            .map(|s| s.name.name.as_str())
            .skip_while(|s| *s == "super")
            .collect();
        // Try the fully-qualified `mod1::mod2::name` form first so
        // sibling-module call sites (`other::greet`) resolve directly
        // to the function's [`DefId`] when the resolver registered
        // it via [`Self::register_item_with_module`]. The single
        // segment lookup is the fallback for plain paths and for
        // multi-segment paths whose head is something other than an
        // inline module (`fmt::println`, `http::Response::text` -
        // these stay opaque-by-head, matching the historical
        // tree-walker behaviour).
        if effective.len() > 1 {
            let joined = effective.join("::");
            if let Some(resolution) = self.lookup_value_or_type(&joined) {
                self.resolutions.insert(anchor, resolution);
                for segment in &path.segments {
                    self.resolve_generic_args(&segment.generics);
                }
                return;
            }
            // Root-cause stdlib-member validation. The resolver is
            // opaque-by-head for stdlib paths - it has no per-module
            // export model, so `module::nonexistent` slipped through
            // `check` and only failed at runtime (GX0002). The
            // generated table in `stdlib_exports` lists every
            // `module::member` the runtime actually binds, so a
            // two-segment value path whose head is a known stdlib
            // module but whose full name is absent is definitively an
            // unresolved name. User-defined module members never reach
            // here: they resolve via the joined lookup above (a real
            // binding) and return. Restricted to two segments so
            // `module::Type::method` paths stay opaque-by-head.
            if effective.len() == 2
                && crate::stdlib_exports::STDLIB_MODULES
                    .binary_search(&effective[0])
                    .is_ok()
                && crate::stdlib_exports::STDLIB_QUALIFIED
                    .binary_search(&joined.as_str())
                    .is_err()
            {
                self.emit(ResolveError::UnresolvedName { name: joined }, span);
                self.resolutions.insert(anchor, Resolution::Err);
                for segment in &path.segments {
                    self.resolve_generic_args(&segment.generics);
                }
                return;
            }
        }
        let head_name = head.name.name.clone();
        let lookup_name = effective.first().copied().unwrap_or(head_name.as_str());
        // For multi-segment paths (`a::b::c`), the head can only be a
        // module / type / trait - never a local value binding. A
        // local variable that happens to share a module's name (a
        // common shadow, e.g. `let mut provider = "";` plus
        // `mod provider`) must not capture `provider::xxx` paths.
        // The flat lookup-value-then-type fallback below would
        // otherwise resolve the head to the local binding, and the
        // VM tier would then dispatch the call as a method-on-string,
        // silently returning Unit.
        let resolution = if effective.len() > 1 {
            self.scopes.lookup_type(lookup_name).map(|b| b.resolution)
        } else {
            self.lookup_value_or_type(lookup_name)
        };
        let resolution = resolution.unwrap_or_else(|| {
            self.emit(
                ResolveError::UnresolvedName {
                    name: lookup_name.to_string(),
                },
                span,
            );
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
        // Strip leading module-relative prefixes (`super`/`crate`/`self`)
        // so a struct literal written inside an inline child module
        // (`super::P { .. }` in a `#[cfg(test)] mod tests {}`) resolves
        // the parent module's type, the same way `resolve_value_path`
        // resolves a `super::foo()` call. The head of the stripped path
        // is the type name (`super::P` -> `P`, `Shape::Rect` -> `Shape`).
        let effective: Vec<&str> = path
            .segments
            .iter()
            .map(|s| s.name.name.as_str())
            .skip_while(|s| matches!(*s, "super" | "crate" | "self"))
            .collect();
        let lookup_name = effective
            .first()
            .copied()
            .unwrap_or(head.name.name.as_str());
        // A sibling-module-qualified type (`other::Widget { .. }`)
        // registers under its joined name; prefer that, then fall back
        // to the bare head (covers enum struct-variant literals like
        // `Shape::Rect { .. }`, whose head is the enum type).
        let joined = (effective.len() > 1)
            .then(|| {
                self.scopes
                    .lookup_type(&effective.join("::"))
                    .map(|b| b.resolution)
            })
            .flatten();
        let resolution = joined
            .or_else(|| self.scopes.lookup_type(lookup_name).map(|b| b.resolution))
            .unwrap_or_else(|| {
                self.emit(
                    ResolveError::UnresolvedName {
                        name: lookup_name.to_string(),
                    },
                    span,
                );
                Resolution::Err
            });
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
            | PatternKind::Range { .. }
            | PatternKind::Error => {}
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
