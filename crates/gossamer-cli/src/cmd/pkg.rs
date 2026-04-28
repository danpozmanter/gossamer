//! Package-management subcommands: `add`, `remove`, `tidy`,
//! `fetch`, `vendor`. Each operates on the nearest enclosing
//! `project.toml` (or an explicit `--manifest PATH`).

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};

use crate::paths::friendly_io_error;

/// `gos add SPEC [--manifest PATH]` — declares a registry
/// dependency. `SPEC` is `<id>` or `<id>@<version>`.
pub(crate) fn add(spec: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let (id_text, version_text) = match spec.split_once('@') {
        Some((id, ver)) => (id, ver),
        None => (spec, "0.1.0"),
    };
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let version = gossamer_pkg::Version::parse(version_text)
        .with_context(|| format!("invalid version `{version_text}`"))?;
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let changed = gossamer_pkg::add_registry(&mut m, &id, version);
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "add: {action} {id} ({version})",
        action = if changed { "added" } else { "kept" }
    );
    Ok(())
}

/// `gos remove ID [--manifest PATH]` — drops the matching
/// dependency entry; errors when nothing matched.
pub(crate) fn remove(id_text: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let removed = gossamer_pkg::remove(&mut m, &id);
    if !removed {
        return Err(anyhow!("dependency {id} is not declared"));
    }
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("remove: dropped {id}");
    Ok(())
}

/// `gos tidy [--manifest PATH]` — re-renders the manifest so
/// whitespace + entry ordering match the canonical layout.
pub(crate) fn tidy(manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("tidy: canonicalised {}", path.display());
    Ok(())
}

/// `gos fetch [--manifest PATH] [--offline]` — populates the
/// download cache for every transitive dependency.
pub(crate) fn fetch(manifest: Option<PathBuf>, offline: bool) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions { offline });
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    println!("fetch: {} project(s) cached", fetched.len());
    for entry in &fetched {
        println!("  {} → {}", entry.resolved.id, entry.source.digest);
    }
    Ok(())
}

/// `gos vendor [--manifest PATH] [--out DIR]` — materialises every
/// transitive dependency into `<out>/` for an offline / reproducible
/// build.
pub(crate) fn vendor(manifest: Option<PathBuf>, out: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source = fs::read_to_string(&path).map_err(|e| friendly_io_error(e, &path))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions::default());
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    let dest = out.unwrap_or_else(|| PathBuf::from("vendor"));
    let written = gossamer_pkg::vendor(&fetched, &dest)
        .with_context(|| format!("writing vendor dir {}", dest.display()))?;
    let total: usize = written.values().map(Vec::len).sum();
    println!(
        "vendor: wrote {total} file(s) for {} project(s) to {}",
        written.len(),
        dest.display()
    );
    Ok(())
}
