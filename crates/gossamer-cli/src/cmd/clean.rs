//! `gos clean [--vendor] [--dry-run]` — drop the frontend cache and
//! optionally the vendor tree.

use std::fs;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// Entry point for `gos clean`.
pub(crate) fn run(vendor: bool, dry_run: bool) -> Result<()> {
    let mut removed_bytes: u64 = 0;
    let mut removed_files: u32 = 0;
    let cache = gossamer_driver::cache_dir();
    if cache.is_dir() {
        let bytes = dir_size(&cache);
        if dry_run {
            println!(
                "would remove frontend cache at {} ({bytes} bytes)",
                cache.display()
            );
        } else {
            fs::remove_dir_all(&cache).with_context(|| format!("remove {}", cache.display()))?;
            println!(
                "removed frontend cache at {} ({bytes} bytes)",
                cache.display()
            );
        }
        removed_bytes += bytes;
        removed_files += 1;
    } else {
        println!("frontend cache absent at {}", cache.display());
    }
    if vendor {
        let vendor_dir = std::env::current_dir()?.join("vendor");
        if vendor_dir.is_dir() {
            let bytes = dir_size(&vendor_dir);
            if dry_run {
                println!(
                    "would remove vendor tree at {} ({bytes} bytes)",
                    vendor_dir.display()
                );
            } else {
                fs::remove_dir_all(&vendor_dir)
                    .with_context(|| format!("remove {}", vendor_dir.display()))?;
                println!(
                    "removed vendor tree at {} ({bytes} bytes)",
                    vendor_dir.display()
                );
            }
            removed_bytes += bytes;
            removed_files += 1;
        } else {
            println!("vendor tree absent at {}", vendor_dir.display());
        }
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    println!("clean: {verb} {removed_files} entr(y|ies), {removed_bytes} bytes total");
    Ok(())
}

/// Sums every regular file's byte length under `root`. Broken
/// symlinks and per-entry I/O errors are treated as 0 bytes — the
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
