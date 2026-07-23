//! End-to-end tests for the interactive REPL (`gos repl`).
//!
//! Drives the binary's stdin with a fixed input script and asserts
//! against captured stdout / stderr. The REPL prints the banner and
//! per-input results to stdout; runtime errors and parse-error
//! summaries go to stderr. With stdin piped (not a TTY) rustyline
//! falls back to its dumb-terminal reader, so the prompt itself
//! never lands in captured output.

use std::env;
use std::fmt::Write as _;
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
fn repl_rejects_rebinding_a_reference_with_its_referent() {
    // `let mut` permits assigning a new `&[i64; 2]`, not replacing the
    // reference binding with a bare `[i64; 2]` value.
    let out = run_repl("let mut x = &[1, 2]\nx = [2, 3]\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("type mismatch"),
        "reference-rebinding mismatch was not reported: {}",
        out.stderr
    );
    assert!(
        out.stderr
            .contains("expected `&[i64; 2]`, found `[i64; 2]`"),
        "reference-rebinding mismatch did not preserve both types: {}",
        out.stderr
    );
}

#[test]
fn repl_reference_rebind_to_temporary_does_not_mutate_old_referent() {
    let out = run_repl(
        "let a = [1, 2]\n\
         let b = [3, 4]\n\
         let mut c = &a\n\
         c = &b\n\
         c = &[5, 6]\n\
         let mut x = [10, 20]\n\
         let mut y = [30, 40]\n\
         let mut r = &mut x\n\
         r = &mut y\n\
         r = &mut [50, 60]\n\
         %bindings\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("a = [1, 2]"),
        "immutable original binding changed or disappeared: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("b = [3, 4]"),
        "immutable referent binding was overwritten by reference rebind: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("mut c = [5, 6]"),
        "reference binding did not move to the new temporary referent: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("b = [5, 6]"),
        "reference rebind leaked through and mutated immutable `b`: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("mut x = [10, 20]") && out.stdout.contains("mut y = [30, 40]"),
        "mutable reference rebind changed an old named referent: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("mut r = [50, 60]"),
        "mutable reference binding did not move to the new temporary referent: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("mut y = [50, 60]"),
        "mutable reference rebind leaked through and mutated old `y`: {}",
        out.stdout
    );
}

#[test]
fn repl_issue_49_cannot_mutate_immutable_value_through_mutable_reference_chain() {
    let out = run_repl(
        "let a = [1, 2]\n\
         let mut b = &a\n\
         let mut c = &mut b\n\
         c[0] = 0\n\
         %bindings\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr
            .contains("cannot assign through shared reference `c`"),
        "issue #49 write must be rejected as a shared-reference violation: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("a = [1, 2]"),
        "immutable source changed through the rejected alias chain: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("a = [0, 2]"),
        "immutable source was modified despite GT0031: {}",
        out.stdout
    );
}

#[test]
fn repl_reference_assignment_error_shows_concrete_type() {
    let out = run_repl("let mut a = 12\nlet mut b = &mut a\nb = 16\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr
            .contains("type mismatch: expected `&mut i64`, found `{integer}`"),
        "reference mismatch must show the resolved type: {}",
        out.stderr
    );
    assert!(
        !out.stderr.contains("&mut ?"),
        "inference variable leaked into REPL diagnostic: {}",
        out.stderr
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
fn repl_bindings_and_declarations_accept_regex_filters() {
    let out = run_repl(
        "let alpha_value = 11\n\
         let beta_value = 22\n\
         fn AlphaFn(value: i64) -> i64 { value }\n\
         fn BetaFn(value: String) -> String { value }\n\
         %bindings ^alpha.*11$\n\
         %declarations Alpha.*i64\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("  1: alpha_value = 11"),
        "binding regex should match anywhere in rendered bindings: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("beta_value = 22"),
        "binding regex should exclude non-matches: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("  1: fn AlphaFn(value: i64) -> i64 { value }"),
        "declaration regex should match anywhere in source text: {}",
        out.stdout
    );
    assert!(
        !out.stdout
            .contains("fn BetaFn(value: String) -> String { value }"),
        "declaration regex should exclude non-matches: {}",
        out.stdout
    );
}

#[test]
fn repl_bindings_and_declarations_report_invalid_regex() {
    let out = run_repl("let value = 1\nfn value_of() -> i64 { 1 }\n%b [\n%d [\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("invalid bindings regex `[`"),
        "invalid binding regex should produce a useful error: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("invalid declarations regex `[`"),
        "invalid declaration regex should produce a useful error: {}",
        out.stderr
    );
}

#[test]
fn repl_struct_construction_and_display_match_source_shapes() {
    let out = run_repl(
        "struct Pair { x: i64, y: i64 }\n\
         struct Tup(String, i64)\n\
         let p = Pair { x: 0, y: 0 }\n\
         p\n\
         let q = Pair { y: 4, 3 }\n\
         q\n\
         let t = Tup(\"row\", 7)\n\
         t\n\
         %declarations\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("binding added (1 total)"),
        "named struct literal should bind; stdout: {}; stderr: {}",
        out.stdout,
        out.stderr
    );
    assert!(
        out.stdout.contains("Out[4]: Pair { x: 0, y: 0 }"),
        "named struct value should render with fields; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[6]: Pair { x: 3, y: 4 }"),
        "mixed named struct literal should render in declaration order; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[8]: Tup(\"row\", 7)"),
        "tuple struct value should render with parentheses; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("  1: struct Pair { x: i64, y: i64 }"),
        "%declarations should list accumulated declarations; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("  2: struct Tup(String, i64)"),
        "%declarations should list tuple struct declarations; stdout: {}",
        out.stdout
    );

    let legacy = run_repl("struct Marker\n");
    assert!(
        legacy
            .stderr
            .contains("`{ field: Type }`, `(Type, ...)`, or `;` after a struct name"),
        "bare unit declarations must be rejected; stderr: {}",
        legacy.stderr
    );
}

