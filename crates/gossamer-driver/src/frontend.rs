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
//!
//! An accepted pass publishes its complete result through
//! [`crate::frontend_cache`], and a later pass over unchanged inputs
//! restores it instead of re-running any stage.

#![forbid(unsafe_code)]

use gossamer_ast::{ItemKind, SourceFile};
use gossamer_diagnostics::Diagnostic;
use gossamer_lex::FileId;
use gossamer_pkg::Edition;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{
    ExhaustivenessError, TyCtxt, check_arena_escapes, check_exhaustiveness,
    typecheck_source_file_with_edition,
};
use std::time::{Duration, Instant};

use crate::frontend_cache::{
    CachedFrontend, cache_enabled, frontend_key, load_blob, store_frontend,
};
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
    /// Whether the whole front-end result was restored from the cache.
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
/// - **Resolve**: every emitted resolve diagnostic is fatal, so `check`
///   reports the same resolve set the LSP and the REPL do.
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
    let trace = std::env::var_os("GOSSAMER_CACHE_TRACE").is_some();
    let cache_key = cache_enabled().then(|| frontend_key(source, edition.as_str(), file_id));

    let restore_started = Instant::now();
    if let Some(key) = &cache_key
        && let Some(cached) = load_blob::<CachedFrontend>(key)
    {
        if trace {
            eprintln!("cache: frontend restored for {}", key.as_hex());
        }
        // Only a pass that produced zero diagnostics publishes a blob, so a
        // hit is proof the program was accepted under this exact key.
        return FrontendOutcome {
            checked: CheckedFrontend {
                edition,
                sf: cached.sf,
                resolutions: cached.resolutions,
                table: cached.table,
                tcx: cached.tcx,
            },
            diagnostics: Vec::new(),
            timings: FrontendTimings {
                parse: restore_started.elapsed(),
                parse_cache_hit: true,
                ..FrontendTimings::default()
            },
        };
    }

    let phase_started = Instant::now();
    let (sf, parse_diags) = gossamer_parse::autoderive::parse_with_autoderive(source, file_id);
    let parse = phase_started.elapsed();

    let mut diagnostics: Vec<Diagnostic> = parse_diags
        .iter()
        .map(gossamer_parse::ParseDiagnostic::to_diagnostic)
        .collect();

    let phase_started = Instant::now();
    let (resolutions, resolve_diags) = resolve_source_file(&sf);
    let resolve = phase_started.elapsed();
    // Every resolve diagnostic the resolver chose to emit is fatal. The
    // resolver already suppresses the one class that is not actionable
    // (names the parser fabricated during recovery), so admitting the rest
    // keeps `gos check` a superset of what the LSP and the REPL report.
    let in_scope = collect_top_level_names(&sf);
    diagnostics.extend(
        resolve_diags
            .iter()
            .map(|diag| diag.to_diagnostic(&in_scope)),
    );

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

    // The blob is the sole cache-validity marker, and publishing it is what
    // makes a later invocation's hit proof of acceptance. A rejected program
    // therefore never reaches the cache.
    let checked = CheckedFrontend {
        edition,
        sf,
        resolutions,
        table,
        tcx,
    };
    if diagnostics.is_empty()
        && let Some(key) = &cache_key
    {
        store_frontend(
            key,
            &checked.sf,
            &checked.resolutions,
            &checked.table,
            &checked.tcx,
        );
    }

    FrontendOutcome {
        checked,
        diagnostics,
        timings: FrontendTimings {
            parse,
            resolve,
            typecheck,
            exhaustiveness,
            arena_escape,
            parse_cache_hit: false,
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
