//! Stream C.2 — per-lint regression coverage.
//! Each lint is exercised by a small Gossamer snippet and asserted
//! to fire (or not fire) at the expected place. New lints must add a
//! matching test here.

use gossamer_diagnostics::Diagnostic;
use gossamer_lex::SourceMap;
use gossamer_lint::{DAY_ONE_LINTS, Level, Registry, apply_attributes, lint_explanation, run};
use gossamer_parse::parse_source_file;

fn lint(source: &str) -> Vec<Diagnostic> {
    lint_with(source, Registry::with_defaults())
}

fn lint_with(source: &str, registry: Registry) -> Vec<Diagnostic> {
    let mut map = SourceMap::new();
    let file = map.add_file("t.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse errors: {parse_diags:?}");
    run(&sf, &registry)
}

fn has_code(diags: &[Diagnostic], code: &str) -> bool {
    diags.iter().any(|d| d.code.as_str() == code)
}

#[test]
fn unused_variable_fires_on_let_without_use() {
    let diags = lint("fn main() { let x = 1i64 }\n");
    assert!(has_code(&diags, "GL0001"), "got {:?}", diags_codes(&diags));
}

#[test]
fn unused_variable_silenced_by_underscore_prefix() {
    let diags = lint("fn main() { let _x = 1i64 }\n");
    assert!(!has_code(&diags, "GL0001"));
}

#[test]
fn unused_variable_silenced_when_read_later() {
    let diags = lint(
        "fn main() { let x = 1i64 let _y: i64 = x }\n",
    );
    assert!(!has_code(&diags, "GL0001"), "{:?}", diags_codes(&diags));
}

#[test]
fn unused_import_fires_on_free_standing_use() {
    let diags = lint("use fmt\nfn main() { }\n");
    assert!(has_code(&diags, "GL0002"));
}

#[test]
fn unused_import_silent_when_path_referenced() {
    let diags = lint("use fmt\nfn main() { fmt::println(\"hi\") }\n");
    assert!(!has_code(&diags, "GL0002"), "{:?}", diags_codes(&diags));
}

#[test]
fn unused_mut_variable_fires_when_never_reassigned() {
    let diags = lint(
        "fn main() { let mut x = 1i64 let _y: i64 = x }\n",
    );
    assert!(has_code(&diags, "GL0003"), "{:?}", diags_codes(&diags));
}

#[test]
fn unused_mut_variable_silent_when_reassigned() {
    let diags = lint(
        "fn main() { let mut x = 1i64 x = 2i64 let _y: i64 = x }\n",
    );
    assert!(!has_code(&diags, "GL0003"));
}

#[test]
fn needless_return_fires_on_trailing_return_stmt() {
    let diags = lint(
        "fn answer() -> i64 { return 42i64 }\n",
    );
    assert!(has_code(&diags, "GL0004"), "{:?}", diags_codes(&diags));
}

#[test]
fn needless_bool_fires_on_if_true_else_false() {
    let diags = lint(
        "fn demo(x: bool) -> bool { if x { true } else { false } }\n",
    );
    assert!(has_code(&diags, "GL0005"), "{:?}", diags_codes(&diags));
}

#[test]
fn comparison_to_bool_literal_fires() {
    let diags = lint(
        "fn demo(x: bool) -> bool { x == true }\n",
    );
    assert!(has_code(&diags, "GL0006"), "{:?}", diags_codes(&diags));
}

#[test]
fn single_match_fires_with_one_arm() {
    let diags = lint(
        "fn demo(x: i64) -> i64 { match x { _ => 1i64 } }\n",
    );
    assert!(has_code(&diags, "GL0007"), "{:?}", diags_codes(&diags));
}

#[test]
fn shadowed_binding_fires_on_redeclared_let() {
    let diags = lint(
        "fn main() { let x = 1i64 let x = 2i64 let _y: i64 = x }\n",
    );
    assert!(has_code(&diags, "GL0008"), "{:?}", diags_codes(&diags));
}

#[test]
fn unchecked_result_fires_on_let_wildcard_ok() {
    let diags = lint(
        "fn main() { let _ = Ok(1i64) }\n",
    );
    assert!(has_code(&diags, "GL0009"), "{:?}", diags_codes(&diags));
}

#[test]
fn empty_block_fires_on_bare_brace_pair() {
    let diags = lint("fn main() { { } }\n");
    assert!(has_code(&diags, "GL0010"), "{:?}", diags_codes(&diags));
}

#[test]
fn panic_in_main_fires_on_direct_call() {
    let diags = lint("fn main() { panic(\"bad\") }\n");
    assert!(has_code(&diags, "GL0011"), "{:?}", diags_codes(&diags));
}

#[test]
fn redundant_clone_fires_on_literal_receiver() {
    let diags = lint(
        "fn main() { let _ = 1i64.clone() }\n",
    );
    assert!(has_code(&diags, "GL0012"), "{:?}", diags_codes(&diags));
}

#[test]
fn double_negation_fires_on_not_not_expr() {
    let diags = lint(
        "fn demo(x: bool) -> bool { !!x }\n",
    );
    assert!(has_code(&diags, "GL0013"), "{:?}", diags_codes(&diags));
}

#[test]
fn self_assignment_fires_on_x_eq_x() {
    let diags = lint(
        "fn demo(x: i64) { x = x }\n",
    );
    assert!(has_code(&diags, "GL0014"), "{:?}", diags_codes(&diags));
}

#[test]
fn todo_macro_fires_on_todo_invocation() {
    // Gossamer has no user-defined macros; `todo` is a plain builtin
    // call. The lint still fires on invocations as a "finish me"
    // marker for work in progress.
    let diags = lint(
        "fn main() { todo() }\n",
    );
    assert!(has_code(&diags, "GL0015"), "{:?}", diags_codes(&diags));
}

#[test]
fn allow_level_silences_a_lint() {
    let mut registry = Registry::with_defaults();
    registry.set("unused_variable", Level::Allow);
    let diags = lint_with("fn main() { let x = 1i64 }\n", registry);
    assert!(!has_code(&diags, "GL0001"));
}

#[test]
fn deny_level_upgrades_to_error() {
    let mut registry = Registry::with_defaults();
    registry.set("unused_variable", Level::Deny);
    let diags = lint_with("fn main() { let x = 1i64 }\n", registry);
    assert!(
        diags
            .iter()
            .any(|d| d.code.as_str() == "GL0001"
                && matches!(
                    d.severity,
                    gossamer_diagnostics::Severity::Error
                ))
    );
}

#[test]
fn apply_attributes_respects_inline_allow() {
    let mut map = SourceMap::new();
    let source = "#[lint(allow(unused_variable))]\nfn main() { let x = 1i64 }\n";
    let file = map.add_file("t.gos", source.to_string());
    let (sf, _) = parse_source_file(source, file);
    let mut registry = Registry::with_defaults();
    for item in &sf.items {
        apply_attributes(&item.attrs, &mut registry);
    }
    let diags = run(&sf, &registry);
    assert!(!diags.iter().any(|d| d.code.as_str() == "GL0001"));
}

#[test]
fn every_day_one_lint_has_an_explanation() {
    for id in DAY_ONE_LINTS {
        assert!(
            lint_explanation(id).is_some(),
            "lint `{id}` is missing an explanation",
        );
    }
}

#[test]
fn day_one_set_has_at_least_fifteen_lints() {
    assert!(DAY_ONE_LINTS.len() >= 15);
}

fn diags_codes(diags: &[Diagnostic]) -> Vec<&str> {
    diags.iter().map(|d| d.code.as_str()).collect()
}
