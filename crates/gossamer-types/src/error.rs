//! Diagnostics emitted by the type checker.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::Span;
use thiserror::Error;

/// One type-checker diagnostic paired with its source location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDiagnostic {
    /// Specific error variant.
    pub error: TypeError,
    /// Where in the source the error was detected.
    pub span: Span,
}

impl TypeDiagnostic {
    /// Constructs a diagnostic from its error and span.
    #[must_use]
    pub const fn new(error: TypeError, span: Span) -> Self {
        Self { error, span }
    }
}

impl fmt::Display for TypeDiagnostic {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.error)
    }
}

/// Every failure mode the type checker can report.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TypeError {
    /// Two concrete types that should be equal are not.
    #[error("type mismatch: expected `{expected}`, found `{found}`")]
    TypeMismatch {
        /// Expected type, rendered via [`crate::render_ty`].
        expected: String,
        /// Found type, rendered via [`crate::render_ty`].
        found: String,
    },
    /// A method call could not be resolved to any known definition.
    #[error("no method named `{name}` found for type `{ty}`")]
    UnresolvedMethod {
        /// Receiver type.
        ty: String,
        /// Method name.
        name: String,
    },
    /// A binary or unary operator could not be resolved for the given
    /// operand types.
    #[error("cannot apply `{op}` to `{lhs}` and `{rhs}`")]
    UnresolvedOp {
        /// Operator symbol.
        op: String,
        /// Left-hand type.
        lhs: String,
        /// Right-hand type (for unary ops this is the operand).
        rhs: String,
    },
    /// A `match` expression lacks coverage for one or more patterns.
    #[error("non-exhaustive patterns: {missing}")]
    NonExhaustiveMatch {
        /// Human-readable description of the missing patterns.
        missing: String,
    },
    /// `value as T` was requested between two types that are not in
    /// the `as`-cast whitelist (non-primitive source, struct source,
    /// etc.).
    #[error("non-primitive cast: `{from}` as `{to}`")]
    InvalidCast {
        /// Source type.
        from: String,
        /// Target type.
        to: String,
    },
    /// Field access (`value.field`) on a type that has no such field.
    /// Splits two failure modes: `opaque` is true when the receiver's
    /// type is known but the checker has no field map for it (typical
    /// of dynamic stdlib types like `json::Value`); `opaque` is false
    /// when the type does have fields but `field` isn't one of them.
    #[error("type `{ty}` has no field `{field}`")]
    UnknownField {
        /// Receiver type.
        ty: String,
        /// Field name attempted.
        field: String,
        /// `true` when the receiver is opaque to the checker.
        opaque: bool,
    },
}

impl TypeError {
    /// Returns a short stable tag useful for snapshot tests.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::TypeMismatch { .. } => "type-mismatch",
            Self::UnresolvedMethod { .. } => "unresolved-method",
            Self::UnresolvedOp { .. } => "unresolved-op",
            Self::NonExhaustiveMatch { .. } => "non-exhaustive-match",
            Self::InvalidCast { .. } => "invalid-cast",
            Self::UnknownField { .. } => "unknown-field",
        }
    }

    /// Stable error code used by the diagnostics framework.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::TypeMismatch { .. } => "GT0001",
            Self::UnresolvedMethod { .. } => "GT0002",
            Self::UnresolvedOp { .. } => "GT0003",
            Self::NonExhaustiveMatch { .. } => "GT0004",
            Self::InvalidCast { .. } => "GT0005",
            Self::UnknownField { .. } => "GT0006",
        }
    }
}

/// Maps the most common `expected X, found Y` pairs to a one-line
/// "did you mean" hint. Pure string compare on the rendered types
/// — keeps the table small and avoids re-deriving structure here.
fn mismatch_suggestion(expected: &str, found: &str) -> Option<String> {
    // String / &str
    if expected == "String" && found.ends_with("&str") {
        return Some("did you mean to call `.to_string()` on the value?".to_string());
    }
    if expected.ends_with("&str") && found == "String" {
        return Some("did you mean to call `.as_str()` on the value?".to_string());
    }
    // Numeric width — i32 ↔ i64, u32 ↔ u64, etc.
    let int_suffixes = [
        "i8", "i16", "i32", "i64", "i128", "u8", "u16", "u32", "u64", "u128", "isize", "usize",
    ];
    if int_suffixes.contains(&expected) && int_suffixes.contains(&found) {
        return Some(format!("cast explicitly with `<expr> as {expected}`"));
    }
    // T → Option<T>
    if let Some(inner) = expected
        .strip_prefix("Option<")
        .and_then(|s| s.strip_suffix('>'))
    {
        if inner == found {
            return Some(format!(
                "did you mean to wrap with `Some(<expr>)` to lift `{inner}` into `Option<{inner}>`?"
            ));
        }
    }
    // Result<T, _> → T (handler returned a Result, caller wanted the inner value)
    if found.starts_with("Result<") && !expected.starts_with("Result<") {
        return Some(
            "did you mean to propagate with `?` (`<expr>?`) to unwrap the `Result`?".to_string(),
        );
    }
    // &T vs T
    if let Some(rest) = found.strip_prefix('&') {
        if rest == expected {
            return Some(format!(
                "did you mean to dereference with `*<expr>` to get `{expected}`?"
            ));
        }
    }
    if let Some(rest) = expected.strip_prefix('&') {
        if rest == found {
            return Some(format!(
                "did you mean to take a reference with `&<expr>` to get `&{found}`?"
            ));
        }
    }
    None
}

impl TypeDiagnostic {
    /// Renders this diagnostic as a structured
    /// [`gossamer_diagnostics::Diagnostic`] for the new error frame.
    #[must_use]
    pub fn to_diagnostic(&self) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Code, Diagnostic, Location};
        let location = Location::new(self.span.file, self.span);
        let title = format!("{}", self.error);
        let mut out =
            Diagnostic::error(Code(self.error.code()), title.clone()).with_primary(location, title);
        match &self.error {
            TypeError::TypeMismatch { expected, found } => {
                out = out.with_note(format!("expected `{expected}`, found `{found}`"));
                if let Some(suggestion) = mismatch_suggestion(expected, found) {
                    out = out.with_help(suggestion);
                }
            }
            TypeError::UnresolvedMethod { ty, name } => {
                out = out
                    .with_help(format!("`{ty}` has no method named `{name}`"))
                    .with_note("check for a typo or an impl block missing from scope");
            }
            TypeError::UnresolvedOp { op, lhs, rhs } => {
                out = out.with_note(format!(
                    "operator `{op}` requires matching operand types; got `{lhs}` and `{rhs}`"
                ));
            }
            TypeError::NonExhaustiveMatch { missing } => {
                out = out
                    .with_help(format!("add an arm for: {missing}"))
                    .with_note("match expressions must cover every possible value");
            }
            TypeError::InvalidCast { from, to } => {
                out = out
                    .with_help(
                        "`as` is restricted to numeric ↔ numeric, `bool`/`char` → integer, `u8` → `char`, and no-op same-type casts",
                    )
                    .with_note(format!("cannot cast `{from}` to `{to}`"));
            }
            TypeError::UnknownField { ty, field, opaque } => {
                if *opaque {
                    out = out.with_help(format!(
                        "`{ty}` has no named struct fields exposed to the language. \
                         Use the type's methods (e.g. `value.get(\"{field}\")` for \
                         `json::Value`) instead of named-field access."
                    ));
                } else {
                    out = out.with_help(format!(
                        "check the spelling of `.{field}` and that the struct \
                         definition for `{ty}` is in scope."
                    ));
                }
            }
        }
        out
    }
}
