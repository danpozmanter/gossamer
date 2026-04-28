//! `gos watch CMD PATH` — re-runs `gos CMD <file>` for every
//! `.gos` under `PATH` whenever any of them is modified.

use std::fs;
use std::path::PathBuf;

use anyhow::{Result, anyhow};

use crate::paths::collect_lint_targets;

/// Entry point for `gos watch`.
pub(crate) fn run(command: &str, path: &PathBuf, forward: &[String]) -> Result<()> {
    let targets = collect_lint_targets(path)?;
    if targets.is_empty() {
        return Err(anyhow!("no `.gos` files found under {}", path.display()));
    }
    eprintln!(
        "watch: running `gos {command} <file>` on change under {} ({} files)",
        path.display(),
        targets.len()
    );
    let mut signatures = snapshot_mtimes(&targets);
    run_watch_command(command, &targets, forward);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let next = snapshot_mtimes(&targets);
        if next != signatures {
            eprintln!("watch: change detected; re-running");
            run_watch_command(command, &targets, forward);
            signatures = next;
        }
    }
}

fn snapshot_mtimes(files: &[PathBuf]) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    files
        .iter()
        .map(|path| {
            let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            (path.clone(), mtime)
        })
        .collect()
}

fn run_watch_command(command: &str, targets: &[PathBuf], forward: &[String]) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("gos"));
    for target in targets {
        let mut child = std::process::Command::new(&exe);
        child.arg(command).arg(target);
        for arg in forward {
            child.arg(arg);
        }
        let status = child.status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("watch: {} exited with {s}", target.display()),
            Err(err) => eprintln!("watch: spawn failed: {err}"),
        }
    }
}
