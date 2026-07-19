//! Cache inspection and retention commands.

use anyhow::Result;
use gossamer_driver::cache_maintenance::{self, CachePolicy};

pub(crate) fn status(paths_only: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let entries = cache_maintenance::status(&cwd);
    if paths_only {
        for entry in entries {
            println!("{}\t{}", entry.class.name(), entry.path.display());
        }
        return Ok(());
    }
    let mut total = 0;
    for entry in entries {
        total += entry.bytes;
        println!(
            "{:<10} {:>12} bytes  {:>8} files  {}",
            entry.class.name(),
            entry.bytes,
            entry.files,
            entry.path.display()
        );
    }
    println!("total      {total:>12} bytes");
    Ok(())
}

pub(crate) fn prune(dry_run: bool) -> Result<()> {
    let cwd = std::env::current_dir()?;
    let policy = CachePolicy::default();
    let (bytes, files) = cache_maintenance::prune(&cwd, policy, dry_run)?;
    let verb = if dry_run {
        "would reclaim"
    } else {
        "reclaimed"
    };
    println!(
        "cache prune: {verb} {bytes} bytes from {files} files (cap={}, max-age={} days)",
        policy.max_bytes,
        policy.max_age.as_secs() / 86_400
    );
    Ok(())
}
