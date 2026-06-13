//! Parser from Gossamer tokens to AST.
//! The parser is a hand-written recursive descent driver with a Pratt
//! loop for expressions. It consumes the lexer's token stream directly
//! and emits a best-effort `SourceFile` alongside a list of diagnostics.
//! The parser never panics on malformed input — unexpected tokens
//! resynchronise to the next item or statement boundary and are
//! reported via `ParseDiagnostic`.

#![forbid(unsafe_code)]

pub mod autoderive;
mod diagnostic;
mod expressions;
mod format;
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
pub use format::{FormatError, format_source};
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
            // parse_item left us where we started — guarantee forward
            // progress so an adversarial input cannot pin the loop and
            // blow `items` up to gigabytes of stub allocations.
            parser.bump_public();
            parser.recover_to_item_start_public();
        }
    }
    // Pull `use` decls hoisted out of inline `mod ... { ... }` bodies
    // up to the source-file level so the resolver's top-level
    // `collect_imports` walk picks them up. Single-segment local
    // module imports (`use util`, `use chat`) are dropped: with the
    // sibling auto-bundle, those names are already inline modules
    // at the top level, and re-importing them would clash with the
    // module's own [`gossamer_resolve::DefKind::Mod`] binding.
    uses.extend(
        parser
            .take_hoisted_uses()
            .into_iter()
            .filter(|u| !is_local_single_segment_use(u)),
    );
    let source_file = SourceFile::new(file, uses, items);
    let diagnostics = parser.take_diagnostics();
    (source_file, diagnostics)
}

/// `true` for `use NAME` (no `::` segments, no brace list, no project
/// id) — these reference an intra-project sibling module that the
/// sibling auto-bundle already exposes as an inline `mod NAME { ... }`.
fn is_local_single_segment_use(decl: &gossamer_ast::UseDecl) -> bool {
    if decl.list.is_some() {
        return false;
    }
    match &decl.target {
        gossamer_ast::UseTarget::Module(path) => path.segments.len() == 1,
        gossamer_ast::UseTarget::Project { .. } => false,
    }
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

    /// Single-token advance used as the outer-loop's last-resort
    /// progress guarantee when an item-parser is unable to recover.
    pub fn bump_public(&mut self) {
        if !self.at_eof() {
            self.bump();
        }
    }
}
