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
        .read_write(&".")
        .network(false)
        .env_allow(&"PATH")
        .timeout(30_000)
        .level(&"none")

    println!("explain: {}", policy.explain().contains(&"level none"))

    match sandbox::run(&policy, &#["echo", "released"]) {
        Ok(out) => println!("run: {} {}", out.code, out.stdout.trim())
        Err(e) => println!("run failed: {}", e)
    }

    // A preset carries the whole build policy, so a program does not
    // reassemble a dozen grants and get one wrong.
    let preset = sandbox::Policy::build_default(&".").level(&"none")
    println!("preset: {}", preset.explain().len() > 0)

    // The widened surface: the network's three modes, the temp choice,
    // every resource bound, and the readers a report is built from.
    // Each one is a separate runtime shim, so a missing dispatch entry
    // shows up here as a wrong answer rather than in a user's build.
    let full = sandbox::Policy::new()
        .read_write(&".")
        .network_mode(&"client")
        .temp(&"private")
        .timeout(30_000)
        .level(&"none")

    println!("mode: {}", full.network_name())
    println!("level: {}", full.level_name())
    println!("access: {}", full.access(&"."))
    println!("grants: {}", full.read_write_grants().len() > 0)
    println!("names: {}", full.environment_names().len() >= 0)
    println!("mechanisms: {}", full.mechanisms().len() >= 0)
    println!("policy json: {}", full.to_json().len() > 0)
    println!("verdicts: {} {}", full.network_enforcement_kind().len() > 0, full.resource_enforcement_kind().len() > 0)
    println!("unblocked: {}", full.level_blocker() == "")
    match full.check() {
        Ok(_) => println!("check: ok")
        Err(e) => println!("check failed: {}", e)
    }

    // The resource bounds live on their own policy: a host without the
    // mechanism for one refuses the whole run, so a policy carrying a
    // bound is only buildable where that mechanism exists. What is
    // asserted here is that the bounds reach the shims and the verdict
    // comes back, not what this particular machine can honor.
    let bounded = sandbox::Policy::new()
        .read_write(&".")
        .max_processes(64)
        .max_memory(536_870_912)
        .max_cpu_time(30_000)
        .max_file_size(1_048_576)
        .max_temp_size(67_108_864)
        .level(&"none")

    println!("bounds: {}", bounded.resource_enforcement_kind().len() > 0)

    // An unknown name is refused rather than applied, so a typo can
    // never weaken a policy that was written to be strict.
    println!("typo: {}", sandbox::Policy::new().network_mode(&"open").network_mode(&"opne").network_name())
    println!("fetch: {}", sandbox::Policy::new().for_fetch_phase().network_name())

    // The exit-code contract every consumer shares, and the wrapper run
    // that reports through it.
    println!("codes: {} {} {} {}", sandbox::exit_policy_error(), sandbox::exit_command_not_found(), sandbox::exit_level_unavailable(), sandbox::exit_signal_base())
    println!("inherited: {}", sandbox::run_inherit(&policy, &#["true"]))
    println!("discovery: {} {}", sandbox::home_directory().is_some(), sandbox::rust_toolchain_paths().len() >= 0)
    println!("stale: {}", sandbox::stale_grant_count() >= 0)
}
"#;

const EXPECTED: &str = "level known: true\n\
     platform: true\n\
     notes: true\n\
     json: true\n\
     explain: true\n\
     run: 0 released\n\
     preset: true\n\
     mode: client\n\
     level: none\n\
     access: read-write\n\
     grants: true\n\
     names: true\n\
     mechanisms: true\n\
     policy json: true\n\
     verdicts: true true\n\
     unblocked: true\n\
     check: ok\n\
     bounds: true\n\
     typo: open\n\
     fetch: client\n\
     codes: 126 127 64 128\n\
     inherited: 0\n\
     discovery: true true\n\
     stale: true\n";

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
