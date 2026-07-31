//! Cross-compilation correctness: a binary cross-built for a Linux
//! target must produce output bit-identical to the bytecode VM (`gos
//! run`) for the same source. This is the all-tier gate for
//! cross-compilation - the LLVM AOT tier targeting another machine.
//!
//! Each target is run the cheapest correct way: a binary whose arch
//! matches the host runs natively; a foreign-arch binary runs under the
//! matching QEMU user emulator. The test self-skips a target when its
//! runtime archive or runner is unavailable, so it is a no-op on hosts
//! without the cross setup and a real gate on CI hosts that install it.

use std::path::{Path, PathBuf};
use std::process::Command;

/// Source files exercised in both the VM and the cross-built binary.
/// Deterministic, no goroutines or wall-clock dependence.
const FIXTURES: &[&str] = &[
    "examples/hello_world.gos",
    "examples/fizz_buzz.gos",
    "examples/function_piping.gos",
    "examples/factorial.gos",
    "examples/gcd.gos",
    "examples/digit_sum.gos",
    "feature-testing-examples/i128_enum_payload_arith.gos",
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn have(tool: &str) -> bool {
    which::which(tool).is_ok()
}

fn gos_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_gos"))
}

fn target_arch(triple: &str) -> &str {
    triple.split('-').next().unwrap_or("")
}

/// A prebuilt runtime archive for `triple`, if one exists in the dev
/// tree. CI sets `GOS_RUNTIME_LIB_<TRIPLE>` instead, which the CLI reads
/// directly, so a missing dev-tree archive is not fatal here.
fn dev_runtime_archive(root: &Path, triple: &str) -> Option<PathBuf> {
    let p = root
        .join("target")
        .join(triple)
        .join("release")
        .join("libgossamer_runtime.a");
    p.exists().then_some(p)
}

/// How to execute a binary built for `triple` on this host.
#[derive(Clone, Copy)]
enum Runner {
    /// The target arch matches the host; run the binary directly.
    Native,
    /// Run a foreign-arch binary under the named QEMU user emulator.
    Emulator(&'static str),
}

/// How to execute a binary built for `triple` on this host: natively when
/// the arch matches, else via the matching QEMU emulator. `None` when no
/// runner is available (skip).
fn runner_for(triple: &str) -> Option<Runner> {
    let host = std::env::consts::ARCH;
    match target_arch(triple) {
        a if a == host => Some(Runner::Native),
        "aarch64" => have("qemu-aarch64").then_some(Runner::Emulator("qemu-aarch64")),
        "x86_64" => have("qemu-x86_64").then_some(Runner::Emulator("qemu-x86_64")),
        _ => None,
    }
}

fn cross_one(root: &Path, triple: &str) {
    let Some(runner) = runner_for(triple) else {
        eprintln!("skip cross_{triple}: no native/QEMU runner on this host");
        return;
    };
    let env_key = format!(
        "GOS_RUNTIME_LIB_{}",
        triple.replace(['-', '.'], "_").to_uppercase()
    );
    let archive_env = std::env::var(&env_key).ok();
    if archive_env.is_none() && dev_runtime_archive(root, triple).is_none() {
        eprintln!(
            "skip cross_{triple}: no runtime archive \
             (build `cargo build --release --target {triple} -p gossamer-runtime` or set {env_key})"
        );
        return;
    }

    let out_dir = std::env::temp_dir().join(format!("gos-cross-{triple}"));
    let _ = std::fs::create_dir_all(&out_dir);

    for src in FIXTURES {
        let src_path = root.join(src);
        let vm = Command::new(gos_bin())
            .arg(&src_path)
            .output()
            .expect("spawn gos");
        assert!(vm.status.success(), "gos failed for {src}");

        let mut build = Command::new(gos_bin());
        build
            .args(["build", "--release", "--target", triple])
            .arg(&src_path)
            .arg("--out-dir")
            .arg(&out_dir);
        if let Some(ref archive) = archive_env {
            build.env(&env_key, archive);
        } else if let Some(archive) = dev_runtime_archive(root, triple) {
            build.env(&env_key, archive);
        }
        let build = build.output().expect("spawn gos build");
        if !build.status.success() {
            let err = String::from_utf8_lossy(&build.stderr);
            // The `gos` under test registers musl targets only with the
            // `musl` cargo feature; without it the triple is unknown, so
            // skip rather than fail (the feature-on CI build gates it).
            if err.contains("unknown target") {
                eprintln!("skip cross_{triple}: target not registered in this `gos` build");
                return;
            }
            panic!("cross build failed for {src} ({triple}):\n{err}");
        }

        let stem = Path::new(src).file_stem().unwrap().to_string_lossy();
        let bin = out_dir.join(&*stem);
        assert!(
            bin.exists(),
            "cross binary missing for {src}: {}",
            bin.display()
        );

        let run = match runner {
            Runner::Native => Command::new(&bin).output(),
            Runner::Emulator(emu) => Command::new(emu).arg(&bin).output(),
        }
        .expect("run cross binary");
        assert_eq!(
            run.stdout, vm.stdout,
            "AOT({triple}) stdout != VM for {src}"
        );
    }
}

#[test]
fn cross_aarch64_gnu_matches_vm() {
    cross_one(&repo_root(), "aarch64-unknown-linux-gnu");
}

#[test]
fn cross_aarch64_musl_matches_vm() {
    cross_one(&repo_root(), "aarch64-unknown-linux-musl");
}

#[test]
fn cross_x86_64_gnu_matches_vm() {
    cross_one(&repo_root(), "x86_64-unknown-linux-gnu");
}

#[test]
fn cross_x86_64_musl_matches_vm() {
    cross_one(&repo_root(), "x86_64-unknown-linux-musl");
}
