//! The caller-side spellings every pass after the front end's resolve
//! phase should never see.
//!
//! A labelled argument, a defaulted parameter, and a std function named
//! in value position are all written for the reader's benefit and mean
//! something the checker, HIR, and each tier's codegen already handle in
//! one canonical shape. Rewriting them in one place is what keeps a
//! REPL line, a playground snippet, and a file on disk agreeing about
//! what a call means: every front end calls this, so none of them can
//! drift into accepting a spelling the others reject.

#![forbid(unsafe_code)]

use gossamer_ast::SourceFile;
use gossamer_resolve::{Resolutions, ResolveDiagnostic};

/// Rewrites `sf` in place into the canonical call spellings and returns
/// the diagnostics the named-argument rewrite produced.
pub fn normalize_caller_side_spellings(
    sf: &mut SourceFile,
    resolutions: &Resolutions,
) -> Vec<ResolveDiagnostic> {
    let diagnostics = gossamer_resolve::resolve_named_arguments(sf, resolutions);
    let _ = crate::std_fn_eta::expand_std_fn_values(sf, resolutions);
    diagnostics
}
