//! End-to-end pipeline: source → AST → resolution → types → HIR →
//! MIR → Cranelift text → linked artifact.

#![forbid(unsafe_code)]

use gossamer_codegen_cranelift::{
    CompileOptions, NativeObject, compile_to_object, compile_to_object_with_options,
    emit_module,
};
use anyhow::anyhow;
use gossamer_hir::{lift_closures, lower_source_file};
use gossamer_lex::SourceMap;
use gossamer_mir::{Body, check_generic_layouts, lower_program, optimise};
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

use crate::link::{Artifact, LinkerOptions, TranslationUnit, link};

/// Compiles a single source buffer into a linked [`Artifact`].
#[must_use]
pub fn compile_source(source: &str, unit_name: &str, options: &LinkerOptions) -> Artifact {
    let bodies = lower_to_mir(source, unit_name);
    let module = emit_module(&bodies);
    let unit = TranslationUnit {
        name: unit_name.to_string(),
        module,
    };
    link(&[unit], options)
}

/// Compiles `source` into a native object file suitable for linking
/// with `cc`. Falls back to `Err` when the MIR contains constructs
/// the Cranelift backend cannot yet lower (closures, slices, …).
pub fn compile_source_native(source: &str, unit_name: &str) -> anyhow::Result<NativeObject> {
    let (bodies, tcx) = lower_to_mir_with_tcx(source, unit_name);
    enforce_generic_abi(&bodies, &tcx)?;
    compile_to_object(&bodies, &tcx)
}

/// `--release` build path: lower through the LLVM backend
/// (text IR + `llc -O3`) for release-quality optimisation.
/// Falls back to `Err(BuildKind::Unsupported)`-wrapped errors
/// on MIR shapes the LLVM lowerer doesn't yet cover, which the
/// CLI can translate into a clear "drop `--release` to build
/// via Cranelift" message for the user.
pub fn compile_source_native_release(
    source: &str,
    unit_name: &str,
) -> anyhow::Result<NativeObject> {
    let (bodies, tcx) = lower_to_mir_with_tcx(source, unit_name);
    enforce_generic_abi(&bodies, &tcx)?;
    let llvm_obj = gossamer_codegen_llvm::compile_to_object(&bodies, &tcx)?;
    Ok(NativeObject {
        triple: llvm_obj.triple,
        bytes: llvm_obj.bytes,
    })
}

/// Result of a per-function fallback release build.
///
/// `llvm` always carries the LLVM-lowered subset of the
/// program. `cranelift` is `Some(_)` only when at least one
/// body fell back; the linker step combines both objects.
#[derive(Debug, Clone)]
pub struct ReleaseBuild {
    /// Object emitted by the LLVM backend.
    pub llvm: NativeObject,
    /// Cranelift-emitted companion object containing the
    /// bodies LLVM rejected. Empty when LLVM lowered every
    /// body in the program.
    pub cranelift: Option<NativeObject>,
    /// Names of bodies that fell back. Useful for diagnostics.
    pub fallback_bodies: Vec<String>,
}

/// Per-function fallback release build. Bodies the LLVM
/// lowerer rejects are routed to Cranelift; both objects are
/// returned so the CLI can pass them to `cc` together.
pub fn compile_source_native_release_with_fallback(
    source: &str,
    unit_name: &str,
) -> anyhow::Result<ReleaseBuild> {
    let (bodies, tcx) = lower_to_mir_with_tcx(source, unit_name);
    enforce_generic_abi(&bodies, &tcx)?;
    let outcome = gossamer_codegen_llvm::compile_with_fallback(&bodies, &tcx)?;
    let cranelift = if outcome.fallback_bodies.is_empty() {
        None
    } else {
        // Pass every body in the program to Cranelift so call
        // sites and `Operand::FnRef` for non-fallback bodies
        // (e.g. a fallback `main` that calls an
        // LLVM-lowered helper) still resolve to a declared
        // function id. Use `define_only` so Cranelift emits
        // bodies for the fallback set and `Linkage::Import`
        // declarations for the rest — the linker stitches them
        // back to the LLVM-built primary.
        //
        // The companion also matches the LLVM module's
        // expectation: it renames user `main` to `gos_main`
        // and emits the C-ABI shim itself. Tell Cranelift to
        // do the same rename and to skip its own shim so the
        // linker sees exactly one `main`.
        let options = CompileOptions {
            main_symbol_override: Some("gos_main".to_string()),
            omit_c_main_shim: true,
            define_only: Some(outcome.fallback_bodies.clone()),
        };
        Some(compile_to_object_with_options(&bodies, &tcx, options)?)
    };
    Ok(ReleaseBuild {
        llvm: NativeObject {
            triple: outcome.object.triple,
            bytes: outcome.object.bytes,
        },
        cranelift,
        fallback_bodies: outcome.fallback_bodies,
    })
}

fn lower_to_mir(source: &str, unit_name: &str) -> Vec<Body> {
    lower_to_mir_with_tcx(source, unit_name).0
}

/// Surfaces the Tier B6.3 generic-ABI check as an `anyhow::Error`
/// so the CLI's existing `Err`-render path prints a clean
/// diagnostic. Compiled paths (`compile_source_native`,
/// `compile_source_native_release`,
/// `compile_source_native_release_with_fallback`) all gate on
/// this before handing bodies to a backend.
fn enforce_generic_abi(bodies: &[Body], tcx: &TyCtxt) -> anyhow::Result<()> {
    let errors = check_generic_layouts(bodies, tcx);
    if errors.is_empty() {
        return Ok(());
    }
    Err(anyhow!(errors.join("\n")))
}

/// Same as [`lower_to_mir`], but returns the [`TyCtxt`] alongside
/// the MIR bodies so downstream passes that need type information
/// (e.g. the native codegen's primitive-type classification) can
/// walk `body.local_ty(local)` back into the kind table.
fn lower_to_mir_with_tcx(source: &str, unit_name: &str) -> (Vec<Body>, TyCtxt) {
    let mut map = SourceMap::new();
    let file = map.add_file(unit_name, source.to_string());
    let (sf, _parse_diags) = parse_source_file(source, file);
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let hir = lift_closures(hir);
    let mut bodies = lower_program(&hir, &mut tcx);
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    for body in &mut bodies {
        optimise(body);
    }
    (bodies, tcx)
}
