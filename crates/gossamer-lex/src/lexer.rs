//! Top-level lexer driver. Ties the sub-modules together and exposes a
//! `Lexer` that yields tokens until end of input.

use crate::comment::{CommentOutcome, lex_comment};
use crate::cursor::Cursor;
use crate::diagnostic::LexError;
use crate::number::lex_number;
use crate::punct::lex_punct;
use crate::span::{FileId, Span};
use crate::string::{
    QuotedOutcome, lex_byte, lex_byte_string, lex_char, lex_raw_string, lex_string,
};
use crate::token::{Keyword, Token, TokenKind};

/// Streaming lexer that yields one `Token` per call to `next_token`.
///
/// The lexer is infallible: on bad input it emits `TokenKind::Invalid`
/// or partial-but-classified tokens and records a `LexError` on the
/// side. Callers drain diagnostics via `take_diagnostics` at any point.
pub struct Lexer<'src> {
    cursor: Cursor<'src>,
    file: FileId,
    diagnostics: Vec<LexError>,
}

impl<'src> Lexer<'src> {
    /// Constructs a lexer for `source` tagged with `file`.
    #[must_use]
    pub const fn new(source: &'src str, file: FileId) -> Self {
        Self {
            cursor: Cursor::new(source),
            file,
            diagnostics: Vec::new(),
        }
    }

    /// Returns `true` when the lexer has consumed all of its input.
    #[must_use]
    pub fn is_eof(&self) -> bool {
        self.cursor.is_eof()
    }

    /// Removes and returns every diagnostic collected since the last drain.
    pub fn take_diagnostics(&mut self) -> Vec<LexError> {
        std::mem::take(&mut self.diagnostics)
    }

    /// Consumes and returns the next token, or `Eof` at end of input.
    pub fn next_token(&mut self) -> Token {
        let start = self.current_offset();
        let first = self.cursor.peek();
        if self.cursor.is_eof() {
            return Token::new(TokenKind::Eof, self.span_from(start));
        }
        let kind = self.dispatch(first, start);
        Token::new(kind, self.span_from(start))
    }

    /// Dispatches to the appropriate helper based on the first character.
    fn dispatch(&mut self, first: char, start: u32) -> TokenKind {
        if is_whitespace(first) {
            self.cursor.bump_while(is_whitespace);
            return TokenKind::Whitespace;
        }
        if first == '/'
            && let Some(kind) = self.try_comment(start)
        {
            return kind;
        }
        if first == '"' {
            return self.finish_string(start);
        }
        if first == '\'' {
            return self.finish_char(start);
        }
        if first.is_ascii_digit() {
            return lex_number(&mut self.cursor);
        }
        if is_ident_start(first) {
            return self.lex_ident_or_prefix(start);
        }
        self.lex_punct_or_invalid(start)
    }

    /// Attempts to lex a `//` or `/* */` comment starting at the current
    /// `/`. Returns `None` if the `/` is an operator, not a comment.
    fn try_comment(&mut self, start: u32) -> Option<TokenKind> {
        match lex_comment(&mut self.cursor) {
            CommentOutcome::Lexed(kind) => Some(kind),
            CommentOutcome::Unterminated => {
                self.diagnostics.push(LexError::UnterminatedBlockComment {
                    span: self.span_from(start),
                });
                Some(TokenKind::BlockComment)
            }
            CommentOutcome::NotAComment => None,
        }
    }

    /// Classifies a run of identifier characters as a keyword, a
    /// string-prefix literal (`r"..."`, `b"..."`, `br"..."`), or a
    /// plain identifier.
    fn lex_ident_or_prefix(&mut self, start: u32) -> TokenKind {
        if let Some(kind) = self.try_prefixed_string(start) {
            return kind;
        }
        self.cursor.bump_while(is_ident_continue);
        let text = &self.cursor.source()[start as usize..self.current_offset() as usize];
        Keyword::from_ident(text).map_or(TokenKind::Ident, TokenKind::Keyword)
    }

