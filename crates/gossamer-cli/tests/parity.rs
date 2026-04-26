//! Interpreter ↔ native parity harness.
//!
//! Default `cargo test` runs only the `minimal_parity_*` smoke
//! tests against `examples/hello_world.gos`, which keeps the suite
//! under a couple of seconds. The full `every_example_*` walks
//! across the whole `examples/` directory live behind the
//! `exhaustive_tests` feature flag — invoke `exhaustive_test.sh`
//! at the workspace root, or `cargo test -p gossamer-cli --test
//! parity --features exhaustive_tests`, to run them.
//!
//! Post-L4 there is no launcher fallback; a failed native build
//! means a compiler bug and fails the suite.

#![allow(missing_docs)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let crate_dir = PathBuf::from(manifest_dir);
    crate_dir
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn examples_dir() -> PathBuf {
    workspace_root().join("examples")
}

/// Captured output of a single program execution.
struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

fn run_interpreter(source: &Path) -> Run {
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(source)
        .output()
        .expect("spawn gos run");
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        code: out.status.code(),
    }
}

/// Builds `source` natively and runs the produced artifact. Returns
/// `None` when the build fails — every legal program is expected to
/// compile after L4.
fn run_native(source: &Path) -> Option<Run> {
    let out_path = source.with_extension("");
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(source)
        .output()
        .expect("spawn gos build");
    if !build.status.success() {
        return None;
    }
    let run_out = Command::new(&out_path)
        .output()
        .expect("run native artifact");
    let _ = std::fs::remove_file(&out_path);
    Some(Run {
        stdout: String::from_utf8_lossy(&run_out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&run_out.stderr).into_owned(),
        code: run_out.status.code(),
    })
}

fn rel_to_workspace(path: &Path) -> String {
    let root = workspace_root();
    path.strip_prefix(&root)
        .map_or_else(
            |_| path.display().to_string(),
            |p| p.to_string_lossy().into_owned(),
        )
}

/// Single example used by the always-on smoke tests. Picked because
/// it terminates immediately, has zero external dependencies, and
/// covers the dominant `println` lowering path.
fn minimal_example() -> PathBuf {
    examples_dir().join("hello_world.gos")
}

#[test]
fn minimal_parity_runs_hello_world_through_the_interpreter() {
    // Smoke test: the interpreter executes the canonical example
    // with exit 0. Catches gross interpreter regressions in well
    // under a second.
    let path = minimal_example();
    let run = run_interpreter(&path);
    assert_eq!(
        run.code,
        Some(0),
        "{} failed under the interpreter: stderr={}",
        rel_to_workspace(&path),
        run.stderr,
    );
}

#[test]
fn minimal_parity_native_matches_interpreter_for_hello_world() {
    // Smoke test: the interpreter and native tiers produce
    // byte-identical output for the canonical example.
    let path = minimal_example();
    let interp = run_interpreter(&path);
    let native = run_native(&path).unwrap_or_else(|| {
        panic!("native build of {} failed", rel_to_workspace(&path));
    });
    assert_eq!(interp.stdout, native.stdout, "stdout diverged");
    assert_eq!(interp.stderr, native.stderr, "stderr diverged");
    assert_eq!(interp.code, native.code, "exit code diverged");
}

#[test]
fn every_example_with_committed_expected_matches_interpreter_stdout() {
    // Run-and-diff CI gate. For each `examples/<name>.gos` whose
    // sibling `examples/<name>.expected.txt` exists, the
    // interpreter's stdout must match the committed file
    // byte-for-byte.
    //
    // Examples without an expected file are exempt — they're
    // either non-deterministic (timing-driven `go expr`) or
    // depend on external state (network, stdin). Add an
    // `<name>.expected.txt` next to the source to opt the example
    // into the gate.
    let mut failures = Vec::new();
    for path in gos_examples_with_expected() {
        let expected_path = path.with_extension("expected.txt");
        let expected = std::fs::read_to_string(&expected_path)
            .expect("read expected.txt");
        let run = run_interpreter(&path);
        if run.stdout != expected {
            failures.push(format!(
                "{}: stdout diverged from {}\n  expected: {:?}\n  actual:   {:?}",
                rel_to_workspace(&path),
                rel_to_workspace(&expected_path),
                expected,
                run.stdout,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} example(s) drifted from committed expected output:\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

/// Returns every `examples/*.gos` whose sibling `<name>.expected.txt`
/// exists. Used by the always-on `every_example_with_committed_expected_*`
/// gate; the file count is small (~4 today) so this stays fast.
fn gos_examples_with_expected() -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = std::fs::read_dir(examples_dir())
        .expect("read examples dir")
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|e| e == "gos"))
        .filter(|p| p.with_extension("expected.txt").exists())
        .collect();
    out.sort();
    out
}

// ----------------------------------------------------------------
// Exhaustive walks. Compiled only when the `exhaustive_tests`
// feature is enabled (set by `exhaustive_test.sh`). The default
// `cargo test` skips the entire module so a hung example can't
// stall the routine suite.
// ----------------------------------------------------------------

#[cfg(feature = "exhaustive_tests")]
mod full {
    use super::*;

    /// Examples deliberately excluded from the parity walks because
    /// they are non-terminating, require external state, or depend
    /// on CLI args / a live server that the bare `gos run <path>`
    /// shape can't supply. Each is covered by a dedicated
    /// integration test elsewhere.
    const NON_TERMINATING_EXAMPLES: &[&str] = &[
        "web_server.gos",  // HTTP server runs until shutdown — would hang
        "http_client.gos", // expects a live `web_server.gos` on :8080
        "grep.gos",        // requires CLI args (PATTERN [FILE...])
    ];

    fn gos_examples() -> Vec<PathBuf> {
        let mut out: Vec<PathBuf> = std::fs::read_dir(examples_dir())
            .expect("read examples dir")
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|e| e == "gos"))
            .filter(|p| {
                let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
                !NON_TERMINATING_EXAMPLES.contains(&name)
            })
            .collect();
        out.sort();
        out
    }

    #[test]
    fn every_example_runs_cleanly_through_the_interpreter() {
        let mut failures = Vec::new();
        for path in gos_examples() {
            let key = rel_to_workspace(&path);
            let run = run_interpreter(&path);
            if run.code != Some(0) {
                failures.push(format!(
                    "{key}: interpreter exit={:?} stderr={}",
                    run.code, run.stderr
                ));
            }
        }
        assert!(
            failures.is_empty(),
            "{} example(s) failed under the interpreter:\n{}",
            failures.len(),
            failures.join("\n")
        );
    }

    #[test]
    fn interpreter_and_native_paths_agree_on_every_example() {
        let mut divergences = Vec::new();
        let mut build_failures = Vec::new();
        for path in gos_examples() {
            let key = rel_to_workspace(&path);
            let interp = run_interpreter(&path);
            let Some(native) = run_native(&path) else {
                build_failures.push(key);
                continue;
            };
            if interp.stdout != native.stdout
                || interp.stderr != native.stderr
                || interp.code != native.code
            {
                divergences.push(format!(
                    "{key}:\n  interp stdout: {:?}\n  native stdout: {:?}\n  interp code: {:?}\n  native code: {:?}",
                    interp.stdout, native.stdout, interp.code, native.code
                ));
            }
        }
        assert!(
            build_failures.is_empty(),
            "these examples failed to build natively:\n{}",
            build_failures.join("\n")
        );
        assert!(
            divergences.is_empty(),
            "{} example(s) diverged between interpreter and native:\n{}",
            divergences.len(),
            divergences.join("\n\n")
        );
    }
}
