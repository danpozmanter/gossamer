//! Host primitives whose `std` implementation aborts on
//! wasm32-unknown-unknown.
//!
//! Rust's unsupported-platform shims for the browser target do not
//! report an error for the clock, a thread sleep, the process id, or
//! the temporary directory - they panic, and a panic on that target is
//! a wasm trap that ends the module with `unreachable`. The playground
//! links this crate, so a runtime path that needs one of those reaches
//! it through here: native keeps the `std` implementation verbatim, and
//! the browser gets the facility the platform actually offers.

/// Whether a thread that waits here can be woken by other work.
///
/// The browser runs the VM on one thread and settles every goroutine at
/// its spawn, so a rendezvous entered there has nobody left to end it -
/// and the parking primitive underneath aborts rather than blocking. A
/// wait guarded by this reports instead of entering.
pub const CAN_BLOCK: bool = !cfg!(all(target_arch = "wasm32", target_os = "unknown"));

/// Whether this build can end the process.
///
/// A wasm module has no process of its own to end: `std::process::exit`
/// aborts the module with a trap instead. A program that asks to exit is
/// answered by ending its run and reporting the status.
pub const CAN_END_PROCESS: bool = !cfg!(all(target_arch = "wasm32", target_os = "unknown"));

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub use std::time::Instant;

// `web-time` is the ecosystem's drop-in `std::time` for the browser:
// `Instant` reads `performance.now()` and its `SystemTime` reads
// `Date.now()`, both of which every wasm host provides.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub use web_time::Instant;

/// Current wall-clock time.
///
/// The answer is a `std::time::SystemTime` on every target: a wall
/// clock has one origin, and a timestamp read here compares against
/// one a file's metadata carries.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[must_use]
pub fn system_time_now() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

/// Current wall-clock time, read from the host's `Date.now()` and
/// rebased onto the Unix epoch. `std::time::SystemTime` arithmetic
/// is arithmetic on that origin, so only the reading needs the host.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[must_use]
pub fn system_time_now() -> std::time::SystemTime {
    let since_epoch = web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default();
    std::time::UNIX_EPOCH + since_epoch
}

/// Blocks the calling thread until `duration` has elapsed.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
pub fn sleep(duration: std::time::Duration) {
    std::thread::sleep(duration);
}

/// Returns at once. The browser runs the VM on one thread with no way
/// to block it, and the wasm scheduler settles every goroutine at its
/// spawn, so nothing is waiting on the delay to end.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
pub fn sleep(_duration: std::time::Duration) {}

/// Identifier of the running process.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[must_use]
pub fn process_id() -> u32 {
    std::process::id()
}

/// Identifier of the running process. A wasm module is the single
/// process of its host page, which has no id of its own to report.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[must_use]
pub fn process_id() -> u32 {
    1
}

/// Directory for temporary files.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
#[must_use]
pub fn temp_dir() -> std::path::PathBuf {
    std::env::temp_dir()
}

/// Directory for temporary files. The browser has no filesystem, so
/// this names the conventional location and leaves the refusal to the
/// filesystem call that opens a path under it.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
#[must_use]
pub fn temp_dir() -> std::path::PathBuf {
    std::path::PathBuf::from("/tmp")
}
