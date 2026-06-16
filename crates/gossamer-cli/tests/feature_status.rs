//! Integration test for `gos feature-status`.
//!
//! Drives the built `gos` binary end-to-end, asserting that each
//! flag combination produces the documented shape and that
//! `--check` enforces its CI gate.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-feature-status-{pid}-{n}-{tag}",
        pid = std::process::id(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(gos_bin())
        .args(args)
        .output()
        .expect("spawn gos");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).into_owned(),
        String::from_utf8_lossy(&output.stderr).into_owned(),
    )
}

#[test]
fn table_output_contains_known_items() {
    let (code, stdout, _) = run(&["feature-status"]);
    assert_eq!(code, 0, "exit 0 expected");
    assert!(stdout.contains("std::fmt"), "missing std::fmt: {stdout}");
    assert!(stdout.contains("lang::if_let"), "missing lang::if_let");
    assert!(stdout.contains("Status"), "header missing");
    assert!(stdout.contains("shipped"), "shipped tag missing");
}

#[test]
fn json_output_parses_back() {
    let (code, stdout, _) = run(&["feature-status", "--format", "json"]);
    assert_eq!(code, 0, "exit 0 expected");
    let trimmed = stdout.trim();
    assert!(trimmed.starts_with('['), "expected JSON array: {trimmed}");
    assert!(trimmed.ends_with(']'));
    assert!(trimmed.contains("\"name\":"));
    assert!(trimmed.contains("\"status\":"));
    // Spot-check by hunting for one specific entry.
    assert!(
        trimmed.contains("\"std::fmt\""),
        "stdlib entry should round-trip"
    );
}

#[test]
fn markdown_output_renders_table() {
    let (code, stdout, _) = run(&["feature-status", "--format", "markdown"]);
    assert_eq!(code, 0);
    assert!(
        stdout.contains("| Name | Status | Tier-Parity | Doc |"),
        "missing markdown header"
    );
}

#[test]
fn filter_glob_narrows_results() {
    let (code, stdout, _) = run(&["feature-status", "--filter", "std::http::*"]);
    assert_eq!(code, 0);
    for line in stdout.lines() {
        // Skip the header rows (Name / -----).
        if line.starts_with("Name ") || line.starts_with("---") || line.is_empty() {
            continue;
        }
        // Each non-header row's first column starts with std::http::.
        let first = line.split('|').next().unwrap_or("").trim();
        assert!(
            first.starts_with("std::http::"),
            "row {first:?} should be under std::http::",
        );
    }
    assert!(
        stdout.contains("std::http::"),
        "must show at least one http entry"
    );
}

#[test]
fn status_filter_narrows_to_one_lifecycle_stage() {
    let (code, stdout, _) = run(&["feature-status", "--status", "experimental"]);
    assert_eq!(code, 0);
    // Every non-header row must end with "experimental" in the
    // status column.
    let mut saw_any = false;
    for line in stdout.lines() {
        if line.starts_with("Name ") || line.starts_with("---") || line.is_empty() {
            continue;
        }
        saw_any = true;
        let parts: Vec<&str> = line.split('|').collect();
        if parts.len() >= 2 {
            assert!(
                parts[1].trim() == "experimental",
                "non-experimental row leaked: {line}",
            );
        }
    }
    assert!(saw_any, "registry should ship experimental items");
}

#[test]
fn check_mode_passes_when_shipped_items_have_tests() {
    let tmp = scratch("ok");
    let docs = tmp.join("docs_src");
    fs::create_dir_all(docs.join("language")).unwrap();
    fs::create_dir_all(docs.join("stdlib")).unwrap();

    // Single shipped item, with both doc page + passing sidecar entry.
    let sidecar = tmp.join("sidecar.json");
    fs::write(docs.join("language/if_let.md"), "Status: shipped\n").unwrap();
    fs::write(
        &sidecar,
        r#"[
  {"name":"lang::if_let","tiers":{"vm":"pass","cranelift":"pass","llvm":"pass"}}
]
"#,
    )
    .unwrap();

    // Filter down to the one path so unrelated registry entries don't
    // bring the check down.
    let (code, stdout, stderr) = run(&[
        "feature-status",
        "--check",
        "--filter",
        "lang::if_let",
        "--sidecar",
        sidecar.to_str().unwrap(),
        "--docs-root",
        docs.to_str().unwrap(),
    ]);
    assert_eq!(
        code, 0,
        "check should pass: stdout={stdout} stderr={stderr}"
    );
    assert!(stdout.contains("ok"), "expected ok line, got {stdout}");
}

#[test]
fn check_mode_fails_when_shipped_lacks_test() {
    let tmp = scratch("missing-test");
    let docs = tmp.join("docs_src");
    fs::create_dir_all(docs.join("language")).unwrap();
    // Doc page exists, but no sidecar - `--check` should bail.
    fs::write(docs.join("language/match.md"), "Status: shipped\n").unwrap();

    let sidecar = tmp.join("sidecar.json");
    fs::write(&sidecar, "[]\n").unwrap();

    let (code, _, stderr) = run(&[
        "feature-status",
        "--check",
        "--filter",
        "lang::match",
        "--sidecar",
        sidecar.to_str().unwrap(),
        "--docs-root",
        docs.to_str().unwrap(),
    ]);
    assert_ne!(code, 0, "check should fail when test sidecar is empty");
    assert!(
        stderr.contains("missing tier-parity test")
            || stderr.contains("feature-status check failed"),
        "expected failure message, got {stderr}",
    );
}

#[test]
fn check_mode_fails_when_shipped_lacks_doc_page() {
    let tmp = scratch("missing-doc");
    let docs = tmp.join("docs_src");
    fs::create_dir_all(docs.join("language")).unwrap();
    // Sidecar has a record but no docs page on disk.
    let sidecar = tmp.join("sidecar.json");
    fs::write(
        &sidecar,
        r#"[{"name":"lang::if","tiers":{"vm":"pass","cranelift":"pass","llvm":"pass"}}]"#,
    )
    .unwrap();
    let (code, _, stderr) = run(&[
        "feature-status",
        "--check",
        "--filter",
        "lang::if",
        "--sidecar",
        sidecar.to_str().unwrap(),
        "--docs-root",
        docs.to_str().unwrap(),
    ]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("missing doc page") || stderr.contains("check failed"),
        "expected missing-doc failure, got {stderr}",
    );
}

#[test]
fn unknown_format_returns_error() {
    let (code, _, stderr) = run(&["feature-status", "--format", "yaml"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("unknown"),
        "expected format error: {stderr}"
    );
}

#[test]
fn unknown_status_returns_error() {
    let (code, _, stderr) = run(&["feature-status", "--status", "bogus"]);
    assert_ne!(code, 0);
    assert!(
        stderr.contains("unknown"),
        "expected status error: {stderr}"
    );
}
