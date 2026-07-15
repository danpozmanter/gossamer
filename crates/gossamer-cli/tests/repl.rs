//! End-to-end tests for the interactive REPL (`gos repl`).
//!
//! Drives the binary's stdin with a fixed input script and asserts
//! against captured stdout / stderr. The REPL prints the banner and
//! per-input results to stdout; runtime errors and parse-error
//! summaries go to stderr. With stdin piped (not a TTY) rustyline
//! falls back to its dumb-terminal reader, so the prompt itself
//! never lands in captured output.

use std::env;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::Duration;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

struct ReplOutput {
    stdout: String,
    stderr: String,
    success: bool,
}

/// Spawns `gos repl`, writes `input` to its stdin, waits for the
/// process to terminate, and returns the captured streams. EOF on
/// stdin terminates the loop cleanly via rustyline's `ReadlineError::Eof`
/// branch, so explicit `%quit` is optional.
fn run_repl(input: &str) -> ReplOutput {
    let mut child = Command::new(gos_bin())
        .arg("repl")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos repl");
    {
        let stdin = child.stdin.as_mut().expect("stdin handle");
        stdin.write_all(input.as_bytes()).expect("write stdin");
    }
    // Drop stdin by taking it out - closes the pipe so the REPL sees EOF.
    drop(child.stdin.take());

    // Bounded wait so a hung REPL fails fast rather than blocking CI.
    let start = std::time::Instant::now();
    loop {
        if child.try_wait().expect("try_wait").is_some() {
            break;
        }
        if start.elapsed() > Duration::from_secs(30) {
            let _ = child.kill();
            panic!("gos repl did not terminate within 30s");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let out = child.wait_with_output().expect("wait_with_output");
    ReplOutput {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        success: out.status.success(),
    }
}

#[test]
fn repl_evaluates_simple_expression() {
    let out = run_repl("1 + 2\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[1]: 3"),
        "expected `Out[1]: 3` in stdout; got: {}",
        out.stdout
    );
}

#[test]
fn repl_persists_bindings_across_lines() {
    let out = run_repl("let x = 5\nx * 2\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("binding added"),
        "expected binding-added confirmation; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[2]: 10"),
        "binding `x` did not persist; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_uses_repr_for_results_and_display_for_explicit_printing() {
    let out = run_repl(
        "let x = \"wow\"\n\
         x\n\
         println(x)\n\
         [x, \"ok\"]\n\
         \"ab\".chars()\n\
         \"ab\".bytes()\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[2]: \"wow\""),
        "bare string must be quoted: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("wow\n"),
        "println must use unquoted display text: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[4]: [\"wow\", \"ok\"]"),
        "nested string repr is wrong: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[5]: ['a', 'b']"),
        "char vectors must use char literals: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[6]: [97, 98]"),
        "String::bytes method must execute: {}",
        out.stdout
    );
}

