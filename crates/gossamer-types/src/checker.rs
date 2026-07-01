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
    checker.check_deferred_structural();
    checker.resolve_table();
    (checker.table, checker.diagnostics)
}

/// Hard limit on type-checker recursion depth. Mirrors the parser's
/// guard and keeps adversarial input that survives parsing from
/// blowing the C stack inside [`TypeChecker::check_expr`].
const RECURSION_LIMIT: u32 = 256;

/// Expected type pushed down into an expression while it is checked -
/// the "checking mode" of bidirectional typechecking. The expectation
/// decides structural questions unification cannot settle after the
/// fact (an array literal lowering as a fixed `[T; N]` versus a heap
/// `Vec<T>`), and it propagates through value-producing positions:
/// block tails, `if` / `match` branches, `&`-borrows, call and
/// constructor arguments.
#[derive(Clone, Copy, Debug)]
enum Expectation {
    /// No expectation: the expression synthesizes its type bottom-up.
    None,
    /// The expression must produce this type. Literal containers adopt
    /// the expected shape and their children are unified against the
    /// expected child types, so mismatches surface at the leaf span.
    HasType(Ty),
    /// Shape-only hint: literal containers adopt a matching expected
    /// shape but nothing is unified. Used where the expectation source
    /// is unreliable - name-global method signatures and variant
    /// constructors whose declared payload may belong to another type.
    Coerce(Ty),
}

impl Expectation {
    /// The expected type, if any.
    fn ty(self) -> Option<Ty> {
        match self {
            Expectation::None => None,
            Expectation::HasType(ty) | Expectation::Coerce(ty) => Some(ty),
        }
    }

    /// Re-wraps a child type at the same expectation strength, so a
    /// `Vec<T>` expectation hands its elements a `T` expectation of
    /// equal force.
    fn rewrap(self, ty: Ty) -> Expectation {
        match self {
            Expectation::None => Expectation::None,
            Expectation::HasType(_) => Expectation::HasType(ty),
            Expectation::Coerce(_) => Expectation::Coerce(ty),
        }
    }

    /// Whether the expectation participates in unification.
    fn unifies(self) -> bool {
        matches!(self, Expectation::HasType(_))
    }
}

/// A structural use (`value[i]` / `value(args)` / `value.N`) whose
/// operand type was still an unresolved inference variable when first
/// checked. Unsuffixed integer/float literals (`let x = 5`) only
/// default to a concrete scalar after the whole file is checked, so the
/// soundness check is deferred and re-run once defaulting has happened.
#[derive(Clone, Copy)]
enum DeferredStructuralKind {
    Index,
    Call,
    TupleField(u64),
}

struct DeferredStructural {
    ty: Ty,
    span: Span,
    kind: DeferredStructuralKind,
}

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
    /// `for x in self.items` loop binds `x` at the i64 default -
    /// printing a `[String]` field's element pointers as integers.
    current_self_ty: Option<Ty>,
    /// Generics of the `impl` block currently being checked, if any. An
    /// `impl<T> Wrapper<T>` brings `T` into scope for every method, so a
    /// method signature `(&self) -> T` records a rigid `Param(T)` (matching
    /// the struct's generic) rather than a fresh inference variable that
    /// never binds. Merged ahead of each method's own generics in
    /// [`Self::check_fn`].
    current_impl_generics: Option<gossamer_ast::Generics>,
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
    /// Non-receiver parameter types of user `impl` / trait methods,
    /// keyed by method name + arity. Every distinct signature is
    /// kept (method dispatch is name-global, so several types may
    /// share a name + arity); a literal argument is re-typed only
    /// when exactly one candidate expects a container shape at that
    /// position. Mirrors the free-fn re-typing against `fn_sigs`.
    method_arg_sigs: HashMap<(String, usize), Vec<Vec<Ty>>>,
    /// Declared return type of the function body currently being
    /// checked; drives literal re-typing at explicit `return`
    /// statements (the block-tail path is handled in `check_fn`).
    current_fn_ret: Option<Ty>,
    /// Per-enclosing-`loop` break-value type var plus whether any value
    /// break fired. A `loop` pushes `(fresh_var, false)`; each `break value`
    /// inside unifies its value with the var and sets the flag, so `let x =
    /// loop { break v }` infers `x` as `v`'s type. The flag distinguishes a
    /// value break whose type is still an unresolved (e.g. integer-literal)
    /// var from a loop with no value break at all - the former yields the
    /// var (it defaults later), the latter stays divergent (`never`).
    loop_break_tys: Vec<(Ty, bool)>,
    /// Declared return types of non-generic `impl` methods, keyed by
    /// `(self type name, method name, arity)`. When a method-call
    /// receiver resolves to that Adt, the call types as the declared
    /// return instead of a fresh inference var - without this,
    /// `sel.params()` reaches MIR untyped and the compiled tier
    /// guesses the element layout.
    method_ret_types: HashMap<(String, String, usize), Ty>,
    /// Declared argument arity (excluding `self`) of each non-generic
    /// user method, keyed by `(type_name, method_name)`. Drives the
    /// method-call arity check (GT0018): a call with the wrong count
    /// aborts on the VM and zero-fills/drops on the compiled tier, so it
    /// is rejected statically the same way free calls are.
    method_arities: HashMap<(String, String), usize>,
    /// Structural uses whose operand was an unresolved inference var at
    /// first check; re-validated after integer/float defaulting.
    deferred_structural: Vec<DeferredStructural>,
    /// Tuple-variant payload types keyed by `(enum_name,
    /// variant_name)`. Drives literal re-typing at variant
    /// constructor sites so `Value::Blob([1, 2, 3])` records a heap
    /// `[u8]`, not a fixed `[i64; 3]` whose first slot would pose as
    /// the payload word on the compiled tier.
    enum_variant_payloads: HashMap<(String, String), Vec<Ty>>,
    /// `Adt` type of every non-generic user enum, keyed by name. A
    /// tuple-variant constructor call (`E::B(1)`) resolves its result
    /// to this so the value carries the concrete enum type instead of a
    /// fresh inference variable - without it `E::B(1) < E::B(2)` leaves
    /// both operands unresolved and the comparison can't dispatch.
    enum_tys: HashMap<String, Ty>,
    /// Declared types for `const NAME: T = ...` items, keyed by
    /// `DefId`. Without this, a path expression that resolves to a
    /// const falls back to a fresh inference variable, leaving the
    /// use site unconstrained and the codegen reading the slot at
    /// the wrong layout.
    const_tys: HashMap<gossamer_resolve::DefId, Ty>,
    /// Type-parameter names and right-hand-side AST of every `type X<..> =
    /// T` alias, keyed by the alias's `DefId`. Built up front so a use of
    /// `X` expands to `T` lazily during type lowering (transparent
    /// aliases) - with the params substituted by the use-site arguments
    /// for a generic alias - rather than surfacing `X` as an opaque
    /// `adt#N`.
    alias_targets: HashMap<gossamer_resolve::DefId, (Vec<String>, gossamer_ast::Type)>,
    /// Alias `DefId`s currently being expanded, guarding against cyclic
    /// aliases (`type A = B; type B = A`).
    alias_expanding: std::collections::HashSet<gossamer_resolve::DefId>,
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
    /// Generic type-parameter count for every named function, keyed by
    /// `DefId`. Drives per-call-site instantiation: each call to a
    /// generic function gets one fresh inference variable per parameter,
    /// substituted into the signature before unifying with the arguments,
    /// so distinct call sites bind the parameters independently.
    fn_generic_arity: HashMap<gossamer_resolve::DefId, usize>,
    /// Declared trait bounds on each generic function's type parameters,
    /// keyed by `DefId`, outer index = parameter index, inner = bound
    /// trait names. After a call's parameters resolve to concrete types,
    /// each bound is checked against [`Self::trait_impl_types`].
    fn_param_bounds: HashMap<gossamer_resolve::DefId, Vec<Vec<String>>>,
    /// Per-position flags marking which of each generic function's
    /// parameters are const parameters, keyed by `DefId`. Lets a call
    /// site record a `GenericArg::Const` (rather than a type argument)
    /// at each const position when inferring the substitution.
    fn_generic_const_mask: HashMap<gossamer_resolve::DefId, Vec<bool>>,
    /// Set of concrete type names implementing each trait, keyed by trait
    /// name. Built from every `impl Trait for Type` block so a generic
    /// call can verify a `T: Trait` bound is satisfied by the argument.
    trait_impl_types: HashMap<String, std::collections::HashSet<String>>,
    /// Declared return type of each trait method, keyed by
    /// `(trait name, method name)`. Lets a `s.method()` call on a bound
    /// type parameter (`s: &T`, `T: Shape`) resolve to the method's real
    /// return type instead of the i64 default - so a `String`-returning
    /// trait method renders as text on the compiled tiers.
    trait_method_ret: HashMap<(String, String), Ty>,
    /// Per-parameter trait bounds of the function currently being checked,
    /// indexed by parameter position. Set on entry to a generic function
    /// body so a method call on a `Param` receiver can find its bounds.
    current_param_bounds: Vec<Vec<String>>,
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
    /// Currently-active const-generic-parameter name → `ParamIdx`
    /// mapping, populated alongside [`Self::current_generic_scope`].
    /// Lets a `[T; N]` array-length expression naming a const
    /// parameter type as a symbolic [`crate::ArrayLen::Param`] rather
    /// than collapsing to a concrete `0`.
    current_const_generic_scope: HashMap<String, crate::ParamIdx>,
    /// Trait names declared in this source file. Populated upfront
    /// by `collect_signatures` from every `ItemKind::Trait`. Used
    /// by `register_fn_sig` to validate that each `<T: Bound>`
    /// names a trait that actually exists - typos surface as a
    /// `GT0011 unknown-trait-bound` diagnostic at declaration time
    /// instead of as a runtime "no method" error later.
    declared_trait_names: std::collections::HashSet<String>,
    /// Local `let`-binding pattern nodes whose value flows into a
    /// stdlib `archive::{tar,zip}::write` call. A pre-scan of each
    /// function body fills this so the binding's literal initializer
    /// is re-typed to the `[(String, [u8])]` parameter - backward
    /// inference the single-pass checker can't otherwise reach, which
    /// the compiled tier needs so the nested byte arrays become heap
    /// Vecs instead of fixed inline arrays.
    write_arg_bindings: HashMap<NodeId, Ty>,
    /// Path-expression nodes sitting in a callee position (a call's
    /// callee, or the rhs of `|>`). A bare std-module path there is a
    /// normal stdlib call shape; everywhere else it is a std fn used
    /// as a VALUE, which is only legal for the
    /// [`crate::std_fn_values`] tabled set (GT0015 otherwise).
    callee_path_nodes: std::collections::HashSet<NodeId>,
    /// Names of every user-declared struct and enum in this source file.
    /// Distinguishes a genuine user Adt receiver (eligible for the
    /// name-global method-dispatch soundness check) from the sentinel
    /// Adts the checker synthesizes (`Result`, `Option`, `http::Response`,
    /// `VecDeque`).
    user_type_decls: std::collections::HashSet<String>,
    /// For every method name defined in a user `impl` block (inherent or
    /// trait impl) or declared on a trait, the set of user type names
    /// that own it. Lets [`Self::maybe_reject_unknown_adt_method`] reject
    /// `b.label()` when `label` belongs to a different type than `b`.
    user_method_owners: HashMap<String, std::collections::HashSet<String>>,
    /// Method names declared directly on each trait, keyed by trait name.
    /// Used with [`Self::trait_supertraits`] to detect a method reached
    /// only through a bound's supertrait (P0-5).
    trait_own_methods: HashMap<String, std::collections::HashSet<String>>,
    /// Supertrait names of each trait, keyed by trait name, from the
    /// `trait Pet: Animal` clause.
    trait_supertraits: HashMap<String, Vec<String>>,
    /// Callee nodes of a call sitting on the right of `|>`. The pipe
    /// desugars `x |> f(a)` to `f(a, x)` during HIR lowering, so such a
    /// call supplies one fewer explicit argument than the callee's
    /// arity; the arity check accounts for the implicit piped argument.
    pipe_stage_callees: std::collections::HashSet<NodeId>,
    /// Declared variant names of every user enum, keyed by enum name.
    /// Lets a `Enum::Variant` path reject an undeclared variant
    /// (`Shape::Triangle`) at check, instead of faulting at runtime.
    enum_variants: HashMap<String, std::collections::HashSet<String>>,
}

/// Saved generic-parameter scopes restored by
/// [`TypeChecker::leave_generic_scope`].
struct GenericScope {
    types: HashMap<String, (crate::ParamIdx, Box<str>)>,
    consts: HashMap<String, crate::ParamIdx>,
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
            current_impl_generics: None,
            recursion_depth: 0,
            recursion_limit_reported: false,
            struct_fields: checker_struct_fields,
            fn_sigs: HashMap::new(),
            method_arg_sigs: HashMap::new(),
            current_fn_ret: None,
            loop_break_tys: Vec::new(),
            method_ret_types: HashMap::new(),
            method_arities: HashMap::new(),
            deferred_structural: Vec::new(),
            enum_variant_payloads: HashMap::new(),
            enum_tys: HashMap::new(),
            const_tys: HashMap::new(),
            alias_targets: HashMap::new(),
            alias_expanding: std::collections::HashSet::new(),
            struct_generic_arity: HashMap::new(),
            fn_generic_arity: HashMap::new(),
            fn_param_bounds: HashMap::new(),
            fn_generic_const_mask: HashMap::new(),
            trait_impl_types: HashMap::new(),
            trait_method_ret: HashMap::new(),
            current_param_bounds: Vec::new(),
            current_generic_scope: HashMap::new(),
            current_const_generic_scope: HashMap::new(),
            declared_trait_names: std::collections::HashSet::new(),
            write_arg_bindings: HashMap::new(),
            callee_path_nodes: std::collections::HashSet::new(),
            user_type_decls: std::collections::HashSet::new(),
            user_method_owners: HashMap::new(),
            trait_own_methods: HashMap::new(),
            trait_supertraits: HashMap::new(),
            pipe_stage_callees: std::collections::HashSet::new(),
            enum_variants: HashMap::new(),
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
    /// Per-parameter list of bound trait names for a generics clause,
    /// indexed by full parameter position so const and lifetime slots
    /// keep their place (`<'a, T: A + B, const N: usize>` ->
    /// `[[], [A, B], []]`).
    fn type_param_bounds(generics: &gossamer_ast::Generics) -> Vec<Vec<String>> {
        generics
            .params
            .iter()
            .map(|p| match p {
                gossamer_ast::GenericParam::Type { bounds, .. } => bounds
                    .iter()
                    .filter_map(|b| b.path.segments.last())
                    .map(|s| s.name.name.clone())
                    .collect(),
                _ => Vec::new(),
            })
            .collect()
    }

    /// Per-parameter flags marking which generic positions are const
    /// parameters, indexed by full parameter position.
    fn const_param_mask(generics: &gossamer_ast::Generics) -> Vec<bool> {
        generics
            .params
            .iter()
            .map(|p| matches!(p, gossamer_ast::GenericParam::Const { .. }))
            .collect()
    }

    fn enter_generic_scope(&mut self, generics: &gossamer_ast::Generics) -> GenericScope {
        let prior_types = std::mem::take(&mut self.current_generic_scope);
        let prior_consts = std::mem::take(&mut self.current_const_generic_scope);
        for (i, param) in generics.params.iter().enumerate() {
            match param {
                gossamer_ast::GenericParam::Type { name, .. } => {
                    let owned: Box<str> = name.name.clone().into_boxed_str();
                    self.current_generic_scope
                        .insert(name.name.clone(), (crate::ParamIdx(i as u32), owned));
                }
                gossamer_ast::GenericParam::Const { name, .. } => {
                    self.current_const_generic_scope
                        .insert(name.name.clone(), crate::ParamIdx(i as u32));
                }
                gossamer_ast::GenericParam::Lifetime { .. } => {}
            }
        }
        GenericScope {
            types: prior_types,
            consts: prior_consts,
        }
    }

    /// Enters a generic scope combining an `impl` block's generics (first,
    /// so they keep the `Param` indices the struct's fields use) with a
    /// method's own generics (offset after them). Used when checking a
    /// method of `impl<T> Wrapper<T>`, so `-> T` records `Param(0)`
    /// matching `Wrapper`'s first generic, while the method's own `<U>`
    /// gets the next index.
    fn enter_generic_scope_combined(
        &mut self,
        outer: &gossamer_ast::Generics,
        inner: &gossamer_ast::Generics,
    ) -> GenericScope {
        let prior_types = std::mem::take(&mut self.current_generic_scope);
        let prior_consts = std::mem::take(&mut self.current_const_generic_scope);
        let mut idx = 0u32;
        for param in outer.params.iter().chain(inner.params.iter()) {
            match param {
                gossamer_ast::GenericParam::Type { name, .. } => {
                    let owned: Box<str> = name.name.clone().into_boxed_str();
                    self.current_generic_scope
                        .insert(name.name.clone(), (crate::ParamIdx(idx), owned));
                    idx += 1;
                }
                gossamer_ast::GenericParam::Const { name, .. } => {
                    self.current_const_generic_scope
                        .insert(name.name.clone(), crate::ParamIdx(idx));
                    idx += 1;
                }
                gossamer_ast::GenericParam::Lifetime { .. } => {}
            }
        }
        GenericScope {
            types: prior_types,
            consts: prior_consts,
        }
    }

    /// Restores a generic-parameter scope saved by
    /// [`Self::enter_generic_scope`].
    fn leave_generic_scope(&mut self, prior: GenericScope) {
        self.current_generic_scope = prior.types;
        self.current_const_generic_scope = prior.consts;
    }

    /// Verifies each instantiated type parameter of a generic call
    /// satisfies its declared trait bounds: the concrete type must carry
    /// an `impl Bound for Type` (or `Bound` is a recognised built-in
    /// trait). A still-unresolved parameter or a non-named type is left
    /// for argument unification to report; only a concrete type with a
    /// definitely-missing impl is flagged.
    fn check_trait_bounds(&mut self, def: gossamer_resolve::DefId, vars: &[Ty], span: Span) {
        let Some(bounds) = self.fn_param_bounds.get(&def).cloned() else {
            return;
        };
        for (i, var) in vars.iter().enumerate() {
            let resolved = self.infer.resolve(self.tcx, *var);
            let Some(ty_name) = self.concrete_type_name(resolved) else {
                continue;
            };
            for bound in bounds.get(i).into_iter().flatten() {
                if known_builtin_trait(bound) {
                    continue;
                }
                let satisfied = self
                    .trait_impl_types
                    .get(bound)
                    .is_some_and(|s| s.contains(&ty_name));
                if !satisfied {
                    self.emit(
                        TypeError::TraitBoundNotSatisfied {
                            ty: ty_name.clone(),
                            bound: bound.clone(),
                        },
                        span,
                    );
                }
            }
        }
    }

    /// Name of a concrete named type (`Dog`, `Cat`), peeling `&` / `&mut`.
    /// `None` for inference variables, primitives, and structural types so
    /// a bound check only fires on a definitely-named type.
    fn concrete_type_name(&self, ty: Ty) -> Option<String> {
        let mut t = ty;
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(t) {
            t = *inner;
        }
        match self.tcx.kind(t) {
            Some(TyKind::Adt { def, .. }) => self.tcx.def_name(*def).map(str::to_string),
            _ => None,
        }
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
        self.subst_generics_in_ty(ty, substs, &[])
    }

    /// Infers a const generic array length from a call argument: a
    /// parameter typed `[T; N]` (peeling references) matched against an
    /// argument of concrete length yields `(N's param index, length)`.
    fn infer_array_const_len(&self, param_ty: Ty, arg_ty: Ty) -> Option<(usize, i128)> {
        let mut p = param_ty;
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(p) {
            p = *inner;
        }
        let TyKind::Array {
            len: crate::ArrayLen::Param(idx),
            ..
        } = self.tcx.kind_of(p)
        else {
            return None;
        };
        let idx = idx.0 as usize;
        let mut a = arg_ty;
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(a) {
            a = *inner;
        }
        let TyKind::Array {
            len: crate::ArrayLen::Concrete(n),
            ..
        } = self.tcx.kind_of(a)
        else {
            return None;
        };
        Some((idx, *n as i128))
    }

