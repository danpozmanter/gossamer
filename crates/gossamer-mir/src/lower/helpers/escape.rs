//! Conservative escape analysis driving automatic arena regions.
//!
//! A loop body whose allocations provably do not outlive the iteration can
//! be wrapped in an arena region (`gos_rt_arena_push` .. `arena_pop`) so
//! the whole iteration's heap is reclaimed in one bulk free instead of a
//! per-node reference-count teardown. This is what lets idiomatic
//! allocation-churn code (build a tree, consume it, discard) approach a
//! tracing GC's throughput without the user writing a single annotation.
//!
//! Soundness is the entire game: regioning a loop whose allocations DO
//! escape is a use-after-free. Every check here is a conservative
//! over-approximation - when in doubt, the loop is NOT regioned.

use std::collections::{HashMap, HashSet};

use gossamer_hir::{HirBlock, HirExpr, HirExprKind, HirItemKind, HirProgram, HirStmt, HirStmtKind};
use gossamer_resolve::DefId;
use gossamer_types::{Ty, TyCtxt, TyKind};

/// Method names that mutate their receiver in place (could stash an
/// argument into a caller-owned container).
const MUTATOR_METHODS: &[&str] = &[
    "push",
    "pop",
    "insert",
    "remove",
    "append",
    "extend",
    "swap",
    "sort",
    "sort_by",
    "retain",
    "clear",
    "truncate",
    "set",
    "inc",
    "or_insert",
    "reverse",
    "push_str",
    "drain",
];

/// True for types whose values carry no heap ownership - copying or
/// dropping them frees nothing, so they can flow out of a region freely.
pub(crate) fn is_copy_ty(tcx: &TyCtxt, ty: Ty) -> bool {
    matches!(
        tcx.kind_of(ty),
        TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char | TyKind::Unit
    )
}

/// Free functions that may let a value escape beyond their own return:
/// they spawn goroutines, touch channels, write a static, or stash a
/// value through a parameter. Calling one inside an auto-region is unsound.
/// Computed as a transitive closure over the static call graph.
pub(crate) fn collect_region_unsafe_fns(program: &HirProgram, tcx: &TyCtxt) -> HashSet<DefId> {
    let mut static_defs: HashSet<DefId> = HashSet::new();
    for item in &program.items {
        if let HirItemKind::Static(_) = &item.kind {
            if let Some(d) = item.def {
                static_defs.insert(d);
            }
        }
    }

    let mut direct_unsafe: HashSet<DefId> = HashSet::new();
    let mut callees: HashMap<DefId, HashSet<DefId>> = HashMap::new();

    for item in &program.items {
        let HirItemKind::Fn(f) = &item.kind else {
            continue;
        };
        let (Some(def), Some(body)) = (item.def, &f.body) else {
            continue;
        };
        let params: HashSet<String> = f
            .params
            .iter()
            .flat_map(|p| {
                let mut names = Vec::new();
                pat_binding_names(&p.pattern, &mut names);
                names
            })
            .collect();
        let mut scan = Scan {
            tcx,
            params: &params,
            statics: &static_defs,
            unsafe_now: false,
            callees: HashSet::new(),
        };
        scan.block(&body.block);
        if scan.unsafe_now {
            direct_unsafe.insert(def);
        }
        callees.insert(def, scan.callees);
    }

    // Fixpoint: a function is unsafe if it is directly unsafe or calls an
    // unsafe function.
    let mut unsafe_set = direct_unsafe;
    loop {
        let mut changed = false;
        for (def, cs) in &callees {
            if !unsafe_set.contains(def) && cs.iter().any(|c| unsafe_set.contains(c)) {
                unsafe_set.insert(*def);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    unsafe_set
}

/// Every identifier a pattern binds, walking tuple / variant / struct / ref /
/// `@` sub-patterns so a destructured `let (a, b) = …` registers both names.
fn pat_binding_names(pat: &gossamer_hir::HirPat, out: &mut Vec<String>) {
    use gossamer_hir::HirPatKind;
    match &pat.kind {
        HirPatKind::Binding { name, .. } => out.push(name.name.clone()),
        HirPatKind::At { name, sub, .. } => {
            out.push(name.name.clone());
            pat_binding_names(sub, out);
        }
        HirPatKind::Tuple(parts) | HirPatKind::Variant { fields: parts, .. } => {
            for p in parts {
                pat_binding_names(p, out);
            }
        }
        HirPatKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(p) => pat_binding_names(p, out),
                    // Shorthand `Foo { x }` binds the field name itself.
                    None => out.push(f.name.name.clone()),
                }
            }
        }
        HirPatKind::Ref { inner, .. } => pat_binding_names(inner, out),
        HirPatKind::Or(alts) => {
            // Every arm of an or-pattern binds the same names; one arm suffices.
            if let Some(first) = alts.first() {
                pat_binding_names(first, out);
            }
        }
        HirPatKind::Wildcard
        | HirPatKind::Literal(_)
        | HirPatKind::Rest
        | HirPatKind::Range { .. } => {}
    }
}

