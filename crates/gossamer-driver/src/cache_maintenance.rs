//! Shared inspection, cleanup, and bounded-retention support for toolchain
//! caches. Cache contents are disposable, so failed accounting never makes a
//! build fail.

#![forbid(unsafe_code)]

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Independently manageable cache classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CacheClass {
    /// Parsed frontend source blobs.
    Frontend,
    /// LLVM incremental object files.
    Ir,
    /// Rust-binding runner and staticlib Cargo builds.
    Runners,
    /// Downloaded package source trees.
    Packages,
    /// Legacy build-graph artifacts.
    Build,
}

impl CacheClass {
    /// Stable command-line name.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Frontend => "frontend",
            Self::Ir => "ir",
            Self::Runners => "runners",
            Self::Packages => "packages",
            Self::Build => "build",
        }
    }

    /// Every known cache class.
    #[must_use]
    pub const fn all() -> &'static [Self] {
        &[
            Self::Frontend,
            Self::Ir,
            Self::Runners,
            Self::Packages,
            Self::Build,
        ]
    }

    /// Parses a command-line name.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        Self::all()
            .iter()
            .copied()
            .find(|class| class.name() == value)
    }
}

/// One cache root with its current accounting.
#[derive(Debug, Clone)]
pub struct CacheEntry {
    /// Owning cache class.
    pub class: CacheClass,
    /// Absolute or project-relative cache root.
    pub path: PathBuf,
    /// Aggregate regular-file bytes.
    pub bytes: u64,
    /// Aggregate regular-file count.
    pub files: u64,
}

/// A retention policy. Environment variables intentionally make it easy for
/// CI and constrained developer machines to tighten the defaults.
#[derive(Debug, Clone, Copy)]
pub struct CachePolicy {
    /// Aggregate cache capacity across all discovered roots.
    pub max_bytes: u64,
    /// Age after which an entry is eligible for deletion.
    pub max_age: Duration,
}

impl Default for CachePolicy {
    fn default() -> Self {
        let max_bytes = env_u64("GOS_CACHE_MAX_BYTES").unwrap_or(20 * 1024 * 1024 * 1024);
        let days = env_u64("GOS_CACHE_MAX_AGE_DAYS").unwrap_or(30);
        Self {
            max_bytes,
            max_age: Duration::from_secs(days.saturating_mul(86_400)),
        }
    }
}

impl CachePolicy {
    /// Per-class cap. An explicit `GOS_CACHE_<CLASS>_MAX_BYTES` value wins.
    #[must_use]
    pub fn class_max_bytes(self, class: CacheClass) -> u64 {
        let default = match class {
            CacheClass::Runners => 10 * 1024 * 1024 * 1024,
            CacheClass::Ir => 5 * 1024 * 1024 * 1024,
            CacheClass::Frontend => 1024 * 1024 * 1024,
            CacheClass::Packages | CacheClass::Build => 2 * 1024 * 1024 * 1024,
        };
        let name = format!("GOS_CACHE_{}_MAX_BYTES", class.name().to_ascii_uppercase());
        env_u64(&name).unwrap_or(default)
    }
}

fn env_u64(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.parse().ok()
}

/// Resolves all user and project cache roots. It mirrors the existing cache
/// producers rather than inventing another root.
#[must_use]
pub fn paths(cwd: &Path) -> Vec<(CacheClass, PathBuf)> {
    let frontend = crate::frontend_cache::cache_dir();
    let shared = frontend
        .parent()
        .map_or_else(|| frontend.clone(), Path::to_path_buf);
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let binding_root = if let Some(root) = std::env::var_os("GOSSAMER_CACHE") {
        PathBuf::from(root).join("gossamer")
    } else {
        shared.clone()
    };
    let mut out = vec![
        (CacheClass::Frontend, frontend),
        (CacheClass::Ir, shared.join("ir-cache")),
        (CacheClass::Runners, binding_root.join("runners")),
        (CacheClass::Ir, cwd.join(".gos-cache").join("ir-cache")),
    ];
    if let Some(home) = home {
        out.push((CacheClass::Packages, home.join(".gossamer").join("cache")));
        out.push((CacheClass::Build, home.join(".gossamer").join("build")));
    }
    out
}

/// Reports every known cache root. Missing roots are represented as zeroes.
#[must_use]
pub fn status(cwd: &Path) -> Vec<CacheEntry> {
    paths(cwd)
        .into_iter()
        .map(|(class, path)| {
            let (bytes, files) = dir_size(&path);
            CacheEntry {
                class,
                path,
                bytes,
                files,
            }
        })
        .collect()
}

