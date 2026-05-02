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

/// Loads a parsed [`gossamer_ast::SourceFile`] from the on-disk
/// frontend cache when `cache_key` hits, otherwise re-parses
/// `source`. The optional `trace` flag turns on the
/// `cache: parse skipped` log line consumers grep for.
pub(crate) fn load_or_parse(
    source: &str,
    file_id: gossamer_lex::FileId,
    cache_key: &gossamer_driver::FrontendCacheKey,
    trace: bool,
) -> (
    gossamer_ast::SourceFile,
    Vec<gossamer_parse::ParseDiagnostic>,
) {
    if let Some(cached) = gossamer_driver::load_blob::<gossamer_ast::SourceFile>(cache_key) {
        if trace {
            eprintln!("cache: parse skipped for {}", cache_key.as_hex());
        }
        return (cached, Vec::new());
    }
    gossamer_parse::parse_source_file(source, file_id)
}

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

/// Best-effort enumeration of every top-level item name a source
/// file declares. Used to seed "did you mean ...?" suggestions in
/// the resolver diagnostic renderer.
pub(crate) fn collect_top_level_names(sf: &gossamer_ast::SourceFile) -> Vec<&str> {
    let mut out = Vec::new();
    for item in &sf.items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Struct(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Enum(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Trait(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::TypeAlias(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Const(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Static(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Mod(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Impl(_) | gossamer_ast::ItemKind::AttrItem(_) => {}
        }
    }
    out
}

/// Parses, resolves, type-checks, and exhaustiveness-checks
/// `source`. Returns the lowered HIR program on success. When any
/// stage produces error-severity diagnostics, prints them through
/// the shared renderer and returns `Err` — no subsequent execution
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
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, file_id);
    if !parse_diags.is_empty() {
        for diag in &parse_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to execute",
            parse_diags.len()
        ));
    }
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    let in_scope: Vec<&str> = collect_top_level_names(&sf);
    let unresolved: Vec<_> = resolve_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_resolve::ResolveError::UnresolvedName { .. }
                    | gossamer_resolve::ResolveError::DuplicateItem { .. }
            )
        })
        .collect();
    if !unresolved.is_empty() {
        for diag in &unresolved {
            let structured = diag.to_diagnostic(&in_scope);
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} resolve error(s); refusing to execute",
            unresolved.len()
        ));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !type_diags.is_empty() {
        for diag in &type_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} type error(s); refusing to execute",
            type_diags.len()
        ));
    }
    let exhaustive_diags = gossamer_types::check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    let nonexhaustive: Vec<_> = exhaustive_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_types::ExhaustivenessError::NonExhaustive { .. }
            )
        })
        .collect();
    if !nonexhaustive.is_empty() {
        for diag in nonexhaustive {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!("non-exhaustive match; refusing to execute"));
    }
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let cache_key = gossamer_driver::FrontendCacheKey::new(source, env!("CARGO_PKG_VERSION"));
    if std::env::var_os("GOSSAMER_CACHE_TRACE").is_some()
        && gossamer_driver::observe_hit(&cache_key)
    {
        eprintln!(
            "cache: frontend hit for {} (skip not wired yet)",
            cache_key.as_hex()
        );
    }
    gossamer_driver::mark_success(&cache_key);
    Ok((program, sf, tcx))
}
