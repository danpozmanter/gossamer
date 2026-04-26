//! Build script for `gossamer-cli`.
//!
//! Ensures the `gossamer-runtime` static library is present at the
//! standard `target/<profile>/` location every time `cargo build`
//! processes the cli, and exposes that absolute path to the cli at
//! compile time via `GOSSAMER_RUNTIME_LIB_PATH`.
//!
//! Why this exists: cargo only emits the `staticlib` artefact when
//! the runtime crate is the *direct* build target. When the cli (or
//! its dependents) pulls the runtime in transitively as an `rlib`,
//! the staticlib is never written. CI runs that built the cli first
//! then ran the tests would observe `libgossamer_runtime.a` missing
//! from `target/debug/`. This script sidesteps that by invoking
//! cargo against the runtime crate explicitly.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../gossamer-runtime/src");
    println!("cargo:rerun-if-changed=../gossamer-runtime/Cargo.toml");
    println!("cargo:rerun-if-env-changed=GOS_RUNTIME_LIB");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();

    // Honour CARGO_TARGET_DIR if set, otherwise default to
    // <workspace>/target — the same logic cargo uses internally.
    let target_dir = env::var_os("CARGO_TARGET_DIR")
        .map_or_else(|| workspace_root.join("target"), PathBuf::from);

    let profile = env::var("PROFILE").unwrap_or_else(|_| "debug".to_string());
    let lib_dir = target_dir.join(&profile);
    let lib_name = if cfg!(target_env = "msvc") {
        "gossamer_runtime.lib"
    } else {
        "libgossamer_runtime.a"
    };
    let lib_path = lib_dir.join(lib_name);

    // Force-build the runtime crate so the staticlib gets emitted.
    // Use a separate target dir to avoid a deadlock against the
    // outer cargo invocation that owns `target/`'s build lock.
    if !lib_path.exists() {
        build_runtime_into(&workspace_root, &target_dir, &profile);
    }

    println!(
        "cargo:rustc-env=GOSSAMER_RUNTIME_LIB_PATH={}",
        lib_path.display()
    );
}

/// Invokes `cargo build -p gossamer-runtime` with an isolated target
/// directory, then copies the resulting staticlib into the outer
/// `target/<profile>/` so downstream lookups find it.
fn build_runtime_into(workspace_root: &Path, target_dir: &Path, profile: &str) {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let inner_target = target_dir.join("runtime-staticlib");

    let mut cmd = Command::new(&cargo);
    cmd.arg("build")
        .arg("-p")
        .arg("gossamer-runtime")
        .arg("--target-dir")
        .arg(&inner_target)
        .current_dir(workspace_root);
    if profile == "release" {
        cmd.arg("--release");
    }
    // Strip cargo-set vars that bias the inner build toward this
    // crate's flags. `RUSTFLAGS` is preserved since CI sets
    // `-D warnings` workspace-wide and the runtime must build clean.
    for var in ["CARGO_PRIMARY_PACKAGE", "CARGO_PKG_NAME", "RUSTC_WRAPPER"] {
        cmd.env_remove(var);
    }

    let status = cmd.status().expect("invoke cargo for runtime build");
    assert!(
        status.success(),
        "failed to build gossamer-runtime staticlib"
    );

    let lib_name = if cfg!(target_env = "msvc") {
        "gossamer_runtime.lib"
    } else {
        "libgossamer_runtime.a"
    };
    let inner_artifact = inner_target.join(profile).join(lib_name);
    let outer_artifact = target_dir.join(profile).join(lib_name);
    if let Some(parent) = outer_artifact.parent() {
        std::fs::create_dir_all(parent).expect("create outer profile dir");
    }
    std::fs::copy(&inner_artifact, &outer_artifact).expect("copy staticlib into outer target dir");
}
