#![allow(missing_docs)]

use gossamer_lex::SourceMap;
use gossamer_parse::{ParseError, parse_source_file};

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

#[test]
fn unit_struct_declaration_accepts_bare_form() {
    let source = "struct Unit\n";
    let mut map = SourceMap::new();
    let file = map.add_file("unit_struct.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "`struct Unit` must parse cleanly; got {diags:?}"
    );
    assert_eq!(sf.items.len(), 1);
    let gossamer_ast::ItemKind::Struct(decl) = &sf.items[0].kind else {
        panic!("expected a struct item");
    };
    assert!(matches!(decl.body, gossamer_ast::StructBody::Unit));
}

#[test]
fn multiline_lists_use_newlines_and_tolerate_legacy_commas() {
    for source in [
        "struct Point {\n    x: i64\n    y: i64\n}\n",
        "struct Point {\n    x: i64,\n    y: i64,\n}\n",
        "enum Choice {\n    One\n    Two(i64)\n}\n",
        "fn add(\n    x: i64\n    y: i64\n) -> i64 { x + y }\nfn main() { add(\n    1\n    2\n) }\n",
    ] {
        let mut map = SourceMap::new();
        let file = map.add_file("newline_lists.gos", source.to_string());
        let (_, diags) = parse_source_file(source, file);
        assert!(diags.is_empty(), "`{source}` produced {diags:?}");
    }
}

#[test]
fn same_line_lists_require_commas() {
    for source in [
        "struct Point { x: i64 y: i64 }\n",
        "enum Choice { One Two }\n",
        "fn add(x: i64 y: i64) { }\n",
        "fn main() { add(1 2) }\n",
    ] {
        let mut map = SourceMap::new();
        let file = map.add_file("comma_lists.gos", source.to_string());
        let (_, diags) = parse_source_file(source, file);
        assert!(!diags.is_empty(), "`{source}` unexpectedly parsed");
    }
}

#[test]
fn statement_semicolons_are_always_rejected() {
    for source in [
        "use example;\n",
        "let x = 1;\n",
        "fn main() { let x = 9;\nprintln(x) }\n",
        "fn main() { println(1); println(2) }\n",
    ] {
        let mut map = SourceMap::new();
        let file = map.add_file("semicolon.gos", source.to_string());
        let (_, diags) = parse_source_file(source, file);
        assert!(!diags.is_empty(), "`{source}` unexpectedly parsed");
    }
}

#[test]
fn empty_named_struct_declaration_accepts_braces() {
    let source = "struct Unit {}\n";
    let mut map = SourceMap::new();
    let file = map.add_file("empty_named_struct.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "`struct Unit {{}}` must parse cleanly; got {diags:?}"
    );
    let gossamer_ast::ItemKind::Struct(decl) = &sf.items[0].kind else {
        panic!("expected a struct item");
    };
    assert!(matches!(
        &decl.body,
        gossamer_ast::StructBody::Named(fields) if fields.is_empty()
    ));
}

