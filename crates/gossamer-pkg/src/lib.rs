//! Package manager for Gossamer (Phases 27–28).
//! Reads `project.toml`, validates project identifiers + versions,
//! resolves transitive dependencies via minimum-version-selection,
//! and emits a `project.lock`. layers on a content-
//! addressable cache and a fetcher that materialises path/git/
//! registry/tarball sources into the cache, plus a `vendor` writer.

#![forbid(unsafe_code)]

pub mod cache;
pub mod edit;
pub mod fetch;
pub mod id;
pub mod lockfile;
pub mod manifest;
pub mod resolver;
pub mod scaffold;
pub mod sha256;
pub mod tar;
pub mod transport;
pub mod version;

pub use cache::{Cache, CacheError, CachedSource, Fetched};
pub use edit::{add_registry, pin_to_resolved, remove, tidy};
pub use fetch::{FetchOptions, Fetcher, vendor};
pub use id::{ProjectId, ProjectIdError};
pub use lockfile::{LOCKFILE_HEADER, Lockfile, LockfileError};
pub use manifest::{
    DependencySpec, GitRef, InlineDependency, Manifest, ManifestError, ProjectTable,
    RustBindingSpec, find_manifest,
};
pub use resolver::{
    Requirement, RequirementSpec, ResolveError, Resolved, ResolvedSource, Resolver,
    VersionCatalogue,
};
pub use scaffold::{render_initial_manifest, render_main_source};
pub use transport::{
    HttpTransport, HttpsTransport, StaticTransport, Transport, TransportError, fetch_verified,
};
pub use version::{CaretRange, Version, VersionError};
