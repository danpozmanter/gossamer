// Associated constants are ordinary constants reached through a type or a
// trait bound. Each one hoists to a top-level `const` with a mangled name,
// and every `Type::NAME` / `Self::NAME` / `T::NAME` read is rewritten to
// name that constant. The result is plain Gossamer source, so it compiles
// through every tier with no dedicated runtime or lowering support.

use gossamer_ast::assoc::type_head_name as assoc_type_head_name;
use gossamer_ast::{
    AssocIndex, AssocResolution, ConstDecl, Expr, ExprKind, FnDecl, GenericParam, Generics, Ident,
    ImplItem, PathExpr, PathSegment, TraitItem, Visibility, WhereClause,
};

/// Name prefix of the top-level constant an associated constant hoists to.
/// The `__gos_` prefix is reserved for compiler-generated items, so the
/// name can never shadow one the program declares.
const ASSOC_CONST_PREFIX: &str = "__gos_assoc_const_";

/// Name of the top-level constant holding `owner`'s associated constant
/// `name`, where `owner` is the impl's self type or the declaring trait.
fn hoisted_name(owner: &str, name: &str) -> String {
    format!("{ASSOC_CONST_PREFIX}{owner}_{name}")
}

/// Rewrites every associated-constant read to the top-level constant it
/// names, then hoists each associated constant's value into that constant.
pub fn hoist_associated_consts(sf: &mut SourceFile) {
    let index = AssocIndex::build(sf);
    if index.is_assoc_const_free() {
        return;
    }
    let empty = HashMap::new();
    rewrite_items(&mut sf.items, &index, None, None, &empty);
    let mut ids = AssocIds {
        next: sf.next_node_id,
    };
    hoist_items(&mut sf.items, &mut ids);
    sf.next_node_id = ids.next;
}

/// Node-id allocator for the constants and paths this pass synthesizes.
struct AssocIds {
    next: u32,
}

impl AssocIds {
    fn id(&mut self) -> NodeId {
        let id = NodeId::from_raw(self.next);
        self.next += 1;
        id
    }
}

/// Renumbers a cloned type's node ids so the copy never shares a table
/// entry with the item it was cloned from.
struct TypeRenum<'a> {
    ids: &'a mut AssocIds,
}

impl gossamer_ast::VisitorMut for TypeRenum<'_> {
    fn visit_type(&mut self, t: &mut gossamer_ast::Type) {
        t.id = self.ids.id();
        gossamer_ast::visitor::walk_type_mut(self, t);
    }
    fn visit_expr(&mut self, e: &mut Expr) {
        e.id = self.ids.id();
        gossamer_ast::visitor::walk_expr_mut(self, e);
    }
}

/// Trait names bounding each type parameter of a declaration, from the
/// angle brackets and the `where` clause alike.
fn param_bounds(generics: &Generics, where_clause: &WhereClause) -> HashMap<String, Vec<String>> {
    let mut out: HashMap<String, Vec<String>> = HashMap::new();
    for param in &generics.params {
        let GenericParam::Type { name, bounds, .. } = param else {
            continue;
        };
        let entry = out.entry(name.name.clone()).or_default();
        entry.extend(bounds.iter().filter_map(|b| b.trait_name().map(str::to_string)));
    }
    for predicate in &where_clause.predicates {
        let Some(name) = assoc_type_head_name(&predicate.bounded) else {
            continue;
        };
        let entry = out.entry(name.to_string()).or_default();
        entry.extend(
            predicate
                .bounds
                .iter()
                .filter_map(|b| b.trait_name().map(str::to_string)),
        );
    }
    out
}

fn merged_bounds(
    outer: &HashMap<String, Vec<String>>,
    generics: &Generics,
    where_clause: &WhereClause,
) -> HashMap<String, Vec<String>> {
    let mut merged = outer.clone();
    for (name, bounds) in param_bounds(generics, where_clause) {
        merged.entry(name).or_default().extend(bounds);
    }
    merged
}

