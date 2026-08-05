#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Real Cranelift-backed native codegen.
//! Lowers a slice of MIR [`Body`]s into a `cranelift-object` module
//! and serialises the result as ELF (or the host's equivalent object
//! format). Supported today:
//! - `fn main() -> i64` with integer arithmetic (`+`, `-`, `*`, `/`,
//!   `%`, `&`, `|`, `^`, `<<`, `>>`, unary `-`, `!`),
//! - integer constants,
//! - direct calls between lowered functions,
//! - `return` of an `i64`.
//!
//! A C-ABI shim `main(argc, argv) -> i32` is emitted automatically:
//! it calls the Gossamer `main` and truncates the `i64` result into
//! the process exit code, so the object file links through a
//! standard `cc` invocation.
//! Aggregates (tuples/arrays/structs), strings, closures, and
//! anything that needs a GC heap are not yet lowered - those
//! constructs fall back to [`crate::emit::emit_module`] for
//! inspection.

// Allow patterns the Cranelift lowering deliberately uses:
//   - `similar_names` fires on `print_str`/`print_i64`/etc.
//     intrinsic-name shadowing within the same arm. The
//     parallel naming makes the dispatch table readable.
//   - `many_single_char_names` fires on hot inner-loop locals
//     (`a`, `b`, `n`, `m`, `k`) where longer names would
//     overflow the 100-col limit.
//   - `items_after_statements` flags inline `extern "C"` decls
//     localised to the one helper that uses them. Hoisting them
//     to module scope spreads the FFI surface; localised wins.
//   - `too_many_lines` / `cognitive_complexity` fire on the
//     intrinsic-dispatch arm and the `lower_intrinsic_call`
//     match. Splitting either hides the one-arm-per-symbol
//     structure that makes the table grep-able.
//   - `unnecessary_wraps` flags helpers whose `Result` exists
//     so call sites can still `?` them once a future lowering
//     can fail.
//   - `if_chain_can_be_rewritten_with_match` would flatten
//     short `if let Some(x) = .. else if let Some(y) = ..`
//     chains into match-on-tuple-of-options that's strictly
//     uglier here.
//   - `doc_markdown` flags identifiers like `i64`, `f64`,
//     etc. in plain-prose docs. Backticking every numeric
//     type name in every comment is noise.
//   - `manual_debug_impl` flags `JitModule`'s `Debug` impl
//     (which deliberately omits the JIT module pointer to keep
//     debug output stable across runs).
#![forbid(unsafe_code)]
#![allow(clippy::comparison_chain)]

use std::collections::HashMap;

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, Function, GlobalValueData, InstBuilder, MemFlagsData, Signature,
    StackSlotData, StackSlotKind, UserExternalName, UserFuncName, condcodes::IntCC,
    immediates::Imm64, types,
};
use cranelift_codegen::isa::{CallConv, TargetFrontendConfig};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{Context, ir};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module, ModuleDeclarations};
use cranelift_object::{ObjectBuilder, ObjectModule};
use gossamer_mir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind, Terminator,
    UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};
use rayon::prelude::*;

use super::*;

pub(super) fn build_offline_module(
    module: &dyn Module,
    intrinsics: &IntrinsicContext,
    function_ids_by_name: &HashMap<String, FuncId>,
) -> OfflineModule {
    let frontend_config = module.target_config();
    let default_call_conv = module.isa().default_call_conv();
    let decls = module.declarations();
    let mut func_sigs: HashMap<u32, (Signature, bool)> = HashMap::new();
    let mut populate_fn = |func_id: FuncId| {
        func_sigs.entry(func_id.as_u32()).or_insert_with(|| {
            let decl = decls.get_function_decl(func_id);
            (decl.signature.clone(), decl.linkage.is_final())
        });
    };
    for &func_id in function_ids_by_name.values() {
        populate_fn(func_id);
    }
    for &func_id in intrinsics.externs.values() {
        populate_fn(func_id);
    }
    for &func_id in intrinsics.functions.values() {
        populate_fn(func_id);
    }
    let mut data_info: HashMap<u32, (bool, bool)> = HashMap::new();
    for &data_id in intrinsics.strings.values() {
        let decl = decls.get_data_decl(data_id);
        data_info.insert(data_id.as_u32(), (decl.linkage.is_final(), decl.tls));
    }
    OfflineModule {
        frontend_config,
        default_call_conv,
        func_sigs,
        data_info,
    }
}

