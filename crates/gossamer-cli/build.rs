//! Build script for `gossamer-cli`.
//!
//! Three responsibilities:
//!
//! 1. Ensures the `gossamer-runtime` static library is present at
//!    the standard `target/<profile>/` location every time `cargo
//!    build` processes the cli, and exposes that absolute path to
//!    the cli at compile time via `GOSSAMER_RUNTIME_LIB_PATH`.
//! 2. On Linux hosts where the `x86_64-unknown-linux-musl` rustup
//!    target is installed, additionally builds the runtime
//!    against that target and exposes the resulting archive path
//!    via `GOSSAMER_RUNTIME_LIB_PATH_MUSL`. The cli's `gos build
//!    --release` link path uses that archive to produce a fully
//!    static binary (no glibc/libgcc_s/ld-linux dependency).
//! 3. Enforces dispatch-table parity: every `gos_rt_*` symbol
//!    declared in `crates/gossamer-runtime/src/c_abi.rs` must be
//!    referenced by at least one of the LLVM lowerer, the
//!    Cranelift native backend, the Cranelift JIT symbol map, or
//!    the in-file `KNOWN_UNUSED_RUNTIME_SYMBOLS` allowlist. Catches
//!    the silent-zero footgun documented in the H8 audit finding:
//!    a new runtime symbol added without wiring would otherwise
//!    compile clean but produce wrong code at run time.
//!
//! Why responsibility 1 exists: cargo only emits the `staticlib`
//! artefact when the runtime crate is the *direct* build target.
//! When the cli (or its dependents) pulls the runtime in
//! transitively as an `rlib`, the staticlib is never written. CI
//! runs that built the cli first then ran the tests would observe
//! `libgossamer_runtime.a` missing from `target/debug/`. This
//! script sidesteps that by invoking cargo against the runtime
//! crate explicitly.

use std::collections::BTreeSet;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Runtime symbols that intentionally have no codegen dispatch arm.
/// Add a one-line comment justifying each entry.
const KNOWN_UNUSED_RUNTIME_SYMBOLS: &[&str] = &[
    // Intentionally never called from generated code: a debug-only
    // helper used by manual `gdb`/`lldb` sessions.
    "gos_rt_result_dbg",
];

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=../gossamer-runtime/src");
    println!("cargo:rerun-if-changed=../gossamer-runtime/Cargo.toml");
    println!("cargo:rerun-if-changed=../gossamer-codegen-cranelift/src/native.rs");
    println!("cargo:rerun-if-changed=../gossamer-codegen-cranelift/src/jit.rs");
    println!("cargo:rerun-if-changed=../gossamer-codegen-llvm/src/emit.rs");
    println!("cargo:rerun-if-changed=../gossamer-codegen-llvm/src/lower.rs");
    println!("cargo:rerun-if-env-changed=GOS_RUNTIME_LIB");
    println!("cargo:rerun-if-env-changed=GOSSAMER_SKIP_DISPATCH_PARITY");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let workspace_root = manifest
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf();

    if env::var_os("GOSSAMER_SKIP_DISPATCH_PARITY").is_none() {
        check_dispatch_parity(&workspace_root);
    }

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
    // The `rerun-if-changed` directives above only fire build.rs
    // when the runtime sources change; we always re-invoke the
    // inner cargo so it picks up source edits and refreshes the
    // staticlib in-place. Cargo's own incremental layer keeps the
    // re-run cheap when nothing has changed.
    build_runtime_into(&workspace_root, &target_dir, &profile, None);

    println!(
        "cargo:rustc-env=GOSSAMER_RUNTIME_LIB_PATH={}",
        lib_path.display()
    );

    // Linux + release: also build the runtime against musl when the
    // rustup target is installed. The `gos build --release` link
    // path consumes this archive to produce a fully static binary.
    // Skip silently when the target isn't available — we still ship
    // the dynamic path as a fallback.
    if cfg!(target_os = "linux") && profile == "release" {
        let musl_triple = "x86_64-unknown-linux-musl";
        if rustup_target_installed(musl_triple) {
            let musl_lib_path =
                build_runtime_into(&workspace_root, &target_dir, &profile, Some(musl_triple));
            println!(
                "cargo:rustc-env=GOSSAMER_RUNTIME_LIB_PATH_MUSL={}",
                musl_lib_path.display()
            );
        }
    }
}

