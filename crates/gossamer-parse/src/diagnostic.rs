//! Parse diagnostics emitted while producing an AST.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::Span;
use thiserror::Error;

/// Every class of error the parser may emit.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ParseError {
    /// An unexpected token appeared where the grammar requires something else.
    /// `{found}` already includes its own backticks for keyword/punct
    /// tokens (`token_text` formats them), so the outer format string
    /// does not double-wrap.
    #[error("unexpected {found}, expected {expected}")]
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
    #[error("unterminated {construct} - expected `{delimiter}`")]
    Unterminated {
        /// Name of the construct (e.g. `block`, `tuple`).
        construct: String,
        /// Expected closing delimiter.
        delimiter: String,
    },
    /// A comparison operator was chained without parentheses, e.g. `a == b == c`.
    #[error("comparison operator `{op}` is non-associative - parenthesise the operands")]
    NonAssociativeCompare {
        /// Operator spelling.
        op: String,
    },
    /// A range operator was chained without parentheses, e.g. `1..2..3`.
    #[error("range operator `{op}` is non-associative - parenthesise the operands")]
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
    /// `extern "C" { ... }` or `extern "C" fn` encountered at item position.
    ///
    /// The `extern` keyword is reserved; FFI is expressed through
    /// `[rust-bindings]` in `project.toml` plus the `gossamer-binding` crate.
    #[error("extern blocks are not supported - use `[rust-bindings]` in `project.toml`")]
    ExternReserved,
    /// An expression, type, or pattern nested past the parser's hard
    /// recursion limit. Emitted to keep adversarial inputs from
    /// blowing the C stack while still letting the parser recover.
    #[error("expression nests beyond {limit} levels (consider rewriting with a helper)")]
    RecursionLimit {
        /// Configured recursion limit at which this error was raised.
        limit: u32,
    },
    /// A tokenization error (unterminated comment/string, bad escape,
    /// ...) surfaced through the parse diagnostics so it reaches the
    /// driver instead of being dropped with the lexer.
    #[error("{message}")]
    Lex {
        /// Rendered lexer diagnostic.
        message: String,
    },
    /// A bare statement appeared inside a `mod { }` body, where only items
    /// are allowed. Top-level statements belong only to the entry file's
    /// implicit `fn main`.
    #[error("statements are only allowed at the top level of the entry file")]
    StatementOutsideEntry,
    /// The entry file mixed bare top-level statements with an explicit
    /// `fn main`; an entry file uses exactly one entry form.
    #[error("cannot mix top-level statements with an explicit `fn main`")]
    MixedEntryForms,
    /// A format-macro placeholder `{...}` whose contents are neither a
    /// binding name nor a format spec - typically an expression like
    /// `{age + 1}`, which the macros do not interpolate.
    #[error("malformed format placeholder `{{{text}}}`")]
    MalformedFormatPlaceholder {
        /// The placeholder's inner text (without the braces).
        text: String,
    },
    /// A struct used in a `to_json` / `from_json` (or toml/yaml) call has a
    /// field whose type the serde synthesizer cannot handle. Without this the
    /// whole struct's serde was silently dropped and the call surfaced only as
    /// an opaque unknown-name error.
    #[error(
        "`{ty}` cannot derive `{op}`: field `{field}` has type `{field_ty}`, which is not serializable"
    )]
    SerdeUnserializableField {
        /// The struct being serialized.
        ty: String,
        /// The offending field's name.
        field: String,
        /// The offending field's type spelling.
        field_ty: String,
        /// The serde operation requested (`to_json`, `from_json`, ...).
        op: String,
    },
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
        let (code, title, help) = self.error.code_title_help();
        let mut out = Diagnostic::error(Code(code), title.clone()).with_primary(location, title);
        if let Some(help) = help {
            out = out.with_help(help);
        }
        out
    }
}

impl ParseError {
    /// Diagnostic code, title, and optional help text for this error.
    fn code_title_help(&self) -> (&'static str, String, Option<String>) {
        match self {
            ParseError::Unexpected { expected, found } => (
                "GP0001",
                format!("unexpected {found}, expected {expected}"),
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
                format!("unterminated {construct} - expected `{delimiter}`"),
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
            ParseError::ExpectedInt => ("GP0009", "expected an integer literal".to_string(), None),
            ParseError::ExpectedString => ("GP0010", "expected a string literal".to_string(), None),
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
            ParseError::MalformedAttribute => ("GP0013", "malformed attribute".to_string(), None),
            ParseError::MalformedUse => ("GP0014", "malformed `use` declaration".to_string(), None),
            ParseError::UnexpectedConstruct => ("GP0015", "unexpected construct".to_string(), None),
            ParseError::ExternReserved => (
                "GP0016",
                "extern blocks are not supported in Gossamer 0.5.0".to_string(),
                Some(
                    "FFI is expressed through the `[rust-bindings]` section of `project.toml` \
                     plus the `gossamer-binding` crate (see `docs_src/libraries.md`). \
                     Remove the `extern \"C\" { ... }` block or rewrite the binding as a \
                     Rust crate consumed via `[rust-bindings]`."
                        .to_string(),
                ),
            ),
            ParseError::RecursionLimit { limit } => (
                "GP0017",
                format!("expression nests beyond {limit} levels"),
                Some("split the expression into smaller helpers".to_string()),
            ),
            ParseError::Lex { message } => ("GP0018", message.clone(), None),
            other => other.code_title_help_entry(),
        }
    }

    /// Code/title/help for the entry-form and format-placeholder errors.
    /// Split out of [`Self::code_title_help`] to keep each match small.
    fn code_title_help_entry(&self) -> (&'static str, String, Option<String>) {
        match self {
            ParseError::StatementOutsideEntry => (
                "GP0019",
                "statements are only allowed at the top level of the entry file".to_string(),
                Some(
                    "a module body contains items only; move executable code into a function, \
                     or into the entry file's top level (its implicit `fn main`)"
                        .to_string(),
                ),
            ),
            ParseError::MixedEntryForms => (
                "GP0020",
                "cannot mix top-level statements with an explicit `fn main`".to_string(),
                Some(
                    "the entry file is already implicitly `fn main` when it carries top-level \
                     statements; move the statements into your `fn main`, or remove the explicit \
                     `fn main`"
                        .to_string(),
                ),
            ),
            ParseError::MalformedFormatPlaceholder { text } => (
                "GP0021",
                format!("malformed format placeholder `{{{text}}}`"),
                Some(
                    "format macros interpolate a binding name or a `{:spec}`, not an expression; \
                     bind it first or pass it as a positional argument with `{}`"
                        .to_string(),
                ),
            ),
            ParseError::SerdeUnserializableField {
                ty,
                field,
                field_ty,
                op,
            } => (
                "GP0022",
                format!(
                    "`{ty}` cannot derive `{op}`: field `{field}` has type `{field_ty}`, which is not serializable"
                ),
                Some(format!(
                    "give `{field}` a serializable type (scalar, String, Vec, Option, tuple, \
                     HashMap<String, _>, json::Value, or a nested struct), or hand-write `{op}`"
                )),
            ),
            // Every other variant is handled by `code_title_help`; this split
            // exists only to keep that match under the line cap.
            _ => unreachable!("code_title_help dispatches non-entry variants"),
        }
    }
}
