//! A filtered token stream that hides whitespace and comments from the parser.

#![forbid(unsafe_code)]

use gossamer_lex::{FileId, Keyword, Lexer, Punct, Span, Token, TokenKind};

/// Classification of a comment preserved from the raw token stream. The
/// parser does not consume doc-comment semantics yet, but keeping the
/// kind available lets later phases attach leading `//` comments to
/// their documented items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DocKind {
    /// `// ...` comment.
    Line,
    /// `/* ... */` comment.
    Block,
}

/// One stored comment: its span and kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StoredComment {
    /// Source range.
    pub span: Span,
    /// Whether the comment is line or block form.
    pub kind: DocKind,
}

/// A buffered, whitespace- and comment-filtered view over the lexer.
pub struct TokenStream {
    /// Backing file id used to construct end-of-file spans.
    file: FileId,
    /// Significant tokens (non-whitespace, non-comment).
    tokens: Vec<Token>,
    /// Current position inside `tokens`.
    position: usize,
    /// Synthetic terminating `Eof` token span.
    eof_span: Span,
    /// Stored comments in source order for later diagnostic/doc work.
    comments: Vec<StoredComment>,
}

impl TokenStream {
    /// Lexes `source`, buffers every significant token, and records
    /// comments on the side. The final entry in `tokens` is always `Eof`.
    #[must_use]
    pub fn new(source: &str, file: FileId) -> Self {
        let mut lexer = Lexer::new(source, file);
        let mut tokens = Vec::new();
        let mut comments = Vec::new();
        let mut eof_span;
        loop {
            let token = lexer.next_token();
            eof_span = token.span;
            match token.kind {
                TokenKind::Whitespace => {}
                TokenKind::LineComment => comments.push(StoredComment {
                    span: token.span,
                    kind: DocKind::Line,
                }),
                TokenKind::BlockComment => comments.push(StoredComment {
                    span: token.span,
                    kind: DocKind::Block,
                }),
                TokenKind::Eof => {
                    tokens.push(token);
                    break;
                }
                _ => tokens.push(token),
            }
        }
        Self {
            file,
            tokens,
            position: 0,
            eof_span,
            comments,
        }
    }

    /// Returns the file id the stream was built for.
    #[must_use]
    pub const fn file(&self) -> FileId {
        self.file
    }

    /// Returns the comments discovered while lexing, in source order.
    #[must_use]
    pub fn comments(&self) -> &[StoredComment] {
        &self.comments
    }

    /// Returns the current token without advancing.
    #[must_use]
    pub fn peek(&self) -> Token {
        self.tokens[self.position]
    }

    /// Returns the token `offset` positions after the cursor, clamped to `Eof`.
    #[must_use]
    pub fn peek_at(&self, offset: usize) -> Token {
        let index = self.position.saturating_add(offset);
        let last = self.tokens.len().saturating_sub(1);
        self.tokens[index.min(last)]
    }

    /// Returns the second token after the cursor.
    #[must_use]
    pub fn peek2(&self) -> Token {
        self.peek_at(1)
    }

    /// Returns the current cursor position as a checkpoint for rewinding.
    #[must_use]
    pub const fn checkpoint(&self) -> usize {
        self.position
    }

    /// Returns the span of the most recently consumed token. Falls back
    /// to the synthetic EOF span when nothing has been consumed yet.
    #[must_use]
    pub fn previous_span(&self) -> Span {
        if self.position == 0 {
            return self.tokens[0].span;
        }
        self.tokens[self.position - 1].span
    }

    /// Rewinds the cursor to a previously captured checkpoint.
    pub fn rewind(&mut self, mark: usize) {
        self.position = mark;
    }

    /// Consumes and returns the current token.
    pub fn bump(&mut self) -> Token {
        let token = self.peek();
        if !matches!(token.kind, TokenKind::Eof) {
            self.position += 1;
        }
        token
    }

    /// Returns `true` when the next token matches `kind`.
    #[must_use]
    pub fn at_kind(&self, kind: TokenKind) -> bool {
        self.peek().kind == kind
    }

    /// Returns `true` when the next token is the given keyword.
    #[must_use]
    pub fn at_keyword(&self, keyword: Keyword) -> bool {
        matches!(self.peek().kind, TokenKind::Keyword(found) if found == keyword)
    }

    /// Returns `true` when the next token is the given punctuation.
    #[must_use]
    pub fn at_punct(&self, punct: Punct) -> bool {
        matches!(self.peek().kind, TokenKind::Punct(found) if found == punct)
    }

    /// If the next token is `keyword`, consume it and return `true`.
    pub fn eat_keyword(&mut self, keyword: Keyword) -> bool {
        if self.at_keyword(keyword) {
            self.bump();
            return true;
        }
        false
    }

    /// If the next token is `punct`, consume it and return `true`.
    pub fn eat_punct(&mut self, punct: Punct) -> bool {
        if self.at_punct(punct) {
            self.bump();
            return true;
        }
        false
    }

    /// Returns `true` when the cursor is at the synthetic `Eof` token.
    #[must_use]
    pub fn at_eof(&self) -> bool {
        matches!(self.peek().kind, TokenKind::Eof)
    }

    /// Returns the span that the parser should use for diagnostics beyond
    /// the end of input.
    #[must_use]
    pub const fn eof_span(&self) -> Span {
        self.eof_span
    }
}
