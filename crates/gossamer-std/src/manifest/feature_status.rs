//! Lifecycle status registry - declares whether each documented
//! stdlib module and language feature is `Shipped`, `Experimental`,
//! `Planned`, or `Removed`.
//!
//! Single source of truth for the `gos feature-status` subcommand
//! and the "Status: ..." markers emitted into the per-module doc
//! pages. `Shipped` is the default; entries appear here only when
//! the status departs from "fully implemented across every tier
//! the manifest claims".
//!
//! Drift between this table and the rendered doc pages is gated
//! by `gos doc --emit-stdlib --check`.

#![forbid(unsafe_code)]

/// Lifecycle stage of a stdlib module or documented language feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Status {
    /// Implemented across every tier the manifest claims and ready
    /// for production use. Doc page + cross-tier parity test required.
    Shipped,
    /// Surface is wired but has known gaps (partial implementation,
    /// platform-specific, or pending audit). Doc page required;
    /// tier-parity coverage optional.
    Experimental,
    /// Documented in the registry so consumers can see what's on
    /// the roadmap. No doc page or test required yet.
    Planned,
    /// Previously shipped, since withdrawn. Kept in the registry so
    /// tooling can answer "where did `foo` go?" with a deliberate
    /// removal note.
    Removed,
}

impl Status {
    /// Returns the short lowercase tag printed in the table and
    /// embedded in doc pages (`"shipped"`, `"experimental"`, ...).
    #[must_use]
    pub const fn tag(self) -> &'static str {
        match self {
            Status::Shipped => "shipped",
            Status::Experimental => "experimental",
            Status::Planned => "planned",
            Status::Removed => "removed",
        }
    }

    /// Parses the inverse of [`Status::tag`]. Returns `None` for any
    /// unrecognised tag so `--status=foo` can surface a CLI error
    /// instead of silently matching nothing.
    #[must_use]
    pub fn parse(tag: &str) -> Option<Status> {
        match tag {
            "shipped" => Some(Status::Shipped),
            "experimental" => Some(Status::Experimental),
            "planned" => Some(Status::Planned),
            "removed" => Some(Status::Removed),
            _ => None,
        }
    }
}

/// One entry in the lifecycle registry - qualified path, status,
/// brief description.
#[derive(Debug, Clone, Copy)]
pub struct FeatureStatus {
    /// Canonical path. Stdlib modules use the `std::foo::bar`
    /// shape; language features use the `lang::if_let` shape so
    /// the two namespaces never collide.
    pub path: &'static str,
    /// Lifecycle stage.
    pub status: Status,
    /// One-line description surfaced in `gos feature-status`.
    pub doc: &'static str,
}