#[test]
fn empty_tuple_struct_declaration_accepts_parentheses() {
    let source = "struct Unit()\n";
    let mut map = SourceMap::new();
    let file = map.add_file("empty_tuple_struct.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "`struct Unit()` must parse cleanly; got {diags:?}"
    );
    let gossamer_ast::ItemKind::Struct(decl) = &sf.items[0].kind else {
        panic!("expected a struct item");
    };
    assert!(matches!(
        &decl.body,
        gossamer_ast::StructBody::Tuple(fields) if fields.is_empty()
    ));
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

#[test]
fn use_brace_group_accepts_multi_segment_paths() {
    // `use std::{env, encoding::json, strings}` - a multi-segment path
    // inside a brace group must parse cleanly and expand to the same
    // entries as the split form.
    let source = "use std::{env, encoding::json, strings}\n";
    let mut map = SourceMap::new();
    let file = map.add_file("grouped.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "grouped multi-segment use must parse: {diags:?}"
    );
    assert_eq!(sf.uses.len(), 1);
    let list = sf.uses[0].list.as_ref().expect("brace list");
    assert_eq!(list.len(), 3);
    // `env` and `strings` are single-segment; `encoding::json` carries the
    // `encoding` prefix with `json` as the bound name.
    assert!(list[0].prefix.is_empty() && list[0].name.name == "env");
    assert_eq!(
        list[1]
            .prefix
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>(),
        vec!["encoding"]
    );
    assert_eq!(list[1].name.name, "json");
    assert!(list[2].prefix.is_empty() && list[2].name.name == "strings");
}

#[test]
fn use_brace_group_accepts_nested_groups() {
    // `use std::{encoding::{json, yaml}, strings}` - a nested brace group.
    let source = "use std::{encoding::{json, yaml}, strings}\n";
    let mut map = SourceMap::new();
    let file = map.add_file("nested.gos", source.to_string());
    let (sf, diags) = parse_source_file(source, file);
    assert!(diags.is_empty(), "nested group must parse: {diags:?}");
    let list = sf.uses[0].list.as_ref().expect("brace list");
    let names: Vec<(Vec<&str>, &str)> = list
        .iter()
        .map(|e| {
            (
                e.prefix.iter().map(|s| s.name.as_str()).collect::<Vec<_>>(),
                e.name.name.as_str(),
            )
        })
        .collect();
    assert_eq!(
        names,
        vec![
            (vec!["encoding"], "json"),
            (vec!["encoding"], "yaml"),
            (vec![], "strings"),
        ]
    );
}

/// A direct `_` in a pipe call is replaced before resolution. This covers the
/// macro-expansion path as well as a non-trailing argument, which the default
/// data-last pipe rule cannot express.
#[test]
fn pipe_direct_argument_placeholder_parses_and_desugars() {
    let source = "use std::strings\nfn main() {\n\
        let greeting = \"world\" |> format!(\"hello, {}\", _)\n\
        let part = \"world\" |> strings::slice(_, 1, 4)\n\
    }\n";
    let mut map = SourceMap::new();
    let file = map.add_file("pipe_args.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "direct pipe placeholders must be consumed during parsing: {diags:?}"
    );
}

#[test]
fn pipe_rejects_multiple_direct_argument_placeholders() {
    let source = "fn main() {\n\
        let _ = 1 |> pair(_, _)\n\
        let _ = 1 |> outer(inner(_))\n\
    }\n";
    let mut map = SourceMap::new();
    let file = map.add_file("pipe_many_args.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags
            .iter()
            .any(|diag| matches!(diag.error, ParseError::PipePlaceholderInvalid)),
        "repeated or nested pipe placeholders need a focused parse error: {diags:?}"
    );
}

#[test]
fn pipe_accepts_dotdot_as_range_argument() {
    let source = "fn main() { let _ = [1, 2] |> iter::zip(..) |> _.collect() }\n";
    let mut map = SourceMap::new();
    let file = map.add_file("pipe_dotdot.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "`..` should remain a range argument in pipe calls: {diags:?}"
    );
}

#[test]
fn method_turbofish_shorthand_parses_only_before_calls() {
    let source = concat!(
        "struct Point { x: i64 }\n",
        "fn main() {\n",
        "    let left = Point { x: 1 }\n",
        "    let right = Point { x: 2 }\n",
        "    let a = \"12\".parse<i64>()\n",
        "    let b = \"34\".parse::<i64>()\n",
        "    let shorter = a.len < 3\n",
        "    let ordered = left.x < right.x\n",
        "}\n",
    );
    let mut map = SourceMap::new();
    let file = map.add_file("method_turbofish.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "method turbofish shorthand and field comparisons should parse cleanly: {diags:?}"
    );
}

#[test]
fn format_macro_family_rejects_missing_and_unused_positional_arguments() {
    for macro_name in ["format", "println", "print", "eprintln", "eprint", "panic"] {
        for (template, expected, found) in [("one", 0, 1), ("{}", 1, 0)] {
            let source = format!("fn main() {{ {macro_name}!(\"{template}\", \"two\") }}");
            let source = if found == 0 {
                format!("fn main() {{ {macro_name}!(\"{template}\") }}")
            } else {
                source
            };
            let mut map = SourceMap::new();
            let file = map.add_file("format_args.gos", source.clone());
            let (_sf, diags) = parse_source_file(&source, file);
            assert!(
                diags.iter().any(|diag| matches!(
                    diag.error,
                    ParseError::FormatArgumentCount {
                        expected: actual_expected,
                        found: actual_found,
                    } if actual_expected == expected && actual_found == found
                )),
                "{macro_name}!({template:?}) should reject its positional arguments: {diags:?}"
            );
        }
    }
}

#[test]
fn format_macro_family_requires_literal_templates() {
    for macro_name in ["format", "println", "print", "eprintln", "eprint", "panic"] {
        let source =
            format!("fn main() {{ let template = \"{{}}\"; {macro_name}!(template, \"value\") }}");
        let mut map = SourceMap::new();
        let file = map.add_file("format_literal.gos", source.clone());
        let (_sf, diags) = parse_source_file(&source, file);
        assert!(
            diags
                .iter()
                .any(|diag| matches!(diag.error, ParseError::FormatStringMustBeLiteral)),
            "{macro_name}! must require a literal template: {diags:?}"
        );
    }
}

#[test]
fn pipes_require_an_explicit_format_macro_placeholder() {
    let source = "fn main() {\n\
        let value = \"world\"\n\
        value |> println!(\"hello, {}\", _)\n\
    }\n";
    let mut map = SourceMap::new();
    let file = map.add_file("pipe_format_placeholder.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "an explicit placeholder should accept the piped value: {diags:?}"
    );

    for macro_name in ["format", "println", "print", "eprintln", "eprint", "panic"] {
        let source = format!("fn main() {{ \"world\" |> {macro_name}!(\"hello\") }}");
        let mut map = SourceMap::new();
        let file = map.add_file("pipe_format_implicit.gos", source.clone());
        let (_sf, diags) = parse_source_file(&source, file);
        assert!(
            diags
                .iter()
                .any(|diag| matches!(diag.error, ParseError::PipedFormatArgumentNeedsPlaceholder)),
            "{macro_name}! must not accept an implicit piped format value: {diags:?}"
        );
    }
}

#[test]
fn open_end_range_pattern_must_not_use_the_inclusive_marker() {
    let valid = "fn main() { let _ = match 1 { 1.. => 0, _ => 1 } }";
    let mut map = SourceMap::new();
    let file = map.add_file("open_end_range.gos", valid.to_string());
    let (_sf, diags) = parse_source_file(valid, file);
    assert!(
        diags.is_empty(),
        "`lo..` should be a valid open-end range: {diags:?}"
    );

    let invalid = "fn main() { let _ = match 1 { 1..= => 0, _ => 1 } }";
    let mut map = SourceMap::new();
    let file = map.add_file("invalid_open_end_range.gos", invalid.to_string());
    let (_sf, diags) = parse_source_file(invalid, file);
    assert!(
        diags
            .iter()
            .any(|diag| matches!(&diag.error, ParseError::InclusiveRangeMissingEnd)),
        "`lo..=` should require an upper bound: {diags:?}"
    );
}

#[test]
fn inclusive_value_range_requires_an_upper_bound() {
    for source in ["fn main() { 10..= }", "fn main() { ..= }"] {
        let mut map = SourceMap::new();
        let file = map.add_file("missing_range_end.gos", source.to_string());
        let (_sf, diags) = parse_source_file(source, file);
        assert_eq!(
            diags
                .iter()
                .filter(|diag| matches!(diag.error, ParseError::InclusiveRangeMissingEnd))
                .count(),
            1,
            "expected one precise inclusive-range diagnostic: {diags:?}"
        );
        let rendered = diags
            .iter()
            .find(|diag| matches!(diag.error, ParseError::InclusiveRangeMissingEnd))
            .expect("inclusive range diagnostic")
            .to_diagnostic();
        assert_eq!(rendered.code.as_str(), "GP0026");
        assert!(rendered.title.contains("requires an upper bound"));
        assert!(rendered.helps.iter().any(|help| help.contains("use `..`")));
    }
}

#[test]
fn match_arm_commas_are_optional_at_line_boundaries() {
    let source = concat!(
        "fn classify(n: i64) -> String {\n",
        "    match n {\n",
        "        ..0 => \"negative\"\n",
        "        0 => \"zero\",\n",
        "        _ => { \"positive\" }\n",
        "    }\n",
        "}\n",
    );
    let mut map = SourceMap::new();
    let file = map.add_file("comma_optional_match.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "comma-free match arms must parse: {diags:?}"
    );
}

#[test]
fn comma_free_match_arm_before_open_start_range_pattern_parses() {
    let source = concat!(
        "fn classify(n: i64) -> String {\n",
        "    match n {\n",
        "        0.. => \"non-negative\"\n",
        "        ..0 => \"negative\"\n",
        "        _ => \"other\"\n",
        "    }\n",
        "}\n",
    );
    let mut map = SourceMap::new();
    let file = map.add_file("open_range_comma_optional_match.gos", source.to_string());
    let (_sf, diags) = parse_source_file(source, file);
    assert!(
        diags.is_empty(),
        "comma-free open range match arms must parse: {diags:?}"
    );
}

#[test]
fn malformed_match_arms_report_one_local_error_each() {
    let cases = [
        ("fn main() { match 1 { 1 10 } }", 0),
        ("fn main() { match 1 { 1 => } }", 1),
        ("fn main() { match 1 { 1 => 10 2 => 20 } }", 2),
    ];
    for (source, expected) in cases {
        let mut map = SourceMap::new();
        let file = map.add_file("malformed_match.gos", source.to_string());
        let (_sf, diags) = parse_source_file(source, file);
        assert_eq!(diags.len(), 1, "unexpected diagnostic cascade: {diags:?}");
        let expected = match expected {
            0 => matches!(diags[0].error, ParseError::MatchArmMissingArrow { .. }),
            1 => matches!(diags[0].error, ParseError::MatchArmMissingBody),
            _ => matches!(diags[0].error, ParseError::MatchArmMissingSeparator),
        };
        assert!(expected, "wrong diagnostic: {diags:?}");
        let structured = diags[0].to_diagnostic();
        assert_eq!(
            structured.code.as_str(),
            match expected {
                true if matches!(diags[0].error, ParseError::MatchArmMissingArrow { .. }) => {
                    "GP0029"
                }
                true if matches!(diags[0].error, ParseError::MatchArmMissingBody) => "GP0030",
                _ => "GP0031",
            }
        );
        assert!(
            !structured.helps.is_empty(),
            "diagnostic needs actionable help"
        );
        assert!(
            !matches!(diags[0].error, ParseError::MixedEntryForms),
            "a match-arm error must not be misreported as an entry-form error"
        );
    }
}