    /// If the cursor sits at `r"`, `r#`, `b"`, `b'`, or `br"`/`br#`,
    /// lexes the corresponding literal and returns its token kind.
    fn try_prefixed_string(&mut self, start: u32) -> Option<TokenKind> {
        match (self.cursor.peek(), self.cursor.peek_nth(1)) {
            ('r', '"' | '#') => Some(self.drive_raw_string(start, false)),
            ('b', '"') => Some(self.drive_byte_string(start)),
            ('b', '\'') => Some(self.drive_byte_literal(start)),
            ('b', 'r') if matches!(self.cursor.peek_nth(2), '"' | '#') => {
                self.cursor.bump();
                Some(self.drive_raw_string(start, true))
            }
            _ => None,
        }
    }

    /// Lexes a `"..."` string literal and forwards any diagnostics.
    fn finish_string(&mut self, start: u32) -> TokenKind {
        let outcome = lex_string(&mut self.cursor, self.file, start);
        self.absorb_quoted(outcome)
    }

    /// Lexes a `'x'` character literal and forwards any diagnostics.
    fn finish_char(&mut self, start: u32) -> TokenKind {
        let outcome = lex_char(&mut self.cursor, self.file, start);
        self.absorb_quoted(outcome)
    }

    /// Drives the raw-string sub-lexer for either `r"..."` or `br"..."`.
    fn drive_raw_string(&mut self, start: u32, byte_flavor: bool) -> TokenKind {
        let outcome = lex_raw_string(&mut self.cursor, self.file, start, byte_flavor);
        self.absorb_quoted(outcome)
    }

    /// Drives the byte-string sub-lexer for `b"..."`.
    fn drive_byte_string(&mut self, start: u32) -> TokenKind {
        self.cursor.bump();
        let outcome = lex_byte_string(&mut self.cursor, self.file, start);
        self.absorb_quoted(outcome)
    }

    /// Drives the byte-literal sub-lexer for `b'x'`.
    fn drive_byte_literal(&mut self, start: u32) -> TokenKind {
        self.cursor.bump();
        let outcome = lex_byte(&mut self.cursor, self.file, start);
        self.absorb_quoted(outcome)
    }

    /// Attempts to lex a punctuation token at the current cursor.
    /// Emits `Invalid` plus a diagnostic when nothing matches.
    fn lex_punct_or_invalid(&mut self, start: u32) -> TokenKind {
        if let Some(punct) = lex_punct(&mut self.cursor) {
            TokenKind::Punct(punct)
        } else {
            self.cursor.bump();
            self.diagnostics.push(LexError::UnexpectedChar {
                span: self.span_from(start),
            });
            TokenKind::Invalid
        }
    }

    /// Moves diagnostics out of a `QuotedOutcome` into the lexer state.
    fn absorb_quoted(&mut self, outcome: QuotedOutcome) -> TokenKind {
        self.diagnostics.extend(outcome.diagnostics);
        outcome.kind
    }

    /// Returns the current cursor byte offset as a `u32`.
    fn current_offset(&self) -> u32 {
        u32::try_from(self.cursor.offset()).unwrap_or(u32::MAX)
    }

    /// Builds a span from `start` to the cursor's current offset.
    fn span_from(&self, start: u32) -> Span {
        Span::new(self.file, start, self.current_offset())
    }
}

/// Returns `true` when `character` is ASCII whitespace.
fn is_whitespace(character: char) -> bool {
    matches!(character, ' ' | '\t' | '\r' | '\n')
}

/// Returns `true` when `character` may start an identifier.
fn is_ident_start(character: char) -> bool {
    character == '_' || character.is_ascii_alphabetic()
}

/// Returns `true` when `character` may continue an identifier.
fn is_ident_continue(character: char) -> bool {
    character == '_' || character.is_ascii_alphanumeric()
}

/// Convenience helper: collect every token of `source` into a `Vec`.
#[must_use]
pub fn tokenize(source: &str, file: FileId) -> (Vec<Token>, Vec<LexError>) {
    let mut lexer = Lexer::new(source, file);
    let mut tokens = Vec::new();
    loop {
        let token = lexer.next_token();
        let done = token.kind == TokenKind::Eof;
        tokens.push(token);
        if done {
            break;
        }
    }
    let diagnostics = lexer.take_diagnostics();
    (tokens, diagnostics)
}