/// Entries that override the default `Shipped` status for stdlib
/// modules, plus every documented language feature. Items not
/// listed here default to `Shipped` (computed at runtime by walking
/// `manifest::ALL_MODULES`).
pub const FEATURE_STATUS: &[FeatureStatus] = &[
    // -----------------------------------------------------------------
    // Language features. All `lang::*` so the namespace never collides
    // with the `std::*` stdlib paths.
    // -----------------------------------------------------------------
    lang("lang::let", "Immutable binding."),
    lang("lang::let_mut", "Mutable binding."),
    lang("lang::if", "Conditional expression."),
    lang("lang::match", "Exhaustive pattern match expression."),
    lang("lang::if_let", "Single-variant pattern sugar."),
    lang(
        "lang::while_let",
        "Loop that drains while a pattern matches.",
    ),
    lang("lang::for", "Iterator-driven loop."),
    lang("lang::loop", "Unconditional loop with `break value`."),
    lang(
        "lang::break",
        "Exit the innermost loop, optionally with a value.",
    ),
    lang(
        "lang::continue",
        "Skip to the next iteration of the innermost loop.",
    ),
    lang("lang::return", "Exit the enclosing function with a value."),
    lang(
        "lang::question_mark",
        "Short-circuit Result / Option propagation operator.",
    ),
    lang("lang::pipe", "Forward-pipe operator `|>`."),
    lang("lang::closure", "Lambda expression `|args| body`."),
    lang("lang::fn", "Function declaration."),
    lang("lang::struct", "Product type declaration."),
    lang(
        "lang::enum",
        "Sum type declaration with payload-carrying variants.",
    ),
    lang("lang::trait", "Behaviour interface declaration."),
    lang("lang::impl", "Inherent and trait implementation blocks."),
    lang(
        "lang::generics",
        "Type parameters on functions / impls / structs.",
    ),
    lang("lang::go", "Goroutine spawn."),
    lang("lang::select", "Channel multiplex select expression."),
    lang("lang::channel", "Typed channel via `std::sync::channel`."),
    lang(
        "lang::spawn",
        "Goroutine join handle: `spawn(f)` -> `JoinHandle<T>`, `.join()` -> `Result<T, String>`.",
    ),
    lang(
        "lang::macros",
        "Six built-in format macros (print/println/eprint/eprintln/format/panic).",
    ),
    lang(
        "lang::doctest",
        "Fenced code in `//` doc comments runs under `gos test`.",
    ),
    lang("lang::cfg", "Conditional compilation attribute."),
    lang(
        "lang::attribute",
        "Built-in attributes (`#[cfg]`, `#[test]`, `#[bench]`, `#[derive]`).",
    ),
    lang("lang::const", "Compile-time constant binding."),
    lang(
        "lang::static",
        "Module-level mutable or immutable static slot.",
    ),
    lang(
        "lang::mut_ref_params",
        "`&mut Vec<T>` / `&mut [T]` parameters write through to the caller's storage on every tier.",
    ),
    // Identifier rules - Unicode XID_Start / XID_Continue (UAX #31).
    lang(
        "lang::unicode_identifiers",
        "Identifiers follow UAX #31 (matches Rust 2024).",
    ),
    // Experimental / partial language surface.
    FeatureStatus {
        path: "lang::move_keyword",
        status: Status::Planned,
        doc: "`move` closure capture keyword - parses, lowers to the same Fn shape as a non-move closure (GC handles ownership).",
    },
    FeatureStatus {
        path: "lang::async_await",
        status: Status::Planned,
        doc: "`async fn` / `.await` - goroutines + channels cover the same shape today.",
    },
    FeatureStatus {
        path: "lang::lifetimes",
        status: Status::Planned,
        doc: "Explicit lifetime annotations - not needed under the current GC model; tracked in case a borrow-checker mode lands.",
    },
    // -----------------------------------------------------------------
    // Stdlib status overrides. Only modules whose status departs from
    // the implicit `Shipped` default are listed here; everything else
    // is computed from `manifest::ALL_MODULES` at runtime.
    // -----------------------------------------------------------------
    FeatureStatus {
        path: "std::tls",
        status: Status::Experimental,
        doc: "TLS surface (rustls-backed) - handshake works; mTLS auth + custom verifiers still maturing.",
    },
    FeatureStatus {
        path: "std::database::sql",
        status: Status::Experimental,
        doc: "Driver-pluggable SQL access (Conn, Tx, Stmt, Rows, Pool, migrate, query::Select). Drivers register at startup via gossamer_runtime::sql::register; no driver ships in the box.",
    },
    FeatureStatus {
        path: "std::html::template",
        status: Status::Experimental,
        doc: "Context-aware HTML template engine - auto-escape works (text/attr/URL/JS), pipeline operator set still expanding. Heuristic classifier, NOT a content-security-policy substitute; the `html::escape` primitive (wired on every tier) is the supported cross-tier escape.",
    },
    FeatureStatus {
        path: "std::text::template",
        status: Status::Experimental,
        doc: "Plain-text template engine - feature parity with html::template tracked together.",
    },
    // -----------------------------------------------------------------
    // Sub-module stdlib feature entries. Not manifest modules (the
    // implicit-Shipped walk never synthesises them), so the 0.13.0
    // HTTP tier-parity surface is registered explicitly.
    // -----------------------------------------------------------------
    shipped(
        "std::http::client_request_native",
        "`http::request` / `http::request_bytes` native on the compiled tiers through one ureq engine.",
    ),
    shipped(
        "std::http::response_headers",
        "Client `Response.headers` (lowercase, wire order) plus honored server response headers with chainable `with_header`.",
    ),
    shipped(
        "std::http::redirect_policy",
        "`Client::builder().max_redirects(n).timeout_ms(ms).build()`; `max_redirects(0)` returns the raw 3xx.",
    ),
    shipped(
        "std::http::binary_bodies",
        "`Response.raw_bytes` / `Request.raw_body` packed byte bodies, NUL-safe on every tier.",
    ),
    shipped(
        "std::http::streaming_responses",
        "`Response::stream` chunked server streaming plus `ResponseStream::next_chunk` client byte reads.",
    ),
    shipped(
        "std::http::server_request_headers",
        "Inbound `Request.headers` populated on every tier; `path` strips the query string.",
    ),
    // -----------------------------------------------------------------
    // Tooling features. `tooling::*` mirrors the `lang::*` namespace
    // convention for surface that is neither language nor stdlib.
    // -----------------------------------------------------------------
    shipped(
        "tooling::faithful_fmt",
        "Token-stream `gos fmt`: comments and macros preserved verbatim, idempotent, no-destruction self-check.",
    ),
];

