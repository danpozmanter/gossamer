//! `project.lock` writer + reader.
//!
//! The lockfile records every transitive dependency's exact source
//! plus the sha256 of its fetched tree, so subsequent builds reproduce
//! bit-for-bit. The format is intentionally line-oriented TOML with
//! one `[[project]]` array entry per dependency.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::fmt::Write;
use std::path::Path;

use thiserror::Error;

use crate::cache::Fetched;
use crate::id::ProjectId;
use crate::resolver::{Resolved, ResolvedSource};
use crate::version::Version;

/// Header magic for sanity-checking lockfiles.
pub const LOCKFILE_HEADER: &str = "# gossamer project.lock v1\n";

/// Canonical filename written to a project root.
pub const LOCKFILE_FILENAME: &str = "project.lock";

/// Errors raised by [`Lockfile::parse`] and the verify helpers.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum LockfileError {
    /// Unexpected line format.
    #[error("malformed lockfile line: {0}")]
    Malformed(String),
    /// Required key missing for an entry.
    #[error("missing field {field} for {id}")]
    MissingField {
        /// Project id.
        id: String,
        /// Field name.
        field: &'static str,
    },
    /// Resolved entry has no matching lockfile entry.
    #[error("lockfile drift: {id} is not pinned in project.lock — run `gos fetch --update`")]
    MissingPin {
        /// Project id with no lockfile match.
        id: String,
    },
    /// Resolved entry's pin / digest does not match the lockfile.
    #[error(
        "lockfile drift for {id}: resolved {resolved}, lock has {locked}; run `gos fetch --update`"
    )]
    Drift {
        /// Project id.
        id: String,
        /// What the resolver picked.
        resolved: String,
        /// What the lockfile records.
        locked: String,
    },
    /// Lockfile required but missing on disk.
    #[error("lockfile required but missing at {path}; run `gos fetch` to create one")]
    LockfileMissing {
        /// Path the search ran against.
        path: String,
    },
}

/// One lockfile entry — a `Resolved` plus the sha256 of the fetched
/// source tree (when known). Path sources omit `sha256` because their
/// contents are read live from disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockedEntry {
    /// Resolver output for this dep.
    pub resolved: Resolved,
    /// SHA-256 of the cached source tree, hex. `None` for path sources.
    pub sha256: Option<String>,
    /// Hex ed25519 publisher key that signed a registry source. Pinned
    /// on first fetch; later fetches must present the same key. `None`
    /// for non-registry sources.
    pub owner_pubkey: Option<String>,
}

/// Parsed lockfile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockfile {
    /// Locked entries in deterministic id-sorted order.
    pub entries: Vec<LockedEntry>,
}

impl Lockfile {
    /// Builds a lockfile from a resolver output without digest info.
    /// Prefer [`Self::from_fetched`] when fetch output is available.
    #[must_use]
    pub fn from_resolved(resolved: &[Resolved]) -> Self {
        let mut entries: Vec<LockedEntry> = resolved
            .iter()
            .cloned()
            .map(|resolved| LockedEntry {
                resolved,
                sha256: None,
                owner_pubkey: None,
            })
            .collect();
        entries.sort_by(|a, b| a.resolved.id.as_str().cmp(b.resolved.id.as_str()));
        Self { entries }
    }

    /// Builds a lockfile from a fetched dependency set. Each entry's
    /// `sha256` is populated from the fetched source's content digest.
    #[must_use]
    pub fn from_fetched(fetched: &[Fetched]) -> Self {
        let mut entries: Vec<LockedEntry> = fetched
            .iter()
            .map(|f| LockedEntry {
                resolved: f.resolved.clone(),
                sha256: match &f.resolved.pin {
                    ResolvedSource::Path(_) => None,
                    _ => Some(f.source.digest.clone()),
                },
                owner_pubkey: f.owner_pubkey.clone(),
            })
            .collect();
        entries.sort_by(|a, b| a.resolved.id.as_str().cmp(b.resolved.id.as_str()));
        Self { entries }
    }

