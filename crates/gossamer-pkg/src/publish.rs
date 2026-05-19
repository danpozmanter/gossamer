//! Publish flow: pack a project into a deterministic tar archive,
//! compute its sha256, optionally sign with ed25519, and upload to
//! the registry's `v1/upload` endpoint.
//!
//! Determinism rules for the tar buffer (sourced from the `tar`
//! module's `pack` helper):
//!
//! - File entries are emitted in lexicographic order.
//! - mtime / uid / gid are zeroed.
//! - Mode is normalised to `0o644`.
//! - End-of-archive marker is two 512-byte zero blocks.
//!
//! Two runs of `pack_crate` on the same source tree produce bytewise
//! identical archives, so the sha256 is stable across machines.

#![allow(
    clippy::map_unwrap_or,
    clippy::doc_markdown,
    clippy::elidable_lifetime_names,
    clippy::type_complexity,
    clippy::must_use_candidate
)]
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use thiserror::Error;

use crate::sha256;
use crate::tar;
use crate::transport::{Transport, TransportError};

/// Errors raised by [`pack_crate`].
#[derive(Debug, Clone, thiserror::Error, PartialEq, Eq)]
pub enum PackError {
    /// Underlying filesystem error.
    #[error("io reading {path}: {reason}")]
    Io {
        /// Path being read.
        path: String,
        /// Stringified io::Error.
        reason: String,
    },
    /// `project.toml` is missing from the project root.
    #[error("project root {0} has no project.toml")]
    MissingManifest(String),
    /// USTAR pack error (path too long / file too big).
    #[error("pack: {0}")]
    TarPack(String),
}

/// Errors raised by the publish flow above and beyond [`PackError`].
#[derive(Debug, Error)]
pub enum PublishError {
    /// Pack-time failure.
    #[error("pack: {0}")]
    Pack(#[from] PackError),
    /// Transport-level failure (DNS / TCP / TLS / status).
    #[error("transport: {0}")]
    Transport(#[from] TransportError),
    /// Caller-side validation / config failure.
    #[error("publish: {0}")]
    Config(String),
}

/// One packed crate ready to upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedArtifact {
    /// Deterministic tar buffer.
    pub bytes: Vec<u8>,
    /// SHA-256 (hex, lowercase) of `bytes`.
    pub sha256: String,
}

/// Walks `root` for every file (excluding `target/`, `.gos-cache/`,
/// `.git/`, `vendor/`, `.gos-bindings/`, hidden dotfiles), packs them
/// into a deterministic tar buffer, and returns the archive plus its
/// sha256.
///
/// The tar is *uncompressed* USTAR — the publish protocol leaves
/// compression to the registry's storage backend. This keeps the
/// digest verifiable from the on-the-wire bytes without a decompress
/// step, and matches the existing fetcher's straight-tar contract.
pub fn pack_crate(root: &Path) -> Result<PublishedArtifact, PackError> {
    let manifest_path = root.join("project.toml");
    if !manifest_path.is_file() {
        return Err(PackError::MissingManifest(root.display().to_string()));
    }
    let mut entries: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    walk(root, root, &mut entries)?;
    let bytes = tar::pack(&entries).map_err(|e| PackError::TarPack(e.to_string()))?;
    let sha = sha256::hex(&bytes);
    Ok(PublishedArtifact { bytes, sha256: sha })
}

fn walk(base: &Path, current: &Path, out: &mut BTreeMap<String, Vec<u8>>) -> Result<(), PackError> {
    if !current.is_dir() {
        return Ok(());
    }
    let mut paths: Vec<PathBuf> = std::fs::read_dir(current)
        .map_err(|e| PackError::Io {
            path: current.display().to_string(),
            reason: e.to_string(),
        })?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
    paths.sort();
    for path in paths {
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if should_skip(name) {
            continue;
        }
        if path.is_dir() {
            walk(base, &path, out)?;
        } else if path.is_file() {
            let key = path.strip_prefix(base).map_or_else(
                |_| path.display().to_string(),
                |p| p.to_string_lossy().replace('\\', "/"),
            );
            let bytes = std::fs::read(&path).map_err(|e| PackError::Io {
                path: path.display().to_string(),
                reason: e.to_string(),
            })?;
            out.insert(key, bytes);
        }
    }
    Ok(())
}

fn should_skip(name: &str) -> bool {
    matches!(
        name,
        "target"
            | "vendor"
            | ".git"
            | ".gos-cache"
            | ".gos-bindings"
            | "node_modules"
            | ".DS_Store"
    ) || name.ends_with(".rs.bk")
}

/// Per-publish metadata recorded by `gos publish`.
#[derive(Debug, Clone)]
pub struct PublishRequest<'a> {
    /// Project id being published (e.g. `example.com/widget`).
    pub project_id: &'a str,
    /// Version being published (`MAJOR.MINOR.PATCH`).
    pub version: &'a str,
    /// Deterministic tar payload + sha256.
    pub artifact: &'a PublishedArtifact,
    /// Optional ed25519 signature over `artifact.bytes`.
    pub signature: Option<[u8; 64]>,
    /// Optional ed25519 public key for the signature.
    pub public_key: Option<[u8; 32]>,
    /// Bearer token authenticating the upload.
    pub auth_token: Option<&'a str>,
}

