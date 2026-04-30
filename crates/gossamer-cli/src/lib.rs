//! Library entry point for the `gos` toolchain.
//!
//! `main.rs` is a tiny shim that calls [`run_main`]. The library
//! form exists so the per-project Rust-binding runner generated
//! from `gossamer-runner-template` can pull `gossamer-cli` in as
//! a dependency, statically link every binding, and dispatch the
//! same subcommand surface as the on-PATH `gos` binary.

#![forbid(unsafe_code)]
#![allow(clippy::similar_names, clippy::ptr_arg)]

pub mod binding_dispatch;
pub mod cli;
pub mod cmd;
pub mod doc;
pub mod loaders;
pub mod paths;
pub mod repl;
pub mod repl_helper;
pub mod style;

pub use binding_dispatch::{DispatchOutcome, dispatch_runner_if_needed, needs_runner_dispatch};

/// Library entry point. Equivalent to running `gos` from the
/// command line, but invokable from a `main()` that wants to do
/// pre-work first (notably the binding-runner shim).
#[must_use]
pub fn run_main() -> std::process::ExitCode {
    cli::run()
}