    /// Returns the pinned publisher keys (project id → hex public key)
    /// recorded in this lockfile, for the fetcher to enforce on the
    /// next fetch.
    #[must_use]
    pub fn pinned_keys(&self) -> BTreeMap<String, String> {
        self.entries
            .iter()
            .filter_map(|e| {
                e.owner_pubkey
                    .clone()
                    .map(|k| (e.resolved.id.as_str().to_string(), k))
            })
            .collect()
    }

    /// Reads the lockfile from the canonical `<project_root>/project.lock`
    /// path. `Ok(None)` when the file does not exist.
    pub fn load(project_root: &Path) -> Result<Option<Self>, LockfileError> {
        let path = project_root.join(LOCKFILE_FILENAME);
        match std::fs::read_to_string(&path) {
            Ok(text) => Ok(Some(Self::parse(&text)?)),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(LockfileError::Malformed(format!(
                "reading {}: {err}",
                path.display()
            ))),
        }
    }

    /// Reads the lockfile, returning [`LockfileError::LockfileMissing`]
    /// when absent. Use this for `--locked` enforcement.
    pub fn load_required(project_root: &Path) -> Result<Self, LockfileError> {
        match Self::load(project_root)? {
            Some(lock) => Ok(lock),
            None => Err(LockfileError::LockfileMissing {
                path: project_root.join(LOCKFILE_FILENAME).display().to_string(),
            }),
        }
    }

    /// Writes the lockfile to `<project_root>/project.lock`.
    pub fn write(&self, project_root: &Path) -> std::io::Result<()> {
        std::fs::write(project_root.join(LOCKFILE_FILENAME), self.render())
    }

    /// Renders the lockfile to canonical TOML.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(LOCKFILE_HEADER);
        out.push('\n');
        for entry in &self.entries {
            let _ = writeln!(out, "[[project]]");
            let _ = writeln!(out, "id = \"{}\"", entry.resolved.id);
            match &entry.resolved.pin {
                ResolvedSource::Registry(v) => {
                    let _ = writeln!(out, "source = \"registry\"");
                    let _ = writeln!(out, "version = \"{v}\"");
                }
                ResolvedSource::Git { url, reference } => {
                    let _ = writeln!(out, "source = \"git\"");
                    let _ = writeln!(out, "url = \"{url}\"");
                    let _ = writeln!(out, "ref = \"{reference}\"");
                }
                ResolvedSource::Path(path) => {
                    let _ = writeln!(out, "source = \"path\"");
                    let _ = writeln!(out, "path = \"{path}\"");
                }
                ResolvedSource::Tarball { url, sha256 } => {
                    let _ = writeln!(out, "source = \"tarball\"");
                    let _ = writeln!(out, "url = \"{url}\"");
                    let _ = writeln!(out, "tarball_sha256 = \"{sha256}\"");
                }
            }
            if let Some(d) = &entry.sha256 {
                let _ = writeln!(out, "sha256 = \"{d}\"");
            }
            if let Some(k) = &entry.owner_pubkey {
                let _ = writeln!(out, "owner_pubkey = \"{k}\"");
            }
            out.push('\n');
        }
        out
    }

    /// Parses a lockfile previously produced by [`Self::render`].
    pub fn parse(source: &str) -> Result<Self, LockfileError> {
        let mut entries: Vec<LockedEntry> = Vec::new();
        let mut current: Option<BTreeMap<String, String>> = None;
        for raw in source.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[project]]" {
                if let Some(map) = current.take() {
                    entries.push(table_to_locked(map)?);
                }
                current = Some(BTreeMap::new());
                continue;
            }
            let (key, value) = line
                .split_once('=')
                .ok_or_else(|| LockfileError::Malformed(line.to_string()))?;
            let key = key.trim().to_string();
            let value = value.trim().trim_matches('"').to_string();
            let map = current
                .as_mut()
                .ok_or_else(|| LockfileError::Malformed(line.to_string()))?;
            map.insert(key, value);
        }
        if let Some(map) = current.take() {
            entries.push(table_to_locked(map)?);
        }
        entries.sort_by(|a, b| a.resolved.id.as_str().cmp(b.resolved.id.as_str()));
        Ok(Self { entries })
    }

    /// Verifies that each entry in `resolved` has a matching pin in
    /// this lockfile. Drift is a hard error.
    pub fn verify_against(&self, resolved: &[Resolved]) -> Result<(), LockfileError> {
        let by_id: BTreeMap<&str, &LockedEntry> = self
            .entries
            .iter()
            .map(|e| (e.resolved.id.as_str(), e))
            .collect();
        for entry in resolved {
            let locked = by_id
                .get(entry.id.as_str())
                .ok_or_else(|| LockfileError::MissingPin {
                    id: entry.id.as_str().to_string(),
                })?;
            if locked.resolved.pin != entry.pin {
                return Err(LockfileError::Drift {
                    id: entry.id.as_str().to_string(),
                    resolved: format!("{:?}", entry.pin),
                    locked: format!("{:?}", locked.resolved.pin),
                });
            }
        }
        Ok(())
    }

    /// Verifies that each fetched entry matches the lockfile's pin
    /// *and* recorded sha256.
    pub fn verify_fetched(&self, fetched: &[Fetched]) -> Result<(), LockfileError> {
        let by_id: BTreeMap<&str, &LockedEntry> = self
            .entries
            .iter()
            .map(|e| (e.resolved.id.as_str(), e))
            .collect();
        for f in fetched {
            let locked =
                by_id
                    .get(f.resolved.id.as_str())
                    .ok_or_else(|| LockfileError::MissingPin {
                        id: f.resolved.id.as_str().to_string(),
                    })?;
            if locked.resolved.pin != f.resolved.pin {
                return Err(LockfileError::Drift {
                    id: f.resolved.id.as_str().to_string(),
                    resolved: format!("{:?}", f.resolved.pin),
                    locked: format!("{:?}", locked.resolved.pin),
                });
            }
            if let Some(d) = &locked.sha256
                && d != &f.source.digest
            {
                return Err(LockfileError::Drift {
                    id: f.resolved.id.as_str().to_string(),
                    resolved: format!("sha256={}", f.source.digest),
                    locked: format!("sha256={d}"),
                });
            }
        }
        Ok(())
    }
}

