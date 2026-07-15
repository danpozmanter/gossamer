/// Synthesizes a `fn __gos_typeinfo_<Name>() -> [(String, String)]`
/// returning each field's `(name, type)` for every named-field struct,
/// so `typeInfo::<Name>()` reflects the type's fields at compile time
/// (the comptime reflection surface). Only emitted when the source
/// mentions `typeInfo` so non-reflecting programs carry no extra items.
#[must_use]
pub fn synthesize_type_info(parsed: &SourceFile) -> String {
    let mut out = String::new();
    for item in flatten_items(&parsed.items) {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        let StructBody::Named(fields) = &decl.body else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        let entries: Vec<String> = fields
            .iter()
            .map(|f| format!("(\"{}\", \"{}\")", f.name.name, ty_to_string(&f.ty)))
            .collect();
        out.push_str(&format!(
            "fn {}() -> [(String, String)] {{ [{}] }}\n",
            type_info_fn(&decl.name.name),
            entries.join(", "),
        ));
    }
    out
}

/// Rewrites `typeInfo::<Type>()` into a call to the synthesized
/// `__gos_typeinfo_<Type>()` reflection function.
pub fn rewrite_type_info_calls(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    use gossamer_ast::expr::{Expr, ExprKind};
    use gossamer_ast::visitor::walk_expr_mut;

    struct Rewriter;
    impl VisitorMut for Rewriter {
        fn visit_expr(&mut self, expr: &mut Expr) {
            walk_expr_mut(self, expr);
            let ExprKind::Call { callee, .. } = &mut expr.kind else {
                return;
            };
            let ExprKind::Path(path) = &mut callee.kind else {
                return;
            };
            if path.segments.len() != 1 {
                return;
            }
            let seg = &mut path.segments[0];
            if seg.name.name != "typeInfo" || seg.generics.len() != 1 {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            let Some(type_seg) = tp.segments.last() else {
                return;
            };
            seg.name.name = type_info_fn(&type_seg.name.name);
            seg.generics.clear();
        }
    }
    Rewriter.visit_source_file(sf);
}

/// Hands out fresh node ids for cloned `inline for` bodies. The resolver
/// and checker key maps on `NodeId`, so each unrolled copy needs unique
/// ids rather than the source body's (which would collide).
struct InlineForIds {
    next: u32,
}

impl InlineForIds {
    fn id(&mut self) -> NodeId {
        let id = NodeId::from_raw(self.next);
        self.next += 1;
        id
    }
}

/// Renumbers a cloned subtree's node ids in place.
struct InlineForRenum<'a> {
    ids: &'a mut InlineForIds,
}

impl gossamer_ast::VisitorMut for InlineForRenum<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        e.id = self.ids.id();
        gossamer_ast::visitor::walk_expr_mut(self, e);
    }
    fn visit_pattern(&mut self, p: &mut gossamer_ast::pattern::Pattern) {
        p.id = self.ids.id();
        gossamer_ast::visitor::walk_pattern_mut(self, p);
    }
    fn visit_stmt(&mut self, s: &mut gossamer_ast::stmt::Stmt) {
        s.id = self.ids.id();
        gossamer_ast::visitor::walk_stmt_mut(self, s);
    }
    fn visit_type(&mut self, t: &mut gossamer_ast::ty::Type) {
        t.id = self.ids.id();
        gossamer_ast::visitor::walk_type_mut(self, t);
    }
}

/// Collects each non-generic named-field struct's `(field_name,
/// field_type_string)` list - the data backing both `typeInfo::<T>()` and
/// the `inline for` unroller.
fn collect_struct_fields(sf: &SourceFile) -> HashMap<String, Vec<(String, String)>> {
    let mut map = HashMap::new();
    for item in flatten_items(&sf.items) {
        let ItemKind::Struct(decl) = &item.kind else {
            continue;
        };
        let StructBody::Named(fields) = &decl.body else {
            continue;
        };
        if !decl.generics.params.is_empty() {
            continue;
        }
        let entries = fields
            .iter()
            .map(|f| (f.name.name.clone(), ty_to_string(&f.ty)))
            .collect();
        map.insert(decl.name.name.clone(), entries);
    }
    map
}

