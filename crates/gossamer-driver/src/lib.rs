//! High-level orchestration of the Gossamer compiler pipeline.
//! Introduces the linker/static-assembly path: every upstream
//! crate is chained together by [`pipeline::compile_source`] and the
//! result is turned into a deterministic [`link::Artifact`] by
//! [`link::link`]. Later phases (package manager, cross-compilation)
//! hang new options off the shared [`link::LinkerOptions`].

#![forbid(unsafe_code)]

pub mod build;
pub mod frontend_cache;
pub mod link;
pub mod pipeline;
pub mod target;

pub use build::{
    BuildCache, BuildError, BuildGraph, BuildOutput, Crate, Profile, build_workspace,
    fingerprint as crate_fingerprint, fingerprint_all, timed,
};
pub use link::{
    ARTIFACT_MAGIC, Artifact, LinkerOptions, Symbol, TargetTriple, TranslationUnit, fingerprint,
    link,
};
pub use frontend_cache::{
    FrontendCacheKey, cache_dir, load_blob, load_blob_in, mark_success, mark_success_in,
    observe_hit, observe_hit_in, store_blob, store_blob_in,
};
pub use pipeline::{
    ReleaseBuild, compile_source, compile_source_native, compile_source_native_release,
    compile_source_native_release_with_fallback,
};
pub use target::{
    ObjectFormat, PrebuiltRuntime, REGISTERED_TARGETS, TargetInfo, all_targets, lookup_target,
};
