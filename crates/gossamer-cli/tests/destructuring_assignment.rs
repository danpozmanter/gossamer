//! Tuple-destructuring assignment `(a, b) = rhs` on every tier.
//!
//! The fixture is also a tier-parity spec, which proves the tiers agree
//! with each other. These tests pin the absolute output, so all three
//! agreeing on a wrong answer is still a failure.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};
use std::process::Command;

fn gos_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../feature-testing-examples/destructuring_assignment.gos")
}

const EXPECTED: &str = "\
1 1
1 1
2 3 1
5 6 7 8
9 10
hi #[1, 2, 3]
1 2 3
42
21 34
";

fn assert_stdout(label: &str, output: &std::process::Output) {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        output.status.success(),
        "{label} failed: {stderr}\n{stdout}"
    );
    assert_eq!(
        stdout, EXPECTED,
        "{label} stdout mismatch (stderr: {stderr})"
    );
}

#[test]
fn destructuring_assignment_matches_on_the_bytecode_vm() {
    let output = Command::new(gos_binary())
        .arg("run")
        .arg(fixture())
        .env("GOS_JIT", "0")
        .output()
        .expect("run gos");
    assert_stdout("bytecode VM", &output);
}

#[test]
fn destructuring_assignment_matches_under_the_jit() {
    let output = Command::new(gos_binary())
        .arg("run")
        .arg(fixture())
        .env("GOSSAMER_JIT_THRESHOLD", "1")
        .env_remove("GOS_JIT")
        .output()
        .expect("run gos");
    assert_stdout("Cranelift JIT", &output);
}

#[test]
fn destructuring_assignment_matches_in_a_native_build() {
    let dir = std::env::temp_dir().join(format!("gos-destructure-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let build = Command::new(gos_binary())
        .arg("build")
        .arg("--out-dir")
        .arg(&dir)
        .arg(fixture())
        .output()
        .expect("build gos");
    assert!(
        build.status.success(),
        "gos build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir.join("destructuring_assignment");
    let output = Command::new(&binary).output().expect("run native binary");
    assert_stdout("native build", &output);
    let _ = std::fs::remove_dir_all(&dir);
}
