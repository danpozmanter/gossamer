//! Fetcher backing `gos fetch` / `gos vendor`.
//!
//! Materialises every source kind declared in the manifest:
//!
//! - `Path` — read the local filesystem.
//! - `Tarball` — HTTP(S) GET, sha256-verify, USTAR-unpack.
//! - `Git` — shell out to `git clone --bare` into the cache, then
//!   `git archive` the requested ref into the source tree.
//! - `Registry` — look the version up in the [`VersionCatalogue`]
//!   for a `(download_url, tarball_sha256)` pair, fetch that
//!   tarball, sha256-verify, USTAR-unpack.
//!
//! Every network fetch is content-addressed: the verified payload's
//! sha256 is the cache key. Cache hits skip the network entirely.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use crate::cache::{Cache, CacheError, CachedSource, Fetched};
use crate::resolver::{CatalogueEntry, Resolved, ResolvedSource, VersionCatalogue};
use crate::sha256;
use crate::tar;
use crate::transport::{StaticTransport, Transport, TransportError};

/// Default registry URL for the public Gossamer package server.
pub const DEFAULT_REGISTRY_URL: &str = "https://pkg.gossamer.dev";

/// Fetcher configuration.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// When `true`, the fetcher refuses to populate cache entries it
    /// does not already have. Mirrors the SPEC §16.x `--offline` flag.
    pub offline: bool,
    /// When `true`, fetches a yanked registry version succeed.
    /// Default is to error with [`CacheError::Yanked`].
    pub allow_yanked: bool,
    /// Registry URL used for catalogue lookups (e.g. download URLs
    /// and sha256 pins). Defaults to [`DEFAULT_REGISTRY_URL`].
    pub registry_url: String,
    /// Optional bearer token sent on registry requests. The CLI loads
    /// this from `~/.gossamer/credentials.toml`.
    pub auth_token: Option<String>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            offline: false,
            allow_yanked: false,
            registry_url: DEFAULT_REGISTRY_URL.to_string(),
            auth_token: None,
        }
    }
}

impl FetchOptions {
    /// Builds a default `FetchOptions` with a registry URL set.
    #[must_use]
    pub fn with_registry(registry_url: impl Into<String>) -> Self {
        Self {
            registry_url: registry_url.into(),
            ..Self::default()
        }
    }
}

/// Fetcher driver.
pub struct Fetcher {
    options: FetchOptions,
    transport: Arc<dyn Transport>,
    catalogue: VersionCatalogue,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Fetcher")
            .field("options", &self.options)
            .field("transport", &"<dyn Transport>")
            .field("catalogue", &self.catalogue)
            .finish()
    }
}

impl Default for Fetcher {
    fn default() -> Self {
        Self::new(FetchOptions::default())
    }
}

impl Fetcher {
    /// Constructs a fetcher with the given options.
    #[must_use]
    pub fn new(options: FetchOptions) -> Self {
        Self {
            options,
            transport: Arc::new(StaticTransport::new()),
            catalogue: VersionCatalogue::new(),
        }
    }

    /// Constructs a fetcher that uses `transport` for every
    /// network-backed source kind.
    #[must_use]
    pub fn with_transport(options: FetchOptions, transport: Arc<dyn Transport>) -> Self {
        Self {
            options,
            transport,
            catalogue: VersionCatalogue::new(),
        }
    }

    /// Attaches a pre-populated catalogue so registry fetches can
    /// look up `(download_url, sha256)` without re-hitting the index.
    #[must_use]
    pub fn with_catalogue(mut self, catalogue: VersionCatalogue) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Returns the transport this fetcher uses.
    #[must_use]
    pub fn transport(&self) -> &Arc<dyn Transport> {
        &self.transport
    }

    /// Returns the configured options.
    #[must_use]
    pub fn options(&self) -> &FetchOptions {
        &self.options
    }

    /// Returns the catalogue this fetcher consults for registry lookups.
    #[must_use]
    pub fn catalogue(&self) -> &VersionCatalogue {
        &self.catalogue
    }

