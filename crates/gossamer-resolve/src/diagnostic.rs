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
    /// Closest name visible where the error was raised.
    ///
    /// Locals, parameters, and closure bindings exist only in the resolver's
    /// scope stack, so the candidate is captured at the point of failure;
    /// nothing downstream can reconstruct that scope.
    pub in_scope_candidate: Option<String>,
}

impl ResolveDiagnostic {
    /// Constructs a diagnostic pairing an error with its source span.
    #[must_use]
    pub const fn new(error: ResolveError, span: Span) -> Self {
        Self {
            error,
            span,
            in_scope_candidate: None,
        }
    }

    /// Attaches the closest name that was visible where the error was raised.
    #[must_use]
    pub fn with_candidate(mut self, candidate: Option<String>) -> Self {
        self.in_scope_candidate = candidate;
        self
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
    /// A `use` path that names no stdlib module or registered external item.
    #[error("module `{path}` does not exist")]
    UnknownModulePath {
        /// The path as written.
        path: String,
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
    /// A `use` path whose module exists but which names an item that
    /// module does not export.
    #[error("no `{name}` in `std::{module}`")]
    UnknownStdItem {
        /// Item name as written.
        name: String,
        /// `std::`-relative path of the module that was searched.
        module: String,
    },
    /// A `use` path naming a spelling that a canonical type replaced.
    #[error("`{path}` does not exist - use `{replacement}` instead")]
    RemovedStdItem {
        /// The path as written.
        path: String,
        /// The canonical name to import instead.
        replacement: String,
    },
    /// An item declared without `pub` named from outside the module
    /// that declares it.
    #[error("{kind} `{name}` is private to module `{module}`")]
    PrivateItem {
        /// Item name as declared.
        name: String,
        /// `::`-joined path of the module the item is private to.
        module: String,
        /// Item shape, for the message.
        kind: &'static str,
    },
    /// A bare enum-variant name that more than one enum declares.
    #[error("`{name}` is a variant of more than one enum")]
    AmbiguousVariant {
        /// Variant name as written.
        name: String,
        /// Enums that declare a variant of this name, in source order.
        enums: Vec<String>,
    },
    /// A bare name that resolves nowhere in scope but names an item some
    /// module in this unit declares.
    #[error("`{name}` is not in scope; module `{module}` declares it")]
    NotImported {
        /// Name as written.
        name: String,
        /// `::`-joined path of the module that declares it.
        module: String,
    },
    /// A `mod name;` declaration with no source behind it.
    #[error("no source file for module `{name}`")]
    MissingModuleSource {
        /// Module name as declared.
        name: String,
    },
}

impl ResolveError {
    /// Returns a short stable tag useful for snapshot tests.
    #[must_use]
    pub const fn tag(&self) -> &'static str {
        match self {
            Self::UnknownModulePath { .. } => "unknown-module-path",
            Self::UnresolvedName { .. } => "unresolved-name",
            Self::WrongNamespace { .. } => "wrong-namespace",
            Self::DuplicateItem { .. } => "duplicate-item",
            Self::DuplicateImport { .. } => "duplicate-import",
            Self::UnknownStdItem { .. } => "unknown-std-item",
            Self::RemovedStdItem { .. } => "removed-std-item",
            Self::PrivateItem { .. } => "private-item",
            Self::AmbiguousVariant { .. } => "ambiguous-variant",
            Self::NotImported { .. } => "not-imported",
            Self::MissingModuleSource { .. } => "missing-module-source",
        }
    }

    /// Whether this error is about a name the parser fabricated during
    /// recovery rather than one the user wrote.
    ///
    /// The parse error that produced the placeholder is the actionable
    /// report; repeating it as an unresolved name would point the user at
    /// a name that does not appear in their source.
    #[must_use]
    pub fn is_about_parse_placeholder(&self) -> bool {
        let reported = match self {
            Self::UnresolvedName { name }
            | Self::WrongNamespace { name, .. }
            | Self::DuplicateItem { name }
            | Self::AmbiguousVariant { name, .. }
            | Self::MissingModuleSource { name }
            | Self::DuplicateImport { name } => name,
            Self::UnknownModulePath { path } | Self::RemovedStdItem { path, .. } => path,
            Self::PrivateItem { name, module, .. } => {
                return name
                    .split("::")
                    .chain(module.split("::"))
                    .any(gossamer_ast::common::is_error_name);
            }
            Self::UnknownStdItem { name, module } | Self::NotImported { name, module } => {
                return name
                    .split("::")
                    .chain(module.split("::"))
                    .any(gossamer_ast::common::is_error_name);
            }
        };
        reported
            .split("::")
            .any(gossamer_ast::common::is_error_name)
    }

    /// Stable error code used by the diagnostics framework.
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnresolvedName { .. } => "GR0001",
            Self::WrongNamespace { .. } => "GR0002",
            Self::DuplicateItem { .. } => "GR0003",
            Self::DuplicateImport { .. } => "GR0004",
            Self::UnknownModulePath { .. } => "GR0005",
            Self::RemovedStdItem { .. } => "GR0006",
            Self::UnknownStdItem { .. } => "GR0007",
            Self::PrivateItem { .. } => "GR0008",
            Self::AmbiguousVariant { .. } => "GR0009",
            Self::MissingModuleSource { .. } => "GR0010",
            Self::NotImported { .. } => "GR0011",
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
        use gossamer_diagnostics::{Code, Diagnostic, Location, Suggestion, suggest};
        let location = Location::new(self.span.file, self.span);
        let title = format!("{}", self.error);
        let mut out =
            Diagnostic::error(Code(self.error.code()), title.clone()).with_primary(location, title);
        if let ResolveError::UnresolvedName { name } = &self.error {
            if let Some(replacement) = crate::stdlib_exports::canonical_collection_name(name) {
                return out.with_help(format!(
                    "`{replacement}` is the one spelling for this type; write `{replacement}` \
                     and import it with `use std::collections::{replacement}`"
                ));
            }
            let mut module_paths = crate::STDLIB_MODULE_PATHS.iter().filter(|path| {
                **path == name.as_str()
                    || path
                        .strip_suffix(name)
                        .is_some_and(|prefix| prefix.ends_with("::"))
            });
            if let (Some(path), None) = (module_paths.next(), module_paths.next()) {
                // The name is a real stdlib module, so the import is the fix.
                // A spelling candidate found below would be a different name
                // entirely, and applying it would rewrite a correct call onto
                // an unrelated type.
                return out.with_help(format!(
                    "standard library module `{name}` is not in scope; add `use std::{path}`"
                ));
            }
            if let Some(module) = crate::stdlib_exports::sole_stdlib_module_exporting(name) {
                // Exactly one standard library module exports this name, so
                // the import is unambiguous. A spelling candidate found below
                // would name a different item entirely.
                return out.with_help(format!(
                    "`{name}` is exported by `std::{module}`; add `use std::{module}::{name}`"
                ));
            }
            // The scope-derived candidate is checked first: it is the only
            // one that can name a local, a parameter, or a closure binding.
            if let Some(suggestion) = self
                .in_scope_candidate
                .as_deref()
                .or_else(|| suggest(name, in_scope.iter().copied(), 2))
                .or_else(|| suggest(name, crate::scope::prelude_suggestion_names(), 2))
            {
                out = out.with_suggestion(Suggestion::replacement(
                    location,
                    format!("did you mean `{suggestion}`?"),
                    suggestion.to_string(),
                ));
            }
        } else {
            out = self.with_error_specific_help(out);
        }
        out
    }

