//! Tests for `gos doc --emit-stdlib` and `--check`.

use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop(); // deps/
    p.pop(); // debug/
    p.push("gos");
    p
}

fn tmp(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let p = std::env::temp_dir().join(format!("gos-doc-{tag}-{pid}-{n}"));
    std::fs::create_dir_all(&p).unwrap();
    p
}

#[test]
fn emit_stdlib_writes_one_page_per_module_plus_index() {
    let dir = tmp("emit");
    let out = Command::new(gos_bin())
        .args(["doc", "--emit-stdlib"])
        .arg(&dir)
        .output()
        .expect("spawn gos doc");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("stdlib pages to"));
    // At minimum: index plus a handful of well-known modules.
    assert!(dir.join("index.md").exists());
    assert!(dir.join("http.md").exists());
    assert!(dir.join("io.md").exists());
    assert!(dir.join("crypto_sha256.md").exists());
    let index = std::fs::read_to_string(dir.join("index.md")).unwrap();
    assert!(index.contains("std::http"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_stdlib_check_passes_on_fresh_emit() {
    let dir = tmp("check_pass");
    // Emit, then verify with --check.
    let emit = Command::new(gos_bin())
        .args(["doc", "--emit-stdlib"])
        .arg(&dir)
        .output()
        .expect("emit");
    assert!(emit.status.success());

    let check = Command::new(gos_bin())
        .args(["doc", "--emit-stdlib"])
        .arg(&dir)
        .arg("--check")
        .output()
        .expect("check");
    assert!(
        check.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    assert!(String::from_utf8_lossy(&check.stdout).contains("in sync"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_stdlib_check_fails_on_drift() {
    let dir = tmp("check_fail");
    // Emit, then mutate one page to force drift.
    Command::new(gos_bin())
        .args(["doc", "--emit-stdlib"])
        .arg(&dir)
        .output()
        .expect("emit");
    let mutated_page = dir.join("io.md");
    std::fs::write(&mutated_page, "garbage content").unwrap();

    let check = Command::new(gos_bin())
        .args(["doc", "--emit-stdlib"])
        .arg(&dir)
        .arg("--check")
        .output()
        .expect("check");
    assert!(!check.status.success(), "check must fail on drift");
    let stderr = String::from_utf8_lossy(&check.stderr);
    assert!(stderr.contains("drift") || stderr.contains("io.md"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn doc_without_file_or_emit_stdlib_errors() {
    let out = Command::new(gos_bin())
        .args(["doc"])
        .output()
        .expect("spawn gos doc");
    assert!(!out.status.success());
}