/// Uploads `request` to `<registry_url>/v1/upload/<id>/<ver>` using
/// the given transport. The publish body is a tiny JSON wrapper
/// embedding the artifact (hex), the sha256, and optional
/// signature/public-key (both hex).
///
/// `transport` is expected to dispatch `request_with_body` for the
/// PUT/POST. The `Transport` trait only exposes `get`, so we route
/// the upload via the wrapper [`upload_with`].
pub fn upload_with(
    transport: &dyn UploadTransport,
    registry_url: &str,
    request: &PublishRequest<'_>,
) -> Result<(), PublishError> {
    let url = format!(
        "{base}/v1/upload/{id}/{version}",
        base = registry_url.trim_end_matches('/'),
        id = request.project_id,
        version = request.version,
    );
    let body = render_publish_body(request);
    transport
        .post(&url, body.as_bytes(), request.auth_token)
        .map_err(PublishError::Transport)?;
    Ok(())
}

fn render_publish_body(request: &PublishRequest<'_>) -> String {
    let signature_hex = request
        .signature
        .map(|s| crate::signing::hex_encode(&s))
        .unwrap_or_default();
    let public_key_hex = request
        .public_key
        .map(|k| crate::signing::hex_encode(&k))
        .unwrap_or_default();
    let artifact_hex = crate::signing::hex_encode(&request.artifact.bytes);
    format!(
        "{{\"id\":\"{id}\",\"version\":\"{ver}\",\"sha256\":\"{sha}\",\"signature\":\"{sig}\",\"public_key\":\"{pk}\",\"artifact\":\"{art}\"}}",
        id = request.project_id,
        ver = request.version,
        sha = request.artifact.sha256,
        sig = signature_hex,
        pk = public_key_hex,
        art = artifact_hex,
    )
}

/// Transport extension that supports POSTing a body with an optional
/// auth header. Implemented for both [`crate::transport::HttpsTransport`] and a test
/// double; kept as a separate trait so the registry-side mock can
/// inspect the body without going through the `Transport::get` API.
pub trait UploadTransport: Send + Sync {
    /// Sends `body` to `url` with an optional `Authorization: Bearer`
    /// header. Returns `Ok(())` on a 2xx response.
    fn post(&self, url: &str, body: &[u8], auth_token: Option<&str>) -> Result<(), TransportError>;
}

/// Generic upload transport backed by any `Transport` + plain HTTP/1.1.
/// Sends a `POST` with `Content-Type: application/json`. Used by the
/// CLI's publish path when the registry speaks the minimal upload
/// protocol.
///
/// The transport handles TLS / DNS / status-line parsing itself.
pub struct HttpUploader<'a> {
    /// Underlying transport — used as a TLS connector.
    pub transport: &'a dyn Transport,
}

impl UploadTransport for HttpUploader<'_> {
    fn post(&self, url: &str, body: &[u8], auth_token: Option<&str>) -> Result<(), TransportError> {
        // The `Transport::post` default impl rejects unsupported
        // transports; `HttpTransport` / `HttpsTransport` override
        // it with the rustls-backed POST shape (`Content-Type:
        // application/json`, optional `Authorization: Bearer`).
        self.transport
            .post(url, body, "application/json", auth_token)
            .map(|_response_body| ())
    }
}

