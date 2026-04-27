//! Safe-Rust wrappers around the Rust 2024 `unsafe std::env::set_var`
//! / `remove_var` calls. The unsafe is contained here so the rest of
//! the workspace (which carries `#![forbid(unsafe_code)]`) can call
//! into it through ordinary safe Rust.
//!
//! ## Soundness contract
//!
//! `std::env::set_var` is `unsafe fn` because POSIX `setenv` is not
//! thread-safe: a concurrent reader can observe a torn pointer or
//! use-after-free if another thread mutates the env table while the
//! read is in flight. The standard mitigation — and Gossamer's
//! contract — is to set environment variables **before any
//! goroutine spawn or thread creation**.
//!
//! Beyond that mitigation, every external library that reads the
//! environment (libc, child processes inheriting env, the host
//! Gossamer toolchain itself) is also subject to the same race; no
//! amount of Rust-side wrapping changes that. We surface a `safe_env`
//! API anyway because:
//!
//! - It moves the unsafe out of every caller into a single audited
//!   site (this file).
//! - It lets `gossamer-std::os::set_env` work in normal user
//!   workflows (CI scripts, test fixtures, one-shot CLIs) without
//!   forcing them into "stub returns error" land.
//! - It documents the constraint at the API boundary instead of
//!   leaving it as folklore.
//!
//! See also: <https://github.com/rust-lang/rust/issues/27970> for
//! the long-running discussion of why `std::env::set_var` had to
//! become `unsafe`.

#![allow(unsafe_code)]

/// Sets the environment variable `name` to `value`. **Call before
/// spawning any goroutine / thread**; concurrent readers from
/// other threads or external libraries can otherwise observe a
/// torn value.
pub fn set_env(name: &str, value: &str) {
    // SAFETY: contained-unsafe pattern. The contract above forbids
    // concurrent env reads; the rest of the workspace can call
    // this from safe Rust because the unsafe-ness is structural,
    // not memory-corruption-shaped from the caller's perspective.
    unsafe { std::env::set_var(name, value) }
}

/// Unsets `name`. Same threading contract as [`set_env`].
pub fn unset_env(name: &str) {
    // SAFETY: same as `set_env` — POSIX `unsetenv` shares the
    // same thread-safety contract as `setenv`.
    unsafe { std::env::remove_var(name) }
}
