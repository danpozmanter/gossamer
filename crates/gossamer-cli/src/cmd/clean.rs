//! `gos clean [--vendor] [--dry-run]` - drop build artifacts and caches:
//! the project `target/` directory, the per-project `.gos-cache`
//! incremental IR-object cache, the frontend cache, and optionally the
//! vendor tree.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Entry point for `gos clean`.
pub(crate) struct Options {
    pub(crate) vendor: bool,
    pub(crate) dry_run: bool,
    pub(crate) classes: Vec<gossamer_driver::cache_maintenance::CacheClass>,
}

pub(crate) fn run(options: Options) -> Result<()> {
    let Options {
        vendor,
        dry_run,
        mut classes,
    } = options;
    let mut removed_bytes: u64 = 0;
    let mut removed_files: u32 = 0;

    // Project-local build artifacts are always disposable.
    // anchored at the current directory: `gos build` writes the binary
    // under `target/{debug,release}` and caches per-body objects in
    // `.gos-cache/ir-cache`.
    let cwd = std::env::current_dir()?;
    let (dir, label) = (cwd.join("target"), "build artifacts (target/)");
    remove_dir(&dir, label, dry_run, &mut removed_bytes, &mut removed_files)?;
    if classes.is_empty() {
        classes.extend([
            gossamer_driver::cache_maintenance::CacheClass::Frontend,
            gossamer_driver::cache_maintenance::CacheClass::Ir,
        ]);
    }
    for entry in gossamer_driver::cache_maintenance::remove(&cwd, &classes, dry_run)? {
        let verb = if dry_run { "would remove" } else { "removed" };
        println!(
            "{verb} {} cache at {} ({} bytes)",
            entry.class.name(),
            entry.path.display(),
            entry.bytes
        );
        removed_bytes += entry.bytes;
        removed_files += 1;
    }

    if vendor {
        remove_dir(
            &cwd.join("vendor"),
            "vendor tree",
            dry_run,
            &mut removed_bytes,
            &mut removed_files,
        )?;
    }

    let verb = if dry_run { "would remove" } else { "removed" };
    println!("clean: {verb} {removed_files} target(s), {removed_bytes} bytes total");
    Ok(())
}

/// Removes `dir` (recursively) if present, accumulating the byte/entry
/// tally. A `dry_run` only reports. Absent targets print a note and are
/// skipped - `gos clean` is idempotent.
fn remove_dir(
    dir: &Path,
    label: &str,
    dry_run: bool,
    removed_bytes: &mut u64,
    removed_files: &mut u32,
) -> Result<()> {
    if !dir.is_dir() {
        println!("{label} absent at {}", dir.display());
        return Ok(());
    }
    let bytes = dir_size(dir);
    if dry_run {
        println!("would remove {label} at {} ({bytes} bytes)", dir.display());
    } else {
        fs::remove_dir_all(dir).with_context(|| format!("remove {}", dir.display()))?;
        println!("removed {label} at {} ({bytes} bytes)", dir.display());
    }
    *removed_bytes += bytes;
    *removed_files += 1;
    Ok(())
}

/// Sums every regular file's byte length under `root`. Broken
/// symlinks and per-entry I/O errors are treated as 0 bytes - the
/// tally is advisory, never required for correctness.
fn dir_size(root: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}
