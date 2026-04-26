//! Error types reported by the lexer during tokenization.

use crate::span::Span;

/// A single lexer diagnostic produced alongside an emitted `Token::Invalid`
/// or an otherwise-recoverable tokenization error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LexError {
    /// Encountered a byte sequence that does not start any valid token.
    #[error("unexpected character at byte {}", .span.start)]
    UnexpectedChar {
        /// Location of the offending byte(s).
        span: Span,
    },
    /// A `/* ... */` block comment ran to end of file without `*/`.
    #[error("unterminated block comment")]
    UnterminatedBlockComment {
        /// Span of the `/*` that opened the comment.
        span: Span,
    },
    /// A string literal ran to end of file without its closing quote.
    #[error("unterminated string literal")]
    UnterminatedString {
        /// Span of the opening quote.
        span: Span,
    },
    /// A raw string literal ran to end of file without its closing delimiter.
    #[error("unterminated raw string literal")]
    UnterminatedRawString {
        /// Span of the opening `r#...\"` sequence.
        span: Span,
    },
    /// A character literal ran to end of file or newline without its closing quote.
    #[error("unterminated character literal")]
    UnterminatedChar {
        /// Span of the opening quote.
        span: Span,
    },
    /// A character literal held zero or multiple Unicode scalar values.
    #[error("character literal must contain exactly one character")]
    BadCharLiteralLength {
        /// Span covering the literal including both quotes.
        span: Span,
    },
    /// An escape sequence like `\q` or a truncated `\x` was encountered.
    #[error("invalid escape sequence")]
    BadEscape {
        /// Span of the offending escape, starting at the backslash.
        span: Span,
    },
    /// A `\u{...}` escape named a value that is not a Unicode scalar.
    #[error("invalid unicode escape")]
    BadUnicodeEscape {
        /// Span of the full `\u{...}` sequence.
        span: Span,
    },
    /// A numeric literal used a digit outside its declared base.
    #[error("invalid digit for numeric base")]
    BadNumericDigit {
        /// Span of the offending digit.
        span: Span,
    },
    /// A numeric literal had no digits after its base prefix or `_` placeholder.
    #[error("numeric literal has no digits")]
    EmptyNumericLiteral {
        /// Span of the partial literal.
        span: Span,
    },
}

impl LexError {
    /// Returns the span this diagnostic points at.
    #[must_use]
    pub const fn span(&self) -> Span {
        match *self {
            Self::UnexpectedChar { span }
            | Self::UnterminatedBlockComment { span }
            | Self::UnterminatedString { span }
            | Self::UnterminatedRawString { span }
            | Self::UnterminatedChar { span }
            | Self::BadCharLiteralLength { span }
            | Self::BadEscape { span }
            | Self::BadUnicodeEscape { span }
            | Self::BadNumericDigit { span }
            | Self::EmptyNumericLiteral { span } => span,
        }
    }
}
