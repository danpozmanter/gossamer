//! Infrastructure hook for per-source frontend caching.
//!
//! `gos run` / `gos check` / `gos test` currently re-parse and
//! re-typecheck every `.gos` source on every invocation. This
//! module lays the groundwork for skipping that work when the
//! source hasn't changed: it computes a content-addressed cache
//! key (source bytes + toolchain version) and persists a marker
//! per successful compile under a cache directory rooted at
//! `$XDG_CACHE_HOME/gossamer` (or `$HOME/.cache/gossamer` / the
//! workspace `target/` as a fallback).
//!
//! What it does **today**: records that a source was successfully
//! compiled, and reports cache hits through `observe_hit`.
//!
//! What it does **not yet do**: skip the actual compile. Achieving
//! that needs the frontend to serialize its intermediate
//! structures (`SourceFile`, `Resolutions`, `TypeTable`,
//! `HirProgram`) so a hit can deserialize instead of re-running
//! the pipeline. That work is scoped as the second half of this
//! feature and is deliberately out of this first slice — see
//! `docs/incremental.md` for the staged rollout.

#![forbid(unsafe_code)]

use std::fs;
use std::path::{Path, PathBuf};

use gossamer_pkg::sha256;

/// Content-addressed identifier for one frontend compile. The key
/// combines the source bytes with the toolchain version so a
/// compiler upgrade invalidates every cached entry.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FrontendCacheKey {
    hash: String,
}

impl FrontendCacheKey {
    /// Computes a cache key from `source` text and the driver
    /// toolchain identifier (typically `env!("CARGO_PKG_VERSION")`).
    #[must_use]
    pub fn new(source: &str, toolchain: &str) -> Self {
        let mut buf = Vec::with_capacity(source.len() + toolchain.len() + 8);
        buf.extend_from_slice(toolchain.as_bytes());
        buf.push(0);
        buf.extend_from_slice(source.as_bytes());
        Self {
            hash: sha256::hex(&buf),
        }
    }

    /// Returns the hex SHA-256 identifying this key.
    #[must_use]
    pub fn as_hex(&self) -> &str {
        &self.hash
    }
}

/// Resolves the cache root directory, creating it when absent.
/// Order of precedence: `GOSSAMER_CACHE_DIR` env var,
/// `$XDG_CACHE_HOME/gossamer`, `$HOME/.cache/gossamer`, then a
/// workspace-relative fallback under `target/gossamer-frontend`.
#[must_use]
pub fn cache_dir() -> PathBuf {
    if let Ok(explicit) = std::env::var("GOSSAMER_CACHE_DIR") {
        return PathBuf::from(explicit);
    }
    if let Ok(xdg) = std::env::var("XDG_CACHE_HOME") {
        return PathBuf::from(xdg).join("gossamer").join("frontend");
    }
    if let Ok(home) = std::env::var("HOME") {
        return PathBuf::from(home)
            .join(".cache")
            .join("gossamer")
            .join("frontend");
    }
    PathBuf::from("target").join("gossamer-frontend")
}

/// Writes a zero-byte marker indicating the key was compiled
/// successfully. Creates the cache directory on demand. Silently
/// swallows I/O errors — the cache is advisory, never required.
pub fn mark_success(key: &FrontendCacheKey) {
    mark_success_in(&cache_dir(), key);
}

/// Variant of [`mark_success`] that writes into `root` instead of
/// the shared cache directory. Used by tests and by callers that
/// want an isolated workspace-local cache.
pub fn mark_success_in(root: &Path, key: &FrontendCacheKey) {
    let _ = fs::create_dir_all(root);
    let _ = fs::write(marker_path(root, key), b"");
}

/// Returns `true` when `key` has a marker recorded by a prior
/// successful compile.
#[must_use]
pub fn observe_hit(key: &FrontendCacheKey) -> bool {
    observe_hit_in(&cache_dir(), key)
}

