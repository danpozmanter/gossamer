//! `gos fmt [PATH] [--check]` — re-renders source files through
//! the AST formatter. With no path, walks every `.gos` under the
//! project root.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::paths::{collect_lint_targets, default_test_root, friendly_io_error, read_source};

/// `gos fmt` dispatcher: routes between single-file and
/// whole-tree walks.
pub(crate) fn dispatch(path: Option<PathBuf>, check_only: bool) -> Result<()> {
    let resolved = match path {
        Some(p) => p,
        None => default_test_root()?,
    };
    let meta = fs::metadata(&resolved).map_err(|e| friendly_io_error(e, &resolved))?;
    if meta.is_file() {
        return run(&resolved, check_only);
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
        match run(file, check_only) {
            Ok(()) => {}
            Err(err) => {
                eprintln!("{err}");
                total_errors += 1;
            }
        }
    }
    if total_errors > 0 {
        return Err(anyhow!(
            "fmt: {total_errors} file(s) failed across {} source(s)",
            files.len()
        ));
    }
    Ok(())
}

fn run(file: &PathBuf, check_only: bool) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !diags.is_empty() {
        let render_opts = gossamer_diagnostics::RenderOptions {
            colour: crate::paths::stderr_supports_colour(),
        };
        for diag in &diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "{}",
                gossamer_diagnostics::render(&structured, &map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to format",
            diags.len()
        ));
    }
    let formatted = format!("{sf}");
    let formatted = if formatted.ends_with('\n') {
        formatted
    } else {
        formatted + "\n"
    };
    if check_only {
        if formatted == source {
            println!("fmt: {} already formatted", file.display());
            return Ok(());
        }
        return Err(anyhow!("{} is not formatted", file.display()));
    }
    if formatted == source {
        println!("fmt: {} unchanged", file.display());
    } else {
        fs::write(file, &formatted).with_context(|| format!("writing {}", file.display()))?;
        println!("fmt: rewrote {}", file.display());
    }
    Ok(())
}