#[derive(Clone, Debug)]
pub struct NativeObject {
    /// Target triple the object was produced for.
    pub triple: String,
    /// Serialised object bytes (ELF on Linux, Mach-O on macOS, …).
    pub bytes: Vec<u8>,
}

pub(crate) fn build_native_isa(
    pic: bool,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>> {
    // COFF (Windows) has no GOT. Under `is_pic` cranelift emits
    // `movq sym@GOTPCREL(%rip)` (a load *through* a GOT slot) for
    // every symbol address, but the object backend rewrites the
    // resulting `GotRelative` to a plain `Relative` reloc pointing
    // straight at the symbol - so the load reads the symbol's first
    // bytes as if they were its address, corrupting every string and
    // data reference. Mach-O and ELF resolve the GOT load correctly
    // (ELF via GOTPCRELX relaxation), so PIC stays on there; on COFF
    // we emit position-dependent code (direct `lea` / `Abs8`) which
    // the PE base-relocation table fixes up at load time.
    let pic = pic && cfg!(not(target_os = "windows"));
    let mut flag_builder = settings::builder();
    flag_builder
        .set("opt_level", "speed")
        .map_err(|e| anyhow!("flag opt_level: {e}"))?;
    flag_builder
        .set("is_pic", if pic { "true" } else { "false" })
        .map_err(|e| anyhow!("flag is_pic: {e}"))?;
    flag_builder
        .set("use_colocated_libcalls", "false")
        .map_err(|e| anyhow!("flag use_colocated_libcalls: {e}"))?;
    flag_builder
        .set("unwind_info", "false")
        .map_err(|e| anyhow!("flag unwind_info: {e}"))?;
    // Permit `i128` arguments / returns in the x64 ABI: the by-value two-word
    // `Result` / `Option` representation (and the `gos_rt_result_*` runtime
    // helpers) cross function boundaries as `i128`, which the bare x64 backend
    // rejects without this extension.
    flag_builder
        .set("enable_llvm_abi_extensions", "true")
        .map_err(|e| anyhow!("flag enable_llvm_abi_extensions: {e}"))?;
    let flags = settings::Flags::new(flag_builder);
    let isa_builder = cranelift_native::builder().map_err(|e| anyhow!("native isa: {e}"))?;
    let isa = isa_builder
        .finish(flags)
        .map_err(|e| anyhow!("native isa finish: {e}"))?;
    Ok(isa)
}

pub fn compile_to_object(bodies: &[Body], tcx: &TyCtxt) -> Result<NativeObject> {
    compile_to_object_with_options(bodies, tcx, CompileOptions::default())
}

#[derive(Default)]
pub struct CompileOptions {
    /// Symbol the user's `main` body should be exported under.
    /// `None` keeps the default `gossamer_main` rename. Set to
    /// `gos_main` for fallback companion mode.
    pub main_symbol_override: Option<String>,
    /// When `true`, the C-ABI `main(argc,argv)` shim is *not*
    /// emitted. Used for the fallback companion object since
    /// the LLVM-built primary already provides the shim.
    pub omit_c_main_shim: bool,
    /// Body names the lowerer should *define* in the emitted
    /// object. Bodies passed in but not listed here are merely
    /// declared (`Linkage::Import`) so the emitted code can
    /// take their address and call them while leaving the
    /// definition for an LLVM-built sibling object.
    /// `None` defines every passed body (the historical default).
    pub define_only: Option<Vec<String>>,
}

pub fn compile_to_object_with_options(
    bodies: &[Body],
    tcx: &TyCtxt,
    options: CompileOptions,
) -> Result<NativeObject> {
    let isa = build_native_isa(true)?;
    let triple = isa.triple().to_string();

    let builder = ObjectBuilder::new(
        isa,
        "gossamer".to_string().into_bytes(),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| anyhow!("object builder: {e}"))?;
    let mut module = ObjectModule::new(builder);

    let main_rename = options
        .main_symbol_override
        .as_deref()
        .unwrap_or("gossamer_main");
    let define_only_set: Option<HashSet<String>> =
        options.define_only.map(|v| v.into_iter().collect());
    let lowered = lower_program_full(
        &mut module,
        bodies,
        tcx,
        Some(main_rename),
        options.omit_c_main_shim,
        define_only_set.as_ref(),
        LoweringMode::Parallel,
    )?;

    if !options.omit_c_main_shim {
        if let Some(gos_main) = lowered.function_ids_by_name.get("main").copied() {
            emit_c_main_shim(&mut module, gos_main)?;
        }
    }

    let product = module.finish();
    let bytes = product.emit().map_err(|e| anyhow!("emit object: {e}"))?;
    Ok(NativeObject { triple, bytes })
}

pub fn compile_to_object_at_path(
    bodies: &[Body],
    tcx: &TyCtxt,
    obj_out: &std::path::Path,
) -> Result<String> {
    compile_to_object_at_path_with_options(bodies, tcx, obj_out, CompileOptions::default())
}

pub fn compile_to_object_at_path_with_options(
    bodies: &[Body],
    tcx: &TyCtxt,
    obj_out: &std::path::Path,
    options: CompileOptions,
) -> Result<String> {
    let isa = build_native_isa(true)?;
    let triple = isa.triple().to_string();

    let builder = ObjectBuilder::new(
        isa,
        "gossamer".to_string().into_bytes(),
        cranelift_module::default_libcall_names(),
    )
    .map_err(|e| anyhow!("object builder: {e}"))?;
    let mut module = ObjectModule::new(builder);

    let main_rename = options
        .main_symbol_override
        .as_deref()
        .unwrap_or("gossamer_main");
    let define_only_set: Option<HashSet<String>> =
        options.define_only.map(|v| v.into_iter().collect());
    let lowered = lower_program_full(
        &mut module,
        bodies,
        tcx,
        Some(main_rename),
        options.omit_c_main_shim,
        define_only_set.as_ref(),
        LoweringMode::Parallel,
    )?;

    if !options.omit_c_main_shim {
        if let Some(gos_main) = lowered.function_ids_by_name.get("main").copied() {
            emit_c_main_shim(&mut module, gos_main)?;
        }
    }

    let product = module.finish();
    if let Some(parent) = obj_out.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| anyhow!("creating {}: {e}", parent.display()))?;
    }
    let f = std::fs::File::create(obj_out)
        .map_err(|e| anyhow!("creating {}: {e}", obj_out.display()))?;
    let mut w = std::io::BufWriter::new(f);
    product
        .object
        .write_stream(&mut w)
        .map_err(|e| anyhow!("emit object: {e}"))?;
    use std::io::Write as _;
    w.flush()
        .map_err(|e| anyhow!("flushing {}: {e}", obj_out.display()))?;
    Ok(triple)
}

