//! `gos mcp` - runs the model-context-protocol server over stdio.
//! Blocks until the client closes stdin.

use anyhow::{Context, Result, anyhow};

/// Entry point for `gos mcp`.
pub(crate) fn run() -> Result<()> {
    let gos_exe = std::env::current_exe().context("resolving the gos executable path")?;
    gossamer_mcp::run_stdio(gossamer_mcp::ServerConfig { gos_exe }).map_err(|e| anyhow!("mcp: {e}"))
}
