//! Core parser state shared across all productions.

#![forbid(unsafe_code)]

use gossamer_ast::{NodeId, NodeIdGenerator};
use gossamer_lex::{FileId, Keyword, Punct, Span, Token, TokenKind};

use crate::diagnostic::{ParseDiagnostic, ParseError};
use crate::stream::TokenStream;

/// Hand-written recursive-descent parser over a buffered token stream.
pub struct Parser<'src> {
    /// Raw source text (so the parser can recover identifier names and
    /// preserve literal spellings).
    pub(crate) source: &'src str,
    /// Token source.
    pub(crate) tokens: TokenStream,
    /// Monotonic AST id generator.
    pub(crate) ids: NodeIdGenerator,
    /// Accumulated diagnostics (drained by `parse_source_file`).
    pub(crate) diagnostics: Vec<ParseDiagnostic>,
    /// Depth of nested contexts that forbid an unparenthesised struct
    /// literal (`if`, `while`, `match` scrutinee).
    pub(crate) no_struct_literal_depth: u32,
    /// Depth of contexts where `|` denotes a pattern alternative and
    /// must not be consumed as bitwise-or by the Pratt loop.
    pub(crate) pattern_pipe_depth: u32,
}

impl<'src> Parser<'src> {
    /// Builds a parser for `source` tagged with `file`.
    #[must_use]
    pub fn new(source: &'src str, file: FileId) -> Self {
        Self {
            source,
            tokens: TokenStream::new(source, file),
            ids: NodeIdGenerator::new(),
            diagnostics: Vec::new(),
            no_struct_literal_depth: 0,
            pattern_pipe_depth: 0,
        }
    }

    /// Returns the file id being parsed.
    #[must_use]
    pub fn file(&self) -> FileId {
        self.tokens.file()
    }

    /// Allocates the next fresh AST node id.
    pub(crate) fn alloc_id(&mut self) -> NodeId {
        self.ids.next()
    }

    /// Builds a span covering [`lo.start`, `hi.end`) in the current file.
    #[must_use]
    pub(crate) fn join(&self, lo: Span, hi: Span) -> Span {
        Span::new(
            self.tokens.file(),
            lo.start.min(hi.start),
            lo.end.max(hi.end),
        )
    }

    /// Records a diagnostic without stopping the parser.
    pub(crate) fn record(&mut self, error: ParseError, span: Span) {
        self.diagnostics.push(ParseDiagnostic::new(error, span));
    }

    /// Returns the accumulated diagnostics, leaving the parser's vector empty.
    pub fn take_diagnostics(&mut self) -> Vec<ParseDiagnostic> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Returns the span of the current (peeked) token.
    #[must_use]
    pub(crate) fn peek_span(&self) -> Span {
        self.tokens.peek().span
    }

    /// Peeks the current token.
    #[must_use]
    pub(crate) fn peek(&self) -> Token {
        self.tokens.peek()
    }

    /// Peeks the nth token after the cursor.
    #[must_use]
    pub(crate) fn peek_nth(&self, offset: usize) -> Token {
        self.tokens.peek_at(offset)
    }

    /// Consumes the current token and returns it.
    pub(crate) fn bump(&mut self) -> Token {
        self.tokens.bump()
    }

    /// Returns the span of the most recently consumed token, or the
    /// current token's span when nothing has been consumed yet. Used
    /// to close a span range after `start..`.
    #[must_use]
    pub(crate) fn last_span(&self) -> Span {
        let position = self.tokens.checkpoint();
        if position == 0 {
            return self.peek_span();
        }
        self.tokens.previous_span()
    }

    /// Returns `true` at end of input.
    #[must_use]
    pub(crate) fn at_eof(&self) -> bool {
        self.tokens.at_eof()
    }

    /// Returns `true` when the current token matches a punctuation kind.
    #[must_use]
    pub(crate) fn at_punct(&self, punct: Punct) -> bool {
        self.tokens.at_punct(punct)
    }

    /// Returns `true` when the current token matches a keyword.
    #[must_use]
    pub(crate) fn at_keyword(&self, keyword: Keyword) -> bool {
        self.tokens.at_keyword(keyword)
    }

    /// Attempts to consume `punct`, returning whether it was present.
    pub(crate) fn eat_punct(&mut self, punct: Punct) -> bool {
        self.tokens.eat_punct(punct)
    }

