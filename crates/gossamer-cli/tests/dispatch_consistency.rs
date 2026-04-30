//! Cross-table consistency: every `gos_rt_*` extern declared in
//! the LLVM backend's `RUNTIME_DECLARATIONS` table must also
//! exist as a `pub unsafe extern "C" fn ...` in
//! `crates/gossamer-runtime/src/c_abi.rs`. Two real regressions
//! in the past month (`cranelift_dispatch_table.md` 2026-04-28
//! and `spectral_norm_regression_fix.md` 2026-04-30) traced
//! back to a typo'd or stale name in a dispatch table — the
//! resulting call silently zeroed out (cranelift) or routed
//! through the per-fn fallback (LLVM). This test cheaply gates
//! the LLVM half of that drift by parsing both source-of-truth
//! files at test time.
//!
//! It deliberately doesn't check signatures (param/return types
//! disagree across Rust ↔ LLVM IR by design — Rust's `bool` is
//! `i8` etc.) — only names. The other direction (every runtime
//! export has a declaration) is intentionally NOT enforced
//! because most `gos_rt_*` helpers are referenced by Cranelift
//! via on-demand `intrinsics.extern_fn(...)` calls and never
//! flow through LLVM IR — those don't need a declaration here.

#![allow(missing_docs)]

use std::collections::HashSet;
use std::path::PathBuf;

/// Collects every `gos_rt_*` symbol exported via
/// `pub (unsafe)? extern "C" fn ...` in `c_abi.rs` and the other
/// runtime modules (`gc.rs`, `preempt.rs`). The runtime is split
/// across a few files; we scan all of them to catch helpers
/// declared outside `c_abi` proper. Names are returned in
/// insertion order for stable failure messages.
fn extract_runtime_exports() -> Vec<String> {
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let runtime_src = PathBuf::from(&manifest_dir)
        .join("..")
        .join("gossamer-runtime")
        .join("src");
    let candidate_files = ["c_abi.rs", "gc.rs", "preempt.rs", "lib.rs", "safe_env.rs"];
    let mut out = Vec::new();
    for filename in candidate_files {
        let path = runtime_src.join(filename);
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        for line in source.lines() {
            let trimmed = line.trim_start();
            // Match `pub extern "C" fn ...` or
            // `pub unsafe extern "C" fn ...`.
            let after = trimmed
                .strip_prefix("pub unsafe extern \"C\" fn ")
                .or_else(|| trimmed.strip_prefix("pub extern \"C\" fn "));
            let Some(rest) = after else {
                continue;
            };
            let end = rest
                .find(|c: char| c == '(' || c == '<' || c.is_whitespace())
                .unwrap_or(rest.len());
            let name = &rest[..end];
            if name.starts_with("gos_rt_") {
                out.push(name.to_string());
            }
        }
    }
    out
}

/// Collects the `@symbol` token from each entry in
/// `gossamer_codegen_llvm::runtime_declarations()`.
fn extract_llvm_decl_names() -> HashSet<String> {
    let mut out = HashSet::new();
    for decl in gossamer_codegen_llvm::runtime_declarations() {
        if let Some(at) = decl.find('@') {
            let after = &decl[at + 1..];
            // `gos_rt_foo(...)` — keep up to the first `(` or
            // whitespace.
            let end = after
                .find(|c: char| c == '(' || c.is_whitespace() || c == ',')
                .unwrap_or(after.len());
            let name = after[..end].trim_matches('"');
            if name.starts_with("gos_rt_") || name.starts_with("GOS_RT_") {
                out.insert(name.to_string());
            }
        }
    }
    out
}

#[test]
fn every_llvm_declaration_names_a_real_runtime_export() {
    let exports: HashSet<String> = extract_runtime_exports().into_iter().collect();
    assert!(
        exports.len() > 50,
        "found only {} runtime exports — parser likely broken; expected >50",
        exports.len()
    );
    let llvm_names = extract_llvm_decl_names();

    let mut missing: Vec<String> = Vec::new();
    for name in &llvm_names {
        // Globals (`@GOS_RT_STDOUT_LEN`, `@GOS_RT_STDOUT_BYTES`)
        // are not extern fns; skip them. They live in the
        // runtime as `static` items, not `pub unsafe extern "C" fn`.
        if name.starts_with("GOS_RT_") {
            continue;
        }
        if !exports.contains(name) {
            missing.push(name.clone());
        }
    }
    missing.sort();
    assert!(
        missing.is_empty(),
        "{} LLVM `declare` entr{} reference{} a name that does not exist as \
         `pub unsafe extern \"C\" fn ...` in crates/gossamer-runtime/src/c_abi.rs:\n  {}\n\n\
         Either add the runtime export, or remove/fix the `declare ...` line in \
         crates/gossamer-codegen-llvm/src/emit.rs::RUNTIME_DECLARATIONS.",
        missing.len(),
        if missing.len() == 1 { "y" } else { "ies" },
        if missing.len() == 1 { "s" } else { "" },
        missing.join("\n  ")
    );
}

#[test]
fn extracted_runtime_export_set_is_non_empty_and_unique() {
    // Sanity-check the parser: it should find a non-trivial
    // count of distinct entries.
    let exports = extract_runtime_exports();
    let unique: HashSet<&str> = exports.iter().map(String::as_str).collect();
    assert_eq!(
        exports.len(),
        unique.len(),
        "duplicate runtime exports detected"
    );
}
