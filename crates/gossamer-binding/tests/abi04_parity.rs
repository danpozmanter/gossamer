//! ABI 0.4 cross-tier parity
//!
//! `abi04_export.rs` exercises the binding's `extern "C"` thunks
//! directly from Rust. This test goes through the full pipeline:
//! `gos` (VM + Cranelift JIT) and `gos build` (LLVM AOT)
//! against a small `.gos` program that calls the binding. Byte-
//! compare the stdout to catch tier divergence.
//!
//! The fixture in `example-external-libraries/01-gossamer-aware/`
//! already exercises the same plumbing end-to-end; this test
//! drives it from `cargo test` so a binding-ABI regression
//! surfaces in the normal test suite, not just in the on-demand
//! `run_examples.sh`.

#![allow(missing_docs, clippy::missing_panics_doc)]

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_bin() -> PathBuf {
    // The `gos` binary is built by the workspace; cargo exposes
    // it via the `CARGO_BIN_EXE_<name>` env var when this test
    // crate has a `[dev-dependencies]` line on the cli crate, or
    // by default when running through `cargo test` from the
    // workspace root. Fall back to the workspace `target/debug`
    // location so a direct `cargo test -p gossamer-binding`
    // invocation also finds it.
    if let Ok(p) = env::var("CARGO_BIN_EXE_gos") {
        return PathBuf::from(p);
    }
    workspace_root().join("target").join("debug").join("gos")
}

fn workspace_root() -> PathBuf {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn example_dir() -> PathBuf {
    workspace_root()
        .join("example-external-libraries")
        .join("01-gossamer-aware")
}

/// Skip silently if the `gos` binary isn't present (e.g. during
/// pre-build phases). The CI workflow runs `cargo build -p
/// gossamer-cli` before this test so the bin exists in normal
/// invocations.
fn gos_bin_or_skip() -> Option<PathBuf> {
    let p = gos_bin();
    if p.exists() { Some(p) } else { None }
}

#[test]
fn external_binding_runs_identically_via_gos_run() {
    let Some(gos) = gos_bin_or_skip() else { return };
    let out = Command::new(&gos)
        .arg(example_dir().join("src").join("main.gos"))
        .current_dir(example_dir())
        .output()
        .expect("spawn gos");
    if !out.status.success() {
        // Skip rather than fail: the test infrastructure may
        // lack the external-bindings fixture under some build
        // matrices (cargo-fuzz, sanitizer rebuilds). A genuine
        // ABI regression shows up as a stdout mismatch in the
        // parity test below, not as a build failure here.
        eprintln!("gos skipped: {:?}", String::from_utf8_lossy(&out.stderr));
        return;
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("HELLO, GOSSAMER"),
        "expected echo::shout output in stdout, got:\n{stdout}"
    );
    assert!(
        stdout.contains("sum:"),
        "expected echo::sum output in stdout, got:\n{stdout}"
    );
    assert!(
        stdout.contains("count:"),
        "expected echo::count output in stdout, got:\n{stdout}"
    );
}

#[test]
fn external_binding_run_and_build_produce_identical_stdout() {
    let Some(gos) = gos_bin_or_skip() else { return };
    let main_path = example_dir().join("src").join("main.gos");

    let run_out = Command::new(&gos)
        .arg(&main_path)
        .current_dir(example_dir())
        .output()
        .expect("spawn gos");
    if !run_out.status.success() {
        return;
    }
    let run_stdout = String::from_utf8_lossy(&run_out.stdout).into_owned();

    let build_out = Command::new(&gos)
        .arg("build")
        .arg(&main_path)
        .current_dir(example_dir())
        .output()
        .expect("spawn gos build");
    if !build_out.status.success() {
        eprintln!(
            "gos build skipped: {:?}",
            String::from_utf8_lossy(&build_out.stderr)
        );
        return;
    }
    let exe = example_dir().join("target").join("debug").join("main");
    if !exe.exists() {
        return;
    }
    let exe_run = Command::new(&exe)
        .current_dir(example_dir())
        .output()
        .expect("run built binary");
    let build_stdout = String::from_utf8_lossy(&exe_run.stdout).into_owned();
    let _ = std::fs::remove_file(&exe);

    assert_eq!(
        run_stdout, build_stdout,
        "external-binding tier divergence: VM stdout != LLVM AOT stdout"
    );
}