    /// Attempts to consume `keyword`, returning whether it was present.
    pub(crate) fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        self.tokens.eat_keyword(keyword)
    }

    /// Consumes `punct` or records a diagnostic if absent.
    pub(crate) fn expect_punct(&mut self, punct: Punct, context: &str) -> bool {
        if self.eat_punct(punct) {
            return true;
        }
        let found = self.peek_text();
        self.record(
            ParseError::Unexpected {
                expected: format!("`{}` {}", punct.as_str(), context),
                found,
            },
            self.peek_span(),
        );
        false
    }

    /// Consumes `keyword` or records a diagnostic if absent.
    pub(crate) fn expect_keyword(&mut self, keyword: Keyword, context: &str) -> bool {
        if self.eat_keyword(keyword) {
            return true;
        }
        let found = self.peek_text();
        self.record(
            ParseError::Unexpected {
                expected: format!("`{}` {}", keyword.as_str(), context),
                found,
            },
            self.peek_span(),
        );
        false
    }

    /// Returns a short human-readable description of the current token,
    /// used when composing "unexpected token" diagnostics.
    #[must_use]
    pub(crate) fn peek_text(&self) -> String {
        token_text(self.peek())
    }

    /// Consumes an identifier token and returns its span if present.
    pub(crate) fn eat_ident(&mut self) -> Option<Span> {
        if matches!(self.peek().kind, TokenKind::Ident) {
            return Some(self.bump().span);
        }
        None
    }

    /// Enters a scope where unparenthesised struct literals are forbidden.
    pub(crate) fn enter_no_struct(&mut self) {
        self.no_struct_literal_depth = self.no_struct_literal_depth.saturating_add(1);
    }

    /// Leaves a scope where unparenthesised struct literals are forbidden.
    pub(crate) fn leave_no_struct(&mut self) {
        self.no_struct_literal_depth = self.no_struct_literal_depth.saturating_sub(1);
    }

    /// `true` when a struct literal is currently forbidden without parens.
    #[must_use]
    pub(crate) const fn struct_literal_forbidden(&self) -> bool {
        self.no_struct_literal_depth > 0
    }

    /// Enters a scope where `|` denotes a pattern alternative.
    pub(crate) fn enter_pattern_pipe(&mut self) {
        self.pattern_pipe_depth = self.pattern_pipe_depth.saturating_add(1);
    }

    /// Leaves a scope where `|` denotes a pattern alternative.
    pub(crate) fn leave_pattern_pipe(&mut self) {
        self.pattern_pipe_depth = self.pattern_pipe_depth.saturating_sub(1);
    }

    /// `true` when the Pratt loop must treat bitwise `|` as a pattern separator.
    #[must_use]
    pub(crate) const fn in_pattern_pipe(&self) -> bool {
        self.pattern_pipe_depth > 0
    }

    /// Returns the raw source slice covered by `span`.
    #[must_use]
    pub(crate) fn slice(&self, span: Span) -> &'src str {
        let start = span.start as usize;
        let end = span.end as usize;
        if end > self.source.len() || start > end {
            return "";
        }
        &self.source[start..end]
    }
}

/// Returns a short human-readable rendering of a token for diagnostics.
fn token_text(token: Token) -> String {
    match token.kind {
        TokenKind::Eof => "<end of input>".to_string(),
        TokenKind::Keyword(keyword) => format!("keyword `{}`", keyword.as_str()),
        TokenKind::Punct(punct) => format!("`{}`", punct.as_str()),
        TokenKind::Ident => "identifier".to_string(),
        TokenKind::IntLit => "integer literal".to_string(),
        TokenKind::FloatLit => "float literal".to_string(),
        TokenKind::StringLit | TokenKind::RawStringLit { .. } => "string literal".to_string(),
        TokenKind::CharLit => "char literal".to_string(),
        TokenKind::ByteLit => "byte literal".to_string(),
        TokenKind::ByteStringLit | TokenKind::RawByteStringLit { .. } => {
            "byte string literal".to_string()
        }
        TokenKind::LineComment | TokenKind::BlockComment => "comment".to_string(),
        TokenKind::Whitespace => "whitespace".to_string(),
        TokenKind::Invalid => "invalid token".to_string(),
    }
}