/// In-memory upload transport for tests — records every POST in
/// order so assertions can inspect what would have been sent.
#[derive(Debug, Default)]
pub struct RecordingUploader {
    /// Captured POSTs: `(url, body, auth_token)`.
    pub posts: parking_lot::Mutex<Vec<(String, Vec<u8>, Option<String>)>>,
}

impl RecordingUploader {
    /// Returns a fresh recording uploader.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Snapshot of every recorded post.
    pub fn take_posts(&self) -> Vec<(String, Vec<u8>, Option<String>)> {
        std::mem::take(&mut *self.posts.lock())
    }
}

impl UploadTransport for RecordingUploader {
    fn post(&self, url: &str, body: &[u8], auth_token: Option<&str>) -> Result<(), TransportError> {
        self.posts.lock().push((
            url.to_string(),
            body.to_vec(),
            auth_token.map(str::to_string),
        ));
        Ok(())
    }
}

/// Yank a published version by POSTing to `<registry>/v1/yank/<id>/<ver>`.
/// `reason` is the optional human-readable explanation.
pub fn yank_with(
    transport: &dyn UploadTransport,
    registry_url: &str,
    project_id: &str,
    version: &str,
    reason: Option<&str>,
    auth_token: Option<&str>,
) -> Result<(), PublishError> {
    let url = format!(
        "{base}/v1/yank/{id}/{version}",
        base = registry_url.trim_end_matches('/'),
        id = project_id,
        version = version,
    );
    let body = format!(
        "{{\"reason\":\"{}\"}}",
        reason.unwrap_or("").replace('"', "\\\"")
    );
    transport.post(&url, body.as_bytes(), auth_token)?;
    Ok(())
}

/// Owner management: add / remove / list users authorised to publish
/// `project_id`. The registry endpoint is `v1/owners/<id>` with the
/// HTTP verb encoded in the body (POST + `{"op":"add"|"remove"}`).
pub fn owner_op_with(
    transport: &dyn UploadTransport,
    registry_url: &str,
    project_id: &str,
    op: &str,
    user: Option<&str>,
    auth_token: Option<&str>,
) -> Result<(), PublishError> {
    let url = format!(
        "{base}/v1/owners/{id}",
        base = registry_url.trim_end_matches('/'),
        id = project_id,
    );
    let body = format!(
        "{{\"op\":\"{op}\",\"user\":\"{}\"}}",
        user.unwrap_or("").replace('"', "\\\""),
    );
    transport.post(&url, body.as_bytes(), auth_token)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_crate_returns_deterministic_bytes() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("gossamer-pack-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(tmp.join("src")).unwrap();
        std::fs::write(tmp.join("project.toml"), b"[project]\nid = \"a.b/c\"\n").unwrap();
        std::fs::write(tmp.join("src/main.gos"), b"fn main() {}\n").unwrap();
        let a = pack_crate(&tmp).unwrap();
        let b = pack_crate(&tmp).unwrap();
        assert_eq!(a.bytes, b.bytes, "pack must be byte-stable");
        assert_eq!(a.sha256, b.sha256);
        assert_eq!(a.sha256.len(), 64);
        // Unpacking the produced tar yields back the original tree.
        let back = crate::tar::unpack(&a.bytes).unwrap();
        assert!(back.contains_key("project.toml"));
        assert!(back.contains_key("src/main.gos"));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn pack_crate_rejects_missing_manifest() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("gossamer-pack-empty-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        let err = pack_crate(&tmp).unwrap_err();
        assert!(matches!(err, PackError::MissingManifest(_)));
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn recording_uploader_captures_post() {
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("gossamer-pack-up-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(tmp.join("project.toml"), b"[project]\nid = \"a.b/c\"\n").unwrap();
        let artifact = pack_crate(&tmp).unwrap();
        let uploader = RecordingUploader::new();
        let req = PublishRequest {
            project_id: "a.b/c",
            version: "0.1.0",
            artifact: &artifact,
            signature: None,
            public_key: None,
            auth_token: Some("tkn"),
        };
        upload_with(&uploader, "https://reg.example.test", &req).unwrap();
        let posts = uploader.take_posts();
        assert_eq!(posts.len(), 1);
        assert_eq!(posts[0].0, "https://reg.example.test/v1/upload/a.b/c/0.1.0");
        assert_eq!(posts[0].2.as_deref(), Some("tkn"));
        let body = std::str::from_utf8(&posts[0].1).unwrap();
        assert!(body.contains("\"sha256\""));
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
