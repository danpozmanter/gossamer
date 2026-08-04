//! Catches the historical LLVM aggregate-print path from
//! `~/dev/contexts/lang/adversarial_analysis.md`.
//!
//! The program below builds a struct, prints it through the
//! `Display`-style format implementation it carries, and exits.
//! Both `gos build` and `gos build --release` must produce a
//! binary that exits cleanly and prints the same text.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn aggregate_println_lowers_through_release_pipeline() {
    let dir = env::temp_dir().join(format!("gos-agg-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("agg.gos");
    // The release build must lower the display helper without
    // relying on a companion backend object.
    std::fs::write(
        &source,
        r#"
struct Point {
    x: i64,
    y: i64,
}

fn show(p: Point) {
    println!("Point {{ x: {}, y: {} }}", p.x, p.y)
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
        let bin = dir
            .join("target")
            .join(profile)
            .join(format!("agg{}", std::env::consts::EXE_SUFFIX));
        assert!(bin.exists(), "missing {}", bin.display());

        let out = Command::new(&bin).output().expect("run agg");
        assert!(
            out.status.success(),
            "binary exited non-zero (release={release}): stderr={}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("Point { x: 7, y: 11 }"),
            "expected 'Point {{ x: 7, y: 11 }}' in stdout (release={release}), got: {stdout:?}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