/// Single-segment name of a path expression, if it is one.
fn path_root_name(expr: &HirExpr) -> Option<&str> {
    match &expr.kind {
        HirExprKind::Path { segments, .. } => segments.first().map(|s| s.name.as_str()),
        _ => None,
    }
}

/// Peels `&`/`&mut`/deref wrappers to the inner place expression.
fn peel_refs(expr: &HirExpr) -> &HirExpr {
    let mut cur = expr;
    loop {
        match &cur.kind {
            HirExprKind::Unary { operand, .. } => cur = operand,
            _ => return cur,
        }
    }
}

/// Root path name of a place expression (`x`, `*x`, `x.f`, `x[i]`, `&x`).
fn place_root_name(expr: &HirExpr) -> Option<&str> {
    match &expr.kind {
        HirExprKind::Path { segments, .. } => segments.first().map(|s| s.name.as_str()),
        HirExprKind::Unary { operand, .. } => place_root_name(operand),
        HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
            place_root_name(receiver)
        }
        HirExprKind::Index { base, .. } => place_root_name(base),
        _ => None,
    }
}

struct Scan<'a> {
    tcx: &'a TyCtxt,
    params: &'a HashSet<String>,
    statics: &'a HashSet<DefId>,
    unsafe_now: bool,
    callees: HashSet<DefId>,
}

impl Scan<'_> {
    fn block(&mut self, b: &HirBlock) {
        for s in &b.stmts {
            self.stmt(s);
        }
        if let Some(t) = &b.tail {
            self.expr(t);
        }
    }

    fn stmt(&mut self, s: &HirStmt) {
        match &s.kind {
            HirStmtKind::Let { init, .. } => {
                if let Some(e) = init {
                    self.expr(e);
                }
            }
            HirStmtKind::Expr { expr, .. } | HirStmtKind::Defer(expr) => self.expr(expr),
            HirStmtKind::Go(_) => self.unsafe_now = true,
            HirStmtKind::Item(_) => {}
        }
    }

    fn expr(&mut self, e: &HirExpr) {
        match &e.kind {
            HirExprKind::Go(_) | HirExprKind::Select { .. } => {
                self.unsafe_now = true;
            }
            HirExprKind::Call { callee, args } => {
                if let HirExprKind::Path { def: Some(d), .. } = &callee.kind {
                    self.callees.insert(*d);
                }
                self.expr(callee);
                for a in args {
                    self.expr(a);
                }
            }
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
            } => {
                // A mutator on a parameter-rooted receiver may stash an
                // argument into a caller-owned structure → escape.
                if MUTATOR_METHODS.contains(&name.name.as_str())
                    && place_root_name(receiver).is_some_and(|r| self.params.contains(r))
                {
                    self.unsafe_now = true;
                }
                self.expr(receiver);
                for a in args {
                    self.expr(a);
                }
            }
            HirExprKind::Assign { place, value } => {
                // Writing a static, or storing a non-Copy value through a
                // parameter, escapes.
                if let HirExprKind::Path { def: Some(d), .. } = &place.kind {
                    if self.statics.contains(d) {
                        self.unsafe_now = true;
                    }
                }
                if !is_copy_ty(self.tcx, value.ty)
                    && place_root_name(place).is_some_and(|r| self.params.contains(r))
                {
                    self.unsafe_now = true;
                }
                self.expr(place);
                self.expr(value);
            }
            // Structural recursion over everything else.
            HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
                self.expr(receiver);
            }
            HirExprKind::Index { base, index } => {
                self.expr(base);
                self.expr(index);
            }
            HirExprKind::Unary { operand, .. } => self.expr(operand),
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.expr(condition);
                self.expr(then_branch);
                if let Some(e) = else_branch {
                    self.expr(e);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g);
                    }
                    self.expr(&arm.body);
                }
            }
            HirExprKind::Loop { body } => self.expr(body),
            HirExprKind::While { condition, body } => {
                self.expr(condition);
                self.expr(body);
            }
            HirExprKind::Block(b) => self.block(b),
            HirExprKind::Return(Some(e)) | HirExprKind::Break(Some(e)) => self.expr(e),
            HirExprKind::Tuple(items) => {
                for i in items {
                    self.expr(i);
                }
            }
            HirExprKind::Array(arr) => match arr {
                gossamer_hir::HirArrayExpr::List(items) => {
                    for i in items {
                        self.expr(i);
                    }
                }
                gossamer_hir::HirArrayExpr::Repeat { value, count } => {
                    self.expr(value);
                    self.expr(count);
                }
            },
            HirExprKind::Cast { value, .. } => self.expr(value),
            HirExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.expr(s);
                }
                if let Some(en) = end {
                    self.expr(en);
                }
            }
            // Closures capture by reference into a GC env; treat any closure
            // as opaque (its body may escape). Conservative: mark unsafe.
            HirExprKind::Closure { .. } | HirExprKind::LiftedClosure { .. } => {
                self.unsafe_now = true;
            }
            HirExprKind::Literal(_)
            | HirExprKind::Path { .. }
            | HirExprKind::Continue
            | HirExprKind::Return(None)
            | HirExprKind::Break(None)
            | HirExprKind::Placeholder => {}
        }
    }
}