/// True when `ty` is the by-value two-word (`i128`) inline enum
/// representation: a `Result<T, E>` / `Option<T>` sentinel ADT or a
/// user enum classified inline by the typechecker. Such values cross
/// a function boundary as a packed `i128` (`[disc, payload]`), the
/// shape `gos_rt_result_new` / `gos_rt_result_disc` already use, so
/// the cranelift signature must declare the slot `I128` rather than a
/// pointer. (The same condition drives `type_slot_count`'s "= 2".)
pub(super) fn is_inline_two_word_ty(tcx: &TyCtxt, ty: gossamer_types::Ty) -> bool {
    matches!(
        tcx.kind_of(ty),
        TyKind::Adt { def, .. } if def.local == u32::MAX || def.local == u32::MAX - 1
    ) || tcx.is_inline_enum_ty(ty)
}

/// True when `body` returns a by-value aggregate occupying more than one flat
/// slot - a tuple, a fixed-size array, or a struct - the shape lowered through
/// the structural-return-value (sret) ABI: the caller passes a pointer to a
/// result slot sized to the aggregate as a hidden trailing argument and the
/// callee writes the result words there instead of heap-allocating a fresh
/// block per call. This keeps a hot aggregate-returning body (`build`, a
/// monomorphised `get() -> Point`) from leaking a block on every call - the
/// caller's slot is reused (a stack slot in a JIT caller, a stack buffer at the
/// VM trampoline). Inline two-word enums (`Result` / `Option` / inline user
/// enums) return as a packed `i128` register value, not through sret, so they
/// are excluded. Mirrors the LLVM tier's aggregate-return ABI.
pub(super) fn body_returns_sret_aggregate(body: &Body, tcx: &TyCtxt) -> bool {
    let ret = body.local_ty(Local::RETURN);
    if is_inline_two_word_ty(tcx, ret) {
        return false;
    }
    // One-word address-represented aggregates (a single-managed-field struct)
    // return through sret too: their locals may be stack-slot-backed, so a raw
    // one-word return could hand the caller a pointer into the dying frame,
    // and the sret copy (plus the fresh-block free) keeps hot
    // aggregate-returning loops from leaking a heap block per call.
    matches!(
        tcx.kind_of(ret),
        TyKind::Tuple(_) | TyKind::Array { .. } | TyKind::Adt { .. }
    ) && (type_slot_count(tcx, ret) > 1 || single_slot_addr_aggregate(tcx, ret))
}

