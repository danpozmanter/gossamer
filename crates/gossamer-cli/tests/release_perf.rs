//! Release-tier silent-fallback regression gate.
//!
//! The 2026-04-30 spectral-norm incident (`spectral_norm_regression_fix.md`)
//! shipped a malformed `runtime_refs` entry that corrupted the LLVM IR
//! and forced a silent per-fn Cranelift fallback for unrelated bodies.
//! Spectral-norm slowed from 0.93s to 21.6s — a 23× regression — and
//! the existing test suite was *green*. The same shape regressed again
//! a few weeks later (`spectral_norm_regression_fix.md` 2026-04-30).
//!
//! `tier_parity.rs::llvm_release_lowers_every_example_without_fallback`
//! gates the CASE WHERE THE BUILD ITSELF CALLS THE FALLBACK (it errors
//! when `GOSSAMER_FAIL_ON_LLVM_FALLBACK=1`). But a regression that
//! lowers a body to a no-op stub or to wrong-but-runnable code passes
//! that gate. The only end-to-end signal for that class is wall-clock:
//! the release tier should not be markedly slower than the debug tier
//! on a workload where LLVM -O3 has a real edge over Cranelift -O2.
//!
//! This test builds a numeric-loop workload twice — `gos build` (debug
//! Cranelift) and `gos build --release` (LLVM) — runs each, and
//! asserts the release wall-clock is no worse than the debug one. A
//! small noise margin is allowed because LLVM compile time can dwarf
//! the program's runtime on tiny workloads. The workload is sized so
//! the release tier should win comfortably (~3-10×) on any reasonable
//! laptop, leaving plenty of headroom for noise.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!(
        "gos-relperf-{}-{}-{}",
        std::process::id(),
        tag,
        rand_suffix(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn rand_suffix() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64)
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

fn build(src: &Path, release: bool, scratch: &Path) -> PathBuf {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    assert!(
        out.status.success(),
        "gos build (release={release}) failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    for entry in fs::read_dir(scratch).unwrap().flatten() {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            return p;
        }
    }
    panic!("no binary in {}", scratch.display());
}

/// Runs `bin` `runs` times, returns the best (lowest) wall-clock
/// duration. Best-of-N filters jitter from concurrent CI load.
fn time_best(bin: &Path, runs: u32) -> Duration {
    let mut best = Duration::from_secs(u64::MAX);
    for _ in 0..runs {
        let start = Instant::now();
        let out = Command::new(bin).output().expect("spawn bin");
        let dur = start.elapsed();
        assert!(
            out.status.success(),
            "binary exited non-zero: stderr={}",
            String::from_utf8_lossy(&out.stderr),
        );
        if dur < best {
            best = dur;
        }
    }
    best
}

/// A numeric loop where LLVM -O3 has a clear edge over Cranelift
/// -O2: i64 multiply-add chain, no allocations, no calls inside
/// the loop. Sized so total runtime is ~50-200ms in release and
/// ~500ms-2s in debug on a 2026-era laptop. Outputs a final
/// scalar so the optimizer can't dead-code-eliminate the loop.
const NUMERIC_LOOP_SOURCE: &str = r#"
fn main() {
    let n: i64 = 50000000
    let mut acc: i64 = 0
    let mut i: i64 = 0
    while i < n {
        acc = acc + i * i - i
        i = i + 1
    }
    println!("acc={}", acc)
}
"#;

#[test]
fn release_tier_is_at_least_as_fast_as_debug_on_numeric_loop() {
    // Skip silently when LLVM tooling isn't on PATH — matches the
    // existing pattern in tier_parity. Without LLVM the release
    // build is just Cranelift again, so the comparison is
    // meaningless.
    if which_llc_missing() {
        eprintln!("skipping: LLVM tooling not on PATH");
        return;
    }
    let dir = fresh_dir("numeric_loop");
    let src = dir.join("loop.gos");
    fs::write(&src, NUMERIC_LOOP_SOURCE).unwrap();
    let dbg_dir = dir.join("dbg");
    fs::create_dir_all(&dbg_dir).unwrap();
    let rel_dir = dir.join("rel");
    fs::create_dir_all(&rel_dir).unwrap();

    let dbg_bin = build(&src, false, &dbg_dir);
    let rel_bin = build(&src, true, &rel_dir);

    let dbg_time = time_best(&dbg_bin, 3);
    let rel_time = time_best(&rel_bin, 3);
    let _ = fs::remove_dir_all(&dir);

    eprintln!("debug (cranelift): {dbg_time:?}");
    eprintln!("release (llvm):    {rel_time:?}");

    // `release` must be no slower than `debug` plus a small
    // noise margin. The historical silent-fallback regression
    // made release ≈ debug (LLVM body fell back to Cranelift,
    // so identical wall-clock plus LLVM build-time overhead).
    // 1.10× tolerance accommodates jitter without letting a
    // 23× spectral-norm-style regression sneak through.
    let bound = dbg_time.mul_f64(1.10);
    assert!(
        rel_time <= bound,
        "release tier ({rel_time:?}) is slower than debug tier ({dbg_time:?}) — \
         this is the silent-fallback fingerprint from `spectral_norm_regression_fix.md`. \
         Re-run with `GOS_LLVM_DUMP=1` and inspect /tmp/gos-llvm-*/unit.ll for missing \
         user-fn `define` blocks or stale runtime_refs entries.",
    );
}

fn which_llc_missing() -> bool {
    if std::env::var("GOS_LLC").is_ok() {
        return false;
    }
    for cand in ["llc-18", "llc"] {
        if Command::new(cand).arg("--version").output().is_ok() {
            return false;
        }
    }
    true
}
