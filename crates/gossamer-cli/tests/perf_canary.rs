//! Performance canary for the compiled (`gos build --release`) tier.
//!
//! A tight scalar floating-point loop - the shape of spectral-norm's
//! `eval_A` matrix kernel - built at `-O3` and timed. The point is NOT
//! to benchmark precisely (CI machines vary) but to catch catastrophic
//! per-call / per-iteration overhead regressions: when the compiled
//! tier emitted a `gos_rt_*` shadow-stack / safepoint call in every
//! function prologue, this kernel ran ~100x slower (a 0.05s loop became
//! multiple seconds) because the opaque call blocked LLVM from inlining
//! the hot leaf and vectorising the inner loop. The wall-clock cap is
//! deliberately generous (a healthy build finishes in well under a
//! second) so only a true order-of-magnitude regression trips it.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        // On Windows the built binary is `<stem>.exe`; match that and
        // exclude the `.gos` source / `.pdb` debug file that share the dir.
        p.is_file() && p.extension().is_some_and(|e| e.eq_ignore_ascii_case("exe"))
    }
}

#[test]
fn release_scalar_kernel_is_not_pathologically_slow() {
    // Self-contained spectral-norm-style kernel: a hot leaf function
    // called in a doubly-nested loop, summing `1 / eval_A(i, j)`. At
    // -O3 with a clean compiled tier this runs in tens of milliseconds;
    // a per-call instrumentation regression makes it seconds.
    let src = r#"
fn eval_a(i: i64, j: i64) -> f64 {
    let s = i + j
    let d = s * (s + 1) / 2 + i + 1
    1.0 / (d as f64)
}

fn main() {
    let n: i64 = 2000
    let mut total: f64 = 0.0
    let mut i: i64 = 0
    while i < n {
        let mut j: i64 = 0
        while j < n {
            total += eval_a(i, j)
            j += 1
        }
        i += 1
    }
    println("{}", total)
}
"#;

    let dir = env::temp_dir().join(format!("gos-perfcanary-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("kernel.gos");
    std::fs::write(&source, src).unwrap();

    let build = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&source)
        .output()
        .expect("spawn gos build --release");
    assert!(
        build.status.success(),
        "release build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = std::fs::read_dir(&dir)
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .find(|p| is_executable(p))
        .expect("no built binary");

    let start = Instant::now();
    let out = Command::new(&bin).output().expect("run kernel");
    let elapsed = start.elapsed();
    assert!(out.status.success(), "kernel exited non-zero");

    // A clean -O3 build runs the 4M-iteration kernel in well under a
    // second. The shadow-stack regression made comparable kernels take
    // multiple seconds. Cap at 3s: generous for slow/loaded CI, but a
    // ~100x per-call regression blows straight through it.
    let cap = std::time::Duration::from_secs(3);
    assert!(
        elapsed < cap,
        "release scalar kernel took {elapsed:?} (cap {cap:?}) - likely a per-call \
         instrumentation regression (shadow-stack / safepoint emitted in the hot \
         leaf, blocking inlining/vectorisation)"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