/// Returns the concrete type name `T` when `iter` is `typeInfo::<T>()`,
/// the reflection call a compile-time field loop iterates.
fn typeinfo_loop_target(iter: &gossamer_ast::expr::Expr) -> Option<String> {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Call { callee, args } = &iter.kind else {
        return None;
    };
    if !args.is_empty() {
        return None;
    }
    let ExprKind::Path(path) = &callee.kind else {
        return None;
    };
    if path.segments.len() != 1 {
        return None;
    }
    let seg = &path.segments[0];
    if seg.name.name != "typeInfo" || seg.generics.len() != 1 {
        return None;
    }
    let GenericArg::Type(ty) = &seg.generics[0] else {
        return None;
    };
    let TypeKind::Path(tp) = &ty.kind else {
        return None;
    };
    Some(tp.segments.last()?.name.name.clone())
}

/// Expands every `for PAT in typeInfo::<T>() { BODY }` over a known struct
/// `T` into a straight-line block: the body is cloned once per field, the
/// loop variables bound to the field's `(name, type)` as comptime string
/// literals, `field_of(recv, name)` resolved to `recv.<name>`, and
/// `match` / `if` over the comptime values folded to the taken branch.
/// Pure AST -> AST in the single compile, so the emitted field code lowers
/// natively on every tier and the construct never reaches the comptime
/// fold pass (no whole-program re-compile tax).
pub fn expand_typeinfo_loops(sf: &mut SourceFile) {
    use gossamer_ast::VisitorMut;
    let fields = collect_struct_fields(sf);
    if fields.is_empty() {
        return;
    }
    let mut ids = InlineForIds {
        next: sf.next_node_id,
    };
    let mut expander = TypeInfoLoopExpander {
        fields: &fields,
        ids: &mut ids,
    };
    expander.visit_source_file(sf);
    sf.next_node_id = ids.next;
}

struct TypeInfoLoopExpander<'a> {
    fields: &'a HashMap<String, Vec<(String, String)>>,
    ids: &'a mut InlineForIds,
}

impl gossamer_ast::VisitorMut for TypeInfoLoopExpander<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        // Post-order: expand nested field loops before the enclosing one
        // clones the body.
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let ExprKind::For {
            pattern,
            iter,
            body,
            ..
        } = &e.kind
        else {
            return;
        };
        let Some(tyname) = typeinfo_loop_target(iter) else {
            return;
        };
        let Some(flds) = self.fields.get(&tyname) else {
            return;
        };
        let block = unroll_typeinfo_loop(pattern, body, flds, self.ids);
        e.kind = ExprKind::Block(block);
    }
}

/// Unrolls one field loop body across `fields`, returning the straight-line
/// block. Each field's copy is scoped in its own nested block so any `let`
/// bindings the body introduces do not collide across iterations.
fn unroll_typeinfo_loop(
    pattern: &gossamer_ast::pattern::Pattern,
    body: &gossamer_ast::expr::Expr,
    fields: &[(String, String)],
    ids: &mut InlineForIds,
) -> gossamer_ast::expr::Block {
    use gossamer_ast::expr::Block;
    use gossamer_ast::stmt::{Stmt, StmtKind};
    let mut stmts = Vec::with_capacity(fields.len());
    for (fname, ftype) in fields {
        let mut binds: HashMap<String, String> = HashMap::new();
        bind_loop_pattern(pattern, fname, ftype, &mut binds);
        let mut clone = body.clone();
        let mut subst = InlineForSubst { binds: &binds };
        subst.visit_expr(&mut clone);
        let mut renum = InlineForRenum { ids };
        renum.visit_expr(&mut clone);
        let span = clone.span;
        let id = ids.id();
        stmts.push(Stmt::new(
            id,
            span,
            StmtKind::Expr {
                expr: Box::new(clone),
                has_semi: true,
            },
        ));
    }
    Block {
        stmts,
        tail: None,
        synthetic: true,
        is_arena: false,
        is_comptime: false,
    }
}

/// Binds the loop pattern's identifiers to the current field's `(name,
/// type)` strings. Supports the canonical `(name, ty)` tuple pattern;
/// wildcards bind nothing.
fn bind_loop_pattern(
    pattern: &gossamer_ast::pattern::Pattern,
    fname: &str,
    ftype: &str,
    binds: &mut HashMap<String, String>,
) {
    use gossamer_ast::pattern::PatternKind;
    let PatternKind::Tuple(elems) = &pattern.kind else {
        return;
    };
    if elems.len() != 2 {
        return;
    }
    bind_one(&elems[0], fname, binds);
    bind_one(&elems[1], ftype, binds);
}

fn bind_one(p: &gossamer_ast::pattern::Pattern, val: &str, binds: &mut HashMap<String, String>) {
    use gossamer_ast::pattern::PatternKind;
    if let PatternKind::Ident {
        name,
        subpattern: None,
        ..
    } = &p.kind
    {
        binds.insert(name.name.clone(), val.to_string());
    }
}

