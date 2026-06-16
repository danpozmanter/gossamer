//! Content-addressable cache backing the fetcher.
//!
//! Every cached source tree lives at
//! `~/.gossamer/cache/pkg/<sha256>/source/`. The cache key is the
//! sha256 of the canonical `path\0bytes\0...` serialisation of the
//! file map. The on-disk layout pairs each digest directory with an
//! `id.txt` sidecar so the runtime can re-hash on read and reject
//! silent corruption.
//!
//! The in-memory [`Cache`] is a transient mirror of the disk
//! layer; tests drive it directly when no `GOS_CACHE_DIR` is set.
//! Production builds construct a [`Cache`] anchored on the resolved
//! cache root via [`Cache::with_disk_root`] so a second fetch hits
//! disk and never touches the network.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::id::ProjectId;
use crate::resolver::Resolved;
use crate::sha256;

/// One cached source tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CachedSource {
    /// Project the source belongs to.
    pub id: ProjectId,
    /// Mapping of file path to file contents.
    pub files: BTreeMap<String, Vec<u8>>,
    /// SHA-256 of the canonical serialisation of `files`.
    pub digest: String,
}

impl CachedSource {
    /// Builds a cached source from a directory-shaped file map.
    /// The digest is a SHA-256 of the concatenation
    /// `path\0bytes\0path\0bytes\0...` in path-sorted order, so equal
    /// inputs produce equal digests across runs and platforms.
    #[must_use]
    pub fn build(id: ProjectId, files: BTreeMap<String, Vec<u8>>) -> Self {
        let digest = compute_digest(&files);
        Self { id, files, digest }
    }
}

fn compute_digest(files: &BTreeMap<String, Vec<u8>>) -> String {
    let mut buf = Vec::new();
    for (path, bytes) in files {
        buf.extend_from_slice(path.as_bytes());
        buf.push(0);
        buf.extend_from_slice(bytes);
        buf.push(0);
    }
    sha256::hex(&buf)
}

/// Returns the cache directory, honouring `$GOS_CACHE_DIR` first,
/// then `$HOME/.gossamer/cache`. Returns `None` when neither is
/// available (e.g. sandboxed CI without a `HOME`).
#[must_use]
pub fn default_cache_root() -> Option<PathBuf> {
    if let Ok(dir) = std::env::var("GOS_CACHE_DIR") {
        return Some(PathBuf::from(dir));
    }
    if let Ok(home) = std::env::var("HOME") {
        return Some(PathBuf::from(home).join(".gossamer").join("cache"));
    }
    None
}

/// In-memory cache used by tests and as the on-disk cache's
/// transient layer.
#[derive(Debug, Default)]
pub struct Cache {
    entries: BTreeMap<String, CachedSource>,
    disk_root: Option<PathBuf>,
}

impl Cache {
    /// Returns an empty cache with no disk-backed layer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns a cache anchored on `root`. Disk reads/writes go under
    /// `root/pkg/<digest>/source/`.
    #[must_use]
    pub fn with_disk_root(root: PathBuf) -> Self {
        Self {
            entries: BTreeMap::new(),
            disk_root: Some(root),
        }
    }

    /// Returns the configured disk root, if any.
    #[must_use]
    pub fn disk_root(&self) -> Option<&Path> {
        self.disk_root.as_deref()
    }

    /// Stores `source` keyed on its digest. Returns `true` when the
    /// entry was new. When a disk root is set, the source is also
    /// written under `root/pkg/<digest>/source/`.
    pub fn insert(&mut self, source: CachedSource) -> bool {
        if let Some(root) = self.disk_root.clone()
            && let Err(e) = write_to_disk(&root, &source)
        {
            eprintln!(
                "cache: warning: failed to persist {} to {}: {e}",
                source.digest,
                root.display()
            );
        }
        let key = source.digest.clone();
        self.entries.insert(key, source).is_none()
    }

    /// Looks up a cached entry by digest. Consults the in-memory
    /// map first; on miss with a disk root configured, attempts to
    /// load the digest from disk (with re-hash verification).
    pub fn get(&mut self, digest: &str) -> Option<&CachedSource> {
        if !self.entries.contains_key(digest)
            && let Some(root) = self.disk_root.clone()
            && let Some(source) = load_from_disk(&root, digest)
        {
            self.entries.insert(digest.to_string(), source);
        }
        self.entries.get(digest)
    }

