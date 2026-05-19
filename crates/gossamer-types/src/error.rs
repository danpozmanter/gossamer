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
    /// A `Result<T, E>` expression was used as a statement without
    /// binding or propagating the value. SPEC §9: discarded Results
    /// are a compile error unless explicitly suppressed with `let _ =`.
    #[error("unused `Result` value — the `Err` variant may go unhandled")]
    DiscardedResult,
    /// An expression, type, or pattern nested past the type-checker's
    /// hard recursion limit. Emitted on adversarial input that
    /// survives parsing (rare, but the typechecker has its own
    /// guard to keep `cargo check`-style probes from crashing).
    #[error("expression nests beyond {limit} levels (consider rewriting with a helper)")]
    RecursionLimit {
        /// Recursion limit at which the error was raised.
        limit: u32,
    },
    /// An integer literal overflows the value range of its declared
    /// type-suffix (e.g. `300i8`, `99999999999999999999i64`). Treated
    /// as `TyKind::Error` so downstream typing does not cascade.
    #[error("integer literal `{literal}` does not fit in `{ty}`")]
    IntLiteralOverflow {
        /// Source spelling of the literal, including suffix.
        literal: String,
        /// Name of the suffix-derived target type.
        ty: String,
    },
    /// A string escape that the parser accepted but cannot be
    /// validly decoded (out-of-range `\u{...}`, surrogate code
    /// point, non-ASCII `\x..`). Surfaced from the AST→typechecker
    /// boundary so that downstream lowering sees no malformed
    /// strings.
    #[error("invalid escape `{escape}` in string literal: {reason}")]
    InvalidEscape {
        /// Verbatim text of the rejected escape sequence.
        escape: String,
        /// Human-readable reason the escape is invalid.
        reason: String,
    },
    /// A trait bound `<T: Bound>` on a generic parameter names a
    /// trait the resolver does not know about. Catches typos
    /// (`Hashabel` for `Hashable`) at function declaration before
    /// the call site stumbles into a runtime "no method" error.
    #[error("unknown trait `{name}` in bound for parameter `{param}`")]
    UnknownTraitBound {
        /// Generic parameter the bound was attached to.
        param: String,
        /// Trait name as written.
        name: String,
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
            Self::DiscardedResult => "discarded-result",
            Self::RecursionLimit { .. } => "recursion-limit",
            Self::IntLiteralOverflow { .. } => "int-literal-overflow",
            Self::InvalidEscape { .. } => "invalid-escape",
            Self::UnknownTraitBound { .. } => "unknown-trait-bound",
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
            Self::DiscardedResult => "GT0007",
            Self::RecursionLimit { .. } => "GT0008",
            Self::IntLiteralOverflow { .. } => "GT0009",
            Self::InvalidEscape { .. } => "GT0010",
            Self::UnknownTraitBound { .. } => "GT0011",
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
            TypeError::DiscardedResult => {
                out = out
                    .with_help(
                        "propagate the error with `?`, handle it with `match` / `if let`, \
                         or explicitly discard with `let _ = <expr>`",
                    )
                    .with_note("SPEC §9: every `Result` value must be handled");
            }
            TypeError::RecursionLimit { .. } => {
                out = out
                    .with_help("split the expression into smaller helpers")
                    .with_note(
                        "the typechecker bails out at a fixed depth to avoid a C-stack overflow",
                    );
            }
            TypeError::IntLiteralOverflow { literal, ty } => {
                out = out.with_help(format!("`{literal}` exceeds the range of `{ty}`"));
            }
            TypeError::InvalidEscape { escape, reason } => {
                out = out.with_help(format!("`{escape}` is not a valid escape: {reason}"));
            }
            TypeError::UnknownTraitBound { param, name } => {
                out = out
                    .with_help(format!(
                        "trait `{name}` is not declared anywhere; bound on `{param}` cannot be enforced",
                    ))
                    .with_note("check for a typo or import the trait into scope");
            }
        }
        out
    }
}
