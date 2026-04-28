//! Item-attribute walkers shared by `gos test` and `gos bench`.

use std::path::PathBuf;

use anyhow::Result;

use crate::loaders::load_and_check_with_sf;
use crate::paths::read_source;

/// Returns `true` when `item` carries an outer attribute whose final
/// path segment is `name` — used to detect `#[test]` / `#[bench]`.
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
/// `gos test` uses this to discover `#[test]`-annotated functions
/// that sit inside a `#[cfg(test)] mod tests { ... }` block.
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

/// Loads `file`, runs frontend checks, then invokes every selected
/// function under the tree-walker `iterations` times. Returns
/// `(passes, failures, total_nanos)` for the bench harness.
pub(crate) fn run_selected_fns(
    file: &PathBuf,
    selector: impl Fn(&gossamer_ast::Item) -> bool,
    iterations: u32,
) -> Result<(u32, u32, u128)> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (program, sf, _tcx) = load_and_check_with_sf(&source, file_id, &map)?;
    let mut selected: Vec<String> = Vec::new();
    collect_selected_fn_names(&sf.items, &selector, &mut selected);
    if selected.is_empty() {
        return Ok((0, 0, 0));
    }
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(&program);
    let mut passes = 0u32;
    let mut failures = 0u32;
    let mut total_nanos: u128 = 0;
    for name in &selected {
        for _ in 0..iterations {
            let started = std::time::Instant::now();
            match interp.call(name, Vec::new()) {
                Ok(_) => {
                    total_nanos += started.elapsed().as_nanos();
                    passes += 1;
                }
                Err(err) => {
                    eprintln!("  FAIL {name}: {err}");
                    failures += 1;
                    break;
                }
            }
        }
    }
    Ok((passes, failures, total_nanos))
}