fn rewrite_items(
    items: &mut [Item],
    index: &AssocIndex,
    self_ty: Option<&str>,
    self_trait: Option<&str>,
    params: &HashMap<String, Vec<String>>,
) {
    for item in items {
        match &mut item.kind {
            ItemKind::Fn(decl) => rewrite_fn(decl, index, self_ty, self_trait, params),
            ItemKind::Impl(decl) => {
                let impl_self = assoc_type_head_name(&decl.self_ty).map(ToString::to_string);
                let impl_trait = decl.trait_ref.as_ref().and_then(|b| b.trait_name());
                let impl_trait = impl_trait.map(ToString::to_string);
                let impl_params = merged_bounds(params, &decl.generics, &decl.where_clause);
                for impl_item in &mut decl.items {
                    match impl_item {
                        ImplItem::Fn(fn_decl) => rewrite_fn(
                            fn_decl,
                            index,
                            impl_self.as_deref(),
                            impl_trait.as_deref(),
                            &impl_params,
                        ),
                        ImplItem::Const { value, .. } => rewrite_expr(
                            value,
                            index,
                            impl_self.as_deref(),
                            impl_trait.as_deref(),
                            &impl_params,
                        ),
                        ImplItem::Type { .. } => {}
                    }
                }
            }
            ItemKind::Trait(decl) => {
                let trait_name = decl.name.name.clone();
                let trait_params = merged_bounds(params, &decl.generics, &decl.where_clause);
                for trait_item in &mut decl.items {
                    match trait_item {
                        TraitItem::Fn(fn_decl) => {
                            rewrite_fn(fn_decl, index, None, Some(&trait_name), &trait_params);
                        }
                        TraitItem::Const {
                            default: Some(value),
                            ..
                        } => rewrite_expr(value, index, None, Some(&trait_name), &trait_params),
                        TraitItem::Const { .. } | TraitItem::Type { .. } => {}
                    }
                }
            }
            ItemKind::Const(decl) => rewrite_expr(&mut decl.value, index, None, None, params),
            ItemKind::Static(decl) => rewrite_expr(&mut decl.value, index, None, None, params),
            ItemKind::Mod(decl) => {
                if let ModBody::Inline(inner) = &mut decl.body {
                    rewrite_items(inner, index, self_ty, self_trait, params);
                }
            }
            _ => {}
        }
    }
}

fn rewrite_fn(
    decl: &mut FnDecl,
    index: &AssocIndex,
    self_ty: Option<&str>,
    self_trait: Option<&str>,
    params: &HashMap<String, Vec<String>>,
) {
    let fn_params = merged_bounds(params, &decl.generics, &decl.where_clause);
    if let Some(body) = &mut decl.body {
        rewrite_expr(body, index, self_ty, self_trait, &fn_params);
    }
}

fn rewrite_expr(
    expr: &mut Expr,
    index: &AssocIndex,
    self_ty: Option<&str>,
    self_trait: Option<&str>,
    params: &HashMap<String, Vec<String>>,
) {
    let mut rewriter = ConstReadRewriter {
        index,
        self_ty,
        self_trait,
        params,
    };
    gossamer_ast::VisitorMut::visit_expr(&mut rewriter, expr);
}

struct ConstReadRewriter<'a> {
    index: &'a AssocIndex,
    self_ty: Option<&'a str>,
    self_trait: Option<&'a str>,
    params: &'a HashMap<String, Vec<String>>,
}

impl ConstReadRewriter<'_> {
    /// Top-level constant a `Base::NAME` read resolves to, or `None` when
    /// the path names no associated constant reachable from here.
    fn owner_of(&self, base: &str, name: &str) -> Option<String> {
        if base == "Self" {
            if let Some(self_ty) = self.self_ty
                && let Some(owner) = self.index.assoc_const_owner_for_self(self_ty, name)
            {
                return Some(owner);
            }
            let traits = self
                .index
                .with_supertraits(self.self_trait.into_iter().map(ToString::to_string).collect());
            return self.owner_through(&traits, name);
        }
        if let Some(bounds) = self.params.get(base) {
            let traits = self.index.with_supertraits(bounds.clone());
            return self.owner_through(&traits, name);
        }
        self.index.assoc_const_owner_for_self(base, name)
    }

    /// First of `traits` that resolves `name` to a single owner.
    fn owner_through(&self, traits: &[String], name: &str) -> Option<String> {
        traits
            .iter()
            .find_map(|t| match self.index.assoc_const_owner_for_trait(t, name) {
                AssocResolution::Found(owner) => Some(owner),
                AssocResolution::Ambiguous | AssocResolution::Unknown => None,
            })
    }
}

