//! In-memory manifest mutators behind `gos add` / `gos remove` /
//! `gos tidy`.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use crate::id::ProjectId;
use crate::manifest::{DependencySpec, Manifest};
use crate::resolver::{Resolved, ResolvedSource};
use crate::version::{CaretRange, Version};

/// Inserts a registry dependency on `id` at `version`. Returns `true`
/// when the manifest changed.
pub fn add_registry(manifest: &mut Manifest, id: &ProjectId, version: Version) -> bool {
    let key = id.as_str().to_string();
    let new_spec = DependencySpec::Registry(CaretRange::new(version));
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

/// Updates the manifest entry for `id` with the resolver pin so the
/// declared range matches the actually-selected version. No-op for
/// inline dependencies.
pub fn pin_to_resolved(manifest: &mut Manifest, resolved: &Resolved) {
    if let ResolvedSource::Registry(version) = &resolved.pin {
        let spec = DependencySpec::Registry(CaretRange::new(*version));
        manifest
            .dependencies
            .insert(resolved.id.as_str().to_string(), spec);
    }
}
