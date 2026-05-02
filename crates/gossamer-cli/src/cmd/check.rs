//! `gos check [PATH]` — parse + resolve + typecheck + exhaustiveness.
//!
//! Walks `<project-root>/src` when invoked with no path; honours a
//! single file or a directory when supplied. Renders every stage's
//! diagnostics through the shared renderer, surfaces a non-zero exit
//! when any stage produces error-severity output.

use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::loaders::{collect_top_level_names, load_or_parse, print_timings};
use crate::paths::{
    collect_lint_targets, default_test_root, friendly_io_error, read_source, stderr_supports_colour,
};

/// `gos check` dispatcher: routes between single-file and
/// whole-project walks.
pub(crate) fn dispatch(path: Option<PathBuf>, timings: bool) -> Result<()> {
    if let Err(err) = crate::binding_dispatch::ensure_external_signatures() {
        eprintln!("warning: failed to load rust-binding signatures: {err}");
    }
    let resolved = match path {
        Some(p) => p,
        None => default_test_root()?,
    };
    let meta = fs::metadata(&resolved).map_err(|e| friendly_io_error(e, &resolved))?;
    if meta.is_file() {
        return run(&resolved, timings);
    }
    let files = collect_lint_targets(&resolved)?;
    if files.is_empty() {
        return Err(anyhow!(
            "no `.gos` sources found under {}",
            resolved.display()
        ));
    }
    let mut total_errors = 0u32;
    for file in &files {
        if files.len() > 1 {
            println!("=== {} ===", file.display());
        }
        match run(file, timings) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("{err}");
                total_errors += 1;
            }
        }
    }
    if total_errors > 0 {
        return Err(anyhow!(
            "check: {total_errors} {file_word} failed across {} source(s)",
            files.len(),
            file_word = if total_errors == 1 { "file" } else { "files" },
        ));
    }
    println!(
        "check: {n} {file_word} ok",
        n = files.len(),
        file_word = if files.len() == 1 { "file" } else { "files" },
    );
    Ok(())
}

/// Single-file `gos check`. Public to the crate so the dispatcher
/// above and the `cmd::watch` re-runner can share it.
pub(crate) fn run(file: &PathBuf, timings: bool) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let cache_key = gossamer_driver::FrontendCacheKey::new(&source, env!("CARGO_PKG_VERSION"));
    let trace = std::env::var_os("GOSSAMER_CACHE_TRACE").is_some();
    let stage_parse = std::time::Instant::now();
    let (sf, parse_diags) = load_or_parse(&source, file_id, &cache_key, trace);
    let parse_elapsed = stage_parse.elapsed();
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: stderr_supports_colour(),
    };
    let mut total_errors = parse_diags.len();
    for diag in &parse_diags {
        let structured = diag.to_diagnostic();
        eprintln!(
            "{}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let stage_resolve = std::time::Instant::now();
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    let resolve_elapsed = stage_resolve.elapsed();
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
    total_errors += unresolved.len();
    let in_scope: Vec<&str> = collect_top_level_names(&sf);
    for diag in unresolved {
        let structured = diag.to_diagnostic(&in_scope);
        eprintln!(
            "{}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let stage_typeck = std::time::Instant::now();
    let (table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    let typeck_elapsed = stage_typeck.elapsed();
    total_errors += type_diags.len();
    for diag in &type_diags {
        let structured = diag.to_diagnostic();
        eprintln!(
            "{}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let stage_exhaust = std::time::Instant::now();
    let exhaustive_diags = gossamer_types::check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    let exhaust_elapsed = stage_exhaust.elapsed();
    let nonexhaustive: Vec<_> = exhaustive_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_types::ExhaustivenessError::NonExhaustive { .. }
            )
        })
        .collect();
    total_errors += nonexhaustive.len();
    for diag in nonexhaustive {
        let structured = diag.to_diagnostic();
        eprintln!(
            "{}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    if total_errors > 0 {
        return Err(anyhow!("check failed with {total_errors} diagnostic(s)"));
    }
    if trace && gossamer_driver::observe_hit(&cache_key) {
        eprintln!(
            "cache: frontend hit for {} (parse skipped)",
            cache_key.as_hex()
        );
    }
    gossamer_driver::mark_success(&cache_key);
    gossamer_driver::store_blob(&cache_key, &sf);
    println!("check: ok ({} items typed)", sf.items.len());
    if timings {
        print_timings(
            source.len(),
            parse_elapsed,
            resolve_elapsed,
            typeck_elapsed,
            exhaust_elapsed,
        );
    }
    Ok(())
}