    /// Attaches the help line for every resolve error that is not an
    /// unresolved name, which carries its own did-you-mean search.
    fn with_error_specific_help(
        &self,
        out: gossamer_diagnostics::Diagnostic,
    ) -> gossamer_diagnostics::Diagnostic {
        use gossamer_diagnostics::{Location, Suggestion, suggest};
        let location = Location::new(self.span.file, self.span);
        match &self.error {
            ResolveError::WrongNamespace {
                name,
                expected,
                found,
            } => out.with_help(format!(
                "use a {expected} in this position; `{name}` resolves to a {found}"
            )),
            ResolveError::UnknownModulePath { path } => match closest_module_path(path) {
                Some(known) => out.with_suggestion(Suggestion::replacement(
                    location,
                    format!("did you mean `std::{known}`?"),
                    format!("std::{known}"),
                )),
                None => out.with_help(
                    "a standard library path is spelled in full, as in \
                         `use std::encoding::json`; `gos doc std` lists the modules"
                        .to_string(),
                ),
            },
            ResolveError::DuplicateItem { name } => out.with_help(format!(
                "rename or remove one `{name}` declaration in this module"
            )),
            ResolveError::DuplicateImport { name } => out.with_help(format!(
                "remove one import of `{name}`, or alias one with `as`"
            )),
            ResolveError::RemovedStdItem { replacement, .. } => out.with_help(format!(
                "`{replacement}` is the one spelling for this type; import it as \
                 `use std::collections::{replacement}`"
            )),
            ResolveError::UnknownStdItem { name, module } => {
                let exports = crate::stdlib_exports::stdlib_module_item_names(module);
                match suggest(name, exports.iter().copied(), 2) {
                    Some(known) => out.with_suggestion(Suggestion::replacement(
                        location,
                        format!("did you mean `std::{module}::{known}`?"),
                        format!("std::{module}::{known}"),
                    )),
                    None => out.with_help(format!(
                        "`gos doc std::{module}` lists what this module exports"
                    )),
                }
            }
            ResolveError::PrivateItem { name, module, kind } => out.with_help(format!(
                "`{name}` is declared without `pub`, so only `{module}` and its child \
                 modules can name it; write `pub` on the {kind} to reach it from here"
            )),
            ResolveError::NotImported { name, module } => out
                .with_suggestion(Suggestion::replacement(
                    location,
                    format!("did you mean `{module}::{name}`?"),
                    format!("{module}::{name}"),
                ))
                .with_help(format!(
                    "a module's items are reached through a path or an import; add \
                     `use {module}::{name}` to name it directly"
                )),
            ResolveError::MissingModuleSource { name } => out.with_help(format!(
                "`mod {name};` names a module whose source the build never supplied; \
                 add `{name}.gos` (or `{name}/mod.gos`) beside the entry inside a \
                 project, or write the body inline as `mod {name} {{ ... }}`"
            )),
            ResolveError::AmbiguousVariant { name, enums } => out.with_help(format!(
                "{} declare a variant named `{name}`; write the enum, as in `{}::{name}`",
                enums
                    .iter()
                    .map(|e| format!("`{e}`"))
                    .collect::<Vec<_>>()
                    .join(" and "),
                enums.first().map_or("Enum", String::as_str)
            )),
            ResolveError::UnresolvedName { .. } => out,
        }
    }
}

/// Closest real standard library module path to `path`.
///
/// Compares the leaf segment as well as the whole path, so the common
/// mistake of omitting an intermediate namespace (`std::json` for
/// `std::encoding::json`) resolves to the module the user meant.
fn closest_module_path(path: &str) -> Option<&'static str> {
    let written = path.strip_prefix("std::").unwrap_or(path);
    if let Some(exact) = crate::STDLIB_MODULE_PATHS
        .iter()
        .find(|known| known.rsplit("::").next() == Some(written))
    {
        return Some(exact);
    }
    gossamer_diagnostics::suggest(written, crate::STDLIB_MODULE_PATHS.iter().copied(), 2)
}
