//! Item-attribute walkers shared by `gos test` and `gos bench`.

/// Returns `true` when `item` carries an outer attribute whose final
/// path segment is `name` - used to detect `#[test]` / `#[bench]`.
pub(crate) fn item_has_attr(item: &gossamer_ast::Item, name: &str) -> bool {
    item.attrs.outer.iter().any(|a| {
        a.path
            .segments
            .last()
            .is_some_and(|seg| seg.name.name == name)
    })
}

/// Walks `items` in source order, including nested inline modules,
/// and appends the name of every `Fn` matched by `selector` to `out`.
/// `gos test` and `gos bench` use this to discover `#[test]`- and
/// `#[bench]`-annotated functions, including those nested inside an
/// inline `mod ...` block (e.g. `#[cfg(test)] mod tests { ... }`).
pub(crate) fn collect_selected_fn_names(
    items: &[gossamer_ast::Item],
    selector: &impl Fn(&gossamer_ast::Item) -> bool,
    out: &mut Vec<String>,
) {
    for item in items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) if selector(item) => {
                out.push(decl.name.name.clone());
            }
            gossamer_ast::ItemKind::Mod(mod_decl) => {
                if let gossamer_ast::ModBody::Inline(inner) = &mod_decl.body {
                    collect_selected_fn_names(inner, selector, out);
                }
            }
            _ => {}
        }
    }
}
