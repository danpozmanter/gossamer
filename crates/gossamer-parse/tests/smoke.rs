#![allow(missing_docs)]

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;

fn example(name: &str) -> String {
    format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn hello_world_parses_cleanly() {
    let path = example("hello_world.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!(
        "hello_world: {} uses, {} items, {} diags",
        sf.uses.len(),
        sf.items.len(),
        diags.len()
    );
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}

#[test]
fn web_server_parses_cleanly() {
    let path = example("web_server.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!(
        "web_server: {} uses, {} items, {} diags",
        sf.uses.len(),
        sf.items.len(),
        diags.len()
    );
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}

#[test]
fn line_count_parses_cleanly() {
    let path = example("line_count.gos");
    let source = std::fs::read_to_string(&path).unwrap();
    let mut map = SourceMap::new();
    let file = map.add_file(&path, source.clone());
    let (sf, diags) = parse_source_file(&source, file);
    eprintln!(
        "line_count: {} uses, {} items, {} diags",
        sf.uses.len(),
        sf.items.len(),
        diags.len()
    );
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(diags.is_empty(), "diagnostics should be empty");
}

/// Regression: `expr as i64 < width` must parse as a comparison, not as
/// the start of a generic argument list on `i64`. The bug surfaced
/// when the formatter stripped redundant parens from
/// `(out.len() as i64) < width` in `examples/list_dir.gos`. The fix
/// restricts `parse_type_path_segment` from consuming `<` after a
/// primitive type name (primitives never carry generics).
#[test]
fn cast_to_primitive_followed_by_lt_parses_as_comparison() {
    let source = "fn pad(s: i64, width: i64) {\n    while s as i64 < width {\n    }\n}\n";
    let mut map = SourceMap::new();
    let file = map.add_file("cast_lt.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(
        diags.is_empty(),
        "cast-then-comparison must not produce parse diagnostics; got {} diag(s)",
        diags.len()
    );
    assert_eq!(sf.items.len(), 1, "expected exactly one item (`fn pad`)");
}

/// Companion regression: `Vec<i64>` and friends must still parse as a
/// generic type argument list. The primitive-only narrowing in the
/// fix above should not regress generics on user / stdlib types.
#[test]
fn generic_arg_list_on_user_type_still_parses() {
    let source = "fn build() -> Vec<i64> {\n    Vec::new()\n}\n";
    let mut map = SourceMap::new();
    let file = map.add_file("vec_generic.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(
        diags.is_empty(),
        "Vec<i64> must still parse cleanly; got {} diag(s)",
        diags.len()
    );
    assert_eq!(sf.items.len(), 1);
}
