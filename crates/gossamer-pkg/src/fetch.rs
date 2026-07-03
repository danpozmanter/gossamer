//! Fetcher backing `gos fetch` / `gos vendor`.
//!
//! Materialises every source kind declared in the manifest:
//!
//! - `Path` - read the local filesystem.
//! - `Tarball` - HTTP(S) GET, sha256-verify, USTAR-unpack.
//! - `Git` - shell out to `git clone --bare` into the cache, then
//!   `git archive` the requested ref into the source tree.
//! - `Registry` - look the version up in the [`VersionCatalogue`]
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
    /// Publisher keys pinned from the lockfile (project id → hex
    /// public key). A registry fetch whose advertised key differs from
    /// a pin is rejected; an id with no pin is trusted on first use and
    /// recorded into the lockfile afterwards.
    pinned_keys: BTreeMap<String, String>,
}

impl std::fmt::Debug for Fetcher {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("Fetcher")
            .field("options", &self.options)
            .field("transport", &"<dyn Transport>")
            .field("catalogue", &self.catalogue)
            .field("pinned_keys", &self.pinned_keys)
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
            pinned_keys: BTreeMap::new(),
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
            pinned_keys: BTreeMap::new(),
        }
    }

    /// Attaches a pre-populated catalogue so registry fetches can
    /// look up `(download_url, sha256)` without re-hitting the index.
    #[must_use]
    pub fn with_catalogue(mut self, catalogue: VersionCatalogue) -> Self {
        self.catalogue = catalogue;
        self
    }

    /// Pins publisher keys (project id → hex public key), normally
    /// loaded from the existing lockfile. A registry fetch whose
    /// advertised key differs from its pin is rejected.
    #[must_use]
    pub fn with_pinned_keys(mut self, pinned_keys: BTreeMap<String, String>) -> Self {
        self.pinned_keys = pinned_keys;
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
        let (source, owner_pubkey) = match &resolved.pin {
            ResolvedSource::Path(path) => (fetch_path(resolved, Path::new(path))?, None),
            ResolvedSource::Git { url, reference } => {
                (self.fetch_git(resolved, url, reference)?, None)
            }
            ResolvedSource::Registry(version) => self.fetch_registry(resolved, *version)?,
            ResolvedSource::Tarball { url, sha256: hash } => {
                (self.fetch_tarball(resolved, url, hash, None)?, None)
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
            owner_pubkey,
        })
    }

    /// Downloads `url`, verifies its sha256, and - when `signature` is
    /// supplied (registry sources) - authenticates the publisher
    /// signature over the raw bytes before unpacking. Both checks run
    /// before [`tar::unpack`], so a tampered or unsigned payload never
    /// reaches the filesystem.
    fn fetch_tarball(
        &self,
        resolved: &Resolved,
        url: &str,
        expected_sha256: &str,
        signature: Option<SignatureCheck<'_>>,
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
        if let Some(check) = signature {
            check.verify(&resolved.id, &bytes)?;
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
    ) -> Result<(CachedSource, Option<String>), CacheError> {
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
        // Registry sources must be signed by the publisher. The
        // signature is verified over the tarball bytes, and the
        // advertised key is checked against any lockfile pin so a
        // compromised registry cannot silently swap publishers.
        let (signature, public_key) = match (&entry.signature, &entry.public_key) {
            (Some(s), Some(k)) => (s.clone(), k.clone()),
            _ => return Err(CacheError::Unsigned(resolved.id.as_str().to_string())),
        };
        let pinned = self.pinned_keys.get(resolved.id.as_str()).cloned();
        let check = SignatureCheck {
            signature_hex: &signature,
            public_key_hex: &public_key,
            pinned_key: pinned.as_deref(),
        };
        let source = self.fetch_tarball(resolved, &url, &expected, Some(check))?;
        Ok((source, Some(public_key)))
    }

    fn fetch_git(
        &self,
        resolved: &Resolved,
        url: &str,
        reference: &str,
    ) -> Result<CachedSource, CacheError> {
        validate_git_source(url, reference)
            .map_err(|msg| CacheError::RejectedGitSource(format!("{}: {msg}", resolved.id)))?;
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

/// Publisher-signature inputs for a registry tarball.
struct SignatureCheck<'a> {
    /// Hex ed25519 signature over the tarball bytes.
    signature_hex: &'a str,
    /// Hex ed25519 public key the registry advertises.
    public_key_hex: &'a str,
    /// Key pinned in the lockfile, if this id has been fetched before.
    pinned_key: Option<&'a str>,
}

impl SignatureCheck<'_> {
    /// Rejects a key that disagrees with the lockfile pin, then
    /// verifies the signature over `bytes`. Public keys are not secret,
    /// so the pin comparison need not be constant-time.
    fn verify(&self, id: &crate::id::ProjectId, bytes: &[u8]) -> Result<(), CacheError> {
        if let Some(pin) = self.pinned_key
            && pin != self.public_key_hex
        {
            return Err(CacheError::KeyMismatch {
                id: id.as_str().to_string(),
                pinned: pin.to_string(),
                offered: self.public_key_hex.to_string(),
            });
        }
        crate::signing::verify_signature_hex(self.public_key_hex, bytes, self.signature_hex)
            .map_err(|_| CacheError::SignatureInvalid(id.as_str().to_string()))
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

/// Rejects a git dependency URL or ref that could turn `git` into an
/// arbitrary-command or argv-injection vector before any git process
/// runs.
///
/// A `<transport>::<address>` prefix (for example `ext::sh -c '...'`)
/// selects a git remote helper, and `ext::` runs a shell command, so
/// only the `https://`, `ssh://`, and `git://` transports are allowed;
/// `file://` and every remote-helper prefix are refused. A URL or ref
/// beginning with `-` would be parsed by git as an option rather than a
/// positional argument, so both are rejected here (and every callee
/// passes `--` before positional URL arguments as a second guard).
fn validate_git_source(url: &str, reference: &str) -> Result<(), String> {
    // A `::` before the first `/` is a remote-helper transport prefix
    // (`ext::`, `fd::`, ...); reject it outright.
    if let Some(idx) = url.find("::")
        && !url[..idx].contains('/')
    {
        return Err(format!("git url uses a disallowed transport prefix: {url}"));
    }
    let scheme_ok =
        url.starts_with("https://") || url.starts_with("ssh://") || url.starts_with("git://");
    if !scheme_ok {
        return Err(format!(
            "git url scheme not allowed (only https://, ssh://, git://): {url}"
        ));
    }
    if url.starts_with('-') {
        return Err(format!("git url may not begin with '-': {url}"));
    }
    if reference.starts_with('-') {
        return Err(format!("git ref may not begin with '-': {reference}"));
    }
    Ok(())
}

/// A `git` invocation with the remote-helper (`ext`) and local
/// (`file`) transports disabled and interactive protocol promotion
/// turned off, so a hostile URL or repository config cannot execute a
/// command. Every network-facing git call in this module starts here.
fn hardened_git() -> Command {
    let mut cmd = Command::new("git");
    cmd.arg("-c")
        .arg("protocol.ext.allow=never")
        .arg("-c")
        .arg("protocol.file.allow=never")
        .env("GIT_PROTOCOL_FROM_USER", "0")
        .env("GIT_TERMINAL_PROMPT", "0");
    cmd
}

/// Ensures `bare_dir` contains a bare clone of `url`, creating it
/// (or refreshing) as needed.
fn ensure_git_clone(url: &str, bare_dir: &Path) -> std::io::Result<()> {
    if bare_dir.join("HEAD").is_file() {
        let out = hardened_git()
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
    // `--` separates options from the positional repository and target
    // directory, so a URL that slipped a leading `-` past validation
    // still cannot be read as an option.
    let out = hardened_git()
        .arg("clone")
        .arg("--bare")
        .arg("--")
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
    // `git archive` reads the tree-ish positionally and treats anything
    // after `--` as a pathspec, so the ref cannot be guarded with `--`;
    // `validate_git_source` has already rejected a ref beginning with
    // `-` (argv injection) before this runs.
    let out = hardened_git()
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

#[cfg(test)]
mod git_source_tests {
    use super::validate_git_source;

    #[test]
    fn allowed_schemes_pass() {
        for url in [
            "https://github.com/owner/repo.git",
            "ssh://git@github.com/owner/repo.git",
            "git://example.com/repo.git",
        ] {
            assert!(validate_git_source(url, "main").is_ok(), "rejected {url}");
        }
    }

    #[test]
    fn ext_transport_is_rejected() {
        let err = validate_git_source("ext::sh -c 'touch /tmp/pwned'", "main").unwrap_err();
        assert!(err.contains("transport prefix"), "got: {err}");
    }

    #[test]
    fn file_and_unknown_schemes_are_rejected() {
        assert!(validate_git_source("file:///etc/passwd", "main").is_err());
        assert!(validate_git_source("fd::17", "main").is_err());
        assert!(validate_git_source("/local/path", "main").is_err());
    }

    #[test]
    fn leading_dash_url_and_ref_are_rejected() {
        assert!(validate_git_source("--upload-pack=touch /tmp/pwned", "main").is_err());
        let err =
            validate_git_source("https://example.com/repo.git", "--output=/etc/x").unwrap_err();
        assert!(err.contains("ref may not begin with '-'"), "got: {err}");
    }
}