const fn lang(path: &'static str, doc: &'static str) -> FeatureStatus {
    shipped(path, doc)
}

const fn shipped(path: &'static str, doc: &'static str) -> FeatureStatus {
    FeatureStatus {
        path,
        status: Status::Shipped,
        doc,
    }
}

/// Returns the registered status for `path`, falling back to
/// `Shipped` when `path` is a stdlib module present in
/// `manifest::ALL_MODULES` and to `None` otherwise. Callers wanting
/// the synthesised default-`Shipped` view over the full stdlib +
/// language surface should iterate `all_entries` instead.
#[must_use]
pub fn lookup(path: &str) -> Option<FeatureStatus> {
    if let Some(entry) = FEATURE_STATUS.iter().find(|e| e.path == path) {
        return Some(*entry);
    }
    if let Some(module) = super::ALL_MODULES.iter().find(|m| m.path == path) {
        return Some(FeatureStatus {
            path: module.path,
            status: Status::Shipped,
            doc: module.summary,
        });
    }
    None
}

/// Returns every entry in the registry merged with the implicit
/// stdlib defaults. Stdlib modules that don't appear in
/// `FEATURE_STATUS` are synthesised as `Shipped`. Entries are
/// returned in a stable order: registry entries first (declaration
/// order), then the synthesised stdlib defaults (manifest order).
#[must_use]
pub fn all_entries() -> Vec<FeatureStatus> {
    let mut out: Vec<FeatureStatus> = FEATURE_STATUS.to_vec();
    for module in super::ALL_MODULES {
        if FEATURE_STATUS.iter().any(|e| e.path == module.path) {
            continue;
        }
        if out.iter().any(|e| e.path == module.path) {
            continue;
        }
        out.push(FeatureStatus {
            path: module.path,
            status: Status::Shipped,
            doc: module.summary,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_tag_round_trips() {
        for tag in ["shipped", "experimental", "planned", "removed"] {
            let parsed = Status::parse(tag).expect("known tag");
            assert_eq!(parsed.tag(), tag);
        }
    }

    #[test]
    fn lookup_returns_explicit_entry() {
        let entry = lookup("std::tls").expect("tls registered");
        assert_eq!(entry.status, Status::Experimental);
    }

    #[test]
    fn lookup_defaults_stdlib_modules_to_shipped() {
        let entry = lookup("std::fmt").expect("fmt in manifest");
        assert_eq!(entry.status, Status::Shipped);
    }

    #[test]
    fn lookup_returns_none_for_unknown() {
        assert!(lookup("std::does::not::exist").is_none());
    }

    #[test]
    fn all_entries_covers_every_stdlib_module() {
        let entries = all_entries();
        for module in super::super::ALL_MODULES {
            assert!(
                entries.iter().any(|e| e.path == module.path),
                "missing default-Shipped entry for {}",
                module.path,
            );
        }
    }

    #[test]
    fn language_features_present() {
        let entries = all_entries();
        for path in ["lang::if_let", "lang::pipe", "lang::go", "lang::select"] {
            assert!(
                entries.iter().any(|e| e.path == path),
                "missing language entry {path}",
            );
        }
    }
}