/// Substitutes bound loop variables with their comptime string values,
/// resolves `field_of(recv, name)` projections, and folds `match` / `if` /
/// string operations over the now-comptime values. Post-order, so a node's
/// children are already substituted when it is folded.
struct InlineForSubst<'a> {
    binds: &'a HashMap<String, String>,
}

impl gossamer_ast::VisitorMut for InlineForSubst<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        gossamer_ast::visitor::walk_expr_mut(self, e);
        if let Some(kind) = self.fold_node(e) {
            e.kind = kind;
        }
    }
}

impl InlineForSubst<'_> {
    fn fold_node(&self, e: &gossamer_ast::expr::Expr) -> Option<gossamer_ast::expr::ExprKind> {
        use gossamer_ast::common::Ident;
        use gossamer_ast::expr::{ExprKind, FieldSelector, Literal};
        match &e.kind {
            ExprKind::Path(p) if p.segments.len() == 1 && p.segments[0].generics.is_empty() => self
                .binds
                .get(p.segments[0].name.name.as_str())
                .map(|v| ExprKind::Literal(Literal::String(v.clone()))),
            ExprKind::Call { callee, args } if is_field_of(callee) && args.len() == 2 => {
                let ExprKind::Literal(Literal::String(fld)) = &args[1].kind else {
                    return None;
                };
                Some(ExprKind::FieldAccess {
                    receiver: Box::new(args[0].clone()),
                    field: FieldSelector::Named(Ident::new(fld.clone())),
                })
            }
            ExprKind::Match { scrutinee, arms } => {
                let s = literal_str(scrutinee)?;
                let body = select_arm(&s, arms)?;
                Some(body.kind.clone())
            }
            ExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => {
                let ExprKind::Literal(Literal::Bool(b)) = &condition.kind else {
                    return None;
                };
                if *b {
                    Some(then_branch.kind.clone())
                } else {
                    Some(
                        else_branch
                            .as_ref()
                            .map_or(ExprKind::Literal(Literal::Unit), |x| x.kind.clone()),
                    )
                }
            }
            ExprKind::Binary { op, lhs, rhs } => fold_binary(*op, lhs, rhs),
            _ => None,
        }
    }
}

fn is_field_of(callee: &gossamer_ast::expr::Expr) -> bool {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Path(p) = &callee.kind else {
        return false;
    };
    p.segments.len() == 1 && p.segments[0].name.name == "field_of"
}

fn literal_str(e: &gossamer_ast::expr::Expr) -> Option<String> {
    use gossamer_ast::expr::{ExprKind, Literal};
    if let ExprKind::Literal(Literal::String(s)) = &e.kind {
        Some(s.clone())
    } else {
        None
    }
}

/// Selects the first arm (no guard) whose pattern matches the comptime
/// string `s`: a string-literal pattern equal to `s`, a wildcard, or an
/// or-pattern containing either.
fn select_arm<'a>(
    s: &str,
    arms: &'a [gossamer_ast::expr::MatchArm],
) -> Option<&'a gossamer_ast::expr::Expr> {
    arms.iter()
        .find(|arm| arm.guard.is_none() && pattern_matches_str(&arm.pattern, s))
        .map(|arm| &arm.body)
}

fn pattern_matches_str(p: &gossamer_ast::pattern::Pattern, s: &str) -> bool {
    use gossamer_ast::expr::Literal;
    use gossamer_ast::pattern::PatternKind;
    match &p.kind {
        PatternKind::Wildcard => true,
        PatternKind::Literal(Literal::String(lit)) => lit == s,
        PatternKind::Or(alts) => alts.iter().any(|a| pattern_matches_str(a, s)),
        _ => false,
    }
}

/// Folds string concatenation and string equality over two comptime string
/// literals; other shapes stay runtime.
fn fold_binary(
    op: gossamer_ast::common::BinaryOp,
    lhs: &gossamer_ast::expr::Expr,
    rhs: &gossamer_ast::expr::Expr,
) -> Option<gossamer_ast::expr::ExprKind> {
    use gossamer_ast::common::BinaryOp;
    use gossamer_ast::expr::{ExprKind, Literal};
    let (l, r) = (literal_str(lhs)?, literal_str(rhs)?);
    match op {
        BinaryOp::Add => Some(ExprKind::Literal(Literal::String(l + &r))),
        BinaryOp::Eq => Some(ExprKind::Literal(Literal::Bool(l == r))),
        BinaryOp::Ne => Some(ExprKind::Literal(Literal::Bool(l != r))),
        _ => None,
    }
}

