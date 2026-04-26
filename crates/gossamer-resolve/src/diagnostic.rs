//! Diagnostics emitted by the name resolver.

#![forbid(unsafe_code)]

use std::fmt;

use gossamer_lex::Span;
use thiserror::Error;

/// A single resolver diagnostic with its source span.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolveDiagnostic {
    /// The specific error that occurred.
    pub error: ResolveError,
    /// Where in the source the error was detected.
    pub span: Span,
}

impl ResolveDiagnostic {
    /// Constructs a diagnostic pairing an error with its source span.
    #[must_use]
    pub const fn new(error: ResolveError, span: Span) -> Self {
        Self { error, span }
    }
}

impl fmt::Display for ResolveDiagnostic {
    fn fmt(&self, out: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(out, "{}", self.error)
    }
}

/// Every failure mode the resolver can report.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// The first segment of a path could not be resolved to any name in
    /// the current scope.
    #[error("cannot find `{name}` in this scope")]
    UnresolvedName {
        /// Name that could not be resolved.
        name: String,
    },
    /// A resolved name exists, but in the wrong namespace for this usage.
    #[error("expected {expected} but `{name}` is a {found}")]
    WrongNamespace {
        /// Name that was looked up.
        name: String,
        /// Namespace the caller was searching.
        expected: &'static str,
        /// Namespace where the name actually lives.
        found: &'static str,
    },
    /// Two items in the same module share a name.
    #[error("the name `{name}` is defined multiple times in this module")]
    DuplicateItem {
        /// Conflicting name.
        name: String,
    },
    /// Two `use` declarations import the same final name.
    #[error("the name `{name}` is imported multiple times in this scope")]
    DuplicateImport {
        /// Conflicting name.
        name: String,
    },
}

impl ResolveError {
    /// Returns a short stable tag useful for snapshot tests.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::UnresolvedName { .. } => "unresolved-name",
            Self::WrongNamespace { .. } => "wrong-namespace",
            Self::DuplicateItem { .. } => "duplicate-item",
            Self::DuplicateImport { .. } => "duplicate-import",
        }
    }

    /// Stable error code used by the diagnostics framework.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnresolvedName { .. } => "GR0001",
            Self::WrongNamespace { .. } => "GR0002",
            Self::DuplicateItem { .. } => "GR0003",
            Self::DuplicateImport { .. } => "GR0004",
        }
    }
}

impl ResolveDiagnostic {
    /// Renders this diagnostic as a structured
    /// [`gossamer_diagnostics::Diagnostic`]. When `in_scope` is
    /// non-empty, an `UnresolvedName` diagnostic also carries a
    /// did-you-mean suggestion drawn from the provided names.
    #[must_use]
    pub fn to_diagnostic(&self, in_scope: &[&str]) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Code, Diagnostic, Location, suggest};
        let location = Location::new(self.span.file, self.span);
        let title = format!("{}", self.error);
        let mut out =
            Diagnostic::error(Code(self.error.code()), title.clone()).with_primary(location, title);
        if let ResolveError::UnresolvedName { name } = &self.error {
            if let Some(suggestion) = suggest(name, in_scope.iter().copied(), 2) {
                out = out.with_help(format!("did you mean `{suggestion}`?"));
            }
        }
        out
    }
}