/// Walks a loop body and decides whether it is safe to wrap in an arena
/// region: no control flow escapes the region without a pop, no value
/// created in the body outlives the iteration, and every callee is
/// region-safe. `outer_local_ty` resolves a name visible before the loop
/// to its type (so an outer non-Copy value passed into a call is rejected).
pub(crate) struct LoopEligibility<'a> {
    pub tcx: &'a TyCtxt,
    pub unsafe_fns: &'a HashSet<DefId>,
    /// Names declared inside the loop body so far (let-bindings); these die
    /// at the iteration boundary and are safe to pass around.
    in_body: HashSet<String>,
    ok: bool,
    /// True once the body is seen to allocate a heap value (a call returning a
    /// heap type, etc.). A region only pays off if there is something to arena;
    /// a purely-scalar body (a counter scan, byte stores) must NOT be wrapped,
    /// or every iteration pays two `arena_push`/`arena_pop` calls for nothing.
    allocates: bool,
}

impl<'a> LoopEligibility<'a> {
    pub fn new(tcx: &'a TyCtxt, unsafe_fns: &'a HashSet<DefId>) -> Self {
        Self {
            tcx,
            unsafe_fns,
            in_body: HashSet::new(),
            ok: true,
            allocates: false,
        }
    }

    /// A call/expression result type that lives on the heap (so wrapping the
    /// body in an arena region can bulk-free it). Scalars / unit / refs do not.
    fn is_alloc_ty(&self, ty: Ty) -> bool {
        use gossamer_types::TyKind;
        matches!(
            self.tcx.kind_of(ty),
            TyKind::Adt { .. }
                | TyKind::Vec(_)
                | TyKind::Slice(_)
                | TyKind::HashMap { .. }
                | TyKind::String
                | TyKind::DynError
                | TyKind::JsonValue
                | TyKind::Tuple(_)
        )
    }

    /// Returns true if `body` (a loop body expression) is region-eligible.
    pub fn check(mut self, body: &HirExpr) -> bool {
        self.expr(body, true);
        self.ok && self.allocates
    }

    fn block(&mut self, b: &HirBlock, top: bool) {
        for s in &b.stmts {
            match &s.kind {
                HirStmtKind::Let { pattern, init, .. } => {
                    if let Some(e) = init {
                        self.expr(e, false);
                    }
                    let mut names = Vec::new();
                    pat_binding_names(pattern, &mut names);
                    self.in_body.extend(names);
                }
                HirStmtKind::Expr { expr, .. } => self.expr(expr, false),
                HirStmtKind::Defer(_) | HirStmtKind::Go(_) | HirStmtKind::Item(_) => {
                    self.ok = false;
                }
            }
            if !self.ok {
                return;
            }
        }
        if let Some(t) = &b.tail {
            self.expr(t, top);
        }
    }

