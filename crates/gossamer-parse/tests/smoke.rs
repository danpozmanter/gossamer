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

/// A leading UTF-8 BOM (the Windows-editor default) is stripped at the
/// parse entry, so a BOM-prefixed file parses identically to a plain
/// one rather than choking on the marker.
#[test]
fn leading_bom_parses_like_plain_source() {
    let with = "\u{feff}fn main() {\n    let x = 1\n}\n";
    let without = "fn main() {\n    let x = 1\n}\n";
    let mut map = SourceMap::new();
    let fa = map.add_file("with_bom.gos", with.to_string());
    let fb = map.add_file("plain.gos", without.to_string());
    let (sf_a, diags_a) = parse_source_file(with, fa);
    let (sf_b, diags_b) = parse_source_file(without, fb);
    assert!(
        diags_a.is_empty(),
        "BOM-prefixed source must parse cleanly; got {} diag(s)",
        diags_a.len()
    );
    assert_eq!(sf_a.items.len(), sf_b.items.len());
    assert_eq!(diags_a.len(), diags_b.len());
}

/// Regression: a leading BOM shifted token spans off the parser's
/// source basis, so `Parser::slice` split the BOM mid-char and
/// panicked on this fuzz input (`fuzz/artifacts/typecheck/crash-fbf8…`).
/// Parsing arbitrary bytes must never panic.
#[test]
fn bom_prefixed_malformed_input_does_not_panic() {
    let src = "\u{feff}fn\"\u{4}n\"\u{4}";
    let mut map = SourceMap::new();
    let file = map.add_file("crash.gos", src.to_string());
    let _ = parse_source_file(src, file);
}

/// Regression: nested generic argument lists close with a maximal-munch
/// `>>` (or `>>=` / `>=`) token. The type/turbofish/generic-param parsers
/// must split that token into the closing `>` for each level instead of
/// rejecting it (`Vec<Vec<String>>`, `HashMap<String, Vec<i64>>`).
#[test]
fn nested_generics_closing_shift_right_parses() {
    let source = concat!(
        "fn f(a: Vec<Vec<String>>, b: Vec<Vec<Vec<i64>>>) -> Vec<Vec<i64>> {\n",
        "    b\n",
        "}\n",
    );
    let mut map = SourceMap::new();
    let file = map.add_file("nested_generics.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(
        diags.is_empty(),
        "nested generics with `>>` must parse cleanly; got {} diag(s)",
        diags.len()
    );
    assert_eq!(sf.items.len(), 1, "expected exactly one item (`fn f`)");
}

/// Regression: the match-scrutinee / loop-condition struct-literal
/// restriction must be suspended inside delimited sub-expressions -
/// call arguments, parentheses, index brackets, array literals, and
/// blocks. `match http::serve("addr", App { }) { .. }` used to fail
/// with "unexpected `{`, expected `)` to close argument list".
#[test]
fn struct_literal_in_delimited_scrutinee_positions_parses() {
    let source = concat!(
        "struct App { n: i64 }\n",
        "fn pick(a: App) -> i64 { a.n }\n",
        "fn f() -> i64 {\n",
        "    match pick(App { n: 1 }) {\n",
        "        1 => 10,\n",
        "        _ => 0,\n",
        "    }\n",
        "}\n",
        "fn g() -> i64 {\n",
        "    if pick(App { n: 2 }) == 2 { 1 } else { 0 }\n",
        "}\n",
        "fn h() -> i64 {\n",
        "    while pick(App { n: 0 }) == 99 { }\n",
        "    match (pick(App { n: 3 }), [pick(App { n: 4 })]) {\n",
        "        (3, _) => 3,\n",
        "        _ => 0,\n",
        "    }\n",
        "}\n",
    );
    let mut map = SourceMap::new();
    let file = map.add_file("struct_lit_scrutinee.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    for diag in &diags {
        eprintln!("  {diag}");
    }
    assert!(
        diags.is_empty(),
        "struct literals inside delimited scrutinee sub-expressions must parse; \
         got {} diag(s)",
        diags.len()
    );
    assert_eq!(sf.items.len(), 5, "expected five items");
}
