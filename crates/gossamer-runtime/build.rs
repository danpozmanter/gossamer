//! Emits a `tsan` cfg when the crate is being compiled under
//! `ThreadSanitizer` (`-Zsanitizer=thread`), so `lib.rs` can fall back
//! to the default system allocator there. `mimalloc` (the normal
//! global allocator) is incompatible with `TSan`: its lazy global lock
//! init races on first allocation, and an uninstrumented allocator
//! blinds `TSan` to real heap races. `cfg(sanitize)` is nightly-only,
//! so we detect the flag from the rustflags the sanitizer job sets.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(tsan)");
    // `cargo-fuzz` sets `--cfg fuzzing`; declare it so the allocator
    // gates in `lib.rs` / `rc.rs` (which fall back to the system
    // allocator under the fuzz harness) don't trip `unexpected_cfgs`.
    println!("cargo:rustc-check-cfg=cfg(fuzzing)");
    println!("cargo:rerun-if-env-changed=RUSTFLAGS");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");

    let flags = std::env::var("CARGO_ENCODED_RUSTFLAGS")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(|| std::env::var("RUSTFLAGS").ok())
        .unwrap_or_default();

    if flags.contains("sanitizer=thread") {
        println!("cargo:rustc-cfg=tsan");
    }
}
