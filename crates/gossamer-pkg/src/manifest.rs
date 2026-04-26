//! `project.toml` parser.
//! Ships a deliberately small TOML reader covering exactly
//! the subset SPEC §6.4 / §16.1 specifies. Pulling in a full TOML
//! crate is overkill for the manifest grammar and would balloon the
//! workspace's dependency graph; the keys we accept are well-defined
//! enough that hand parsing stays manageable.

#![forbid(unsafe_code)]
#![allow(
    clippy::too_many_lines,
    clippy::format_push_string,
    clippy::needless_pass_by_value,
    clippy::implicit_clone
)]

use std::collections::BTreeMap;

use thiserror::Error;

use crate::id::{ProjectId, ProjectIdError};
use crate::version::{CaretRange, Version, VersionError};

/// Parsed `project.toml`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    /// `[project]` table.
    pub project: ProjectTable,
    /// `[dependencies]` map keyed by project id.
    pub dependencies: BTreeMap<String, DependencySpec>,
    /// `[registries]` map keyed by DNS prefix.
    pub registries: BTreeMap<String, String>,
}

/// `[project]` table contents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectTable {
    /// `project.id`.
    pub id: ProjectId,
    /// `project.version`.
    pub version: Version,
    /// `project.authors`. Empty when omitted.
    pub authors: Vec<String>,
    /// `project.license`. Empty string when omitted.
    pub license: String,
    /// `project.output` — optional override for the binary `gos
    /// build` writes. Relative paths resolve against the manifest's
    /// directory; absent falls back to the source stem.
    pub output: Option<String>,
}

/// One entry in `[dependencies]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencySpec {
    /// Bare version literal — registry source by default.
    Registry(CaretRange),
    /// Inline table form: `git`, `path`, or `tarball`.
    Inline(InlineDependency),
}

/// Inline-table dependency variants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InlineDependency {
    /// `{ git = "...", tag = "..." }`.
    Git {
        /// Repository URL.
        url: String,
        /// Tag, branch, or commit reference.
        reference: String,
    },
    /// `{ path = "..." }`.
    Path {
        /// Local filesystem path relative to the manifest.
        path: String,
    },
    /// `{ tarball = "...", sha256 = "..." }`.
    Tarball {
        /// HTTP(S) URL of the archive.
        url: String,
        /// Mandatory sha256 of the archive contents.
        sha256: String,
    },
}

