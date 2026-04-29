//! Language-server-protocol adapter for Gossamer.
//! Runs an LSP server over stdio. Three capabilities land in this
//! first slice:
//! - `textDocument/publishDiagnostics` on open / change — full
//!   parse + resolve + typecheck pipeline per document.
//! - `textDocument/hover` — displays the interned type of the
//!   symbol under the cursor when the type checker can resolve it.
//! - `textDocument/definition` — jumps to the declaring item when
//!   the cursor is on a path expression.
//!
//!

#![forbid(unsafe_code)]

mod inlay;
mod navigation;
mod protocol;
mod semantic_tokens;
mod server;
mod session;
mod stdlib_index;
mod symbols;
mod workspace_index;

pub use server::run_stdio;