    /// Checks a call argument: it must be Copy, or created inside the body
    /// (an in-body let or a fresh call result), never an outer non-Copy.
    /// References are peeled first so `&mut seed` is judged by the referent
    /// (`seed: i64`, Copy - safe), not the reference type.
    fn check_arg(&mut self, arg: &HirExpr) {
        let inner = peel_refs(arg);
        if is_copy_ty(self.tcx, inner.ty) {
            return;
        }
        match &inner.kind {
            // Fresh value produced in the body - dies with the region.
            HirExprKind::Call { .. }
            | HirExprKind::Literal(_)
            | HirExprKind::Tuple(_)
            | HirExprKind::Array(_) => {}
            HirExprKind::Path { .. } => {
                // A non-Copy value passed into a call is safe only if it was
                // created inside the body (dies with the region). An outer
                // local, parameter, or global is rejected conservatively.
                match path_root_name(inner) {
                    Some(root) if self.in_body.contains(root) => {}
                    _ => self.ok = false,
                }
            }
            _ => {
                // Field/index/etc of something - conservatively reject if
                // it is non-Copy and not obviously in-body.
                if !place_root_name(inner).is_some_and(|r| self.in_body.contains(r)) {
                    self.ok = false;
                }
            }
        }
    }

    fn expr(&mut self, e: &HirExpr, _top: bool) {
        if !self.ok {
            return;
        }
        match &e.kind {
            // Control flow that would skip the region pop, or escape values.
            // Any break/continue/return can bypass the pop emitted at the
            // body's fall-through exit, leaving the region open.
            HirExprKind::Return(_)
            | HirExprKind::Break(_)
            | HirExprKind::Continue
            | HirExprKind::Go(_)
            | HirExprKind::Select { .. }
            | HirExprKind::Closure { .. }
            | HirExprKind::LiftedClosure { .. } => {
                self.ok = false;
            }
            // Nested loops are analyzed (and regioned) on their own.
            HirExprKind::Loop { .. } | HirExprKind::While { .. } => {
                self.ok = false;
            }
            HirExprKind::Call { callee, args } => {
                if self.is_alloc_ty(e.ty) {
                    self.allocates = true;
                }
                match &callee.kind {
                    HirExprKind::Path { def: Some(d), .. } => {
                        if self.unsafe_fns.contains(d) {
                            self.ok = false;
                        }
                    }
                    // Unresolved / non-path callee - cannot vet it.
                    _ => self.ok = false,
                }
                for a in args {
                    self.check_arg(a);
                    self.expr(a, false);
                }
            }
            // A method call could mutate an outer container; too risky to
            // vet here, so loops containing them are never auto-regioned.
            HirExprKind::MethodCall { .. } => self.ok = false,
            HirExprKind::Assign { place, value } => {
                // Only Copy-typed places (i64 accumulators, loop counters)
                // may be assigned; storing a heap value into any binding
                // could let it outlive the iteration.
                if !is_copy_ty(self.tcx, place.ty) {
                    self.ok = false;
                }
                self.expr(value, false);
            }
            HirExprKind::Field { receiver, .. } | HirExprKind::TupleIndex { receiver, .. } => {
                self.expr(receiver, false);
            }
            HirExprKind::Index { base, index } => {
                self.expr(base, false);
                self.expr(index, false);
            }
            HirExprKind::Unary { operand, .. } => self.expr(operand, false),
            HirExprKind::Binary { lhs, rhs, .. } => {
                self.expr(lhs, false);
                self.expr(rhs, false);
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                self.expr(condition, false);
                self.expr(then_branch, false);
                if let Some(el) = else_branch {
                    self.expr(el, false);
                }
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.expr(scrutinee, false);
                for arm in arms {
                    if let Some(g) = &arm.guard {
                        self.expr(g, false);
                    }
                    self.expr(&arm.body, false);
                }
            }
            HirExprKind::Block(b) => self.block(b, false),
            HirExprKind::Tuple(items) => {
                for i in items {
                    self.expr(i, false);
                }
            }
            HirExprKind::Array(arr) => match arr {
                gossamer_hir::HirArrayExpr::List(items) => {
                    for i in items {
                        self.expr(i, false);
                    }
                }
                gossamer_hir::HirArrayExpr::Repeat { value, count } => {
                    self.expr(value, false);
                    self.expr(count, false);
                }
            },
            HirExprKind::Cast { value, .. } => self.expr(value, false),
            HirExprKind::Range { start, end, .. } => {
                if let Some(s) = start {
                    self.expr(s, false);
                }
                if let Some(en) = end {
                    self.expr(en, false);
                }
            }
            HirExprKind::Literal(_) | HirExprKind::Path { .. } | HirExprKind::Placeholder => {}
        }
    }
}
