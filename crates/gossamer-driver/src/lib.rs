//! High-level orchestration of the Gossamer compiler pipeline.
//! Introduces the linker/static-assembly path: every upstream
//! crate is chained together by [`pipeline::compile_source`] and the
//! result is turned into a deterministic [`link::Artifact`] by
//! [`link::link`]. Later phases (package manager, cross-compilation)
//! hang new options off the shared [`link::LinkerOptions`].

// `deny` rather than `forbid` so the single FFI liveness probe in
// `binding_runner::pid_alive` (`libc::kill` / Win32 `OpenProcess`) can opt in
// via a scoped, documented `#[allow(unsafe_code)]`. Every other site stays
// unsafe-free.
#![deny(unsafe_code)]

pub mod binding_runner;
pub mod build;
pub mod frontend;
pub mod frontend_cache;
pub mod link;
pub mod macos_deployment;
pub mod pipeline;
pub mod target;

pub use binding_runner::{
    BindingRunner, BindingRunnerError, DumpedItem, DumpedModule, DumpedType,
    Profile as RunnerProfile, RenderedBinding, SignatureDump, StaticBindingsLib,
    parse_signature_dump,
};

pub use build::{
    BuildCache, BuildError, BuildGraph, BuildOutput, Crate, Profile, build_workspace,
    fingerprint as crate_fingerprint, fingerprint_all, timed,
};
pub use frontend::{FrontendOutcome, check_frontend, check_frontend_with_edition};
pub use frontend_cache::{
    FrontendCacheKey, cache_dir, load_blob, load_blob_in, raw_blob_path, raw_blob_path_in,
    store_blob, store_blob_in, store_raw, store_raw_in,
};
pub use link::{
    ARTIFACT_MAGIC, Artifact, LinkerOptions, Symbol, TargetTriple, TranslationUnit, fingerprint,
    link,
};
pub use pipeline::{
    CheckedFrontend, ReleaseBuild, ReleaseBuildPaths, compile_at_paths_from_frontend,
    compile_release_at_paths_from_frontend, compile_source, compile_source_native,
    compile_source_native_from_frontend, compile_source_native_from_frontend_at_path,
    compile_source_native_release, compile_source_native_release_with_fallback,
    compile_source_native_release_with_fallback_from_frontend,
};
pub use target::{
    ObjectFormat, PrebuiltRuntime, REGISTERED_TARGETS, TargetInfo, all_targets, lookup_target,
};
