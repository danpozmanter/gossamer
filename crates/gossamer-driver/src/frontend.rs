//! The single authoritative front-end gate.
//!
//! [`check_frontend`] runs parse + resolve + typecheck + exhaustiveness
//! under one fatal-error policy. `gos check`, `gos`, `gos build`,
//! `gos test`, and `gos bench` all call it and treat a non-empty
//! diagnostic list identically: render the diagnostics and refuse to
//! proceed. Centralising the policy here is what keeps the gates from
//! drifting (`check` rejecting a program that `build` then miscompiles).
//!
//! The contract: anything `gos` rejects dynamically or the LLVM
//! backend cannot lower must be rejected here, statically, on every
//! tier. `check` is the strongest gate, never a weaker one.

#![forbid(unsafe_code)]

use gossamer_ast::{ItemKind, SourceFile};
use gossamer_diagnostics::Diagnostic;
use gossamer_lex::FileId;
use gossamer_pkg::Edition;
use gossamer_resolve::{ResolveError, resolve_source_file};
use gossamer_types::{
    ExhaustivenessError, TyCtxt, check_arena_escapes, check_exhaustiveness,
    typecheck_source_file_with_edition,
};
use std::time::{Duration, Instant};

use crate::frontend_cache::{FrontendCacheKey, load_blob, store_blob};
use crate::pipeline::CheckedFrontend;

/// Result of the shared front-end gate.
///
/// `diagnostics` carries every fatal diagnostic under the unified
/// policy, already lowered to the renderable [`Diagnostic`] form. An
/// empty list means the program is accepted; `checked` is always
/// populated so a caller that wants to keep lowering a partially-typed
/// program (the legacy infallible codegen entry points) still can.
pub struct FrontendOutcome {
    /// Parsed, resolved, and typechecked artifacts.
    pub checked: CheckedFrontend,
    /// Fatal diagnostics under the unified policy; empty == accepted.
    pub diagnostics: Vec<Diagnostic>,
    /// Wall-clock timings for the frontend stages that produced this outcome.
    pub timings: FrontendTimings,
}

/// Timings from one authoritative frontend pass.
#[derive(Clone, Copy, Debug, Default)]
pub struct FrontendTimings {
    /// Parse and parse-cache lookup time.
    pub parse: Duration,
    /// Name-resolution time.
    pub resolve: Duration,
    /// Typechecking time.
    pub typecheck: Duration,
    /// Match-exhaustiveness analysis time.
    pub exhaustiveness: Duration,
    /// Arena-escape analysis time.
    pub arena_escape: Duration,
    /// Whether the parsed source file was restored from the frontend cache.
    pub parse_cache_hit: bool,
}

impl FrontendOutcome {
    /// `true` when the program passed every front-end gate.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        self.diagnostics.is_empty()
    }
}

/// Runs the full front-end on already-augmented `source` and applies the
/// single fatal-error policy:
///
/// - **Parse**: every parse diagnostic is fatal.
/// - **Resolve**: `UnresolvedName`, `DuplicateItem`, and
///   `UnknownModulePath` (GR0005, the canonical-`std`-path check) are
///   fatal; lower-severity resolve diagnostics are not.
/// - **Type**: every type diagnostic is fatal.
/// - **Exhaustiveness**: a non-exhaustive `match` (GM0001) is fatal.
/// - **Arena escape**: a value allocated in an `arena { }` block that is
///   used after the block (GM0003) is fatal.
///
/// `source` must already carry the autoderive augmentation (the synthesized
/// `__gos_serde_*` free functions and the implicit-`main` folding); the CLI
/// applies `autoderive::augment_source` before calling.
#[must_use]
pub fn check_frontend(source: &str, file_id: FileId) -> FrontendOutcome {
    check_frontend_with_edition(source, file_id, Edition::E2026)
}