#[test]
fn repl_decodes_byte_string_literals_without_prefix_or_quotes() {
    let out = run_repl("b'b'\nb\"b\"\nb\"a\\n\"\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[1]: 98"),
        "byte literal should render as its u8 value; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[2]: [98]"),
        "byte string should contain only body bytes; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[3]: [97, 10]"),
        "byte string escapes should decode before vector output; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_mutable_assignment_persists_across_lines() {
    // Regression (issue #14): reassigning a `let mut` binding from an earlier
    // input was applied in a throwaway frame and discarded, so a later read
    // still saw the original value.
    let out = run_repl("let mut name = \"Steven\"\nname = \"Mark\"\nname\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[3]: \"Mark\""),
        "reassignment to `name` did not persist; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("Steven"),
        "stale value returned after reassignment; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_compound_assignment_accumulates_across_lines() {
    // `+=` on a persisted binding must fold across inputs, in order.
    let out = run_repl("let mut c = 0\nc += 5\nc += 3\nc\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[4]: 8"),
        "compound assignment did not accumulate; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_bindings_show_current_shadowed_lets_only() {
    let out = run_repl("let i = 1\nlet mut i = 2\n%bindings\ni = 3\n%bindings\ni\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("binding added (1 total)"),
        "first binding count should be one; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("binding added (2 total)"),
        "shadowing let should replace the visible binding count; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("  1: mut i = 2"),
        "visible binding should show the current shadowing value; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("  1: mut i = 3"),
        "assignment should update the displayed current value; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("let i = 1") && !out.stdout.contains("let mut i = 2"),
        "`%bindings` must not show replay source lines; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[4]: 3"),
        "assignment must still apply to the active shadowing binding; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_bindings_show_immutable_values_without_let_prefix() {
    let out = run_repl("let i = 3\n%bindings\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("  1: i = 3"),
        "immutable binding should render as `name = value`; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("let i = 3"),
        "`%bindings` must not show the original let source; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_prints_runtime_error_without_crashing() {
    let out = run_repl("panic!(\"boom\")\n1 + 1\n");
    assert!(
        out.success,
        "repl should keep running after a runtime panic; stderr: {}",
        out.stderr
    );
    // The error line goes to stderr ("error: ..."); the recovery
    // expression's result lands on stdout. Both must be present.
    assert!(
        out.stderr.contains("boom") || out.stderr.contains("GX0005"),
        "panic message missing from stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("Out[2]: 2"),
        "REPL did not recover after the panic; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_handles_empty_input() {
    let out = run_repl("");
    assert!(
        out.success,
        "empty stdin should close cleanly with exit 0; stderr: {}",
        out.stderr
    );
    // Banner is the only stdout we expect; no `Out[` lines.
    assert!(
        !out.stdout.contains("Out["),
        "no expression should have been evaluated; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_handles_syntax_error_recovery() {
    let out = run_repl("let z = @@@\n1 + 2\n");
    assert!(
        out.success,
        "repl must survive a syntax error and exit zero; stderr: {}",
        out.stderr
    );
    // The bad line should not appear as a successful `Out[N]`.
    assert!(
        out.stdout.contains("Out[2]: 3"),
        "good input after a syntax error did not evaluate; stdout: {}\nstderr: {}",
        out.stdout,
        out.stderr
    );
}

#[test]
fn repl_evaluates_function_definition() {
    let out = run_repl("fn add(a: i64, b: i64) -> i64 { a + b }\nadd(1, 2)\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("added 1 declarations"),
        "expected declaration confirmation; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[2]: 3"),
        "user-defined fn was not callable from the next input; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_quit_terminates_with_exit_zero() {
    let out = run_repl("%quit\n");
    assert!(
        out.success,
        "%quit should exit zero; stderr: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("Out["),
        "no expression should evaluate before %quit; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_preserves_base_banner() {
    let out = run_repl("%help\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("meta-commands: %quit  %history  %bindings  %reset  %help  %ls"),
        "bare %help should keep the existing banner; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("`let` bindings persist across inputs."),
        "bare %help should keep the existing REPL summary; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_finds_stdlib_symbol() {
    let out = run_repl("%help strings::trim\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::strings::trim [fn]"),
        "expected qualified stdlib item help; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("Removes leading and trailing whitespace."),
        "expected manifest doc text; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_shows_a_function_signature_and_docs() {
    let out = run_repl("%help strings::slice\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains(
            "fn slice(text: String, start: i64, end: i64) -> Result<String, errors::Error>"
        ),
        "function help must include the complete public signature: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Safe byte-range slice"),
        "function help must retain documentation: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_uses_checker_exposed_stdlib_signatures() {
    let out = run_repl("%help fs::read\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("fn read(path: String) -> Result<Vec<u8>, io::Error>"),
        "expected generated catalog signature for fs::read: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_distinguishes_same_leaf_function_names_by_type() {
    let out = run_repl("%help count\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("fn count(text: String, needle: String | char) -> i64"),
        "strings count signature missing: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("fn count<T>(items: Vec<T>) -> i64"),
        "iter count signature missing: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_searches_regex() {
    let out = run_repl("%help /question_mark/\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("lang::question_mark (shipped)"),
        "expected feature-status regex match; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_ls_lists_modules() {
    let out = run_repl("%ls\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::strings"),
        "expected stdlib module list; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("module"),
        "expected module rows; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("experimental"),
        "%ls must not render feature status for modules; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_ls_lists_namespace_items() {
    let out = run_repl("%ls strings\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::strings"),
        "expected strings module row; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("std::strings::trim"),
        "expected strings item rows; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_ls_lists_the_complete_io_namespace() {
    let out = run_repl("%ls io\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for item in ["stdin", "stdout", "stderr", "ReadAll", "Copy"] {
        assert!(
            out.stdout.contains(&format!("std::io::{item}")),
            "%ls io omitted {item}; stdout: {}",
            out.stdout
        );
    }
}

#[test]
fn repl_meta_ls_filters_regex_to_modules_only() {
    let out = run_repl("%ls /std::regex/\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::regex"),
        "expected regex-filtered module; stdout: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("replace_all"),
        "%ls must not list function members; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_ls_rejects_functions() {
    let out = run_repl("%ls strings::slice\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("%ls accepts module names only"),
        "expected function rejection; stderr: {}",
        out.stderr
    );
}

#[test]
fn repl_rejects_invalid_string_call_arguments_before_execution() {
    let out = run_repl(
        "let s = \"abcde\"\n\
         s.slice(1)\n\
         s.slice(1..3)\n\
         s.slice(\"a\")\n\
         s.slice(1, 3)\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr
            .contains("strings::slice` takes 2 argument(s) but 1 were supplied"),
        "missing slice-end argument was not rejected: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("expected `i64`"),
        "non-integer slice argument was not rejected: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("Out[5]: Ok(\"bc\")"),
        "valid slice call should still run: {}",
        out.stdout
    );
}

#[test]
fn repl_reports_each_invalid_string_argument_once_with_its_value() {
    let out = run_repl("strings::count(1, \"a\")\nstrings::count(\"ab\", 1)\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert_eq!(
        out.stderr
            .matches("parameter `text` of `strings::count`")
            .count(),
        1,
        "the first invalid argument must be reported exactly once: {}",
        out.stderr
    );
    assert_eq!(
        out.stderr
            .matches("parameter `needle` of `strings::count`")
            .count(),
        1,
        "the second invalid argument must be reported exactly once: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("found `{integer}` (value `1`)"),
        "the diagnostic must include the supplied literal: {}",
        out.stderr
    );
}

#[test]
fn repl_reports_array_arguments_without_inference_variable_types() {
    let out = run_repl("strings::slice([1, 2, 3], 1, 2)\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("found `array`"),
        "array type should be user-facing: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains('?'),
        "diagnostic must not expose inference variables: {}",
        out.stderr
    );
}

#[test]
fn repl_rejects_unqualified_std_functions() {
    let out = run_repl("count(\"abc\", 'a')\nstrings::count(\"abc\")\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("cannot find `count` in this scope"),
        "unqualified std function must not dispatch ambiguously: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("strings::count` takes 2 argument(s) but 1 were supplied"),
        "qualified std function must still enforce arity: {}",
        out.stderr
    );
}
