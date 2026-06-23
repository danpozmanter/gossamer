//! The single authoritative front-end gate.
//!
//! [`check_frontend`] runs parse + resolve + typecheck + exhaustiveness
//! under one fatal-error policy. `gos check`, `gos run`, `gos build`,
//! `gos test`, and `gos bench` all call it and treat a non-empty
//! diagnostic list identically: render the diagnostics and refuse to
//! proceed. Centralising the policy here is what keeps the gates from
//! drifting (`check` rejecting a program that `build` then miscompiles).
//!
//! The contract: anything `gos run` rejects dynamically or the LLVM
//! backend cannot lower must be rejected here, statically, on every
//! tier. `check` is the strongest gate, never a weaker one.

#![forbid(unsafe_code)]

use gossamer_ast::{ItemKind, SourceFile};
use gossamer_diagnostics::Diagnostic;
use gossamer_lex::FileId;
use gossamer_resolve::{ResolveError, resolve_source_file};
use gossamer_types::{ExhaustivenessError, TyCtxt, check_exhaustiveness, typecheck_source_file};

use crate::frontend_cache::{FrontendCacheKey, load_blob, mark_success, store_blob};
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
///
/// `source` must already carry the autoderive augmentation (the synthesized
/// `__gos_serde_*` free functions and the implicit-`main` folding); the CLI
/// applies `autoderive::augment_source` before calling.
#[must_use]
pub fn check_frontend(source: &str, file_id: FileId) -> FrontendOutcome {
    let cache_key = FrontendCacheKey::new(source, env!("CARGO_PKG_VERSION"));
    let trace = std::env::var_os("GOSSAMER_CACHE_TRACE").is_some();
    let (sf, parse_diags) = if let Some(cached) = load_blob::<SourceFile>(&cache_key) {
        if trace {
            eprintln!("cache: parse skipped for {}", cache_key.as_hex());
        }
        // The cached blob is the post-augmentation `SourceFile` stored after
        // a previous successful gate, so the implicit `fn main` is present.
        (cached, Vec::new())
    } else {
        gossamer_parse::autoderive::parse_with_autoderive(source, file_id)
    };

    let mut diagnostics: Vec<Diagnostic> = parse_diags
        .iter()
        .map(gossamer_parse::ParseDiagnostic::to_diagnostic)
        .collect();

    let (resolutions, resolve_diags) = resolve_source_file(&sf);
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

    let mut tcx = TyCtxt::new();
    let (table, type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    diagnostics.extend(
        type_diags
            .iter()
            .map(gossamer_types::TypeDiagnostic::to_diagnostic),
    );

    let exhaustive_diags = check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    for diag in &exhaustive_diags {
        if matches!(diag.error, ExhaustivenessError::NonExhaustive { .. }) {
            diagnostics.push(diag.to_diagnostic());
        }
    }

    if diagnostics.is_empty() {
        mark_success(&cache_key);
        store_blob(&cache_key, &sf);
    }

    FrontendOutcome {
        checked: CheckedFrontend {
            sf,
            resolutions,
            table,
            tcx,
        },
        diagnostics,
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
