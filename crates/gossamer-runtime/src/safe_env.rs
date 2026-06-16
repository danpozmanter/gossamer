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
//! read is in flight. The standard mitigation - and Gossamer's
//! contract - is to set environment variables **before any
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

use parking_lot::Mutex;

/// Process-global serialisation mutex.
///
/// every `safe_env` call acquires this lock so
/// concurrent Gossamer goroutines / threads can't race their
/// `setenv` calls against each other. This does NOT help against
/// third-party C libraries reading the env table without
/// coordinating through this lock - POSIX `setenv` is
/// fundamentally racy against any reader that doesn't share the
/// lock, and Rust has no way to retrofit thread-safety onto libc.
///
/// What this lock buys: callers that exclusively use
/// `safe_env::set_env` / `unset_env` are mutually consistent.
/// Combined with the "call before goroutine spawn" idiom in the
/// module docs, the practical race surface shrinks to "Gossamer
/// code calls `os::set_env` while a linked Rust dependency
/// concurrently reads `getenv`", which is a documented limit, not
/// a silent corruption.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

/// Sets the environment variable `name` to `value`.
///
/// 0.6.0 changed this to acquire a process-global mutex so
/// concurrent Gossamer-side env mutations are serialised. The
/// "call before spawning any goroutine / thread" idiom from the
/// module docs is still recommended for portability against
/// third-party C libraries that read the env without going
/// through this API.
pub fn set_env(name: &str, value: &str) {
    let _guard = ENV_MUTEX.lock();
    // SAFETY: ENV_MUTEX serialises every Gossamer-side mutation
    // of the env table. POSIX `setenv` remains racy against
    // external readers; the lock contains the in-process race.
    unsafe { std::env::set_var(name, value) }
}

/// Unsets `name`. Same threading contract as [`set_env`].
pub fn unset_env(name: &str) {
    let _guard = ENV_MUTEX.lock();
    // SAFETY: same lock as `set_env`. POSIX `unsetenv` shares the
    // thread-safety contract.
    unsafe { std::env::remove_var(name) }
}

/// Lock the env mutex for the duration of a closure. Lets
/// callers that need a read-modify-write sequence (compute
/// envvar value from another envvar, then write it back) keep
/// the mutex held across the read so no other Gossamer goroutine
/// can race them.
pub fn with_env_lock<R>(f: impl FnOnce() -> R) -> R {
    let _guard = ENV_MUTEX.lock();
    f()
}
