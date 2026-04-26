//! `project.lock` writer + reader.
//! The lockfile records every transitive dependency's exact source so
//! later builds reproduce bit-for-bit. The format is intentionally
//! line-oriented TOML with one entry per dependency.

#![forbid(unsafe_code)]
#![allow(clippy::needless_pass_by_value)]

use std::collections::BTreeMap;
use std::fmt::Write;

use thiserror::Error;

use crate::id::ProjectId;
use crate::resolver::{Resolved, ResolvedSource};
use crate::version::Version;

/// Header magic for sanity-checking lockfiles.
pub const LOCKFILE_HEADER: &str = "# gossamer project.lock v1\n";

/// Errors raised by [`Lockfile::parse`].
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
}

/// Parsed lockfile.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockfile {
    /// Resolved entries in deterministic order.
    pub entries: Vec<Resolved>,
}

impl Lockfile {
    /// Builds a lockfile from a resolver output.
    #[must_use]
    pub fn from_resolved(resolved: &[Resolved]) -> Self {
        let mut entries = resolved.to_vec();
        entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Self { entries }
    }

    /// Renders the lockfile to canonical TOML.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str(LOCKFILE_HEADER);
        out.push('\n');
        for entry in &self.entries {
            let _ = writeln!(out, "[[project]]");
            let _ = writeln!(out, "id = \"{}\"", entry.id);
            match &entry.pin {
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
                    let _ = writeln!(out, "sha256 = \"{sha256}\"");
                }
            }
            out.push('\n');
        }
        out
    }

    /// Parses a lockfile previously produced by [`Self::render`].
    pub fn parse(source: &str) -> Result<Self, LockfileError> {
        let mut entries = Vec::new();
        let mut current: Option<BTreeMap<String, String>> = None;
        for raw in source.lines() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if line == "[[project]]" {
                if let Some(map) = current.take() {
                    entries.push(table_to_resolved(map)?);
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
            entries.push(table_to_resolved(map)?);
        }
        entries.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(Self { entries })
    }
}

fn table_to_resolved(map: BTreeMap<String, String>) -> Result<Resolved, LockfileError> {
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
            let sha256 = map.get("sha256").ok_or(LockfileError::MissingField {
                id: id.as_str().to_string(),
                field: "sha256",
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
    Ok(Resolved { id, pin })
}
