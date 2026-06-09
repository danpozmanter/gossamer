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
    checker.infer.default_unresolved_float_vars(checker.tcx);
    checker.resolve_table();
    (checker.table, checker.diagnostics)
}

/// Hard limit on type-checker recursion depth. Mirrors the parser's
/// guard and keeps adversarial input that survives parsing from
/// blowing the C stack inside [`TypeChecker::check_expr`].
const RECURSION_LIMIT: u32 = 256;

struct TypeChecker<'a> {
    tcx: &'a mut TyCtxt,
    infer: InferCtxt,
    table: TypeTable,
    diagnostics: Vec<TypeDiagnostic>,
    resolutions: &'a Resolutions,
    scopes: Vec<HashMap<gossamer_lex::Symbol, Ty>>,
    binding_types: HashMap<NodeId, Ty>,
    /// The `Self` type of the `impl` block currently being checked,
    /// so a method's `self` receiver binds to the concrete type
    /// instead of a free inference var. Without this, `self.field`
    /// reads inside a method leave the field type unresolved and a
    /// `for x in self.items` loop binds `x` at the i64 default —
    /// printing a `[String]` field's element pointers as integers.
    current_self_ty: Option<Ty>,
    /// Running depth of recursive entries into expression / block /
    /// pattern type checks. Reaching [`RECURSION_LIMIT`] short-circuits
    /// the offending subtree to `tcx.error_ty()` after emitting one
    /// diagnostic.
    recursion_depth: u32,
    /// `true` once the recursion-limit diagnostic has been emitted in
    /// the current source file. Prevents flooding the diagnostic
    /// stream with duplicates.
    recursion_limit_reported: bool,
    /// Ordered field name + type for every named struct, keyed by
    /// the struct's `DefId`. Built during `collect_signatures` so
    /// field-access and struct-literal expressions can resolve leaf
    /// types without having to look up the original AST.
    struct_fields: HashMap<gossamer_resolve::DefId, Vec<(String, Ty)>>,
    /// Cached function signatures keyed by `DefId`. Built during
    /// `collect_signatures` so a cross-function call site can pull
    /// the input/return types instead of returning a fresh var.
    fn_sigs: HashMap<gossamer_resolve::DefId, FnSig>,
    /// Declared types for `const NAME: T = ...` items, keyed by
    /// `DefId`. Without this, a path expression that resolves to a
    /// const falls back to a fresh inference variable, leaving the
    /// use site unconstrained and the codegen reading the slot at
    /// the wrong layout.
    const_tys: HashMap<gossamer_resolve::DefId, Ty>,
    /// Generic-parameter arity for every named struct, keyed by
    /// the struct's `DefId`. Built during `register_struct`. Used
    /// at struct-literal sites to allocate one fresh inference
    /// variable per parameter and substitute them into the
    /// declared field types' `TyKind::Param` slots. Without this,
    /// a `Pair<A, B> { fst: 10, snd: "hi" }` literal would fail
    /// to unify the `A`/`B` `Param` slots against `i64` /
    /// `String` and surface a confusing `type mismatch` against
    /// the rigid `Param`.
    struct_generic_arity: HashMap<gossamer_resolve::DefId, usize>,
    /// Currently-active generic-parameter name → `ParamIdx`
    /// mapping. Populated while walking a struct / enum / fn /
    /// impl declaration so `type_from_ast_path` can render a
    /// type-parameter reference (`A`, `B`) as the right
    /// `TyKind::Param` index. Keyed by name because the AST's
    /// generic param reference and its definition site use names,
    /// not indices.
    ///
    /// `GenericParam::Type` carries an `Ident` without a
    /// resolver-assigned `NodeId`.
    current_generic_scope: HashMap<String, (crate::ParamIdx, Box<str>)>,
    /// Trait names declared in this source file. Populated upfront
    /// by `collect_signatures` from every `ItemKind::Trait`. Used
    /// by `register_fn_sig` to validate that each `<T: Bound>`
    /// names a trait that actually exists — typos surface as a
    /// `GT0011 unknown-trait-bound` diagnostic at declaration time
    /// instead of as a runtime "no method" error later.
    declared_trait_names: std::collections::HashSet<String>,
    /// Local `let`-binding pattern nodes whose value flows into a
    /// stdlib `archive::{tar,zip}::write` call. A pre-scan of each
    /// function body fills this so the binding's literal initializer
    /// is re-typed to the `[(String, [u8])]` parameter — backward
    /// inference the single-pass checker can't otherwise reach, which
    /// the compiled tier needs so the nested byte arrays become heap
    /// Vecs instead of fixed inline arrays.
    write_arg_bindings: HashMap<NodeId, Ty>,
}

impl<'a> TypeChecker<'a> {
    fn new(tcx: &'a mut TyCtxt, resolutions: &'a Resolutions) -> Self {
        register_stdlib_struct_fields(tcx);
        let mut checker_struct_fields = HashMap::new();
        seed_checker_stdlib_struct_fields(tcx, &mut checker_struct_fields);
        Self {
            tcx,
            infer: InferCtxt::new(),
            table: TypeTable::new(),
            diagnostics: Vec::new(),
            resolutions,
            scopes: vec![HashMap::new()],
            binding_types: HashMap::new(),
            current_self_ty: None,
            recursion_depth: 0,
            recursion_limit_reported: false,
            struct_fields: checker_struct_fields,
            fn_sigs: HashMap::new(),
            const_tys: HashMap::new(),
            struct_generic_arity: HashMap::new(),
            current_generic_scope: HashMap::new(),
            declared_trait_names: std::collections::HashSet::new(),
            write_arg_bindings: HashMap::new(),
        }
    }

    /// Returns `Err(())` once the recursion counter hits
    /// [`RECURSION_LIMIT`], emitting a one-shot diagnostic at `span`.
    /// Callers should respond by returning `tcx.error_ty()` so the
    /// caller's caller stops walking into the doomed subtree.
    fn enter_recursion(&mut self, span: Span) -> Result<(), ()> {
        if self.recursion_depth >= RECURSION_LIMIT {
            if !self.recursion_limit_reported {
                self.recursion_limit_reported = true;
                self.emit(
                    TypeError::RecursionLimit {
                        limit: RECURSION_LIMIT,
                    },
                    span,
                );
            }
            return Err(());
        }
        self.recursion_depth += 1;
        Ok(())
    }

    /// Pairs with [`Self::enter_recursion`]. Decrements the counter.
    fn leave_recursion(&mut self) {
        self.recursion_depth = self.recursion_depth.saturating_sub(1);
    }

    /// Pushes a generic-parameter scope while walking a declaration
    /// (struct, enum, fn, trait, impl). Each parameter name maps to
    /// its position so `type_from_ast_path` renders references as
    /// the right `TyKind::Param`. Returns the prior scope so the
    /// caller can restore it.
    fn enter_generic_scope(
        &mut self,
        generics: &gossamer_ast::Generics,
    ) -> HashMap<String, (crate::ParamIdx, Box<str>)> {
        let prior = std::mem::take(&mut self.current_generic_scope);
        for (i, param) in generics.params.iter().enumerate() {
            if let gossamer_ast::GenericParam::Type { name, .. } = param {
                let owned: Box<str> = name.name.clone().into_boxed_str();
                self.current_generic_scope
                    .insert(name.name.clone(), (crate::ParamIdx(i as u32), owned));
            }
        }
        prior
    }

    /// Restores a generic-parameter scope saved by
    /// [`Self::enter_generic_scope`].
    fn leave_generic_scope(&mut self, prior: HashMap<String, (crate::ParamIdx, Box<str>)>) {
        self.current_generic_scope = prior;
    }

    fn fresh(&mut self) -> Ty {
        self.infer.fresh_var(self.tcx)
    }

