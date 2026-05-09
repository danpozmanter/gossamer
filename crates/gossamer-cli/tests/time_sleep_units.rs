//! `time::sleep(ms)` honours millisecond units across tiers.
//!
//! Regression coverage for the 2026-05-07 daemon-launch report:
//! the cranelift / LLVM dispatch routed `time::sleep` directly
//! to `gos_rt_sleep_ns`, treating the millisecond argument as
//! nanoseconds. A `time::sleep(1000)` waited ~1 microsecond
//! instead of one second, busy-spinning every poll loop in
//! `gos build` / `gos build --release` builds. The runtime now
//! exposes `gos_rt_sleep_ms` and both backends route the
//! `time::sleep` symbol through it.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-sleep-{pid}-{n}-{tag}",
        pid = std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn build_native(src: &Path, release: bool, scratch: &Path) -> Result<PathBuf, String> {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    if !out.status.success() {
        return Err(format!(
            "gos build {flag} failed:\n  stderr: {}",
            String::from_utf8_lossy(&out.stderr),
            flag = if release { "--release" } else { "" },
        ));
    }
    let mut binaries = Vec::new();
    for entry in fs::read_dir(scratch)
        .map_err(|e| format!("read_dir: {e}"))?
        .flatten()
    {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            binaries.push(p);
        }
    }
    binaries
        .into_iter()
        .next()
        .ok_or_else(|| format!("no binary in {}", scratch.display()))
}

/// Runs the binary, returns (stdout, `elapsed_millis`).
fn run_timed(bin: &Path) -> (String, u64) {
    let start = Instant::now();
    let out = Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn binary");
    let elapsed = start.elapsed().as_millis() as u64;
    (String::from_utf8_lossy(&out.stdout).into_owned(), elapsed)
}

/// Runs `gos run <src>`, returns (stdout, `elapsed_millis`).
fn run_vm_timed(src: &Path) -> (String, u64) {
    let start = Instant::now();
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn gos run");
    let elapsed = start.elapsed().as_millis() as u64;
    (String::from_utf8_lossy(&out.stdout).into_owned(), elapsed)
}

fn write_source(tag: &str, body: &str) -> (PathBuf, PathBuf) {
    let dir = fresh_dir(tag);
    let src = dir.join(format!("{tag}.gos"));
    let mut f = fs::File::create(&src).expect("write src");
    f.write_all(body.as_bytes()).unwrap();
    drop(f);
    (dir, src)
}

#[test]
fn time_sleep_one_second_actually_waits_at_least_900_ms_in_all_tiers() {
    // `time::sleep(1000)` must wait ≥ 0.9 s. The buggy
    // dispatch slept for nanoseconds and the binary returned
    // in well under 5 ms, so any threshold above noise (≥ 500
    // ms) catches the regression.
    let body = r#"
use std::time
fn main() {
    println!("before")
    time::sleep(1000)
    println!("after")
}
"#;
    let (dir, src) = write_source("sleep_one_second", body);

    let (vm_out, vm_ms) = run_vm_timed(&src);
    assert!(
        vm_out.contains("before") && vm_out.contains("after"),
        "vm did not print both before/after: {vm_out:?}"
    );
    assert!(vm_ms >= 900, "vm slept only {vm_ms} ms; expected ≥ 900");

    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let cl_bin = build_native(&src, false, &cl_dir).expect("cranelift build");
    let (cl_out, cl_ms) = run_timed(&cl_bin);
    assert!(
        cl_out.contains("before") && cl_out.contains("after"),
        "cranelift stdout: {cl_out:?}"
    );
    assert!(
        cl_ms >= 900,
        "cranelift slept only {cl_ms} ms; expected ≥ 900"
    );

    let ll_dir = dir.join("ll");
    fs::create_dir_all(&ll_dir).unwrap();
    let ll_bin = build_native(&src, true, &ll_dir).expect("llvm build");
    let (ll_out, ll_ms) = run_timed(&ll_bin);
    assert!(
        ll_out.contains("before") && ll_out.contains("after"),
        "llvm stdout: {ll_out:?}"
    );
    assert!(ll_ms >= 900, "llvm slept only {ll_ms} ms; expected ≥ 900");

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn time_sleep_zero_or_negative_does_not_block_in_all_tiers() {
    // Zero / negative durations must not block. Catches a
    // would-be regression where `gos_rt_sleep_ms` overflowed
    // the saturating multiply and slept forever.
    let body = r#"
use std::time
fn main() {
    time::sleep(0)
    time::sleep(-5)
    println!("done")
}
"#;
    let (dir, src) = write_source("sleep_zero", body);

    let (vm_out, vm_ms) = run_vm_timed(&src);
    assert!(vm_out.contains("done"));
    assert!(
        vm_ms < 2000,
        "vm should not block on zero sleep, took {vm_ms} ms"
    );

    let cl_dir = dir.join("cl");
    fs::create_dir_all(&cl_dir).unwrap();
    let cl_bin = build_native(&src, false, &cl_dir).expect("cranelift build");
    let (cl_out, cl_ms) = run_timed(&cl_bin);
    assert!(cl_out.contains("done"));
    assert!(
        cl_ms < 2000,
        "cranelift should not block on zero sleep, took {cl_ms} ms"
    );

    let ll_dir = dir.join("ll");
    fs::create_dir_all(&ll_dir).unwrap();
    let ll_bin = build_native(&src, true, &ll_dir).expect("llvm build");
    let (ll_out, ll_ms) = run_timed(&ll_bin);
    assert!(ll_out.contains("done"));
    assert!(
        ll_ms < 2000,
        "llvm should not block on zero sleep, took {ll_ms} ms"
    );

    let _ = fs::remove_dir_all(&dir);
}
