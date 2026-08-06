//! End-to-end tests for the name resolver driven by parsed AST fixtures.

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::{PrimitiveTy, Resolution, ResolveError, resolve_source_file};

fn parse(source: &str) -> gossamer_ast::SourceFile {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(diags.is_empty(), "parse errors: {diags:?}");
    sf
}

#[test]
fn retained_resolver_oom_reproducer_terminates() {
    const REPRO: &[u8] = include_bytes!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../fuzz/artifacts/resolve/oom-14d3e69bb91be48852f7693451d6081e1ef06af7"
    ));
    let source = std::str::from_utf8(REPRO).expect("retained resolver artifact is UTF-8");
    let mut map = SourceMap::new();
    let file = map.add_file("resolve-oom-repro.gos", source.to_owned());
    let (ast, _parse_diagnostics) = parse_source_file(source, file);
    let _ = resolve_source_file(&ast);
}

#[test]
fn simple_hello_world_resolves_without_diagnostics() {
    let source = "use std::fmt\n\nfn main() {\n    fmt::println(\"hello\")\n}\n";
    let sf = parse(source);
    let (resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    assert!(!resolutions.is_empty());
}

#[test]
fn undefined_name_produces_unresolved_diagnostic() {
    let source = "fn main() { xyzzy }\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert_eq!(diags.len(), 1);
    assert!(matches!(
        diags[0].error,
        ResolveError::UnresolvedName { ref name } if name == "xyzzy"
    ));
}

#[test]
fn duplicate_top_level_items_report_diagnostic() {
    let source = "fn foo() {}\nfn foo() {}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(
        diags
            .iter()
            .any(|d| matches!(&d.error, ResolveError::DuplicateItem { name } if name == "foo")),
        "expected duplicate-item diagnostic, got: {diags:?}"
    );
}

