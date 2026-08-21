//! `std::sandbox` through `gos build --release`.
//!
//! Tier parity compares the VM, the JIT, and the debug AOT build. The
//! release path is a different pipeline - full LLVM `-O3`, static-musl -
//! and the sandbox surface reaches it through runtime shims a missing
//! dispatch entry would silently zero. This gate compiles a program that
//! uses the policy builder, the capability report, and a real run, then
//! checks the transcript: a shim that stops being wired fails here
//! rather than in a user's build.

#![allow(missing_docs)]

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

const PROGRAM: &str = r#"
use std::sandbox

fn main() {
    // The capability report is a value, so a program can branch on what
    // the host honors rather than assuming one operating system.
    let level = sandbox::max_level()
    let known = level == "none" || level == "basic" || level == "standard" || level == "strict"
    println!("level known: {}", known)
    println!("platform: {}", sandbox::platform().len() > 0)
    println!("notes: {}", sandbox::notes().len() >= 0)
    println!("json: {}", sandbox::capabilities_json().contains(&"max_level"))

    let policy = sandbox::Policy::new()
        |> $.read_write(&".")
        |> $.network(false)
        |> $.env_allow(&"PATH")
        |> $.timeout(30_000)
        |> $.level(&"none")

    println!("explain: {}", policy.explain().contains(&"level none"))

    match sandbox::run(&policy, &#["echo", "released"]) {
        Ok(out) => println!("run: {} {}", out.code, out.stdout.trim())
        Err(e) => println!("run failed: {}", e)
    }

    // A preset carries the whole build policy, so a program does not
    // reassemble a dozen grants and get one wrong.
    let preset = sandbox::Policy::build_default(&".") |> $.level(&"none")
    println!("preset: {}", preset.explain().len() > 0)
}
"#;

const EXPECTED: &str = "level known: true\n\
     platform: true\n\
     notes: true\n\
     json: true\n\
     explain: true\n\
     run: 0 released\n\
     preset: true\n";

#[test]
fn the_sandbox_surface_survives_a_release_build() {
    let dir = env::temp_dir().join(format!("gos-sandbox-release-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let src = dir.join("sandbox_release.gos");
    std::fs::write(&src, PROGRAM).expect("write source");

    let built = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg("--out-dir")
        .arg(&dir)
        .arg(&src)
        .output()
        .expect("spawn gos build --release");
    assert!(
        built.status.success(),
        "gos build --release failed:\n{}\n{}",
        String::from_utf8_lossy(&built.stdout),
        String::from_utf8_lossy(&built.stderr)
    );

    let binary = dir.join(format!("sandbox_release{}", env::consts::EXE_SUFFIX));
    let ran = Command::new(&binary)
        .output()
        .expect("run the built binary");
    assert!(
        ran.status.success(),
        "the release binary failed:\n{}\n{}",
        String::from_utf8_lossy(&ran.stdout),
        String::from_utf8_lossy(&ran.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&ran.stdout),
        EXPECTED,
        "stderr:\n{}",
        String::from_utf8_lossy(&ran.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