    /// Walks `ty` and substitutes each `TyKind::Param { idx }`
    /// reference with `substs[idx]`. Used at struct-literal and
    /// generic-call sites where the declared field/parameter
    /// types carry rigid `Param` slots that must be replaced by
    /// fresh inference vars (or by explicit generic arguments)
    /// before unification.
    ///
    /// Out-of-range `idx` falls back to the original `ty` so a
    /// malformed declaration produces a deferred unification
    /// error rather than a panic.
    fn subst_params_in_ty(&mut self, ty: Ty, substs: &[Ty]) -> Ty {
        if substs.is_empty() {
            return ty;
        }
        let kind = self.tcx.kind_of(ty).clone();
        match kind {
            TyKind::Param { idx, .. } => substs.get(idx.0 as usize).copied().unwrap_or(ty),
            TyKind::Ref { inner, mutability } => {
                let new_inner = self.subst_params_in_ty(inner, substs);
                if new_inner == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::Ref {
                        inner: new_inner,
                        mutability,
                    })
                }
            }
            TyKind::Tuple(elems) => {
                let new_elems: Vec<Ty> = elems
                    .iter()
                    .map(|e| self.subst_params_in_ty(*e, substs))
                    .collect();
                if new_elems == elems {
                    ty
                } else {
                    self.tcx.intern(TyKind::Tuple(new_elems))
                }
            }
            TyKind::Array { elem, len } => {
                let new_elem = self.subst_params_in_ty(elem, substs);
                if new_elem == elem {
                    ty
                } else {
                    self.tcx.intern(TyKind::Array {
                        elem: new_elem,
                        len,
                    })
                }
            }
            TyKind::Slice(elem) => {
                let new = self.subst_params_in_ty(elem, substs);
                if new == elem {
                    ty
                } else {
                    self.tcx.intern(TyKind::Slice(new))
                }
            }
            TyKind::Vec(elem) => {
                let new = self.subst_params_in_ty(elem, substs);
                if new == elem {
                    ty
                } else {
                    self.tcx.intern(TyKind::Vec(new))
                }
            }
            TyKind::HashMap { key, value } => {
                let new_k = self.subst_params_in_ty(key, substs);
                let new_v = self.subst_params_in_ty(value, substs);
                if new_k == key && new_v == value {
                    ty
                } else {
                    self.tcx.intern(TyKind::HashMap {
                        key: new_k,
                        value: new_v,
                    })
                }
            }
            TyKind::Sender(inner) => {
                let new = self.subst_params_in_ty(inner, substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::Sender(new))
                }
            }
            TyKind::Receiver(inner) => {
                let new = self.subst_params_in_ty(inner, substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::Receiver(new))
                }
            }
            TyKind::JoinHandle(inner) => {
                let new = self.subst_params_in_ty(inner, substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::JoinHandle(new))
                }
            }
            // Adt / Alias / Closure carry their own substs lists;
            // substituting inside them is the monomorph layer's
            // responsibility. For the struct-literal use case
            // those sub-Adts already have concrete (or fresh-var)
            // substs from their own typeck, so we leave them
            // alone.
            _ => ty,
        }
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
            let resolved = self.deep_resolve(ty);
            if resolved != ty {
                self.table.insert(node, resolved);
            }
        }
    }

    /// Resolves a type deeply — after shallow-resolving top-level `Var`
    /// nodes, recurses into `FnPtr` / `FnTrait` sigs so that compound
    /// types like `FnPtr(FnSig { output: Var(1) })` are fully grounded
    /// when the inference var was unified with a concrete type.
    fn deep_resolve(&mut self, ty: Ty) -> Ty {
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind_of(resolved).clone() {
            TyKind::FnPtr(sig) => {
                let out = self.deep_resolve(sig.output);
                let inputs: Vec<Ty> = sig.inputs.iter().map(|&t| self.deep_resolve(t)).collect();
                if out != sig.output || inputs != sig.inputs {
                    self.tcx.intern(TyKind::FnPtr(FnSig {
                        inputs,
                        output: out,
                    }))
                } else {
                    resolved
                }
            }
            TyKind::FnTrait(sig) => {
                let out = self.deep_resolve(sig.output);
                let inputs: Vec<Ty> = sig.inputs.iter().map(|&t| self.deep_resolve(t)).collect();
                if out != sig.output || inputs != sig.inputs {
                    self.tcx.intern(TyKind::FnTrait(FnSig {
                        inputs,
                        output: out,
                    }))
                } else {
                    resolved
                }
            }
            // Recurse into generic arguments / element types so a
            // `Triple<?4, ?5, ?6>` whose inference vars unified to
            // `<i64, String, f64>` is recorded as the concrete
            // `Triple<i64, String, f64>`. Without this, a generic
            // struct's field access (`r.third`) reads the field's
            // `Param(n)` against unresolved-`Var` substs and the field
            // local defaults to i64/ptr — printing an `f64`'s bit
            // pattern or strlen'ing a non-pointer.
            TyKind::Adt { def, substs } => {
                let new_args: Vec<crate::GenericArg> = substs
                    .as_slice()
                    .iter()
                    .map(|arg| match arg {
                        crate::GenericArg::Type(t) => {
                            crate::GenericArg::Type(self.deep_resolve(*t))
                        }
                        crate::GenericArg::Const(c) => crate::GenericArg::Const(*c),
                    })
                    .collect();
                let new_substs = crate::Substs::from_args(new_args);
                if new_substs == substs {
                    resolved
                } else {
                    self.tcx.intern(TyKind::Adt {
                        def,
                        substs: new_substs,
                    })
                }
            }
            // Recurse into element / payload types so a composite whose
            // inner inference var only gained a concrete type via the
            // late integer/float defaulting (`let v = if c { [1, 2] }
            // else { [3, 4] }` -> `[i64; 2]`) is recorded fully grounded.
            // Without this the recorded node keeps `[?v; 2]` and the
            // format/codegen dispatch can't classify the element.
            TyKind::Array { elem, len } => {
                let new_elem = self.deep_resolve(elem);
                if new_elem == elem {
                    resolved
                } else {
                    self.tcx.intern(TyKind::Array {
                        elem: new_elem,
                        len,
                    })
                }
            }
            TyKind::Slice(elem) => self.deep_resolve_wrap(resolved, elem, TyKind::Slice),
            TyKind::Vec(elem) => self.deep_resolve_wrap(resolved, elem, TyKind::Vec),
            TyKind::Sender(elem) => self.deep_resolve_wrap(resolved, elem, TyKind::Sender),
            TyKind::Receiver(elem) => self.deep_resolve_wrap(resolved, elem, TyKind::Receiver),
            TyKind::JoinHandle(elem) => self.deep_resolve_wrap(resolved, elem, TyKind::JoinHandle),
            TyKind::Ref { mutability, inner } => {
                let new_inner = self.deep_resolve(inner);
                if new_inner == inner {
                    resolved
                } else {
                    self.tcx.intern(TyKind::Ref {
                        mutability,
                        inner: new_inner,
                    })
                }
            }
            TyKind::Tuple(elems) => {
                let new: Vec<Ty> = elems.iter().map(|&t| self.deep_resolve(t)).collect();
                if new == elems {
                    resolved
                } else {
                    self.tcx.intern(TyKind::Tuple(new))
                }
            }
            TyKind::HashMap { key, value } => {
                let k = self.deep_resolve(key);
                let v = self.deep_resolve(value);
                if k == key && v == value {
                    resolved
                } else {
                    self.tcx.intern(TyKind::HashMap { key: k, value: v })
                }
            }
            _ => resolved,
        }
    }

    /// Deep-resolves a single-payload composite (`Vec`/`Slice`/channel
    /// endpoints) and re-interns it through `wrap` only when the payload
    /// actually changed.
    fn deep_resolve_wrap(&mut self, resolved: Ty, elem: Ty, wrap: fn(Ty) -> TyKind) -> Ty {
        let new_elem = self.deep_resolve(elem);
        if new_elem == elem {
            resolved
        } else {
            self.tcx.intern(wrap(new_elem))
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
            scope.insert(gossamer_lex::Symbol::intern(name), ty);
        }
    }

    fn lookup_local(&self, name: &str) -> Option<Ty> {
        let sym = gossamer_lex::Symbol::intern(name);
        for scope in self.scopes.iter().rev() {
            if let Some(ty) = scope.get(&sym) {
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
        // First pass: index every trait name declared in this
        // tree so subsequent `register_fn_sig` calls can validate
        // `<T: Bound>` bounds against it.
        self.collect_trait_names(items);
        for item in items {
            match &item.kind {
                ItemKind::Fn(decl) => self.register_fn_sig(item.id, decl, item.span),
                ItemKind::Impl(decl) => self.collect_impl_signatures(decl),
                ItemKind::Trait(decl) => self.collect_trait_signatures(decl),
                ItemKind::Struct(decl) => {
                    self.register_struct(item.id, decl);
                }
                ItemKind::Enum(decl) => self.register_enum(item.id, decl),
                ItemKind::Const(decl) => self.register_const(item.id, &decl.ty),
                ItemKind::Mod(decl) => {
                    if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                        self.collect_signatures(inner);
                    }
                }
                _ => {}
            }
        }
    }

    /// Walks `items` recursively into inline modules and records
    /// every `ItemKind::Trait` name. Idempotent — re-calling
    /// adds to the existing set.
    fn collect_trait_names(&mut self, items: &[Item]) {
        for item in items {
            match &item.kind {
                ItemKind::Trait(decl) => {
                    self.declared_trait_names.insert(decl.name.name.clone());
                }
                ItemKind::Mod(decl) => {
                    if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                        self.collect_trait_names(inner);
                    }
                }
                _ => {}
            }
        }
    }

    fn register_const(&mut self, item_id: NodeId, ty: &gossamer_ast::Type) {
        let Some(def) = self.resolutions.definition_of(item_id) else {
            return;
        };
        let resolved = self.type_from_ast(ty);
        self.const_tys.insert(def, resolved);
    }

    /// Registers an enum's `DefId -> name` so `render_ty` / `adt_dispatch_name`
    /// recover "Shape" instead of the "adt#N" placeholder — needed for `==` /
    /// `{:?}` dispatch on enum values whose type resolves to the Adt.
    fn register_enum(&mut self, item_id: NodeId, decl: &gossamer_ast::EnumDecl) {
        if let Some(def) = self.resolutions.definition_of(item_id) {
            self.tcx.register_def_name(def, decl.name.name.as_str());
        }
    }

    fn register_struct(&mut self, item_id: NodeId, decl: &gossamer_ast::StructDecl) {
        let Some(def) = self.resolutions.definition_of(item_id) else {
            return;
        };
        let name = decl.name.name.as_str();
        self.tcx.register_def_name(def, name);
        // Build the generic-parameter scope so `Pair<A, B> { fst:
        // A, snd: B }` field-type references resolve to the right
        // `TyKind::Param` indices.
        let prior_scope = self.enter_generic_scope(&decl.generics);
        // Record the struct's generic-parameter arity in source
        // order so struct-literal substitution at use sites knows
        // how many fresh inference variables to allocate.
        let arity = decl
            .generics
            .params
            .iter()
            .filter(|p| matches!(p, gossamer_ast::GenericParam::Type { .. }))
            .count();
        if arity > 0 {
            self.struct_generic_arity.insert(def, arity);
        }
        if let StructBody::Named(fields) = &decl.body {
            let list: Vec<(String, Ty)> = fields
                .iter()
                .map(|f| (f.name.name.clone(), self.type_from_ast(&f.ty)))
                .collect();
            let tys: Vec<Ty> = list.iter().map(|(_, t)| *t).collect();
            self.tcx.register_struct_fields(def, tys);
            self.struct_fields.insert(def, list);
        }
        self.leave_generic_scope(prior_scope);
    }

    /// Resolves `receiver_ty.field_name` to the leaf field type.
    /// Auto-dereferences through `&T`/`&mut T` wrappers. Returns
    /// `None` when the receiver does not name a known struct or the
    /// field is not declared on it.
    /// Resolves field access to a type, distinguishing failure
    /// modes worth surfacing to the user:
    ///
    /// - `Err(UnknownField { opaque: true })` — the receiver is an
    ///   `Adt` whose field map isn't registered (typical of opaque
    ///   stdlib types like `json::Value`).
    /// - `Err(UnknownField { opaque: false })` — the receiver is a
    ///   known struct but the field name doesn't match any of its
    ///   fields.
    ///
    /// Non-Adt receivers (primitives, tuples, unresolved inference
    /// vars, generic params) get `Ok(fresh_var)` so the rest of the
    /// expression keeps type-checking. Catching those would either
    /// fight the trait-method machinery or block legitimate
    /// inference.
    fn lookup_field_ty_diagnosed(
        &mut self,
        receiver_ty: Ty,
        field_name: &str,
    ) -> Result<Ty, TypeError> {
        let resolved = self.infer.resolve(self.tcx, receiver_ty);
        let mut cur = resolved;
        loop {
            match self.tcx.kind_of(cur).clone() {
                TyKind::Ref { inner, .. } => cur = inner,
                TyKind::Adt { def, substs } => {
                    let ty_name = crate::printer::render_ty(self.tcx, resolved);
                    let Some(fields) = self.struct_fields.get(&def).cloned() else {
                        return Err(TypeError::UnknownField {
                            ty: ty_name,
                            field: field_name.to_string(),
                            opaque: true,
                        });
                    };
                    for (name, ty) in fields {
                        if name == field_name {
                            // Substitute `TyKind::Param { idx }`
                            // slots in the declared field type
                            // with the matching generic argument
                            // from the receiver's `substs`. This
                            // is the dual of the substitution at
                            // struct-literal sites: literals
                            // allocate fresh vars for each
                            // parameter; field reads need to
                            // resolve `Param` back to the
                            // receiver's per-instance argument.
                            let substs_vec = substs.types();
                            return Ok(self.subst_params_in_ty(ty, &substs_vec));
                        }
                    }
                    return Err(TypeError::UnknownField {
                        ty: ty_name,
                        field: field_name.to_string(),
                        opaque: false,
                    });
                }
                _ => return Ok(self.fresh()),
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

    fn register_fn_sig(&mut self, node: NodeId, decl: &FnDecl, span: Span) {
        let sig = self.fn_sig_of(decl);
        if let Some(def) = self.resolutions.definition_of(node) {
            self.fn_sigs.insert(def, sig);
        }
        // Validate that every declared trait bound on the fn's
        // generic parameters names a trait this source file (or a
        // recognised built-in) actually declares. Catches typos
        // (`Hashabel` → `Hashable`) at the declaration site
        // instead of as a runtime "no method" error later.
        for param in &decl.generics.params {
            if let gossamer_ast::GenericParam::Type { name, bounds, .. } = param {
                for bound in bounds {
                    let Some(seg) = bound.path.segments.last() else {
                        continue;
                    };
                    let bound_name = seg.name.name.as_str();
                    if bound_name.is_empty() {
                        continue;
                    }
                    let resolved = self.declared_trait_names.contains(bound_name)
                        || known_builtin_trait(bound_name);
                    if !resolved {
                        self.emit(
                            TypeError::UnknownTraitBound {
                                param: name.name.clone(),
                                name: bound_name.to_string(),
                            },
                            span,
                        );
                    }
                }
            }
        }
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
                // Type the impl's `Self` so HIR lowering can pin
                // each method's `self` parameter to it. Without
                // this, `self.field` reads later fall through MIR
                // lowering's struct-name lookup and abort with
                // the unsupported placeholder.
                let self_ty = self.type_from_ast(&decl.self_ty);
                let prev_self = self.current_self_ty.replace(self_ty);
                for impl_item in &decl.items {
                    if let ImplItem::Fn(fn_decl) = impl_item {
                        self.check_fn(fn_decl);
                    } else if let ImplItem::Const { value, .. } = impl_item {
                        self.check_expr(value);
                    }
                }
                self.current_self_ty = prev_self;
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
                self.recoerce_arg_literal(&decl.value, annotated);
            }
            ItemKind::Static(decl) => {
                let annotated = self.type_from_ast(&decl.ty);
                let init = self.check_expr(&decl.value);
                self.unify(annotated, init, decl.value.span);
                self.recoerce_arg_literal(&decl.value, annotated);
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
            ItemKind::Mod(decl) => {
                if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                    for nested in inner {
                        self.check_item(nested);
                    }
                }
            }
            ItemKind::AttrItem(_) => {}
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
            self.collect_write_arg_bindings(body);
            let body_ty = self.check_expr(body);
            self.unify(ret, body_ty, body.span);
            // Re-record an array/tuple literal in the return position
            // (block tail / branch / arm) to the declared return shape so
            // `fn f() -> Vec<T> { [..] }` yields a growable Vec, not `[T; N]`.
            self.recoerce_arg_literal(body, ret);
        }
        self.pop_scope();
    }

    /// Pre-scans a function body for `archive::{tar,zip}::write(arg)`
    /// calls whose single argument is a path to a local binding, and
    /// records that binding's node so its literal initializer is later
    /// re-typed to the `[(String, [u8])]` parameter.
    fn collect_write_arg_bindings(&mut self, body: &Expr) {
        let mut collector = WriteArgPathCollector {
            arg_paths: Vec::new(),
        };
        gossamer_ast::visitor::Visitor::visit_expr(&mut collector, body);
        if collector.arg_paths.is_empty() {
            return;
        }
        let s = self.tcx.string_ty();
        let u8_ty = self.tcx.int_ty(IntTy::U8);
        let vec_u8 = self.tcx.intern(TyKind::Vec(u8_ty));
        let pair = self.tcx.intern(TyKind::Tuple(vec![s, vec_u8]));
        let vec_pair = self.tcx.intern(TyKind::Vec(pair));
        for path_node in collector.arg_paths {
            if let Some(Resolution::Local(binding)) = self.resolutions.get(path_node) {
                self.write_arg_bindings.insert(binding, vec_pair);
            }
        }
    }

    fn bind_fn_param(&mut self, param: &FnParam) {
        match param {
            FnParam::Typed { pattern, ty } => {
                let param_ty = self.type_from_ast(ty);
                self.bind_pattern(pattern, param_ty);
            }
            FnParam::Receiver(recv) => {
                // Bind `self` to the enclosing `impl`'s `Self` type so
                // `self.field` accesses resolve; fall back to a fresh
                // var only outside an impl context (defensive). `&self`
                // / `&mut self` wrap it in a reference so the type
                // matches the receiver form.
                let ty = match self.current_self_ty {
                    Some(self_ty) => match recv {
                        gossamer_ast::Receiver::Owned => self_ty,
                        gossamer_ast::Receiver::RefShared => self.tcx.intern(TyKind::Ref {
                            mutability: Mutbl::Not,
                            inner: self_ty,
                        }),
                        gossamer_ast::Receiver::RefMut => self.tcx.intern(TyKind::Ref {
                            mutability: Mutbl::Mut,
                            inner: self_ty,
                        }),
                    },
                    None => self.fresh(),
                };
                self.bind_local("self", ty);
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr) -> Ty {
        if self.enter_recursion(expr.span).is_err() {
            let err = self.tcx.error_ty();
            return self.record(expr.id, err);
        }
        let ty = self.check_expr_kind(expr);
        self.leave_recursion();
        self.record(expr.id, ty)
    }

    #[allow(
        clippy::too_many_lines,
        reason = "expression dispatch — arms map 1:1 to ExprKind variants; splitting hides the dispatch table"
    )]
    fn check_expr_kind(&mut self, expr: &Expr) -> Ty {
        match &expr.kind {
            ExprKind::Literal(lit) => self.type_of_literal(lit, expr.span),
            ExprKind::Path(path) => self.check_path_expr(expr.id, path, expr.span),
            ExprKind::Call { callee, args } => self.check_call(callee, args),
            ExprKind::MethodCall { receiver, args, .. } => self.check_method_call(receiver, args),
            ExprKind::FieldAccess { receiver, field } => {
                let receiver_ty = self.check_expr(receiver);
                match field {
                    gossamer_ast::FieldSelector::Named(name) => {
                        match self.lookup_field_ty_diagnosed(receiver_ty, &name.name) {
                            Ok(ty) => ty,
                            Err(err) => {
                                self.emit(err, expr.span);
                                self.fresh()
                            }
                        }
                    }
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
                // Resolve the header path to an Adt type. Unifying
                // named field values with the declared field
                // types lets downstream field-access nodes see
                // concrete leaf types.
                //
                // For a generic struct (`Pair<A, B>`), the
                // declared field types carry `TyKind::Param`
                // slots. We allocate one fresh inference variable
                // per generic parameter and substitute those into
                // each field type before unifying with the
                // literal's value type — that lets the inferencer
                // pin `A` and `B` from the field values.
                let head_node = expr.id;
                let (struct_ty, substs_table) = if let Some(res) = self.resolutions.get(head_node) {
                    match res {
                        Resolution::Def {
                            def,
                            kind:
                                gossamer_resolve::DefKind::Struct | gossamer_resolve::DefKind::Enum,
                        } => {
                            let arity = self.struct_generic_arity.get(&def).copied().unwrap_or(0);
                            let substs: Vec<Ty> = (0..arity).map(|_| self.fresh()).collect();
                            let substs_obj = crate::Substs::from_types(substs.iter().copied());
                            (
                                self.tcx.intern(TyKind::Adt {
                                    def,
                                    substs: substs_obj,
                                }),
                                substs,
                            )
                        }
                        _ => (self.fresh(), Vec::new()),
                    }
                } else {
                    (self.fresh(), Vec::new())
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
                                // Substitute `Param { idx }` slots
                                // with the fresh inference vars
                                // allocated above so unification
                                // can drive `A`, `B`, ... from
                                // each literal's value type.
                                let dty_sub = self.subst_params_in_ty(*dty, &substs_table);
                                self.unify(dty_sub, val_ty, value.span);
                                // A Vec/slice-typed field initialized with an
                                // array literal: re-record it as the growable
                                // shape so `S { xs: ["a", "b"] }` lays a heap
                                // Vec, not a fixed `[T; N]`, into the struct.
                                self.recoerce_arg_literal(value, dty_sub);
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
            ExprKind::Try(inner) => {
                let inner_ty = self.check_expr(inner);
                self.unwrap_result_like(inner_ty)
                    .unwrap_or_else(|| self.fresh())
            }
            ExprKind::Go(inner) => {
                self.check_expr(inner);
                self.fresh()
            }
            ExprKind::Select(arms) => self.check_select(arms),
            ExprKind::MacroCall(_) | ExprKind::Error => self.fresh(),
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr]) -> Ty {
        let callee_ty = self.check_expr(callee);
        let arg_tys: Vec<Ty> = args.iter().map(|a| self.check_expr(a)).collect();
        let resolved = self.infer.resolve(self.tcx, callee_ty);
        let kind = self.tcx.kind(resolved).cloned();
        // Recognised callee shapes: `FnPtr` (anonymous or first-class
        // closure pointer) and `FnDef { def, .. }` (named function
        // resolved to a definition). Looking the def up in
        // `fn_sigs` lets cross-function call sites pin both args and
        // return type to the callee's signature instead of returning
        // a fresh inference variable that never gets bound.
        let sig_lookup: Option<FnSig> = match kind {
            Some(TyKind::FnPtr(sig)) => Some(sig),
            Some(TyKind::FnDef { def, .. }) => self.fn_sigs.get(&def).cloned(),
            _ => None,
        };
        if let Some(sig) = sig_lookup {
            if sig.inputs.len() == arg_tys.len() {
                for (param, (arg_ty, arg_expr)) in sig.inputs.iter().zip(arg_tys.iter().zip(args)) {
                    // Auto-coerce `T` ↔ `&T` at call boundaries: a
                    // signature param `&Shape` accepts a `Shape`
                    // argument and vice versa. Native lowering
                    // already treats every value-vs-reference
                    // distinction as a no-op (the GC owns
                    // everything), so enforcing strict match here
                    // would only produce diagnostics on programs
                    // the runtime accepts.
                    let param_inner = match self.tcx.kind(*param) {
                        Some(TyKind::Ref { inner, .. }) => Some(*inner),
                        _ => None,
                    };
                    let arg_inner = match self.tcx.kind(*arg_ty) {
                        Some(TyKind::Ref { inner, .. }) => Some(*inner),
                        _ => None,
                    };
                    let (lhs, rhs) = match (param_inner, arg_inner) {
                        (Some(p), None) => (p, *arg_ty),
                        (None, Some(a)) => (*param, a),
                        _ => (*param, *arg_ty),
                    };
                    self.unify(lhs, rhs, arg_expr.span);
                    // Re-type an array/tuple literal argument to match
                    // the parameter so a nested `[1, 2, 3]` byte array
                    // inside a `(String, [u8])` tuple is recorded as a
                    // Vec rather than a fixed `[i64; N]` — the compiled
                    // tier then builds a heap Vec at every level and
                    // the layout stays consistent end to end.
                    self.recoerce_arg_literal(arg_expr, *param);
                }
                return sig.output;
            }
        }
        // Fallback: known stdlib free functions whose signatures are
        // not present in `fn_sigs` (because they live outside user
        // source). Returning a real type instead of a fresh variable
        // lets the type checker catch mismatches such as returning
        // `Result<json::Value, String>` from a function declared
        // `Result<ComicResponse, String>`.
        if let ExprKind::Path(path) = &callee.kind {
            let names: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
            let (module, last) = names.split_at(names.len().saturating_sub(1));
            let Some(last) = last.first().copied() else {
                return self.fresh();
            };
            // `archive::tar::write` / `archive::zip::write` take
            // `[(String, [u8])]` and return `Result<[u8], Error>`.
            // These are stdlib (no `fn_sig`), so re-type the literal
            // argument against the synthesized parameter type so a
            // `[("a", [1, 2, 3])]` literal builds heap Vecs at every
            // level on the compiled tier.
            if last == "write"
                && matches!(module.last().copied(), Some("tar" | "zip"))
                && args.len() == 1
            {
                return self.archive_write_call_ty(&args[0]);
            }
            let module_ok = matches!(
                module,
                ["json"] | ["encoding", "json"] | ["std", "encoding", "json"]
            );
            if module_ok {
                match last {
                    "parse" | "decode" => {
                        let j = self.tcx.json_value_ty();
                        let e = self.tcx.dyn_error_ty();
                        return self.result_adt_ty(j, e);
                    }
                    "render" | "encode" => return self.tcx.string_ty(),
                    "get" | "at" | "as_array" | "identity" => {
                        return self.tcx.json_value_ty();
                    }
                    "as_i64" | "len" => return self.tcx.int_ty(IntTy::I64),
                    "as_f64" => return self.tcx.float_ty(FloatTy::F64),
                    "as_str" => return self.tcx.string_ty(),
                    "as_bool" | "is_null" => return self.tcx.bool_ty(),
                    _ => {}
                }
            }
            let errors_ok = matches!(module, ["errors"] | ["std", "errors"]);
            if errors_ok {
                match last {
                    "new" | "wrap" => return self.tcx.dyn_error_ty(),
                    _ => {}
                }
            }
            // Built-in intrinsics emitted by the parser's macro
            // expansion (`format!` only — `println!` / `print!` /
            // `eprintln!` / `eprint!` etc. expand to a call to the
            // outer name with the format-built string as the single
            // argument, and pinning `println` to Unit broke
            // generic-monomorph paths that route through user-named
            // functions called `println`). Pinning `__concat` and
            // `__fmt_prec` to `String` is safe: they're synthetic
            // names the parser injects and no user code can
            // shadow them.
            if module.is_empty() {
                match last {
                    "__concat" | "__fmt_prec" => {
                        return self.tcx.string_ty();
                    }
                    // Variant constructors. The resolver doesn't
                    // hand `Some` / `Ok` / `Err` / `None` a `DefId`,
                    // so the call expression typechecks as a fresh
                    // `Var` and the binding `let first = Some(10)`
                    // collapses to `Int(I64)` — losing the Adt
                    // wrapper. Match dispatch later treats the
                    // 8-byte `*mut GosResult` pointer as a raw i64
                    // and reads garbage from the slot. Recognise
                    // the four standard variants here and synthesise
                    // the right Adt: `Some(t)` → `Option<t>`,
                    // `Ok(t)` → `Result<t, ?>`, `Err(e)` →
                    // `Result<?, e>`, `None` → `Option<?>`.
                    "Some" => {
                        let payload = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                        return self.option_adt_ty(payload);
                    }
                    "None" => {
                        let payload = self.fresh();
                        return self.option_adt_ty(payload);
                    }
                    "Ok" => {
                        let ok_ty = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                        let err_ty = self.fresh();
                        return self.result_adt_ty(ok_ty, err_ty);
                    }
                    "Err" => {
                        let ok_ty = self.fresh();
                        let err_ty = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                        return self.result_adt_ty(ok_ty, err_ty);
                    }
                    _ => {}
                }
            }
        }
        self.fresh()
    }

    /// Re-types the single `[(String, [u8])]` argument of the stdlib
    /// `archive::{tar,zip}::write` calls and returns their
    /// `Result<[u8], Error>` output type.
    fn archive_write_call_ty(&mut self, arg: &Expr) -> Ty {
        let s = self.tcx.string_ty();
        let u8_ty = self.tcx.int_ty(IntTy::U8);
        let vec_u8 = self.tcx.intern(TyKind::Vec(u8_ty));
        let pair = self.tcx.intern(TyKind::Tuple(vec![s, vec_u8]));
        let vec_pair = self.tcx.intern(TyKind::Vec(pair));
        self.recoerce_arg_literal(arg, vec_pair);
        let e = self.tcx.dyn_error_ty();
        self.result_adt_ty(vec_u8, e)
    }

    /// Re-records an array/tuple literal argument's node type to match
    /// the callee parameter type. Array literals are otherwise typed
    /// as a fixed `[T; N]`; when the parameter wants a growable `[T]`
    /// (Vec) — including nested inside a tuple inside a Vec, as in
    /// `archive::tar::write([(String, [u8])])` — the literal must be
    /// recorded as the Vec so the compiled tier heap-allocates it at
    /// every level instead of laying an inline array into the tuple.
    fn recoerce_arg_literal(&mut self, expr: &Expr, expected: Ty) {
        let expected = self.infer.resolve(self.tcx, expected);
        let expected = match self.tcx.kind(expected) {
            Some(TyKind::Ref { inner, .. }) => *inner,
            _ => expected,
        };
        match &expr.kind {
            ExprKind::Array(ArrayExpr::List(elems)) => {
                if let Some(TyKind::Vec(e) | TyKind::Slice(e)) = self.tcx.kind(expected).cloned() {
                    self.record(expr.id, expected);
                    for el in elems {
                        self.recoerce_arg_literal(el, e);
                    }
                }
            }
            ExprKind::Array(ArrayExpr::Repeat { value, .. }) => {
                if let Some(TyKind::Vec(e) | TyKind::Slice(e)) = self.tcx.kind(expected).cloned() {
                    self.record(expr.id, expected);
                    self.recoerce_arg_literal(value, e);
                }
            }
            ExprKind::Tuple(elems) => {
                if let Some(TyKind::Tuple(tys)) = self.tcx.kind(expected).cloned() {
                    if tys.len() == elems.len() {
                        self.record(expr.id, expected);
                        for (el, t) in elems.iter().zip(tys) {
                            self.recoerce_arg_literal(el, t);
                        }
                    }
                }
            }
            // Push the expected type through value-producing positions so
            // a literal in a block tail / branch / arm is re-recorded too:
            // `fn f() -> Vec<T> { [..] }`,
            // `let v: Vec<T> = if c { [..] } else { [..] }`. The wrapping
            // node is re-recorded as well — codegen sizes the block/if/
            // match result slot from its node type, so leaving it `[T; N]`
            // while the branches build a heap Vec would desync the slot.
            ExprKind::Block(block) | ExprKind::Unsafe(block) => {
                if let Some(tail) = &block.tail {
                    self.record(expr.id, expected);
                    self.recoerce_arg_literal(tail, expected);
                }
            }
            ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.record(expr.id, expected);
                self.recoerce_arg_literal(then_branch, expected);
                if let Some(else_branch) = else_branch {
                    self.recoerce_arg_literal(else_branch, expected);
                }
            }
            ExprKind::Match { arms, .. } => {
                self.record(expr.id, expected);
                for arm in arms {
                    self.recoerce_arg_literal(&arm.body, expected);
                }
            }
            _ => {}
        }
    }

    fn option_adt_ty(&mut self, payload: Ty) -> Ty {
        let substs = crate::Substs::from_types([payload]);
        let def = gossamer_resolve::DefId::local(u32::MAX - 1);
        self.tcx.register_def_name(def, "Option");
        self.tcx.intern(TyKind::Adt { def, substs })
    }

    fn weak_adt_ty(&mut self, payload: Ty) -> Ty {
        let substs = crate::Substs::from_types([payload]);
        let def = gossamer_resolve::DefId::local(u32::MAX - 6);
        self.tcx.register_def_name(def, "Weak");
        self.tcx.intern(TyKind::Adt { def, substs })
    }

    fn check_method_call(&mut self, receiver: &Expr, args: &[Expr]) -> Ty {
        self.check_expr(receiver);
        for arg in args {
            self.check_expr(arg);
        }
        self.fresh()
    }

    fn unwrap_result_like(&mut self, ty: Ty) -> Option<Ty> {
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind(resolved) {
            Some(TyKind::Ref { inner, .. }) => self.unwrap_result_like(*inner),
            Some(TyKind::Adt { def, substs })
                if def.local == u32::MAX || def.local == u32::MAX - 1 =>
            {
                substs.types().first().copied()
            }
            _ => None,
        }
    }

    fn result_adt_ty(&mut self, ok: Ty, err: Ty) -> Ty {
        let substs = crate::Substs::from_types([ok, err]);
        let def = gossamer_resolve::DefId::local(u32::MAX);
        self.tcx.register_def_name(def, "Result");
        self.tcx.intern(TyKind::Adt { def, substs })
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
            UnaryOp::Deref => {
                // `*x` strips a single `&T` / `&mut T` wrapper.
                // For any other concrete operand shape the deref is
                // an identity (matches the interp's behaviour on
                // for-loop bound elements where the iterator hands
                // back values rather than references). Without
                // either pinning, downstream `println!("{}", *x)`
                // dispatches via `TyKind::Var → StrPtr` and tries
                // to dereference the value as a pointer — segv.
                let resolved = self.infer.resolve(self.tcx, operand_ty);
                match self.tcx.kind(resolved) {
                    Some(TyKind::Ref { inner, .. }) => *inner,
                    _ => operand_ty,
                }
            }
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
            BinaryOp::PipeGt => self.pipe_result_ty(lhs_ty, lhs.span, rhs, rhs_ty),
            _ => {
                self.unify(lhs_ty, rhs_ty, span);
                lhs_ty
            }
        }
    }

    /// Returns the result type of a `lhs |> rhs` pipe expression.
    ///
    /// `|>` desugars to `rhs(lhs)` (or `rhs(partial_args…, lhs)` for
    /// partial-application RHS). The expression type is the callee's
    /// return type, not the callee's function type. Unifies `lhs_ty`
    /// with the callee's last parameter so that un-annotated closure
    /// params (`|x| x + 1`) are pinned from the piped value's type.
    fn pipe_result_ty(&mut self, lhs_ty: Ty, lhs_span: Span, rhs: &Expr, rhs_ty: Ty) -> Ty {
        // Try to extract the callee's return type from rhs_ty first.
        let resolved = self.infer.resolve(self.tcx, rhs_ty);
        match self.tcx.kind_of(resolved).clone() {
            TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => {
                if let Some(&last) = sig.inputs.last() {
                    self.unify(lhs_ty, last, lhs_span);
                }
                return self.infer.resolve(self.tcx, sig.output);
            }
            TyKind::FnDef { def, .. } => {
                if let Some(sig) = self.fn_sigs.get(&def).cloned() {
                    if let Some(&last) = sig.inputs.last() {
                        self.unify(lhs_ty, last, lhs_span);
                    }
                    return sig.output;
                }
            }
            _ => {}
        }
        // rhs_ty might be an unresolved Var when `rhs` is a partial-application
        // Call (e.g. `add(1)` from `x |> add(1)` where check_call's arity guard
        // fired). Recover by inspecting the call's inner callee type directly.
        if let ExprKind::Call {
            callee: inner_callee,
            ..
        } = &rhs.kind
        {
            let inner_ty = self.table.get(inner_callee.id).unwrap_or(rhs_ty);
            let resolved_inner = self.infer.resolve(self.tcx, inner_ty);
            match self.tcx.kind_of(resolved_inner).clone() {
                TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => {
                    return self.infer.resolve(self.tcx, sig.output);
                }
                TyKind::FnDef { def, .. } => {
                    if let Some(sig) = self.fn_sigs.get(&def).cloned() {
                        return sig.output;
                    }
                }
                _ => {}
            }
        }
        rhs_ty
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
            let joined = self.join_branch_tys(then_ty, else_ty, else_branch.span);
            // When the branches joined to a Vec/slice, re-record each
            // array-literal branch to that shape so an unannotated
            // `let v = if c { [1, 2] } else { [3, 4, 5] }` lowers both
            // arms as a heap Vec, matching the joined result slot.
            self.recoerce_arg_literal(then_branch, joined);
            self.recoerce_arg_literal(else_branch, joined);
            joined
        } else {
            self.tcx.unit()
        }
    }

    /// Joins two branch result types (if/else, match arms). Two array
    /// literals of differing length — or an array and a Vec/slice — join
    /// to a growable `Vec<T>`, the only type that holds both, so
    /// `if c { ["a", "b"] } else { ["c"] }` is a `Vec<String>` rather than
    /// a length mismatch. Equal-length arrays and every other type unify
    /// normally (a same-length pair stays a fixed `[T; N]`, coercible to a
    /// Vec later if the surrounding context wants one).
    fn join_branch_tys(&mut self, a: Ty, b: Ty, span: Span) -> Ty {
        let ra = self.infer.resolve(self.tcx, a);
        let rb = self.infer.resolve(self.tcx, b);
        let elem_of = |k: &TyKind| match k {
            TyKind::Array { elem, len } => Some((*elem, Some(*len))),
            TyKind::Vec(elem) | TyKind::Slice(elem) => Some((*elem, None)),
            _ => None,
        };
        if let (Some(ka), Some(kb)) = (self.tcx.kind(ra).cloned(), self.tcx.kind(rb).cloned()) {
            if let (Some((ea, la)), Some((eb, lb))) = (elem_of(&ka), elem_of(&kb)) {
                self.unify(ea, eb, span);
                if la.is_none() || lb.is_none() || la != lb {
                    return self.tcx.intern(TyKind::Vec(ea));
                }
                self.unify(a, b, span);
                return a;
            }
        }
        self.unify(a, b, span);
        a
    }

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm]) -> Ty {
        let scrut_ty = self.check_expr(scrutinee);
        let mut result_ty = self.fresh();
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
            result_ty = self.join_branch_tys(result_ty, body_ty, arm.body.span);
            self.pop_scope();
        }
        // Second pass: if the arms joined to a Vec/slice, re-record every
        // array-literal arm body to that shape so an unannotated
        // `let v = match n { 0 => ["a"], _ => ["b", "c"] }` lowers each
        // arm as a heap Vec, matching the joined result slot.
        for arm in arms {
            self.recoerce_arg_literal(&arm.body, result_ty);
        }
        result_ty
    }

    /// Type-checks a `select { … }` expression. Each arm is checked in its own
    /// scope: a recv arm binds its pattern to the channel's element type, a
    /// send arm unifies the sent value against the channel's element type, and
    /// every arm body unifies into the shared result type.
    fn check_select(&mut self, arms: &[gossamer_ast::SelectArm]) -> Ty {
        use gossamer_ast::SelectOp;
        let result_ty = self.fresh();
        for arm in arms {
            self.push_scope();
            match &arm.op {
                SelectOp::Recv { pattern, channel } => {
                    let chan_ty = self.check_expr(channel);
                    let resolved = self.infer.resolve(self.tcx, chan_ty);
                    let elem = match self.tcx.kind_of(resolved).clone() {
                        TyKind::Receiver(inner) | TyKind::Sender(inner) => inner,
                        _ => self.fresh(),
                    };
                    let pat_ty = self.type_of_pattern(pattern);
                    self.unify(pat_ty, elem, pattern.span);
                    self.bind_pattern(pattern, elem);
                }
                SelectOp::Send { channel, value } => {
                    let chan_ty = self.check_expr(channel);
                    let resolved = self.infer.resolve(self.tcx, chan_ty);
                    let elem = match self.tcx.kind_of(resolved).clone() {
                        TyKind::Sender(inner) | TyKind::Receiver(inner) => inner,
                        _ => self.fresh(),
                    };
                    let val_ty = self.check_expr(value);
                    self.unify(elem, val_ty, value.span);
                }
                SelectOp::Default => {}
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
        // integer type. When the iterator is itself a method call
        // (`xs.iter()`, `xs.into_iter()`) whose return type is an
        // unresolved inference variable, fall back to looking at
        // the method's receiver — `.iter()` and friends always
        // produce the receiver's element type, regardless of which
        // wrapper they technically return.
        let derived = {
            let starting = self.infer.resolve(self.tcx, iter_ty);
            let is_var = matches!(self.tcx.kind(starting), Some(TyKind::Var(_)));
            let starting = if is_var {
                if let ExprKind::MethodCall { receiver, .. } = &iter.kind {
                    let recv_ty = self.check_expr(receiver);
                    self.infer.resolve(self.tcx, recv_ty)
                } else {
                    starting
                }
            } else {
                starting
            };
            let mut cur = starting;
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
        let mut diverged = false;
        for stmt in &block.stmts {
            self.check_stmt(stmt);
            if !diverged && stmt_diverges(stmt) {
                diverged = true;
            }
        }
        let ty = if let Some(tail) = &block.tail {
            self.check_expr(tail)
        } else if diverged {
            // A block whose statements unconditionally diverge
            // (`return`, `break`, `continue`, `panic!`) and whose
            // tail is missing has type `!`. Without this, a match
            // arm body like `{ eprintln!(...); return Err(msg) }`
            // would be typed as `unit` and force the match's
            // result type away from the other arms' real type.
            self.tcx.never()
        } else {
            self.tcx.unit()
        };
        self.pop_scope();
        ty
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match &stmt.kind {
            StmtKind::Let { pattern, ty, init } => {
                let forced = self.write_arg_bindings.get(&pattern.id).copied();
                let binding_ty = match ty {
                    Some(ty) => self.type_from_ast(ty),
                    None => forced.unwrap_or_else(|| self.fresh()),
                };
                if let Some(init) = init {
                    let init_ty = self.check_expr(init);
                    self.unify(binding_ty, init_ty, init.span);
                    // Re-record an array/tuple literal initializer to the
                    // binding's declared shape: an explicit annotation
                    // (`let x: Vec<String> = ["a", "b"]`) makes the literal
                    // a growable Vec/slice rather than a fixed `[T; N]`,
                    // and a `forced` write-arg binding flows the parameter
                    // type in (see `collect_write_arg_bindings`).
                    if ty.is_some() {
                        self.recoerce_arg_literal(init, binding_ty);
                    } else if let Some(forced) = forced {
                        self.recoerce_arg_literal(init, forced);
                    }
                }
                self.bind_pattern(pattern, binding_ty);
            }
            StmtKind::Expr { expr, .. } => {
                let expr_ty = self.check_expr(expr);
                // N6 / SPEC §9: a `Result<T,E>` value used as a statement
                // (value discarded) is a compile error. The explicit discard
                // form `let _ = expr` goes through `StmtKind::Let` and is
                // not subject to this check.
                let resolved = self.infer.resolve(self.tcx, expr_ty);
                if let Some(TyKind::Adt { def, .. }) = self.tcx.kind(resolved) {
                    if self.tcx.def_name(*def) == Some("Result") {
                        self.emit(TypeError::DiscardedResult, expr.span);
                    }
                }
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
        if ret.is_some() {
            self.recoerce_arg_literal(body, output);
        }
        self.pop_scope();
        self.tcx.intern(TyKind::FnPtr(FnSig { inputs, output }))
    }

    fn check_array(&mut self, arr: &ArrayExpr) -> Ty {
        match arr {
            ArrayExpr::List(elems) => {
                let mut elem_ty = if let Some(first) = elems.first() {
                    self.check_expr(first)
                } else {
                    self.fresh()
                };
                // Join element types rather than a plain unify so a nested
                // literal whose inner arrays differ in length —
                // `[["a", "b"], ["c"]]` — settles on `Vec<String>` instead
                // of failing `[String; 2]` vs `[String; 1]`.
                for elem in elems.iter().skip(1) {
                    let ty = self.check_expr(elem);
                    elem_ty = self.join_branch_tys(elem_ty, ty, elem.span);
                }
                // If the elements joined to a growable Vec/slice, re-record
                // each one to that shape so every inner array literal lowers
                // as a heap Vec, matching the outer element slot.
                let resolved_elem = self.infer.resolve(self.tcx, elem_ty);
                if matches!(
                    self.tcx.kind(resolved_elem),
                    Some(TyKind::Vec(_) | TyKind::Slice(_))
                ) {
                    for elem in elems {
                        self.recoerce_arg_literal(elem, elem_ty);
                    }
                }
                self.tcx.intern(TyKind::Array {
                    elem: elem_ty,
                    len: elems.len(),
                })
            }
            ArrayExpr::Repeat { value, count } => {
                let elem_ty = self.check_expr(value);
                self.check_expr(count);
                let len = self.evaluate_array_len(count).unwrap_or(0);
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
                gossamer_resolve::DefKind::Const => self
                    .const_tys
                    .get(&def)
                    .copied()
                    .unwrap_or_else(|| self.fresh()),
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

    fn type_of_literal(&mut self, lit: &Literal, span: Span) -> Ty {
        match lit {
            Literal::Int(text) => self.type_of_int_literal(text, span),
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

    fn type_of_int_literal(&mut self, text: &str, span: Span) -> Ty {
        for (suffix, int_ty) in INT_SUFFIXES {
            if text.ends_with(suffix) {
                if !int_literal_fits(text, *int_ty) {
                    self.emit(
                        TypeError::IntLiteralOverflow {
                            literal: text.to_string(),
                            ty: (*suffix).to_string(),
                        },
                        span,
                    );
                    return self.tcx.error_ty();
                }
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
        // typechecking. Validate magnitude against the widest
        // integer bucket the language exposes (`u128`/`i128`),
        // not against `i64` alone — `let x: u64 = u64::MAX` is
        // a legitimate program; the use-site unification will
        // either succeed (assign to u64) or fail with a normal
        // type-mismatch diagnostic. Only literals whose
        // magnitude is genuinely impossible to represent in any
        // Gossamer integer type get the GT0009 here.
        if let Some(magnitude) = parse_int_magnitude(text)
            && magnitude > u128::from(u64::MAX)
        {
            self.emit(
                TypeError::IntLiteralOverflow {
                    literal: text.to_string(),
                    ty: "any integer type".to_string(),
                },
                span,
            );
            return self.tcx.error_ty();
        }
        self.infer.fresh_int_var(self.tcx)
    }

    fn type_of_float_literal(&mut self, text: &str) -> Ty {
        for (suffix, float_ty) in FLOAT_SUFFIXES {
            if text.ends_with(suffix) {
                return self.tcx.float_ty(*float_ty);
            }
        }
        // Unsuffixed float literal: a float-defaulting inference var.
        // Takes its use-site float width when constrained, falls back
        // to `f64` otherwise (see `default_unresolved_float_vars`).
        self.infer.fresh_float_var(self.tcx)
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
                let count = self.evaluate_array_len(len).unwrap_or(0);
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
        // Recognise the stdlib's opaque dynamic JSON value by
        // surface name. The resolver doesn't allocate a `DefId`
        // for it (it comes in via `use std::encoding::json` as a
        // bare import), so we'd otherwise fall through to a fresh
        // inference variable and lose the receiver-shape signal
        // that downstream MIR needs to route field access through
        // the json runtime helpers.
        if path_matches_json_value(path) {
            return self.tcx.json_value_ty();
        }
        if path_matches_dyn_error(path) {
            return self.tcx.dyn_error_ty();
        }
        if let Some(resolution) = self.resolutions.get(node) {
            match resolution {
                Resolution::Primitive(prim) => return self.type_from_primitive(prim),
                Resolution::Def { def, kind } => {
                    // A path resolving to a generic type parameter (`fn
                    // f<T>(x: T)` or `struct Pair<A, B> { fst: A }`)
                    // must surface as `TyKind::Param`, not as an `Adt`
                    // whose `def` happens to point at the parameter's
                    // binding. Without this branch, the `A` in `Pair<A>`
                    // unifies as an opaque ADT and any concrete struct
                    // literal hits a `type mismatch: expected adt#N`
                    // error rather than driving inference of A from
                    // the field-value type.
                    //
                    // Use the resolver's per-resolution kind rather
                    // than `resolutions.kind_of(def)` because the
                    // resolver records the DefKind on the
                    // resolution itself but not always on the
                    // separate def→kind map (`bind_generics` only
                    // inserts into the scope, not into the global
                    // map).
                    if kind == gossamer_resolve::DefKind::TypeParam {
                        if let Some((idx, name)) =
                            self.current_generic_scope.get(head_name).cloned()
                        {
                            return self.tcx.intern(TyKind::Param { idx, name });
                        }
                        return self.fresh();
                    }
                    let substs = self.substs_from_ast(path);
                    return self.tcx.intern(TyKind::Adt { def, substs });
                }
                Resolution::Import { .. } | Resolution::Err | Resolution::Local(_) => {}
            }
        }
        // Fallback for built-in generic enums the resolver doesn't
        // hand out a DefId for (`Result<T, E>`, `Option<T>`). Without
        // this, an annotation like `let r: Result<i64, String> = ...`
        // falls through to a fresh inference variable, losing the
        // substs the variant-binding fixup later needs to re-pin
        // `x` in `Ok(x) => …` to the actual payload type. The
        // sentinel `DefId`s use `u32::MAX` / `u32::MAX-1` so they
        // never collide with anything the resolver emits.
        match head_name {
            "Result" => {
                let mut substs = self.substs_from_ast(path);
                // `Result<T>` with a single arg is shorthand for
                // `Result<T, errors::Error>`, matching Rust's
                // `anyhow::Result<T>` convention.
                if substs.types().len() == 1 {
                    let e = self.tcx.dyn_error_ty();
                    substs = crate::Substs::from_types([substs.types()[0], e]);
                }
                let def = gossamer_resolve::DefId::local(u32::MAX);
                self.tcx.register_def_name(def, "Result");
                return self.tcx.intern(TyKind::Adt { def, substs });
            }
            "Option" => {
                let substs = self.substs_from_ast(path);
                let def = gossamer_resolve::DefId::local(u32::MAX - 1);
                self.tcx.register_def_name(def, "Option");
                return self.tcx.intern(TyKind::Adt { def, substs });
            }
            "Vec" => {
                let substs = self.substs_from_ast(path);
                let elem = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::Vec(elem));
            }
            "HashMap" => {
                let substs = self.substs_from_ast(path);
                let tys = substs.types();
                let key = tys.first().copied().unwrap_or_else(|| self.fresh());
                let value = tys.get(1).copied().unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::HashMap { key, value });
            }
            // `Box<T>` / `Arc<T>` / `Rc<T>` are transparent in a fully
            // GC'd language — every value is heap-shared already, so
            // these wrappers carry no runtime distinction. Keep the
            // surface accepting the spelling (Rust users expect to be
            // able to write `Box<List>` for a recursive enum payload)
            // by unwrapping to the inner type at type-check time.
            "Box" | "Arc" | "Rc" => {
                let substs = self.substs_from_ast(path);
                let tys = substs.types();
                if let Some(inner) = tys.first().copied() {
                    return inner;
                }
                return self.fresh();
            }
            // `Weak<T>` — a non-owning reference into an RC allocation.
            // Unlike `Box`/`Arc`/`Rc` it is NOT transparent: it carries
            // its own sentinel ADT so the drop pass releases it via the
            // weak helpers and `upgrade()` can produce an `Option<T>`.
            "Weak" => {
                let substs = self.substs_from_ast(path);
                let payload = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                return self.weak_adt_ty(payload);
            }
            _ => {}
        }
        // Recognise stdlib struct types by their last path segment
        // so parameter annotations like `entry: &fs::DirInfo` resolve
        // to the sentinel Adt rather than a fresh inference variable.
        // Without this, the MIR can't recover "DirInfo" from the
        // parameter's type and field access (`entry.is_symlink`) falls
        // through to gos_rt_json_get instead of a Field(idx) projection.
        let tail = path.segments.last().map_or("", |s| s.name.name.as_str());
        let stdlib_def_offset: Option<u32> = match tail {
            "DirInfo" => Some(2),
            "Output" => Some(3),
            "ResponseStream" => Some(4),
            "Response" => Some(5),
            _ => None,
        };
        if let Some(off) = stdlib_def_offset {
            let def = gossamer_resolve::DefId::local(u32::MAX - off);
            return self.tcx.intern(TyKind::Adt {
                def,
                substs: crate::Substs::new(),
            });
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
                        let value = self.evaluate_generic_const_arg(expr);
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

    /// Evaluates an array-length expression to a `usize`, emitting a
    /// diagnostic when the literal magnitude exceeds `usize::MAX`.
    /// Returns `None` for non-literal forms (the caller falls back to
    /// `0`, matching the historical lenient behaviour).
    fn evaluate_array_len(&mut self, expr: &Expr) -> Option<usize> {
        let raw = evaluate_const_int_from_expr(expr)?;
        if raw > usize::MAX as u128 {
            self.emit(
                TypeError::IntLiteralOverflow {
                    literal: format!("{raw}"),
                    ty: "usize".to_string(),
                },
                expr.span,
            );
            return None;
        }
        Some(raw as usize)
    }

    /// Evaluates a `const` generic argument to an `i128`, emitting a
    /// diagnostic when the literal magnitude does not fit. Returns
    /// `0` on overflow so the surrounding `Substs` stays well-formed.
    fn evaluate_generic_const_arg(&mut self, expr: &Expr) -> i128 {
        let Some(raw) = evaluate_const_int_from_expr(expr) else {
            return 0;
        };
        if let Ok(value) = i128::try_from(raw) {
            value
        } else {
            self.emit(
                TypeError::IntLiteralOverflow {
                    literal: format!("{raw}"),
                    ty: "i128".to_string(),
                },
                expr.span,
            );
            0
        }
    }

    fn type_of_pattern(&mut self, pattern: &Pattern) -> Ty {
        if self.enter_recursion(pattern.span).is_err() {
            return self.tcx.error_ty();
        }
        let ty = self.type_of_pattern_kind(pattern);
        self.leave_recursion();
        ty
    }

    fn type_of_pattern_kind(&mut self, pattern: &Pattern) -> Ty {
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Ident { .. }
            | PatternKind::Path(_)
            | PatternKind::Struct { .. }
            | PatternKind::TupleStruct { .. }
            | PatternKind::Rest => self.fresh(),
            PatternKind::Error => self.tcx.error_ty(),
            PatternKind::Literal(lit) => self.type_of_literal(lit, pattern.span),
            PatternKind::Tuple(parts) => {
                let tys: Vec<Ty> = parts.iter().map(|p| self.type_of_pattern(p)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            PatternKind::Range { lo, .. } => self.type_of_literal(lo, pattern.span),
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
            PatternKind::TupleStruct { path, elems } => {
                // For built-in generic enums (`Option<T>`,
                // `Result<T, E>`) pull the payload type out of the
                // scrutinee's substs and bind it to the matching
                // pattern element. Without this, `Some(x)` would
                // bind `x` to a fresh inference variable that no
                // later use forces back to `T`, leaving downstream
                // code looking at an unresolved `Var`. Falls back
                // to a fresh var for any other variant constructor.
                let payload_tys = self.payload_types_for_variant(path, ty);
                for (i, elem) in elems.iter().enumerate() {
                    let elem_ty = payload_tys
                        .as_ref()
                        .and_then(|tys| tys.get(i).copied())
                        .unwrap_or_else(|| self.fresh());
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
            | PatternKind::Rest
            | PatternKind::Error => {}
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

    /// Returns the payload tuple element types for a tuple-struct
    /// pattern when the scrutinee is `Option<T>` or `Result<T, E>`.
    /// Returns `None` for any other shape (user enums, unknown
    /// substs); callers fall back to fresh inference variables.
    fn payload_types_for_variant(
        &self,
        path: &gossamer_ast::Path,
        scrutinee_ty: Ty,
    ) -> Option<Vec<Ty>> {
        let resolved = self.infer.resolve(self.tcx, scrutinee_ty);
        let resolved = match self.tcx.kind(resolved)? {
            TyKind::Ref { inner, .. } => *inner,
            _ => resolved,
        };
        let TyKind::Adt { substs, .. } = self.tcx.kind(resolved)? else {
            return None;
        };
        let last = path.segments.last()?.name.name.as_str();
        let args: Vec<Ty> = substs.types();
        match (last, args.as_slice()) {
            ("Some", [t]) => Some(vec![*t]),
            ("Ok", [t, _]) => Some(vec![*t]),
            ("Err", [_, e]) => Some(vec![*e]),
            _ => None,
        }
    }
}

/// Pre-registers field types for stdlib structs that user source can
/// name (e.g. `fs::DirInfo`, `os::Output`, `http::Response`,
/// `http::ResponseStream`). The MIR-side dispatch pins free-call
/// destinations to sentinel `DefId`s (`u32::MAX - N`) for these
/// structs; without their field types registered here, `entry.path`
/// / `r.status` projections leave the result `Var(_)` and downstream
/// `.len()` / println fall back to the wrong dispatch.
fn register_stdlib_struct_fields(tcx: &mut TyCtxt) {
    let str_ty = tcx.string_ty();
    let i64_ty = tcx.int_ty(IntTy::I64);
    let bool_ty = tcx.bool_ty();
    // DirInfo: [name, path, is_file, is_dir, is_symlink, size, modified_ms]
    tcx.register_struct_fields(
        gossamer_resolve::DefId::local(u32::MAX - 2),
        vec![str_ty, str_ty, bool_ty, bool_ty, bool_ty, i64_ty, i64_ty],
    );
    // Output: [stdout, stderr, code]
    tcx.register_struct_fields(
        gossamer_resolve::DefId::local(u32::MAX - 3),
        vec![str_ty, str_ty, i64_ty],
    );
    // ResponseStream: [__handle, status, content_type]
    tcx.register_struct_fields(
        gossamer_resolve::DefId::local(u32::MAX - 4),
        vec![i64_ty, i64_ty, str_ty],
    );
    // Response: [status, body, raw_bytes, content_type, location].
    // raw_bytes is Vec<u8>; the per-name `gos_rt_http_response_*`
    // helpers handle the actual dispatch, so the field-list ordering
    // matters only for source-name lookup.
    let u8_ty = tcx.int_ty(IntTy::U8);
    let vec_u8 = tcx.intern(TyKind::Vec(u8_ty));
    tcx.register_struct_fields(
        gossamer_resolve::DefId::local(u32::MAX - 5),
        vec![i64_ty, str_ty, vec_u8, str_ty, str_ty],
    );
}

/// Seeds the checker's own `struct_fields` map with the same stdlib
/// struct entries that `register_stdlib_struct_fields` puts in `tcx`.
/// Without this, `lookup_field_ty_diagnosed` returns `UnknownField {
/// opaque: true }` for `entry: &fs::DirInfo` even though `tcx` knows
/// the field layout.
fn seed_checker_stdlib_struct_fields(
    tcx: &mut TyCtxt,
    map: &mut HashMap<gossamer_resolve::DefId, Vec<(String, Ty)>>,
) {
    let str_ty = tcx.string_ty();
    let i64_ty = tcx.int_ty(IntTy::I64);
    let u8_ty = tcx.int_ty(IntTy::U8);
    let vec_u8 = tcx.intern(TyKind::Vec(u8_ty));
    let bool_ty = tcx.bool_ty();
    let entries: &[(u32, &[(&str, Ty)])] = &[
        (
            2,
            &[
                ("name", str_ty),
                ("path", str_ty),
                ("is_file", bool_ty),
                ("is_dir", bool_ty),
                ("is_symlink", bool_ty),
                ("size", i64_ty),
                ("modified_ms", i64_ty),
            ],
        ),
        (
            3,
            &[("stdout", str_ty), ("stderr", str_ty), ("code", i64_ty)],
        ),
        (
            4,
            &[
                ("__handle", i64_ty),
                ("status", i64_ty),
                ("content_type", str_ty),
            ],
        ),
        (
            5,
            &[
                ("status", i64_ty),
                ("body", str_ty),
                ("raw_bytes", vec_u8),
                ("content_type", str_ty),
                ("location", str_ty),
            ],
        ),
    ];
    for (offset, fields) in entries {
        let def = gossamer_resolve::DefId::local(u32::MAX - offset);
        let list: Vec<(String, Ty)> = fields.iter().map(|(n, t)| ((*n).to_string(), *t)).collect();
        map.insert(def, list);
    }
}

fn evaluate_const_int_from_expr(expr: &Expr) -> Option<u128> {
    if let ExprKind::Literal(Literal::Int(text)) = &expr.kind {
        let cleaned = strip_int_suffix(text).replace('_', "");
        return parse_int(&cleaned);
    }
    None
}

/// Parses the magnitude of an integer literal as a `u128`. The leading
/// `-` is dropped before parsing; callers that care about signedness
/// must apply it externally. Returns `None` for non-parseable text.
fn parse_int_magnitude(text: &str) -> Option<u128> {
    let cleaned = strip_int_suffix(text).replace('_', "");
    let trimmed = cleaned.strip_prefix('-').unwrap_or(&cleaned);
    parse_int(trimmed)
}

/// Returns `true` when the unsigned magnitude of `text` fits in the
/// declared integer type. Treats a leading `-` as the literal's
/// negation, applying the signed-range bound from the negative side.
fn int_literal_fits(text: &str, ty: IntTy) -> bool {
    let Some(magnitude) = parse_int_magnitude(text) else {
        return true;
    };
    let negative = text.trim_start().starts_with('-');
    let (signed_max, signed_min_abs, unsigned_max) = int_bounds(ty);
    if let Some(unsigned_max) = unsigned_max {
        if negative {
            return false;
        }
        return magnitude <= unsigned_max;
    }
    let limit = if negative { signed_min_abs } else { signed_max };
    magnitude <= limit
}

/// Returns `(signed_max, signed_min_abs, unsigned_max)` for `ty`. The
/// unsigned slot is `None` for signed widths. The signed-minimum is
/// stored as a positive magnitude (`-i8::MIN` is reported as `128`).
fn int_bounds(ty: IntTy) -> (u128, u128, Option<u128>) {
    match ty {
        IntTy::I8 => (i8::MAX as u128, 1u128 << 7, None),
        IntTy::I16 => (i16::MAX as u128, 1u128 << 15, None),
        IntTy::I32 => (i32::MAX as u128, 1u128 << 31, None),
        IntTy::I64 => (i64::MAX as u128, 1u128 << 63, None),
        IntTy::I128 => (i128::MAX as u128, 1u128 << 127, None),
        IntTy::Isize => (i64::MAX as u128, 1u128 << 63, None),
        IntTy::U8 => (0, 0, Some(u128::from(u8::MAX))),
        IntTy::U16 => (0, 0, Some(u128::from(u16::MAX))),
        IntTy::U32 => (0, 0, Some(u128::from(u32::MAX))),
        IntTy::U64 => (0, 0, Some(u128::from(u64::MAX))),
        IntTy::U128 => (0, 0, Some(u128::MAX)),
        IntTy::Usize => (0, 0, Some(u128::from(u64::MAX))),
    }
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
        | TyKind::JsonValue
        | TyKind::DynError
        | TyKind::Param { .. } => true,
        TyKind::Tuple(parts) => parts.iter().all(|t| checker.is_concrete(*t)),
        TyKind::Array { elem, .. }
        | TyKind::Slice(elem)
        | TyKind::Vec(elem)
        | TyKind::Sender(elem)
        | TyKind::Receiver(elem)
        | TyKind::JoinHandle(elem)
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

/// Returns true when `path` names the stdlib `json::Value` type, in
/// any of the accepted spellings (`json::Value`,
/// `encoding::json::Value`, `std::encoding::json::Value`). The
/// resolver treats every prefix as a bare import binding so the
/// type checker has to recognise the surface syntax directly.
fn path_matches_json_value(path: &TypePath) -> bool {
    let names: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
    matches!(
        names.as_slice(),
        ["json", "Value"] | ["encoding", "json", "Value"] | ["std", "encoding", "json", "Value"]
    )
}

fn path_matches_dyn_error(path: &TypePath) -> bool {
    let names: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
    matches!(
        names.as_slice(),
        ["errors" | "error", "Error"] | ["std", "errors" | "error", "Error"]
    )
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

/// Returns `true` when `stmt` is a statement that always
/// diverges (transfers control out of the enclosing block via
/// `return`, `break`, `continue`, or a panicking call). Used by
/// `check_block` to give a tail-less, divergent block the type
/// `!` instead of `()`.
fn stmt_diverges(stmt: &Stmt) -> bool {
    match &stmt.kind {
        StmtKind::Expr { expr, .. } => expr_diverges(expr),
        StmtKind::Let {
            init: Some(init), ..
        } => expr_diverges(init),
        _ => false,
    }
}

fn expr_diverges(expr: &Expr) -> bool {
    matches!(
        expr.kind,
        ExprKind::Return(_) | ExprKind::Break { .. } | ExprKind::Continue { .. },
    )
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

/// Returns `true` when `name` is a trait the language ships
/// built-in (used for the `<T: Bound>` validation in
/// `register_fn_sig`). The list mirrors stdlib traits that
/// have no source-level declaration in the current file but
/// are part of the surface — keeps a `fn f<T: Iterator>(...)`
/// declaration in a file that itself does not declare
/// `trait Iterator` from raising a false unknown-trait
/// diagnostic.
fn known_builtin_trait(name: &str) -> bool {
    matches!(
        name,
        "Iterator"
            | "IntoIterator"
            | "FromIterator"
            | "Fn"
            | "FnMut"
            | "FnOnce"
            | "Clone"
            | "Copy"
            | "Debug"
            | "Display"
            | "Default"
            | "Hash"
            | "Hashable"
            | "PartialEq"
            | "Eq"
            | "PartialOrd"
            | "Ord"
            | "Sized"
            | "Send"
            | "Sync"
            | "Drop"
            | "From"
            | "Into"
            | "TryFrom"
            | "TryInto"
            | "Add"
            | "Sub"
            | "Mul"
            | "Div"
            | "Rem"
            | "Neg"
            | "Not"
            | "BitAnd"
            | "BitOr"
            | "BitXor"
            | "Shl"
            | "Shr"
            | "Index"
            | "IndexMut"
            | "AsRef"
            | "AsMut"
            | "Read"
            | "Write"
            | "Error"
            | "Future"
            | "Serialize"
            | "Deserialize"
    )
}

/// Collects the argument-path node of every `archive::{tar,zip}::write`
/// call in a function body so the checker can re-type a `let`-bound
/// literal that flows into one. Read-only walk; records node ids only.
struct WriteArgPathCollector {
    arg_paths: Vec<NodeId>,
}

impl gossamer_ast::visitor::Visitor for WriteArgPathCollector {
    fn visit_expr(&mut self, expr: &Expr) {
        if let ExprKind::Call { callee, args } = &expr.kind {
            if args.len() == 1 {
                if let ExprKind::Path(p) = &callee.kind {
                    let n = p.segments.len();
                    if n >= 2
                        && p.segments[n - 1].name.name.as_str() == "write"
                        && matches!(p.segments[n - 2].name.name.as_str(), "tar" | "zip")
                    {
                        self.arg_paths.push(args[0].id);
                    }
                }
            }
        }
        gossamer_ast::visitor::walk_expr(self, expr);
    }
}
