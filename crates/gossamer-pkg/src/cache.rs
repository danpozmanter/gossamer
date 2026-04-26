//! Content-addressable cache backing the fetcher.
//! Every cached source tree lives at
//! `~/.gossamer/cache/projects/<sha256>/`. Ships the
//! addressing scheme + a tiny in-memory cache implementation that
//! tests can drive directly. The disk-backed implementation will
//! follow once the toolchain installer lands.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

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
        let mut buf = Vec::new();
        for (path, bytes) in &files {
            buf.extend_from_slice(path.as_bytes());
            buf.push(0);
            buf.extend_from_slice(bytes);
            buf.push(0);
        }
        let digest = sha256::hex(&buf);
        Self { id, files, digest }
    }
}

/// In-memory cache used by tests and as the on-disk cache's
/// transient layer.
#[derive(Debug, Default)]
pub struct Cache {
    entries: BTreeMap<String, CachedSource>,
}

impl Cache {
    /// Returns an empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Stores `source` keyed on its digest. Returns `true` when the
    /// entry was new.
    pub fn insert(&mut self, source: CachedSource) -> bool {
        let key = source.digest.clone();
        self.entries.insert(key, source).is_none()
    }

    /// Looks up a cached entry by digest.
    #[must_use]
    pub fn get(&self, digest: &str) -> Option<&CachedSource> {
        self.entries.get(digest)
    }

    /// Whether the cache currently contains the given digest.
    #[must_use]
    pub fn contains(&self, digest: &str) -> bool {
        self.entries.contains_key(digest)
    }

    /// Returns every (digest, source) pair currently cached.
    pub fn iter(&self) -> impl Iterator<Item = (&str, &CachedSource)> {
        self.entries.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Number of cached entries.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Errors raised by the cache layer.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CacheError {
    /// `Resolved` source kind is not yet supported by the in-memory
    /// fetcher (i.e. registry / git / tarball before disk fetching
    /// lands).
    #[error("unsupported source for {0}: real fetching not yet implemented")]
    Unsupported(String),
    /// The on-disk path source could not be read.
    #[error("path source for {id} unreadable at {path}")]
    PathUnreadable {
        /// Project id.
        id: String,
        /// Filesystem path that failed.
        path: String,
    },
    /// Digest mismatch — the cached payload differs from the recorded
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
}

/// Resolved source tree fetched into the cache.
#[derive(Debug, Clone)]
pub struct Fetched {
    /// Resolved entry that produced this fetch.
    pub resolved: Resolved,
    /// Cached source contents.
    pub source: CachedSource,
}