#[test]
fn repl_rejects_plain_tuple_for_tuple_struct_function_parameter() {
    let out = run_repl(
        "struct RGB(i64, i64, i64)\n\
         let color = RGB(0, 100, 100)\n\
         let three = (1, 500, -200)\n\
         fn print_color(color: RGB) { println!(\"{}\", color) }\n\
         print_color(color)\n\
         print_color(three)\n",
    );
    assert!(
        out.success,
        "repl should remain live; stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("RGB(0, 100, 100)"),
        "valid nominal argument did not execute: {}",
        out.stdout
    );
    assert!(
        out.stderr
            .contains("expected `RGB`, found `(i64, i64, i64)`"),
        "nominal mismatch was not reported: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("(1, 500, -200)"),
        "rejected call still executed: {}",
        out.stdout
    );
}

#[test]
fn repl_open_ranges_are_lazy_and_printable() {
    let out = run_repl("10..\n..10\n..=10\n(10..).take(5) |> iter::collect()\n10..=\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for expected in [
        "Out[1]: 10..",
        "Out[2]: ..10",
        "Out[3]: ..=10",
        "Out[4]: [10, 11, 12, 13, 14]",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing `{expected}`; stdout: {}",
            out.stdout
        );
    }
    assert!(
        out.stderr
            .contains("inclusive range operator `..=` requires an upper bound"),
        "missing precise inclusive-range diagnostic; stderr: {}",
        out.stderr
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
fn repl_only_accepts_documented_quit_commands() {
    let out = run_repl("%exit\n1 + 2\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("unknown meta-command: %exit"),
        "%exit should not terminate the REPL; stderr: {}",
        out.stderr
    );
    assert!(
        out.stdout.contains("Out[1]: 3"),
        "the REPL did not continue after %exit; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_preserves_base_banner() {
    let out = run_repl("%help\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout
            .contains("%bindings (%b) [regex]  %declarations (%d) [regex]"),
        "bare %help should document filtering session entries; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("%reset (%r)  %help (%h)  %ls (%l)  %find (%f) <regex>"),
        "bare %help should list the remaining shortcuts; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("`let` bindings persist across inputs."),
        "bare %help should keep the existing REPL summary; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_command_shortcuts_match_their_long_forms() {
    let out = run_repl(
        "let answer = 42\n\
         fn identity(value: i64) -> i64 { value }\n\
         %b\n\
         %d\n\
         %l strings\n\
         %f strings.*trim\n\
         %h strings::trim\n\
         %r\n\
         %b\n\
         %d\n\
         %q\n\
         1 + 1\n",
    );
    assert!(out.success, "shortcut session failed: {}", out.stderr);
    assert!(
        out.stdout.contains("answer = 42"),
        "%b did not render bindings: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("fn identity(value: i64) -> i64 { value }"),
        "%d did not render declarations: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("std::strings::trim"),
        "%l, %f, or %h did not reach stdlib discovery: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("session cleared")
            && out.stdout.contains("no `let` bindings yet")
            && out.stdout.contains("no declarations yet"),
        "%r did not clear session state: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("Out["),
        "%q did not stop before the trailing expression: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_find_regex_searches_modules_functions_and_types() {
    let out = run_repl("%find http.*serv\n%find json.*Valu\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::http::serve") && out.stdout.contains("fn"),
        "regex function lookup should find http::serve: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("std::encoding::json::Value") && out.stdout.contains("type"),
        "regex public-type lookup should find json::Value: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_find_plain_text_is_a_name_substring() {
    let out = run_repl("%find part\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::iter::partition")
            && out.stdout.contains("std::http::multipart::Part"),
        "plain text should match contiguous symbol-name text: {}",
        out.stdout
    );
    assert!(
        !out.stdout.contains("std::strings::pad_right")
            && !out.stdout.contains("std::path::parent")
            && !out.stdout.contains("std::process::abort")
            && !out.stdout.contains("std::net::netip::join_addr_port"),
        "plain text should not use fuzzy subsequence matching: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_find_accepts_regex_operators() {
    let out = run_repl("%find p.*rt\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("std::iter::partition")
            && out.stdout.contains("std::net::netip::join_addr_port"),
        "regex operators should broaden find matches: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_find_reports_invalid_regex() {
    let out = run_repl("%find [\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("invalid find regex `[`"),
        "invalid regex should produce a useful error: {}",
        out.stderr
    );
}

#[test]
fn repl_meta_find_requires_a_query() {
    let out = run_repl("%find\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("usage: %find"),
        "empty find should show usage: {}",
        out.stderr
    );
}

#[test]
fn repl_iter_receiver_methods_pipe_dotdot_and_range_index_work() {
    let out = run_repl(
        "let a = [1, 2, 3, 4, 5]\n\
         a.skip(2)\n\
         a.enumerate()\n\
         a.zip(0..).collect()\n\
         a |> iter::zip(..) |> _.collect()\n\
         a[..2]\n\
         [1, 1, 2, 2].dedup()\n\
         a.windows(2)\n\
         a.chunks(2)\n\
         a.pairwise()\n\
         [[1, 2], [3]].flatten()\n\
         a.rev()\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for expected in [
        "Out[2]: [3, 4, 5]",
        "Out[3]: [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]",
        "Out[4]: [(1, 0), (2, 1), (3, 2), (4, 3), (5, 4)]",
        "Out[5]: [(0, 1), (1, 2), (2, 3), (3, 4), (4, 5)]",
        "Out[6]: [1, 2]",
        "Out[7]: [1, 2]",
        "Out[8]: [[1, 2], [2, 3], [3, 4], [4, 5]]",
        "Out[9]: [[1, 2], [3, 4], [5]]",
        "Out[10]: [(1, 2), (2, 3), (3, 4), (4, 5)]",
        "Out[11]: [1, 2, 3]",
        "Out[12]: [5, 4, 3, 2, 1]",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing `{expected}` from issue 44 regression output: {}",
            out.stdout
        );
    }
}

#[test]
fn repl_iter_take_rejects_negative_counts() {
    let out = run_repl("let a = [1, 2, 3]\na.take(-2)\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stderr.contains("count must be non-negative"),
        "negative take should be rejected instead of clamped: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("Out[2]: []"),
        "negative take must not silently return an empty Vec: {}",
        out.stdout
    );
}

#[test]
fn repl_rejects_negative_size_arguments_across_stdlib() {
    let out = run_repl(
        "strings::repeat(\"x\", -1)\n\
         strings::splitn(\"a,b\", -1, \",\")\n\
         strings::pad_left(\"x\", -1, ' ')\n\
         strings::replacen(\"aaa\", \"a\", \"b\", -1)\n\
         let xs = [1, 2, 3]\n\
         xs.take(-1)\n\
         xs.step_by(-1)\n\
         xs.windows(-1)\n\
         iter::repeat(1, -1)\n\
         let v: Vec<i64> = Vec::with_capacity(-1)\n\
         String::with_capacity(-1)\n\
         let m: HashMap<String, i64> = HashMap::with_capacity(-1)\n\
         image::new(-1, 1)\n\
         time::sleep(-1)\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for expected in [
        "strings::repeat: count must be non-negative",
        "strings::splitn: count must be non-negative",
        "strings::pad_left: width must be non-negative",
        "strings::replacen: count must be non-negative",
        "Vec::take: count must be non-negative",
        "Vec::step_by: count must be non-negative",
        "iter::windows: count must be non-negative",
        "iter::repeat: count must be non-negative",
        "Vec::with_capacity: capacity must be non-negative",
        "String::with_capacity: capacity must be non-negative",
        "HashMap::with_capacity: capacity must be non-negative",
        "image::new: width must be non-negative",
        "time::sleep: duration_ms must be non-negative",
    ] {
        assert!(
            out.stderr.contains(expected),
            "missing `{expected}` from negative-size regression stderr: {}",
            out.stderr
        );
    }
    for forbidden in [
        "Out[1]: \"\"",
        "Out[2]: []",
        "Out[6]: []",
        "Out[9]: []",
        "Out[10]: []",
        "Out[11]: \"\"",
    ] {
        assert!(
            !out.stdout.contains(forbidden),
            "negative size argument was silently accepted: {}",
            out.stdout
        );
    }
}

#[test]
fn repl_vec_slice_rejects_bad_arity_and_argument_types() {
    let out = run_repl(
        "let a = [1, 2, 3, 4, 5]\n\
         a.slice(1, 3)\n\
         a.slice(1..3)\n\
         a.slice(..3)\n\
         a.slice(..)\n\
         a.slice(2)\n\
         a.slice(\"two\")\n\
         a.slice(\"two\", 3)\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[2]: Ok([2, 3])"),
        "valid Vec::slice call should still work: {}",
        out.stdout
    );
    assert!(
        out.stderr.contains("type mismatch"),
        "non-integer slice bounds should be rejected by typing: {}",
        out.stderr
    );
    assert!(
        out.stderr.contains("Vec::slice") && out.stderr.contains("takes 2 argument"),
        "wrong Vec::slice arity should be diagnosed: {}",
        out.stderr
    );
    assert!(
        !out.stdout.contains("Ok([])"),
        "invalid Vec::slice calls must not silently return Ok([]): {}",
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
fn repl_meta_help_ls_and_find_cover_core_string_parse() {
    let out = run_repl(
        "%help string::parse\n\
         %help String::parse\n\
         %ls string\n\
         %find String::parse\n\
         \"123\".parse()\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("String::parse [method]"),
        "expected core method help; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout
            .contains("fn parse<T>(self: String) -> Result<T, errors::Error>"),
        "expected parse signature; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("String::parse")
            && out
                .stdout
                .contains("Parses the string into the expected result type."),
        "expected %ls and %find to expose String::parse; stdout: {}",
        out.stdout
    );
    assert!(
        out.stdout.contains("Out[1]: Ok(123)"),
        "existing String::parse execution must still work; stdout: {}",
        out.stdout
    );
}

#[test]
fn repl_meta_help_ls_and_find_cover_core_collection_types() {
    let out = run_repl(
        "%help Vec::new\n\
         %help Vec::push\n\
         %ls Vec\n\
         %find Vec::join\n\
         %help HashMap::new\n\
         %help HashMap::insert\n\
         %ls HashMap\n\
         %find HashMap::pop\n\
         %help BTreeMap::new\n\
         %help BTreeMap::insert\n\
         %ls BTreeMap\n\
         %help HashSet::union\n\
         %ls HashSet\n\
         %help VecDeque::push_back\n\
         %ls VecDeque\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for expected in [
        "Vec::new [assoc]",
        "Vec::push [method]",
        "Vec::join",
        "HashMap::new [assoc]",
        "HashMap::insert [method]",
        "HashMap::pop",
        "BTreeMap::new [assoc]",
        "BTreeMap::insert [method]",
        "HashSet::union [method]",
        "VecDeque::push_back [method]",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing `{expected}` from collection metadata output: {}",
            out.stdout
        );
    }
    assert!(
        out.stderr.is_empty(),
        "collection metadata lookups should not fail: {}",
        out.stderr
    );
}

#[test]
fn repl_meta_help_covers_builtin_receiver_types() {
    let out = run_repl(
        "%help Option::map\n\
         %help Result::map_err\n\
         %help Iterator::collect\n\
         %help Stream::write\n\
         %help Child::wait\n\
         %help AtomicI64::new\n\
         %help http::Response::text\n\
         %help validate::Errors::add\n\
         %ls Option\n\
         %ls http::Response\n",
    );
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for expected in [
        "Option::map [method]",
        "Result::map_err [method]",
        "Iterator::collect [method]",
        "Stream::write [method]",
        "Child::wait [method]",
        "AtomicI64::new [assoc]",
        "http::Response::text [method]",
        "validate::Errors::add [method]",
    ] {
        assert!(
            out.stdout.contains(expected),
            "missing `{expected}` from builtin receiver metadata output: {}",
            out.stdout
        );
    }
    assert!(
        out.stderr.is_empty(),
        "builtin receiver metadata lookups should not fail: {}",
        out.stderr
    );
}

#[test]
fn repl_meta_help_covers_every_builtin_macro_and_prelude_assertion() {
    let mut input = String::new();
    for builtin in gossamer_parse::builtin_macros::BUILTIN_MACROS {
        writeln!(&mut input, "%help {}", builtin.name).expect("write macro-help input");
    }
    input.push_str("%help assert\n%help assert_eq\n");
    let out = run_repl(&input);
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    for builtin in gossamer_parse::builtin_macros::BUILTIN_MACROS {
        assert!(
            out.stdout.contains(&format!("{} [macro]", builtin.name)),
            "missing help for {}: {}",
            builtin.name,
            out.stdout
        );
        assert!(
            out.stdout.contains(builtin.signature),
            "missing signature for {}: {}",
            builtin.name,
            out.stdout
        );
    }
    assert!(
        out.stdout.contains("assert [builtin]") && out.stdout.contains("assert_eq [builtin]"),
        "missing prelude assertion help: {}",
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