pub(super) fn build_signature_from_types(
    module: &dyn Module,
    body: &Body,
    tcx: &TyCtxt,
    bct: &[Option<ir::Type>],
) -> Signature {
    let mut sig = module.make_signature();
    for pidx in 1..=body.arity {
        let local = Local(pidx);
        let ty = body.local_ty(local);
        let cl = if is_inline_two_word_ty(tcx, ty) {
            types::I128
        } else {
            bct.get(local.0 as usize)
                .copied()
                .flatten()
                .unwrap_or_else(|| cl_type_of(tcx, ty, module))
        };
        sig.params.push(AbiParam::new(cl));
    }
    let ret_ty = body.local_ty(Local::RETURN);
    let ret_cl = if is_inline_two_word_ty(tcx, ret_ty) {
        types::I128
    } else {
        bct.get(Local::RETURN.0 as usize)
            .copied()
            .flatten()
            .unwrap_or_else(|| cl_type_of(tcx, ret_ty, module))
    };
    sig.returns.push(AbiParam::new(ret_cl));
    // A by-value aggregate return uses the sret ABI: a hidden trailing pointer
    // arg names a caller-owned slot the callee fills, so no per-call heap block
    // is allocated. The body still returns that pointer (in the return register)
    // for callers that read the result directly.
    if body_returns_sret_aggregate(body, tcx) {
        sig.params
            .push(AbiParam::new(module.target_config().pointer_type()));
    }
    sig
}

pub(crate) fn lower_program(
    module: &mut dyn Module,
    bodies: &[Body],
    tcx: &TyCtxt,
    entry_symbol_for_main: Option<&str>,
) -> Result<LoweredProgram> {
    lower_program_with_linkage(module, bodies, tcx, entry_symbol_for_main, Linkage::Local)
}