/// Specializes generic field-loop templates per turbofish call site, so a
/// reflection-driven serializer can be written once as
/// `fn name<T>(v: T) -> String { ... for (n, t) in typeInfo::<T>() ... }`
/// and called as `name::<User>(x)`. Each `name::<C>(args)` site gets a
/// monomorphic copy `__gos_inlinefor_name_C` with the type parameter
/// replaced by `C` everywhere - so the inner `typeInfo::<T>()` becomes
/// `typeInfo::<C>()`, which the unroller then expands - the call is
/// rewritten to that copy, and the generic template is removed so it is
/// never type-checked generically. Runs before `expand_typeinfo_loops`.
/// Concrete-type loops use no turbofish and skip this pass entirely; a
/// template called without a turbofish leaves no specialization and the
/// removed template surfaces as an ordinary unknown-name error.
pub fn specialize_inline_for_generics(sf: &mut SourceFile) {
    use gossamer_ast::{ItemKind, Visitor};
    let mut templates: HashMap<String, String> = HashMap::new();
    for item in &sf.items {
        if let ItemKind::Fn(decl) = &item.kind
            && let Some(param) = inline_for_template_param(decl)
        {
            templates.insert(decl.name.name.clone(), param);
        }
    }
    if templates.is_empty() {
        return;
    }
    let mut wanted: HashMap<String, std::collections::BTreeSet<String>> = HashMap::new();
    let mut collector = TurbofishCollector {
        templates: &templates,
        wanted: &mut wanted,
    };
    collector.visit_source_file(sf);

    let mut ids = InlineForIds {
        next: sf.next_node_id,
    };
    let mut copies: Vec<gossamer_ast::Item> = Vec::new();
    for item in &sf.items {
        let ItemKind::Fn(decl) = &item.kind else {
            continue;
        };
        let Some(param) = templates.get(&decl.name.name) else {
            continue;
        };
        let Some(types) = wanted.get(&decl.name.name) else {
            continue;
        };
        for concrete in types {
            copies.push(specialize_template(item, decl, param, concrete, &mut ids));
        }
    }
    sf.next_node_id = ids.next;

    sf.items.extend(copies);
    let mut rewriter = TurbofishRewriter {
        templates: &templates,
    };
    gossamer_ast::VisitorMut::visit_source_file(&mut rewriter, sf);
    sf.items.retain(|item| match &item.kind {
        ItemKind::Fn(decl) => !templates.contains_key(&decl.name.name),
        _ => true,
    });
}

/// Returns the type-parameter name of a generic function whose body has a
/// `for ... in typeInfo::<T>()` loop over that parameter.
fn inline_for_template_param(decl: &gossamer_ast::FnDecl) -> Option<String> {
    use gossamer_ast::{GenericParam, Visitor};
    let params: Vec<String> = decl
        .generics
        .params
        .iter()
        .filter_map(|p| match p {
            GenericParam::Type { name, .. } => Some(name.name.clone()),
            _ => None,
        })
        .collect();
    if params.is_empty() {
        return None;
    }
    let body = decl.body.as_ref()?;
    let mut probe = TemplateProbe {
        params: &params,
        found: None,
    };
    probe.visit_expr(body);
    probe.found
}

struct TemplateProbe<'a> {
    params: &'a [String],
    found: Option<String>,
}

impl gossamer_ast::Visitor for TemplateProbe<'_> {
    fn visit_expr(&mut self, e: &gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        if self.found.is_some() {
            return;
        }
        if let ExprKind::For { iter, .. } = &e.kind
            && let Some(t) = typeinfo_loop_target(iter)
            && self.params.contains(&t)
        {
            self.found = Some(t);
            return;
        }
        gossamer_ast::visitor::walk_expr(self, e);
    }
}

struct TurbofishCollector<'a> {
    templates: &'a HashMap<String, String>,
    wanted: &'a mut HashMap<String, std::collections::BTreeSet<String>>,
}

impl gossamer_ast::Visitor for TurbofishCollector<'_> {
    fn visit_expr(&mut self, e: &gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        if let ExprKind::Call { callee, .. } = &e.kind
            && let Some((name, concrete)) = turbofish_call_target(callee)
            && self.templates.contains_key(&name)
        {
            self.wanted.entry(name).or_default().insert(concrete);
        }
        gossamer_ast::visitor::walk_expr(self, e);
    }
}

