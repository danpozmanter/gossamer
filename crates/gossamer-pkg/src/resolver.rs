//! Minimum-version-selection (MVS) resolver per SPEC §16.4.
//! Each project declares a caret range for every dependency. The
//! resolver collects every consumer's range for each project id and
//! picks the smallest version that satisfies them all. This matches
//! Go modules' MVS behaviour: predictable, no surprise upgrades, no
//! search.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use thiserror::Error;

use crate::id::ProjectId;
use crate::manifest::{DependencySpec, InlineDependency, Manifest};
use crate::version::{CaretRange, Version};

/// Catalogue of every version known for a project. Tests inject a
/// catalogue directly; the production resolver populates it from the
/// fetcher.
#[derive(Debug, Clone, Default)]
pub struct VersionCatalogue {
    entries: BTreeMap<String, Vec<Version>>,
}

impl VersionCatalogue {
    /// Returns an empty catalogue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records that `id` is available at `version`.
    pub fn add(&mut self, id: &ProjectId, version: Version) {
        let bucket = self.entries.entry(id.as_str().to_string()).or_default();
        if !bucket.contains(&version) {
            bucket.push(version);
            bucket.sort();
        }
    }

    /// Returns every recorded version for `id`.
    #[must_use]
    pub fn versions(&self, id: &ProjectId) -> &[Version] {
        self.entries
            .get(id.as_str())
            .map_or(&[] as &[Version], |v| v.as_slice())
    }
}

/// Per-dependency declaration the resolver receives from a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Requirement {
    /// Project being depended on.
    pub id: ProjectId,
    /// Source kind. Inline (git/path/tarball) declarations are
    /// surfaced unchanged so [`Resolver::resolve`] can record them in
    /// the lockfile without consulting the version catalogue.
    pub spec: RequirementSpec,
}

/// Distilled form of [`DependencySpec`] for the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequirementSpec {
    /// Versioned registry dependency.
    Range(CaretRange),
    /// Pinned non-registry source.
    Inline(InlineDependency),
}

impl Requirement {
    /// Builds a requirement from a [`DependencySpec`].
    #[must_use]
    pub fn from_spec(id: ProjectId, spec: &DependencySpec) -> Self {
        let spec = match spec {
            DependencySpec::Registry(range) => RequirementSpec::Range(*range),
            DependencySpec::Inline(inline) => RequirementSpec::Inline(inline.clone()),
        };
        Self { id, spec }
    }
}

/// One row in the resolved dependency graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    /// Project being resolved.
    pub id: ProjectId,
    /// Concrete pin.
    pub pin: ResolvedSource,
}

/// Concrete source pin produced by the resolver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedSource {
    /// Registry version pin.
    Registry(Version),
    /// Git checkout pin.
    Git {
        /// Repository URL.
        url: String,
        /// Reference (tag/branch/rev).
        reference: String,
    },
    /// Local path pin.
    Path(String),
    /// Tarball pin.
    Tarball {
        /// Archive URL.
        url: String,
        /// sha256 of the archive.
        sha256: String,
    },
}

/// Resolution failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ResolveError {
    /// No version in the catalogue satisfies the union of requested
    /// ranges.
    #[error("no version of {id} satisfies the requested ranges")]
    Unsatisfiable {
        /// Project being resolved.
        id: String,
    },
    /// Two non-registry pins for the same project disagree.
    #[error("conflicting non-registry pins for {id}")]
    ConflictingPins {
        /// Project being resolved.
        id: String,
    },
}

/// MVS resolver entry point.
#[derive(Debug, Default)]
pub struct Resolver {
    catalogue: VersionCatalogue,
}

impl Resolver {
    /// Returns a resolver backed by `catalogue`.
    #[must_use]
    pub fn new(catalogue: VersionCatalogue) -> Self {
        Self { catalogue }
    }

    /// Resolves every dependency listed in `manifest` and returns the
    /// concrete pin per project. Inline dependencies pass through
    /// verbatim; registry dependencies pick the minimum version that
    /// satisfies the range.
    pub fn resolve(&self, manifest: &Manifest) -> Result<Vec<Resolved>, ResolveError> {
        let mut requirements: BTreeMap<String, (ProjectId, Vec<RequirementSpec>)> = BTreeMap::new();
        for (raw_id, spec) in &manifest.dependencies {
            let id = ProjectId::parse(raw_id)
                .map_err(|_| ResolveError::Unsatisfiable { id: raw_id.clone() })?;
            let req = Requirement::from_spec(id.clone(), spec);
            let entry = requirements
                .entry(raw_id.clone())
                .or_insert_with(|| (id.clone(), Vec::new()));
            entry.1.push(req.spec);
        }
        let mut resolved = Vec::with_capacity(requirements.len());
        for (raw_id, (id, specs)) in requirements {
            resolved.push(self.resolve_one(&raw_id, &id, &specs)?);
        }
        resolved.sort_by(|a, b| a.id.as_str().cmp(b.id.as_str()));
        Ok(resolved)
    }

    fn resolve_one(
        &self,
        raw_id: &str,
        id: &ProjectId,
        specs: &[RequirementSpec],
    ) -> Result<Resolved, ResolveError> {
        let inline_pins: Vec<&InlineDependency> = specs
            .iter()
            .filter_map(|s| match s {
                RequirementSpec::Inline(inline) => Some(inline),
                RequirementSpec::Range(_) => None,
            })
            .collect();
        if !inline_pins.is_empty() {
            if inline_pins.iter().any(|p| !inline_eq(p, inline_pins[0])) {
                return Err(ResolveError::ConflictingPins {
                    id: raw_id.to_string(),
                });
            }
            let pin = inline_pin_to_resolved(inline_pins[0]);
            return Ok(Resolved {
                id: id.clone(),
                pin,
            });
        }
        let ranges: Vec<&CaretRange> = specs
            .iter()
            .filter_map(|s| match s {
                RequirementSpec::Range(r) => Some(r),
                RequirementSpec::Inline(_) => None,
            })
            .collect();
        let candidates = self.catalogue.versions(id);
        for &version in candidates {
            if ranges.iter().all(|r| r.matches(version)) {
                return Ok(Resolved {
                    id: id.clone(),
                    pin: ResolvedSource::Registry(version),
                });
            }
        }
        Err(ResolveError::Unsatisfiable {
            id: raw_id.to_string(),
        })
    }
}

fn inline_pin_to_resolved(pin: &InlineDependency) -> ResolvedSource {
    match pin {
        InlineDependency::Git { url, reference } => ResolvedSource::Git {
            url: url.clone(),
            reference: reference.clone(),
        },
        InlineDependency::Path { path } => ResolvedSource::Path(path.clone()),
        InlineDependency::Tarball { url, sha256 } => ResolvedSource::Tarball {
            url: url.clone(),
            sha256: sha256.clone(),
        },
    }
}

fn inline_eq(a: &InlineDependency, b: &InlineDependency) -> bool {
    a == b
}
