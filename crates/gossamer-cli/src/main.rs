//! `gos` — command-line entry point for the Gossamer toolchain.
//!
//! The actual subcommand surface lives in [`gossamer_cli::cli`]
//! (clap derive + dispatch table). This shim performs one
//! pre-step before delegating to the library: when a project
//! declares `[rust-bindings]`, build the per-project runner and
//! re-exec into it. The runner sets `GOSSAMER_IN_RUNNER=1` so the
//! re-entry returns immediately to in-process dispatch.

#![forbid(unsafe_code)]

use std::process::ExitCode;

fn main() -> ExitCode {
    // Return freed pages to the OS promptly (mimalloc purge delay -> 0)
    // so the `gos run` VM tier and the toolchain share the predictable
    // footprint compiled programs get from `runtime_init`.
    gossamer_runtime::init_process_allocator();
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match gossamer_cli::dispatch_runner_if_needed(&args) {
        gossamer_cli::DispatchOutcome::InProcess => gossamer_cli::run_main(),
        gossamer_cli::DispatchOutcome::Failed(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