/// Returns `(fn_name, concrete_type)` when `callee` is `fn_name::<Concrete>`.
fn turbofish_call_target(callee: &gossamer_ast::expr::Expr) -> Option<(String, String)> {
    use gossamer_ast::expr::ExprKind;
    let ExprKind::Path(p) = &callee.kind else {
        return None;
    };
    if p.segments.len() != 1 {
        return None;
    }
    let seg = &p.segments[0];
    if seg.generics.len() != 1 {
        return None;
    }
    let GenericArg::Type(ty) = &seg.generics[0] else {
        return None;
    };
    let TypeKind::Path(tp) = &ty.kind else {
        return None;
    };
    Some((seg.name.name.clone(), tp.segments.last()?.name.name.clone()))
}

/// Builds one monomorphic copy of `decl` with the type parameter `param`
/// replaced by `concrete`, freshly numbered and renamed.
fn specialize_template(
    item: &gossamer_ast::Item,
    decl: &gossamer_ast::FnDecl,
    param: &str,
    concrete: &str,
    ids: &mut InlineForIds,
) -> gossamer_ast::Item {
    use gossamer_ast::items::Generics;
    use gossamer_ast::{FnParam, Ident, ItemKind, VisitorMut};
    let mut copy = decl.clone();
    copy.name = Ident::new(format!("__gos_inlinefor_{}_{concrete}", decl.name.name));
    copy.generics = Generics { params: Vec::new() };

    let mut sub = SubstTypeParam { param, concrete };
    for p in &mut copy.params {
        if let FnParam::Typed { ty, .. } = p {
            sub.visit_type(ty);
        }
    }
    if let Some(ret) = &mut copy.ret {
        sub.visit_type(ret);
    }
    if let Some(body) = &mut copy.body {
        sub.visit_expr(body);
    }

    let mut renum = InlineForRenum { ids };
    for p in &mut copy.params {
        if let FnParam::Typed { pattern, ty, .. } = p {
            renum.visit_pattern(pattern);
            renum.visit_type(ty);
        }
    }
    if let Some(ret) = &mut copy.ret {
        renum.visit_type(ret);
    }
    if let Some(body) = &mut copy.body {
        renum.visit_expr(body);
    }

    gossamer_ast::Item::new(
        ids.id(),
        item.span,
        item.attrs.clone(),
        item.visibility,
        ItemKind::Fn(copy),
    )
}

/// Replaces a type parameter path `T` with the concrete type `concrete`
/// throughout the visited types - including the `T` inside the body's
/// `typeInfo::<T>()` turbofish, which the type walker reaches.
struct SubstTypeParam<'a> {
    param: &'a str,
    concrete: &'a str,
}

impl gossamer_ast::VisitorMut for SubstTypeParam<'_> {
    fn visit_type(&mut self, ty: &mut gossamer_ast::ty::Type) {
        use gossamer_ast::ty::TypeKind;
        gossamer_ast::visitor::walk_type_mut(self, ty);
        if let TypeKind::Path(tp) = &mut ty.kind
            && tp.segments.len() == 1
            && tp.segments[0].name.name == self.param
            && tp.segments[0].generics.is_empty()
        {
            tp.segments[0].name = gossamer_ast::Ident::new(self.concrete);
        }
    }
}

/// Rewrites each `name::<C>(args)` template call to `__gos_inlinefor_name_C`.
struct TurbofishRewriter<'a> {
    templates: &'a HashMap<String, String>,
}

impl gossamer_ast::VisitorMut for TurbofishRewriter<'_> {
    fn visit_expr(&mut self, e: &mut gossamer_ast::expr::Expr) {
        use gossamer_ast::expr::ExprKind;
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let ExprKind::Call { callee, .. } = &mut e.kind else {
            return;
        };
        let ExprKind::Path(p) = &mut callee.kind else {
            return;
        };
        if p.segments.len() != 1 {
            return;
        }
        let orig = p.segments[0].name.name.clone();
        if !self.templates.contains_key(&orig) {
            return;
        }
        let concrete = {
            let seg = &p.segments[0];
            if seg.generics.len() != 1 {
                return;
            }
            let GenericArg::Type(ty) = &seg.generics[0] else {
                return;
            };
            let TypeKind::Path(tp) = &ty.kind else {
                return;
            };
            match tp.segments.last() {
                Some(s) => s.name.name.clone(),
                None => return,
            }
        };
        let seg = &mut p.segments[0];
        seg.name = gossamer_ast::Ident::new(format!("__gos_inlinefor_{orig}_{concrete}"));
        seg.generics.clear();
    }
}

