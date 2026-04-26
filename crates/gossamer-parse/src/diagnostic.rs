//! Parse diagnostics emitted while producing an AST.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::Span;
use thiserror::Error;

/// Every class of error the parser may emit.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// An unexpected token appeared where the grammar requires something else.
    #[error("unexpected token `{found}`, expected {expected}")]
    Unexpected {
        /// Human-readable description of what was expected.
        expected: String,
        /// Source text of the token that was actually seen.
        found: String,
    },
    /// End of file encountered while parsing a construct.
    #[error("unexpected end of input while parsing {construct}")]
    UnexpectedEof {
        /// Name of the construct being parsed.
        construct: String,
    },
    /// A construct required to be terminated was not.
    #[error("unterminated {construct} — expected `{delimiter}`")]
    Unterminated {
        /// Name of the construct (e.g. `block`, `tuple`).
        construct: String,
        /// Expected closing delimiter.
        delimiter: String,
    },
    /// A comparison operator was chained without parentheses, e.g. `a == b == c`.
    #[error("comparison operator `{op}` is non-associative — parenthesise the operands")]
    NonAssociativeCompare {
        /// Operator spelling.
        op: String,
    },
    /// A range operator was chained without parentheses, e.g. `1..2..3`.
    #[error("range operator `{op}` is non-associative — parenthesise the operands")]
    NonAssociativeRange {
        /// Operator spelling.
        op: String,
    },
    /// A braced struct literal appeared directly in the scrutinee of an
    /// `if`, `while`, or `match`, where it is ambiguous with the block start.
    #[error("struct literal must be parenthesised in `if`/`while`/`match` scrutinee")]
    StructLiteralNeedsParens,
    /// The right-hand side of `|>` did not match any of the forms in SPEC §4.6.
    #[error("E0601: right-hand side of `|>` must be a callable")]
    PipeRhsInvalid,
    /// An assignment appeared in a non-statement expression position.
    #[error("assignment is only valid at statement position")]
    AssignmentNotAllowed,
    /// An integer literal is required by the grammar at this position.
    #[error("expected an integer literal")]
    ExpectedInt,
    /// A string literal is required by the grammar at this position.
    #[error("expected a string literal")]
    ExpectedString,
    /// A trailing integer produced an invalid tuple index (`foo.0xff`, etc.).
    #[error("invalid tuple index")]
    InvalidTupleIndex,
    /// A label token is malformed (missing identifier after `'`).
    #[error("expected a label identifier after `'`")]
    MalformedLabel,
    /// An unsupported or malformed attribute.
    #[error("malformed attribute")]
    MalformedAttribute,
    /// A use declaration target could not be parsed.
    #[error("malformed `use` declaration")]
    MalformedUse,
    /// Two consecutive tokens formed something the parser does not recognise.
    #[error("unexpected construct")]
    UnexpectedConstruct,
}

/// A diagnostic with its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseDiagnostic {
    /// Classification of the error.
    pub error: ParseError,
    /// Source range the diagnostic refers to.
    pub span: Span,
}

impl ParseDiagnostic {
    /// Builds a diagnostic from an error and span.
    #[must_use]
    pub const fn new(error: ParseError, span: Span) -> Self {
        Self { error, span }
    }
}

impl fmt::Display for ParseDiagnostic {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            out,
            "{}..{}: {}",
            self.span.start, self.span.end, self.error
        )
    }
}

impl std::error::Error for ParseDiagnostic {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        Some(&self.error)
    }
}

impl ParseDiagnostic {
    /// Renders this parse diagnostic as a structured
    /// [`gossamer_diagnostics::Diagnostic`].
    #[must_use]
    pub fn to_diagnostic(&self) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Code, Diagnostic, Location};
        let location = Location::new(self.span.file, self.span);
        let (code, title, help): (&'static str, String, Option<String>) = match &self.error {
            ParseError::Unexpected { expected, found } => (
                "GP0001",
                format!("unexpected token `{found}`, expected {expected}"),
                None,
            ),
            ParseError::UnexpectedEof { construct } => (
                "GP0002",
                format!("unexpected end of input while parsing {construct}"),
                Some(format!("finish the {construct} or remove it")),
            ),
            ParseError::Unterminated {
                construct,
                delimiter,
            } => (
                "GP0003",
                format!("unterminated {construct} — expected `{delimiter}`"),
                Some(format!("add `{delimiter}` to close the {construct}")),
            ),
            ParseError::NonAssociativeCompare { op } => (
                "GP0004",
                format!("comparison operator `{op}` is non-associative"),
                Some("parenthesise the operands".to_string()),
            ),
            ParseError::NonAssociativeRange { op } => (
                "GP0005",
                format!("range operator `{op}` is non-associative"),
                Some("parenthesise the operands".to_string()),
            ),
            ParseError::StructLiteralNeedsParens => (
                "GP0006",
                "struct literal must be parenthesised in an `if`/`while`/`match` scrutinee"
                    .to_string(),
                Some("wrap the struct literal in `(...)`".to_string()),
            ),
            ParseError::PipeRhsInvalid => (
                "GP0007",
                "right-hand side of `|>` must be a callable".to_string(),
                None,
            ),
            ParseError::AssignmentNotAllowed => (
                "GP0008",
                "assignment is only valid at statement position".to_string(),
                None,
            ),
            ParseError::ExpectedInt => (
                "GP0009",
                "expected an integer literal".to_string(),
                None,
            ),
            ParseError::ExpectedString => (
                "GP0010",
                "expected a string literal".to_string(),
                None,
            ),
            ParseError::InvalidTupleIndex => (
                "GP0011",
                "invalid tuple index".to_string(),
                Some("tuple indices must be plain decimal integers".to_string()),
            ),
            ParseError::MalformedLabel => (
                "GP0012",
                "expected a label identifier after `'`".to_string(),
                None,
            ),
            ParseError::MalformedAttribute => (
                "GP0013",
                "malformed attribute".to_string(),
                None,
            ),
            ParseError::MalformedUse => (
                "GP0014",
                "malformed `use` declaration".to_string(),
                None,
            ),
            ParseError::UnexpectedConstruct => (
                "GP0015",
                "unexpected construct".to_string(),
                None,
            ),
        };
        let mut out = Diagnostic::error(Code(code), title.clone())
            .with_primary(location, title);
        if let Some(help) = help {
            out = out.with_help(help);
        }
        out
    }
}
