//! `gos parse FILE` — pretty-prints the AST for the supplied source.

use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::paths::read_source;

/// Entry point for `gos parse FILE`.
pub(crate) fn run(file: &PathBuf) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !diags.is_empty() {
        for diag in &diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!("{} parse error(s)", diags.len()));
    }
    println!("{sf}");
    Ok(())
}