/// Removes selected cache classes, returning the paths actually removed.
pub fn remove(
    cwd: &Path,
    classes: &[CacheClass],
    dry_run: bool,
) -> std::io::Result<Vec<CacheEntry>> {
    let mut removed = Vec::new();
    for (class, path) in paths(cwd) {
        if !classes.contains(&class) || !path.is_dir() {
            continue;
        }
        let (bytes, files) = dir_size(&path);
        if !dry_run {
            fs::remove_dir_all(&path)?;
        }
        removed.push(CacheEntry {
            class,
            path,
            bytes,
            files,
        });
    }
    Ok(removed)
}

/// Prunes expired files first, then oldest files until the aggregate budget is
/// met. Runner directories with a build lock are skipped so an active build is
/// never disrupted. Returns reclaimed bytes and files.
pub fn prune(cwd: &Path, policy: CachePolicy, dry_run: bool) -> std::io::Result<(u64, u64)> {
    let now = SystemTime::now();
    let mut files = Vec::new();
    for (class, root) in paths(cwd) {
        let mut root_files = Vec::new();
        collect_files(&root, &mut root_files);
        files.extend(root_files.into_iter().map(|entry| (class, entry)));
    }
    files.sort_by_key(|(_, entry)| entry.modified);
    let mut total: u64 = files.iter().map(|(_, entry)| entry.bytes).sum();
    let mut class_totals: HashMap<CacheClass, u64> = HashMap::new();
    for (class, entry) in &files {
        *class_totals.entry(*class).or_default() += entry.bytes;
    }
    let mut reclaimed = 0;
    let mut count = 0;
    for (class, entry) in files {
        let expired = now
            .duration_since(entry.modified)
            .is_ok_and(|age| age > policy.max_age);
        let class_over =
            class_totals.get(&class).copied().unwrap_or_default() > policy.class_max_bytes(class);
        if !expired && total <= policy.max_bytes && !class_over {
            continue;
        }
        if runner_locked(&entry.path) {
            continue;
        }
        if !dry_run {
            let _ = fs::remove_file(&entry.path);
            cleanup_empty_parents(&entry.path);
        }
        total = total.saturating_sub(entry.bytes);
        let class_total = class_totals.entry(class).or_default();
        *class_total = class_total.saturating_sub(entry.bytes);
        reclaimed += entry.bytes;
        count += 1;
    }
    Ok((reclaimed, count))
}

#[derive(Debug)]
struct FileEntry {
    path: PathBuf,
    bytes: u64,
    modified: SystemTime,
}

fn collect_files(root: &Path, out: &mut Vec<FileEntry>) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            collect_files(&path, out);
        } else if meta.is_file() {
            out.push(FileEntry {
                path,
                bytes: meta.len(),
                modified: meta.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
}

fn dir_size(root: &Path) -> (u64, u64) {
    let mut entries = Vec::new();
    collect_files(root, &mut entries);
    (
        entries.iter().map(|entry| entry.bytes).sum(),
        entries.len() as u64,
    )
}

fn runner_locked(path: &Path) -> bool {
    path.ancestors()
        .any(|ancestor| ancestor.join(".gos-build.lock").is_file())
}

fn cleanup_empty_parents(path: &Path) {
    for parent in path.ancestors().skip(1) {
        if fs::remove_dir(parent).is_err() {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "gossamer-cache-maintenance-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn walker_counts_regular_files_without_following_symlinks() {
        let root = scratch("size");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("nested")).unwrap();
        fs::write(root.join("a"), b"abc").unwrap();
        fs::write(root.join("nested").join("b"), b"12345").unwrap();
        assert_eq!(dir_size(&root), (8, 2));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn runner_lock_marks_descendant_files_ineligible() {
        let root = scratch("lock");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(root.join("runner").join("target")).unwrap();
        fs::write(root.join("runner").join(".gos-build.lock"), b"lock").unwrap();
        let artifact = root.join("runner").join("target").join("artifact");
        fs::write(&artifact, b"x").unwrap();
        assert!(runner_locked(&artifact));
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn default_policy_has_a_smaller_frontend_cap_than_runner_cap() {
        let policy = CachePolicy::default();
        assert!(
            policy.class_max_bytes(CacheClass::Frontend)
                < policy.class_max_bytes(CacheClass::Runners)
        );
    }
}
