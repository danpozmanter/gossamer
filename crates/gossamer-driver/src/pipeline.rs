//! End-to-end pipeline: source → AST → resolution → types → HIR →
//! MIR → Cranelift text → linked artifact.

#![forbid(unsafe_code)]

use anyhow::anyhow;
use gossamer_ast::SourceFile;
use gossamer_codegen_cranelift::{
    CompileOptions, NativeObject, compile_to_object, compile_to_object_with_options, emit_module,
};
use gossamer_hir::{lift_closures, lower_source_file};
use gossamer_lex::SourceMap;
use gossamer_mir::{
    Body, check_generic_layouts, inline_general, inline_small_callees, inline_trivial_wrappers,
    lower_program, optimise,
};
use gossamer_resolve::Resolutions;
use gossamer_types::{TyCtxt, TypeTable};

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

/// Pre-parsed, resolved, and typechecked program bundled for reuse.
/// Created once by `gos build`'s validation pass and consumed by
/// the codegen path, avoiding a redundant frontend round-trip.
pub struct CheckedFrontend {
    /// Parsed AST.
    pub sf: SourceFile,
    /// Name-resolution map.
    pub resolutions: Resolutions,
    /// Type-inference result table.
    pub table: TypeTable,
    /// Accumulated type context (mutated during lowering).
    pub tcx: TyCtxt,
}

/// Compiles `source` into a native object file suitable for linking
/// with `cc`. Returns `Err` only on lower-level failures (generic-ABI
/// enforcement, cranelift module emission); the MIR lowerer itself
/// covers every HIR shape, so user-visible compiler gaps no longer
/// short-circuit through this path.
pub fn compile_source_native(source: &str, unit_name: &str) -> anyhow::Result<NativeObject> {
    let (bodies, tcx) = lower_to_mir_with_tcx(source, unit_name);
    enforce_generic_abi(&bodies, &tcx)?;
    compile_to_object(&bodies, &tcx)
}

/// Like [`compile_source_native`] but uses already-computed frontend
/// artifacts, skipping a second parse/resolve/typecheck round-trip.
pub fn compile_source_native_from_frontend(
    checked: CheckedFrontend,
) -> anyhow::Result<NativeObject> {
    let (bodies, tcx) = lower_to_mir_from_frontend(checked);
    enforce_generic_abi(&bodies, &tcx)?;
    compile_to_object(&bodies, &tcx)
}

