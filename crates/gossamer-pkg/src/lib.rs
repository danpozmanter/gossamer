//! Package manager for Gossamer.
//!
//! Reads `project.toml`, resolves transitive dependencies, fetches
//! every source kind into a content-addressed cache (with optional
//! disk persistence), pins the result to `project.lock`, and drives
//! the `gos publish` / `gos yank` flow for the registry.

#![forbid(unsafe_code)]

pub mod cache;
pub mod credentials;
pub mod edit;
pub mod fetch;
pub mod id;
pub mod lockfile;
pub mod manifest;
pub mod publish;
pub mod resolver;
pub mod scaffold;
pub mod sha256;
pub mod signing;
pub mod tar;
pub mod transport;
pub mod version;

pub use cache::{Cache, CacheError, CachedSource, Fetched, default_cache_root};
pub use credentials::{Credential, CredentialStore, CredentialStoreError};
pub use edit::{add_registry, pin_to_resolved, remove, tidy};
pub use fetch::{DEFAULT_REGISTRY_URL, FetchOptions, Fetcher, vendor};
pub use id::{ProjectId, ProjectIdError};
pub use lockfile::{LOCKFILE_FILENAME, LOCKFILE_HEADER, LockedEntry, Lockfile, LockfileError};
pub use manifest::{
    DependencySpec, GitRef, InlineDependency, Manifest, ManifestError, ProjectTable,
    RustBindingSpec, find_manifest,
};
pub use publish::{PackError, PublishError, PublishedArtifact, pack_crate};
pub use resolver::{
    CacheBackedLoader, CatalogueEntry, FnLoader, NoopLoader, Requirement, RequirementSpec,
    ResolveError, Resolved, ResolvedSource, Resolver, TransitiveLoader, VersionCatalogue,
    resolve_transitive,
};
pub use scaffold::{render_initial_manifest, render_main_source};
pub use signing::{SigningError, SigningKey, VerifyingKey, sign_bytes, verify_bytes};
pub use transport::{
    HttpTransport, HttpsTransport, StaticTransport, Transport, TransportError, fetch_verified,
};
pub use version::{CaretRange, Version, VersionError};
