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
    /// `[rust-bindings]` map keyed by Cargo crate name.
    pub rust_bindings: BTreeMap<String, RustBindingSpec>,
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

/// One entry in `[rust-bindings]` — a Rust crate to statically
/// link into the per-project runner / compiled binary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RustBindingSpec {
    /// `{ path = "..." }` — local Cargo path-dep.
    Path {
        /// Optional informational version range.
        version: Option<CaretRange>,
        /// Path as written in the manifest (relative to the
        /// manifest dir or absolute).
        path: String,
        /// Cargo features.
        features: Vec<String>,
        /// Whether `default-features` is enabled.
        default_features: bool,
    },
    /// `{ git = "..." }` — Cargo git-dep.
    Git {
        /// Optional informational version range.
        version: Option<CaretRange>,
        /// Repository URL.
        url: String,
        /// Optional reference (branch/tag/rev).
        reference: Option<GitRef>,
        /// Cargo features.
        features: Vec<String>,
        /// Whether `default-features` is enabled.
        default_features: bool,
    },
    /// `{ version = "..." }` — crates.io passthrough.
    Crates {
        /// Required version range.
        version: CaretRange,
        /// Cargo features.
        features: Vec<String>,
        /// Whether `default-features` is enabled.
        default_features: bool,
    },
}

/// Reference for a `git` rust-binding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GitRef {
    /// `branch = "..."`.
    Branch(String),
    /// `tag = "..."`.
    Tag(String),
    /// `rev = "..."`.
    Rev(String),
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
    /// A `[rust-bindings]` key violates the Cargo package-name regex.
    #[error("invalid rust-binding name {0:?}: must match [A-Za-z_][A-Za-z0-9_-]*")]
    BadBindingName(String),
    /// `[rust-bindings]` entry mixed `path`, `git`, and version-only.
    #[error("ambiguous rust-binding for {0}: pick exactly one of path/git/version")]
    AmbiguousRustBinding(String),
    /// `[rust-bindings]` git source mixed branch/tag/rev.
    #[error("rust-binding {0} git source: pick at most one of branch/tag/rev")]
    AmbiguousGitRef(String),
    /// `[rust-bindings]` crates.io entry missing a `version` value.
    #[error("rust-binding {0} from crates.io requires a version")]
    MissingBindingVersion(String),
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
        let mut rust_bindings: BTreeMap<String, RustBindingSpec> = BTreeMap::new();
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
                Some("rust-bindings") => {
                    if !is_valid_binding_name(&key) {
                        return Err(ManifestError::BadBindingName(key.clone()));
                    }
                    let spec = parse_rust_binding_value(value, &key)?;
                    rust_bindings.insert(key, spec);
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
            rust_bindings,
        })
    }

    /// SHA-256 of the canonicalised `[rust-bindings]` set, with
    /// path-deps resolved against `manifest_dir`. Used as the cache
    /// key for the per-project runner.
    #[must_use]
    pub fn rust_binding_fingerprint(&self, manifest_dir: &std::path::Path) -> [u8; 32] {
        use sha2::Digest;
        let mut hasher = sha2::Sha256::new();
        for (name, spec) in &self.rust_bindings {
            hasher.update(name.as_bytes());
            hasher.update(b"\0");
            for entry in canonical_binding_kv(spec, manifest_dir) {
                hasher.update(entry.as_bytes());
                hasher.update(b"\0");
            }
            hasher.update(b"\x1e");
        }
        let mut out = [0u8; 32];
        out.copy_from_slice(&hasher.finalize());
        out
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
        if !self.rust_bindings.is_empty() {
            out.push_str("\n[rust-bindings]\n");
            for (name, spec) in &self.rust_bindings {
                out.push_str(&format!("{name} = {}\n", render_rust_binding(spec)));
            }
        }
        out
    }
}

fn render_rust_binding(spec: &RustBindingSpec) -> String {
    let mut parts: Vec<String> = Vec::new();
    match spec {
        RustBindingSpec::Path {
            version,
            path,
            features,
            default_features,
        } => {
            if let Some(v) = version {
                parts.push(format!("version = \"{}\"", v.minimum));
            }
            parts.push(format!("path = \"{path}\""));
            push_features(&mut parts, features, *default_features);
        }
        RustBindingSpec::Git {
            version,
            url,
            reference,
            features,
            default_features,
        } => {
            if let Some(v) = version {
                parts.push(format!("version = \"{}\"", v.minimum));
            }
            parts.push(format!("git = \"{url}\""));
            if let Some(r) = reference {
                match r {
                    GitRef::Branch(b) => parts.push(format!("branch = \"{b}\"")),
                    GitRef::Tag(t) => parts.push(format!("tag = \"{t}\"")),
                    GitRef::Rev(r) => parts.push(format!("rev = \"{r}\"")),
                }
            }
            push_features(&mut parts, features, *default_features);
        }
        RustBindingSpec::Crates {
            version,
            features,
            default_features,
        } => {
            parts.push(format!("version = \"{}\"", version.minimum));
            push_features(&mut parts, features, *default_features);
        }
    }
    format!("{{ {} }}", parts.join(", "))
}

fn push_features(parts: &mut Vec<String>, features: &[String], default_features: bool) {
    if !features.is_empty() {
        let listed: Vec<String> = features.iter().map(|f| format!("\"{f}\"")).collect();
        parts.push(format!("features = [{}]", listed.join(", ")));
    }
    if !default_features {
        parts.push("default-features = false".to_string());
    }
}