/// JIT-specific lowerer: build CLIF serially so short `gos`
/// promotions do not instantiate rayon's global worker pool.
pub(crate) fn lower_program_serial(
    module: &mut dyn Module,
    bodies: &[Body],
    tcx: &TyCtxt,
    entry_symbol_for_main: Option<&str>,
) -> Result<LoweredProgram> {
    lower_program_full(
        module,
        bodies,
        tcx,
        entry_symbol_for_main,
        false,
        None,
        LoweringMode::Serial,
    )
}

/// Like [`lower_program`] but lets the caller pick the linkage
/// for user-defined functions. The fallback companion path
/// uses `Linkage::Export` so the LLVM-emitted primary object
/// can resolve user-function calls across the object boundary.
#[allow(
    dead_code,
    reason = "exposed for the LLVM fallback companion to opt into Export linkage"
)]
pub(crate) fn lower_program_with_linkage(
    module: &mut dyn Module,
    bodies: &[Body],
    tcx: &TyCtxt,
    entry_symbol_for_main: Option<&str>,
    linkage: Linkage,
) -> Result<LoweredProgram> {
    lower_program_full(
        module,
        bodies,
        tcx,
        entry_symbol_for_main,
        matches!(linkage, Linkage::Export),
        None,
        LoweringMode::Parallel,
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoweringMode {
    Parallel,
    Serial,
}

/// Internal lowerer with full per-body linkage / definition
/// control. `cross_object` toggles the `Export` linkage every
/// fallback-companion build needs; `define_only` (when `Some`)
/// limits which bodies are *defined* rather than declared as
/// `Import`.
pub(crate) fn lower_program_full(
    module: &mut dyn Module,
    bodies: &[Body],
    tcx: &TyCtxt,
    entry_symbol_for_main: Option<&str>,
    cross_object: bool,
    define_only: Option<&HashSet<String>>,
    mode: LoweringMode,
) -> Result<LoweredProgram> {
    if std::env::var("GOS_DUMP_MIR").is_ok() {
        for body in bodies {
            eprintln!("=== MIR {} ===", body.name);
            for (i, local) in body.locals.iter().enumerate() {
                eprintln!("  _{i}: {:?}", tcx.kind_of(local.ty));
            }
            for block in &body.blocks {
                eprintln!("  bb{}:", block.id.as_u32());
                for stmt in &block.stmts {
                    eprintln!("    {:?}", stmt.kind);
                }
                eprintln!("    term: {:?}", block.terminator);
            }
        }
    }

    // Refuse 128-bit integer types up front. Cranelift's pointer
    // width on x86-64 is 64 bits and the runtime print path only
    // covers i64/u64; silently truncating to i64 corrupts every
    // value with the high half set. Surfacing the limit at build
    // time matches the `i128_use_panics_native_build…` regression
    // gate.
    for body in bodies {
        for (idx, local) in body.locals.iter().enumerate() {
            if let TyKind::Int(IntTy::I128 | IntTy::U128) = tcx.kind_of(local.ty) {
                bail!(
                    "i128 / u128 are not supported by the compiled tier yet (in fn `{}`, local _{}); use the bytecode VM for now",
                    body.name,
                    idx
                );
            }
        }
    }

    // Declare every function up-front so call-sites can resolve.
    // We key the map by the resolver-assigned `DefId.local` so
    // `Operand::FnRef(def)` from MIR lowers to the right function
    // ref, with a by-name fallback for the rare body that has no
    // resolver id (synthesised closures).
    //
    // N1+C2: precompute one `body_cl_types` Vec per body and reuse
    // it for both the declaration-phase signature and the definition-
    // phase codegen. Avoids the O(body) HashMap scan being run twice
    // per function and eliminates the per-local `infer_body_cl_types`
    // calls that previously happened inside `ensure_var` / `define_var_to`.
    let mut function_ids_by_def: HashMap<u32, FuncId> = HashMap::new();
    let mut function_ids_by_name: HashMap<String, FuncId> = HashMap::new();
    let body_should_be_defined = |name: &str| -> bool {
        match define_only {
            Some(allowed) => allowed.contains(name),
            None => true,
        }
    };
    // Precompute one type-inference Vec per body. Kept in parallel
    // with `bodies` by index so the definition loop can look them up
    // without re-running inference.
    let body_type_vecs: Vec<Vec<Option<ir::Type>>> = bodies
        .iter()
        .map(|body| infer_body_cl_types(body, tcx, &*module))
        .collect();
    for (body, bct) in bodies.iter().zip(body_type_vecs.iter()) {
        let signature = build_signature_from_types(&*module, body, tcx, bct);
        let symbol = if body.name == "main" {
            entry_symbol_for_main.map_or_else(|| body.name.clone(), str::to_string)
        } else {
            body.name.clone()
        };
        let lk = if body_should_be_defined(&body.name) {
            if cross_object {
                Linkage::Export
            } else {
                Linkage::Local
            }
        } else {
            // Body is referenced (call-site, address-of) but
            // its body lives in a sibling object - declare as
            // Import so the linker resolves the symbol.
            Linkage::Import
        };
        let id = module
            .declare_function(&symbol, lk, &signature)
            .map_err(|e| anyhow!("declare {symbol}: {e}"))?;
        function_ids_by_name.insert(body.name.clone(), id);
        if let Some(def) = body.def {
            function_ids_by_def.insert(def.local, id);
        }
    }

    // N9-A: Seed the IntrinsicContext with all function maps so that
    // clones sent to rayon threads carry complete function-pointer tables.
    let mut intrinsics = IntrinsicContext::new();
    intrinsics.functions.clone_from(&function_ids_by_name);
    intrinsics.functions_by_def.clone_from(&function_ids_by_def);
    // Per-callee sret return-slot count: for every body that returns a
    // multi-slot aggregate through the sret ABI, record how many 8-byte words
    // its result slot holds, keyed the same way `resolve_callee` looks a callee
    // up (mangled/plain name and def-local id). A call site sizes its
    // caller-owned sret slot from this instead of a fixed guess, so a struct /
    // array / 3+-tuple return lands in a correctly-sized slot rather than
    // overflowing a 16-byte one.
    for body in bodies {
        if body_returns_sret_aggregate(body, tcx) {
            let slots = type_slot_count(tcx, body.local_ty(Local::RETURN)).max(1);
            intrinsics
                .sret_slots_by_name
                .insert(body.name.clone(), slots);
            if let Some(def) = body.def {
                intrinsics.sret_slots_by_def.insert(def.local, slots);
            }
        }
    }

    // N9-B: Pre-declare every runtime symbol the codegen may reference
    // so that all IntrinsicContext cache lookups in the parallel phase
    // hit without touching the module. Three categories:
    //   1. Every symbol in the ABI registry (covers all gos_rt_* helpers
    //      including the cleanup free-functions).
    //   2. C standard-library symbols used by codegen helpers directly
    //      (malloc, strlen, calloc).
    //   3. Infrastructure strings and all ConstValue::Str literals from
    //      bodies; shape thunks whose names encode Fn-trait signatures.
    let ptr_ty = module.target_config().pointer_type();
    for entry in gossamer_abi::REGISTRY {
        intrinsics.extern_fn_by_name(module, entry.name)?;
    }
    intrinsics.extern_fn(module, "malloc", &[ptr_ty], &[ptr_ty])?;
    intrinsics.extern_fn(module, "strlen", &[ptr_ty], &[types::I64])?;
    intrinsics.extern_fn(module, "calloc", &[ptr_ty, ptr_ty], &[ptr_ty])?;
    // Helper-emitted string literals. These are produced by the
    // codegen itself (bounds-check labels, fallback placeholders,
    // common format separators) rather than appearing in any
    // body's ConstValue::Str list. Pre-interning them here so
    // the parallel-phase `OfflineModule` never sees a fresh
    // `declare_data` call from one of the helpers.
    for &s in &["", " ", ", ", "<value>", "array index"] {
        intrinsics.intern_string(module, s)?;
    }
    // Fixed diagnostic strings the Assert / Unreachable terminators
    // hand to `gos_rt_panic`, plus every dynamic `panic!` message, so
    // the parallel phase never declares a fresh string.
    for &s in STATIC_PANIC_MESSAGES {
        intrinsics.intern_string(module, s)?;
    }
    for body in bodies {
        for block in &body.blocks {
            if let Terminator::Panic { message } = &block.terminator {
                intrinsics.intern_string(module, message)?;
            }
        }
    }
    for body in bodies {
        for s in collect_body_str_consts(body) {
            if s.starts_with("__fn_thunk_") {
                if !intrinsics.functions.contains_key(&s) {
                    define_shape_thunk(module, &mut intrinsics, &s)?;
                }
            } else {
                // RC child-layout meta symbols (`gos_rc_meta_*`) must be
                // interned as DATA before the parallel phase: the enum
                // constructor lowering resolves them via intern_rc_meta,
                // which would otherwise declare_data mid-parallel.
                if let Some(blob) = tcx.rc_meta(&s) {
                    intrinsics.intern_rc_meta(module, &s, blob)?;
                }
                intrinsics.intern_string(module, &s)?;
            }
        }
        // Pre-intern the body's name so the call-stack-push prologue
        // can lift it into a data ref without touching the offline
        // module mid-parallel-phase.
        intrinsics.intern_string(module, &body.name)?;
        // Tuple tag blobs are data objects the print lowering resolves
        // at emit time, so every tuple shape a body can print - and
        // every tuple nested inside one, which `t.0` can print on its
        // own - is interned here.
        let mut tuple_tys = Vec::new();
        for local in &body.locals {
            super::operand::nested_tuple_types(tcx, local.ty, &mut tuple_tys);
        }
        for ty in tuple_tys {
            if let Some(tags) = super::operand::tuple_tags_for_ty(tcx, ty) {
                intrinsics.intern_tuple_tags(module, &tags)?;
            }
        }
    }

    // Pre-declare every `gos_binding_*` external call target against the
    // real module so the parallel phase resolves each from the intrinsic
    // cache. A [rust-bindings] crate exposing many functions is otherwise
    // the first caller of a symbol, and that first `declare_function`
    // would land on the OfflineModule mid-parallel-phase. The signature is
    // computed exactly as `lower_external_binding_call` does at the call
    // site, so the single name-keyed declaration matches every use.
    for body in bodies {
        for block in &body.blocks {
            if let Terminator::Call {
                callee,
                args,
                destination,
                ..
            } = &block.terminator
                && let Some(name) = callee_prelude_name(callee)
                && name.starts_with("gos_binding_")
            {
                let dest_ty = body.local_ty(destination.local);
                let returns: Vec<ir::Type> = match mir_ty_to_cabi(tcx, dest_ty, ptr_ty) {
                    Some(t) => vec![t],
                    None => Vec::new(),
                };
                let params: Vec<ir::Type> = args
                    .iter()
                    .map(|arg| operand_cabi_ty(arg, body, tcx, ptr_ty))
                    .collect();
                let static_name: &'static str = Box::leak(name.into_boxed_str());
                intrinsics.extern_fn(module, static_name, &params, &returns)?;
            }
        }
    }

    // Pre-intern every scalar `static mut` referenced by a load or store so
    // the parallel phase resolves the backing writable data object from the
    // intrinsic cache instead of calling declare_data on the OfflineModule.
    // Non-scalar statics are left un-interned: their lowering declines the
    // body, which then runs on the VM.
    for body in bodies {
        for sref in collect_body_static_refs(body) {
            if is_scalar_static_ty(tcx, sref.ty) {
                let cl_ty = cl_type_of(tcx, sref.ty, module);
                intrinsics.intern_static(module, sref, cl_ty)?;
            }
        }
    }

    // N9-C: Build the OfflineModule snapshot. From this point the real
    // ObjectModule is only needed for define_function (N9-E below).
    let offline = build_offline_module(module, &intrinsics, &function_ids_by_name);

    // Inter-procedural capture summary: feeds the cleanup pass so
    // owning bindings whose only outbound use is a non-capturing
    // user fn get a precise per-block drop instead of being forced
    // into the escape set.
    let capture_summary = gossamer_mir::build_capture_summary(bodies);

    // N9-D: Build every function's IR. Object/AOT builds keep the
    // parallel path; the in-process JIT uses `Serial` to avoid faulting
    // rayon workers for a handful of short-lived hot bodies.
    let dump_clif = std::env::var("GOS_DUMP_CLIF").is_ok();
    let lower_one = |body: &Body,
                     bct: &Vec<Option<ir::Type>>|
     -> Result<(FuncId, String, Function)> {
        let id = function_ids_by_name
            .get(&body.name)
            .copied()
            .ok_or_else(|| anyhow!("function id missing: {}", body.name))?;
        let mut offline_clone = offline.clone();
        let mut local_intrinsics = intrinsics.clone();
        local_intrinsics.body_cl_types.clone_from(bct);
        let signature = build_signature_from_types(&offline_clone, body, tcx, bct);
        let mut func = Function::with_name_signature(UserFuncName::user(0, id.as_u32()), signature);
        let mut fb_ctx = FunctionBuilderContext::new();
        lower_body(
            &mut offline_clone,
            &mut func,
            &mut fb_ctx,
            body,
            tcx,
            &function_ids_by_def,
            &function_ids_by_name,
            &mut local_intrinsics,
            &capture_summary,
        )?;
        Ok((id, body.name.clone(), func))
    };
    let ir_pairs: Vec<(FuncId, String, Function)> = match mode {
        LoweringMode::Parallel => bodies
            .par_iter()
            .zip(body_type_vecs.par_iter())
            .filter(|(body, _)| body_should_be_defined(&body.name))
            .map(|(body, bct)| lower_one(body, bct))
            .collect::<Result<Vec<_>>>()?,
        LoweringMode::Serial => bodies
            .iter()
            .zip(body_type_vecs.iter())
            .filter(|(body, _)| body_should_be_defined(&body.name))
            .map(|(body, bct)| lower_one(body, bct))
            .collect::<Result<Vec<_>>>()?,
    };

    // N9-E: Emit each compiled function into the real ObjectModule
    // sequentially (ObjectModule is not Sync). Cranelift compilation
    // happens here too, but the IR construction above (the expensive
    // allocation-heavy work) ran in parallel.
    let mut emitted_code_bytes = 0u64;
    for (id, name, func) in ir_pairs {
        if dump_clif {
            eprintln!("=== CLIF {name} ===\n{}", func.display());
        }
        let mut ctx = Context::for_function(func);
        module.define_function(id, &mut ctx).map_err(|e| {
            let detail = match &e {
                cranelift_module::ModuleError::Compilation(ce) => format!("{ce:#}\n{ce:?}"),
                other => format!("{other:#}"),
            };
            anyhow!("define {name}: {detail}")
        })?;
        let code_bytes = ctx
            .compiled_code()
            .map(|code| u64::from(code.code_info().total_size))
            .ok_or_else(|| anyhow!("define {name}: Cranelift returned no compiled code"))?;
        emitted_code_bytes = emitted_code_bytes.saturating_add(code_bytes);
    }

    Ok(LoweredProgram {
        function_ids_by_name,
        emitted_code_bytes,
        function_ids_by_def,
    })
}
