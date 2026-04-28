//! `gos` — command-line entry point for the Gossamer toolchain.
//!
//! The actual subcommand surface lives in [`crate::cli`] (clap
//! derive + dispatch table) and the per-command logic in
//! [`crate::cmd::*`]. Keeping `main.rs` to a single function makes
//! `gos` boot-time wiring trivial to read: register a panic-friendly
//! exit code, hand off to `cli::run`.

#![forbid(unsafe_code)]
#![allow(clippy::similar_names, clippy::ptr_arg)]

mod cli;
mod cmd;
mod doc;
mod loaders;
mod paths;
mod repl;
mod repl_helper;
mod style;

use std::process::ExitCode;

/// Entry point. Returns a non-zero exit code when a subcommand fails.
fn main() -> ExitCode {
    cli::run()
}
