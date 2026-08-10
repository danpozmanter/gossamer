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

/// The bare print builtins take their values directly rather than
/// through `__concat`, so their aggregate arguments need the same
/// derived-`fmt` routing the macro form gets. Without it the VM prints
/// the value and the native build refuses to lower it.
#[test]
fn bare_println_of_an_aggregate_matches_the_vm_on_the_native_tier() {
    let dir = env::temp_dir().join(format!("gos-agg-bare-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("bare.gos");
    std::fs::write(
        &source,
        r"
struct P { x: i64, y: i64 }
enum E { A, B(i64) }

fn main() {
    let p = P { x: 1, y: 2 }
    println(p)
    println(E::B(5))
}
",
    )
    .unwrap();

    let vm = Command::new(gos_bin())
        .arg("run")
        .arg(&source)
        .output()
        .expect("spawn gos run");
    assert!(
        vm.status.success(),
        "vm run failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    let vm_out = String::from_utf8_lossy(&vm.stdout).to_string();
    assert!(
        vm_out.contains("P { x: 1, y: 2 }") && vm_out.contains("B(5)"),
        "unexpected vm output: {vm_out:?}"
    );

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source)
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "native build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let bin = dir
        .join("target")
        .join("debug")
        .join(format!("bare{}", std::env::consts::EXE_SUFFIX));
    let native = Command::new(&bin).output().expect("run bare");
    assert!(
        native.status.success(),
        "native binary exited non-zero: {}",
        String::from_utf8_lossy(&native.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&native.stdout),
        vm_out,
        "native output must match the vm"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
