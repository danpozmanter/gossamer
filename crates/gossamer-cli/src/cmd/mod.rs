//! Subcommand implementations for `gos`.
//!
//! `main.rs` parses the [`crate::cli::Cli`] enum and dispatches each
//! variant into the matching module here. Keeping the per-command
//! logic in dedicated files makes `main.rs` a routing table — the
//! place to look when a flag stops landing where you expect.

pub(crate) mod attr_walk;
pub(crate) mod bench;
pub(crate) mod build;
pub(crate) mod check;
pub(crate) mod clean;
pub(crate) mod env_cmd;
pub(crate) mod explain;
pub(crate) mod fmt_cmd;
pub(crate) mod lint_cmd;
pub(crate) mod lsp_cmd;
pub(crate) mod parse;
pub(crate) mod pkg;
pub(crate) mod run;
pub(crate) mod scaffold;
pub(crate) mod skill_prompt;
pub(crate) mod test;
pub(crate) mod watch;

pub(crate) use run::RunMode;
pub(crate) use test::TestOpts;