#[test]
fn primitive_types_are_always_in_scope() {
    let source = "fn add(x: i32, y: i32) -> i32 { x }\n";
    let sf = parse(source);
    let (resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let found_primitive = resolutions.sorted_entries().iter().any(|(_, res)| {
        matches!(
            res,
            Resolution::Primitive(PrimitiveTy::Int(gossamer_resolve::IntWidth::W32))
        )
    });
    assert!(found_primitive, "expected i32 primitive resolution");
}

#[test]
fn forward_reference_between_items_resolves() {
    let source = "fn main() { helper() }\nfn helper() {}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn let_binding_shadows_and_resolves_to_local() {
    let source = "fn main() {\n    let x = 1\n    let y = x\n}\n";
    let sf = parse(source);
    let (resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
    let has_local = resolutions
        .sorted_entries()
        .iter()
        .any(|(_, res)| matches!(res, Resolution::Local(_)));
    assert!(has_local, "expected local resolution for `x`");
}

#[test]
fn use_list_imports_each_name_into_scope() {
    let source = "use std::sync::atomic::{AtomicU64, Ordering}\n\nfn main() {\n    AtomicU64::new(0)\n    Ordering::Relaxed\n}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn imported_name_resolves_to_import_resolution() {
    let source = "use std::fmt\n\nfn main() {\n    fmt::println(\"x\")\n}\n";
    let sf = parse(source);
    let (resolutions, _diags) = resolve_source_file(&sf);
    let has_import = resolutions
        .sorted_entries()
        .iter()
        .any(|(_, res)| matches!(res, Resolution::Import { .. }));
    assert!(has_import, "expected import resolution for `fmt`");
}

#[test]
fn qualified_stdlib_module_requires_import() {
    let source = "fn main() {\n    env::args()\n}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(
        diags.iter().any(
            |diag| matches!(&diag.error, ResolveError::UnresolvedName { name } if name == "env")
        ),
        "expected unimported stdlib module to be unresolved: {diags:?}"
    );
}

#[test]
fn qualified_stdlib_module_resolves_after_import() {
    let source = "use std::env\n\nfn main() {\n    env::args()\n}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn full_stdlib_item_import_resolves_bound_tail_name() {
    let source =
        "use std::iter::skip_while\n\nfn main() {\n    skip_while(|x: i64| x < 3, [1, 2, 3])\n}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn bogus_multisegment_use_path_is_rejected() {
    for source in ["use iter\n", "use stp::iter\n", "use whatever::iter\n"] {
        let sf = parse(source);
        let (_resolutions, diags) = resolve_source_file(&sf);
        assert!(
            diags
                .iter()
                .any(|diag| matches!(diag.error, ResolveError::UnknownModulePath { .. })),
            "expected unknown module path diagnostic for {source:?}: {diags:?}"
        );
    }
}

#[test]
fn relative_use_paths_are_not_stdlib_typo_checked() {
    let source = "fn helper(a: i64, b: i64) -> i64 { a + b }\n\
#[cfg(test)]\n\
mod tests {\n\
    use super::helper as add\n\
    #[test]\n\
    fn adds() { let _ = add(1, 2) }\n\
}\n";
    let sf = parse(source);
    let (_resolutions, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn example_programs_resolve_without_diagnostics() {
    for name in ["hello_world.gos", "line_count.gos", "web_server.gos"] {
        let path = format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).expect("read example");
        let sf = parse(&source);
        let (_resolutions, diags) = resolve_source_file(&sf);
        let unresolved: Vec<_> = diags
            .iter()
            .filter(|d| matches!(d.error, ResolveError::UnresolvedName { .. }))
            .collect();
        assert!(unresolved.is_empty(), "{path} unresolved: {unresolved:?}");
    }
}

#[test]
fn use_names_a_local_module_from_the_crate_root() {
    // The bundler inlines a sibling file as `mod options { ... }`, so a
    // `use` may name it directly, through `root::`, or through `crate::`.
    let body = "mod options {\n    enum Colorize { Always, Never }\n    fn tag() -> i64 { 1 }\n}\n";
    for path in [
        "use options::Colorize",
        "use root::options::Colorize",
        "use crate::options::Colorize",
        "use options::{Colorize, tag}",
    ] {
        let source = format!("{path}\n\n{body}fn main() {{ }}\n");
        let sf = parse(&source);
        let (_, diags) = resolve_source_file(&sf);
        assert!(
            diags.is_empty(),
            "{path}: unexpected diagnostics: {diags:?}"
        );
    }
}

#[test]
fn use_names_a_nested_local_module() {
    let source = "use example::options::tag\n\nmod example {\n    mod options {\n        fn tag() -> i64 { 1 }\n    }\n}\nfn main() { }\n";
    let sf = parse(source);
    let (_, diags) = resolve_source_file(&sf);
    assert!(diags.is_empty(), "unexpected diagnostics: {diags:?}");
}

#[test]
fn use_of_an_unknown_module_path_still_reports() {
    let source = "use nowhere::Thing\n\nfn main() { }\n";
    let sf = parse(source);
    let (_, diags) = resolve_source_file(&sf);
    assert!(
        diags
            .iter()
            .any(|d| matches!(d.error, ResolveError::UnknownModulePath { .. })),
        "expected UnknownModulePath, got: {diags:?}"
    );
}

#[test]
fn renamed_container_names_report_their_replacement() {
    // The rename hint used to reach only `use` declarations, so a bare
    // `HashSet<i64>` in a signature or a `HashSet::new()` call reported a
    // plain missing name and left the reader to guess the new spelling.
    for source in [
        "fn f(s: HashSet<i64>) -> i64 { 0 }\nfn main() { }\n",
        "fn main() { let _s = HashSet::new() }\n",
        "fn f(m: HashMap<String, i64>) -> i64 { 0 }\nfn main() { }\n",
        "fn main() { let _d = VecDeque::new() }\n",
    ] {
        let sf = parse(source);
        let (_, diags) = resolve_source_file(&sf);
        assert!(
            diags
                .iter()
                .any(|d| matches!(d.error, ResolveError::RemovedStdItem { .. })),
            "no rename hint for:\n{source}\n{diags:?}"
        );
    }
}
