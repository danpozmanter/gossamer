//! In-memory manifest mutators behind `gos add` / `gos remove` /
//! `gos tidy`.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use crate::id::ProjectId;
use crate::manifest::{DependencySpec, Manifest};
use crate::resolver::{Resolved, ResolvedSource};
use crate::version::VersionReq;

/// Inserts a registry dependency on `id` at `requirement`. Returns
/// `true` when the manifest changed.
///
/// The requirement is taken rather than built here, because a bare
/// version and a `^` version mean different things and only the caller
/// knows which the user asked for.
pub fn add_registry(manifest: &mut Manifest, id: &ProjectId, requirement: VersionReq) -> bool {
    let key = id.as_str().to_string();
    let new_spec = DependencySpec::Registry(requirement);
    match manifest.dependencies.get(&key) {
        Some(existing) if existing == &new_spec => false,
        _ => {
            manifest.dependencies.insert(key, new_spec);
            true
        }
    }
}

/// Removes the dependency on `id`. Returns `true` if it was present.
pub fn remove(manifest: &mut Manifest, id: &ProjectId) -> bool {
    manifest.dependencies.remove(id.as_str()).is_some()
}

/// Drops every dependency that no entry in `keep` references. Used by
/// `gos tidy` after the resolver computes the actual closure.
pub fn tidy(manifest: &mut Manifest, keep: &[Resolved]) {
    let kept: BTreeSet<String> = keep.iter().map(|r| r.id.as_str().to_string()).collect();
    manifest.dependencies.retain(|k, _| kept.contains(k));
}

/// Rewrites the manifest entry for `id` as an exact pin on the version
/// the resolver selected. No-op for inline dependencies.
pub fn pin_to_resolved(manifest: &mut Manifest, resolved: &Resolved) {
    if let ResolvedSource::Registry(version) = &resolved.pin {
        let spec = DependencySpec::Registry(VersionReq::exact(version.clone()));
        manifest
            .dependencies
            .insert(resolved.id.as_str().to_string(), spec);
    }
}
