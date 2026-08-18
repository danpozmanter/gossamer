//! `gos check [PATH]` - parse + resolve + typecheck + exhaustiveness.
//!
//! Walks `<project-root>/src` when invoked with no path; honours a
//! single file or a directory when supplied. Renders every stage's
//! diagnostics through the shared renderer, surfaces a non-zero exit
//! when any stage produces error-severity output.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::loaders::print_timings;
use crate::paths::{
    collect_lint_targets, default_test_root, friendly_io_error, resolve_project_entry,
    stderr_supports_colour,
};

/// `gos check` dispatcher: routes between single-file and
/// whole-project walks.
pub(crate) fn dispatch(
    path: Option<PathBuf>,
    timings: bool,
    message_format: crate::cli::MessageFormat,
    fix: bool,
) -> Result<()> {
    if let Err(err) = crate::binding_dispatch::ensure_external_signatures() {
        eprintln!("warning: failed to load rust-binding signatures: {err}");
    }
    let resolved = match path {
        Some(p) => p,
        None => default_test_root()?,
    };
    let meta = fs::metadata(&resolved).map_err(|e| friendly_io_error(e, &resolved))?;
    if meta.is_file() {
        return run(&resolved, timings, message_format, fix);
    }
    // A project directory is checked as one bundled unit (the entry plus
    // its auto-bundled sibling / subdirectory modules), so cross-module
    // references resolve exactly as they do under `gos` / `gos
    // build`. Without this, each file is type-checked in isolation and a
    // valid `crate::other::item` call reports a false unresolved-name
    // error. A directory without a single resolvable entry falls back to
    // the per-file sweep below.
    if let Ok(entry) = resolve_project_entry(&resolved) {
        return run(&entry, timings, message_format, fix);
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
        match run(file, timings, message_format, fix) {
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
pub(crate) fn run(
    file: &Path,
    timings: bool,
    message_format: crate::cli::MessageFormat,
    fix: bool,
) -> Result<()> {
    let unit = crate::paths::read_entry_unit(file)?;
    let user_source = unit.source;
    // Augment with the synthesized serde free functions (`__gos_serde_*`)
    // so `to_json::<T>(..)` / `from_json::<T>(..)` resolve, exactly as
    // `gos` / `gos build` do before reaching the source map.
    let source = gossamer_parse::autoderive::augment_source(&user_source);
    // Comptime fold makes `gos check` authoritative for comptime: a
    // region that is not compile-time-known is reported here, not
    // deferred to `run` / `build`.
    let source = crate::comptime_fold::fold_comptime(source, &file.to_string_lossy())?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    crate::paths::register_unit_origins(&mut map, file_id, &unit.entry, &unit.origins);
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: stderr_supports_colour(),
    };
    // `check` runs the same authoritative front-end gate as `run` /
    // `build` / `test` / `bench`: one parse + resolve + typecheck +
    // exhaustiveness pass under a single fatal-error policy. Sharing the
    // gate is what keeps `check` from drifting looser than the tiers it
    // is meant to guard.
    let stage = std::time::Instant::now();
    let outcome = gossamer_driver::check_frontend(&source, file_id);
    let elapsed = stage.elapsed();
    for diag in outcome.diagnostics.iter().chain(&outcome.warnings) {
        emit_diag(diag, &map, render_opts, message_format);
    }
    if fix {
        let applied = apply_suggestions(file, &user_source, &source, &outcome.diagnostics)?;
        println!("fix: {applied} edit(s) applied to {}", file.display());
    }
    // The editor and `gos lint` both run the default lint registry, so
    // `check` runs it too and stays the superset gate. Lints are advisory
    // here: they report at warning severity and never move the exit code,
    // which `gos lint --deny-warnings` remains the gate for.
    for diag in lint_diagnostics(file, &mut map)? {
        emit_diag(&diag, &map, render_opts, message_format);
    }
    if !outcome.diagnostics.is_empty() {
        return Err(anyhow!(
            "check failed with {} diagnostic(s)",
            outcome.diagnostics.len()
        ));
    }
    println!("check: ok ({} items typed)", outcome.checked.sf.items.len());
    if timings {
        // The shared gate runs the stages back-to-back, so only the total
        // is meaningful here; the per-stage split is reported as a single
        // typeck bucket.
        print_timings(
            source.len(),
            std::time::Duration::ZERO,
            std::time::Duration::ZERO,
            elapsed,
            std::time::Duration::ZERO,
        );
    }
    Ok(())
}

/// Runs the default lint registry over `path`'s own text and returns
/// its findings at warning severity.
///
/// The findings are anchored to a second entry in `map` holding that
/// text alone: the checked source carries the project bundle, the
/// synthesized autoderive tail, and any comptime splices, and a lint
/// span is only meaningful against the text the author wrote. Sibling
/// modules are linted when `gos lint` walks them under their own paths.
fn lint_diagnostics(
    path: &Path,
    map: &mut gossamer_lex::SourceMap,
) -> Result<Vec<gossamer_diagnostics::Diagnostic>> {
    let source = crate::paths::read_source(path)?;
    let lint_file = map.add_file(path.to_string_lossy().into_owned(), source.clone());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, lint_file);
    if !parse_diags.is_empty() {
        return Ok(Vec::new());
    }
    let mut registry = gossamer_lint::Registry::with_defaults();
    for item in &sf.items {
        gossamer_lint::apply_attributes(&item.attrs, &mut registry);
    }
    let mut diagnostics = gossamer_lint::run(&sf, &source, &registry);
    for diag in &mut diagnostics {
        diag.severity = gossamer_diagnostics::Severity::Warning;
    }
    Ok(diagnostics)
}

/// Renders a structured diagnostic to stderr, branching on
/// `--message-format`. Plain mode uses the colour-aware text
/// frame; JSON mode writes the single-line JSON object directly
/// (the trailing newline is part of `render_json`'s output).
fn emit_diag(
    structured: &gossamer_diagnostics::Diagnostic,
    map: &gossamer_lex::SourceMap,
    render_opts: gossamer_diagnostics::RenderOptions,
    message_format: crate::cli::MessageFormat,
) {
    match message_format {
        crate::cli::MessageFormat::Plain => {
            eprintln!(
                "{}",
                gossamer_diagnostics::render(structured, map, render_opts)
            );
        }
        crate::cli::MessageFormat::Json => {
            // Single-line JSON envelope; `render_json` adds the
            // trailing `\n` so consumers can read line-delimited.
            eprint!("{}", gossamer_diagnostics::render_json(structured, map));
        }
    }
}

/// Applies the machine-applicable rewrites for `file` and returns how
/// many landed: first the suggestions the diagnostics carry, then the
/// lint fixes `gos lint --fix` would apply, each verified against a
/// re-check.
///
/// The two sources are applied in rounds rather than merged. A lint fix
/// is derived from what the source means, and a source that still holds
/// an unresolved name means something else: an undefined-variable error
/// makes its intended binding look unused, so a merged pass would rename
/// the binding and the use apart.
///
/// A suggestion also has to address the file on disk. The checked text is
/// the author's source plus the project bundle, the synthesized
/// autoderive tail, and any comptime splices, so the applicable region is
/// the longest common prefix of the two; a span reaching past it is left
/// for the author.
fn apply_suggestions(
    file: &Path,
    user_source: &str,
    checked_source: &str,
    diagnostics: &[gossamer_diagnostics::Diagnostic],
) -> Result<usize> {
    let safe_len = common_prefix_len(user_source, checked_source).min(user_source.len());
    let mut current = crate::paths::read_source(file)?;
    let original = current.clone();
    let mut applied = 0usize;

    let suggested: Vec<gossamer_lint::Fix> = diagnostics
        .iter()
        .flat_map(|diag| &diag.suggestions)
        .filter(|s| (s.location.span.end as usize) <= safe_len)
        .map(|s| gossamer_lint::Fix {
            span: s.location.span,
            replacement: s.replacement.clone(),
            lint_id: "diagnostic",
        })
        .collect();
    if !suggested.is_empty() {
        let candidate = gossamer_lint::apply_fixes(&current, &suggested);
        if candidate != current && recheck(file, &candidate) < diagnostics.len() {
            current = candidate;
            applied += suggested.len();
        }
    }

    let lint_fixes = lint_fixes_for(file, &current);
    if !lint_fixes.is_empty() {
        let candidate = gossamer_lint::apply_fixes(&current, &lint_fixes);
        let baseline = recheck(file, &current);
        if candidate != current && recheck(file, &candidate) <= baseline {
            current = candidate;
            applied += lint_fixes.len();
        }
    }

    if current == original {
        return Ok(0);
    }
    fs::write(file, current).map_err(|e| friendly_io_error(e, file))?;
    Ok(applied)
}

/// Auto-applicable lint edits for `source` read as `file`'s text, under
/// the registry its own attributes configure.
fn lint_fixes_for(file: &Path, source: &str) -> Vec<gossamer_lint::Fix> {
    let mut map = gossamer_lex::SourceMap::new();
    let id = map.add_file(file.to_string_lossy().into_owned(), source.to_string());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, id);
    if !parse_diags.is_empty() {
        return Vec::new();
    }
    let mut registry = gossamer_lint::Registry::with_defaults();
    for item in &sf.items {
        gossamer_lint::apply_attributes(&item.attrs, &mut registry);
    }
    gossamer_lint::fixes(&sf, &registry, source)
}

/// Error count the front-end gate reports for `candidate` as `file`'s text.
fn recheck(file: &Path, candidate: &str) -> usize {
    let augmented = gossamer_parse::autoderive::augment_source(candidate);
    let Ok(folded) = crate::comptime_fold::fold_comptime(augmented, &file.to_string_lossy()) else {
        return usize::MAX;
    };
    let mut map = gossamer_lex::SourceMap::new();
    let id = map.add_file(file.to_string_lossy().into_owned(), folded.clone());
    let outcome = gossamer_driver::check_frontend(&folded, id);
    outcome.diagnostics.len()
}

/// Byte length of the longest common prefix of `a` and `b`, rounded down
/// to a character boundary so a span slice stays valid UTF-8.
fn common_prefix_len(a: &str, b: &str) -> usize {
    let mut len = a
        .as_bytes()
        .iter()
        .zip(b.as_bytes())
        .take_while(|(x, y)| x == y)
        .count();
    while len > 0 && !a.is_char_boundary(len) {
        len -= 1;
    }
    len
}