    /// Mutable handle to the catalogue.
    pub fn catalogue_mut(&mut self) -> &mut VersionCatalogue {
        &mut self.catalogue
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
            ResolvedSource::Git { url, reference } => self.fetch_git(resolved, url, reference)?,
            ResolvedSource::Registry(version) => self.fetch_registry(resolved, *version)?,
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

    fn fetch_tarball(
        &self,
        resolved: &Resolved,
        url: &str,
        expected_sha256: &str,
    ) -> Result<CachedSource, CacheError> {
        let bytes = self
            .transport
            .get(url)
            .map_err(|e| map_transport_error(&resolved.id, e))?;
        let actual = sha256::hex(&bytes);
        if actual != expected_sha256 {
            return Err(CacheError::DigestMismatch {
                id: resolved.id.as_str().to_string(),
                expected: expected_sha256.to_string(),
                found: actual,
            });
        }
        let files = tar::unpack(&bytes).map_err(|e| {
            CacheError::Unsupported(format!("{}: tarball unpack failed: {e}", resolved.id))
        })?;
        Ok(CachedSource::build(resolved.id.clone(), files))
    }

    fn fetch_registry(
        &self,
        resolved: &Resolved,
        version: crate::version::Version,
    ) -> Result<CachedSource, CacheError> {
        let entry = self.catalogue.entry(&resolved.id, version).ok_or_else(|| {
            CacheError::Unsupported(format!(
                "{}: registry catalogue has no entry for {version}; \
                 did you run `gos fetch` to hydrate the index?",
                resolved.id
            ))
        })?;
        if entry.yanked && !self.options.allow_yanked {
            return Err(CacheError::Yanked {
                id: resolved.id.as_str().to_string(),
                version: version.to_string(),
                reason: entry
                    .yank_reason
                    .clone()
                    .unwrap_or_else(|| "(no reason given)".to_string()),
            });
        }
        let url = registry_download_url(&self.options.registry_url, &resolved.id, entry);
        let expected = entry.tarball_sha256.clone().ok_or_else(|| {
            CacheError::Unsupported(format!(
                "{}: registry entry for {version} is missing a sha256 pin",
                resolved.id
            ))
        })?;
        self.fetch_tarball(resolved, &url, &expected)
    }

    fn fetch_git(
        &self,
        resolved: &Resolved,
        url: &str,
        reference: &str,
    ) -> Result<CachedSource, CacheError> {
        let cache_root = crate::cache::default_cache_root().ok_or_else(|| {
            CacheError::Unsupported(format!(
                "{}: git fetch needs a writable cache (set GOS_CACHE_DIR or $HOME)",
                resolved.id
            ))
        })?;
        let bare_dir = cache_root.join("git").join(sha256::hex(url.as_bytes()));
        ensure_git_clone(url, &bare_dir).map_err(|e| {
            CacheError::Unsupported(format!("{}: git fetch failed: {e}", resolved.id))
        })?;
        let tarball = git_archive(&bare_dir, reference).map_err(|e| {
            CacheError::Unsupported(format!("{}: git archive failed: {e}", resolved.id))
        })?;
        let files = tar::unpack(&tarball).map_err(|e| {
            CacheError::Unsupported(format!("{}: tarball unpack failed: {e}", resolved.id))
        })?;
        Ok(CachedSource::build(resolved.id.clone(), files))
    }
}

fn registry_download_url(
    registry_url: &str,
    id: &crate::id::ProjectId,
    entry: &CatalogueEntry,
) -> String {
    if let Some(url) = &entry.download_url {
        return url.clone();
    }
    format!(
        "{base}/v1/download/{id}/{version}.tar",
        base = registry_url.trim_end_matches('/'),
        id = id.as_str(),
        version = entry.version,
    )
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
        .map_or_else(
            || file.display().to_string(),
            |p| p.to_string_lossy().into_owned(),
        )
        .replace('\\', "/")
}

/// Ensures `bare_dir` contains a bare clone of `url`, creating it
/// (or refreshing) as needed.
fn ensure_git_clone(url: &str, bare_dir: &Path) -> std::io::Result<()> {
    if bare_dir.join("HEAD").is_file() {
        let out = Command::new("git")
            .arg("--git-dir")
            .arg(bare_dir)
            .arg("fetch")
            .arg("--all")
            .arg("--tags")
            .output()?;
        if !out.status.success() {
            return Err(std::io::Error::other(format!(
                "git fetch in {}: {}",
                bare_dir.display(),
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        return Ok(());
    }
    if let Some(parent) = bare_dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let out = Command::new("git")
        .arg("clone")
        .arg("--bare")
        .arg(url)
        .arg(bare_dir)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git clone {url} -> {}: {}",
            bare_dir.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(())
}

/// Produces a tar buffer of the project tree at `reference` from the
/// bare clone at `bare_dir`.
fn git_archive(bare_dir: &Path, reference: &str) -> std::io::Result<Vec<u8>> {
    let out = Command::new("git")
        .arg("--git-dir")
        .arg(bare_dir)
        .arg("archive")
        .arg("--format=tar")
        .arg(reference)
        .output()?;
    if !out.status.success() {
        return Err(std::io::Error::other(format!(
            "git archive {} {reference}: {}",
            bare_dir.display(),
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    Ok(out.stdout)
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
        let project_dir = dest_dir.join(entry.resolved.id.as_str().replace('/', "__"));
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

/// Deterministic test-only synthetic-source helper, retained behind
/// `cfg(any(test, feature = "synthetic-source"))` so production
/// fetches cannot accidentally invoke it.
#[cfg(any(test, feature = "synthetic-source"))]
#[must_use]
pub fn synthetic_source_for_test(resolved: &Resolved, seed: &str) -> CachedSource {
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
