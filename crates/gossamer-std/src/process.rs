//! Child processes and process control. Mirrors Rust's
//! `std::process` shape. Types and helpers are re-exported from
//! [`crate::exec`] (Go's `os/exec` shape) under the Rust-style
//! `process` namespace so call sites read uniformly with
//! `process::Command::new(...)` and `process::exit(...)`.

#![forbid(unsafe_code)]

pub use crate::exec::{Child, Command, ExitStatus, Output, Stdio};

/// Returns the current process ID.
#[must_use]
pub fn id() -> u32 {
    std::process::id()
}

/// Exits the current process with the given status code. Drops no
/// destructors - use it only for terminal error paths.
pub fn exit(code: i32) -> ! {
    std::process::exit(code)
}

/// Aborts the current process immediately. No unwinding, no
/// destructors. Equivalent to calling `abort(3)`.
pub fn abort() -> ! {
    std::process::abort()
}
