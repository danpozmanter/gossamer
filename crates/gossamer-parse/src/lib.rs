//! Parser from Gossamer tokens to AST.
//! The parser is a hand-written recursive descent driver with a Pratt
//! loop for expressions. It consumes the lexer's token stream directly
//! and emits a best-effort `SourceFile` alongside a list of diagnostics.
//! The parser never panics on malformed input — unexpected tokens
//! resynchronise to the next item or statement boundary and are
//! reported via `ParseDiagnostic`.

#![forbid(unsafe_code)]

mod diagnostic;
mod expressions;
mod generics;
mod items;
mod parser;
mod patterns;
mod recovery;
mod statements;
mod stream;
mod types;
mod use_decls;

pub use diagnostic::{ParseDiagnostic, ParseError};
pub use parser::Parser;
pub use stream::{DocKind, StoredComment, TokenStream};

use gossamer_ast::SourceFile;
use gossamer_lex::{FileId, Keyword};

/// Parses `source` into a `SourceFile` AST and returns any diagnostics
/// collected along the way.
#[must_use]
pub fn parse_source_file(source: &str, file: FileId) -> (SourceFile, Vec<ParseDiagnostic>) {
    let mut parser = Parser::new(source, file);
    let mut uses = Vec::new();
    while parser.at_keyword_public(Keyword::Use) {
        let use_decl = parser.parse_use_decl();
        uses.push(use_decl);
    }
    let mut items = Vec::new();
    while !parser.at_eof_public() {
        let before = parser.checkpoint_public();
        let item = parser.parse_item();
        items.push(item);
        if parser.checkpoint_public() == before {
            parser.recover_to_item_start_public();
        }
    }
    let source_file = SourceFile::new(file, uses, items);
    let diagnostics = parser.take_diagnostics();
    (source_file, diagnostics)
}

// Public shims so `parse_source_file` can talk to the parser across the
// module boundary without exposing internal helpers as part of the
// `Parser` type's public API.
impl Parser<'_> {
    /// Returns `true` when the cursor is at `keyword`. Public facade
    /// used by `parse_source_file`.
    #[must_use]
    pub fn at_keyword_public(&self, keyword: Keyword) -> bool {
        self.at_keyword(keyword)
    }

    /// Returns `true` when the cursor is at end of input. Public facade
    /// used by `parse_source_file`.
    #[must_use]
    pub fn at_eof_public(&self) -> bool {
        self.at_eof()
    }

    /// Captures the current position for progress detection at the
    /// top-level item loop.
    #[must_use]
    pub fn checkpoint_public(&self) -> usize {
        self.tokens.checkpoint()
    }

    /// Public facade that forwards to the item-start recovery helper.
    pub fn recover_to_item_start_public(&mut self) {
        self.recover_to_item_start();
    }
}
