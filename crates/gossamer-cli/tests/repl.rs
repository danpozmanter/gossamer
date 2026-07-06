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
fn repl_mutable_assignment_persists_across_lines() {
    // Regression (issue #14): reassigning a `let mut` binding from an earlier
    // input was applied in a throwaway frame and discarded, so a later read
    // still saw the original value.
    let out = run_repl("let mut name = \"Steven\"\nname = \"Mark\"\nname\n");
    assert!(out.success, "repl should exit zero; stderr: {}", out.stderr);
    assert!(
        out.stdout.contains("Out[3]: Mark"),
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