fn canonical_binding_kv(spec: &RustBindingSpec, manifest_dir: &std::path::Path) -> Vec<String> {
    let mut entries: Vec<String> = Vec::new();
    match spec {
        RustBindingSpec::Path {
            version,
            path,
            features,
            default_features,
        } => {
            entries.push("kind=path".to_string());
            if let Some(v) = version {
                entries.push(format!("version={}", v.minimum));
            }
            let resolved = resolve_path(manifest_dir, path);
            entries.push(format!("path={}", resolved.display()));
            push_canonical_features(&mut entries, features, *default_features);
        }
        RustBindingSpec::Git {
            version,
            url,
            reference,
            features,
            default_features,
        } => {
            entries.push("kind=git".to_string());
            if let Some(v) = version {
                entries.push(format!("version={}", v.minimum));
            }
            entries.push(format!("url={url}"));
            if let Some(r) = reference {
                match r {
                    GitRef::Branch(b) => entries.push(format!("branch={b}")),
                    GitRef::Tag(t) => entries.push(format!("tag={t}")),
                    GitRef::Rev(r) => entries.push(format!("rev={r}")),
                }
            }
            push_canonical_features(&mut entries, features, *default_features);
        }
        RustBindingSpec::Crates {
            version,
            features,
            default_features,
        } => {
            entries.push("kind=crates".to_string());
            entries.push(format!("version={}", version.minimum));
            push_canonical_features(&mut entries, features, *default_features);
        }
    }
    entries.sort();
    entries
}

fn push_canonical_features(out: &mut Vec<String>, features: &[String], default_features: bool) {
    let mut sorted: Vec<String> = features.to_vec();
    sorted.sort();
    for f in sorted {
        out.push(format!("feature={f}"));
    }
    out.push(format!("default-features={default_features}"));
}

fn resolve_path(base: &std::path::Path, raw: &str) -> std::path::PathBuf {
    let p = std::path::Path::new(raw);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    }
}

fn is_valid_binding_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn parse_rust_binding_value(value: &str, key: &str) -> Result<RustBindingSpec, ManifestError> {
    let trimmed = value.trim();
    let Some(table) = trimmed.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
        return Err(ManifestError::WrongType {
            field: format!("rust-bindings.{key}"),
            expected: "inline table",
        });
    };
    let mut version: Option<CaretRange> = None;
    let mut path: Option<String> = None;
    let mut git: Option<String> = None;
    let mut branch: Option<String> = None;
    let mut tag: Option<String> = None;
    let mut rev: Option<String> = None;
    let mut features: Vec<String> = Vec::new();
    let mut default_features: bool = true;
    for entry in split_top_level_commas(table) {
        let entry = entry.trim();
        if entry.is_empty() {
            continue;
        }
        let (k, v) = split_key_value(entry).ok_or_else(|| ManifestError::Malformed {
            line_no: 0,
            line: entry.to_string(),
        })?;
        match k.as_str() {
            "version" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.version"),
                    expected: "string",
                })?;
                version = Some(CaretRange::parse(s)?);
            }
            "path" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.path"),
                    expected: "string",
                })?;
                path = Some(s.to_string());
            }
            "git" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.git"),
                    expected: "string",
                })?;
                git = Some(s.to_string());
            }
            "branch" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.branch"),
                    expected: "string",
                })?;
                branch = Some(s.to_string());
            }
            "tag" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.tag"),
                    expected: "string",
                })?;
                tag = Some(s.to_string());
            }
            "rev" => {
                let s = parse_string(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.rev"),
                    expected: "string",
                })?;
                rev = Some(s.to_string());
            }
            "features" => {
                features = parse_string_array(v).ok_or_else(|| ManifestError::WrongType {
                    field: format!("rust-bindings.{key}.features"),
                    expected: "array of strings",
                })?;
            }
            "default-features" | "default_features" => {
                let s = v.trim();
                default_features = match s {
                    "true" => true,
                    "false" => false,
                    _ => {
                        return Err(ManifestError::WrongType {
                            field: format!("rust-bindings.{key}.default-features"),
                            expected: "boolean",
                        });
                    }
                };
            }
            other => {
                return Err(ManifestError::Malformed {
                    line_no: 0,
                    line: format!("unknown rust-binding field {other}"),
                });
            }
        }
    }
    let active = [path.is_some(), git.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if active > 1 {
        return Err(ManifestError::AmbiguousRustBinding(key.to_string()));
    }
    let git_ref_count = [branch.is_some(), tag.is_some(), rev.is_some()]
        .iter()
        .filter(|b| **b)
        .count();
    if git_ref_count > 1 {
        return Err(ManifestError::AmbiguousGitRef(key.to_string()));
    }
    if let Some(path) = path {
        return Ok(RustBindingSpec::Path {
            version,
            path,
            features,
            default_features,
        });
    }
    if let Some(url) = git {
        let reference = if let Some(b) = branch {
            Some(GitRef::Branch(b))
        } else if let Some(t) = tag {
            Some(GitRef::Tag(t))
        } else {
            rev.map(GitRef::Rev)
        };
        return Ok(RustBindingSpec::Git {
            version,
            url,
            reference,
            features,
            default_features,
        });
    }
    let version = version.ok_or_else(|| ManifestError::MissingBindingVersion(key.to_string()))?;
    Ok(RustBindingSpec::Crates {
        version,
        features,
        default_features,
    })
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
