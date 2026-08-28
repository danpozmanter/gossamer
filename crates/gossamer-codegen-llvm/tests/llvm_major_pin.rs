//! The LLVM major this backend shells out to and the major `rustc`
//! bundles have to be the same number.
//!
//! The prebuilt runtime archive is compiled by `rustc`, so its bitcode
//! carries that LLVM's format. An older `clang` cannot read it at all
//! (`Unknown attribute kind`), which is what makes cross-language LTO a
//! version-matching problem rather than a flag. Nothing enforces the
//! pairing at build time, so a `rust-toolchain.toml` bump would
//! otherwise reopen the split silently.

#![allow(missing_docs)]

use std::process::Command;

use gossamer_codegen_llvm::PREFERRED_LLVM_MAJOR;

/// The LLVM major `rustc` reports for itself.
fn rustc_llvm_major() -> Option<u32> {
    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let out = Command::new(rustc)
        .args(["--version", "--verbose"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let rest = text.split("LLVM version: ").nth(1)?;
    let digits: String = rest.chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[test]
fn preferred_llvm_major_matches_rustc() {
    let Some(rustc_major) = rustc_llvm_major() else {
        // A `rustc` that does not report its LLVM version (a distro
        // build with the field stripped) leaves nothing to compare.
        eprintln!("skipping: rustc did not report an LLVM version");
        return;
    };
    assert_eq!(
        PREFERRED_LLVM_MAJOR, rustc_major,
        "rustc bundles LLVM {rustc_major} but this backend prefers LLVM \
         {PREFERRED_LLVM_MAJOR}. Bumping `rust-toolchain.toml` also bumps \
         `PREFERRED_LLVM_MAJOR` and the candidate lists in `emit.rs`, or the \
         runtime archive's bitcode is unreadable by the tools we shell out to."
    );
}
