//! Fetcher backing `gos fetch` / `gos vendor`.
//! The Path source kind reads the working directory directly. Git,
//! registry, and URL-tarball sources currently produce deterministic
//! synthetic payloads keyed off their pin so the cache layer + the
//! offline-mode logic can be exercised end-to-end. swaps the
//! synthetic implementations for real network fetchers.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::cache::{Cache, CacheError, CachedSource, Fetched};
use crate::resolver::{Resolved, ResolvedSource};
use crate::sha256;
use crate::tar;
use crate::transport::{StaticTransport, Transport, TransportError};

/// Fetcher configuration.
#[derive(Debug, Clone, Default)]
pub struct FetchOptions {
    /// When `true`, the fetcher refuses to populate cache entries it
    /// does not already have. Mirrors the SPEC §16.x `--offline` flag.
    pub offline: bool,
}

/// Fetcher driver.
pub struct Fetcher {
    options: FetchOptions,
    transport: Arc<dyn Transport>,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Fetcher")
            .field("options", &self.options)
            .field("transport", &"<dyn Transport>")
            .finish()
    }
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new(FetchOptions::default())
    }
}

impl Fetcher {
    /// Constructs a fetcher with the given options. The tarball path
    /// wires up an empty [`StaticTransport`] — callers that actually
    /// want to pull from the network use [`Fetcher::with_transport`].
    #[must_use]
    pub fn new(options: FetchOptions) -> Self {
        Self {
            options,
            transport: Arc::new(StaticTransport::new()),
        }
    }

    /// Constructs a fetcher that uses `transport` for every
    /// network-backed source kind. The transport is stored behind an
    /// `Arc<dyn Transport>` so tests can inject a
    /// [`StaticTransport`] while production builds hand in
    /// [`crate::HttpsTransport`].
    #[must_use]
    pub fn with_transport(options: FetchOptions, transport: Arc<dyn Transport>) -> Self {
        Self { options, transport }
    }

    /// Resolves every entry in `resolved` and inserts its source tree
    /// into `cache`. Returns one [`Fetched`] per entry in input order.
    pub fn fetch_all(
        &self,
        resolved: &[Resolved],
        cache: &mut Cache,
    ) -> Result<Vec<Fetched>, CacheError> {
        let mut out = Vec::with_capacity(resolved.len());
        for entry in resolved {
            out.push(self.fetch_one(entry, cache)?);
        }
        Ok(out)
    }

    fn fetch_one(&self, resolved: &Resolved, cache: &mut Cache) -> Result<Fetched, CacheError> {
        let source = match &resolved.pin {
            ResolvedSource::Path(path) => fetch_path(resolved, Path::new(path))?,
            ResolvedSource::Git { url, reference } => synthetic_source(
                resolved,
                &format!("git\0{url}\0{reference}"),
            ),
            ResolvedSource::Registry(version) => synthetic_source(
                resolved,
                &format!("registry\0{}\0{version}", resolved.id),
            ),
            ResolvedSource::Tarball { url, sha256: hash } => {
                self.fetch_tarball(resolved, url, hash)?
            }
        };
        if self.options.offline && !cache.contains(&source.digest) {
            return Err(CacheError::Unsupported(format!(
                "{}: offline mode and entry not in cache",
                resolved.id
            )));
        }
        cache.insert(source.clone());
        Ok(Fetched {
            resolved: resolved.clone(),
            source,
        })
    }
}

impl Fetcher {
    /// Fetches `url` via the configured transport, verifies the raw
    /// archive bytes against `expected_sha256`, unpacks the
    /// resulting tarball into a file map, and builds a
    /// [`CachedSource`]. A digest mismatch is a hard error: under no
    /// circumstances do untrusted bytes reach the cache.
    fn fetch_tarball(
        &self,
        resolved: &Resolved,
        url: &str,
        expected_sha256: &str,
    ) -> Result<CachedSource, CacheError> {
        let bytes = self.transport.get(url).map_err(|e| map_transport_error(&resolved.id, e))?;
        let actual = sha256::hex(&bytes);
        if actual != expected_sha256 {
            return Err(CacheError::DigestMismatch {
                id: resolved.id.as_str().to_string(),
                expected: expected_sha256.to_string(),
                found: actual,
            });
        }
        let files = tar::unpack(&bytes).map_err(|e| CacheError::Unsupported(format!(
            "{}: tarball unpack failed: {e}",
            resolved.id
        )))?;
        Ok(CachedSource::build(resolved.id.clone(), files))
    }
}

fn map_transport_error(id: &crate::id::ProjectId, err: TransportError) -> CacheError {
    CacheError::Unsupported(format!("{id}: transport: {err}"))
}

fn fetch_path(resolved: &Resolved, base: &Path) -> Result<CachedSource, CacheError> {
    let mut files = BTreeMap::new();
    walk_path(base, base, &mut files).map_err(|_| CacheError::PathUnreadable {
        id: resolved.id.as_str().to_string(),
        path: base.display().to_string(),
    })?;
    Ok(CachedSource::build(resolved.id.clone(), files))
}

fn walk_path(
    base: &Path,
    current: &Path,
    out: &mut BTreeMap<String, Vec<u8>>,
) -> std::io::Result<()> {
    if current.is_file() {
        let bytes = std::fs::read(current)?;
        let key = relative_key(base, current);
        out.insert(key, bytes);
        return Ok(());
    }
    if !current.is_dir() {
        return Ok(());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(current)?
        .filter_map(|res| res.ok().map(|e| e.path()))
        .collect();
    entries.sort();
    for entry in entries {
        walk_path(base, &entry, out)?;
    }
    Ok(())
}

fn relative_key(base: &Path, file: &Path) -> String {
    file.strip_prefix(base)
        .ok()
        .map_or_else(|| file.display().to_string(), |p| p.to_string_lossy().into_owned())
        .replace('\\', "/")
}

fn synthetic_source(resolved: &Resolved, seed: &str) -> CachedSource {
    let mut files = BTreeMap::new();
    let body = format!(
        "// stub source for {id} (seed {seed})\nfn __stub() {{}}\n",
        id = resolved.id
    );
    files.insert("src/main.gos".to_string(), body.into_bytes());
    let digest_seed = format!("{}\0{seed}", resolved.id);
    let digest = sha256::hex(digest_seed.as_bytes());
    CachedSource {
        id: resolved.id.clone(),
        files,
        digest,
    }
}

/// Implements `gos vendor` by writing every cached source tree under
/// `dest_dir/<id-with-slashes-replaced>/`. Returns the per-id list of
/// written files.
pub fn vendor(
    fetched: &[Fetched],
    dest_dir: &Path,
) -> Result<BTreeMap<String, Vec<String>>, std::io::Error> {
    std::fs::create_dir_all(dest_dir)?;
    let mut out = BTreeMap::new();
    for entry in fetched {
        let project_dir =
            dest_dir.join(entry.resolved.id.as_str().replace('/', "__"));
        std::fs::create_dir_all(&project_dir)?;
        let mut written = Vec::new();
        for (path, bytes) in &entry.source.files {
            let target = project_dir.join(path);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&target, bytes)?;
            written.push(path.clone());
        }
        out.insert(entry.resolved.id.as_str().to_string(), written);
    }
    Ok(out)
}
