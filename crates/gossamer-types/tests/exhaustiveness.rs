//! End-to-end tests for match exhaustiveness and unreachable-arm
//! detection.

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{ExhaustivenessError, TyCtxt, check_exhaustiveness, typecheck_source_file};

fn run(source: &str) -> Vec<gossamer_types::ExhaustivenessDiagnostic> {
    let mut map = SourceMap::new();
    let file = map.add_file("test.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    check_exhaustiveness(&sf, &resolutions, &table, &tcx)
}

#[test]
fn bool_match_missing_false_is_reported() {
    let source = r"
fn main() {
    let x = true
    match x {
        true => 1i32,
    }
}
";
    let diagnostics = run(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(&d.error, ExhaustivenessError::NonExhaustive { missing } if missing.iter().any(|m| m == "false"))),
        "expected missing `false`: {diagnostics:?}"
    );
}

#[test]
fn bool_match_with_wildcard_is_exhaustive() {
    let source = r"
fn main() {
    let x = true
    match x {
        true => 1i32,
        _ => 0i32,
    }
}
";
    let diagnostics = run(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn bool_match_with_both_literals_is_exhaustive() {
    let source = r"
fn main() {
    let x = true
    match x {
        true => 1i32,
        false => 0i32,
    }
}
";
    let diagnostics = run(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn enum_match_missing_variant_is_reported() {
    let source = r"
enum Dir { North, South, East, West }

fn main() {
    let d = Dir::North
    match d {
        Dir::North => 0i32,
        Dir::South => 1i32,
    }
}
";
    let diagnostics = run(source);
    assert!(
        diagnostics.iter().any(|d| matches!(
            &d.error,
            ExhaustivenessError::NonExhaustive { missing } if missing.iter().any(|m| m == "East") && missing.iter().any(|m| m == "West")
        )),
        "expected missing East+West: {diagnostics:?}"
    );
}

#[test]
fn enum_match_with_all_variants_is_exhaustive() {
    let source = r"
enum Dir { North, South, East, West }

fn main() {
    let d = Dir::North
    match d {
        Dir::North => 0i32,
        Dir::South => 1i32,
        Dir::East => 2i32,
        Dir::West => 3i32,
    }
}
";
    let diagnostics = run(source);
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn arm_after_wildcard_is_unreachable() {
    let source = r"
fn main() {
    let x = true
    match x {
        _ => 0i32,
        true => 1i32,
    }
}
";
    let diagnostics = run(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d.error, ExhaustivenessError::UnreachableArm)),
        "expected unreachable diagnostic: {diagnostics:?}"
    );
}

#[test]
fn guarded_wildcard_does_not_trigger_unreachable() {
    let source = r"
fn main() {
    let x = true
    match x {
        _ if x => 0i32,
        true => 1i32,
        false => 2i32,
    }
}
";
    let diagnostics = run(source);
    assert!(
        diagnostics
            .iter()
            .all(|d| !matches!(d.error, ExhaustivenessError::UnreachableArm)),
        "unexpected unreachable: {diagnostics:?}"
    );
}

#[test]
fn duplicate_bool_literal_is_unreachable() {
    let source = r"
fn main() {
    let x = true
    match x {
        true => 1i32,
        true => 2i32,
        false => 0i32,
    }
}
";
    let diagnostics = run(source);
    assert!(
        diagnostics
            .iter()
            .any(|d| matches!(d.error, ExhaustivenessError::UnreachableArm)),
        "expected duplicate-literal diagnostic: {diagnostics:?}"
    );
}

fn non_exhaustive(diagnostics: &[gossamer_types::ExhaustivenessDiagnostic]) -> bool {
    diagnostics
        .iter()
        .any(|d| matches!(d.error, ExhaustivenessError::NonExhaustive { .. }))
}

// 0.18.1: an `i64` scrutinee with no wildcard arm was treated as
// exhaustive, so the compiled tier ran off the end of the dispatch and
// SIGSEGV'd. A non-enumerable scalar now requires a catch-all.
#[test]
fn int_match_without_wildcard_is_non_exhaustive() {
    let diagnostics = run("fn f(n: i64) -> i64 { match n { 0 => 10, 1 => 20, } }\n");
    assert!(non_exhaustive(&diagnostics), "{diagnostics:?}");
}

#[test]
fn int_match_with_wildcard_is_exhaustive() {
    let diagnostics = run("fn f(n: i64) -> i64 { match n { 0 => 10, _ => 0, } }\n");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn int_match_with_binding_catch_all_is_exhaustive() {
    let diagnostics = run("fn f(n: i64) -> i64 { match n { 0 => 10, other => other, } }\n");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn string_match_without_wildcard_is_non_exhaustive() {
    let diagnostics = run("fn f(s: String) -> i64 { match s { \"a\" => 1, \"b\" => 2, } }\n");
    assert!(non_exhaustive(&diagnostics), "{diagnostics:?}");
}

// 0.18.1: `Option` / `Result` are built-in sentinel ADTs absent from the
// user-enum table, so a missing-variant match was treated as exhaustive
// and the compiled tier read an uninitialised discriminant (garbage).
#[test]
fn option_match_missing_none_is_non_exhaustive() {
    let diagnostics = run("fn f(o: Option<i64>) -> i64 { match o { Some(n) => n, } }\n");
    assert!(non_exhaustive(&diagnostics), "{diagnostics:?}");
}

#[test]
fn option_match_both_arms_is_exhaustive() {
    let diagnostics = run("fn f(o: Option<i64>) -> i64 { match o { Some(n) => n, None => 0, } }\n");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

#[test]
fn result_match_missing_err_is_non_exhaustive() {
    let diagnostics = run("fn f(r: Result<i64, i64>) -> i64 { match r { Ok(n) => n, } }\n");
    assert!(non_exhaustive(&diagnostics), "{diagnostics:?}");
}

#[test]
fn result_match_both_arms_is_exhaustive() {
    let diagnostics =
        run("fn f(r: Result<i64, i64>) -> i64 { match r { Ok(n) => n, Err(e) => e, } }\n");
    assert!(diagnostics.is_empty(), "{diagnostics:?}");
}

// 0.18.1: a guarded-only `Some` arm does not prove `Some` is covered, so
// `Some(0)` fell through; with `Option` now enumerable this is reported.
#[test]
fn guarded_only_some_arm_leaves_some_uncovered() {
    let diagnostics =
        run("fn f(o: Option<i64>) -> i64 { match o { Some(n) if n > 0 => n, None => 0, } }\n");
    assert!(non_exhaustive(&diagnostics), "{diagnostics:?}");
}

#[test]
fn example_programs_have_no_spurious_exhaustiveness_errors() {
    for name in ["hello_world.gos", "line_count.gos", "web_server.gos"] {
        let path = format!("{}/../../examples/{name}", env!("CARGO_MANIFEST_DIR"));
        let source = std::fs::read_to_string(&path).expect("read example");
        let diagnostics = run(&source);
        let non_exhaustive: Vec<_> = diagnostics
            .iter()
            .filter(|d| matches!(d.error, ExhaustivenessError::NonExhaustive { .. }))
            .collect();
        assert!(
            non_exhaustive.is_empty(),
            "{path}: spurious non-exhaustive: {non_exhaustive:?}"
        );
    }
}