fn table_to_locked(map: BTreeMap<String, String>) -> Result<LockedEntry, LockfileError> {
    let id_text = map.get("id").ok_or(LockfileError::MissingField {
        id: String::new(),
        field: "id",
    })?;
    let id = ProjectId::parse(id_text).map_err(|_| LockfileError::Malformed(id_text.clone()))?;
    let source = map.get("source").ok_or(LockfileError::MissingField {
        id: id.as_str().to_string(),
        field: "source",
    })?;
    let pin = match source.as_str() {
        "registry" => {
            let version = map.get("version").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "version",
            })?;
            ResolvedSource::Registry(
                Version::parse(version).map_err(|_| LockfileError::Malformed(version.clone()))?,
            )
        }
        "git" => {
            let url = map.get("url").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "url",
            })?;
            let reference = map.get("ref").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "ref",
            })?;
            ResolvedSource::Git {
                url: url.clone(),
                reference: reference.clone(),
            }
        }
        "path" => {
            let path = map.get("path").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "path",
            })?;
            ResolvedSource::Path(path.clone())
        }
        "tarball" => {
            let url = map.get("url").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "url",
            })?;
            let sha256 = map
                .get("tarball_sha256")
                .or_else(|| map.get("sha256"))
                .ok_or(LockfileError::MissingField {
                    id: id.as_str().to_string(),
                    field: "tarball_sha256",
                })?;
            ResolvedSource::Tarball {
                url: url.clone(),
                sha256: sha256.clone(),
            }
        }
        other => {
            return Err(LockfileError::Malformed(format!("unknown source {other}")));
        }
    };
    let sha = match &pin {
        ResolvedSource::Tarball { .. } => map.get("tree_sha256").cloned(),
        _ => map.get("sha256").cloned(),
    };
    let owner_pubkey = map.get("owner_pubkey").cloned();
    Ok(LockedEntry {
        resolved: Resolved { id, pin },
        sha256: sha,
        owner_pubkey,
    })
}
