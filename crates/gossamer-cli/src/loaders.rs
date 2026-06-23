//! Shared "parse + check + lower" helpers reused across `gos check`,
//! `gos run`, `gos test`, `gos bench`, etc.
//!
//! These functions exist to give every subcommand the same
//! diagnostic rendering and the same "refuse to execute on
//! statically-invalid input" rule. The tradeoff: a small amount of
//! duplication versus piping the diagnostic stream through every
//! subcommand individually.

use anyhow::{Result, anyhow};

use crate::paths::stderr_supports_colour;

/// Pretty-prints frontend stage timings for `gos check --timings`.
pub(crate) fn print_timings(
    source_len: usize,
    parse: std::time::Duration,
    resolve: std::time::Duration,
    typeck: std::time::Duration,
    exhaust: std::time::Duration,
) {
    let total = parse + resolve + typeck + exhaust;
    println!(
        "timings: {source_len} bytes source; parse {:>6.2}ms, resolve {:>6.2}ms, typeck {:>6.2}ms, exhaust {:>6.2}ms, total {:>6.2}ms",
        parse.as_secs_f64() * 1000.0,
        resolve.as_secs_f64() * 1000.0,
        typeck.as_secs_f64() * 1000.0,
        exhaust.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0,
    );
}

/// Parses, resolves, type-checks, and exhaustiveness-checks
/// `source`. Returns the lowered HIR program on success. When any
/// stage produces error-severity diagnostics, prints them through
/// the shared renderer and returns `Err` - no subsequent execution
/// may happen. Used by every `gos` subcommand that runs user code
/// so the interpreter, native build, test runner, and bench runner
/// all reject the same static-invalid programs.
pub(crate) fn load_and_check(
    source: &str,
    file_id: gossamer_lex::FileId,
    map: &gossamer_lex::SourceMap,
) -> Result<(gossamer_hir::HirProgram, gossamer_types::TyCtxt)> {
    load_and_check_with_sf(source, file_id, map).map(|(program, _, tcx)| (program, tcx))
}

/// Same as [`load_and_check`] but also returns the parsed
/// [`gossamer_ast::SourceFile`] for callers (`gos bench`, `gos test`)
/// that need AST-level item walks on top of the lowered program.
pub(crate) fn load_and_check_with_sf(
    source: &str,
    file_id: gossamer_lex::FileId,
    map: &gossamer_lex::SourceMap,
) -> Result<(
    gossamer_hir::HirProgram,
    gossamer_ast::SourceFile,
    gossamer_types::TyCtxt,
)> {
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: stderr_supports_colour(),
    };
    // The single authoritative front-end gate: parse + resolve +
    // typecheck + exhaustiveness under one fatal policy, shared with
    // `gos check` / `gos build` so a program rejected by any one is
    // rejected by all. `check_frontend` synthesizes the implicit `fn
    // main` for an entry file's top-level statements, so `sf` carries it.
    let outcome = gossamer_driver::check_frontend(source, file_id);
    if !outcome.diagnostics.is_empty() {
        for diag in &outcome.diagnostics {
            eprintln!("{}", gossamer_diagnostics::render(diag, map, render_opts));
        }
        return Err(anyhow!(
            "{} front-end error(s); refusing to execute",
            outcome.diagnostics.len()
        ));
    }
    let gossamer_driver::CheckedFrontend {
        sf,
        resolutions,
        table,
        mut tcx,
    } = outcome.checked;
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);
    Ok((program, sf, tcx))
}