/// Errors returned by [`Manifest::parse`].
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ManifestError {
    /// A required field was missing.
    #[error("missing required field {0}")]
    MissingField(&'static str),
    /// A field had the wrong type (e.g. expected string, found list).
    #[error("expected {expected} for {field}")]
    WrongType {
        /// Field name.
        field: String,
        /// Human-readable expected type.
        expected: &'static str,
    },
    /// A line could not be parsed.
    #[error("malformed line {line_no}: {line}")]
    Malformed {
        /// One-based line number.
        line_no: u32,
        /// Verbatim text of the offending line.
        line: String,
    },
    /// The project id failed validation.
    #[error("invalid project id: {0}")]
    BadId(#[from] ProjectIdError),
    /// The version literal failed validation.
    #[error("invalid version: {0}")]
    BadVersion(#[from] VersionError),
    /// An inline dependency table mixed incompatible keys.
    #[error("ambiguous dependency for {0}: pick at most one of git/path/tarball")]
    AmbiguousDependency(String),
}

/// Walks parent directories of `start` looking for a `project.toml`.
/// Returns the first match, or `None` if the filesystem root is
/// reached. `start` may be either a directory or a file (in which
/// case its parent is walked).
#[must_use]
pub fn find_manifest(start: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut cursor: std::path::PathBuf = if start.is_file() {
        start.parent()?.to_path_buf()
    } else {
        start.to_path_buf()
    };
    loop {
        let candidate = cursor.join("project.toml");
        if candidate.is_file() {
            return Some(candidate);
        }
        cursor = cursor.parent()?.to_path_buf();
    }
}

impl Manifest {
    /// Parses a `project.toml` document.
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let mut current_section: Option<String> = None;
        let mut project = RawTable::default();
        let mut deps: BTreeMap<String, DependencySpec> = BTreeMap::new();
        let mut registries: BTreeMap<String, String> = BTreeMap::new();
        for (i, raw_line) in source.lines().enumerate() {
            let line_no = u32::try_from(i + 1).expect("line overflow");
            let trimmed = strip_comment(raw_line).trim();
            if trimmed.is_empty() {
                continue;
            }
            if let Some(section) = trimmed.strip_prefix('[').and_then(|s| s.strip_suffix(']')) {
                current_section = Some(section.trim().to_string());
                continue;
            }
            let (key, value) =
                split_key_value(trimmed).ok_or_else(|| ManifestError::Malformed {
                    line_no,
                    line: raw_line.to_string(),
                })?;
            match current_section.as_deref() {
                Some("project") => project.insert(key.to_string(), value.to_string()),
                Some("dependencies") => {
                    let spec = parse_dependency_value(value, &key)?;
                    deps.insert(key.to_string(), spec);
                }
                Some("registries") => {
                    let url = parse_string(value).ok_or_else(|| ManifestError::WrongType {
                        field: format!("registries.{key}"),
                        expected: "string",
                    })?;
                    registries.insert(key.to_string(), url.to_string());
                }
                Some(other) => {
                    return Err(ManifestError::Malformed {
                        line_no,
                        line: format!("unknown section [{other}]"),
                    });
                }
                None => {
                    return Err(ManifestError::Malformed {
                        line_no,
                        line: raw_line.to_string(),
                    });
                }
            }
        }
        let id_text = project
            .get("id")
            .ok_or(ManifestError::MissingField("project.id"))?;
        let version_text = project
            .get("version")
            .ok_or(ManifestError::MissingField("project.version"))?;
        let id = ProjectId::parse(parse_string(id_text).ok_or(ManifestError::WrongType {
            field: "project.id".to_string(),
            expected: "string",
        })?)?;
        let version =
            Version::parse(parse_string(version_text).ok_or(ManifestError::WrongType {
                field: "project.version".to_string(),
                expected: "string",
            })?)?;
        let authors = project
            .get("authors")
            .map(|raw| parse_string_array(raw).unwrap_or_default())
            .unwrap_or_default();
        let license = project
            .get("license")
            .and_then(|raw| parse_string(raw).map(str::to_string))
            .unwrap_or_default();
        let output = project
            .get("output")
            .and_then(|raw| parse_string(raw).map(str::to_string));
        Ok(Self {
            project: ProjectTable {
                id,
                version,
                authors,
                license,
                output,
            },
            dependencies: deps,
            registries,
        })
    }

    /// Renders the manifest back to canonical TOML.
    #[must_use]
    pub fn render(&self) -> String {
        let mut out = String::new();
        out.push_str("[project]\n");
        out.push_str(&format!("id = \"{}\"\n", self.project.id));
        out.push_str(&format!("version = \"{}\"\n", self.project.version));
        if !self.project.authors.is_empty() {
            out.push_str("authors = [");
            for (i, a) in self.project.authors.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&format!("\"{a}\""));
            }
            out.push_str("]\n");
        }
        if !self.project.license.is_empty() {
            out.push_str(&format!("license = \"{}\"\n", self.project.license));
        }
        if let Some(output) = &self.project.output {
            out.push_str(&format!("output = \"{output}\"\n"));
        }
        if !self.dependencies.is_empty() {
            out.push_str("\n[dependencies]\n");
            for (id, spec) in &self.dependencies {
                out.push_str(&format!("\"{id}\" = {}\n", render_dependency(spec)));
            }
        }
        if !self.registries.is_empty() {
            out.push_str("\n[registries]\n");
            for (prefix, url) in &self.registries {
                out.push_str(&format!("\"{prefix}\" = \"{url}\"\n"));
            }
        }
        out
    }
}

#[derive(Debug, Default)]
struct RawTable {
    entries: Vec<(String, String)>,
}

impl RawTable {
    fn insert(&mut self, key: String, value: String) {
        if let Some(slot) = self.entries.iter_mut().find(|(k, _)| k == &key) {
            slot.1 = value;
        } else {
            self.entries.push((key, value));
        }
    }
    fn get(&self, key: &str) -> Option<&String> {
        self.entries.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }
}

