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
fn simple_hello_world_resolves_without_diagnostics() {
    let source = "use fmt\n\nfn main() {\n    fmt::println(\"hello\")\n}\n";
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
    let source = "use fmt\n\nfn main() {\n    fmt::println(\"x\")\n}\n";
    let sf = parse(source);
    let (resolutions, _diags) = resolve_source_file(&sf);
    let has_import = resolutions
        .sorted_entries()
        .iter()
        .any(|(_, res)| matches!(res, Resolution::Import { .. }));
    assert!(has_import, "expected import resolution for `fmt`");
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
