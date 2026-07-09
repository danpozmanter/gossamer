//! `gos` - command-line entry point for the Gossamer toolchain.
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
    // Apply the same process-wide allocator knobs to the `gos run` VM tier
    // and the toolchain that compiled programs get from `runtime_init`.
    gossamer_runtime::init_process_allocator();
    // Arm the byte-budget recursion guard at the shallowest point of
    // the main thread, so deeply recursive walker code (closures,
    // methods, `gos run`'s top-level `main`, the REPL) raises a clean
    // stack-overflow error instead of overflowing the OS stack and
    // aborting. Spawned goroutines arm their own 1 MiB coroutine
    // stacks; this covers the main thread, budgeted for the smallest
    // main stack we target (Windows defaults to ~1 MiB).
    gossamer_coro::arm_stack_guard(
        gossamer_coro::DEFAULT_STACK_BYTES - gossamer_coro::STACK_GUARD_MARGIN,
    );
    let args: Vec<std::ffi::OsString> = std::env::args_os().collect();
    match gossamer_cli::dispatch_runner_if_needed(&args) {
        gossamer_cli::DispatchOutcome::InProcess => gossamer_cli::run_main(),
        gossamer_cli::DispatchOutcome::Failed(err) => {
            eprintln!("error: {err}");
            ExitCode::FAILURE
        }
    }
}