fn strip_comment(line: &str) -> &str {
    let bytes = line.as_bytes();
    let mut in_string = false;
    for (i, b) in bytes.iter().enumerate() {
        match *b {
            b'"' => in_string = !in_string,
            b'#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

fn split_key_value(line: &str) -> Option<(String, &str)> {
    let eq = line.find('=')?;
    let key_text = line[..eq].trim();
    let value_text = line[eq + 1..].trim();
    let key = if let Some(stripped) = key_text.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
        stripped.to_string()
    } else {
        key_text.to_string()
    };
    Some((key, value_text))
}

fn parse_string(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    trimmed.strip_prefix('"').and_then(|s| s.strip_suffix('"'))
}

fn parse_string_array(text: &str) -> Option<Vec<String>> {
    let trimmed = text.trim();
    let inner = trimmed
        .strip_prefix('[')
        .and_then(|s| s.strip_suffix(']'))?;
    if inner.trim().is_empty() {
        return Some(Vec::new());
    }
    let mut out = Vec::new();
    for piece in inner.split(',') {
        let piece = piece.trim();
        let value = parse_string(piece)?;
        out.push(value.to_string());
    }
    Some(out)
}

fn parse_dependency_value(value: &str, key: &str) -> Result<DependencySpec, ManifestError> {
    let trimmed = value.trim();
    if let Some(literal) = parse_string(trimmed) {
        return Ok(DependencySpec::Registry(CaretRange::parse(literal)?));
    }
    if let Some(table) = trimmed.strip_prefix('{').and_then(|s| s.strip_suffix('}')) {
        let mut git_url: Option<String> = None;
        let mut git_ref: Option<String> = None;
        let mut path: Option<String> = None;
        let mut tarball: Option<String> = None;
        let mut sha256: Option<String> = None;
        for entry in split_top_level_commas(table) {
            let (k, v) = split_key_value(entry.trim()).ok_or_else(|| ManifestError::Malformed {
                line_no: 0,
                line: entry.to_string(),
            })?;
            let v = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                field: format!("{key}.{k}"),
                expected: "string",
            })?;
            match k.as_str() {
                "git" => git_url = Some(v.to_string()),
                "tag" | "branch" | "rev" => git_ref = Some(v.to_string()),
                "path" => path = Some(v.to_string()),
                "tarball" => tarball = Some(v.to_string()),
                "sha256" => sha256 = Some(v.to_string()),
                other => {
                    return Err(ManifestError::Malformed {
                        line_no: 0,
                        line: format!("unknown dependency field {other}"),
                    });
                }
            }
        }
        let active = [git_url.is_some(), path.is_some(), tarball.is_some()]
            .iter()
            .filter(|b| **b)
            .count();
        if active != 1 {
            return Err(ManifestError::AmbiguousDependency(key.to_string()));
        }
        if let Some(url) = git_url {
            return Ok(DependencySpec::Inline(InlineDependency::Git {
                url,
                reference: git_ref.unwrap_or_else(|| "main".to_string()),
            }));
        }
        if let Some(path) = path {
            return Ok(DependencySpec::Inline(InlineDependency::Path { path }));
        }
        if let Some(url) = tarball {
            let sha256 = sha256.ok_or(ManifestError::WrongType {
                field: format!("{key}.sha256"),
                expected: "string (mandatory for tarball)",
            })?;
            return Ok(DependencySpec::Inline(InlineDependency::Tarball {
                url,
                sha256,
            }));
        }
    }
    Err(ManifestError::WrongType {
        field: key.to_string(),
        expected: "string version literal or inline-table dependency",
    })
}

fn split_top_level_commas(text: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let mut depth: i32 = 0;
    let mut start = 0;
    for (i, ch) in text.char_indices() {
        match ch {
            '{' | '[' => depth += 1,
            '}' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                out.push(&text[start..i]);
                start = i + 1;
            }
            _ => {}
        }
    }
    if start < text.len() {
        out.push(&text[start..]);
    }
    out
}

fn render_dependency(spec: &DependencySpec) -> String {
    match spec {
        DependencySpec::Registry(range) => format!("\"{}\"", range.minimum),
        DependencySpec::Inline(InlineDependency::Git { url, reference }) => {
            format!("{{ git = \"{url}\", tag = \"{reference}\" }}")
        }
        DependencySpec::Inline(InlineDependency::Path { path }) => {
            format!("{{ path = \"{path}\" }}")
        }
        DependencySpec::Inline(InlineDependency::Tarball { url, sha256 }) => {
            format!("{{ tarball = \"{url}\", sha256 = \"{sha256}\" }}")
        }
    }
}