/// Returns true when the rustup `<triple>` target's std library is
/// installed locally — checked by probing for the rustlib dir, not
/// by shelling out to `rustup`.
fn rustup_target_installed(triple: &str) -> bool {
    let Ok(out) = Command::new(env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string()))
        .args(["--print", "sysroot"])
        .output()
    else {
        return false;
    };
    if !out.status.success() {
        return false;
    }
    let sysroot = String::from_utf8_lossy(&out.stdout).trim().to_string();
    let probe = PathBuf::from(&sysroot)
        .join("lib")
        .join("rustlib")
        .join(triple)
        .join("lib");
    probe.exists()
}

/// Invokes `cargo build -p gossamer-runtime` with an isolated target
/// directory, then copies the resulting staticlib into the outer
/// `target/<profile>/` so downstream lookups find it. When `triple`
/// is supplied, builds against that rustup target and the artifact
/// is copied into `target/<triple>/<profile>/`. Returns the path to
/// the resulting staticlib.
fn build_runtime_into(
    workspace_root: &Path,
    target_dir: &Path,
    profile: &str,
    triple: Option<&str>,
) -> PathBuf {
    let cargo = env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let inner_target = match triple {
        Some(t) => target_dir.join(format!("runtime-staticlib-{t}")),
        None => target_dir.join("runtime-staticlib"),
    };

    let mut cmd = Command::new(&cargo);
    cmd.arg("build")
        .arg("-p")
        .arg("gossamer-runtime")
        .arg("--target-dir")
        .arg(&inner_target)
        .current_dir(workspace_root);
    if let Some(t) = triple {
        cmd.arg("--target").arg(t);
    }
    if profile == "release" {
        cmd.arg("--release");
    }
    // Strip cargo-set vars that bias the inner build toward this
    // crate's flags. The outer-cargo `RUSTFLAGS` is removed so
    // workspace-wide flags (CI's `-D warnings`, IDE toggles, etc.)
    // can't leak into the runtime build. The Gossamer-internal
    // codegen knobs are owned by this build script (release adds
    // `function-sections`/`data-sections`); the only user-facing
    // override is `GOSSAMERFLAGS`, which the cli forwards as
    // `RUSTFLAGS` to the inner cargo invocation. We deliberately
    // do NOT honour the outer `RUSTFLAGS` even as a fallback.
    for var in [
        "CARGO_PRIMARY_PACKAGE",
        "CARGO_PKG_NAME",
        "RUSTC_WRAPPER",
        "RUSTFLAGS",
    ] {
        cmd.env_remove(var);
    }
    // Function-level sections so the user-binary linker's
    // `--gc-sections` (static-musl path) can prune unused
    // `gos_rt_*` helpers. Without these the runtime archive is a
    // single big `.text` blob — pulling any symbol pulls every
    // symbol. Promoting each Rust function/static to its own ELF
    // section lets gc-sections drop the unreferenced ones at user-
    // binary link time. Only matters in release; debug builds skip
    // these flags so the inner cargo cache stays warm for `gos
    // run`. Users can extend the flag set via `GOSSAMERFLAGS`.
    let mut flags: Vec<String> = Vec::new();
    if profile == "release" {
        flags.push("-Cfunction-sections=yes".to_string());
        flags.push("-Cdata-sections=yes".to_string());
    }
    if let Ok(extra) = env::var("GOSSAMERFLAGS") {
        if !extra.trim().is_empty() {
            flags.push(extra);
        }
    }
    if !flags.is_empty() {
        cmd.env("RUSTFLAGS", flags.join(" "));
    }

    let status = cmd.status().expect("invoke cargo for runtime build");
    assert!(
        status.success(),
        "failed to build gossamer-runtime staticlib (triple={triple:?})"
    );

    let lib_name = if cfg!(target_env = "msvc") {
        "gossamer_runtime.lib"
    } else {
        "libgossamer_runtime.a"
    };
    let inner_profile_dir = match triple {
        Some(t) => inner_target.join(t).join(profile),
        None => inner_target.join(profile),
    };
    let inner_artifact = inner_profile_dir.join(lib_name);
    let outer_profile_dir = match triple {
        Some(t) => target_dir.join(t).join(profile),
        None => target_dir.join(profile),
    };
    let outer_artifact = outer_profile_dir.join(lib_name);
    if let Some(parent) = outer_artifact.parent() {
        std::fs::create_dir_all(parent).expect("create outer profile dir");
    }
    std::fs::copy(&inner_artifact, &outer_artifact).expect("copy staticlib into outer target dir");
    outer_artifact
}