impl gossamer_ast::VisitorMut for ConstReadRewriter<'_> {
    fn visit_expr(&mut self, e: &mut Expr) {
        gossamer_ast::visitor::walk_expr_mut(self, e);
        let ExprKind::Path(path) = &mut e.kind else {
            return;
        };
        // The last two segments carry the read: `Gauge::MAX` and the
        // module-qualified `inner::Gauge::MAX` name the same constant.
        let Some((base, name)) = assoc_read_segments(path) else {
            return;
        };
        let Some(owner) = self.owner_of(&base, &name) else {
            return;
        };
        *path = PathExpr {
            segments: vec![PathSegment::new(hoisted_name(&owner, &name))],
        };
    }
}

/// Base type or trait name and item name of an associated-item read. The
/// last two segments carry it, so a module-qualified path resolves the
/// same way as a bare one.
fn assoc_read_segments(path: &PathExpr) -> Option<(String, String)> {
    let count = path.segments.len();
    if count < 2 {
        return None;
    }
    Some((
        path.segments[count - 2].name.name.clone(),
        path.segments[count - 1].name.name.clone(),
    ))
}

/// Moves each associated constant's value into a top-level constant that
/// sits immediately before the item it came from, so a later constant can
/// read it exactly as it could read any earlier constant.
fn hoist_items(items: &mut Vec<Item>, ids: &mut AssocIds) {
    let mut out: Vec<Item> = Vec::with_capacity(items.len());
    for mut item in std::mem::take(items) {
        collect_hoists(&mut item, &mut out, ids);
        out.push(item);
    }
    *items = out;
}

fn collect_hoists(item: &mut Item, out: &mut Vec<Item>, ids: &mut AssocIds) {
    let span = item.span;
    match &mut item.kind {
        ItemKind::Impl(decl) => {
            let Some(owner) = assoc_type_head_name(&decl.self_ty).map(ToString::to_string) else {
                return;
            };
            for impl_item in &mut decl.items {
                if let ImplItem::Const {
                    name, ty, value, ..
                } = impl_item
                {
                    out.push(lift_const(&owner, name, ty, value, span, ids));
                }
            }
        }
        ItemKind::Trait(decl) => {
            let owner = decl.name.name.clone();
            for trait_item in &mut decl.items {
                if let TraitItem::Const {
                    name,
                    ty,
                    default: Some(value),
                    ..
                } = trait_item
                {
                    out.push(lift_const(&owner, name, ty, value, span, ids));
                }
            }
        }
        ItemKind::Mod(decl) => {
            if let ModBody::Inline(inner) = &mut decl.body {
                for inner_item in inner {
                    collect_hoists(inner_item, out, ids);
                }
            }
        }
        _ => {}
    }
}

fn lift_const(
    owner: &str,
    name: &Ident,
    ty: &gossamer_ast::Type,
    value: &mut Expr,
    span: gossamer_lex::Span,
    ids: &mut AssocIds,
) -> Item {
    let hoisted = hoisted_name(owner, &name.name);
    let read = Expr::new(
        ids.id(),
        value.span,
        ExprKind::Path(PathExpr::single(hoisted.clone())),
    );
    let init = std::mem::replace(value, read);
    let mut const_ty = ty.clone();
    gossamer_ast::VisitorMut::visit_type(&mut TypeRenum { ids }, &mut const_ty);
    Item::new(
        ids.id(),
        span,
        gossamer_ast::Attrs::default(),
        Visibility::Public,
        ItemKind::Const(ConstDecl {
            name: Ident::new(hoisted),
            ty: const_ty,
            value: init,
        }),
    )
}
