//! Catches the M3 finding (LLVM "aggregate-print refused" error
//! path is not exercised) from
//! `~/dev/contexts/lang/adversarial_analysis.md`.
//!
//! The LLVM lowerer rejects `println!("{}", some_aggregate)` with
//! `BuildError::Unsupported`. Per `compile_with_fallback`
//! (`crates/gossamer-codegen-llvm/src/emit.rs`) the rejected body
//! falls back to Cranelift while the rest of the program continues
//! through LLVM. Without a regression test this seam silently
//! breaks the moment either side stops handling the shape.
//!
//! The program below builds a struct, prints it through the
//! `Display`-style format implementation it carries, and exits.
//! Both `gos build` (pure Cranelift) and `gos build --release`
//! (LLVM + Cranelift fallback) must produce a binary that exits
//! cleanly and prints something.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn aggregate_println_falls_back_through_release_pipeline() {
    let dir = env::temp_dir().join(format!("gos-agg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("agg.gos");
    // `Point` is a tuple-struct held by value. `format!("{}",
    // point.x)` lowers cleanly through the LLVM print intrinsics
    // (scalar `i64`); the line that prints the whole point goes
    // through the Display-style helper, which is the case
    // `concat_print_kind() == ConcatKind::Unsupported` rejects.
    // The release build must still link end-to-end thanks to the
    // Cranelift fallback object.
    std::fs::write(
        &source,
        r#"
struct Point {
    x: i64,
    y: i64,
}

fn show(p: Point) {
    println!("Point({}, {})", p.x, p.y)
}

fn main() {
    let p = Point { x: 7, y: 11 }
    show(p)
}
"#,
    )
    .unwrap();

    for release in [false, true] {
        let mut cmd = Command::new(gos_bin());
        cmd.arg("build");
        if release {
            cmd.arg("--release");
        }
        cmd.arg(&source);
        let build = cmd.output().expect("spawn gos build");
        assert!(
            build.status.success(),
            "build failed (release={release}): {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let profile = if release { "release" } else { "debug" };
        let bin = dir.join("target").join(profile).join("agg");
        assert!(bin.exists(), "missing {}", bin.display());

        let out = Command::new(&bin).output().expect("run agg");
        assert!(
            out.status.success(),
            "binary exited non-zero (release={release}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Point(7, 11)"),
            "expected 'Point(7, 11)' in stdout (release={release}), got: {stdout:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
