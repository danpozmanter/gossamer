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
//! What it does **today**: persists a successfully parsed source file and
//! treats a complete, deserializable blob as a cache hit.
//!
//! What it does **not yet do**: skip the actual compile. Achieving
//! that needs the frontend to serialize its intermediate
//! structures (`SourceFile`, `Resolutions`, `TypeTable`,
//! `HirProgram`) so a hit can deserialize instead of re-running
//! the pipeline. That work is scoped as the second half of this
//! feature and is deliberately out of this first slice - see
//! `docs/incremental.md` for the staged rollout.

#![forbid(unsafe_code)]

use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use gossamer_pkg::sha256;

const BLOB_MAGIC: &[u8; 8] = b"GOSFC001";
const MAX_BLOB_BYTES: usize = 16 * 1024 * 1024;
static CACHE_WRITE_ID: AtomicU64 = AtomicU64::new(0);

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
        Self::new_with_context(source, toolchain, "edition=2026")
    }

    /// Computes a cache key with an explicit semantic context. An edition
    /// changes source typing and lowering, so it must never share a cached AST
    /// with a different edition.
    #[must_use]
    pub fn new_with_context(source: &str, toolchain: &str, context: &str) -> Self {
        // The build stamp changes when the frontend crates recompile, so a
        // development rebuild with an unchanged version string cannot
        // serve ASTs parsed by older frontend code.
        let stamp = env!("GOS_DRIVER_BUILD_STAMP");
        // The stamp alone rotates only when a frontend crate's sources
        // change (its build script watches those directories). A rebuild
        // that touches other compiler crates relinks the executable
        // without re-running that script, so the running binary's own
        // identity is mixed in too - any rebuilt `gos` starts from a
        // cold frontend cache instead of consuming blobs written by a
        // different build of the compiler. Mirrors the object cache's
        // `compiler_fingerprint`.
        let exe = exe_fingerprint();
        let mut buf = Vec::with_capacity(
            source.len() + toolchain.len() + context.len() + stamp.len() + exe.len() + 10,
        );
        buf.extend_from_slice(toolchain.as_bytes());
        buf.push(0);
        buf.extend_from_slice(stamp.as_bytes());
        buf.push(0);
        buf.extend_from_slice(exe.as_bytes());
        buf.push(0);
        buf.extend_from_slice(context.as_bytes());
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

/// Identity of the running compiler binary (size and mtime), computed once.
/// Folded into every frontend cache key so a rebuilt `gos` - even one whose
/// frontend crates did not recompile - never reads blobs a different build
/// of the compiler wrote.
fn exe_fingerprint() -> &'static str {
    static FP: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    FP.get_or_init(|| {
        let mut s = String::new();
        if let Ok(exe) = std::env::current_exe()
            && let Ok(meta) = fs::metadata(&exe)
        {
            s.push_str(&format!("len={}", meta.len()));
            if let Ok(mtime) = meta.modified()
                && let Ok(dur) = mtime.duration_since(std::time::UNIX_EPOCH)
            {
                s.push_str(&format!("|mtime={}", dur.as_nanos()));
            }
        }
        s
    })
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

/// Serializes `value` as a postcard blob keyed by `key`. Errors
/// silently - cache writes are advisory.
pub fn store_blob<T: serde::Serialize>(key: &FrontendCacheKey, value: &T) {
    store_blob_in(&cache_dir(), key, value);
}

/// Variant of [`store_blob`] that writes into `root` instead of the
/// shared cache directory.
pub fn store_blob_in<T: serde::Serialize>(root: &Path, key: &FrontendCacheKey, value: &T) {
    let _ = fs::create_dir_all(root);
    let Ok(encoded) = postcard::to_allocvec(value) else {
        return;
    };
    if encoded.len() > MAX_BLOB_BYTES {
        return;
    }
    // Do not build a second `magic + encoded` allocation. Frontend trees can
    // approach the blob ceiling, and the atomic writer already owns the file
    // handle needed to write a small prefix followed by the postcard bytes.
    let _ = write_atomic_parts(&blob_path(root, key), BLOB_MAGIC, &encoded);
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
    let bytes = read_capped(&blob_path(root, key), BLOB_MAGIC.len() + MAX_BLOB_BYTES)?;
    let payload = bytes.strip_prefix(BLOB_MAGIC)?;
    postcard::from_bytes(payload).ok()
}

/// Writes `bytes` directly to the cache file for `key` without any
/// envelope encoding. Use this for raw binary blobs (object files,
/// large buffers) where a serde `Vec<u8>` round-trip would force
/// an extra full-buffer clone on every cache miss.
pub fn store_raw(key: &FrontendCacheKey, bytes: &[u8]) {
    store_raw_in(&cache_dir(), key, bytes);
}

/// Variant of [`store_raw`] that writes into `root`.
pub fn store_raw_in(root: &Path, key: &FrontendCacheKey, bytes: &[u8]) {
    let _ = fs::create_dir_all(root);
    let _ = write_atomic(&blob_path(root, key), bytes);
}

/// Returns the on-disk path the cache uses for `key` if a prior
/// `store_raw` (or `store_blob`) populated it. Used by callers that
/// want to copy the cache file directly to a destination path
/// instead of round-tripping through a `Vec<u8>` in memory.
#[must_use]
pub fn raw_blob_path(key: &FrontendCacheKey) -> Option<PathBuf> {
    raw_blob_path_in(&cache_dir(), key)
}

/// Variant of [`raw_blob_path`] that consults `root`.
#[must_use]
pub fn raw_blob_path_in(root: &Path, key: &FrontendCacheKey) -> Option<PathBuf> {
    let p = blob_path(root, key);
    if p.is_file() { Some(p) } else { None }
}

fn blob_path(dir: &Path, key: &FrontendCacheKey) -> PathBuf {
    dir.join(format!("{}.bin", key.as_hex()))
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, |file| file.write_all(bytes))
}