    /// Like [`Self::subst_params_in_ty`] but also rewrites a const
    /// generic array length (`[T; N]` where `N` is the `idx`-th
    /// parameter) to a concrete `ArrayLen` when `const_substs[idx]`
    /// supplies a value. Used at generic call sites where the const
    /// argument is inferred from the array argument's length.
    #[allow(
        clippy::too_many_lines,
        reason = "type-constructor dispatch - arms map 1:1 to TyKind variants; splitting hides the type walk"
    )]
    fn subst_generics_in_ty(&mut self, ty: Ty, substs: &[Ty], const_substs: &[Option<i128>]) -> Ty {
        if substs.is_empty() && const_substs.iter().all(Option::is_none) {
            return ty;
        }
        let kind = self.tcx.kind_of(ty).clone();
        match kind {
            TyKind::Param { idx, .. } => substs.get(idx.0 as usize).copied().unwrap_or(ty),
            TyKind::Ref { inner, mutability } => {
                let new_inner = self.subst_generics_in_ty(inner, substs, const_substs);
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
                    .map(|e| self.subst_generics_in_ty(*e, substs, const_substs))
                    .collect();
                if new_elems == elems {
                    ty
                } else {
                    self.tcx.intern(TyKind::Tuple(new_elems))
                }
            }
            TyKind::Array { elem, len } => {
                let new_elem = self.subst_generics_in_ty(elem, substs, const_substs);
                let new_len = subst_array_len(len, const_substs);
                if new_elem == elem && new_len == len {
                    ty
                } else {
                    self.tcx.intern(TyKind::Array {
                        elem: new_elem,
                        len: new_len,
                    })
                }
            }
            TyKind::Slice(elem) => {
                let new = self.subst_generics_in_ty(elem, substs, const_substs);
                if new == elem {
                    ty
                } else {
                    self.tcx.intern(TyKind::Slice(new))
                }
            }
            TyKind::Vec(elem) => {
                let new = self.subst_generics_in_ty(elem, substs, const_substs);
                if new == elem {
                    ty
                } else {
                    self.tcx.intern(TyKind::Vec(new))
                }
            }
            TyKind::HashMap { key, value } => {
                let new_k = self.subst_generics_in_ty(key, substs, const_substs);
                let new_v = self.subst_generics_in_ty(value, substs, const_substs);
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
                let new = self.subst_generics_in_ty(inner, substs, const_substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::Sender(new))
                }
            }
            TyKind::Receiver(inner) => {
                let new = self.subst_generics_in_ty(inner, substs, const_substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::Receiver(new))
                }
            }
            TyKind::JoinHandle(inner) => {
                let new = self.subst_generics_in_ty(inner, substs, const_substs);
                if new == inner {
                    ty
                } else {
                    self.tcx.intern(TyKind::JoinHandle(new))
                }
            }
            // Adt / Alias carry their own generic-argument lists; a
            // function signature naming a generic struct (`w: Wrapper<T>`)
            // holds the function's rigid `Param` inside those args, so
            // substitute into them too. Without this, instantiating the
            // signature leaves `Wrapper<Param>` and unifying it against a
            // concrete `Wrapper<i64>` argument fails (rigid `Param` vs
            // `i64`).
            TyKind::Adt {
                def,
                substs: adt_substs,
            } => {
                let new_substs = self.subst_generics_in_substs(&adt_substs, substs, const_substs);
                if new_substs == adt_substs {
                    ty
                } else {
                    self.tcx.intern(TyKind::Adt {
                        def,
                        substs: new_substs,
                    })
                }
            }
            TyKind::Alias {
                def,
                substs: alias_substs,
            } => {
                let new_substs = self.subst_generics_in_substs(&alias_substs, substs, const_substs);
                if new_substs == alias_substs {
                    ty
                } else {
                    self.tcx.intern(TyKind::Alias {
                        def,
                        substs: new_substs,
                    })
                }
            }
            _ => ty,
        }
    }

    /// Substitutes the generic-parameter slots inside a `Substs`' type
    /// arguments, leaving const arguments untouched. Used by
    /// [`Self::subst_generics_in_ty`] to instantiate a generic struct named
    /// in a function signature (`Wrapper<T>`).
    fn subst_generics_in_substs(
        &mut self,
        args: &crate::Substs,
        substs: &[Ty],
        const_substs: &[Option<i128>],
    ) -> crate::Substs {
        let new_args: Vec<crate::GenericArg> = args
            .as_slice()
            .iter()
            .map(|a| match a {
                crate::GenericArg::Type(t) => {
                    crate::GenericArg::Type(self.subst_generics_in_ty(*t, substs, const_substs))
                }
                crate::GenericArg::Const(c) => crate::GenericArg::Const(*c),
            })
            .collect();
        crate::Substs::from_args(new_args)
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

    /// Resolves a type deeply - after shallow-resolving top-level `Var`
    /// nodes, recurses into `FnPtr` / `FnTrait` sigs so that compound
    /// types like `FnPtr(FnSig { output: Var(1) })` are fully grounded
    /// when the inference var was unified with a concrete type.
    /// Deep-resolves every type argument inside a `Substs`.
    fn deep_resolve_substs(&mut self, substs: &crate::Substs) -> crate::Substs {
        let new_args: Vec<crate::GenericArg> = substs
            .as_slice()
            .iter()
            .map(|arg| match arg {
                crate::GenericArg::Type(t) => crate::GenericArg::Type(self.deep_resolve(*t)),
                crate::GenericArg::Const(c) => crate::GenericArg::Const(*c),
            })
            .collect();
        crate::Substs::from_args(new_args)
    }

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
            // local defaults to i64/ptr - printing an `f64`'s bit
            // pattern or strlen'ing a non-pointer.
            TyKind::Adt { def, substs } => {
                let new_substs = self.deep_resolve_substs(&substs);
                if new_substs == substs {
                    resolved
                } else {
                    self.tcx.intern(TyKind::Adt {
                        def,
                        substs: new_substs,
                    })
                }
            }
            // A generic call's callee carries the per-call-site type
            // arguments as inference variables; resolve them so the MIR
            // monomorphiser sees the concrete instantiation (`fn<Dog>`)
            // rather than an unresolved variable.
            TyKind::FnDef { def, substs } => {
                let new_substs = self.deep_resolve_substs(&substs);
                if new_substs == substs {
                    resolved
                } else {
                    self.tcx.intern(TyKind::FnDef {
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
        // First pass: index every trait name + its methods + supertraits,
        // and every user struct / enum name, so subsequent passes can
        // validate `<T: Bound>` bounds, reject name-global method
        // mis-dispatch, and detect supertrait-through-bound calls
        // regardless of declaration order relative to impl blocks.
        self.collect_trait_names(items);
        // Register alias targets before any type lowering so a struct
        // field / let / param naming `X` (where `type X = T`) expands to
        // `T` regardless of declaration order.
        self.collect_type_aliases(items);
        for item in items {
            match &item.kind {
                ItemKind::Fn(decl) => self.register_fn_sig(item.id, decl, item.span),
                ItemKind::Impl(decl) => self.collect_impl_signatures(decl),
                ItemKind::Trait(decl) => self.collect_trait_signatures(decl),
                ItemKind::Struct(decl) => {
                    self.validate_derives(&item.attrs, item.span);
                    self.register_struct(item.id, decl);
                }
                ItemKind::Enum(decl) => {
                    self.validate_derives(&item.attrs, item.span);
                    self.register_enum(item.id, decl, item.span);
                }
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

    /// Walks `items` recursively into inline modules and records, for
    /// later passes: every trait name (for `<T: Bound>` validation),
    /// each trait's own method names and supertrait list (for the
    /// supertrait-through-bound check), and every user struct / enum
    /// name (to tell a real user Adt receiver from a synthesized
    /// sentinel one). Idempotent - re-calling adds to the existing sets.
    fn collect_trait_names(&mut self, items: &[Item]) {
        for item in items {
            match &item.kind {
                ItemKind::Trait(decl) => {
                    self.declared_trait_names.insert(decl.name.name.clone());
                    let methods: std::collections::HashSet<String> = decl
                        .items
                        .iter()
                        .filter_map(|it| match it {
                            TraitItem::Fn(fn_decl) => Some(fn_decl.name.name.clone()),
                            _ => None,
                        })
                        .collect();
                    self.trait_own_methods
                        .entry(decl.name.name.clone())
                        .or_default()
                        .extend(methods);
                    let supers: Vec<String> = decl
                        .supertraits
                        .iter()
                        .filter_map(|b| b.path.segments.last().map(|s| s.name.name.clone()))
                        .collect();
                    if !supers.is_empty() {
                        self.trait_supertraits
                            .insert(decl.name.name.clone(), supers);
                    }
                }
                ItemKind::Struct(decl) => {
                    self.user_type_decls.insert(decl.name.name.clone());
                }
                ItemKind::Enum(decl) => {
                    self.user_type_decls.insert(decl.name.name.clone());
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

    /// Records each `type X<..> = T` alias's type-parameter names and
    /// right-hand side by `DefId`, recursing into inline modules. A use of
    /// the alias expands to `T` (with the params substituted by the
    /// use-site arguments for a generic alias) during type lowering.
    fn collect_type_aliases(&mut self, items: &[Item]) {
        for item in items {
            match &item.kind {
                ItemKind::TypeAlias(decl) => {
                    if let Some(def) = self.resolutions.definition_of(item.id) {
                        let params: Vec<String> = decl
                            .generics
                            .params
                            .iter()
                            .filter_map(|p| match p {
                                gossamer_ast::GenericParam::Type { name, .. } => {
                                    Some(name.name.clone())
                                }
                                _ => None,
                            })
                            .collect();
                        self.alias_targets.insert(def, (params, decl.ty.clone()));
                    }
                }
                ItemKind::Mod(decl) => {
                    if let gossamer_ast::ModBody::Inline(inner) = &decl.body {
                        self.collect_type_aliases(inner);
                    }
                }
                _ => {}
            }
        }
    }

    /// Registers an enum's `DefId -> name` so `render_ty` / `adt_dispatch_name`
    /// recover "Shape" instead of the "adt#N" placeholder - needed for `==` /
    /// `{:?}` dispatch on enum values whose type resolves to the Adt.
    fn register_enum(&mut self, item_id: NodeId, decl: &gossamer_ast::EnumDecl, span: Span) {
        if let Some(def) = self.resolutions.definition_of(item_id) {
            self.tcx.register_def_name(def, decl.name.name.as_str());
            // Payload-bearing enums are reference-counted heap
            // values; register the def eagerly so the MIR drop pass
            // sees enum-typed locals as RC-managed in every body,
            // not just bodies lowered after the enum's first
            // constructor. All-unit enums lower as bare `i64`
            // discriminants and are excluded.
            let has_payload = decl.variants.iter().any(|v| match &v.body {
                StructBody::Tuple(fields) => !fields.is_empty(),
                StructBody::Named(fields) => !fields.is_empty(),
                StructBody::Unit => false,
            });
            if has_payload {
                self.tcx.register_rc_managed_enum_def(def.local);
            }
            // Cache the enum's `Adt` type so a tuple-variant constructor call
            // resolves to it. Generic enums need per-call-site substs, so they
            // keep the fresh-variable path.
            if decl.generics.params.is_empty() {
                let adt = self.tcx.intern(TyKind::Adt {
                    def,
                    substs: crate::Substs::from_types(std::iter::empty()),
                });
                self.enum_tys.insert(decl.name.name.clone(), adt);
                self.tcx
                    .register_enum_ty_by_name(decl.name.name.as_str(), adt);
            }
        }
        let variant_names = self
            .enum_variants
            .entry(decl.name.name.clone())
            .or_default();
        for variant in &decl.variants {
            variant_names.insert(variant.name.name.clone());
        }
        for variant in &decl.variants {
            if let StructBody::Tuple(fields) = &variant.body {
                let tys: Vec<Ty> = fields.iter().map(|f| self.type_from_ast(&f.ty)).collect();
                self.enum_variant_payloads
                    .insert((decl.name.name.clone(), variant.name.name.clone()), tys);
            }
        }
        // Per-variant field types in declaration (discriminant) order, for the
        // MIR structural-equality descriptor of heap enums.
        if let Some(def) = self.resolutions.definition_of(item_id) {
            let mut variant_tys: Vec<Vec<Ty>> = Vec::with_capacity(decl.variants.len());
            for variant in &decl.variants {
                let tys: Vec<Ty> = match &variant.body {
                    StructBody::Tuple(fields) => {
                        fields.iter().map(|f| self.type_from_ast(&f.ty)).collect()
                    }
                    StructBody::Named(fields) => {
                        fields.iter().map(|f| self.type_from_ast(&f.ty)).collect()
                    }
                    StructBody::Unit => Vec::new(),
                };
                variant_tys.push(tys);
            }
            self.tcx.register_enum_variant_tys(def, variant_tys);
        }
        // The heap representation stores the discriminant in a one-byte
        // header field.
        if decl.variants.len() > 256 {
            self.emit(
                TypeError::TooManyVariants {
                    name: decl.name.name.clone(),
                    count: decl.variants.len(),
                },
                span,
            );
        }
    }

    /// Rejects `#[derive(...)]` names that synthesize nothing. Gossamer's
    /// value-type structs / enums compare, order, hash, and copy by value
    /// automatically, so the meaningful derives are exactly `Debug`, `Default`,
    /// `PartialEq`, `Eq`, `PartialOrd`, and `Ord`. Every other name is either
    /// automatic (`Clone` - `let b = a` copies, `Hash`, `Copy`, `Display`,
    /// serde) or implemented with `impl Trait for T` (`From`, operators).
    fn validate_derives(&mut self, attrs: &gossamer_ast::Attrs, span: Span) {
        for attr in &attrs.outer {
            let is_derive =
                attr.path.segments.len() == 1 && attr.path.segments[0].name.name == "derive";
            if !is_derive {
                continue;
            }
            let Some(tokens) = &attr.tokens else {
                continue;
            };
            for tok in tokens.split(',') {
                let name = tok.trim();
                if name.is_empty()
                    || matches!(
                        name,
                        "Debug" | "Default" | "PartialEq" | "Eq" | "PartialOrd" | "Ord"
                    )
                {
                    continue;
                }
                self.emit(
                    TypeError::UnsupportedDerive {
                        name: name.to_string(),
                        hint: derive_rejection_hint(name),
                    },
                    span,
                );
            }
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
        // Tuple-struct fields are modelled as named fields "0".."N-1", so a
        // `Pt(a, b)` constructor (rewritten to a `Pt { 0: a, 1: b }` literal)
        // and positional access `p.0` reuse the named-field machinery.
        let list: Vec<(String, Ty)> = match &decl.body {
            StructBody::Named(fields) => fields
                .iter()
                .map(|f| (f.name.name.clone(), self.type_from_ast(&f.ty)))
                .collect(),
            StructBody::Tuple(fields) => fields
                .iter()
                .enumerate()
                .map(|(i, f)| (i.to_string(), self.type_from_ast(&f.ty)))
                .collect(),
            StructBody::Unit => Vec::new(),
        };
        if !matches!(decl.body, StructBody::Unit) {
            let tys: Vec<Ty> = list.iter().map(|(_, t)| *t).collect();
            self.tcx.register_struct_fields(def, tys);
            self.struct_fields.insert(def, list);
        }
        if matches!(decl.body, StructBody::Tuple(_)) {
            self.tcx.register_tuple_struct(def.local);
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
    /// - `Err(UnknownField { opaque: true })` - the receiver is an
    ///   `Adt` whose field map isn't registered (typical of opaque
    ///   stdlib types like `json::Value`).
    /// - `Err(UnknownField { opaque: false })` - the receiver is a
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
        // Self-type name for receiver-keyed method return types.
        // Generic impls are skipped: their returns may mention
        // `Param` slots that a bare lookup cannot substitute.
        let self_name = if decl.generics.params.is_empty() {
            match &decl.self_ty.kind {
                gossamer_ast::ty::TypeKind::Path(tp) => {
                    tp.segments.last().map(|s| s.name.name.clone())
                }
                _ => None,
            }
        } else {
            None
        };
        // The owner type name for method-ownership tracking is recorded
        // even for generic impls (`impl<T> Stack<T>`), so a method call
        // on a generic user type is not falsely flagged as belonging to
        // a different type.
        let owner_name = match &decl.self_ty.kind {
            gossamer_ast::ty::TypeKind::Path(tp) => tp.segments.last().map(|s| s.name.name.clone()),
            _ => None,
        };
        if let Some(owner) = &owner_name {
            for item in &decl.items {
                if let ImplItem::Fn(fn_decl) = item {
                    self.user_method_owners
                        .entry(fn_decl.name.name.clone())
                        .or_default()
                        .insert(owner.clone());
                }
            }
            // A trait impl exposes the trait's declared methods on the
            // type even when the impl restates only some of them (a
            // default body would otherwise be attributed to no type).
            if let Some(trait_ref) = &decl.trait_ref
                && let Some(trait_seg) = trait_ref.path.segments.last()
                && let Some(methods) = self.trait_own_methods.get(&trait_seg.name.name).cloned()
            {
                for m in methods {
                    self.user_method_owners
                        .entry(m)
                        .or_default()
                        .insert(owner.clone());
                }
            }
        }
        // Record `impl Trait for Type` so a `T: Trait` bound can be verified
        // against the concrete argument type at a generic call site.
        if let Some(trait_ref) = &decl.trait_ref
            && let Some(trait_seg) = trait_ref.path.segments.last()
            && let gossamer_ast::ty::TypeKind::Path(self_tp) = &decl.self_ty.kind
            && let Some(self_seg) = self_tp.segments.last()
        {
            self.trait_impl_types
                .entry(trait_seg.name.name.clone())
                .or_default()
                .insert(self_seg.name.name.clone());
        }
        for item in &decl.items {
            if let ImplItem::Fn(fn_decl) = item {
                let id = NodeId::DUMMY;
                let _ = id;
                self.register_fn_sig_anonymous(fn_decl);
                self.register_method_arg_sig(fn_decl);
                if let Some(name) = &self_name
                    && fn_decl.generics.params.is_empty()
                {
                    let arity = fn_decl
                        .params
                        .iter()
                        .filter(|p| matches!(p, FnParam::Typed { .. }))
                        .count();
                    let ret = match fn_decl.ret.as_ref() {
                        Some(ty) => self.type_from_ast(ty),
                        None => self.tcx.unit(),
                    };
                    self.method_ret_types
                        .insert((name.clone(), fn_decl.name.name.clone(), arity), ret);
                    self.method_arities
                        .insert((name.clone(), fn_decl.name.name.clone()), arity);
                }
            }
        }
    }

    fn collect_trait_signatures(&mut self, decl: &gossamer_ast::TraitDecl) {
        let trait_name = decl.name.name.clone();
        for item in &decl.items {
            if let TraitItem::Fn(fn_decl) = item {
                self.register_fn_sig_anonymous(fn_decl);
                self.register_method_arg_sig(fn_decl);
                let ret = match fn_decl.ret.as_ref() {
                    Some(ty) => self.type_from_ast(ty),
                    None => self.tcx.unit(),
                };
                self.trait_method_ret
                    .insert((trait_name.clone(), fn_decl.name.name.clone()), ret);
            }
        }
    }

    /// Records a method's non-receiver parameter types under its bare
    /// name + arity so [`Self::check_method_call`] can re-type
    /// literal arguments. Every structurally distinct signature for
    /// a key is recorded; coercion later applies only where the
    /// candidates agree (or exactly one is container-shaped), so a
    /// literal is never shaped by the wrong same-named method.
    fn register_method_arg_sig(&mut self, decl: &FnDecl) {
        let inputs: Vec<Ty> = decl
            .params
            .iter()
            .filter(|p| matches!(p, FnParam::Typed { .. }))
            .map(|p| self.param_ty(p))
            .collect();
        let key = (decl.name.name.clone(), inputs.len());
        let entry = self.method_arg_sigs.entry(key).or_default();
        let duplicate = entry.iter().any(|existing| {
            existing.len() == inputs.len()
                && existing
                    .iter()
                    .zip(&inputs)
                    .all(|(x, y)| render_ty(self.tcx, *x) == render_ty(self.tcx, *y))
        });
        if !duplicate {
            entry.push(inputs);
        }
    }

    fn register_fn_sig(&mut self, node: NodeId, decl: &FnDecl, span: Span) {
        let sig = self.fn_sig_of(decl);
        if let Some(def) = self.resolutions.definition_of(node) {
            self.fn_sigs.insert(def, sig);
            // Record the generic arity and per-parameter bounds so each
            // call site can instantiate the parameters independently and
            // verify the argument types satisfy the declared bounds. The
            // arity counts every parameter position (so a const or type
            // parameter's `ParamIdx` indexes the substitution vector
            // directly); the const mask records which positions take a
            // `GenericArg::Const`.
            let has_type_or_const = decl.generics.params.iter().any(|p| {
                matches!(
                    p,
                    gossamer_ast::GenericParam::Type { .. }
                        | gossamer_ast::GenericParam::Const { .. }
                )
            });
            if has_type_or_const {
                self.fn_generic_arity
                    .insert(def, decl.generics.params.len());
                self.fn_param_bounds
                    .insert(def, Self::type_param_bounds(&decl.generics));
                self.fn_generic_const_mask
                    .insert(def, Self::const_param_mask(&decl.generics));
            }
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
        // Enter the function's generic scope so a parameter / return type
        // that names a type parameter (`&T`) records a rigid `TyKind::Param`
        // slot rather than a fresh inference variable. The `Param` slots are
        // what per-call-site instantiation substitutes with fresh variables.
        let prior = self.enter_generic_scope(&decl.generics);
        let inputs: Vec<Ty> = decl
            .params
            .iter()
            .map(|param| self.param_ty(param))
            .collect();
        let output = match decl.ret.as_ref() {
            Some(ty) => self.type_from_ast(ty),
            None => self.tcx.unit(),
        };
        self.leave_generic_scope(prior);
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
                let prev_impl_generics = self.current_impl_generics.replace(decl.generics.clone());
                for impl_item in &decl.items {
                    if let ImplItem::Fn(fn_decl) = impl_item {
                        self.check_fn(fn_decl);
                    } else if let ImplItem::Const { value, .. } = impl_item {
                        self.check_expr(value);
                    }
                }
                self.current_impl_generics = prev_impl_generics;
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
                let init = self.check_expr_expecting(&decl.value, Expectation::HasType(annotated));
                self.unify(annotated, init, decl.value.span);
            }
            ItemKind::Static(decl) => {
                let annotated = self.type_from_ast(&decl.ty);
                let init = self.check_expr_expecting(&decl.value, Expectation::HasType(annotated));
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
        // Enter the function's generic scope so a parameter / return / body
        // type that names a type parameter (`&T`) records a rigid
        // `TyKind::Param`. Monomorphisation substitutes those `Param` slots,
        // and trait-method dispatch on a `T` receiver keys off them.
        let prior_scope = match self.current_impl_generics.clone() {
            Some(impl_g) if !impl_g.params.is_empty() => {
                self.enter_generic_scope_combined(&impl_g, &decl.generics)
            }
            _ => self.enter_generic_scope(&decl.generics),
        };
        let prior_bounds = std::mem::replace(
            &mut self.current_param_bounds,
            Self::type_param_bounds(&decl.generics),
        );
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
            let prev_ret = self.current_fn_ret.replace(ret);
            // The declared return type flows into the body as its
            // expectation, so a literal in return position (block
            // tail / branch / arm) adopts the declared shape -
            // `fn f() -> Vec<T> { [..] }` yields a growable Vec,
            // not `[T; N]`.
            let body_ty = self.check_expr_expecting(body, Expectation::HasType(ret));
            self.current_fn_ret = prev_ret;
            self.unify(ret, body_ty, body.span);
        }
        self.pop_scope();
        self.current_param_bounds = prior_bounds;
        self.leave_generic_scope(prior_scope);
    }

    /// Return type of a method called on a bound type-parameter receiver
    /// (`s.method()` where `s: &T` and `T: Trait`): look the method up in
    /// each of the parameter's bound traits. `None` when the receiver is
    /// not a type parameter or no bound declares the method.
    /// Resolves `ty` and strips any chain of `&` / `&mut` wrappers,
    /// returning the underlying type. References are layout-transparent
    /// in Gossamer (the runtime owns memory), so this is used wherever a
    /// value-vs-reference distinction must not produce a diagnostic.
    fn peel_refs(&mut self, ty: Ty) -> Ty {
        let mut cur = self.infer.resolve(self.tcx, ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(cur) {
            cur = self.infer.resolve(self.tcx, *inner);
        }
        cur
    }

    fn param_method_ret(&mut self, receiver_ty: Ty, method: &str) -> Option<Ty> {
        let mut t = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(t) {
            t = self.infer.resolve(self.tcx, *inner);
        }
        let TyKind::Param { idx, .. } = self.tcx.kind(t)? else {
            return None;
        };
        let bounds = self.current_param_bounds.get(idx.0 as usize)?.clone();
        for bound in bounds {
            if let Some(ret) = self.trait_method_ret.get(&(bound, method.to_string())) {
                return Some(*ret);
            }
        }
        None
    }

    /// Rejects a method on a bound type-parameter receiver that resolves
    /// only through a *supertrait* of one of the parameter's bounds
    /// (P0-5: `fn describe<T: Pet>(p: &T)` calling `p.name()` where
    /// `name` is declared on `Animal` and `trait Pet: Animal`). The
    /// compiled tiers cannot lower supertrait-through-bound dispatch
    /// (SPEC §3.8); it runs right on the VM but miscompiles native, so it
    /// is rejected uniformly. Returns `true` when a diagnostic was
    /// emitted.
    fn reject_supertrait_method_through_bound(
        &mut self,
        receiver_ty: Ty,
        method: &str,
        span: Span,
    ) -> bool {
        let mut t = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(t) {
            t = self.infer.resolve(self.tcx, *inner);
        }
        let Some(TyKind::Param { idx, .. }) = self.tcx.kind(t) else {
            return false;
        };
        let idx = *idx;
        let Some(bounds) = self.current_param_bounds.get(idx.0 as usize).cloned() else {
            return false;
        };
        // If the method is declared directly on any bound, it is a normal
        // generic-bound call (handled elsewhere), not a supertrait leak.
        for bound in &bounds {
            if self
                .trait_own_methods
                .get(bound)
                .is_some_and(|m| m.contains(method))
            {
                return false;
            }
        }
        for bound in &bounds {
            if let Some(supertrait) = self.supertrait_owning_method(bound, method) {
                let param = self
                    .current_generic_scope
                    .iter()
                    .find(|(_, (pidx, _))| *pidx == idx)
                    .map_or_else(|| "T".to_string(), |(name, _)| name.clone());
                self.emit(
                    TypeError::SupertraitMethodThroughBound {
                        param,
                        method: method.to_string(),
                        bound: bound.clone(),
                        supertrait,
                    },
                    span,
                );
                return true;
            }
        }
        false
    }

    /// Walks the supertrait graph of `trait_name` (transitively) and
    /// returns the first supertrait that declares `method`, or `None`.
    fn supertrait_owning_method(&self, trait_name: &str, method: &str) -> Option<String> {
        let mut stack: Vec<String> = self
            .trait_supertraits
            .get(trait_name)
            .cloned()
            .unwrap_or_default();
        let mut seen = std::collections::HashSet::new();
        while let Some(name) = stack.pop() {
            if !seen.insert(name.clone()) {
                continue;
            }
            if self
                .trait_own_methods
                .get(&name)
                .is_some_and(|m| m.contains(method))
            {
                return Some(name);
            }
            if let Some(supers) = self.trait_supertraits.get(&name) {
                stack.extend(supers.iter().cloned());
            }
        }
        None
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
        let vec_pair = self.archive_entry_vec_ty();
        for path_node in collector.arg_paths {
            if let Some(Resolution::Local(binding)) = self.resolutions.get(path_node) {
                self.write_arg_bindings.insert(binding, vec_pair);
            }
        }
    }

    fn bind_fn_param(&mut self, param: &FnParam) {
        match param {
            FnParam::Typed { pattern, ty, .. } => {
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
        self.check_expr_expecting(expr, Expectation::None)
    }

    fn check_expr_expecting(&mut self, expr: &Expr, expected: Expectation) -> Ty {
        if self.enter_recursion(expr.span).is_err() {
            let err = self.tcx.error_ty();
            return self.record(expr.id, err);
        }
        let ty = self.check_expr_kind(expr, expected);
        self.leave_recursion();
        self.record(expr.id, ty)
    }

    /// Resolves the expectation to the structural type it imposes,
    /// peeling one `Ref` - a `&[T]` parameter shapes a bare `[..]`
    /// literal exactly like `[T]` (the borrow is transparent at the
    /// layout level).
    fn expectation_target(&mut self, expected: Expectation) -> Option<Ty> {
        let ty = expected.ty()?;
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind(resolved) {
            Some(TyKind::Ref { inner, .. }) => Some(self.infer.resolve(self.tcx, *inner)),
            Some(_) => Some(resolved),
            None => None,
        }
    }

    /// Type of a `loop` expression: the unified type of its value-carrying
    /// breaks (`let x = loop { break v }` => `x: typeof(v)`); a value-less
    /// loop keeps the divergent `never` type.
    fn check_loop(&mut self, body: &Expr) -> Ty {
        let break_ty = self.fresh();
        self.loop_break_tys.push((break_ty, false));
        self.check_expr(body);
        let (break_ty, used) = self.loop_break_tys.pop().expect("loop stack");
        if used {
            self.infer.resolve(self.tcx, break_ty)
        } else {
            self.tcx.never()
        }
    }

    /// Type-checks a `return value` / `break value`; both diverge (`never`)
    /// but thread their value into the enclosing function return type or the
    /// loop break-type var respectively.
    fn check_return_or_break(&mut self, expr: &Expr, value: Option<&Expr>) -> Ty {
        if let Some(value) = value {
            // `return [..]` carries the declared return shape the same way the
            // block-tail path does, so an explicit `return []` in a `-> [T]` fn
            // is shaped as a Vec rather than a fixed `[T; 0]`.
            let value_expected = match (&expr.kind, self.current_fn_ret) {
                (ExprKind::Return(_), Some(ret)) => Expectation::HasType(ret),
                _ => Expectation::None,
            };
            let got = self.check_expr_expecting(value, value_expected);
            // The expectation only shapes literal containers; unify the checked
            // value against the declared return type so a non-literal mismatch
            // is reported the same way a block tail is.
            if let (ExprKind::Return(_), Some(ret)) = (&expr.kind, self.current_fn_ret) {
                self.unify(ret, got, value.span);
            }
            // `break value` unifies its value with the enclosing loop's
            // break-type var and marks the loop as value-yielding.
            if matches!(expr.kind, ExprKind::Break { .. }) {
                let break_ty = self.loop_break_tys.last_mut().map(|last| {
                    last.1 = true;
                    last.0
                });
                if let Some(break_ty) = break_ty {
                    self.unify(break_ty, got, value.span);
                }
            }
        }
        self.tcx.never()
    }

    #[allow(
        clippy::too_many_lines,
        reason = "expression dispatch - arms map 1:1 to ExprKind variants; splitting hides the dispatch table"
    )]
    fn check_expr_kind(&mut self, expr: &Expr, expected: Expectation) -> Ty {
        match &expr.kind {
            ExprKind::Literal(lit) => self.type_of_literal(lit, expr.span),
            ExprKind::Path(path) => self.check_path_expr(expr.id, path, expr.span),
            ExprKind::Call { callee, args } => self.check_call(callee, args, expected),
            ExprKind::MethodCall {
                receiver,
                name,
                args,
                ..
            } => self.check_method_call(expr.id, &name.name, receiver, args),
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
                        self.check_tuple_field(receiver_ty, *idx, expr.span)
                    }
                }
            }
            ExprKind::Unary { op, operand } => self.check_unary(*op, operand, expr.span, expected),
            ExprKind::Index { base, index } => self.check_index_expr(base, index, expr.span),
            ExprKind::Binary { op, lhs, rhs } => self.check_binary(*op, lhs, rhs, expr.span),
            ExprKind::Assign { place, value, op } => self.check_assign(place, value, *op),
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
            } => self.check_if(condition, then_branch, else_branch.as_deref(), expected),
            ExprKind::Match { scrutinee, arms } => self.check_match(scrutinee, arms, expected),
            ExprKind::Loop { body, .. } => self.check_loop(body),
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
            ExprKind::Block(block) | ExprKind::Unsafe(block) => self.check_block(block, expected),
            ExprKind::Closure { params, ret, body } => {
                self.check_closure(params, ret.as_ref(), body, expected)
            }
            ExprKind::Return(value) | ExprKind::Break { value, .. } => {
                self.check_return_or_break(expr, value.as_deref())
            }
            ExprKind::Continue { .. } => self.tcx.never(),
            ExprKind::Tuple(elems) => {
                let want: Option<Vec<Ty>> = match self.expectation_target(expected) {
                    Some(target) => match self.tcx.kind(target) {
                        Some(TyKind::Tuple(tys)) if tys.len() == elems.len() => Some(tys.clone()),
                        _ => None,
                    },
                    None => None,
                };
                if let Some(want) = want {
                    for (elem, want_ty) in elems.iter().zip(&want) {
                        let got = self.check_expr_expecting(elem, expected.rewrap(*want_ty));
                        if expected.unifies() {
                            self.unify(*want_ty, got, elem.span);
                        }
                    }
                    return self.tcx.intern(TyKind::Tuple(want));
                }
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
                // literal's value type - that lets the inferencer
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
                // `http::Response { … }` - no resolver entry (stdlib
                // opaque type). Pin the literal to the sentinel
                // Response Adt and check the known field shapes so a
                // wrong-typed field reports a clean type mismatch
                // instead of slipping through as an inference
                // variable. `body` stays unchecked: the runtime
                // accepts both String and `[u8]` bodies.
                let resolved_probe = self.infer.resolve(self.tcx, struct_ty);
                let path_tail = path.segments.last().map(|s| s.name.name.as_str());
                let (struct_ty, http_response_fields) =
                    if matches!(self.tcx.kind_of(resolved_probe), TyKind::Var(_))
                        && path_tail == Some("Response")
                    {
                        let def = gossamer_resolve::DefId::local(u32::MAX - 5);
                        let response_ty = self.tcx.intern(TyKind::Adt {
                            def,
                            substs: crate::Substs::new(),
                        });
                        let s = self.tcx.string_ty();
                        let pair = self.tcx.intern(TyKind::Tuple(vec![s, s]));
                        let headers_ty = self.tcx.intern(TyKind::Vec(pair));
                        let fields: Vec<(String, Ty)> = vec![
                            ("status".to_string(), self.tcx.int_ty(IntTy::I64)),
                            ("content_type".to_string(), s),
                            ("headers".to_string(), headers_ty),
                        ];
                        (response_ty, Some(fields))
                    } else {
                        (struct_ty, None)
                    };
                let resolved = self.infer.resolve(self.tcx, struct_ty);
                let declared: Option<Vec<(String, Ty)>> = match self.tcx.kind_of(resolved) {
                    // The literal-specific Response list takes priority
                    // over the stdlib layout: the layout declares
                    // `body: String`, but literal bodies may also be
                    // `[u8]` byte arrays (interp parity).
                    TyKind::Adt { def, .. } => {
                        http_response_fields.or_else(|| self.struct_fields.get(def).cloned())
                    }
                    _ => None,
                };
                for field in fields {
                    if let Some(value) = &field.value {
                        // Substitute `Param { idx }` slots with the
                        // fresh inference vars allocated above so
                        // unification can drive `A`, `B`, ... from
                        // each literal's value type. Checking the
                        // value against the declared field type lets
                        // `S { xs: ["a", "b"] }` lay a heap Vec, not
                        // a fixed `[T; N]`, into a Vec-typed field.
                        let dty_sub = declared.as_ref().and_then(|declared_fields| {
                            declared_fields
                                .iter()
                                .find(|(n, _)| n == &field.name.name)
                                .map(|(_, dty)| *dty)
                        });
                        let dty_sub =
                            dty_sub.map(|dty| self.subst_params_in_ty(dty, &substs_table));
                        let field_expected = match dty_sub {
                            Some(dty) => Expectation::HasType(dty),
                            None => Expectation::None,
                        };
                        let val_ty = self.check_expr_expecting(value, field_expected);
                        if let Some(dty) = dty_sub {
                            self.unify(dty, val_ty, value.span);
                        }
                    }
                }
                if let Some(base) = base {
                    self.check_expr(base);
                }
                struct_ty
            }
            ExprKind::Array(arr) => self.check_array(arr, expected),
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

    /// Returns the type of field `idx` when `ty` is a tuple struct (an
    /// `Adt` whose `idx`-th field is the positional name "idx"), else
    /// `None`. Tuple-struct fields are modelled as named fields "0".."N-1",
    /// so `p.0` positional access reads field "0".
    fn tuple_struct_field_ty(&self, ty: Ty, idx: u32) -> Option<Ty> {
        let TyKind::Adt { def, substs } = self.tcx.kind_of(ty).clone() else {
            return None;
        };
        let is_positional = self
            .struct_fields
            .get(&def)
            .and_then(|list| list.get(idx as usize))
            .is_some_and(|(name, _)| *name == idx.to_string());
        if !is_positional {
            return None;
        }
        self.tcx
            .adt_field_tys(def, &substs)
            .and_then(|tys| tys.get(idx as usize).copied())
    }

    /// Type of `value.N` positional access. Rejects access on a concrete
    /// non-tuple receiver and out-of-range indices (GT0023); a still-
    /// unresolved receiver is deferred for re-check after defaulting.
    fn check_tuple_field(&mut self, receiver_ty: Ty, idx: u32, span: Span) -> Ty {
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(resolved).clone() {
            resolved = self.infer.resolve(self.tcx, inner);
        }
        if let Some(fty) = self.tuple_struct_field_ty(resolved, idx) {
            return fty;
        }
        match self.tcx.kind_of(resolved).clone() {
            TyKind::Tuple(elems) => elems.get(idx as usize).copied().unwrap_or_else(|| {
                let ty = render_ty(self.tcx, resolved);
                self.emit(
                    TypeError::NoTupleField {
                        ty,
                        index: u64::from(idx),
                    },
                    span,
                );
                self.fresh()
            }),
            TyKind::Var(_) => {
                self.deferred_structural.push(DeferredStructural {
                    ty: resolved,
                    span,
                    kind: DeferredStructuralKind::TupleField(u64::from(idx)),
                });
                self.fresh()
            }
            other => {
                if !is_soft_for_structural_use(&other) {
                    let ty = render_ty(self.tcx, resolved);
                    self.emit(
                        TypeError::NoTupleField {
                            ty,
                            index: u64::from(idx),
                        },
                        span,
                    );
                }
                self.fresh()
            }
        }
    }

    /// Element type of `base[index]`. Rejects indexing a concrete
    /// non-indexable receiver (GT0021); a still-unresolved receiver is
    /// deferred for re-check after defaulting.
    fn check_index_expr(&mut self, base: &Expr, index: &Expr, span: Span) -> Ty {
        let base_ty = self.check_expr(base);
        self.check_expr(index);
        let mut cur = self.infer.resolve(self.tcx, base_ty);
        loop {
            match self.tcx.kind_of(cur).clone() {
                TyKind::Ref { inner, .. } => cur = inner,
                TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                    return elem;
                }
                TyKind::String => return self.tcx.int_ty(IntTy::I64),
                TyKind::Var(_) => {
                    self.deferred_structural.push(DeferredStructural {
                        ty: cur,
                        span,
                        kind: DeferredStructuralKind::Index,
                    });
                    return self.fresh();
                }
                other => {
                    // `a[i]` on a user struct / enum routes to its `index` impl
                    // method (one argument); the element type is that method's
                    // return type.
                    if matches!(other, TyKind::Adt { .. })
                        && let Some(adt) = self.adt_name_of(cur)
                        && let Some(&ret) =
                            self.method_ret_types.get(&(adt, "index".to_string(), 1))
                    {
                        return ret;
                    }
                    if !is_soft_for_structural_use(&other) {
                        let ty = render_ty(self.tcx, cur);
                        self.emit(TypeError::NotIndexable { ty }, span);
                    }
                    return self.fresh();
                }
            }
        }
    }

    fn check_call(&mut self, callee: &Expr, args: &[Expr], expected: Expectation) -> Ty {
        if matches!(callee.kind, ExprKind::Path(_)) {
            self.callee_path_nodes.insert(callee.id);
        }
        let callee_ty = self.check_expr(callee);
        let arg_expectations = self.call_arg_expectations(callee, callee_ty, args.len(), expected);
        let arg_tys: Vec<Ty> = args
            .iter()
            .enumerate()
            .map(|(i, a)| {
                let exp = arg_expectations
                    .as_ref()
                    .and_then(|exps| exps.get(i).copied())
                    .unwrap_or(Expectation::None);
                self.check_expr_expecting(a, exp)
            })
            .collect();
        self.check_call_inner(callee, args, callee_ty, &arg_tys)
    }

    /// Per-argument expectations for a call, derived (in priority
    /// order) from the callee's known signature, a variant
    /// constructor's declared payload types (`Value::Blob([1, 2, 3])`
    /// shapes its payload as a heap `[u8]`, not a fixed `[i64; 3]`),
    /// the stdlib archive-write parameter, or - for the bare `Some` /
    /// `Ok` / `Err` constructors - the call's own expected type.
    fn call_arg_expectations(
        &mut self,
        callee: &Expr,
        callee_ty: Ty,
        n_args: usize,
        expected: Expectation,
    ) -> Option<Vec<Expectation>> {
        let resolved = self.infer.resolve(self.tcx, callee_ty);
        // A generic function's parameter types carry rigid `Param` slots;
        // shaping the arguments against them would bind the shared `Param`
        // at the first call and reject every later call with a different
        // concrete type. Leave such arguments unshaped - `check_call_inner`
        // instantiates the signature with fresh variables per call site.
        if let Some(TyKind::FnDef { def, .. }) = self.tcx.kind(resolved)
            && self.fn_generic_arity.contains_key(def)
        {
            return None;
        }
        let sig: Option<FnSig> = match self.tcx.kind(resolved).cloned() {
            Some(TyKind::FnPtr(sig)) => Some(sig),
            Some(TyKind::FnDef { def, .. }) => self.fn_sigs.get(&def).cloned(),
            _ => None,
        };
        if let Some(sig) = sig {
            if sig.inputs.len() == n_args {
                return Some(
                    sig.inputs
                        .iter()
                        .map(|t| Expectation::HasType(*t))
                        .collect(),
                );
            }
            return None;
        }
        let ExprKind::Path(path) = &callee.kind else {
            return None;
        };
        let n = path.segments.len();
        if n >= 2 {
            let key = (
                path.segments[n - 2].name.name.clone(),
                path.segments[n - 1].name.name.clone(),
            );
            if let Some(payloads) = self.enum_variant_payloads.get(&key).cloned()
                && payloads.len() == n_args
            {
                // Coerce-only: variant payload registration is keyed
                // by (enum, variant) name and a same-named pair from
                // another scope must not unify into this call.
                return Some(payloads.iter().map(|t| Expectation::Coerce(*t)).collect());
            }
        }
        let last = path.segments[n - 1].name.name.as_str();
        if last == "write"
            && n >= 2
            && matches!(path.segments[n - 2].name.name.as_str(), "tar" | "zip")
            && n_args == 1
        {
            let entries = self.archive_entry_vec_ty();
            return Some(vec![Expectation::Coerce(entries)]);
        }
        if n == 1 && n_args == 1 {
            // `Some(x)` / `Ok(x)` / `Err(e)`: thread the expected
            // `Option<T>` / `Result<T, E>` payload slot into the
            // argument so `Some([1, 2])` against `Option<Vec<i64>>`
            // lays a heap Vec into the payload.
            let payload_slot = match last {
                "Some" | "Ok" => Some(0),
                "Err" => Some(1),
                _ => None,
            };
            if let Some(slot) = payload_slot
                && let Some(target) = self.expectation_target(expected)
                && let Some(TyKind::Adt { def, substs }) = self.tcx.kind(target)
            {
                let name_ok = match last {
                    "Some" => self.tcx.def_name(*def) == Some("Option"),
                    _ => self.tcx.def_name(*def) == Some("Result"),
                };
                if name_ok && let Some(payload) = substs.types().get(slot).copied() {
                    return Some(vec![expected.rewrap(payload)]);
                }
            }
        }
        None
    }

    /// Rejects `json::render` / `json::encode` of an enum value
    /// (`Result` / `Option` / user enum). The encoder is polymorphic
    /// over `json::Value`, scalars, arrays, and structs, but an enum
    /// has no JSON form and is almost always a `json::parse(..)` whose
    /// `?` was forgotten. The VM tolerated the misuse (emitting the
    /// `Ok` payload) while a native build silently emitted `""`, so the
    /// checker rejects it uniformly with a `?`-pointing diagnostic.
    fn reject_json_enum_arg(&mut self, op: &str, callee: &Expr, args: &[Expr], arg_tys: &[Ty]) {
        let Some(&first_ty) = arg_tys.first() else {
            return;
        };
        let mut peeled = self.infer.resolve(self.tcx, first_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(peeled).cloned() {
            peeled = self.infer.resolve(self.tcx, inner);
        }
        let adt_def = match self.tcx.kind(peeled) {
            Some(TyKind::Adt { def, .. }) => Some(*def),
            _ => None,
        };
        if let Some(def) = adt_def
            && self.tcx.struct_field_tys(def).is_none()
        {
            let span = args.first().map_or(callee.span, |a| a.span);
            let ty = crate::printer::render_ty(self.tcx, peeled);
            self.emit(
                TypeError::JsonNotSerializable {
                    op: op.to_string(),
                    ty,
                },
                span,
            );
        }
    }

    /// Substitutes a generic function's signature for one call site:
    /// type parameters become the fresh inference variables in `vars`,
    /// and const parameters are inferred from the array arguments whose
    /// lengths name them. Re-records the callee as a `FnDef` carrying the
    /// resolved substitution so MIR monomorphisation reads the concrete
    /// instantiation.
    fn instantiate_generic_sig(
        &mut self,
        callee: &Expr,
        def: gossamer_resolve::DefId,
        vars: &[Ty],
        sig: FnSig,
        arg_tys: &[Ty],
    ) -> FnSig {
        let n = vars.len();
        let const_mask = self
            .fn_generic_const_mask
            .get(&def)
            .cloned()
            .unwrap_or_default();
        // Infer each const generic from the array argument whose length
        // names it (`sum_arr([1, 2, 3])` => N = 3), so the substituted
        // `[T; N]` carries the concrete count.
        let mut const_substs: Vec<Option<i128>> = vec![None; n];
        for (param, arg_ty) in sig.inputs.iter().zip(arg_tys.iter()) {
            if let Some((idx, value)) = self.infer_array_const_len(*param, *arg_ty)
                && idx < n
            {
                const_substs[idx] = Some(value);
            }
        }
        // A const-generic array return (`-> [T; N]`) is carried as a runtime
        // GosVec - the same representation as the by-value `[T; N]` parameter
        // it is derived from. The call-site result type is therefore `Vec<T>`,
        // not the substituted fixed-length array: binding it as `[T; k]` would
        // make the caller read the heap Vec inline and treat the buffer pointer
        // as element 0.
        let output = match self.tcx.kind_of(sig.output) {
            TyKind::Array {
                elem,
                len: crate::ArrayLen::Param(_),
            } => {
                let elem = *elem;
                let elem = self.subst_generics_in_ty(elem, vars, &const_substs);
                self.tcx.intern(TyKind::Vec(elem))
            }
            _ => self.subst_generics_in_ty(sig.output, vars, &const_substs),
        };
        let new_sig = FnSig {
            inputs: sig
                .inputs
                .iter()
                .map(|t| self.subst_generics_in_ty(*t, vars, &const_substs))
                .collect(),
            output,
        };
        // Const positions carry the inferred value; every other position
        // carries its fresh type variable (pinned by argument unification).
        let subst_args: Vec<crate::GenericArg> = (0..n)
            .map(|i| {
                if const_mask.get(i).copied().unwrap_or(false) {
                    crate::GenericArg::Const(const_substs[i].unwrap_or(0))
                } else {
                    crate::GenericArg::Type(vars[i])
                }
            })
            .collect();
        let fndef = self.tcx.intern(TyKind::FnDef {
            def,
            substs: crate::Substs::from_args(subst_args),
        });
        self.record(callee.id, fndef);
        new_sig
    }

    #[allow(
        clippy::too_many_lines,
        reason = "sequential callee-shape dispatch: signature, variant constructor, then stdlib fallbacks"
    )]
    fn check_call_inner(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        callee_ty: Ty,
        arg_tys: &[Ty],
    ) -> Ty {
        let resolved = self.infer.resolve(self.tcx, callee_ty);
        let kind = self.tcx.kind(resolved).cloned();
        // Recognised callee shapes: `FnPtr` (anonymous or first-class
        // closure pointer) and `FnDef { def, .. }` (named function
        // resolved to a definition). Looking the def up in
        // `fn_sigs` lets cross-function call sites pin both args and
        // return type to the callee's signature instead of returning
        // a fresh inference variable that never gets bound.
        let callee_def = match self.tcx.kind(resolved) {
            Some(TyKind::FnDef { def, .. }) => Some(*def),
            _ => None,
        };
        let sig_lookup: Option<FnSig> = match kind {
            Some(TyKind::FnPtr(sig)) => Some(sig),
            Some(TyKind::FnDef { def, .. }) => self.fn_sigs.get(&def).cloned(),
            _ => None,
        };
        if let Some(mut sig) = sig_lookup {
            // Per-call-site instantiation of a generic function: replace
            // the signature's rigid `Param` slots with one fresh inference
            // variable each, so independent call sites bind the parameters
            // independently (without this, the second call with a different
            // concrete type fails to unify against the first's binding).
            let inst: Option<(gossamer_resolve::DefId, Vec<Ty>)> = callee_def.and_then(|def| {
                self.fn_generic_arity
                    .get(&def)
                    .copied()
                    .filter(|n| *n > 0)
                    .map(|n| (def, (0..n).map(|_| self.fresh()).collect::<Vec<Ty>>()))
            });
            if let Some((def, vars)) = &inst {
                sig = self.instantiate_generic_sig(callee, *def, vars, sig, arg_tys);
            }
            if sig.inputs.len() == arg_tys.len() {
                for (param, (arg_ty, arg_expr)) in sig.inputs.iter().zip(arg_tys.iter().zip(args)) {
                    self.check_sig_param_arg(*param, *arg_ty, arg_expr.span);
                }
                if let Some((def, vars)) = &inst {
                    self.check_trait_bounds(*def, vars, callee.span);
                }
                return sig.output;
            }
            // A known callee signature whose declared arity does not
            // match the call: the VM aborts (`CallArityMismatch` in the
            // MIR verifier) and the native backend silently drops or
            // zero-fills the surplus/missing arguments. Reject it
            // statically so `check` is never looser than the tiers. A
            // call on the right of `|>` receives the piped value as an
            // implicit trailing argument, so count it toward the arity.
            let pipe_extra = usize::from(self.pipe_stage_callees.contains(&callee.id));
            let effective = arg_tys.len() + pipe_extra;
            if effective != sig.inputs.len() {
                self.emit(
                    TypeError::CallArityMismatch {
                        callee: callee_display_name(callee),
                        expected: sig.inputs.len(),
                        found: effective,
                    },
                    callee.span,
                );
            }
            // Fall through to the existing stdlib / fresh handling so a
            // pipe-stage call keeps its current return typing.
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
            // `strings::` free functions have no `FnSig` to unify
            // against, so validate their string-typed argument slots
            // here. Skipped when the callee resolves to a user `FnDef`
            // (a user module named `strings` keeps its own typing) or
            // when the value is piped in (`|>` appends the data argument
            // during lowering, shifting the positions this table keys
            // on).
            if matches!(module, ["strings"] | ["std", "strings"])
                && !matches!(self.tcx.kind(resolved), Some(TyKind::FnDef { .. }))
                && !self.pipe_stage_callees.contains(&callee.id)
            {
                self.check_strings_free_call_args(last, args, arg_tys);
            }
            // Data-last std combinators (`result::map_err(f, r)`,
            // `iter::map(f, xs)`, ...): the signature table pins
            // closure params to the data payload type. Gated on the
            // callee not being a resolved user `FnDef` so a user
            // module that happens to be named `iter` / `result` /
            // `option` keeps its own typing.
            if !matches!(self.tcx.kind(resolved), Some(TyKind::FnDef { .. }))
                && let Some(ret) = self.check_std_combinator_free_call(
                    callee,
                    args,
                    arg_tys,
                    combinator_module_name(module),
                    last,
                )
            {
                return ret;
            }
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
                // The `[(String, [u8])]` argument shape flowed in via
                // `call_arg_expectations`; only the return type is
                // synthesized here.
                let u8_ty = self.tcx.int_ty(IntTy::U8);
                let vec_u8 = self.tcx.intern(TyKind::Vec(u8_ty));
                let e = self.tcx.dyn_error_ty();
                return self.result_adt_ty(vec_u8, e);
            }
            if let Some(ty) = self.check_stdlib_module_ret_ty(module, last, callee, args, arg_tys) {
                return ty;
            }
            // Built-in intrinsics emitted by the parser's macro
            // expansion (`format!` only - `println!` / `print!` /
            // `eprintln!` / `eprint!` etc. expand to a call to the
            // outer name with the format-built string as the single
            // argument, and pinning `println` to Unit broke
            // generic-monomorph paths that route through user-named
            // functions called `println`). Pinning `__concat` and
            // `__fmt_prec` to `String` is safe: they're synthetic
            // names the parser injects and no user code can
            // shadow them.
            if module.is_empty()
                && let Some(ty) = self.check_bare_intrinsic_call(last, arg_tys)
            {
                return ty;
            }
        }
        self.reject_noncallable_callee(callee, callee_ty);
        self.fresh()
    }

    /// After a call failed every resolution path: if the callee is a
    /// concrete, fully-known value that can never be a function or an ADT
    /// constructor, reject it (`GT0022`) - the compiled tier would emit a
    /// call through a non-function symbol. A qualified path callee
    /// (`String::new`) types loosely as the receiving type and is never
    /// flagged; an inference-var callee (e.g. an unsuffixed `let x = 5`)
    /// is deferred for re-check after defaulting.
    fn reject_noncallable_callee(&mut self, callee: &Expr, callee_ty: Ty) {
        let resolved_callee = self.infer.resolve(self.tcx, callee_ty);
        let callee_kind = self.tcx.kind_of(resolved_callee).clone();
        let qualified_path_callee =
            matches!(&callee.kind, ExprKind::Path(p) if p.segments.len() >= 2);
        if matches!(callee_kind, TyKind::Var(_)) {
            self.deferred_structural.push(DeferredStructural {
                ty: resolved_callee,
                span: callee.span,
                kind: DeferredStructuralKind::Call,
            });
        } else if is_definitely_not_callable_value(&callee_kind) && !qualified_path_callee {
            let ty = render_ty(self.tcx, resolved_callee);
            self.emit(TypeError::NotCallable { ty }, callee.span);
        }
    }

    /// Concrete return type of a stdlib `json` / `errors` / `fs` / `os`
    /// free call whose signature lives outside user source. Returning a
    /// real type (rather than a fresh inference var) lets the checker
    /// catch mismatches - e.g. matching `fs::file_size(p)`'s bare `i64`
    /// against a `Result` pattern. The json accessors return `Option<T>`
    /// at runtime (the interp emits `Some`/`None`), so they are typed as
    /// such; `json::get(v, k).unwrap()` and the autoderive `Some`/`None`
    /// matches both rely on it.
    /// Return type of a qualified `HashMap::pop(m, k)` / `HashMap::get(m, k)`
    /// free-fn call. The method form (`m.pop(k)`) is typed by
    /// `check_method_call` from the receiver's static type; the qualified form
    /// must recover the value type from the first argument's map type so the
    /// `Option<V>` payload binding is the concrete value rather than an
    /// unresolved var - a struct field read on an unresolved payload lowers to
    /// the dynamic json accessor and faults at runtime.
    fn check_qualified_map_accessor_ret(
        &mut self,
        module: &[&str],
        last: &str,
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        if !matches!(
            module,
            ["HashMap"] | ["collections", "HashMap"] | ["std", "collections", "HashMap"]
        ) || !matches!(last, "pop" | "get")
        {
            return None;
        }
        let value = arg_tys.first().and_then(|t| {
            let resolved = self.infer.resolve(self.tcx, *t);
            let peeled = match self.tcx.kind(resolved) {
                Some(TyKind::Ref { inner, .. }) => self.infer.resolve(self.tcx, *inner),
                _ => resolved,
            };
            match self.tcx.kind(peeled) {
                Some(TyKind::HashMap { value, .. }) => Some(*value),
                _ => None,
            }
        });
        value.map(|v| self.option_adt_ty(v))
    }

    /// Rejects a non-string argument in a string-typed parameter slot
    /// of a `strings::` free-function call. The stdlib free path has no
    /// `FnSig` to unify against, so without this the checker accepts an
    /// integer where a `String` is expected and the compiled string
    /// shims dereference it as a pointer (a SIGSEGV the VM masks).
    fn check_strings_free_call_args(&mut self, name: &str, args: &[Expr], arg_tys: &[Ty]) {
        let Some(shapes) = strings_fn_str_params(name) else {
            return;
        };
        for &(idx, shape) in shapes {
            let (Some(arg), Some(&arg_ty)) = (args.get(idx), arg_tys.get(idx)) else {
                continue;
            };
            self.check_str_param_arg(shape, arg_ty, arg.span);
        }
    }

    /// Unifies one call argument against its declared parameter type,
    /// peeling a single reference on either side. Native lowering treats
    /// every value-vs-reference distinction as a no-op (the runtime owns
    /// memory), so a `&Shape` parameter accepts a `Shape` argument and
    /// vice versa.
    fn check_sig_param_arg(&mut self, param: Ty, arg_ty: Ty, span: Span) {
        let param_inner = match self.tcx.kind(param) {
            Some(TyKind::Ref { inner, .. }) => Some(*inner),
            _ => None,
        };
        let arg_inner = match self.tcx.kind(arg_ty) {
            Some(TyKind::Ref { inner, .. }) => Some(*inner),
            _ => None,
        };
        let (lhs, rhs) = match (param_inner, arg_inner) {
            (Some(p), None) => (p, arg_ty),
            (None, Some(a)) => (param, a),
            _ => (param, arg_ty),
        };
        // An unsuffixed float literal is an inference var that unifies
        // leniently with any concrete type, so a `String` parameter would
        // silently accept it and the compiled tier reads its f64 bits as a
        // string pointer. Reject it here; an integer literal is already
        // caught by the unifier's integer-constraint check.
        if matches!(
            self.tcx.kind(self.infer.resolve(self.tcx, lhs)),
            Some(TyKind::String)
        ) && self.infer.is_float_literal_var(self.tcx, rhs)
        {
            self.emit_str_slot_mismatch("{float}", span);
        } else {
            self.unify(lhs, rhs, span);
        }
    }

    /// Validates the string-typed arguments of a `String` method call
    /// (`s.contains(x)`). The method dispatches to the same `strings::`
    /// shim as the free function with the receiver as the implicit first
    /// argument, so the explicit args occupy parameter positions 1..
    fn check_strings_method_call_args(&mut self, method: &str, args: &[Expr], arg_tys: &[Ty]) {
        let Some(shapes) = strings_fn_str_params(method) else {
            return;
        };
        for &(pos, shape) in shapes {
            // Position 0 is the receiver, already known to be a `String`.
            let Some(idx) = pos.checked_sub(1) else {
                continue;
            };
            let (Some(arg), Some(&arg_ty)) = (args.get(idx), arg_tys.get(idx)) else {
                continue;
            };
            self.check_str_param_arg(shape, arg_ty, arg.span);
        }
    }

    /// Validates one argument against a string-shaped parameter slot.
    fn check_str_param_arg(&mut self, shape: StrArgShape, arg_ty: Ty, span: Span) {
        // `&"hi"` (a `Ref<String>`) is layout-transparent to its inner
        // `String` at every call boundary; validate the referent.
        let resolved = self.infer.resolve(self.tcx, arg_ty);
        let inner = match self.tcx.kind(resolved) {
            Some(TyKind::Ref { inner, .. }) => *inner,
            _ => resolved,
        };
        // Unsuffixed numeric literals are inference variables that unify
        // leniently with any concrete type, so a single expected `String`
        // would not reject them. Catch them up front - in either slot
        // shape - so a `5` / `1.5` in a string position is rejected with
        // the same `{integer}` / `{float}` rendering a user call shows.
        if self.infer.is_integer_constrained_var(self.tcx, inner) {
            self.emit_str_slot_mismatch("{integer}", span);
            return;
        }
        if self.infer.is_float_literal_var(self.tcx, inner) {
            self.emit_str_slot_mismatch("{float}", span);
            return;
        }
        match shape {
            StrArgShape::Str => {
                // A `String` slot admits only a real string; reuse the
                // unifier to reject every other concrete type (`char`,
                // `bool`, an `Adt`, ...) with a precise mismatch.
                let s = self.tcx.string_ty();
                self.unify(s, inner, span);
            }
            StrArgShape::StrOrChar => {
                // A pattern / pad slot also admits a `char`, so the
                // unifier (single expected type) is too strict; reject
                // only the numeric / bool shapes the backend would
                // misread as a string pointer.
                let r = self.infer.resolve(self.tcx, inner);
                if matches!(
                    self.tcx.kind(r),
                    Some(TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool)
                ) {
                    self.emit_str_slot_mismatch(&render_ty(self.tcx, r), span);
                }
            }
        }
    }

    fn emit_str_slot_mismatch(&mut self, found: &str, span: Span) {
        self.emit(
            TypeError::TypeMismatch {
                expected: "String".to_string(),
                found: found.to_string(),
            },
            span,
        );
    }

    fn check_stdlib_module_ret_ty(
        &mut self,
        module: &[&str],
        last: &str,
        callee: &Expr,
        args: &[Expr],
        arg_tys: &[Ty],
    ) -> Option<Ty> {
        if let Some(ty) = self.check_qualified_map_accessor_ret(module, last, arg_tys) {
            return Some(ty);
        }
        // `env::var(name) -> Option<String>`. Typing it concretely lets the
        // match checker reject matching its result with `Result` patterns
        // (`Ok`/`Err`), which otherwise silently fell through on the VM and
        // matched by discriminant on the compiled tier.
        if matches!(module, ["env"] | ["std", "env"]) && last == "var" {
            let s = self.tcx.string_ty();
            return Some(self.option_adt_ty(s));
        }
        if matches!(
            module,
            ["json"] | ["encoding", "json"] | ["std", "encoding", "json"]
        ) {
            return match last {
                "parse" | "decode" => {
                    let j = self.tcx.json_value_ty();
                    let e = self.tcx.dyn_error_ty();
                    Some(self.result_adt_ty(j, e))
                }
                "render" | "encode" => {
                    self.reject_json_enum_arg(last, callee, args, arg_tys);
                    Some(self.tcx.string_ty())
                }
                "at" | "identity" => Some(self.tcx.json_value_ty()),
                "get" => {
                    let j = self.tcx.json_value_ty();
                    Some(self.option_adt_ty(j))
                }
                "len" => Some(self.tcx.int_ty(IntTy::I64)),
                "is_null" => Some(self.tcx.bool_ty()),
                "as_i64" => {
                    let i = self.tcx.int_ty(IntTy::I64);
                    Some(self.option_adt_ty(i))
                }
                "as_f64" => {
                    let f = self.tcx.float_ty(FloatTy::F64);
                    Some(self.option_adt_ty(f))
                }
                "as_str" => {
                    let s = self.tcx.string_ty();
                    Some(self.option_adt_ty(s))
                }
                "as_bool" => {
                    let b = self.tcx.bool_ty();
                    Some(self.option_adt_ty(b))
                }
                "as_array" => {
                    let j = self.tcx.json_value_ty();
                    let arr = self.tcx.intern(TyKind::Vec(j));
                    Some(self.option_adt_ty(arr))
                }
                _ => None,
            };
        }
        if matches!(module, ["errors"] | ["std", "errors"]) {
            return match last {
                "new" | "wrap" => Some(self.tcx.dyn_error_ty()),
                _ => None,
            };
        }
        // `VecDeque::new()` yields a `VecDeque<?elem>` whose element generic
        // is pinned by the first `push_back` (see `check_method_call`), so an
        // unannotated `let q = VecDeque::new()` still recovers `Option<T>` for
        // `pop_front()` across every tier.
        if matches!(
            module,
            ["VecDeque"] | ["collections", "VecDeque"] | ["std", "collections", "VecDeque"]
        ) && last == "new"
        {
            let elem = self.fresh();
            let substs = crate::Substs::from_types([elem]);
            let def = gossamer_resolve::DefId::local(u32::MAX - 9);
            self.tcx.register_def_name(def, "VecDeque");
            return Some(self.tcx.intern(TyKind::Adt { def, substs }));
        }
        if matches!(module, ["fs" | "os"] | ["std", "fs" | "os"]) {
            return match last {
                "file_size" => Some(self.tcx.int_ty(IntTy::I64)),
                "exists" | "is_file" | "is_dir" | "is_symlink" => Some(self.tcx.bool_ty()),
                _ => None,
            };
        }
        // `time::Duration` constructors yield the transparent Duration
        // newtype so the method-form accessors (`d.as_millis()`) can
        // dispatch on the receiver's static type; the accessors
        // themselves return a bare `i64`.
        if matches!(module, ["time", "Duration"] | ["std", "time", "Duration"]) {
            return match last {
                "from_millis" | "from_secs" | "from_micros" => Some(self.tcx.duration_ty()),
                "as_millis" | "as_secs" | "as_micros" => Some(self.tcx.int_ty(IntTy::I64)),
                _ => None,
            };
        }
        // `time::Instant::now()` yields the transparent Instant newtype so
        // the method-form accessor (`inst.elapsed_ms()`) can dispatch on
        // the receiver's static type; the accessor itself returns `i64`.
        if matches!(module, ["time", "Instant"] | ["std", "time", "Instant"]) {
            return match last {
                "now" => Some(self.tcx.instant_ty()),
                "elapsed_ms" => Some(self.tcx.int_ty(IntTy::I64)),
                _ => None,
            };
        }
        None
    }

    /// Types the parser-injected format intrinsics and the bare
    /// variant constructors. The resolver doesn't hand `Some` / `Ok` /
    /// `Err` / `None` a `DefId`, so the call expression typechecks as
    /// a fresh `Var` and the binding `let first = Some(10)` collapses
    /// to `Int(I64)` - losing the Adt wrapper. Match dispatch later
    /// treats the 8-byte `*mut GosResult` pointer as a raw i64 and
    /// reads garbage from the slot. Recognise the four standard
    /// variants here and synthesise the right Adt: `Some(t)` →
    /// `Option<t>`, `Ok(t)` → `Result<t, ?>`, `Err(e)` →
    /// `Result<?, e>`, `None` → `Option<?>`. Pinning `__concat` /
    /// `__fmt_prec` to `String` is safe: they're synthetic names the
    /// parser injects and no user code can shadow them.
    fn check_bare_intrinsic_call(&mut self, name: &str, arg_tys: &[Ty]) -> Option<Ty> {
        let ty = match name {
            "__concat" | "__fmt_prec" | "__fmt_pad" | "__fmt_radix" | "__fmt_upper" => {
                self.tcx.string_ty()
            }
            // `channel()` -> `(Sender<?T>, Receiver<?T>)` sharing one element
            // var, so `tx.send(v)` unifies the element through the shared `?T`
            // and `rx.recv()` yields `Option<?T>` with the real payload type
            // even for an inferred (local) channel.
            "channel" if arg_tys.is_empty() => {
                let elem = self.fresh();
                let sender = self.tcx.intern(TyKind::Sender(elem));
                let receiver = self.tcx.intern(TyKind::Receiver(elem));
                self.tcx.intern(TyKind::Tuple(vec![sender, receiver]))
            }
            "Some" => {
                let payload = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                self.option_adt_ty(payload)
            }
            "None" => {
                let payload = self.fresh();
                self.option_adt_ty(payload)
            }
            "Ok" => {
                let ok_ty = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                let err_ty = self.fresh();
                self.result_adt_ty(ok_ty, err_ty)
            }
            "Err" => {
                let ok_ty = self.fresh();
                let err_ty = arg_tys.first().copied().unwrap_or_else(|| self.fresh());
                self.result_adt_ty(ok_ty, err_ty)
            }
            _ => return None,
        };
        Some(ty)
    }

    /// The `[(String, [u8])]` entry-list parameter type of the stdlib
    /// `archive::{tar,zip}::write` calls.
    fn archive_entry_vec_ty(&mut self) -> Ty {
        let s = self.tcx.string_ty();
        let u8_ty = self.tcx.int_ty(IntTy::U8);
        let vec_u8 = self.tcx.intern(TyKind::Vec(u8_ty));
        let pair = self.tcx.intern(TyKind::Tuple(vec![s, vec_u8]));
        self.tcx.intern(TyKind::Vec(pair))
    }

    /// Re-records literal nodes to a type discovered by *joining*
    /// sibling branches - `if c { [1, 2] } else { [3] }` joins to
    /// `Vec<i64>` only after both arms are checked, so the arm
    /// literals (and the wrapper nodes codegen sizes result slots
    /// from) are re-shaped afterwards. This is the synthesis-side
    /// complement of [`Expectation`], which handles every site where
    /// the expected type is known *before* checking.
    /// Bare nominal name of a struct/enum type, seeing through `&`/`&mut`.
    /// Returns `None` for non-ADT types. Used to look up operator-overload
    /// impl methods (`V2::add`).
    fn adt_name_of(&mut self, ty: Ty) -> Option<String> {
        let mut r = self.infer.resolve(self.tcx, ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(r) {
            r = self.infer.resolve(self.tcx, *inner);
        }
        if let Some(TyKind::Adt { def, .. }) = self.tcx.kind(r) {
            self.tcx.def_name(*def).map(str::to_string)
        } else {
            None
        }
    }

    /// Coerces a byte literal compared against an integer operand to that
    /// operand's integer type, so `s[i] == b'>'` type-checks without a cast.
    /// Returns true when it applied (caller then skips the same-type unify).
    fn coerce_byte_literal_cmp(&mut self, lhs: &Expr, lhs_ty: Ty, rhs: &Expr, rhs_ty: Ty) -> bool {
        let is_byte_lit = |e: &Expr| matches!(&e.kind, ExprKind::Literal(Literal::Byte(_)));
        let lr = self.infer.resolve(self.tcx, lhs_ty);
        let rr = self.infer.resolve(self.tcx, rhs_ty);
        if is_byte_lit(lhs) && self.is_integer(rr) {
            self.record(lhs.id, rr);
            true
        } else if is_byte_lit(rhs) && self.is_integer(lr) {
            self.record(rhs.id, lr);
            true
        } else {
            false
        }
    }

    fn adjust_literal_to_join(&mut self, expr: &Expr, expected: Ty) {
        let expected = self.infer.resolve(self.tcx, expected);
        let expected = match self.tcx.kind(expected) {
            Some(TyKind::Ref { inner, .. }) => *inner,
            _ => expected,
        };
        match &expr.kind {
            // `&[..]` / `&mut [..]`: the borrow is transparent at the
            // layout level - re-type the borrowed literal itself
            // (expected already had its `Ref` stripped above).
            ExprKind::Unary {
                op: UnaryOp::RefShared | UnaryOp::RefMut,
                operand,
            } => self.adjust_literal_to_join(operand, expected),
            ExprKind::Array(ArrayExpr::List(elems)) => {
                if let Some(TyKind::Vec(e) | TyKind::Slice(e)) = self.tcx.kind(expected).cloned() {
                    self.record(expr.id, expected);
                    for el in elems {
                        self.adjust_literal_to_join(el, e);
                    }
                }
            }
            ExprKind::Array(ArrayExpr::Repeat { value, .. }) => {
                if let Some(TyKind::Vec(e) | TyKind::Slice(e)) = self.tcx.kind(expected).cloned() {
                    self.record(expr.id, expected);
                    self.adjust_literal_to_join(value, e);
                }
            }
            ExprKind::Tuple(elems) => {
                if let Some(TyKind::Tuple(tys)) = self.tcx.kind(expected).cloned() {
                    if tys.len() == elems.len() {
                        self.record(expr.id, expected);
                        for (el, t) in elems.iter().zip(tys) {
                            self.adjust_literal_to_join(el, t);
                        }
                    }
                }
            }
            // Push the expected type through value-producing positions so
            // a literal in a block tail / branch / arm is re-recorded too:
            // `fn f() -> Vec<T> { [..] }`,
            // `let v: Vec<T> = if c { [..] } else { [..] }`. The wrapping
            // node is re-recorded as well - codegen sizes the block/if/
            // match result slot from its node type, so leaving it `[T; N]`
            // while the branches build a heap Vec would desync the slot.
            ExprKind::Block(block) | ExprKind::Unsafe(block) => {
                if let Some(tail) = &block.tail {
                    self.record(expr.id, expected);
                    self.adjust_literal_to_join(tail, expected);
                }
            }
            ExprKind::If {
                then_branch,
                else_branch,
                ..
            } => {
                self.record(expr.id, expected);
                self.adjust_literal_to_join(then_branch, expected);
                if let Some(else_branch) = else_branch {
                    self.adjust_literal_to_join(else_branch, expected);
                }
            }
            ExprKind::Match { arms, .. } => {
                self.record(expr.id, expected);
                for arm in arms {
                    self.adjust_literal_to_join(&arm.body, expected);
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

    /// `value.downgrade()` produces a `Weak<T>` for any RC-managed aggregate;
    /// `weak.upgrade()` produces `Option<T>` from a `Weak<T>`. Name-global
    /// dispatch resolved these while the receiver type was an unresolved
    /// variable; a concretely-typed receiver (e.g. an enum bound from a
    /// variant constructor) needs the explicit rule. Returns `None` for any
    /// other method / receiver so normal dispatch continues.
    fn check_weak_method(&mut self, method: &str, receiver_ty: Ty, args: &[Expr]) -> Option<Ty> {
        if !args.is_empty() {
            return None;
        }
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, substs })
                if def.local == u32::MAX - 6 && method == "upgrade" =>
            {
                let payload = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                Some(self.option_adt_ty(payload))
            }
            Some(TyKind::Adt { .. }) if method == "downgrade" => Some(self.weak_adt_ty(resolved)),
            _ => None,
        }
    }

    /// `recv` / `try_recv` on a `Receiver<T>` yields `Option<T>`, and `send`
    /// / `try_send` on a `Sender<T>` consumes a `T`. Pinning the element type
    /// here sizes a `while let Some(p) = rx.recv()` binding by `T`'s real slot
    /// count, so a struct sent over a channel materialises as its inline
    /// fields rather than a single pointer word. Returns `None` for any
    /// non-channel receiver so the caller continues normal dispatch.
    fn check_channel_method(&mut self, method: &str, receiver_ty: Ty, args: &[Expr]) -> Option<Ty> {
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        match self.tcx.kind(resolved) {
            Some(TyKind::Receiver(elem)) if matches!(method, "recv" | "try_recv") => {
                let elem = *elem;
                for arg in args {
                    self.check_expr(arg);
                }
                Some(self.option_adt_ty(elem))
            }
            Some(TyKind::Sender(elem)) if matches!(method, "send" | "try_send") => {
                let elem = *elem;
                for arg in args {
                    let v = self.check_expr(arg);
                    self.unify(elem, v, arg.span);
                }
                Some(self.tcx.unit())
            }
            _ => None,
        }
    }

    /// `q.push_back(v)` on a `VecDeque<?elem>` pins the element generic to
    /// `v`'s type, so an unannotated deque infers `VecDeque<String>` from the
    /// first push and `pop_front()` recovers `Option<String>`. Returns
    /// `Some(unit)` when handled, `None` for any other receiver / method.
    fn check_deque_push_back(
        &mut self,
        method: &str,
        receiver_ty: Ty,
        span: gossamer_lex::Span,
        args: &[Expr],
    ) -> Option<Ty> {
        if method != "push_back" || args.len() != 1 {
            return None;
        }
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        if let Some(TyKind::Adt { def, substs }) = self.tcx.kind(resolved)
            && def.local == u32::MAX - 9
        {
            let elem = substs.types().first().copied();
            let v = self.check_expr(&args[0]);
            if let Some(elem) = elem {
                self.unify(elem, v, span);
            }
            return Some(self.tcx.unit());
        }
        None
    }

    /// Expected closure parameter types for a `Vec`/slice/array
    /// closure-combinator method (`xs.sort_by(cmp)`, `xs.map(f)`), or
    /// `None` when the method is not such a combinator or the receiver is
    /// not a sequence. Comparator-shaped methods take two element
    /// parameters; the rest take one.
    fn vec_combinator_closure_inputs(&mut self, method: &str, receiver_ty: Ty) -> Option<Vec<Ty>> {
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        let elem = match self.tcx.kind(resolved) {
            Some(TyKind::Vec(elem) | TyKind::Slice(elem) | TyKind::Array { elem, .. }) => *elem,
            _ => return None,
        };
        match method {
            "sort_by" | "min_by" | "max_by" => Some(vec![elem, elem]),
            "sort_by_key" | "min_by_key" | "max_by_key" | "map" | "filter" | "filter_map"
            | "flat_map" | "for_each" | "any" | "all" | "find" | "position" | "find_map"
            | "take_while" | "skip_while" | "partition" | "group_by" | "count_by" | "sum_by"
            | "product_by" => Some(vec![elem]),
            _ => None,
        }
    }

    fn check_method_call(
        &mut self,
        call_id: NodeId,
        method: &str,
        receiver: &Expr,
        args: &[Expr],
    ) -> Ty {
        let receiver_ty = self.check_expr(receiver);
        // A method on a bound type-parameter receiver (`s.area()` where
        // `s: &T`, `T: Shape`) resolves to the trait method's declared
        // return type, so a `String`-returning trait method is not left to
        // default to i64 and render its pointer bits on the compiled tiers.
        if let Some(ret) = self.param_method_ret(receiver_ty, method) {
            for arg in args {
                self.check_expr(arg);
            }
            return ret;
        }
        if self.reject_supertrait_method_through_bound(receiver_ty, method, receiver.span) {
            for arg in args {
                self.check_expr(arg);
            }
            return self.tcx.error_ty();
        }
        if let Some(ty) = self.check_channel_method(method, receiver_ty, args) {
            return ty;
        }
        if let Some(ty) = self.check_weak_method(method, receiver_ty, args) {
            return ty;
        }
        // `x.into()` converts to an inferred target `B` via `B::from`, and
        // `x.try_into()` to `Result<B, E>` via `B::try_from`. The target is
        // fixed by the use site (a `let B` / `let Result<B, E>`, a parameter,
        // a return), so type it as a fresh variable here and let unification
        // bind it; lowering reads the resolved type and routes accordingly.
        if matches!(method, "into" | "try_into") && args.is_empty() {
            return self.fresh();
        }
        if let Some(ty) = self.check_deque_push_back(method, receiver_ty, receiver.span, args) {
            return ty;
        }
        // Result/Option combinator methods (`r.map_err(f)`,
        // `o.map(f)`) have known signatures: type them through the
        // std combinator table so closure params pin to the payload
        // type instead of falling through unresolved.
        if let Some(ty) =
            self.check_payload_combinator_method(method, receiver_ty, receiver.span, args)
        {
            return ty;
        }
        let candidates = self
            .method_arg_sigs
            .get(&(method.to_string(), args.len()))
            .cloned()
            .unwrap_or_default();
        // For a `Vec`/slice/array closure-combinator (`xs.sort_by(cmp)`,
        // `xs.map(f)`), the closure's parameters are the element type. Pin
        // them via an expectation so a field access in the closure body
        // resolves to the struct projection instead of the dynamic JSON path.
        let closure_combinator_inputs = self.vec_combinator_closure_inputs(method, receiver_ty);
        let mut arg_tys: Vec<Ty> = Vec::with_capacity(args.len());
        for (i, arg) in args.iter().enumerate() {
            // Shape literal arguments by the method's declared
            // parameter so `c.execute(&[V::I(1)])` builds a heap Vec,
            // matching the free-fn call path. Coerce only - no
            // unification: dispatch is name-global, so the coercion
            // target must be unambiguous across every same-named
            // method (non-container candidates are irrelevant - a
            // container literal cannot be meant for them).
            let exp = match (&closure_combinator_inputs, &arg.kind) {
                (Some(inputs), ExprKind::Closure { params, .. })
                    if params.len() == inputs.len() =>
                {
                    let output = self.fresh();
                    let sig = FnSig {
                        inputs: inputs.clone(),
                        output,
                    };
                    Expectation::HasType(self.tcx.intern(TyKind::FnPtr(sig)))
                }
                _ => match self.unique_container_expectation(&candidates, i) {
                    Some(want) => Expectation::Coerce(want),
                    None => Expectation::None,
                },
            };
            arg_tys.push(self.check_expr_expecting(arg, exp));
        }
        // When the receiver resolves to a non-generic Adt with a
        // recorded method return type, use it: a fresh var here
        // leaves chained results (`sel.params()`) untyped all the
        // way into codegen.
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        // `time::Duration` accessors in method form: `d.as_millis()`
        // mirrors the qualified `time::Duration::as_millis(d)` free
        // call, all of which yield a bare `i64`.
        if matches!(self.tcx.kind(resolved), Some(TyKind::Duration))
            && matches!(method, "as_millis" | "as_secs" | "as_micros")
            && args.is_empty()
        {
            return self.tcx.int_ty(IntTy::I64);
        }
        // `inst.elapsed_ms()` in method form mirrors the qualified
        // `time::Instant::elapsed_ms(inst)` free call; both yield `i64`.
        if matches!(self.tcx.kind(resolved), Some(TyKind::Instant))
            && method == "elapsed_ms"
            && args.is_empty()
        {
            return self.tcx.int_ty(IntTy::I64);
        }
        if let Some(TyKind::Adt { def, substs }) = self.tcx.kind(resolved)
            && substs.types().is_empty()
            && let Some(name) = self.tcx.def_name(*def)
            && let Some(&ret) =
                self.method_ret_types
                    .get(&(name.to_string(), method.to_string(), args.len()))
        {
            return ret;
        }
        if let Some(ty) = self.vec_method_ret(method, args, &arg_tys, resolved, receiver.span) {
            return ty;
        }
        if let Some(ty) = self.set_method_ret(method, resolved) {
            return ty;
        }
        if let Some(ty) = self.map_method_ret(method, resolved) {
            return ty;
        }
        if matches!(self.tcx.kind(resolved), Some(TyKind::String)) {
            // `s.contains(x)` dispatches to the same `strings::` shim as
            // the free function with the receiver as the implicit first
            // argument; validate the explicit args so an integer in a
            // string slot is rejected here too. Skipped under `|>`,
            // which appends the piped value as a trailing argument.
            if !self.pipe_stage_callees.contains(&call_id) {
                self.check_strings_method_call_args(method, args, &arg_tys);
            }
            return self.string_method_ret(method, receiver.span);
        }
        self.check_method_arity(call_id, resolved, method, args, receiver.span);
        self.maybe_reject_unknown_adt_method(resolved, method, receiver.span);
        self.fresh()
    }

    /// Rejects a method call whose argument count does not match the
    /// receiver method's declared arity. A call on the right of `|>`
    /// receives the piped value as an implicit trailing argument, so it
    /// is counted toward the supplied arity. Mirrors the free-call
    /// GT0018 check, which method calls never reached.
    fn check_method_arity(
        &mut self,
        call_id: NodeId,
        resolved: Ty,
        method: &str,
        args: &[Expr],
        span: Span,
    ) {
        let Some(TyKind::Adt { def, .. }) = self.tcx.kind(resolved) else {
            return;
        };
        let Some(name) = self.tcx.def_name(*def).map(str::to_string) else {
            return;
        };
        let Some(&expected) = self.method_arities.get(&(name.clone(), method.to_string())) else {
            return;
        };
        let pipe_extra = usize::from(self.pipe_stage_callees.contains(&call_id));
        let effective = args.len() + pipe_extra;
        if effective != expected {
            self.emit(
                TypeError::CallArityMismatch {
                    callee: format!("{name}::{method}"),
                    expected,
                    found: effective,
                },
                span,
            );
        }
    }

    /// Return type of a method on a `HashSet` receiver (sentinel `Adt`,
    /// def `u32::MAX - 7`). Without this the set-algebra methods are left
    /// a fresh `Var`, so iterating their result (`for e in a.union(&b)`)
    /// could not recover the set kind and read the handle as a vec.
    fn set_method_ret(&mut self, method: &str, resolved: Ty) -> Option<Ty> {
        let elem = match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, substs }) if def.local == u32::MAX - 7 => {
                substs.types().first().copied()
            }
            _ => return None,
        };
        match method {
            // New sets - same element type as the receiver.
            "union" | "intersection" | "difference" | "symmetric_difference" => Some(resolved),
            // Snapshot to a Vec of the element type.
            "to_vec" | "iter" => {
                let elem = elem.unwrap_or_else(|| self.tcx.string_ty());
                Some(self.tcx.intern(TyKind::Vec(elem)))
            }
            "insert" | "remove" | "contains" | "is_empty" | "is_subset" | "is_superset"
            | "is_disjoint" => Some(self.tcx.bool_ty()),
            "len" => Some(self.tcx.int_ty(IntTy::I64)),
            _ => None,
        }
    }

    /// Return type of a method on a `HashMap` / `BTreeMap` receiver whose
    /// result depends on the key/value types. Without this `m.iter()` is a
    /// fresh `Var`, so the for-vec lowering can't see the `(K, V)` element
    /// type and mis-sizes the element (especially when a destructure slot
    /// is `_`). Returns `None` for a non-map receiver so dispatch continues.
    fn map_method_ret(&mut self, method: &str, resolved: Ty) -> Option<Ty> {
        let (key, value) = match self.tcx.kind(resolved) {
            Some(TyKind::HashMap { key, value }) => (*key, *value),
            _ => return None,
        };
        match method {
            // `m.iter()` yields `(K, V)` pairs.
            "iter" => {
                let pair = self.tcx.intern(TyKind::Tuple(vec![key, value]));
                Some(self.tcx.intern(TyKind::Vec(pair)))
            }
            "keys" => Some(self.tcx.intern(TyKind::Vec(key))),
            "values" => Some(self.tcx.intern(TyKind::Vec(value))),
            "get" => Some(self.option_adt_ty(value)),
            "contains" | "contains_key" | "is_empty" => Some(self.tcx.bool_ty()),
            "len" => Some(self.tcx.int_ty(IntTy::I64)),
            _ => None,
        }
    }

    /// Return type of a method on a `Vec` / slice / fixed-array receiver
    /// whose result is a function of the element type. Without this the
    /// checker falls through to a fresh `Var`, so a chained `.first()` /
    /// `.index_of(..).map(..)` reaches codegen with an untyped payload and
    /// the native tier mis-represents it. Also checks the `push` / `insert`
    /// argument against the element type (a `[i64]` accepting a `String`
    /// pointer word is a silent memory hazard on the native backend).
    /// Returns `None` for a non-sequence receiver so dispatch continues.
    fn vec_method_ret(
        &mut self,
        method: &str,
        args: &[Expr],
        arg_tys: &[Ty],
        resolved: Ty,
        span: Span,
    ) -> Option<Ty> {
        let elem = match self.tcx.kind(resolved) {
            Some(TyKind::Vec(e) | TyKind::Slice(e)) => *e,
            Some(TyKind::Array { elem, .. }) => *elem,
            _ => return None,
        };
        // References are layout-transparent (the runtime owns memory), so
        // peel them before comparing the pushed element to the slot type.
        let push_arg = match (method, args.len()) {
            ("push", 1) => arg_tys.first().copied(),
            ("insert", 2) => arg_tys.get(1).copied(),
            _ => None,
        };
        if let Some(arg_ty) = push_arg {
            let elem_peeled = self.peel_refs(elem);
            let arg_peeled = self.peel_refs(arg_ty);
            self.unify(elem_peeled, arg_peeled, span);
        }
        match (method, args.len()) {
            ("first" | "last", 0) => Some(self.option_adt_ty(elem)),
            ("reversed" | "to_vec", 0) => Some(self.tcx.intern(TyKind::Vec(elem))),
            ("index_of", 1) => {
                let i = self.tcx.int_ty(IntTy::I64);
                Some(self.option_adt_ty(i))
            }
            ("count_of", 1) => Some(self.tcx.int_ty(IntTy::I64)),
            ("contains", 1) => Some(self.tcx.bool_ty()),
            ("slice", 2) => {
                let vec = self.tcx.intern(TyKind::Vec(elem));
                let err = self.tcx.dyn_error_ty();
                Some(self.result_adt_ty(vec, err))
            }
            _ => None,
        }
    }

    /// Return type of a method on a `String` receiver, with precise types
    /// for the commonly-chained methods (so `s.rfind(&"/").map(|i| i as
    /// i64)` types the `Option<i64>` payload rather than leaving it an
    /// untyped var the native tier mis-represents - the P0-4 shape).
    /// A method outside the `String` surface is the name-global dispatch
    /// leak (a `unicode::*` char predicate like `"abc".is_letter()`, or a
    /// typo like `"abc".bogus()`): it runs the wrong global body on the
    /// VM and fails to lower native, so it is rejected (P0-6).
    fn string_method_ret(&mut self, method: &str, span: Span) -> Ty {
        match method {
            "split" | "splitn" | "split_whitespace" | "lines" => {
                let s = self.tcx.string_ty();
                self.tcx.intern(TyKind::Vec(s))
            }
            "chars" => {
                let c = self.tcx.intern(TyKind::Char);
                self.tcx.intern(TyKind::Vec(c))
            }
            // `Option<i64>` byte offsets - the P0-4 shape.
            "find" | "rfind" | "find_any" | "rfind_any" | "index_rune" => {
                let i = self.tcx.int_ty(IntTy::I64);
                self.option_adt_ty(i)
            }
            "contains" | "contains_any" | "contains_rune" | "starts_with" | "ends_with"
            | "equal_fold" | "is_empty" => self.tcx.bool_ty(),
            "len" | "count" | "byte_at" | "byte_len" => self.tcx.int_ty(IntTy::I64),
            "clone" | "to_string" => self.tcx.string_ty(),
            // Methods that return a fresh `String` (runtime `*mut c_char`):
            // pinning the result type so chained calls (`s.trim().len()`) and
            // typed bindings lower from a known type instead of an inference
            // var carrying an untyped heap payload into MIR.
            "trim" | "trim_start" | "trim_end" | "trim_matches" | "trim_start_matches"
            | "trim_end_matches" | "to_upper" | "to_lower" | "to_uppercase" | "to_lowercase"
            | "to_title" | "replace" | "replacen" | "repeat" | "pad_left" | "pad_right"
            | "center" | "substring" => self.tcx.string_ty(),
            // `as_bytes` -> `[u8]` (runtime `*mut GosVec` of bytes).
            "as_bytes" => {
                let u8_ty = self.tcx.int_ty(IntTy::U8);
                self.tcx.intern(TyKind::Vec(u8_ty))
            }
            // `split_once` / `rsplit_once` -> `Option<(String, String)>`.
            "split_once" | "rsplit_once" => {
                let s = self.tcx.string_ty();
                let pair = self.tcx.intern(TyKind::Tuple(vec![s, s]));
                self.option_adt_ty(pair)
            }
            // `strip_prefix` / `strip_suffix` -> `Option<String>`.
            "strip_prefix" | "strip_suffix" => {
                let s = self.tcx.string_ty();
                self.option_adt_ty(s)
            }
            // `slice(a, b) -> Result<String, errors::Error>` (out-of-range Err).
            "slice" => {
                let s = self.tcx.string_ty();
                let e = self.tcx.dyn_error_ty();
                self.result_adt_ty(s, e)
            }
            // `s.parse()` -> `Result<T, errors::Error>`: the value type T is
            // inferred from the binding, but the error pins to the concrete
            // error type so `{}` Display of an `Err` lowers correctly on the
            // compiled tier (an unresolved error var rendered a garbage char).
            "parse" => {
                let t = self.fresh();
                let e = self.tcx.dyn_error_ty();
                self.result_adt_ty(t, e)
            }
            _ if is_string_method(method) => self.fresh(),
            _ => {
                self.emit(
                    TypeError::UnresolvedMethod {
                        ty: "String".to_string(),
                        name: method.to_string(),
                    },
                    span,
                );
                self.tcx.error_ty()
            }
        }
    }

    /// Rejects a method call on a concrete user struct / enum receiver
    /// when the method demonstrably belongs to a *different* user type -
    /// the name-global dispatch soundness hole (P0-6: `b.label()` runs
    /// `A`'s body against `B`'s memory on the VM and fails to lower on
    /// the native tier). Conservative on purpose: it fires only when the
    /// method name is owned by some user type but not this one, so an
    /// unknown method (a genuine typo with no owner anywhere) or a
    /// builtin / derived method (`clone`) still falls through.
    /// Re-validates deferred structural uses (`value[i]` / `value(args)`
    /// / `value.N`) after integer/float defaulting has given unsuffixed
    /// literals their concrete type. An operand that resolved to a
    /// concrete non-indexable / non-callable / non-tuple type is
    /// rejected here, so `let x = 5; x[0]` - whose `x` was an inference
    /// var at first check - is caught instead of faulting on the
    /// compiled tier.
    fn check_deferred_structural(&mut self) {
        let deferred = std::mem::take(&mut self.deferred_structural);
        for d in deferred {
            let mut resolved = self.infer.resolve(self.tcx, d.ty);
            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(resolved).clone() {
                resolved = self.infer.resolve(self.tcx, inner);
            }
            let kind = self.tcx.kind_of(resolved).clone();
            match d.kind {
                DeferredStructuralKind::Index => {
                    let indexable = matches!(
                        kind,
                        TyKind::Array { .. } | TyKind::Slice(_) | TyKind::Vec(_) | TyKind::String
                    );
                    if !indexable && !is_soft_for_structural_use(&kind) {
                        let ty = render_ty(self.tcx, resolved);
                        self.emit(TypeError::NotIndexable { ty }, d.span);
                    }
                }
                DeferredStructuralKind::Call => {
                    if is_definitely_not_callable_value(&kind) {
                        let ty = render_ty(self.tcx, resolved);
                        self.emit(TypeError::NotCallable { ty }, d.span);
                    }
                }
                DeferredStructuralKind::TupleField(idx) => match &kind {
                    TyKind::Tuple(elems) => {
                        if idx as usize >= elems.len() {
                            let ty = render_ty(self.tcx, resolved);
                            self.emit(TypeError::NoTupleField { ty, index: idx }, d.span);
                        }
                    }
                    other => {
                        let is_tuple_struct = u32::try_from(idx)
                            .ok()
                            .is_some_and(|i| self.tuple_struct_field_ty(resolved, i).is_some());
                        if !is_tuple_struct && !is_soft_for_structural_use(other) {
                            let ty = render_ty(self.tcx, resolved);
                            self.emit(TypeError::NoTupleField { ty, index: idx }, d.span);
                        }
                    }
                },
            }
        }
    }

    fn maybe_reject_unknown_adt_method(&mut self, resolved: Ty, method: &str, span: Span) {
        if matches!(method, "clone") {
            return;
        }
        let Some(TyKind::Adt { def, .. }) = self.tcx.kind(resolved) else {
            return;
        };
        let Some(name) = self.tcx.def_name(*def).map(str::to_string) else {
            return;
        };
        // Only genuine user-declared struct / enum receivers: sentinel
        // Adts (Result / Option / http::Response / VecDeque) are not in
        // `user_type_decls`.
        if !self.user_type_decls.contains(&name) {
            return;
        }
        // `user_method_owners` records every impl and trait method, so a
        // method name that no user type owns is a genuine typo /
        // nonexistent method on a concrete user receiver. Previously this
        // case returned early (only a name owned by *another* type was
        // rejected), so a typo passed `check` and the compiled tier
        // failed to build with an undefined `@Type::method` symbol.
        let owned_here = self
            .user_method_owners
            .get(method)
            .is_some_and(|owners| owners.contains(&name));
        if !owned_here {
            self.emit(
                TypeError::UnresolvedMethod {
                    ty: name,
                    name: method.to_string(),
                },
                span,
            );
        }
    }

    /// The single container-shaped (`Vec` / `Slice` / `Tuple`, ref
    /// transparent) parameter type at position `i` across the
    /// candidate signatures, or `None` when absent or ambiguous.
    fn unique_container_expectation(&mut self, candidates: &[Vec<Ty>], i: usize) -> Option<Ty> {
        let mut found: Option<(Ty, String)> = None;
        for sig in candidates {
            let Some(&ty) = sig.get(i) else { continue };
            let mut peeled = self.infer.resolve(self.tcx, ty);
            while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(peeled) {
                peeled = self.infer.resolve(self.tcx, *inner);
            }
            if !matches!(
                self.tcx.kind(peeled),
                Some(TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Tuple(_))
            ) {
                continue;
            }
            let rendered = render_ty(self.tcx, ty);
            match &found {
                Some((_, existing)) if *existing == rendered => {}
                Some(_) => return None,
                None => found = Some((ty, rendered)),
            }
        }
        found.map(|(ty, _)| ty)
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

    /// Full argument arity (closure/seed args plus the trailing data
    /// arg) of a std data-last combinator the checker can type, or
    /// `None` for names it has no signature row for.
    fn std_combinator_arity(module: &str, name: &str) -> Option<usize> {
        let arity = match (module, name) {
            ("result", "map" | "map_err" | "and_then" | "or_else" | "default" | "default_with") => {
                2
            }
            ("result", "ok" | "err" | "is_ok" | "is_err") => 1,
            (
                "option",
                "map" | "and_then" | "filter" | "or" | "or_else" | "default" | "default_with"
                | "zip",
            ) => 2,
            ("option", "flatten" | "is_some" | "is_none" | "iter") => 1,
            ("iter", "fold" | "scan") => 3,
            (
                "iter",
                "for_each" | "map" | "filter" | "filter_map" | "flat_map" | "reduce" | "sum_by"
                | "product_by" | "any" | "all" | "find" | "position" | "find_map" | "take_while"
                | "skip_while" | "partition" | "sort_by" | "sort_by_key" | "min_by" | "max_by"
                | "min_by_key" | "max_by_key" | "group_by" | "count_by",
            ) => 2,
            _ => return None,
        };
        Some(arity)
    }

    /// `(ok, err)` payload types of a Result-shaped `ty`. A still-free
    /// inference var is unified with a fresh `Result<?, ?>` so the
    /// payload slots exist for the combinator row to pin against.
    fn result_payload_tys(&mut self, ty: Ty, span: Span) -> Option<(Ty, Ty)> {
        let mut resolved = self.infer.resolve(self.tcx, ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, substs }) if def.local == u32::MAX => {
                let tys = substs.types();
                Some((tys.first().copied()?, tys.get(1).copied()?))
            }
            Some(TyKind::Var(_)) => {
                let ok = self.fresh();
                let err = self.fresh();
                let shaped = self.result_adt_ty(ok, err);
                self.unify(resolved, shaped, span);
                Some((ok, err))
            }
            _ => None,
        }
    }

    /// Payload type of an Option-shaped `ty`, unifying a free var
    /// with `Option<?>` the same way as [`Self::result_payload_tys`].
    fn option_payload_ty(&mut self, ty: Ty, span: Span) -> Option<Ty> {
        let mut resolved = self.infer.resolve(self.tcx, ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, substs }) if def.local == u32::MAX - 1 => {
                substs.types().first().copied()
            }
            Some(TyKind::Var(_)) => {
                let payload = self.fresh();
                let shaped = self.option_adt_ty(payload);
                self.unify(resolved, shaped, span);
                Some(payload)
            }
            _ => None,
        }
    }

    /// Element type of a sequence-shaped `ty` (`Vec` / slice / fixed
    /// array, ref-transparent), unifying a free var with `Vec<?>`.
    fn sequence_elem_ty(&mut self, ty: Ty, span: Span) -> Option<Ty> {
        let mut resolved = self.infer.resolve(self.tcx, ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        match self.tcx.kind(resolved) {
            Some(TyKind::Vec(elem) | TyKind::Slice(elem) | TyKind::Array { elem, .. }) => {
                Some(*elem)
            }
            Some(TyKind::Var(_)) => {
                let elem = self.fresh();
                let shaped = self.tcx.intern(TyKind::Vec(elem));
                self.unify(resolved, shaped, span);
                Some(elem)
            }
            _ => None,
        }
    }

    /// Pins a callable argument's parameter types to `inputs` and
    /// returns its output type. This is the load-bearing step for
    /// lifted closures: binding the param inference vars here is what
    /// keeps the HIR lift pass from pinning an unresolved String/Error
    /// param to i64 (which renders the payload as a raw pointer on the
    /// compiled tiers).
    fn callable_output(&mut self, callable_ty: Ty, inputs: &[Ty], span: Span) -> Ty {
        let resolved = self.infer.resolve(self.tcx, callable_ty);
        match self.tcx.kind(resolved).cloned() {
            Some(TyKind::FnPtr(sig) | TyKind::FnTrait(sig)) => {
                if sig.inputs.len() == inputs.len() {
                    for (have, want) in sig.inputs.iter().zip(inputs) {
                        self.unify(*have, *want, span);
                    }
                }
                sig.output
            }
            Some(TyKind::FnDef { def, .. }) => match self.fn_sigs.get(&def).cloned() {
                Some(sig) => {
                    if sig.inputs.len() == inputs.len() {
                        for (have, want) in sig.inputs.iter().zip(inputs) {
                            self.unify(*have, *want, span);
                        }
                    }
                    sig.output
                }
                None => self.fresh(),
            },
            Some(TyKind::Var(_)) => {
                let output = self.fresh();
                let shaped = self.tcx.intern(TyKind::FnPtr(FnSig {
                    inputs: inputs.to_vec(),
                    output,
                }));
                self.unify(resolved, shaped, span);
                output
            }
            _ => self.fresh(),
        }
    }

    /// Resolves `ty` to a Result if possible: an already-Result type
    /// is returned as-is, a free var is unified with
    /// `Result<ok, err>`, anything else degrades to a fresh var.
    fn shape_result_like(&mut self, ty: Ty, ok: Ty, err: Ty, span: Span) -> Ty {
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX => resolved,
            Some(TyKind::Var(_)) => {
                let shaped = self.result_adt_ty(ok, err);
                self.unify(resolved, shaped, span);
                shaped
            }
            _ => self.fresh(),
        }
    }

    /// Option-shaped counterpart of [`Self::shape_result_like`].
    fn shape_option_like(&mut self, ty: Ty, payload: Ty, span: Span) -> Ty {
        let resolved = self.infer.resolve(self.tcx, ty);
        match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX - 1 => resolved,
            Some(TyKind::Var(_)) => {
                let shaped = self.option_adt_ty(payload);
                self.unify(resolved, shaped, span);
                shaped
            }
            _ => self.fresh(),
        }
    }

    /// Return type of a known std data-last combinator call
    /// (`result::*` / `option::*` / closure-taking `iter::*`, free,
    /// piped, or method form). `lead_tys` are the leading closure /
    /// seed argument types; `data_ty` is the trailing data argument
    /// (the method receiver, or the piped value). Unifies closure
    /// parameter vars with the data payload types so lifted closure
    /// bodies keep String/Error params instead of the i64 pin.
    #[allow(
        clippy::too_many_lines,
        reason = "one row per std combinator; splitting the table would obscure the signature catalog"
    )]
    fn std_combinator_ty(
        &mut self,
        module: &str,
        name: &str,
        lead_tys: &[Ty],
        data_ty: Ty,
        span: Span,
    ) -> Option<Ty> {
        if Self::std_combinator_arity(module, name)? != lead_tys.len() + 1 {
            return None;
        }
        match module {
            "result" => {
                let (ok, err) = self.result_payload_tys(data_ty, span)?;
                let ty = match name {
                    "map" => {
                        let mapped = self.callable_output(lead_tys[0], &[ok], span);
                        self.result_adt_ty(mapped, err)
                    }
                    "map_err" => {
                        let mapped = self.callable_output(lead_tys[0], &[err], span);
                        self.result_adt_ty(ok, mapped)
                    }
                    "and_then" => {
                        let out = self.callable_output(lead_tys[0], &[ok], span);
                        let next_ok = self.fresh();
                        self.shape_result_like(out, next_ok, err, span)
                    }
                    "or_else" => {
                        let out = self.callable_output(lead_tys[0], &[err], span);
                        let next_err = self.fresh();
                        self.shape_result_like(out, ok, next_err, span)
                    }
                    "default" => {
                        self.unify(ok, lead_tys[0], span);
                        ok
                    }
                    // The Ok payload and the handler's return mix at
                    // runtime (`Ok(v)` yields `v`, `Err(e)` yields
                    // `f(e)`), and the dominant shape is a discarded
                    // call with a unit handler - pin only the param
                    // and leave the result free.
                    "default_with" => {
                        let _ = self.callable_output(lead_tys[0], &[err], span);
                        self.fresh()
                    }
                    "ok" => self.option_adt_ty(ok),
                    "err" => self.option_adt_ty(err),
                    "is_ok" | "is_err" => self.tcx.bool_ty(),
                    _ => return None,
                };
                Some(ty)
            }
            "option" => {
                let payload = self.option_payload_ty(data_ty, span)?;
                let ty = match name {
                    "map" => {
                        let mapped = self.callable_output(lead_tys[0], &[payload], span);
                        self.option_adt_ty(mapped)
                    }
                    "and_then" => {
                        let out = self.callable_output(lead_tys[0], &[payload], span);
                        let next = self.fresh();
                        self.shape_option_like(out, next, span)
                    }
                    "filter" => {
                        let out = self.callable_output(lead_tys[0], &[payload], span);
                        let bool_ty = self.tcx.bool_ty();
                        self.unify(bool_ty, out, span);
                        self.option_adt_ty(payload)
                    }
                    "or" => {
                        let shaped = self.option_adt_ty(payload);
                        self.unify(shaped, lead_tys[0], span);
                        shaped
                    }
                    "or_else" => {
                        let out = self.callable_output(lead_tys[0], &[], span);
                        self.shape_option_like(out, payload, span)
                    }
                    "default" => {
                        self.unify(payload, lead_tys[0], span);
                        payload
                    }
                    // Same mixed-type rationale as the Result row.
                    "default_with" => {
                        let _ = self.callable_output(lead_tys[0], &[], span);
                        self.fresh()
                    }
                    "zip" => {
                        let other = match self.option_payload_ty(lead_tys[0], span) {
                            Some(other) => other,
                            None => self.fresh(),
                        };
                        let pair = self.tcx.intern(TyKind::Tuple(vec![payload, other]));
                        self.option_adt_ty(pair)
                    }
                    "flatten" => {
                        let inner = self.fresh();
                        self.shape_option_like(payload, inner, span)
                    }
                    "is_some" | "is_none" => self.tcx.bool_ty(),
                    "iter" => self.tcx.intern(TyKind::Vec(payload)),
                    _ => return None,
                };
                Some(ty)
            }
            "iter" => {
                let elem = self.sequence_elem_ty(data_ty, span)?;
                let bool_ty = self.tcx.bool_ty();
                let ty = match name {
                    "for_each" => {
                        let _ = self.callable_output(lead_tys[0], &[elem], span);
                        self.tcx.unit()
                    }
                    "map" => {
                        let mapped = self.callable_output(lead_tys[0], &[elem], span);
                        self.tcx.intern(TyKind::Vec(mapped))
                    }
                    "filter" | "take_while" | "skip_while" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        self.unify(bool_ty, out, span);
                        self.tcx.intern(TyKind::Vec(elem))
                    }
                    "filter_map" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        let mapped = match self.option_payload_ty(out, span) {
                            Some(payload) => payload,
                            None => self.fresh(),
                        };
                        self.tcx.intern(TyKind::Vec(mapped))
                    }
                    "flat_map" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        let mapped = self.sequence_elem_ty(out, span).unwrap_or_else(|| {
                            // Non-sequence closure output is a real
                            // bug, but the runtime flattens anything;
                            // degrade to fresh instead of erroring.
                            self.fresh()
                        });
                        self.tcx.intern(TyKind::Vec(mapped))
                    }
                    "fold" | "scan" => {
                        let acc = lead_tys[0];
                        let out = self.callable_output(lead_tys[1], &[acc, elem], span);
                        self.unify(acc, out, span);
                        if name == "fold" {
                            acc
                        } else {
                            self.tcx.intern(TyKind::Vec(acc))
                        }
                    }
                    "reduce" => {
                        let out = self.callable_output(lead_tys[0], &[elem, elem], span);
                        self.unify(elem, out, span);
                        self.option_adt_ty(elem)
                    }
                    "sum_by" | "product_by" => self.callable_output(lead_tys[0], &[elem], span),
                    "any" | "all" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        self.unify(bool_ty, out, span);
                        bool_ty
                    }
                    "find" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        self.unify(bool_ty, out, span);
                        self.option_adt_ty(elem)
                    }
                    "position" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        self.unify(bool_ty, out, span);
                        let i64_ty = self.tcx.int_ty(IntTy::I64);
                        self.option_adt_ty(i64_ty)
                    }
                    "find_map" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        let mapped = match self.option_payload_ty(out, span) {
                            Some(payload) => payload,
                            None => self.fresh(),
                        };
                        self.option_adt_ty(mapped)
                    }
                    "partition" => {
                        let out = self.callable_output(lead_tys[0], &[elem], span);
                        self.unify(bool_ty, out, span);
                        let vec_ty = self.tcx.intern(TyKind::Vec(elem));
                        self.tcx.intern(TyKind::Tuple(vec![vec_ty, vec_ty]))
                    }
                    // Comparator output is an ordering integer of any
                    // width; the row pins only the element params.
                    "sort_by" => {
                        let _ = self.callable_output(lead_tys[0], &[elem, elem], span);
                        self.tcx.intern(TyKind::Vec(elem))
                    }
                    "sort_by_key" => {
                        let _ = self.callable_output(lead_tys[0], &[elem], span);
                        self.tcx.intern(TyKind::Vec(elem))
                    }
                    "min_by" | "max_by" => {
                        let _ = self.callable_output(lead_tys[0], &[elem, elem], span);
                        self.option_adt_ty(elem)
                    }
                    "min_by_key" | "max_by_key" => {
                        let _ = self.callable_output(lead_tys[0], &[elem], span);
                        self.option_adt_ty(elem)
                    }
                    "group_by" => {
                        let key = self.callable_output(lead_tys[0], &[elem], span);
                        let value = self.tcx.intern(TyKind::Vec(elem));
                        self.tcx.intern(TyKind::HashMap { key, value })
                    }
                    "count_by" => {
                        let key = self.callable_output(lead_tys[0], &[elem], span);
                        let value = self.tcx.int_ty(IntTy::I64);
                        self.tcx.intern(TyKind::HashMap { key, value })
                    }
                    _ => return None,
                };
                Some(ty)
            }
            _ => None,
        }
    }

    /// Types a full-arity std combinator free call (`result::*` /
    /// `option::*` / `iter::*` data-last forms), or emits the loud
    /// uninferrable-closure error when the name has no signature row
    /// but a closure argument is present. `None` falls back to the
    /// existing stdlib heuristics (including partial applications,
    /// which the pipe site completes).
    fn check_std_combinator_free_call(
        &mut self,
        callee: &Expr,
        args: &[Expr],
        arg_tys: &[Ty],
        module: Option<&'static str>,
        name: &str,
    ) -> Option<Ty> {
        let module = module?;
        match Self::std_combinator_arity(module, name) {
            Some(arity) if args.len() == arity => {
                let (lead, data) = arg_tys.split_at(arity - 1);
                let lead = lead.to_vec();
                let span = args.last().map_or(callee.span, |arg| arg.span);
                self.std_combinator_ty(module, name, &lead, data[0], span)
            }
            // Partial application (`xs |> iter::map(f)`) is completed
            // at the pipe site, where the data argument's type is
            // known.
            Some(_) => None,
            // A std combinator the checker has no signature row for
            // cannot type its closure argument; the compiled tiers
            // would pin the param to i64 and print String payloads as
            // pointers. Reject loudly instead.
            None => {
                if args
                    .iter()
                    .any(|arg| matches!(arg.kind, ExprKind::Closure { .. }))
                {
                    self.emit(
                        TypeError::ClosureParamUninferred {
                            combinator: format!("{module}::{name}"),
                        },
                        callee.span,
                    );
                    return Some(self.tcx.error_ty());
                }
                None
            }
        }
    }

    /// Types Result/Option combinator *method* calls
    /// (`r.map_err(f)`, `o.map(f)`) through the same signature table
    /// as the free `result::*` / `option::*` functions, with the
    /// receiver as the data argument. Returns `None` (leaving the
    /// generic method path untouched) when the receiver is not a
    /// resolved Result/Option or the name has no row.
    fn check_payload_combinator_method(
        &mut self,
        method: &str,
        receiver_ty: Ty,
        receiver_span: Span,
        args: &[Expr],
    ) -> Option<Ty> {
        let mut resolved = self.infer.resolve(self.tcx, receiver_ty);
        while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(resolved) {
            resolved = self.infer.resolve(self.tcx, *inner);
        }
        let module = match self.tcx.kind(resolved) {
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX => "result",
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX - 1 => "option",
            _ => return None,
        };
        if Self::std_combinator_arity(module, method)? != args.len() + 1 {
            return None;
        }
        let span = args.first().map_or(receiver_span, |arg| arg.span);
        let lead_tys: Vec<Ty> = args.iter().map(|arg| self.check_expr(arg)).collect();
        self.std_combinator_ty(module, method, &lead_tys, resolved, span)
    }

    fn check_unary(
        &mut self,
        op: UnaryOp,
        operand: &Expr,
        span: Span,
        expected: Expectation,
    ) -> Ty {
        // A borrow is layout-transparent: `&[..]` against an expected
        // `&[T]` (or bare `[T]`) shapes the borrowed literal itself.
        // `expectation_target` already peels one `Ref`, so the operand
        // inherits the peeled target at the same strength.
        let operand_expected = match op {
            UnaryOp::RefShared | UnaryOp::RefMut => match self.expectation_target(expected) {
                Some(target) => expected.rewrap(target),
                None => Expectation::None,
            },
            _ => Expectation::None,
        };
        let operand_ty = self.check_expr_expecting(operand, operand_expected);
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
            UnaryOp::Neg => {
                // `-x` on a user struct / enum routes to its `neg` impl
                // (a zero-arg method on the receiver); the result is that
                // method's return type. Scalars keep the operand type.
                if let Some(adt) = self.adt_name_of(resolved)
                    && let Some(&ret) = self.method_ret_types.get(&(adt, "neg".to_string(), 0))
                {
                    ret
                } else {
                    operand_ty
                }
            }
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
                // to dereference the value as a pointer - segv.
                let resolved = self.infer.resolve(self.tcx, operand_ty);
                match self.tcx.kind(resolved) {
                    Some(TyKind::Ref { inner, .. }) => *inner,
                    _ => operand_ty,
                }
            }
        }
    }

    /// If `expr` is a tuple-variant constructor call (`E::B(1)`), the `Adt`
    /// type of its enum. Used to anchor comparison operands - the constructor's
    /// result is otherwise a fresh variable unless used at a typed site, which
    /// leaves an inline `E::B(1) < E::B(2)` undispatchable. Scoped to the
    /// comparison arm so it does not retype constructors feeding a `let`
    /// destructure (whose compiled-tier payload extraction wants the bare form).
    fn variant_ctor_enum_ty(&self, expr: &Expr) -> Option<Ty> {
        let ExprKind::Call { callee, .. } = &expr.kind else {
            return None;
        };
        let ExprKind::Path(path) = &callee.kind else {
            return None;
        };
        let n = path.segments.len();
        if n < 2 {
            return None;
        }
        let enum_name = path.segments[n - 2].name.name.as_str();
        let var_name = path.segments[n - 1].name.name.as_str();
        if self
            .enum_variants
            .get(enum_name)
            .is_some_and(|vs| vs.contains(var_name))
        {
            self.enum_tys.get(enum_name).copied()
        } else {
            None
        }
    }

    fn check_binary(&mut self, op: BinaryOp, lhs: &Expr, rhs: &Expr, span: Span) -> Ty {
        // The rhs of `|>` is a callee position: a bare std path there
        // (`x |> strings::to_upper`) is the partial-application call
        // shape, not a first-class fn value.
        if op == BinaryOp::PipeGt && matches!(rhs.kind, ExprKind::Path(_)) {
            self.callee_path_nodes.insert(rhs.id);
        }
        // `x |> f(a)` desugars to `f(a, x)`: the call on the right gets
        // the piped value appended as its last argument during lowering,
        // so its declared arity is satisfied by one fewer explicit
        // argument here. Record the callee so the arity check accounts
        // for the implicit piped argument.
        if op == BinaryOp::PipeGt
            && let ExprKind::Call { callee, .. } = &rhs.kind
        {
            self.pipe_stage_callees.insert(callee.id);
        }
        // `x |> recv.m(a)` desugars to `recv.m(a, x)`: the piped value
        // lands as the method's last argument during lowering, so the
        // declared arity is satisfied by one fewer explicit argument
        // here. Record the call node so the arity check adds it back.
        if op == BinaryOp::PipeGt && matches!(rhs.kind, ExprKind::MethodCall { .. }) {
            self.pipe_stage_callees.insert(rhs.id);
        }
        let lhs_ty = self.check_expr(lhs);
        let rhs_ty = self.check_expr(rhs);
        match op {
            BinaryOp::Eq
            | BinaryOp::Ne
            | BinaryOp::Lt
            | BinaryOp::Le
            | BinaryOp::Gt
            | BinaryOp::Ge => {
                // Anchor a variant-constructor operand to its enum so an inline
                // same-variant comparison (`E::B(1) < E::B(2)`) can dispatch -
                // both sides are otherwise fresh variables.
                let lhs_ty = if let Some(e) = self.variant_ctor_enum_ty(lhs) {
                    self.record(lhs.id, e);
                    e
                } else {
                    lhs_ty
                };
                let rhs_ty = if let Some(e) = self.variant_ctor_enum_ty(rhs) {
                    self.record(rhs.id, e);
                    e
                } else {
                    rhs_ty
                };
                // A byte literal compares against any integer operand without
                // an explicit `as i64`: `s[i] == b'>'`. A byte literal is an
                // `Int` value on every tier, so re-typing its node to the
                // integer operand's type lets the comparison flow unchanged.
                if !self.coerce_byte_literal_cmp(lhs, lhs_ty, rhs, rhs_ty) {
                    self.unify(lhs_ty, rhs_ty, span);
                }
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
                // String concatenation accepts a borrowed RHS:
                // `"hello, " + &name` (the documented spelling). Peel
                // the reference before unifying so the expression
                // stays `String` instead of failing `String != &T`.
                if op == BinaryOp::Add {
                    let l = self.infer.resolve(self.tcx, lhs_ty);
                    if matches!(self.tcx.kind_of(l), TyKind::String) {
                        let r = self.infer.resolve(self.tcx, rhs_ty);
                        if let TyKind::Ref { inner, .. } = self.tcx.kind_of(r) {
                            let inner = *inner;
                            self.unify(lhs_ty, inner, span);
                            return lhs_ty;
                        }
                    }
                }
                // Arithmetic on a user struct/enum routes to its operator
                // impl (`+` -> `add`, `-` -> `sub`, `*` -> `mul`, `/` ->
                // `div`); the result is that method's return type. An ADT
                // operand with no such impl is rejected here rather than
                // miscompiling to a runtime "unsupported value kinds" error.
                if let Some(method) = arith_op_method(op)
                    && let Some(adt) = self
                        .adt_name_of(lhs_ty)
                        .or_else(|| self.adt_name_of(rhs_ty))
                {
                    if let Some(&ret) =
                        self.method_ret_types
                            .get(&(adt.clone(), method.to_string(), 1))
                    {
                        return ret;
                    }
                    self.emit(
                        TypeError::UnresolvedOp {
                            op: op.as_str().to_string(),
                            lhs: render_ty(self.tcx, self.infer.resolve(self.tcx, lhs_ty)),
                            rhs: render_ty(self.tcx, self.infer.resolve(self.tcx, rhs_ty)),
                        },
                        span,
                    );
                    return self.tcx.error_ty();
                }
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
        // Data-last std combinators, partially applied through the
        // pipe (`xs |> iter::map(f)`, `r |> result::map_err(f)`,
        // `r |> result::ok`): the piped value is the data argument,
        // so its payload types pin the closure params here.
        let combinator: Option<(&gossamer_ast::PathExpr, &[Expr])> = match &rhs.kind {
            ExprKind::Call { callee, args } => match &callee.kind {
                // A callee that resolved to a user `FnDef` keeps its
                // own typing even under a std-module-shaped name.
                ExprKind::Path(path)
                    if !matches!(
                        self.table
                            .get(callee.id)
                            .map(|t| self.tcx.kind_of(t).clone()),
                        Some(TyKind::FnDef { .. })
                    ) =>
                {
                    Some((path, args.as_slice()))
                }
                _ => None,
            },
            ExprKind::Path(path) => Some((path, &[])),
            _ => None,
        };
        if let Some((path, lead_args)) = combinator {
            let names: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
            let (module, last) = names.split_at(names.len().saturating_sub(1));
            let comb = combinator_module_name(module);
            if let (Some(comb), Some(&last)) = (comb, last.first()) {
                if Self::std_combinator_arity(comb, last) == Some(lead_args.len() + 1) {
                    let lead_tys: Vec<Ty> = lead_args
                        .iter()
                        .map(|arg| {
                            self.table
                                .get(arg.id)
                                .unwrap_or_else(|| self.tcx.error_ty())
                        })
                        .collect();
                    if let Some(ret) =
                        self.std_combinator_ty(comb, last, &lead_tys, lhs_ty, lhs_span)
                    {
                        return ret;
                    }
                }
            }
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

    fn check_assign(&mut self, place: &Expr, value: &Expr, op: gossamer_ast::AssignOp) -> Ty {
        let place_ty = self.check_expr(place);
        // The place's type flows into the value as its expectation so
        // `v = [2, 3]` against a `Vec<i64>` slot lays a heap Vec, not
        // a fixed `[i64; 2]` desynced from the slot's layout.
        let value_ty = self.check_expr_expecting(value, Expectation::HasType(place_ty));
        // `s += &t` / `s += &str`: String append accepts a borrowed operand
        // on the right, mirroring the `+` concatenation operator. Only the
        // compound `+=` form relaxes; plain `=` still requires an owned String.
        if matches!(op, gossamer_ast::AssignOp::AddAssign) {
            let pr = self.infer.resolve(self.tcx, place_ty);
            if matches!(self.tcx.kind(pr), Some(TyKind::String)) {
                let mut vr = self.infer.resolve(self.tcx, value_ty);
                while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(vr) {
                    vr = self.infer.resolve(self.tcx, *inner);
                }
                if matches!(self.tcx.kind(vr), Some(TyKind::String)) {
                    return self.tcx.unit();
                }
            }
        }
        self.unify(place_ty, value_ty, value.span);
        self.tcx.unit()
    }

    /// Validates an `as` cast against the whitelist of permitted
    /// conversions: numeric ↔ numeric, `bool`/`char` → integer,
    /// `u8` → `char`, and same-type no-ops. Matches Rust's RFC 401.
    /// Fails soft when either side is still an inference variable -
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

    fn check_if(
        &mut self,
        condition: &Expr,
        then_branch: &Expr,
        else_branch: Option<&Expr>,
        expected: Expectation,
    ) -> Ty {
        let cond_ty = self.check_expr(condition);
        let bool_ty = self.tcx.bool_ty();
        self.unify(bool_ty, cond_ty, condition.span);
        let then_ty = self.check_expr_expecting(then_branch, expected);
        if let Some(else_branch) = else_branch {
            let else_ty = self.check_expr_expecting(else_branch, expected);
            let joined = self.join_branch_tys(then_ty, else_ty, else_branch.span);
            // When the branches joined to a Vec/slice, re-record each
            // array-literal branch to that shape so an unannotated
            // `let v = if c { [1, 2] } else { [3, 4, 5] }` lowers both
            // arms as a heap Vec, matching the joined result slot.
            self.adjust_literal_to_join(then_branch, joined);
            self.adjust_literal_to_join(else_branch, joined);
            joined
        } else {
            self.tcx.unit()
        }
    }

    /// Joins two branch result types (if/else, match arms). Two array
    /// literals of differing length - or an array and a Vec/slice - join
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

    fn check_match(&mut self, scrutinee: &Expr, arms: &[MatchArm], expected: Expectation) -> Ty {
        let scrut_ty = self.check_expr(scrutinee);
        self.reject_constructor_scrutinee_mismatch(scrut_ty, arms);
        let mut result_ty = self.fresh();
        for arm in arms {
            self.push_scope();
            let pat_ty = self.type_of_pattern(&arm.pattern);
            self.bind_pattern(&arm.pattern, pat_ty);
            // String literal patterns compare by value through any leading `&`
            // on the scrutinee, so `match ref_str { "foo" => ... }` is valid.
            let effective_scrut_ty = if matches!(
                &arm.pattern.kind,
                PatternKind::Literal(Literal::String(_) | Literal::RawString { .. })
            ) {
                let resolved = self.infer.resolve(self.tcx, scrut_ty);
                match self.tcx.kind(resolved) {
                    Some(TyKind::Ref { inner, .. }) => *inner,
                    _ => scrut_ty,
                }
            } else {
                scrut_ty
            };
            self.unify(effective_scrut_ty, pat_ty, arm.pattern.span);
            if let Some(guard) = &arm.guard {
                let guard_ty = self.check_expr(guard);
                let bool_ty = self.tcx.bool_ty();
                self.unify(bool_ty, guard_ty, guard.span);
            }
            let body_ty = self.check_expr_expecting(&arm.body, expected);
            result_ty = self.join_branch_tys(result_ty, body_ty, arm.body.span);
            self.pop_scope();
        }
        // Second pass: if the arms joined to a Vec/slice, re-record every
        // array-literal arm body to that shape so an unannotated
        // `let v = match n { 0 => ["a"], _ => ["b", "c"] }` lowers each
        // arm as a heap Vec, matching the joined result slot.
        for arm in arms {
            self.adjust_literal_to_join(&arm.body, result_ty);
        }
        result_ty
    }

    /// Rejects `Ok` / `Err` / `Some` / `None` arms whose scrutinee's
    /// resolved head is not the matching `Result` / `Option`. The
    /// `unify` mismatch is suppressed for these arms because the
    /// synthesized pattern type carries unresolved payload vars, so the
    /// hole is closed with a direct GT0001 here. Skips a scrutinee whose
    /// head is still an inference variable so a not-yet-resolved shape is
    /// never flagged.
    fn reject_constructor_scrutinee_mismatch(&mut self, scrut_ty: Ty, arms: &[MatchArm]) {
        let resolved = self.infer.resolve(self.tcx, scrut_ty);
        let resolved = match self.tcx.kind(resolved) {
            Some(TyKind::Ref { inner, .. }) => self.infer.resolve(self.tcx, *inner),
            _ => resolved,
        };
        // Judge the container by the resolved head only: an unresolved
        // scrutinee (`Var`) or an error type carries no decision; a
        // partially-inferred `Result<_, Var>` still has a known `Result`
        // head, so a `Some` arm against it is correctly rejected.
        match self.tcx.kind(resolved) {
            Some(TyKind::Var(_) | TyKind::Error) | None => return,
            _ => {}
        }
        for arm in arms {
            let ctor = match &arm.pattern.kind {
                PatternKind::TupleStruct { path, .. } | PatternKind::Path(path) => {
                    Self::bare_result_option_ctor(path)
                }
                _ => None,
            };
            let Some(ctor) = ctor else { continue };
            // `Ok` / `Err` need the `Result` sentinel (`u32::MAX`);
            // `Some` / `None` need the `Option` sentinel (`u32::MAX - 1`).
            let want_def = match ctor {
                "Ok" | "Err" => u32::MAX,
                _ => u32::MAX - 1,
            };
            let matches_container = matches!(
                self.tcx.kind(resolved),
                Some(TyKind::Adt { def, .. }) if def.local == want_def
            );
            if matches_container {
                continue;
            }
            let expected = if want_def == u32::MAX {
                let fresh_ok = self.fresh();
                let fresh_err = self.fresh();
                self.result_adt_ty(fresh_ok, fresh_err)
            } else {
                let fresh = self.fresh();
                self.option_adt_ty(fresh)
            };
            let expected = render_ty(self.tcx, expected);
            let found = render_ty(self.tcx, resolved);
            self.emit(
                TypeError::TypeMismatch { expected, found },
                arm.pattern.span,
            );
        }
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
        // the method's receiver - `.iter()` and friends always
        // produce the receiver's element type, regardless of which
        // wrapper they technically return.
        let derived = {
            let starting = self.infer.resolve(self.tcx, iter_ty);
            let is_var = matches!(self.tcx.kind(starting), Some(TyKind::Var(_)));
            let starting = if is_var {
                // Only `.iter()` / `.into_iter()` produce the receiver's
                // element type. Other methods (`m.get_or(k, d)`,
                // `m.values()`) return a different shape, so falling back to
                // the receiver there would derive the wrong element type.
                if let ExprKind::MethodCall { receiver, name, .. } = &iter.kind
                    && matches!(name.name.as_str(), "iter" | "into_iter")
                {
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

    fn check_block(&mut self, block: &Block, expected: Expectation) -> Ty {
        self.push_scope();
        let mut diverged = false;
        for stmt in &block.stmts {
            self.check_stmt(stmt);
            if !diverged && stmt_diverges(stmt) {
                diverged = true;
            }
        }
        let ty = if let Some(tail) = &block.tail {
            self.check_expr_expecting(tail, expected)
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
                    // The annotated (or write-arg-forced) binding type
                    // flows into the initializer as its expectation:
                    // `let x: Vec<String> = ["a", "b"]` makes the
                    // literal a growable Vec rather than a fixed
                    // `[T; N]` (see `collect_write_arg_bindings` for
                    // the `forced` source).
                    let init_expected = if ty.is_some() || forced.is_some() {
                        Expectation::HasType(binding_ty)
                    } else {
                        Expectation::None
                    };
                    let init_ty = self.check_expr_expecting(init, init_expected);
                    self.unify(binding_ty, init_ty, init.span);
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

    fn check_closure(
        &mut self,
        params: &[ClosureParam],
        ret: Option<&AstType>,
        body: &Expr,
        expected: Expectation,
    ) -> Ty {
        self.push_scope();
        // When the call site expects a function of a known shape (e.g. a
        // `Vec<T>` comparator pins `Fn(T, T) -> _`), unify each unannotated
        // parameter with the expected input type before the body is checked.
        // Without this a field access inside the body (`a.size`) sees the
        // parameter as an unresolved inference var and falls back to the
        // dynamic JSON-field path rather than the struct projection.
        let expected_inputs: Option<Vec<Ty>> = self
            .expectation_target(expected)
            .map(|t| self.infer.resolve(self.tcx, t))
            .and_then(|t| match self.tcx.kind(t) {
                Some(TyKind::FnPtr(sig) | TyKind::FnTrait(sig))
                    if sig.inputs.len() == params.len() =>
                {
                    Some(sig.inputs.clone())
                }
                _ => None,
            });
        let inputs: Vec<Ty> = params
            .iter()
            .enumerate()
            .map(|(i, param)| {
                let ty = match param.ty.as_ref() {
                    Some(ty) => self.type_from_ast(ty),
                    None => self.fresh(),
                };
                if let Some(want) = expected_inputs.as_ref().map(|inputs| inputs[i]) {
                    self.unify(ty, want, body.span);
                }
                self.bind_pattern(&param.pattern, ty);
                ty
            })
            .collect();
        let output = match ret {
            Some(ty) => self.type_from_ast(ty),
            None => self.fresh(),
        };
        let body_expected = if ret.is_some() {
            Expectation::HasType(output)
        } else {
            Expectation::None
        };
        let body_ty = self.check_expr_expecting(body, body_expected);
        self.unify(output, body_ty, body.span);
        self.pop_scope();
        self.tcx.intern(TyKind::FnPtr(FnSig { inputs, output }))
    }

    fn check_array(&mut self, arr: &ArrayExpr, expected: Expectation) -> Ty {
        // An expected growable `[T]` / `Vec<T>` (possibly behind one
        // `&`) shapes the literal: it adopts the expected container
        // type directly - fixed `[T; N]` versus heap Vec is a layout
        // decision unification cannot rewrite later - and its
        // elements are checked against `T` at the same strength.
        let growable: Option<(Ty, Ty)> = match self.expectation_target(expected) {
            Some(target) => match self.tcx.kind(target) {
                Some(TyKind::Vec(elem) | TyKind::Slice(elem)) => Some((target, *elem)),
                _ => None,
            },
            None => None,
        };
        match arr {
            ArrayExpr::List(elems) => {
                if let Some((container, want_elem)) = growable {
                    for elem in elems {
                        let got = self.check_expr_expecting(elem, expected.rewrap(want_elem));
                        if expected.unifies() {
                            self.unify(want_elem, got, elem.span);
                        }
                    }
                    return container;
                }
                let mut elem_ty = if let Some(first) = elems.first() {
                    self.check_expr(first)
                } else {
                    self.fresh()
                };
                // Join element types rather than a plain unify so a nested
                // literal whose inner arrays differ in length -
                // `[["a", "b"], ["c"]]` - settles on `Vec<String>` instead
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
                        self.adjust_literal_to_join(elem, elem_ty);
                    }
                }
                self.tcx.intern(TyKind::Array {
                    elem: elem_ty,
                    len: crate::ArrayLen::Concrete(elems.len()),
                })
            }
            ArrayExpr::Repeat { value, count } => {
                if let Some((container, want_elem)) = growable {
                    let got = self.check_expr_expecting(value, expected.rewrap(want_elem));
                    if expected.unifies() {
                        self.unify(want_elem, got, value.span);
                    }
                    self.check_expr(count);
                    return container;
                }
                let elem_ty = self.check_expr(value);
                self.check_expr(count);
                if let Some(len) = self.evaluate_array_len(count) {
                    self.tcx.intern(TyKind::Array {
                        elem: elem_ty,
                        len: crate::ArrayLen::Concrete(len),
                    })
                } else {
                    // Non-constant count: the result is a heap-allocated Vec.
                    self.tcx.intern(TyKind::Vec(elem_ty))
                }
            }
        }
    }

    fn check_path_expr(&mut self, node: NodeId, path: &gossamer_ast::PathExpr, span: Span) -> Ty {
        // `Enum::Variant` naming a variant the enum does not declare: the
        // resolver resolves the path to the enum head and leaves the bad
        // tail to fault at runtime (GX0002 `Shape::Triangle`). Reject it
        // where the enum is known and the tail is neither a declared
        // variant nor an associated function on the enum.
        if path.segments.len() >= 2 {
            let n = path.segments.len();
            let enum_name = path.segments[n - 2].name.name.as_str();
            let variant = path.segments[n - 1].name.name.as_str();
            if let Some(variants) = self.enum_variants.get(enum_name)
                && !variants.contains(variant)
                && !self
                    .user_method_owners
                    .get(variant)
                    .is_some_and(|owners| owners.contains(enum_name))
            {
                self.emit(
                    TypeError::UnknownVariant {
                        enum_name: enum_name.to_string(),
                        variant: variant.to_string(),
                    },
                    span,
                );
                return self.tcx.error_ty();
            }
        }
        let Some(resolution) = self.resolutions.get(node) else {
            return self.check_std_path_value(node, path, span);
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
            Resolution::Import { .. } | Resolution::Err => {
                self.check_std_path_value(node, path, span)
            }
        }
    }

    /// Types an unresolved path expression, handling std free
    /// functions used as first-class values. Tabled names type as a
    /// concrete `FnPtr` so combinator rows can pin against the
    /// signature; untabled std-fn-shaped paths in a value position
    /// are rejected uniformly (GT0015) because the compiled tiers
    /// have no symbol to take the address of. Everything else keeps
    /// the historical fresh-var fallback.
    fn check_std_path_value(
        &mut self,
        node: NodeId,
        path: &gossamer_ast::PathExpr,
        span: Span,
    ) -> Ty {
        let segments: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
        let joined = segments.join("::");
        if let Some(entry) = crate::std_fn_values::std_fn_value(
            joined.strip_prefix("std::").unwrap_or(joined.as_str()),
        ) {
            let inputs: Vec<Ty> = entry.params.iter().map(|p| self.std_val_ty(*p)).collect();
            let output = self.std_val_ty(entry.ret);
            return self.tcx.intern(TyKind::FnPtr(FnSig { inputs, output }));
        }
        if !self.callee_path_nodes.contains(&node)
            && crate::std_fn_values::is_std_fn_value_shape(&segments)
        {
            self.emit(TypeError::StdFnValueUnsupported { path: joined }, span);
            return self.tcx.error_ty();
        }
        self.fresh()
    }

    /// Concrete [`Ty`] for one [`crate::std_fn_values::StdValTy`] slot.
    fn std_val_ty(&mut self, shape: crate::std_fn_values::StdValTy) -> Ty {
        use crate::std_fn_values::StdValTy;
        match shape {
            StdValTy::Str => self.tcx.string_ty(),
            StdValTy::I64 => self.tcx.int_ty(IntTy::I64),
            StdValTy::Error => self.tcx.dyn_error_ty(),
            StdValTy::ResultI64 => {
                let i64_ty = self.tcx.int_ty(IntTy::I64);
                let err = self.tcx.dyn_error_ty();
                self.result_adt_ty(i64_ty, err)
            }
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
                gossamer_ast::GenericArg::Const(expr) => {
                    crate::GenericArg::Const(self.evaluate_generic_const_arg(expr))
                }
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
                if matches!(int_ty, IntTy::I128 | IntTy::U128) {
                    self.emit(
                        TypeError::Int128Unsupported {
                            ty: (*suffix).to_string(),
                        },
                        span,
                    );
                    return self.tcx.error_ty();
                }
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
        // Unsuffixed integer literal - Go-style untyped constant.
        // The fresh var is integer-constrained so it can only
        // unify with concrete integer types; if no use-site
        // constraints arise it defaults to `i64` at the end of
        // typechecking. Validate magnitude against the widest
        // integer bucket the language exposes (`u128`/`i128`),
        // not against `i64` alone - `let x: u64 = u64::MAX` is
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
            AstTypeKind::Path(path) => self.type_from_ast_path(ast_ty.id, ast_ty.span, path),
            AstTypeKind::Tuple(elems) => {
                let tys: Vec<Ty> = elems.iter().map(|e| self.type_from_ast(e)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            AstTypeKind::Array { elem, len } => {
                let elem_ty = self.type_from_ast(elem);
                let count = self.array_len_from_ast(len);
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
        // i128 / u128 have no runtime representation on any tier
        // (GT0014); reject at the spelling site so every execution
        // mode fails identically instead of the VM running 128-bit
        // arithmetic at silent 64-bit width.
        if let Some(TyKind::Int(it @ (IntTy::I128 | IntTy::U128))) = self.tcx.kind(ty) {
            let name = if matches!(it, IntTy::I128) {
                "i128"
            } else {
                "u128"
            };
            self.emit(
                TypeError::Int128Unsupported {
                    ty: name.to_string(),
                },
                ast_ty.span,
            );
            let err = self.tcx.error_ty();
            return self.record(ast_ty.id, err);
        }
        self.record(ast_ty.id, ty)
    }

    /// Expands a `type X<..> = T` alias `def` to its underlying type `T`,
    /// substituting the alias's type parameters with the use-site arguments
    /// in `path` for a generic alias. Returns `None` when `def` is not a
    /// registered alias or the argument count does not match the alias's
    /// parameters (the caller then falls back to the nominal form). Emits
    /// GT0024 and yields the error type on a cyclic alias.
    fn expand_type_alias(
        &mut self,
        def: gossamer_resolve::DefId,
        name: &str,
        span: Span,
        path: &TypePath,
    ) -> Option<Ty> {
        let (params, rhs) = self.alias_targets.get(&def).cloned()?;
        if !self.alias_expanding.insert(def) {
            self.emit(
                TypeError::CyclicTypeAlias {
                    name: name.to_string(),
                },
                span,
            );
            return Some(self.tcx.error_ty());
        }
        let body = if params.is_empty() {
            Some(rhs)
        } else {
            let args = alias_type_args(path);
            (args.len() == params.len()).then(|| subst_alias_params(&rhs, &params, &args))
        };
        let expanded = body.map(|b| self.type_from_ast(&b));
        self.alias_expanding.remove(&def);
        expanded
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per built-in type constructor; splitting hides the dispatch table"
    )]
    fn type_from_ast_path(&mut self, node: NodeId, span: Span, path: &TypePath) -> Ty {
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
                    // A non-generic `type X = T` alias is transparent: a use
                    // of `X` lowers to `T`, not to an opaque `adt#N`.
                    if kind == gossamer_resolve::DefKind::TypeAlias
                        && let Some(ty) = self.expand_type_alias(def, head_name, span, path)
                    {
                        return ty;
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
            // `Sender<T>` / `Receiver<T>` / `JoinHandle<T>` carry their
            // element type in a dedicated `TyKind`. Resolving the
            // annotation to that kind (rather than a fresh inference var)
            // lets `rx.recv()` recover `Option<T>` and a `Sender`-typed
            // param pin the channel element - without it a struct sent
            // over a channel infers as the default `i64` and materialises
            // a single pointer word instead of its inline fields.
            "Sender" => {
                let substs = self.substs_from_ast(path);
                let elem = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::Sender(elem));
            }
            "Receiver" => {
                let substs = self.substs_from_ast(path);
                let elem = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::Receiver(elem));
            }
            "JoinHandle" => {
                let substs = self.substs_from_ast(path);
                let elem = substs
                    .types()
                    .first()
                    .copied()
                    .unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::JoinHandle(elem));
            }
            "HashMap" => {
                let substs = self.substs_from_ast(path);
                let tys = substs.types();
                let key = tys.first().copied().unwrap_or_else(|| self.fresh());
                let value = tys.get(1).copied().unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::HashMap { key, value });
            }
            // `HashSet<T>` / `BTreeMap<K, V>` are opaque i64 handles at
            // runtime with no dedicated `TyKind`. Resolving the annotation to
            // a named sentinel Adt (rather than a fresh inference var) lets
            // method dispatch recover the receiver kind from its *type* when a
            // set/map flows across a function boundary and the construction
            // tag is gone.
            "HashSet" => {
                let substs = self.substs_from_ast(path);
                let def = gossamer_resolve::DefId::local(u32::MAX - 7);
                self.tcx.register_def_name(def, "HashSet");
                return self.tcx.intern(TyKind::Adt { def, substs });
            }
            // `BTreeMap<K, V>` shares the `HashMap` runtime on every tier
            // (the VM already backs it with the same sorted map, and the
            // map's `keys()` / `iter()` sort deterministically), so it
            // resolves to the same `TyKind::HashMap` and reaches the full
            // map method surface rather than a partial opaque-handle path.
            "BTreeMap" => {
                let substs = self.substs_from_ast(path);
                let tys = substs.types();
                let key = tys.first().copied().unwrap_or_else(|| self.fresh());
                let value = tys.get(1).copied().unwrap_or_else(|| self.fresh());
                return self.tcx.intern(TyKind::HashMap { key, value });
            }
            // `VecDeque<T>` is an opaque ring-buffer handle at runtime with no
            // dedicated `TyKind`. Resolving the annotation to a named sentinel
            // Adt that carries `T` in its substs lets method dispatch recover
            // the element type (so `pop_front()` binds `Option<T>` with the
            // right payload) even when the deque is consumed inline.
            "VecDeque" => {
                let substs = self.substs_from_ast(path);
                let def = gossamer_resolve::DefId::local(u32::MAX - 9);
                self.tcx.register_def_name(def, "VecDeque");
                return self.tcx.intern(TyKind::Adt { def, substs });
            }
            // `Box<T>` / `Arc<T>` / `Rc<T>` are transparent in a fully
            // GC'd language - every value is heap-shared already, so
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
            // `Weak<T>` - a non-owning reference into an RC allocation.
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
        // `time::Duration` / `time::Instant` are transparent i64 newtypes
        // with no resolver `DefId`. An explicit annotation (`d:
        // time::Duration`) must resolve to the dedicated `TyKind` so the
        // method form (`d.as_millis()`) dispatches on the receiver's
        // static type the same way the inference form does - otherwise
        // it falls to name-global dispatch and fails to lower on the
        // compiled tiers. Match the full module path (not a bare tail)
        // so a user type or `flag::Cell::Duration` named `Duration` is
        // left untouched.
        let segs: Vec<&str> = path.segments.iter().map(|s| s.name.name.as_str()).collect();
        if matches!(
            segs.as_slice(),
            ["time", "Duration"] | ["std", "time", "Duration"]
        ) {
            return self.tcx.duration_ty();
        }
        if matches!(
            segs.as_slice(),
            ["time", "Instant"] | ["std", "time", "Instant"]
        ) {
            return self.tcx.instant_ty();
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
            // `context::Context` - an opaque i64 handle with no
            // dedicated `TyKind`. Resolving the annotation to a named
            // sentinel Adt (rather than a fresh inference var) lets
            // method dispatch recover the receiver kind from its *type*
            // when a context flows in as a parameter (the canonical
            // request-propagation shape) and the construction tag is
            // gone - the `is_cancelled` / `cancel` / `done` / `done_chan`
            // calls then route to the `gos_rt_ctx_*` shims. Offset 11:
            // 9 and 10 are taken by `validate::Errors` / `FieldError`.
            "Context" => Some(11),
            // `U8Vec`: a byte-buffer handle. A concrete sentinel here lets
            // the JIT marshal a `buf: U8Vec` parameter across the trampoline
            // (`ty_to_kind` keys on `u32::MAX - 20`) instead of leaving it a
            // fresh inference var the JIT can't classify. It is NOT
            // reference-counted (a handle, like the sockets), which
            // `is_rc_managed` already reports for unregistered sentinels.
            //
            // The sibling sync handles (`Mutex` / `WaitGroup` / `Atomic` /
            // `I64Vec`) are deliberately NOT registered: their methods
            // dispatch by name on every tier (`gos_rt_wg_done`, etc.), and
            // forcing a concrete receiver type reroutes that dispatch and
            // breaks the compiled lowering. A fresh inference var keeps them
            // on the working name-global path.
            "U8Vec" => Some(20),
            _ => None,
        };
        if let Some(off) = stdlib_def_offset {
            let def = gossamer_resolve::DefId::local(u32::MAX - off);
            match tail {
                "Context" => self.tcx.register_def_name(def, "context::Context"),
                "U8Vec" => self.tcx.register_def_name(def, tail),
                _ => {}
            }
            return self.tcx.intern(TyKind::Adt {
                def,
                substs: crate::Substs::new(),
            });
        }
        // `validate::Errors` / `validate::FieldError` are opaque i64
        // handles with no dedicated `TyKind`. Resolving the annotation to
        // a named sentinel Adt (instead of a fresh inference var) lets
        // method dispatch recover the handle kind from its *type* when an
        // `Errors` / `FieldError` flows across a function boundary and the
        // construction-site tag is gone - the same recovery `HashSet`
        // and `BTreeMap` rely on.
        let validate_handle: Option<(u32, &str)> = match tail {
            "Errors" => Some((9, "Errors")),
            "FieldError" => Some((10, "FieldError")),
            _ => None,
        };
        if let Some((off, name)) = validate_handle {
            let def = gossamer_resolve::DefId::local(u32::MAX - off);
            self.tcx.register_def_name(def, name);
            return self.tcx.intern(TyKind::Adt {
                def,
                substs: crate::Substs::new(),
            });
        }
        // `net::TcpStream` / `TcpListener` / `UdpSocket` / `UnixStream` /
        // `UnixListener` are opaque i64 socket handles with no dedicated
        // `TyKind`. Resolving the annotation to a named sentinel Adt
        // (instead of a fresh inference var) lets method dispatch recover
        // the handle kind from its *type* when a socket flows through a
        // struct field or a parameter and the construction-site tag is
        // gone - without this `conn.sock.read(..)` lowers to an undefined
        // name-global symbol on the compiled tiers.
        let net_handle: Option<(u32, &str)> = match tail {
            "TcpStream" => Some((12, "net::TcpStream")),
            "TcpListener" => Some((13, "net::TcpListener")),
            "UdpSocket" => Some((14, "net::UdpSocket")),
            "UnixStream" => Some((15, "net::UnixStream")),
            "UnixListener" => Some((16, "net::UnixListener")),
            _ => None,
        };
        if let Some((off, name)) = net_handle {
            let def = gossamer_resolve::DefId::local(u32::MAX - off);
            self.tcx.register_def_name(def, name);
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

    /// Types an array-length expression. A literal yields a concrete
    /// count; a bare path naming a const generic parameter in scope
    /// yields a symbolic `Param` length linked to that parameter's
    /// position; anything else falls back to a concrete `0`.
    fn array_len_from_ast(&mut self, expr: &Expr) -> crate::ArrayLen {
        if let Some(len) = self.evaluate_array_len(expr) {
            return crate::ArrayLen::Concrete(len);
        }
        if let ExprKind::Path(path) = &expr.kind
            && path.segments.len() == 1
            && let Some(seg) = path.segments.first()
            && let Some(idx) = self
                .current_const_generic_scope
                .get(&seg.name.name)
                .copied()
        {
            return crate::ArrayLen::Param(idx);
        }
        crate::ArrayLen::Concrete(0)
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

    /// The built-in `Result` / `Option` constructor named by an
    /// unqualified pattern path (`Ok` / `Err` / `Some` / `None`), or
    /// `None` for a qualified path or any other name. Qualified
    /// variants (`MyEnum::Ok`) keep their user typing - only the bare
    /// spelling is the reserved built-in.
    fn bare_result_option_ctor(path: &gossamer_ast::Path) -> Option<&'static str> {
        if path.segments.len() != 1 {
            return None;
        }
        match path.segments.last()?.name.name.as_str() {
            "Ok" => Some("Ok"),
            "Err" => Some("Err"),
            "Some" => Some("Some"),
            "None" => Some("None"),
            _ => None,
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
        // A constructor pattern's type stays a fresh inference variable
        // here; the scrutinee unification binds it. Synthesizing the
        // `Result` / `Option` Adt at this site desugars `if let Some(n)
        // = m.get(k)` differently and miscompiles the payload binding,
        // so the bare `Ok` / `Err` / `Some` / `None` mismatch is caught
        // separately in `reject_constructor_scrutinee_mismatch`.
        match &pattern.kind {
            PatternKind::Wildcard
            | PatternKind::Ident { .. }
            | PatternKind::Path(_)
            | PatternKind::Struct { .. }
            | PatternKind::TupleStruct { .. }
            | PatternKind::Slice { .. }
            | PatternKind::Rest => self.fresh(),
            PatternKind::Error => self.tcx.error_ty(),
            PatternKind::Literal(lit) => self.type_of_literal(lit, pattern.span),
            PatternKind::Tuple(parts) => {
                let tys: Vec<Ty> = parts.iter().map(|p| self.type_of_pattern(p)).collect();
                self.tcx.intern(TyKind::Tuple(tys))
            }
            PatternKind::Range { lo, hi, .. } => match lo.as_ref().or(hi.as_ref()) {
                Some(bound) => self.type_of_literal(bound, pattern.span),
                None => self.fresh(),
            },
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
            PatternKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let resolved = self.infer.resolve(self.tcx, ty);
                let elem_ty = match self.tcx.kind(resolved).cloned() {
                    Some(TyKind::Vec(e) | TyKind::Slice(e) | TyKind::Array { elem: e, .. }) => e,
                    _ => self.fresh(),
                };
                for part in prefix {
                    self.bind_pattern(part, elem_ty);
                }
                if let Some(rest) = rest {
                    let rest_ty = self.tcx.intern(TyKind::Vec(elem_ty));
                    self.bind_pattern(rest, rest_ty);
                }
                for part in suffix {
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

/// True when a type is too unresolved or opaque to soundly reject a
/// structural use (`value[i]`, `value.N`) against it. Hard errors are
/// emitted only for concrete, fully-known types; an inference variable,
/// already-errored type, generic parameter, unresolved alias, or trait
/// object fails soft so inference, generics, and name-resolved stdlib
/// paths are never falsely rejected.
fn is_soft_for_structural_use(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Var(_)
            | TyKind::Error
            | TyKind::Param { .. }
            | TyKind::Alias { .. }
            | TyKind::Dyn(_)
    )
}

/// True when a type is a concrete runtime value that is provably not a
/// function and not an ADT constructor, so `value(args)` on it can never
/// resolve to a callable on any tier. ADTs are deliberately excluded:
/// `Some(x)` / `Ok(x)` / `MyEnum::Variant(x)` type their callee as an
/// `Option` / `Result` / enum ADT, and rejecting those would break
/// constructor calls.
fn is_definitely_not_callable_value(kind: &TyKind) -> bool {
    matches!(
        kind,
        TyKind::Bool
            | TyKind::Char
            | TyKind::String
            | TyKind::Int(_)
            | TyKind::Float(_)
            | TyKind::Unit
            | TyKind::Tuple(_)
            | TyKind::Array { .. }
            | TyKind::Slice(_)
            | TyKind::Vec(_)
            | TyKind::HashMap { .. }
            | TyKind::Sender(_)
            | TyKind::Receiver(_)
            | TyKind::JoinHandle(_)
            | TyKind::Duration
            | TyKind::Instant
            | TyKind::JsonValue
            | TyKind::DynError
    )
}

/// Canonical std combinator module name for a call path's module
/// segments, or `None` when the path is not `result` / `option` /
/// `iter` (bare or `std::`-qualified).
/// Required shape of one `strings::` free-function parameter slot,
/// used to make `check` reject a non-string argument that the
/// compiled string shims would otherwise dereference as a string
/// pointer.
#[derive(Clone, Copy)]
enum StrArgShape {
    /// Must be a `String` (a `&String` borrow peels to the same).
    Str,
    /// A pattern or pad slot: a `String` or a `char` (a single-char
    /// pattern coerces to its one-byte string on every tier).
    StrOrChar,
}

/// String-typed parameter positions of the canonical `strings::` free
/// functions, keyed by function name. Only positions that must hold a
/// string-shaped value are listed; integer width / count positions are
/// left out so the several integer widths the runtime accepts there are
/// not rejected. Argument order mirrors the interpreter's free-function
/// table (`stdlib_builtins/strings.rs`), so e.g. `splitn(text, n, sep)`
/// has its separator at index 2. Returns `None` for an unlisted name.
fn strings_fn_str_params(name: &str) -> Option<&'static [(usize, StrArgShape)]> {
    use StrArgShape::{Str, StrOrChar};
    Some(match name {
        "split" | "contains" | "find" | "rfind" | "split_once" | "rsplit_once" | "count"
        | "starts_with" | "ends_with" | "strip_prefix" | "strip_suffix" | "contains_any"
        | "find_any" | "rfind_any" | "trim_matches" | "trim_start_matches" | "trim_end_matches" => {
            &[(0, Str), (1, StrOrChar)]
        }
        "splitn" => &[(0, Str), (2, StrOrChar)],
        "replace" | "replacen" => &[(0, Str), (1, StrOrChar), (2, StrOrChar)],
        "equal_fold" => &[(0, Str), (1, Str)],
        "center" | "pad_left" | "pad_right" => &[(0, Str), (2, StrOrChar)],
        "split_whitespace" | "trim" | "trim_start" | "trim_end" | "to_lower" | "to_upper"
        | "to_title" | "lines" | "repeat" | "slice" | "substring" | "byte_at" => &[(0, Str)],
        _ => return None,
    })
}

fn combinator_module_name(module: &[&str]) -> Option<&'static str> {
    match module {
        ["result"] | ["std", "result"] => Some("result"),
        ["option"] | ["std", "option"] => Some("option"),
        ["iter"] | ["std", "iter"] => Some("iter"),
        _ => None,
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
    // Response: [status, body, raw_bytes, content_type, location,
    // headers]. raw_bytes is Vec<u8>, headers is [(String, String)];
    // the per-name `gos_rt_http_response_*` helpers handle the actual
    // dispatch, so the field-list ordering matters only for
    // source-name lookup.
    let u8_ty = tcx.int_ty(IntTy::U8);
    let vec_u8 = tcx.intern(TyKind::Vec(u8_ty));
    let str_pair = tcx.intern(TyKind::Tuple(vec![str_ty, str_ty]));
    let vec_str_pair = tcx.intern(TyKind::Vec(str_pair));
    tcx.register_struct_fields(
        gossamer_resolve::DefId::local(u32::MAX - 5),
        vec![i64_ty, str_ty, vec_u8, str_ty, str_ty, vec_str_pair],
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
    let str_pair = tcx.intern(TyKind::Tuple(vec![str_ty, str_ty]));
    let vec_str_pair = tcx.intern(TyKind::Vec(str_pair));
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
                ("headers", vec_str_pair),
            ],
        ),
    ];
    for (offset, fields) in entries {
        let def = gossamer_resolve::DefId::local(u32::MAX - offset);
        let list: Vec<(String, Ty)> = fields.iter().map(|(n, t)| ((*n).to_string(), *t)).collect();
        map.insert(def, list);
    }
}

/// Resolves a symbolic array length against a const-substitution list:
/// a `Param(idx)` length becomes `Concrete` when `const_substs[idx]`
/// holds a non-negative value; every other length is unchanged.
/// Operator-overload impl-method name for an arithmetic binary operator,
/// or `None` for operators that are not overloadable on user types.
/// Why a rejected `#[derive(name)]` is unsupported, and what to do instead.
fn derive_rejection_hint(name: &str) -> String {
    match name {
        "Clone" => "values copy by value - `let b = a` is the copy, and `a.clone()` \
                    already works without a derive"
            .to_string(),
        "Hash" | "Hashable" => {
            "structs and enums hash by value automatically; remove it".to_string()
        }
        "Copy" => {
            "values are managed automatically; there is no Copy / move distinction".to_string()
        }
        "Display" => "use `Debug`; `{}` and `{:?}` share one synthesized `fmt`".to_string(),
        "Serialize" | "Deserialize" => {
            "serialization is automatic - call `to_json::<T>` / `from_json::<T>`".to_string()
        }
        "From" | "Into" | "TryFrom" | "TryInto" | "Add" | "Sub" | "Mul" | "Div" | "Rem" | "Neg"
        | "Not" | "BitAnd" | "BitOr" | "BitXor" | "Shl" | "Shr" | "Index" | "IndexMut" => {
            format!("implement it with `impl {name} for T`, not `#[derive]`")
        }
        _ => "Gossamer derives only Debug, Default, PartialEq, Eq, PartialOrd, Ord".to_string(),
    }
}

fn arith_op_method(op: BinaryOp) -> Option<&'static str> {
    match op {
        BinaryOp::Add => Some("add"),
        BinaryOp::Sub => Some("sub"),
        BinaryOp::Mul => Some("mul"),
        BinaryOp::Div => Some("div"),
        BinaryOp::Rem => Some("rem"),
        BinaryOp::BitAnd => Some("bitand"),
        BinaryOp::BitOr => Some("bitor"),
        BinaryOp::BitXor => Some("bitxor"),
        BinaryOp::Shl => Some("shl"),
        BinaryOp::Shr => Some("shr"),
        _ => None,
    }
}

fn subst_array_len(len: crate::ArrayLen, const_substs: &[Option<i128>]) -> crate::ArrayLen {
    let crate::ArrayLen::Param(idx) = len else {
        return len;
    };
    match const_substs.get(idx.0 as usize).copied().flatten() {
        Some(v) if v >= 0 => crate::ArrayLen::Concrete(v as usize),
        _ => len,
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
        | TyKind::Duration
        | TyKind::Instant
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

/// Returns the use-site type arguments of `path` (`Foo<i64, String>` ->
/// `[i64, String]`), used to instantiate a generic alias.
fn alias_type_args(path: &TypePath) -> Vec<AstType> {
    path.segments
        .last()
        .map(|seg| {
            seg.generics
                .iter()
                .filter_map(|g| match g {
                    AstGenericArg::Type(t) => Some(t.clone()),
                    AstGenericArg::Const(_) => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Substitutes each alias type parameter in `rhs` with the matching
/// argument: `subst_alias_params((A, A), [A], [i64])` yields `(i64, i64)`.
fn subst_alias_params(rhs: &AstType, params: &[String], args: &[AstType]) -> AstType {
    use gossamer_ast::VisitorMut;
    let mut out = rhs.clone();
    AliasParamSubst { params, args }.visit_type(&mut out);
    out
}

struct AliasParamSubst<'a> {
    params: &'a [String],
    args: &'a [AstType],
}

impl gossamer_ast::VisitorMut for AliasParamSubst<'_> {
    fn visit_type(&mut self, ty: &mut AstType) {
        if let AstTypeKind::Path(p) = &ty.kind
            && p.segments.len() == 1
            && p.segments[0].generics.is_empty()
            && let Some(i) = self
                .params
                .iter()
                .position(|n| n.as_str() == p.segments[0].name.name.as_str())
        {
            *ty = self.args[i].clone();
            return;
        }
        gossamer_ast::visitor::walk_type_mut(self, ty);
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
/// are part of the surface - keeps a `fn f<T: Iterator>(...)`
/// declaration in a file that itself does not declare
/// `trait Iterator` from raising a false unknown-trait
/// diagnostic.
/// The canonical `String` method surface. Mirrors the compiled-tier
/// dispatch in `gossamer-mir`'s `method_call.rs` (the `TyKind::String`
/// arms) plus the universal `push` / `push_str` building surface, so a
/// String receiver accepts exactly what every tier can lower. A method
/// outside this set on a `String` receiver is the name-global dispatch
/// leak (a `unicode::*` char predicate or a typo) and is rejected. The
/// commonly-typed methods (`find`, `len`, `contains`, ...) are handled
/// with precise return types before this fallback and need not recur
/// here, but listing them keeps the set self-describing.
fn is_string_method(name: &str) -> bool {
    matches!(
        name,
        "len"
            | "is_empty"
            | "as_bytes"
            | "chars"
            | "split"
            | "splitn"
            | "split_whitespace"
            | "split_once"
            | "rsplit_once"
            | "lines"
            | "find"
            | "rfind"
            | "find_any"
            | "rfind_any"
            | "index_rune"
            | "contains"
            | "contains_any"
            | "contains_rune"
            | "starts_with"
            | "ends_with"
            | "equal_fold"
            | "count"
            | "byte_at"
            | "byte_len"
            | "trim"
            | "trim_start"
            | "trim_end"
            | "trim_matches"
            | "trim_start_matches"
            | "trim_end_matches"
            | "replace"
            | "replacen"
            | "to_lower"
            | "to_upper"
            | "to_title"
            | "repeat"
            | "join"
            | "strip_prefix"
            | "strip_suffix"
            | "pad_left"
            | "pad_right"
            | "center"
            | "slice"
            | "substring"
            | "push"
            | "push_str"
            | "push_char"
            | "push_byte"
            | "parse"
    )
}

/// Best-effort human-readable name for a call's callee expression,
/// used in arity diagnostics. A path renders as its joined segments;
/// anything else falls back to a generic label.
fn callee_display_name(callee: &Expr) -> String {
    match &callee.kind {
        ExprKind::Path(path) => path
            .segments
            .iter()
            .map(|s| s.name.name.as_str())
            .collect::<Vec<_>>()
            .join("::"),
        _ => "this function".to_string(),
    }
}

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