/// Path-oriented Cranelift debug build. Same as
/// [`compile_source_native_from_frontend`] but writes the produced
/// object directly to `obj_out` and returns only the triple, so
/// the caller never holds the full object bytes in memory.
pub fn compile_source_native_from_frontend_at_path(
    checked: CheckedFrontend,
    obj_out: &std::path::Path,
) -> anyhow::Result<String> {
    let (bodies, tcx) = lower_to_mir_from_frontend(checked);
    enforce_generic_abi(&bodies, &tcx)?;
    gossamer_codegen_cranelift::compile_to_object_at_path(&bodies, &tcx, obj_out)
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

/// Like [`compile_source_native_release_with_fallback`] but uses
/// already-computed frontend artifacts.
pub fn compile_source_native_release_with_fallback_from_frontend(
    checked: CheckedFrontend,
) -> anyhow::Result<ReleaseBuild> {
    let (bodies, tcx) = lower_to_mir_from_frontend(checked);
    enforce_generic_abi(&bodies, &tcx)?;
    let outcome = gossamer_codegen_llvm::compile_with_fallback(&bodies, &tcx)?;
    let cranelift = if outcome.fallback_bodies.is_empty() {
        None
    } else {
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

/// Result of a path-oriented per-function fallback release build.
/// Contrast with [`ReleaseBuild`], which carries object bytes.
/// The path-oriented form keeps both objects on disk so peak RSS
/// is not pushed up by simultaneous LLVM + Cranelift `Vec<u8>`
/// retention.
#[derive(Debug, Clone)]
pub struct ReleaseBuildPaths {
    /// Triple the LLVM backend reported for the emitted objects.
    pub triple: String,
    /// Names of the bodies that fell back to Cranelift.
    pub fallback_bodies: Vec<String>,
    /// True iff the Cranelift companion object was actually written
    /// (i.e. there was at least one fallback body). When false the
    /// caller should skip the companion in the link step.
    pub has_cranelift_companion: bool,
    /// Paths to the LLVM-emitted per-body object files. One entry per
    /// non-fallback body; pass all of them to the linker alongside
    /// the optional Cranelift companion.
    pub llvm_objects: Vec<std::path::PathBuf>,
}

/// Path-oriented variant of
/// [`compile_source_native_release_with_fallback_from_frontend`].
///
/// The LLVM backend emits one object per non-fallback body into
/// `llvm_obj_dir` (P2 parallel compilation + P3 incremental cache).
/// If any bodies fell back to Cranelift, a single Cranelift companion
/// object is written to `cl_obj_out`. The caller links all objects
/// (both LLVM and the optional Cranelift companion) together.
pub fn compile_release_at_paths_from_frontend(
    checked: CheckedFrontend,
    llvm_obj_dir: &std::path::Path,
    cl_obj_out: &std::path::Path,
) -> anyhow::Result<ReleaseBuildPaths> {
    let (bodies, tcx) = lower_to_mir_from_frontend(checked);
    enforce_generic_abi(&bodies, &tcx)?;
    let (llvm_objects, triple, fallback_bodies) =
        gossamer_codegen_llvm::compile_with_fallback_at_path(&bodies, &tcx, llvm_obj_dir)?;
    let has_cranelift_companion = !fallback_bodies.is_empty();
    if has_cranelift_companion {
        let options = CompileOptions {
            main_symbol_override: Some("gos_main".to_string()),
            omit_c_main_shim: true,
            define_only: Some(fallback_bodies.clone()),
        };
        gossamer_codegen_cranelift::compile_to_object_at_path_with_options(
            &bodies, &tcx, cl_obj_out, options,
        )?;
    }
    Ok(ReleaseBuildPaths {
        triple,
        fallback_bodies,
        has_cranelift_companion,
        llvm_objects,
    })
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
        // declarations for the rest - the linker stitches them
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

/// HIR + MIR lowering from pre-computed frontend artifacts.
fn lower_to_mir_from_frontend(checked: CheckedFrontend) -> (Vec<Body>, TyCtxt) {
    let CheckedFrontend {
        sf,
        resolutions,
        table,
        mut tcx,
    } = checked;
    let hir = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let hir = lift_closures(hir, &mut tcx);
    let mut bodies = lower_program(&hir, &mut tcx);
    gossamer_mir::monomorphise(&mut bodies, &mut tcx);
    inline_trivial_wrappers(&mut bodies);
    inline_small_callees(&mut bodies);
    inline_general(&mut bodies);
    for body in &mut bodies {
        optimise(body, &tcx);
    }
    (bodies, tcx)
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
///
/// Runs the shared front-end gate ([`crate::frontend::check_frontend`])
/// rather than an independent parse/resolve/typecheck of its own, so
/// the single fatal-error policy is the only frontend in the tree.
/// This is the source-taking codegen entry point used by the legacy
/// `compile_source` artifact path and the package builder, both of
/// which validate the program through the gate before reaching codegen;
/// any residual diagnostics here would have been surfaced there.
fn lower_to_mir_with_tcx(source: &str, unit_name: &str) -> (Vec<Body>, TyCtxt) {
    let augmented = gossamer_parse::autoderive::augment_source(source);
    let mut map = SourceMap::new();
    let file = map.add_file(unit_name, augmented.clone());
    let outcome = crate::frontend::check_frontend(&augmented, file);
    lower_to_mir_from_frontend(outcome.checked)
}