fn write_atomic_parts(path: &Path, first: &[u8], second: &[u8]) -> std::io::Result<()> {
    write_atomic_with(path, |file| {
        file.write_all(first)?;
        file.write_all(second)
    })
}

fn write_atomic_with<F>(path: &Path, write: F) -> std::io::Result<()>
where
    F: FnOnce(&mut fs::File) -> std::io::Result<()>,
{
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "cache path has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("cache"),
        std::process::id(),
        CACHE_WRITE_ID.fetch_add(1, Ordering::Relaxed),
    ));
    let result = (|| {
        let mut file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)?;
        write(&mut file)?;
        file.sync_all()?;
        fs::rename(&tmp, path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result
}

/// Reads one cache blob with a sentinel byte. A prior `metadata` size check is
/// insufficient because another process can replace the file before the read;
/// taking at most one extra byte keeps advisory cache corruption from becoming
/// an unbounded allocation.
fn read_capped(path: &Path, limit: usize) -> Option<Vec<u8>> {
    let file = fs::File::open(path).ok()?;
    let mut bytes = Vec::new();
    file.take(u64::try_from(limit.saturating_add(1)).ok()?)
        .read_to_end(&mut bytes)
        .ok()?;
    (bytes.len() <= limit).then_some(bytes)
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
    fn cache_key_changes_when_edition_changes() {
        let eager = FrontendCacheKey::new_with_context("fn main() {}\n", "0.0.0", "edition=2026");
        let lazy = FrontendCacheKey::new_with_context("fn main() {}\n", "0.0.0", "edition=2027");
        assert_ne!(eager, lazy);
    }

    #[test]
    fn cache_key_changes_with_any_source_byte_change() {
        let a = FrontendCacheKey::new("fn main() {}\n", "0.0.0");
        let b = FrontendCacheKey::new("fn main() { }\n", "0.0.0");
        assert_ne!(a, b);
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

    #[test]
    fn blob_loader_rejects_legacy_and_oversized_files_before_decode() {
        let tmp = tempdir();
        let key = FrontendCacheKey::new("fn a() {}\n", "test");
        let path = blob_path(&tmp, &key);
        fs::write(&path, postcard::to_allocvec(&vec!["legacy"]).unwrap()).unwrap();
        assert!(load_blob_in::<Vec<String>>(&tmp, &key).is_none());

        let file = fs::OpenOptions::new()
            .write(true)
            .truncate(true)
            .open(&path)
            .unwrap();
        file.set_len(u64::try_from(BLOB_MAGIC.len() + MAX_BLOB_BYTES + 1).unwrap())
            .unwrap();
        assert!(load_blob_in::<Vec<String>>(&tmp, &key).is_none());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn capped_reader_rejects_the_sentinel_byte() {
        let tmp = tempdir();
        let path = tmp.join("blob");
        fs::write(&path, b"four").unwrap();
        assert!(read_capped(&path, 3).is_none());
        assert_eq!(read_capped(&path, 4), Some(b"four".to_vec()));
        let _ = fs::remove_dir_all(tmp);
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
