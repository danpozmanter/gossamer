//! `gos lsp` — runs the language server over stdio. Blocks until
//! the client sends `exit` (after `shutdown`) or closes stdin.

use anyhow::{Result, anyhow};

/// Entry point for `gos lsp`.
pub(crate) fn run() -> Result<()> {
    gossamer_lsp::run_stdio().map_err(|e| anyhow!("lsp: {e}"))
}