/// Runs the shared frontend under a project-selected language edition. The
/// current edition reaches cache partitioning and later lowering; eager 2026
/// remains the compatibility default for callers without a project manifest.
#[must_use]
pub fn check_frontend_with_edition(
    source: &str,
    file_id: FileId,
    edition: Edition,
) -> FrontendOutcome {
    let phase_started = Instant::now();
    let cache_key = FrontendCacheKey::new_with_context(
        source,
        env!("CARGO_PKG_VERSION"),
        &format!("edition={}", edition.as_str()),
    );
    let trace = std::env::var_os("GOSSAMER_CACHE_TRACE").is_some();
    let (sf, parse_diags, parsed_from_source) =
        if let Some(cached) = load_blob::<SourceFile>(&cache_key) {
            if trace {
                eprintln!("cache: parse skipped for {}", cache_key.as_hex());
            }
            // The cached blob is the post-augmentation `SourceFile` stored after
            // a previous successful gate, so the implicit `fn main` is present.
            (cached, Vec::new(), false)
        } else {
            let (parsed, diagnostics) =
                gossamer_parse::autoderive::parse_with_autoderive(source, file_id);
            (parsed, diagnostics, true)
        };
    let parse = phase_started.elapsed();

    let mut diagnostics: Vec<Diagnostic> = parse_diags
        .iter()
        .map(gossamer_parse::ParseDiagnostic::to_diagnostic)
        .collect();

    let phase_started = Instant::now();
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let resolve = phase_started.elapsed();
    let in_scope = collect_top_level_names(&sf);
    for diag in &resolve_diags {
        if matches!(
            diag.error,
            ResolveError::UnresolvedName { .. }
                | ResolveError::DuplicateItem { .. }
                | ResolveError::UnknownModulePath { .. }
        ) {
            diagnostics.push(diag.to_diagnostic(&in_scope));
        }
    }

    let phase_started = Instant::now();
    let mut tcx = TyCtxt::new();
    let (table, type_diags) =
        typecheck_source_file_with_edition(&sf, &resolutions, &mut tcx, edition);
    let typecheck = phase_started.elapsed();
    diagnostics.extend(
        type_diags
            .iter()
            .map(gossamer_types::TypeDiagnostic::to_diagnostic),
    );

    let phase_started = Instant::now();
    let exhaustive_diags = check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    let exhaustiveness = phase_started.elapsed();
    for diag in &exhaustive_diags {
        if matches!(diag.error, ExhaustivenessError::NonExhaustive { .. }) {
            diagnostics.push(diag.to_diagnostic());
        }
    }

    // Every arena-escape diagnostic is fatal: a value allocated in an
    // `arena { }` block that outlives it is a use-after-free, so it must
    // be rejected on every tier, exactly like a type error.
    let phase_started = Instant::now();
    for diag in check_arena_escapes(&sf, &resolutions, &table, &tcx) {
        diagnostics.push(diag.to_diagnostic());
    }
    let arena_escape = phase_started.elapsed();

    // The blob is the sole cache-validity marker. Rewriting it after a hit
    // used to add two atomic, fsync-backed writes (a redundant `.ok` marker
    // and the same AST) to every successful `gos`; that dominated small
    // process startup. Only a clean parse miss publishes a new advisory blob.
    if diagnostics.is_empty() && parsed_from_source {
        store_blob(&cache_key, &sf);
    }

    FrontendOutcome {
        checked: CheckedFrontend {
            edition,
            sf,
            resolutions,
            table,
            tcx,
        },
        diagnostics,
        timings: FrontendTimings {
            parse,
            resolve,
            typecheck,
            exhaustiveness,
            arena_escape,
            parse_cache_hit: !parsed_from_source,
        },
    }
}

/// Every top-level item name declared in `sf`, used to seed the
/// resolver's "did you mean ...?" suggestions when rendering an
/// unresolved-name diagnostic.
fn collect_top_level_names(sf: &SourceFile) -> Vec<&str> {
    sf.items
        .iter()
        .filter_map(|item| match &item.kind {
            ItemKind::Fn(decl) => Some(decl.name.name.as_str()),
            ItemKind::Struct(decl) => Some(decl.name.name.as_str()),
            ItemKind::Enum(decl) => Some(decl.name.name.as_str()),
            ItemKind::Trait(decl) => Some(decl.name.name.as_str()),
            ItemKind::TypeAlias(decl) => Some(decl.name.name.as_str()),
            ItemKind::Const(decl) => Some(decl.name.name.as_str()),
            ItemKind::Static(decl) => Some(decl.name.name.as_str()),
            ItemKind::Mod(decl) => Some(decl.name.name.as_str()),
            ItemKind::Impl(_) | ItemKind::AttrItem(_) => None,
        })
        .collect()
}