/// Variant of [`observe_hit`] that consults `root` instead of the
/// shared cache directory.
#[must_use]
pub fn observe_hit_in(root: &Path, key: &FrontendCacheKey) -> bool {
    marker_path(root, key).is_file()
}

/// Serializes `value` as a bincode blob keyed by `key`. Errors
/// silently — cache writes are advisory.
pub fn store_blob<T: serde::Serialize>(key: &FrontendCacheKey, value: &T) {
    store_blob_in(&cache_dir(), key, value);
}

/// Variant of [`store_blob`] that writes into `root` instead of the
/// shared cache directory.
pub fn store_blob_in<T: serde::Serialize>(root: &Path, key: &FrontendCacheKey, value: &T) {
    let _ = fs::create_dir_all(root);
    let Ok(encoded) = bincode::serialize(value) else {
        return;
    };
    let _ = fs::write(blob_path(root, key), encoded);
}

/// Attempts to load a previously-cached blob for `key`, returning
/// `None` on any failure (absent, corrupt, wrong schema).
#[must_use]
pub fn load_blob<T: serde::de::DeserializeOwned>(key: &FrontendCacheKey) -> Option<T> {
    load_blob_in(&cache_dir(), key)
}

/// Variant of [`load_blob`] that reads from `root` instead of the
/// shared cache directory.
#[must_use]
pub fn load_blob_in<T: serde::de::DeserializeOwned>(
    root: &Path,
    key: &FrontendCacheKey,
) -> Option<T> {
    let bytes = fs::read(blob_path(root, key)).ok()?;
    bincode::deserialize(&bytes).ok()
}

fn marker_path(dir: &Path, key: &FrontendCacheKey) -> PathBuf {
    dir.join(format!("{}.ok", key.as_hex()))
}

fn blob_path(dir: &Path, key: &FrontendCacheKey) -> PathBuf {
    dir.join(format!("{}.bin", key.as_hex()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_is_deterministic_for_the_same_source() {
        let a = FrontendCacheKey::new("fn main() {}\n", "0.0.0");
        let b = FrontendCacheKey::new("fn main() {}\n", "0.0.0");
        assert_eq!(a, b);
    }

    #[test]
    fn cache_key_changes_when_toolchain_version_changes() {
        let a = FrontendCacheKey::new("fn main() {}\n", "0.0.0");
        let b = FrontendCacheKey::new("fn main() {}\n", "0.0.1");
        assert_ne!(a, b);
    }

    #[test]
    fn cache_key_changes_with_any_source_byte_change() {
        let a = FrontendCacheKey::new("fn main() {}\n", "0.0.0");
        let b = FrontendCacheKey::new("fn main() { }\n", "0.0.0");
        assert_ne!(a, b);
    }

    #[test]
    fn mark_and_observe_round_trip_in_an_isolated_dir() {
        let tmp = tempdir();
        let key = FrontendCacheKey::new("fn a() {}\n", "test");
        assert!(!observe_hit_in(&tmp, &key));
        mark_success_in(&tmp, &key);
        assert!(observe_hit_in(&tmp, &key));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn blob_round_trip_preserves_value() {
        let tmp = tempdir();
        let key = FrontendCacheKey::new("fn a() {}\n", "test");
        let payload = vec!["alpha".to_string(), "beta".to_string()];
        assert!(load_blob_in::<Vec<String>>(&tmp, &key).is_none());
        store_blob_in(&tmp, &key, &payload);
        let round_trip: Vec<String> = load_blob_in(&tmp, &key).expect("blob not found");
        assert_eq!(round_trip, payload);
        let _ = fs::remove_dir_all(&tmp);
    }

    fn tempdir() -> PathBuf {
        let base = std::env::temp_dir();
        let pid = std::process::id();
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let path = base.join(format!("gossamer-cache-test-{pid}-{nonce}"));
        fs::create_dir_all(&path).unwrap();
        path
    }
}