    /// Whether the cache currently contains the given digest. Consults
    /// the disk layer (existence-only) when the in-memory map misses.
    #[must_use]
    pub fn contains(&self, digest: &str) -> bool {
        if self.entries.contains_key(digest) {
            return true;
        }
        if let Some(root) = self.disk_root.as_deref() {
            disk_has_digest(root, digest)
        } else {
            false
        }
    }

    /// Returns every (digest, source) pair currently cached in memory.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &CachedSource)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of in-memory cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the in-memory cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

fn digest_dir(root: &Path, digest: &str) -> PathBuf {
    root.join("pkg").join(digest)
}

fn write_to_disk(root: &Path, source: &CachedSource) -> io::Result<()> {
    let dir = digest_dir(root, &source.digest);
    let source_dir = dir.join("source");
    fs::create_dir_all(&source_dir)?;
    fs::write(dir.join("id.txt"), source.id.as_str().as_bytes())?;
    for (rel, bytes) in &source.files {
        let target = source_dir.join(rel);
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::write(target, bytes)?;
    }
    Ok(())
}

fn disk_has_digest(root: &Path, digest: &str) -> bool {
    digest_dir(root, digest).join("id.txt").is_file()
}

fn load_from_disk(root: &Path, digest: &str) -> Option<CachedSource> {
    let dir = digest_dir(root, digest);
    let id_text = fs::read_to_string(dir.join("id.txt")).ok()?;
    let id = ProjectId::parse(id_text.trim()).ok()?;
    let source_dir = dir.join("source");
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    walk_cached(&source_dir, &source_dir, &mut files).ok()?;
    let actual = compute_digest(&files);
    if actual != digest {
        eprintln!(
            "cache: warning: integrity check failed for {digest}; recomputed {actual} - ignoring on-disk copy"
        );
        return None;
    }
    Some(CachedSource {
        id,
        files,
        digest: digest.to_string(),
    })
}

fn walk_cached(base: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> io::Result<()> {
    if current.is_file() {
        let key = current.strip_prefix(base).map_or_else(
            |_| current.display().to_string(),
            |p| p.to_string_lossy().replace('\\', "/"),
        );
        out.insert(key, fs::read(current)?);
        return Ok(());
    }
    if !current.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = fs::read_dir(current)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for entry in entries {
        walk_cached(base, &entry, out)?;
    }
    Ok(())
}

/// Errors raised by the cache layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheError {
    /// The fetcher could not produce a source tree for this entry.
    /// Wraps the underlying transport / git / parser message.
    #[error("unsupported source for {0}")]
    Unsupported(String),
    /// The on-disk path source could not be read.
    #[error("path source for {id} unreadable at {path}")]
    PathUnreadable {
        /// Project id.
        id: String,
        /// Filesystem path that failed.
        path: String,
    },
    /// Digest mismatch - the cached payload differs from the recorded
    /// `sha256` in the manifest/lockfile.
    #[error("digest mismatch for {id}: expected {expected}, found {found}")]
    DigestMismatch {
        /// Project id.
        id: String,
        /// Expected digest.
        expected: String,
        /// Actually-computed digest.
        found: String,
    },
    /// The fetched package was marked yanked by the registry. Returned
    /// only when `--allow-yanked` was not passed.
    #[error("{id} version {version} is yanked: {reason}")]
    Yanked {
        /// Project id.
        id: String,
        /// Version that was yanked.
        version: String,
        /// Server-supplied reason or `"(no reason given)"`.
        reason: String,
    },
    /// A registry source arrived without the ed25519 signature +
    /// public key the registry is required to advertise. The tarball
    /// is rejected before it is unpacked.
    #[error("{0}: registry source is missing a publisher signature")]
    Unsigned(String),
    /// The publisher signature did not verify against the tarball
    /// bytes. The tarball is rejected before it is unpacked.
    #[error("{0}: publisher signature does not verify")]
    SignatureInvalid(String),
    /// The registry advertised a publisher key that differs from the
    /// one pinned in the lockfile - a key rotation or substitution.
    #[error("{id}: publisher key changed (pinned {pinned}, registry offered {offered})")]
    KeyMismatch {
        /// Project id.
        id: String,
        /// Key pinned in the lockfile.
        pinned: String,
        /// Key the registry index now advertises.
        offered: String,
    },
}

/// Resolved source tree fetched into the cache.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Resolved entry that produced this fetch.
    pub resolved: Resolved,
    /// Cached source contents.
    pub source: CachedSource,
    /// Hex-encoded publisher ed25519 public key that signed this
    /// source, when it came from a registry. Recorded into the
    /// lockfile so later fetches detect a key change. `None` for
    /// path/git/inline-tarball sources.
    pub owner_pubkey: Option<String>,
}