/// Fails the build if any `gos_rt_*` symbol declared in `c_abi.rs`
/// is not referenced by any codegen and is not on the allowlist.
fn check_dispatch_parity(workspace_root: &Path) {
    let c_abi = read_text(workspace_root.join("crates/gossamer-runtime/src/c_abi.rs"));
    let cl_native =
        read_text(workspace_root.join("crates/gossamer-codegen-cranelift/src/native.rs"));
    let cl_jit = read_text(workspace_root.join("crates/gossamer-codegen-cranelift/src/jit.rs"));
    let llvm_emit = read_text(workspace_root.join("crates/gossamer-codegen-llvm/src/emit.rs"));
    let llvm_lower = read_text(workspace_root.join("crates/gossamer-codegen-llvm/src/lower.rs"));

    let defined = extract_runtime_definitions(&c_abi);
    let mut referenced: BTreeSet<String> = BTreeSet::new();
    referenced.extend(extract_referenced_symbols(&cl_native));
    referenced.extend(extract_referenced_symbols(&cl_jit));
    referenced.extend(extract_referenced_symbols(&llvm_emit));
    referenced.extend(extract_referenced_symbols(&llvm_lower));

    let allowed: BTreeSet<String> = KNOWN_UNUSED_RUNTIME_SYMBOLS
        .iter()
        .map(|s| (*s).to_string())
        .collect();

    let mut orphans: Vec<String> = defined
        .iter()
        .filter(|sym| !referenced.contains(sym.as_str()) && !allowed.contains(sym.as_str()))
        .cloned()
        .collect();
    orphans.sort();

    let mut stale_allowlist: Vec<String> = allowed
        .iter()
        .filter(|sym| !defined.contains(sym.as_str()))
        .cloned()
        .collect();
    stale_allowlist.sort();

    if !orphans.is_empty() {
        let lines = orphans.join("\n  ");
        panic!(
            "dispatch-table parity check failed.\n\
             {n} runtime symbol(s) declared in crates/gossamer-runtime/src/c_abi.rs \
             have no corresponding reference in any codegen file:\n  {lines}\n\
             Wire each one through the appropriate codegen, or add it to \
             KNOWN_UNUSED_RUNTIME_SYMBOLS in crates/gossamer-cli/build.rs with a \
             one-line comment justifying the omission. Set \
             GOSSAMER_SKIP_DISPATCH_PARITY=1 to bypass during local debugging.",
            n = orphans.len(),
        );
    }
    if !stale_allowlist.is_empty() {
        let lines = stale_allowlist.join("\n  ");
        panic!(
            "dispatch-table parity check failed.\n\
             {n} symbol(s) listed in KNOWN_UNUSED_RUNTIME_SYMBOLS no longer exist \
             in crates/gossamer-runtime/src/c_abi.rs:\n  {lines}\n\
             Remove the stale entries from build.rs.",
            n = stale_allowlist.len(),
        );
    }
}

fn read_text(path: PathBuf) -> String {
    std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("dispatch-parity: read {}: {err}", path.display()))
}

/// Returns every `gos_rt_*` symbol whose Rust definition appears in
/// `c_abi.rs`. The author convention is `pub unsafe extern "C" fn
/// gos_rt_<name>` or `pub extern "C" fn gos_rt_<name>`; both are
/// matched by anchoring on the `extern "C"` clause.
fn extract_runtime_definitions(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in src.lines() {
        let Some(rest) = line.split_once("extern \"C\" fn ").map(|(_, r)| r) else {
            continue;
        };
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if name.starts_with("gos_rt_") {
            out.insert(name);
        }
    }
    out
}

/// Returns every `gos_rt_*` identifier mentioned anywhere in `src`.
/// We accept any occurrence — string literal in a match arm, LLVM
/// IR `declare`, JIT mapping, or even a comment — because the
/// parity check is an "is this symbol live somewhere" probe, not a
/// per-codegen wiring audit. The unique false-negative this allows
/// (a stale comment "documenting" a symbol that has no real call
/// site) is not worth the parser complexity to filter out.
fn extract_referenced_symbols(src: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    let bytes = src.as_bytes();
    let needle = b"gos_rt_";
    let mut i = 0;
    while i + needle.len() <= bytes.len() {
        if &bytes[i..i + needle.len()] == needle {
            // Reject if preceded by an identifier char — we only
            // want symbol-name occurrences, not "_gos_rt_…" or
            // mid-identifier substring matches.
            let prev_is_ident = i > 0 && is_ident_byte(bytes[i - 1]);
            if !prev_is_ident {
                let mut j = i + needle.len();
                while j < bytes.len() && is_ident_byte(bytes[j]) {
                    j += 1;
                }
                if j > i + needle.len()
                    && let Ok(s) = std::str::from_utf8(&bytes[i..j])
                {
                    out.insert(s.to_string());
                }
                i = j;
                continue;
            }
        }
        i += 1;
    }
    out
}

fn is_ident_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}
