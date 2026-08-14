//! Source rewriters `gos fix` applies to bring a project forward.
//!
//! A migration is not a lint. A lint says something about the code the
//! author wrote and is reported whether or not they act on it; a
//! migration is a mechanical upgrade the toolchain owns, and running it
//! is the whole interaction. They share [`crate::Fix`] as the edit
//! representation and nothing else.
//!
//! Every rewriter here must be **deterministic** - the same input yields
//! the same edits - and **idempotent** - applying it to its own output
//! produces no further edits. `gos fix` re-runs the front end afterwards
//! and keeps the result only when the file still checks, so a rewriter
//! that breaks a program cannot land, but a rewriter that is not
//! idempotent would still churn a repository on every run.

use gossamer_ast::SourceFile;

use crate::Fix;

/// One named migration.
pub struct Rewriter {
    /// Stable identifier, used to select and to report.
    pub id: &'static str,
    /// One line describing what the rewrite does.
    pub summary: &'static str,
    /// Editions this rewriter prepares a project for. Empty means it
    /// applies to every edition.
    pub editions: &'static [&'static str],
    /// Collects the edits this rewriter would make.
    pub collect: fn(&SourceFile, &str, &mut Vec<Fix>),
}

/// Every registered migration, in application order.
pub const REWRITERS: &[Rewriter] = &[];

/// Looks up a rewriter by id.
#[must_use]
pub fn rewriter(id: &str) -> Option<&'static Rewriter> {
    REWRITERS.iter().find(|r| r.id == id)
}

/// Collects the edits every rewriter in `selected` would make.
#[must_use]
pub fn migrations(sf: &SourceFile, source: &str, selected: &[&Rewriter]) -> Vec<Fix> {
    let mut out = Vec::new();
    for rewriter in selected {
        (rewriter.collect)(sf, source, &mut out);
    }
    out
}
