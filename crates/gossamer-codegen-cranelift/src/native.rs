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
//! anything that needs a GC heap are not yet lowered — those
//! constructs fall back to [`super::emit::emit_module`] for
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
//   - `doc_markdown` flags every reference to `fasta_mt`,
//     `i64`, `f64`, etc. in plain-prose docs. Backticking
//     every numeric type name in every comment is noise.
//   - `manual_debug_impl` flags `JitModule`'s `Debug` impl
//     (which deliberately omits the JIT module pointer to keep
//     debug output stable across runs).
#![forbid(unsafe_code)]
#![allow(
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::unnecessary_wraps,
    clippy::if_not_else,
    clippy::doc_markdown,
    clippy::manual_let_else,
    clippy::comparison_chain,
)]

use std::collections::HashMap;

use anyhow::{Result, anyhow, bail};
use cranelift_codegen::ir::{
    AbiParam, Function, InstBuilder, MemFlags, Signature, StackSlotData, StackSlotKind,
    UserFuncName, types,
};
use cranelift_codegen::settings::{self, Configurable};
use cranelift_codegen::{Context, ir};
use cranelift_frontend::{FunctionBuilder, FunctionBuilderContext, Variable};
use cranelift_module::{DataDescription, DataId, FuncId, Linkage, Module};
use cranelift_object::{ObjectBuilder, ObjectModule};
use gossamer_mir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind,
    Terminator, UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

/// Globally-scoped rodata + intrinsic function handles accumulated
/// across every body in a single [`compile_to_object`] run. Keeps
/// the per-function lowering paths from having to thread the
/// module's mutation needs through themselves.
struct IntrinsicContext {
    /// Interned map from string contents to the `DataId` of the
    /// null-terminated rodata slot holding them. Deduped so the same
    /// literal used in twenty calls still occupies one slot.
    strings: HashMap<String, DataId>,
    /// Cached `FuncId` for each C-ABI runtime function we link.
    externs: HashMap<&'static str, FuncId>,
    /// Monotonic counter for freshly-generated rodata symbol names.
    next_str_id: u32,
    /// Mirror of `function_ids_by_name` from [`compile_to_object`].
    /// Populated up-front so intrinsics like `gos_fn_addr` can look
    /// up the target function without threading the parent map
    /// through every call.
    functions: HashMap<String, FuncId>,
    /// Mirror of `function_ids_by_def` so `Operand::FnRef { def }`
    /// operands in non-call position (`let f = fib; f(5)`) can be
    /// materialised as function-pointer values.
    functions_by_def: HashMap<u32, FuncId>,
    /// Per-function: the cranelift element type of stack-allocated
    /// aggregates rooted at each local. Populated when lowering
    /// `Rvalue::Aggregate` / `Rvalue::Repeat`, consumed by
    /// projected reads / writes when the MIR element type is still
    /// an unresolved inference variable. Cleared between bodies.
    elem_cl_ty: HashMap<Local, ir::Type>,
    /// Per-function: size in 8-byte slots of each element in an
    /// aggregate rooted at the local. `1` for scalar arrays,
    /// `N` for `[Struct; _]` where `Struct` has `N` fields.
    /// Projected address computation uses this as the per-index
    /// stride. Cleared between bodies.
    elem_slots: HashMap<Local, u32>,
    /// Per-function: total size in 8-byte slots of the aggregate
    /// rooted at the local. Used so that nested `[T; N]` → `[S;
    /// N]` aggregates produce correct per-element strides.
    /// Cleared between bodies.
    local_slots: HashMap<Local, u32>,
    /// Per-function: the cranelift type each local's Variable was
    /// declared with. Populated by `define_var_to` on first
    /// declaration; consulted by `operand_print_kind` so print
    /// dispatch uses the concrete width even when the MIR local's
    /// type is still an unresolved inference variable. Cleared
    /// between bodies.
    local_declared_ty: HashMap<Local, ir::Type>,
}

impl IntrinsicContext {
    fn new() -> Self {
        Self {
            strings: HashMap::new(),
            externs: HashMap::new(),
            next_str_id: 0,
            functions: HashMap::new(),
            functions_by_def: HashMap::new(),
            elem_cl_ty: HashMap::new(),
            elem_slots: HashMap::new(),
            local_slots: HashMap::new(),
            local_declared_ty: HashMap::new(),
        }
    }

    /// Returns the `DataId` for `text`, defining a new null-
    /// terminated rodata slot on first use.
    fn intern_string(&mut self, module: &mut dyn Module, text: &str) -> Result<DataId> {
        if let Some(id) = self.strings.get(text).copied() {
            return Ok(id);
        }
        let symbol = format!(".Lstr{}", self.next_str_id);
        self.next_str_id += 1;
        let id = module
            .declare_data(&symbol, Linkage::Local, false, false)
            .map_err(|e| anyhow!("declare {symbol}: {e}"))?;
        let mut bytes = text.as_bytes().to_vec();
        bytes.push(0);
        let mut description = DataDescription::new();
        description.define(bytes.into_boxed_slice());
        module
            .define_data(id, &description)
            .map_err(|e| anyhow!("define {symbol}: {e}"))?;
        self.strings.insert(text.to_string(), id);
        Ok(id)
    }

    /// Declares (if needed) an imported C-ABI function and returns
    /// its `FuncId`.
    fn extern_fn(
        &mut self,
        module: &mut dyn Module,
        name: &'static str,
        params: &[ir::Type],
        returns: &[ir::Type],
    ) -> Result<FuncId> {
        if let Some(id) = self.externs.get(name).copied() {
            return Ok(id);
        }
        let mut sig = module.make_signature();
        for p in params {
            sig.params.push(AbiParam::new(*p));
        }
        for r in returns {
            sig.returns.push(AbiParam::new(*r));
        }
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| anyhow!("declare extern {name}: {e}"))?;
        self.externs.insert(name, id);
        Ok(id)
    }
}

/// Native codegen output: the linker-ready object bytes plus the
/// target triple the ISA was configured against.
#[derive(Debug, Clone)]
pub struct NativeObject {
    /// Target triple the object was produced for.
    pub triple: String,
    /// Serialised object bytes (ELF on Linux, Mach-O on macOS, …).
    pub bytes: Vec<u8>,
}

/// Result of declaring and defining every body in a program against
/// some [`Module`] backend. Returned by [`lower_program`] so the
/// caller (object emitter or JIT finaliser) can look up the symbols
/// they care about by name or by resolver-assigned `DefId.local`.
pub(crate) struct LoweredProgram {
    pub function_ids_by_name: HashMap<String, FuncId>,
    /// Reserved for callers that resolve `Operand::FnRef` by
    /// `DefId` rather than name. The JIT only needs name lookup
    /// today; the field stays in the API so the LLVM backend
    /// landing in parallel can drop in without an extra pass.
    #[allow(dead_code)]
    pub function_ids_by_def: HashMap<u32, FuncId>,
}

/// Builds the cranelift settings + native ISA used by both the
/// object and JIT pipelines. `pic` differs by backend: the AOT
/// object emitter needs `is_pic=true` so the produced relocations
/// match what `cc` expects when linking, while `cranelift-jit`
/// hard-rejects PIC at finalisation time (see
/// [the JIT backend's assertion](https://github.com/bytecodealliance/wasmtime/blob/v36.0.7/cranelift/jit/src/backend.rs#L348)).
pub(crate) fn build_native_isa(pic: bool) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>> {
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
    let flags = settings::Flags::new(flag_builder);
    let isa_builder = cranelift_native::builder()
        .map_err(|e| anyhow!("native isa: {e}"))?;
    let isa = isa_builder
        .finish(flags)
        .map_err(|e| anyhow!("native isa finish: {e}"))?;
    Ok(isa)
}

/// Declares every body in `bodies` and lowers each one into the
/// supplied [`Module`]. Returns the symbol-id maps so callers can
/// finalise (object emit / JIT bind) however they like.
///
/// `entry_symbol_for_main` lets the object backend rename the
/// user's `main` to `gossamer_main` so a C-ABI shim can wrap it;
/// the JIT path passes `None` and keeps the original name.
pub(crate) fn lower_program(
    module: &mut dyn Module,
    bodies: &[Body],
    tcx: &TyCtxt,
    entry_symbol_for_main: Option<&str>,
) -> Result<LoweredProgram> {
    lower_program_with_linkage(module, bodies, tcx, entry_symbol_for_main, Linkage::Local)
}

/// Like [`lower_program`] but lets the caller pick the linkage
/// for user-defined functions. The fallback companion path
/// uses `Linkage::Export` so the LLVM-emitted primary object
/// can resolve user-function calls across the object boundary.
#[allow(dead_code)]
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
    )
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
    define_only: Option<&[String]>,
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

    // Declare every function up-front so call-sites can resolve.
    // We key the map by the resolver-assigned `DefId.local` so
    // `Operand::FnRef(def)` from MIR lowers to the right function
    // ref, with a by-name fallback for the rare body that has no
    // resolver id (synthesised closures).
    let mut function_ids_by_def: HashMap<u32, FuncId> = HashMap::new();
    let mut function_ids_by_name: HashMap<String, FuncId> = HashMap::new();
    let body_should_be_defined = |name: &str| -> bool {
        match define_only {
            Some(allowed) => allowed.iter().any(|n| n == name),
            None => true,
        }
    };
    for body in bodies {
        let signature = build_signature(&*module, body, tcx);
        let symbol = if body.name == "main" {
            entry_symbol_for_main
                .map_or_else(|| body.name.clone(), str::to_string)
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
            // its body lives in a sibling object — declare as
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

    let mut intrinsics = IntrinsicContext::new();
    intrinsics.functions.clone_from(&function_ids_by_name);
    intrinsics.functions_by_def.clone_from(&function_ids_by_def);
    for body in bodies {
        if !body_should_be_defined(&body.name) {
            continue;
        }
        intrinsics.elem_cl_ty.clear();
        intrinsics.elem_slots.clear();
        intrinsics.local_slots.clear();
        let id = function_ids_by_name
            .get(&body.name)
            .copied()
            .ok_or_else(|| anyhow!("function id missing: {}", body.name))?;
        let signature = build_signature(&*module, body, tcx);
        let mut func = Function::with_name_signature(
            UserFuncName::user(0, id.as_u32()),
            signature,
        );
        let mut fb_ctx = FunctionBuilderContext::new();
        lower_body(
            module,
            &mut func,
            &mut fb_ctx,
            body,
            tcx,
            &function_ids_by_def,
            &function_ids_by_name,
            &mut intrinsics,
        )?;
        let mut ctx = Context::for_function(func);
        module.define_function(id, &mut ctx).map_err(|e| {
            let detail = match &e {
                cranelift_module::ModuleError::Compilation(ce) => format!("{ce:#}\n{ce:?}"),
                other => format!("{other:#}"),
            };
            anyhow!("define {}: {detail}", body.name)
        })?;
    }

    Ok(LoweredProgram {
        function_ids_by_name,
        function_ids_by_def,
    })
}

/// Lowers `bodies` into a native object file. The first body whose
/// name is `"main"` becomes the program entry point. `tcx` is the
/// type context produced by the frontend; codegen reads it to
/// classify each MIR local into a cranelift type.
pub fn compile_to_object(bodies: &[Body], tcx: &TyCtxt) -> Result<NativeObject> {
    compile_to_object_with_options(bodies, tcx, CompileOptions::default())
}

/// Per-build configuration for the Cranelift backend.
///
/// Default behaviour matches the historical `compile_to_object`
/// — the user's `main` is renamed to `gossamer_main` and a
/// C-ABI `main` shim is appended. The fallback companion path
/// used by the LLVM backend overrides both: it suppresses the
/// shim (LLVM emits it) and renames `main` to `gos_main` so the
/// LLVM-emitted shim's `call gos_main` resolves to the
/// Cranelift-provided body at link time.
#[derive(Debug, Clone, Default)]
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

/// `compile_to_object` plus optional `main` rename / shim
/// suppression. Used by the per-function fallback driver path.
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
    let lowered = lower_program_full(
        &mut module,
        bodies,
        tcx,
        Some(main_rename),
        options.omit_c_main_shim,
        options.define_only.as_deref(),
    )?;

    if !options.omit_c_main_shim {
        if let Some(gos_main) = lowered.function_ids_by_name.get("main").copied() {
            emit_c_main_shim(&mut module, gos_main)?;
        }
    }

    let product = module.finish();
    let bytes = product
        .emit()
        .map_err(|e| anyhow!("emit object: {e}"))?;
    Ok(NativeObject { triple, bytes })
}

fn build_signature(module: &dyn Module, body: &Body, tcx: &TyCtxt) -> Signature {
    let mut sig = module.make_signature();
    for pidx in 1..=body.arity {
        let local = Local(pidx);
        let cl = infer_local_cl_type(body, tcx, module, local)
            .unwrap_or_else(|| cl_type_of(tcx, body.local_ty(local), module));
        sig.params.push(AbiParam::new(cl));
    }
    let ret_cl = infer_local_cl_type(body, tcx, module, Local::RETURN)
        .unwrap_or_else(|| cl_type_of(tcx, body.local_ty(Local::RETURN), module));
    sig.returns.push(AbiParam::new(ret_cl));
    sig
}

/// Classifies a high-level [`Ty`] into the cranelift register
/// type we'll use for the matching SSA local / load / store.
/// Aggregates, references, strings, and anything non-scalar land
/// on the pointer type; a pointer to the stack-slot or rodata
/// backing the value is what the codegen passes around.
fn cl_type_of(tcx: &TyCtxt, ty: Ty, module: &dyn Module) -> ir::Type {
    match tcx.kind_of(ty) {
        TyKind::Bool => types::I8,
        TyKind::Char => types::I32,
        TyKind::Int(int) => match int {
            IntTy::I8 | IntTy::U8 => types::I8,
            IntTy::I16 | IntTy::U16 => types::I16,
            IntTy::I32 | IntTy::U32 => types::I32,
            IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => types::I64,
            IntTy::I128 | IntTy::U128 => types::I64,
        },
        TyKind::Float(float) => match float {
            FloatTy::F32 => types::F32,
            FloatTy::F64 => types::F64,
        },
        TyKind::Unit | TyKind::Never => types::I64,
        _ => module.target_config().pointer_type(),
    }
}

/// Walks `place`'s projection chain from its root local and returns
/// the cranelift type of the final projected value, given the
/// caller's expected type as a fall-back for cases the type
/// interner can't directly answer (ADT field projections — the
/// current interner records the ADT's `DefId` but does not surface
/// a `field_ty(def, variant, idx)` query).
///
/// The hint is normally the destination local's type on the assign
/// side, which is always in agreement with the leaf thanks to the
/// type checker's invariants; so hint-based fallback never widens a
/// field load/store.
fn resolve_place_cl_type(
    tcx: &TyCtxt,
    body: &Body,
    place: &Place,
    module: &dyn Module,
    hint: Option<ir::Type>,
) -> ir::Type {
    let mut ty = body.local_ty(place.local);
    let mut hit_opaque = false;
    for projection in &place.projection {
        match projection {
            Projection::Field(idx) => match tcx.kind_of(ty) {
                TyKind::Tuple(elems) => {
                    if let Some(next) = elems.get(*idx as usize).copied() {
                        ty = next;
                    } else {
                        hit_opaque = true;
                    }
                }
                _ => {
                    // ADT fields / anything else — interner doesn't
                    // surface the leaf type here. Drop to the hint.
                    hit_opaque = true;
                }
            },
            Projection::Index(_) => match tcx.kind_of(ty) {
                TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                    ty = *elem;
                }
                _ => hit_opaque = true,
            },
            Projection::Deref => match tcx.kind_of(ty) {
                TyKind::Ref { inner, .. } => ty = *inner,
                _ => hit_opaque = true,
            },
            Projection::Downcast(_) | Projection::Discriminant => hit_opaque = true,
        }
    }
    if hit_opaque {
        if let Some(h) = hint {
            return h;
        }
    }
    cl_type_of(tcx, ty, module)
}

#[allow(
    clippy::too_many_arguments,
    reason = "lowering plumbing — every parameter is needed by Cranelift's API",
)]
fn lower_body(
    module: &mut dyn Module,
    func: &mut Function,
    fb_ctx: &mut FunctionBuilderContext,
    body: &Body,
    tcx: &TyCtxt,
    function_ids_by_def: &HashMap<u32, FuncId>,
    function_ids_by_name: &HashMap<String, FuncId>,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    let mut builder = FunctionBuilder::new(func, fb_ctx);

    let mut locals: HashMap<Local, Variable> = HashMap::new();
    let mut blocks: HashMap<u32, ir::Block> = HashMap::new();

    for block in &body.blocks {
        let cl_block = builder.create_block();
        blocks.insert(block.id.as_u32(), cl_block);
    }

    // Entry block gets the parameters as its block params.
    if let Some(first_block) = body.blocks.first() {
        let entry = blocks[&first_block.id.as_u32()];
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        for (index, param_local_u32) in (1..=body.arity).enumerate() {
            let local = Local(param_local_u32);
            let param_value = builder.block_params(entry)[index];
            define_var_to(&mut builder, &mut locals, body, tcx, module, local, param_value);
        }
    }

    // Declare a Cranelift-side reference for every callable function.
    let mut callees_by_def: HashMap<u32, ir::FuncRef> = HashMap::new();
    let mut callees_by_name: HashMap<String, ir::FuncRef> = HashMap::new();
    for (def_local, id) in function_ids_by_def {
        let func_ref = module.declare_func_in_func(*id, builder.func);
        callees_by_def.insert(*def_local, func_ref);
    }
    for (name, id) in function_ids_by_name {
        let func_ref = module.declare_func_in_func(*id, builder.func);
        callees_by_name.insert(name.clone(), func_ref);
    }

    for block in &body.blocks {
        let cl_block = blocks[&block.id.as_u32()];
        builder.switch_to_block(cl_block);

        for statement in &block.stmts {
            lower_statement(module, &mut builder, &mut locals, body, tcx, statement, intrinsics)?;
        }

        lower_terminator(
            module,
            &mut builder,
            &mut locals,
            body,
            tcx,
            &mut blocks,
            &callees_by_def,
            &callees_by_name,
            &block.terminator,
            intrinsics,
        )?;
    }

    builder.seal_all_blocks();
    builder.finalize();
    Ok(())
}

fn ensure_var(
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    module: &dyn Module,
    local: Local,
) -> Variable {
    if let Some(var) = locals.get(&local).copied() {
        return var;
    }
    // Read-before-write fallback: prefer the inferred effective type
    // (from body scanning) and only fall back to the MIR-declared
    // type if the inference turned up nothing.
    let inferred = infer_local_cl_type(body, tcx, module, local);
    let cl = inferred.unwrap_or_else(|| cl_type_of(tcx, body.local_ty(local), module));
    let var = builder.declare_var(cl);
    locals.insert(local, var);
    var
}

/// Best-effort cranelift-type inference for a MIR local by scanning
/// the body for assignments. Used when the MIR's recorded type is
/// `Error`/`Var`/otherwise-opaque, so read-before-write paths (in
/// particular: function parameter arrivals whose HIR param type
/// was lost) still declare the right width.
fn infer_local_cl_type(
    body: &Body,
    tcx: &TyCtxt,
    module: &dyn Module,
    local: Local,
) -> Option<ir::Type> {
    // Delegate to the body-wide inference table. Computing it once
    // per body would be more efficient; today this is O(body) per
    // local, bounded by the fixed-point iteration. The types the
    // table picks are stable across callers, so a memoization
    // pass could be added if codegen time matters.
    let table = infer_body_cl_types(body, tcx, module);
    table.get(&local).copied()
}

/// Propagates concrete cranelift types across every local in a body
/// by iterating to a fixed point. Seeds are the MIR-recorded types
/// that map directly to a cranelift scalar; then each `Copy`,
/// `BinaryOp`, and `Cast` assignment propagates the RHS's inferred
/// type to the destination (preferring float over int when an int
/// seed later gets rewritten by a float store — common when a
/// parameter's MIR type came out as `Error` but its body uses are
/// all floating-point).
fn infer_body_cl_types(
    body: &Body,
    tcx: &TyCtxt,
    module: &dyn Module,
) -> HashMap<Local, ir::Type> {
    let mut table: HashMap<Local, ir::Type> = HashMap::new();
    // Seed: MIR types that directly map to a concrete cranelift type.
    for (idx, decl) in body.locals.iter().enumerate() {
        if let Some(cl) = cl_type_of_if_concrete(tcx, decl.ty, module) {
            table.insert(Local(idx as u32), cl);
        }
    }
    let rvalue_ty = |rvalue: &Rvalue, table: &HashMap<Local, ir::Type>| -> Option<ir::Type> {
        let op_ty = |op: &Operand| -> Option<ir::Type> {
            match op {
                Operand::Const(ConstValue::Int(_)) => Some(types::I64),
                Operand::Const(ConstValue::Float(_)) => Some(types::F64),
                Operand::Const(ConstValue::Bool(_)) => Some(types::I8),
                Operand::Const(ConstValue::Char(_)) => Some(types::I32),
                Operand::Const(ConstValue::Str(_)) => {
                    Some(module.target_config().pointer_type())
                }
                Operand::Const(ConstValue::Unit) => None,
                Operand::Copy(place) => {
                    if place.projection.is_empty() {
                        table.get(&place.local).copied()
                    } else {
                        cl_type_of_if_concrete(
                            tcx,
                            resolve_place_ty(tcx, body, place),
                            module,
                        )
                    }
                }
                Operand::FnRef { .. } => None,
            }
        };
        match rvalue {
            Rvalue::Use(op) | Rvalue::UnaryOp { operand: op, .. } => op_ty(op),
            Rvalue::BinaryOp { op, lhs, rhs } => match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                    Some(types::I8)
                }
                _ => op_ty(lhs).or_else(|| op_ty(rhs)),
            },
            Rvalue::Cast { operand, target } => cl_type_of_if_concrete(tcx, *target, module)
                .or_else(|| op_ty(operand)),
            Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } => {
                Some(module.target_config().pointer_type())
            }
            _ => None,
        }
    };
    let mut changed = true;
    while changed {
        changed = false;
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                    if !place.projection.is_empty() {
                        continue;
                    }
                    if let Some(cl) = rvalue_ty(rvalue, &table) {
                        match table.get(&place.local).copied() {
                            None => {
                                table.insert(place.local, cl);
                                changed = true;
                            }
                            // Only upgrade i64 placeholders — locals
                            // whose MIR type or earlier inference
                            // grounded them to a specific non-i64
                            // cranelift type are trusted.
                            Some(current)
                                if current == types::I64 && cl == types::F64 =>
                            {
                                table.insert(place.local, cl);
                                changed = true;
                            }
                            _ => {}
                        }
                    }
                    // Reverse propagation: when the destination of
                    // an assignment has a concrete type and the
                    // operation's semantics guarantee the operands
                    // share that type (Use / UnaryOp / same-type
                    // BinaryOp arithmetic), propagate the type
                    // back to any still-unresolved operand. Catches
                    // parameters that were never assigned (so the
                    // forward sweep never saw them) but are used as
                    // the source of a known-typed copy or arith
                    // expression.
                    if let Some(dst_ty) = table.get(&place.local).copied() {
                        let propagate = match rvalue {
                            Rvalue::Use(_) | Rvalue::UnaryOp { .. } => true,
                            Rvalue::BinaryOp { op, .. } => !matches!(
                                op,
                                BinOp::Eq
                                    | BinOp::Ne
                                    | BinOp::Lt
                                    | BinOp::Le
                                    | BinOp::Gt
                                    | BinOp::Ge
                            ),
                            _ => false,
                        };
                        if propagate {
                            for op in operand_locals(rvalue) {
                                let existing = table.get(&op).copied();
                                let upgrade = existing.is_none()
                                    || (existing == Some(types::I64) && dst_ty == types::F64);
                                if upgrade && existing != Some(dst_ty) {
                                    table.insert(op, dst_ty);
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    table
}

fn operand_locals(rvalue: &Rvalue) -> Vec<Local> {
    let mut out = Vec::new();
    let mut push = |op: &Operand| {
        if let Operand::Copy(place) = op {
            if place.projection.is_empty() {
                out.push(place.local);
            }
        }
    };
    match rvalue {
        Rvalue::Use(op) | Rvalue::UnaryOp { operand: op, .. } => push(op),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            push(lhs);
            push(rhs);
        }
        Rvalue::Cast { operand, .. } => push(operand),
        _ => {}
    }
    out
}

fn cl_type_of_if_concrete(tcx: &TyCtxt, ty: Ty, module: &dyn Module) -> Option<ir::Type> {
    match tcx.kind_of(ty) {
        TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_) => {
            Some(cl_type_of(tcx, ty, module))
        }
        TyKind::Ref { .. } | TyKind::String => Some(module.target_config().pointer_type()),
        _ => None,
    }
}

/// Declares (if needed) and initialises `local` from `value`. Always
/// uses the *value's* cranelift type for the Variable's declaration
/// so type-inference leaks from the front-end (MIR locals whose type
/// is still an unresolved `Var(_)`) don't make us declare the slot
/// at the wrong width.
fn define_var_to(
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    module: &dyn Module,
    local: Local,
    value: ir::Value,
) {
    let preferred = infer_local_cl_type(body, tcx, module, local);
    define_var_to_with(builder, locals, local, value, preferred);
}

/// `define_var_to` variant that accepts an optional target cranelift
/// type. Used when the caller already ran whole-body inference and
/// wants to pin the declared Variable to that type even when this
/// particular value's type would otherwise fit a narrower width.
fn define_var_to_with(
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    local: Local,
    value: ir::Value,
    preferred_ty: Option<ir::Type>,
) {
    let value_ty = value_type(value, builder);
    let decl_ty = preferred_ty.unwrap_or(value_ty);
    let var = *locals
        .entry(local)
        .or_insert_with(|| builder.declare_var(decl_ty));
    let _ = decl_ty;
    // Coerce the value to the declared variable width when they
    // disagree (e.g. we declared the local as F64 from inference,
    // but this particular value was loaded as I64 because the MIR
    // path still considered the source as an inference variable).
    let coerced = if decl_ty == value_ty {
        value
    } else if decl_ty == types::F64 && value_ty == types::I64 {
        builder.ins().bitcast(types::F64, ir::MemFlags::new(), value)
    } else if decl_ty == types::I64 && value_ty == types::F64 {
        builder.ins().bitcast(types::I64, ir::MemFlags::new(), value)
    } else if decl_ty.is_int() && value_ty.is_int() {
        if decl_ty.bits() > value_ty.bits() {
            builder.ins().sextend(decl_ty, value)
        } else {
            builder.ins().ireduce(decl_ty, value)
        }
    } else {
        value
    };
    builder.def_var(var, coerced);
}

fn lower_statement(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    statement: &gossamer_mir::Statement,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    match &statement.kind {
        StatementKind::Assign { place, rvalue } => {
            // Route rvalue-position intrinsic calls (gos_alloc,
            // gos_store, gos_load, …) through the same handler the
            // terminator path uses. Keeps the heap primitives usable
            // as inline expressions inside a single basic block.
            if let Rvalue::CallIntrinsic { name, args } = rvalue {
                if lower_intrinsic_call(
                    module, builder, locals, body, tcx, args, name, place, intrinsics,
                )? {
                    return Ok(());
                }
                // HIR lowering emits `CallIntrinsic { name: "unsupported" }`
                // (and similar placeholders) for constructs MIR cannot
                // lower natively — method calls, closures-on-escaping-
                // paths, `go expr`, etc. Refuse to build rather than
                // emit a runtime-abort stub; the driver surfaces the
                // error to the user. Reaching this arm post-L4 is
                // a compiler bug — every HIR construct should
                // lower to a concrete MIR terminator or a stub.
                bail!("native codegen: unsupported intrinsic `{name}`");
            }
            // Destination hint: when the place has no projections, it's
            // the root local's type. When it does, we still use the
            // root's classification as the hint, but the projected
            // store below picks the correct width from the leaf type.
            let dst_hint = cl_type_of(tcx, body.local_ty(place.local), module);
            // When the rvalue is an aggregate, remember the first
            // operand's cranelift type as the uniform element type.
            // Projected reads/writes later look this up as a hint
            // when the MIR element type is an unresolved inference
            // variable.
            let aggregate_elem_ty: Option<ir::Type> = match rvalue {
                Rvalue::Aggregate { operands, .. } => operands
                    .first()
                    .and_then(|op| operand_cl_type(body, tcx, op, module)),
                Rvalue::Repeat { value, .. } => {
                    operand_cl_type(body, tcx, value, module)
                }
                _ => None,
            };
            // Same for slot counts: remember per-element and total
            // slot widths so downstream projected addresses stride
            // correctly through aggregates of aggregates.
            let (aggregate_elem_slots, aggregate_total_slots): (Option<u32>, Option<u32>) =
                match rvalue {
                    Rvalue::Aggregate { kind, operands } => {
                        let elem = match kind {
                            gossamer_mir::AggregateKind::Array => operands
                                .first()
                                .and_then(|op| {
                                    if let Operand::Copy(p) = op {
                                        intrinsics
                                            .local_slots
                                            .get(&p.local)
                                            .copied()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(1),
                            _ => 1,
                        };
                        let total = match kind {
                            gossamer_mir::AggregateKind::Array => {
                                (operands.len() as u32) * elem
                            }
                            _ => operands.len() as u32,
                        };
                        (Some(elem), Some(total))
                    }
                    Rvalue::Repeat { value, count } => {
                        let elem = if let Operand::Copy(p) = value {
                            intrinsics.local_slots.get(&p.local).copied().unwrap_or(1)
                        } else {
                            1
                        };
                        let total = u32::try_from(*count)
                            .unwrap_or(1)
                            .saturating_mul(elem);
                        (Some(elem), Some(total))
                    }
                    _ => (None, None),
                };
            // For `Use(Copy(src))`/`Use(Move(src))` where the source
            // is a plain local, inherit the source's aggregate
            // metadata. Let-bindings desugar to this pattern
            // (`let ps = <array-literal-temp>`), and without this
            // propagation the binding loses the element stride that
            // the temp had picked up from the aggregate rvalue.
            let copy_src_meta: Option<Local> = match rvalue {
                Rvalue::Use(Operand::Copy(p)) if p.projection.is_empty() => Some(p.local),
                _ => None,
            };
            let value =
                lower_rvalue(module, builder, locals, body, tcx, rvalue, Some(dst_hint), intrinsics)?;
            if place.projection.is_empty() {
                define_var_to(builder, locals, body, tcx, module, place.local, value);
                if let Some(elem) = aggregate_elem_ty {
                    intrinsics.elem_cl_ty.insert(place.local, elem);
                }
                if let Some(slots) = aggregate_elem_slots {
                    intrinsics.elem_slots.insert(place.local, slots);
                }
                if let Some(total) = aggregate_total_slots {
                    intrinsics.local_slots.insert(place.local, total);
                }
                if let Some(src) = copy_src_meta {
                    if let Some(et) = intrinsics.elem_cl_ty.get(&src).copied() {
                        intrinsics.elem_cl_ty.entry(place.local).or_insert(et);
                    }
                    if let Some(es) = intrinsics.elem_slots.get(&src).copied() {
                        intrinsics.elem_slots.entry(place.local).or_insert(es);
                    }
                    if let Some(ls) = intrinsics.local_slots.get(&src).copied() {
                        intrinsics.local_slots.entry(place.local).or_insert(ls);
                    }
                }
            } else {
                let elem_hint = intrinsics.elem_cl_ty.get(&place.local).copied();
                let leaf_ty = resolve_place_cl_type(
                    tcx,
                    body,
                    place,
                    module,
                    elem_hint.or(Some(value_type(value, builder))),
                );
                lower_place_store(
                    module, builder, locals, body, tcx, place, value, leaf_ty, intrinsics,
                )?;
            }
        }
        StatementKind::StorageLive(_)
        | StatementKind::StorageDead(_)
        | StatementKind::Nop => {}
        // SetDiscriminant: store variant index at offset 0 of the
        // enum's backing place. Matches the Downcast convention
        // (tag at slot 0, payload at +8).
        StatementKind::SetDiscriminant { place, variant } => {
            let addr = if place.projection.is_empty() {
                let var = ensure_var(builder, locals, body, tcx, module, place.local);
                builder.use_var(var)
            } else {
                lower_place_address(
                    module, builder, locals, body, tcx, place, intrinsics,
                )?
            };
            let tag = builder.ins().iconst(types::I64, i64::from(*variant));
            builder.ins().store(
                MemFlags::trusted(),
                tag,
                addr,
                ir::immediates::Offset32::new(0),
            );
        }
        // GcWriteBarrier: until the tri-color GC lands the barrier
        // is a no-op. Code is correct without it; once the
        // collector arrives it becomes a call to
        // `gos_rt_gc_write_barrier(place, value)`.
        StatementKind::GcWriteBarrier { .. } => {}
    }
    Ok(())
}

/// Returns the cranelift type of an SSA value.
fn value_type(value: ir::Value, builder: &FunctionBuilder<'_>) -> ir::Type {
    builder.func.dfg.value_type(value)
}

/// Classifies a MIR operand into the shape the `__concat`
/// printf dispatch should pick for its format specifier. The
/// decision is driven by the MIR/Ty layer rather than by the
/// cranelift type alone, because on 64-bit targets pointers and
/// `i64` both lower to `types::I64` at the cranelift level.
///
/// `Unsupported` is returned for operand types we can't print
/// without a Display impl (tuples, structs, Vec, HashMap,
/// Option, Result, etc.). Callers must surface a build error
/// rather than emit a silent stack-pointer print.
#[derive(Debug, Clone, Copy)]
enum PrintKind {
    StrPtr,
    Int,
    Float,
    Bool,
    Char,
    Unsupported(&'static str),
}

/// Best-effort cranelift-type inference for a MIR operand, used
/// when recording aggregate element types. Returns `None` for
/// operands whose type is still an inference variable with no
/// projection-walk fallback.
fn operand_cl_type(
    body: &Body,
    tcx: &TyCtxt,
    operand: &Operand,
    module: &dyn Module,
) -> Option<ir::Type> {
    match operand {
        Operand::Const(ConstValue::Int(_)) => Some(types::I64),
        Operand::Const(ConstValue::Float(_)) => Some(types::F64),
        Operand::Const(ConstValue::Bool(_)) => Some(types::I8),
        Operand::Const(ConstValue::Char(_)) => Some(types::I32),
        Operand::Const(ConstValue::Unit) => None,
        Operand::Const(ConstValue::Str(_)) => Some(module.target_config().pointer_type()),
        Operand::Copy(place) => {
            let ty = resolve_place_ty(tcx, body, place);
            match tcx.kind_of(ty) {
                TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_) => {
                    Some(cl_type_of(tcx, ty, module))
                }
                _ => None,
            }
        }
        Operand::FnRef { .. } => None,
    }
}

fn operand_print_kind(body: &Body, tcx: &TyCtxt, operand: &Operand) -> PrintKind {
    match operand {
        Operand::Const(ConstValue::Str(_)) => PrintKind::StrPtr,
        Operand::Const(ConstValue::Int(_)) => PrintKind::Int,
        Operand::Const(ConstValue::Float(_)) => PrintKind::Float,
        Operand::Const(ConstValue::Bool(_)) => PrintKind::Bool,
        Operand::Const(ConstValue::Char(_)) => PrintKind::Char,
        Operand::Const(ConstValue::Unit) => PrintKind::Int,
        Operand::Copy(place) => {
            let ty = resolve_place_ty(tcx, body, place);
            match tcx.kind_of(ty) {
                TyKind::Bool => PrintKind::Bool,
                TyKind::Char => PrintKind::Char,
                TyKind::Int(_) | TyKind::Unit | TyKind::Never => PrintKind::Int,
                TyKind::Float(_) => PrintKind::Float,
                TyKind::String | TyKind::Ref { .. } => PrintKind::StrPtr,
                // `Var(_)` means the typechecker did not resolve
                // this operand's type. The dominant producer of
                // unresolved-typed locals that flow into println
                // is `__concat` (whose return type is currently
                // not pinned by the typechecker — it returns a
                // String pointer at runtime). Falling back to
                // StrPtr keeps `println!("a={n}")` correct;
                // falling back to Int (the previous default)
                // re-prints the empty-string pointer as a giant
                // integer.
                TyKind::Var(_) => PrintKind::StrPtr,
                // Aggregate / collection / variant-typed values
                // need a Display impl to print sensibly. The
                // compiled tier doesn't dispatch user-defined
                // Display, and silently printing a stack
                // pointer (the previous behavior) is a footgun.
                // Refuse loudly so the user knows to call
                // `format!("{x:?}")` or write their own
                // stringification.
                TyKind::Tuple(_) => PrintKind::Unsupported("tuple"),
                TyKind::Array { .. } => PrintKind::Unsupported("array"),
                TyKind::Slice(_) => PrintKind::Unsupported("slice"),
                TyKind::Vec(_) => PrintKind::Unsupported("Vec"),
                TyKind::HashMap { .. } => PrintKind::Unsupported("HashMap"),
                TyKind::Sender(_) | TyKind::Receiver(_) => PrintKind::Unsupported("channel"),
                TyKind::Adt { .. } => PrintKind::Unsupported("struct or enum"),
                TyKind::Closure { .. } => PrintKind::Unsupported("closure"),
                TyKind::FnDef { .. } | TyKind::FnPtr(_) => PrintKind::Unsupported("function"),
                TyKind::Dyn(_) => PrintKind::Unsupported("dyn Trait"),
                TyKind::Param { .. } | TyKind::Alias { .. } | TyKind::Error => {
                    PrintKind::Unsupported("opaque type")
                }
            }
        }
        Operand::FnRef { .. } => PrintKind::Unsupported("function"),
    }
}

fn resolve_place_ty(tcx: &TyCtxt, body: &Body, place: &Place) -> Ty {
    let mut ty = body.local_ty(place.local);
    for projection in &place.projection {
        ty = match projection {
            Projection::Field(idx) => match tcx.kind_of(ty) {
                TyKind::Tuple(elems) => elems.get(*idx as usize).copied().unwrap_or(ty),
                _ => ty,
            },
            Projection::Index(_) => match tcx.kind_of(ty) {
                TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => *elem,
                _ => ty,
            },
            Projection::Deref => match tcx.kind_of(ty) {
                TyKind::Ref { inner, .. } => *inner,
                _ => ty,
            },
            Projection::Downcast(_) | Projection::Discriminant => ty,
        };
    }
    ty
}

/// Walks a Ty and returns the number of 8-byte slots the underlying
/// array/slice/vec's element type occupies. Used as a fallback when
/// no aggregate metadata was recorded for the local (e.g. parameter
/// arrivals whose body never produced the aggregate). Scalars count
/// as one slot; tuples and named structs count the sum of their
/// members' slots. Returns `None` when the outer type is not an
/// indexable aggregate.
fn stride_slots_from_ty(tcx: &TyCtxt, ty: Ty) -> Option<u32> {
    let mut cur = ty;
    loop {
        match tcx.kind_of(cur).clone() {
            TyKind::Ref { inner, .. } => cur = inner,
            TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
                return Some(type_slot_count(tcx, elem));
            }
            _ => return None,
        }
    }
}

/// Recursively counts the number of 8-byte slots a type occupies in
/// the flat-stack-slot representation the native codegen uses.
fn type_slot_count(tcx: &TyCtxt, ty: Ty) -> u32 {
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems.iter().map(|t| type_slot_count(tcx, *t)).sum::<u32>().max(1),
        TyKind::Array { elem, len } => {
            u32::try_from(len).unwrap_or(1).saturating_mul(type_slot_count(tcx, elem))
        }
        TyKind::Adt { def, .. } => {
            tcx.struct_field_tys(def)
                .map_or(1, |tys| tys.iter().map(|t| type_slot_count(tcx, *t)).sum::<u32>().max(1))
        }
        _ => 1,
    }
}

/// Computes the byte address of the projected slot within its root
/// aggregate, returning a pointer-typed value suitable for a
/// `load` / `store`. Works for `Field(i)` (offset `i*8`) and
/// `Index(local)` (offset `idx*8`). Deref/Downcast/Discriminant
/// remain unimplemented.
#[allow(clippy::too_many_arguments)]
fn lower_place_address(
    module: &dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    intrinsics: &IntrinsicContext,
) -> Result<ir::Value> {
    let var = ensure_var(builder, locals, body, tcx, module, place.local);
    let ptr_ty = module.target_config().pointer_type();
    let root_value = builder.use_var(var);
    // The root local holds a pointer (an aggregate's stack-slot
    // address). Widen it to the target's pointer type so later
    // `iadd`s don't fail on mismatched operand widths.
    let mut current = match value_type(root_value, builder) {
        t if t == ptr_ty => root_value,
        t if t == types::I64 && ptr_ty == types::I32 => {
            builder.ins().ireduce(ptr_ty, root_value)
        }
        t if t == types::I32 && ptr_ty == types::I64 => {
            builder.ins().uextend(ptr_ty, root_value)
        }
        _ => root_value,
    };
    // Track the per-element stride in slots as the projection walks
    // deeper. The initial stride is the root local's `elem_slots`;
    // after an `Index(_)` step we're inside an element, whose own
    // `elem_slots` is `1` unless we later add nested-array
    // tracking.
    let mut stride_slots = intrinsics
        .elem_slots
        .get(&place.local)
        .copied()
        .or_else(|| stride_slots_from_ty(tcx, body.local_ty(place.local)))
        .unwrap_or(1);
    for projection in &place.projection {
        match projection {
            Projection::Field(idx) => {
                let offset = builder.ins().iconst(ptr_ty, i64::from(*idx) * 8);
                current = builder.ins().iadd(current, offset);
            }
            Projection::Index(index_local) => {
                let index_var =
                    ensure_var(builder, locals, body, tcx, module, *index_local);
                let idx_val = builder.use_var(index_var);
                let idx_ptr = match value_type(idx_val, builder) {
                    t if t == ptr_ty => idx_val,
                    t if t == types::I64 && ptr_ty == types::I32 => {
                        builder.ins().ireduce(ptr_ty, idx_val)
                    }
                    t if t == types::I32 && ptr_ty == types::I64 => {
                        builder.ins().uextend(ptr_ty, idx_val)
                    }
                    _ => idx_val,
                };
                let stride = builder
                    .ins()
                    .iconst(ptr_ty, i64::from(stride_slots) * 8);
                let byte_offset = builder.ins().imul(idx_ptr, stride);
                current = builder.ins().iadd(current, byte_offset);
                // After indexing, we're inside a single element;
                // subsequent Field projections use the base-1
                // stride already baked into their scalar offsets.
                stride_slots = 1;
            }
            Projection::Deref => {
                // `*ptr`: the local already holds a pointer; after
                // this projection the address is just that pointer
                // value. Subsequent Field/Index projections
                // compute offsets off of it.
                let loaded = builder.ins().load(
                    ptr_ty,
                    MemFlags::trusted(),
                    current,
                    0,
                );
                current = loaded;
                stride_slots = 1;
            }
            Projection::Discriminant => {
                // Discriminant lives at offset 0 of an enum's
                // backing storage. The following load reads it as
                // i64.
                // No offset change; subsequent projections read
                // the tag word directly.
                stride_slots = 1;
            }
            Projection::Downcast(_) => {
                // Downcast skips past the tag word to the payload.
                let tag_bytes = builder.ins().iconst(ptr_ty, 8);
                current = builder.ins().iadd(current, tag_bytes);
                stride_slots = 1;
            }
        }
    }
    Ok(current)
}

/// Emits a store of `value` through `place`'s projection chain.
/// The leaf type chooses the store width (F64/I64/I32/I16/I8).
#[allow(clippy::too_many_arguments)]
fn lower_place_store(
    module: &dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    value: ir::Value,
    leaf_ty: ir::Type,
    intrinsics: &IntrinsicContext,
) -> Result<()> {
    let addr = lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?;
    // Coerce the value to the leaf's cranelift type where possible;
    // bail loudly when that would be lossy.
    let coerced = coerce_store_value(builder, value, leaf_ty)?;
    builder.ins().store(MemFlags::trusted(), coerced, addr, 0);
    Ok(())
}

/// Stores a call/intrinsic return value into `destination`.
/// When the destination is a bare local, declares the Variable and
/// records its runtime cl type. When the destination carries a
/// projection chain (`s.field = f()`, `a[i] = f()`), runs the
/// existing place-store path: pick the leaf cl type from the
/// projection, then emit a `store` through it.
#[allow(clippy::too_many_arguments)]
fn store_call_result(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    destination: &Place,
    value: ir::Value,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    if destination.projection.is_empty() {
        let ret_ty = value_type(value, builder);
        intrinsics.local_declared_ty.insert(destination.local, ret_ty);
        define_var_to(builder, locals, body, tcx, module, destination.local, value);
        return Ok(());
    }
    let elem_hint = intrinsics.elem_cl_ty.get(&destination.local).copied();
    let leaf_ty = resolve_place_cl_type(
        tcx,
        body,
        destination,
        module,
        elem_hint.or(Some(value_type(value, builder))),
    );
    lower_place_store(
        module, builder, locals, body, tcx, destination, value, leaf_ty, intrinsics,
    )
}

/// Coerce a value to the cranelift type expected by a call-site or
/// store. Handles the two common mismatches: i64 ↔ f64 (bitcast),
/// and widening/narrowing between integer widths.
fn coerce_arg_to(
    builder: &mut FunctionBuilder<'_>,
    value: ir::Value,
    want: ir::Type,
) -> Result<ir::Value> {
    let have = value_type(value, builder);
    if have == want {
        return Ok(value);
    }
    if have == types::I64 && want == types::F64 {
        return Ok(builder.ins().bitcast(types::F64, ir::MemFlags::new(), value));
    }
    if have == types::F64 && want == types::I64 {
        return Ok(builder.ins().bitcast(types::I64, ir::MemFlags::new(), value));
    }
    if have.is_int() && want.is_int() {
        if have.bits() > want.bits() {
            return Ok(builder.ins().ireduce(want, value));
        }
        if have.bits() < want.bits() {
            return Ok(builder.ins().uextend(want, value));
        }
    }
    if have.is_float() && want.is_float() {
        if have.bits() > want.bits() {
            return Ok(builder.ins().fdemote(want, value));
        }
        if have.bits() < want.bits() {
            return Ok(builder.ins().fpromote(want, value));
        }
    }
    bail!("native codegen: cannot coerce {have:?} -> {want:?}")
}

fn coerce_store_value(
    builder: &mut FunctionBuilder<'_>,
    value: ir::Value,
    leaf_ty: ir::Type,
) -> Result<ir::Value> {
    let src = value_type(value, builder);
    if src == leaf_ty {
        return Ok(value);
    }
    // Narrowing integer store: truncate with `ireduce`.
    if src.is_int() && leaf_ty.is_int() {
        if src.bits() > leaf_ty.bits() {
            return Ok(builder.ins().ireduce(leaf_ty, value));
        }
        if src.bits() < leaf_ty.bits() {
            // Caller wrote a narrower value into a wider slot. Safe
            // zero-extend (all sites today are same-width by
            // construction; this branch just avoids a crash).
            return Ok(builder.ins().uextend(leaf_ty, value));
        }
    }
    if src.is_float() && leaf_ty.is_float() && src.bits() != leaf_ty.bits() {
        if src.bits() > leaf_ty.bits() {
            return Ok(builder.ins().fdemote(leaf_ty, value));
        }
        return Ok(builder.ins().fpromote(leaf_ty, value));
    }
    // Cross-kind int↔float store: reinterpret the bits. Real
    // numeric-cast logic lives in `Rvalue::Cast`; a raw
    // aggregate-slot write gets the bit pattern through.
    if src.bits() == leaf_ty.bits() && src != leaf_ty {
        return Ok(builder.ins().bitcast(leaf_ty, ir::MemFlags::new(), value));
    }
    bail!("native codegen: cannot coerce store {src:?} -> {leaf_ty:?}");
}

#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
fn lower_terminator(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    blocks: &mut HashMap<u32, ir::Block>,
    callees_by_def: &HashMap<u32, ir::FuncRef>,
    callees_by_name: &HashMap<String, ir::FuncRef>,
    terminator: &Terminator,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    match terminator {
        Terminator::Goto { target } => {
            let block = blocks[&target.as_u32()];
            builder.ins().jump(block, &[]);
        }
        Terminator::Return => {
            let retval = match locals.get(&Local(0)).copied() {
                Some(var) => builder.use_var(var),
                None => builder.ins().iconst(types::I64, 0),
            };
            builder.ins().return_(&[retval]);
        }
        Terminator::Call {
            callee,
            args,
            destination,
            target,
        } => {
            // Runtime-intrinsic shortcut: calls to the prelude
            // `println` / `panic` don't reach user code — they land
            // in a C-ABI runtime function. MIR lowering carries the
            // callee name as a `Const(Str(...))` when the resolver
            // hasn't assigned a `DefId` (prelude values fall into
            // this bucket). `noreturn` intrinsics (panic) are
            // responsible for terminating the block themselves; the
            // fall-through `jump target` is skipped.
            if let Some(name) = callee_prelude_name(callee) {
                let outcome = lower_intrinsic_outcome(
                    &name, module, builder, locals, body, tcx, args, destination, intrinsics,
                )?;
                if outcome.handled {
                    if !outcome.noreturn {
                        match target {
                            Some(block_id) => {
                                let block = blocks[&block_id.as_u32()];
                                builder.ins().jump(block, &[]);
                            }
                            None => {
                                builder.ins().trap(ir::TrapCode::user(1).unwrap());
                            }
                        }
                    }
                    return Ok(());
                }
            }
            // Indirect call through a closure value. The callee
            // operand is the local that holds the closure's heap
            // env pointer. The env's first word is the real function
            // pointer; subsequent words carry the captures the lifted
            // function reads via `gos_load(env, 8*i)`. The callee's
            // signature is `(env_ptr, args…) -> i64`.
            if let Operand::Copy(place) = callee {
                let ptr_ty = module.target_config().pointer_type();
                // Two shapes hide behind the "Copy(local) callee":
                //   1. Closure env: local holds a pointer to a
                //      heap record whose first word is the lifted
                //      function's address. Indirect-call through
                //      `load(env+0)` with `env` as the implicit
                //      first arg.
                //   2. Plain function pointer: local is a
                //      `FnDef`-typed value obtained from an
                //      `Operand::FnRef` — its value IS the function
                //      address directly. No `env` prelude, no
                //      leading load. `f(x)` becomes a straight
                //      `call_indirect(addr, x)`.
                let callee_ty = body.local_ty(place.local);
                let is_plain_fn = matches!(
                    tcx.kind_of(callee_ty),
                    TyKind::FnDef { .. } | TyKind::FnPtr(_)
                );
                let env_value =
                    lower_place_read(module, builder, locals, body, tcx, place, None, intrinsics)?;
                let env_ptr = if ptr_ty == types::I64 {
                    env_value
                } else {
                    builder.ins().ireduce(ptr_ty, env_value)
                };
                let fn_ptr = if is_plain_fn {
                    env_ptr
                } else {
                    builder.ins().load(
                        ptr_ty,
                        MemFlags::trusted(),
                        env_ptr,
                        ir::immediates::Offset32::new(0),
                    )
                };
                let mut sig = module.make_signature();
                if !is_plain_fn {
                    sig.params.push(AbiParam::new(types::I64));
                }
                for _ in args {
                    sig.params.push(AbiParam::new(types::I64));
                }
                sig.returns.push(AbiParam::new(types::I64));
                let sig_ref = builder.import_signature(sig);
                let mut arg_values = Vec::with_capacity(args.len() + 1);
                if !is_plain_fn {
                    arg_values.push(env_value);
                }
                for op in args {
                    arg_values.push(lower_operand(
                        module, builder, locals, body, tcx, op, None, intrinsics,
                    )?);
                }
                let call = builder.ins().call_indirect(sig_ref, fn_ptr, &arg_values);
                let results = builder.inst_results(call).to_vec();
                if let Some(&ret) = results.first() {
                    store_call_result(
                        module, builder, locals, body, tcx, destination, ret, intrinsics,
                    )?;
                }
                match target {
                    Some(block_id) => {
                        let block = blocks[&block_id.as_u32()];
                        builder.ins().jump(block, &[]);
                    }
                    None => {
                        builder.ins().trap(ir::TrapCode::user(1).unwrap());
                    }
                }
                return Ok(());
            }
            // First try resolving a `Const(Str("name"))` callee
            // against the module's function table — closures lifted
            // by `lift_closures` appear here as `Const(Str)` when the
            // MIR lowerer records them via `local_fn_name`. Only fall
            // through to the runtime diagnostic stub when the name
            // is genuinely unknown.
            if let Operand::Const(ConstValue::Str(name)) = callee {
                if let Some(func_ref) = callees_by_name.get(name).copied() {
                    let expected = builder
                        .func
                        .dfg
                        .signatures
                        .get(builder.func.dfg.ext_funcs[func_ref].signature)
                        .map(|s| s.params.iter().map(|p| p.value_type).collect::<Vec<_>>())
                        .unwrap_or_default();
                    let mut arg_values: Vec<ir::Value> = Vec::with_capacity(args.len());
                    for (idx, op) in args.iter().enumerate() {
                        let mut v = lower_operand(
                            module, builder, locals, body, tcx, op, None, intrinsics,
                        )?;
                        if let Some(want) = expected.get(idx).copied() {
                            v = coerce_arg_to(builder, v, want)?;
                        }
                        arg_values.push(v);
                    }
                    let call = builder.ins().call(func_ref, &arg_values);
                    let results = builder.inst_results(call).to_vec();
                    if let Some(&ret) = results.first() {
                        store_call_result(
                            module, builder, locals, body, tcx, destination, ret, intrinsics,
                        )?;
                    }
                    match target {
                        Some(block_id) => {
                            let block = blocks[&block_id.as_u32()];
                            builder.ins().jump(block, &[]);
                        }
                        None => {
                            builder.ins().trap(ir::TrapCode::user(1).unwrap());
                        }
                    }
                    return Ok(());
                }
                // Stdlib-shaped callees (`std::...`, `fmt::...`,
                // `os::...`, `sync::...`, …) plus enum-variant
                // constructors (`Ok`, `Err`, `Some`, `None`, user
                // enums that start with an uppercase letter) and
                // anything else the codegen has not wired default
                // to a zero-return stub so the program still
                // builds. Semantics match the call returning a
                // default value of its declared type. This is a
                // deliberate L1 compromise; L2 replaces stubs with
                // real runtime symbols.
                let is_variant = name
                    .chars()
                    .next()
                    .is_some_and(char::is_uppercase);
                if name.contains("::") || is_variant {
                    let zero = builder.ins().iconst(types::I64, 0);
                    store_call_result(
                        module, builder, locals, body, tcx, destination, zero, intrinsics,
                    )?;
                    match target {
                        Some(block_id) => {
                            let block = blocks[&block_id.as_u32()];
                            builder.ins().jump(block, &[]);
                        }
                        None => {
                            builder.ins().trap(ir::TrapCode::user(1).unwrap());
                        }
                    }
                    return Ok(());
                }
                bail!(
                    "native codegen: unresolved callee `{name}` — re-run with `gos run`"
                );
            }
            let func_ref = resolve_callee(callee, callees_by_def, callees_by_name)?;
            let expected = builder
                .func
                .dfg
                .signatures
                .get(builder.func.dfg.ext_funcs[func_ref].signature)
                .map(|s| s.params.iter().map(|p| p.value_type).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut arg_values: Vec<ir::Value> = Vec::with_capacity(args.len());
            for (idx, op) in args.iter().enumerate() {
                let mut v = lower_operand(
                    module, builder, locals, body, tcx, op, None, intrinsics,
                )?;
                if let Some(want) = expected.get(idx).copied() {
                    v = coerce_arg_to(builder, v, want)?;
                }
                arg_values.push(v);
            }
            let call = builder.ins().call(func_ref, &arg_values);
            let results = builder.inst_results(call).to_vec();
            if let Some(&ret) = results.first() {
                store_call_result(
                    module, builder, locals, body, tcx, destination, ret, intrinsics,
                )?;
            }
            match target {
                Some(block_id) => {
                    let block = blocks[&block_id.as_u32()];
                    builder.ins().jump(block, &[]);
                }
                None => {
                    builder.ins().trap(ir::TrapCode::user(1).unwrap());
                }
            }
        }
        Terminator::SwitchInt {
            discriminant,
            arms,
            default,
        } => {
            let value = lower_operand(module, builder, locals, body, tcx, discriminant, None, intrinsics)?;
            let value_ty = value_type(value, builder);
            let default_block = blocks[&default.as_u32()];
            // Chain a compare-and-branch per arm, falling through
            // to the next compare on a miss. Cranelift's optimiser
            // collapses the chain into a jump table for dense arms.
            for (arm_value, arm_target) in arms {
                let arm_block = blocks[&arm_target.as_u32()];
                let next = builder.create_block();
                // Match the discriminant's cranelift type; bool
                // discriminants come back as i8, smaller ints as
                // their natural width.
                let cmp_value = builder.ins().iconst(value_ty, i64_truncate(*arm_value));
                let matched = builder.ins().icmp(
                    ir::condcodes::IntCC::Equal,
                    value,
                    cmp_value,
                );
                builder.ins().brif(matched, arm_block, &[], next, &[]);
                builder.switch_to_block(next);
            }
            builder.ins().jump(default_block, &[]);
        }
        Terminator::Assert {
            cond,
            expected,
            target,
            ..
        } => {
            let value = lower_operand(module, builder, locals, body, tcx, cond, None, intrinsics)?;
            let value_ty = value_type(value, builder);
            // `expected` is a bool; coerce the constant to whatever
            // width the cond produces.
            let expected_value =
                builder.ins().iconst(value_ty, i64::from(*expected));
            let pass = builder.create_block();
            let fail = builder.create_block();
            let matched = builder.ins().icmp(
                ir::condcodes::IntCC::Equal,
                value,
                expected_value,
            );
            builder.ins().brif(matched, pass, &[], fail, &[]);
            builder.switch_to_block(fail);
            builder.ins().trap(ir::TrapCode::user(3).unwrap());
            builder.switch_to_block(pass);
            let block = blocks[&target.as_u32()];
            builder.ins().jump(block, &[]);
        }
        Terminator::Panic { .. } => {
            builder.ins().trap(ir::TrapCode::user(4).unwrap());
        }
        Terminator::Drop { target, .. } => {
            // No destructors to run today; treat the drop as a
            // direct jump and revisit once real RAII semantics
            // land in MIR.
            let block = blocks[&target.as_u32()];
            builder.ins().jump(block, &[]);
        }
        Terminator::Unreachable => {
            builder.ins().trap(ir::TrapCode::user(2).unwrap());
        }
    }
    Ok(())
}

/// When `operand` is a `Const(Str("…"))` callee — the shape the HIR
/// lowerer uses for prelude values that don't have a resolver
/// `DefId` — returns the string. The caller compares against the
/// known intrinsic names (`println`, `panic`, …) to decide whether
/// to route the call into the native runtime.
fn callee_prelude_name(operand: &Operand) -> Option<String> {
    match operand {
        Operand::Const(ConstValue::Str(s)) => Some(s.clone()),
        _ => None,
    }
}

/// Outcome of [`lower_intrinsic_outcome`]: whether the intrinsic
/// was handled and whether the generated code is a terminator
/// (noreturn).
struct IntrinsicOutcome {
    handled: bool,
    noreturn: bool,
}

/// Emits one runtime print call per argument, dispatching by the
/// argument's MIR/cranelift type. When `separator` is non-empty,
/// emits a `gos_rt_print_str(separator)` call between each pair of
/// args (used by `println(a, b, c)` for space separation; empty
/// for `__concat`'s direct concatenation).
#[allow(clippy::too_many_arguments)]
fn emit_per_arg_print(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    separator: &str,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let print_str =
        intrinsics.extern_fn(module, "gos_rt_print_str", &[ptr_ty], &[])?;
    let print_i64 =
        intrinsics.extern_fn(module, "gos_rt_print_i64", &[types::I64], &[])?;
    let print_f64 =
        intrinsics.extern_fn(module, "gos_rt_print_f64", &[types::F64], &[])?;
    let print_bool =
        intrinsics.extern_fn(module, "gos_rt_print_bool", &[types::I32], &[])?;
    let print_char =
        intrinsics.extern_fn(module, "gos_rt_print_char", &[types::I32], &[])?;
    let sep_data = if separator.is_empty() {
        None
    } else {
        Some(intrinsics.intern_string(module, separator)?)
    };
    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            if let Some(data) = sep_data {
                let data_ref = module.declare_data_in_func(data, builder.func);
                let ptr = builder.ins().global_value(ptr_ty, data_ref);
                let fref = module.declare_func_in_func(print_str, builder.func);
                builder.ins().call(fref, &[ptr]);
            }
        }
        let kind = operand_print_kind(body, tcx, arg);
        if let PrintKind::Unsupported(label) = kind {
            bail!(
                "native codegen: cannot print a value of {label} type — \
                 the compiled tier has no Display dispatch yet. Stringify \
                 it first (e.g. via `format!(\"{{x:?}}\")` once that lands, \
                 or by writing the field-by-field form by hand)."
            );
        }
        let value =
            lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?;
        let ty = value_type(value, builder);
        match kind {
            PrintKind::StrPtr => {
                let fref = module.declare_func_in_func(print_str, builder.func);
                builder.ins().call(fref, &[value]);
            }
            PrintKind::Int => {
                let n = if ty.bits() < 64 {
                    builder.ins().sextend(types::I64, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(print_i64, builder.func);
                builder.ins().call(fref, &[n]);
            }
            PrintKind::Float => {
                let d = if ty == types::F32 {
                    builder.ins().fpromote(types::F64, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(print_f64, builder.func);
                builder.ins().call(fref, &[d]);
            }
            PrintKind::Bool => {
                let b = if ty.bits() < 32 {
                    builder.ins().uextend(types::I32, value)
                } else if ty.bits() > 32 {
                    builder.ins().ireduce(types::I32, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(print_bool, builder.func);
                builder.ins().call(fref, &[b]);
            }
            PrintKind::Char => {
                let c = if ty.bits() > 32 {
                    builder.ins().ireduce(types::I32, value)
                } else if ty.bits() < 32 {
                    builder.ins().uextend(types::I32, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(print_char, builder.func);
                builder.ins().call(fref, &[c]);
            }
            PrintKind::Unsupported(_) => unreachable!("checked above"),
        }
    }
    Ok(())
}

/// Concatenates the stringification of every argument into a
/// single heap-allocated c-string and returns its pointer. Used by
/// `panic(args...)` so multi-arg panics produce a single
/// formatted message before aborting. Each arg is converted to a
/// string through the same per-type dispatch as
/// [`emit_per_arg_print`]: strings pass through, integers go
/// through `gos_rt_i64_to_str`, floats through `gos_rt_f64_to_str`,
/// bools through `gos_rt_bool_to_str`, chars through
/// `gos_rt_char_to_str`. Pieces are joined with `separator`
/// (empty for tight concat, " " for println-shaped joining).
#[allow(clippy::too_many_arguments)]
fn emit_args_to_concat_string(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    separator: &str,
) -> Result<ir::Value> {
    let ptr_ty = module.target_config().pointer_type();
    let i64_to_str =
        intrinsics.extern_fn(module, "gos_rt_i64_to_str", &[types::I64], &[ptr_ty])?;
    let f64_to_str =
        intrinsics.extern_fn(module, "gos_rt_f64_to_str", &[types::F64], &[ptr_ty])?;
    let bool_to_str =
        intrinsics.extern_fn(module, "gos_rt_bool_to_str", &[types::I32], &[ptr_ty])?;
    let char_to_str =
        intrinsics.extern_fn(module, "gos_rt_char_to_str", &[types::I32], &[ptr_ty])?;
    let str_concat =
        intrinsics.extern_fn(module, "gos_rt_str_concat", &[ptr_ty, ptr_ty], &[ptr_ty])?;
    let sep_data = if separator.is_empty() {
        None
    } else {
        Some(intrinsics.intern_string(module, separator)?)
    };
    let empty_data = intrinsics.intern_string(module, "")?;

    fn arg_to_str_ptr(
        module: &mut dyn Module,
        builder: &mut FunctionBuilder<'_>,
        locals: &mut HashMap<Local, Variable>,
        body: &Body,
        tcx: &TyCtxt,
        arg: &Operand,
        intrinsics: &mut IntrinsicContext,
        i64_to_str: cranelift_module::FuncId,
        f64_to_str: cranelift_module::FuncId,
        bool_to_str: cranelift_module::FuncId,
        char_to_str: cranelift_module::FuncId,
    ) -> Result<ir::Value> {
        let kind = operand_print_kind(body, tcx, arg);
        if let PrintKind::Unsupported(label) = kind {
            bail!(
                "native codegen: cannot stringify a value of {label} type — \
                 the compiled tier has no Display dispatch yet"
            );
        }
        let value = lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?;
        let ty = value_type(value, builder);
        let ptr = match kind {
            PrintKind::StrPtr => value,
            PrintKind::Int => {
                let n = if ty.bits() < 64 {
                    builder.ins().sextend(types::I64, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(i64_to_str, builder.func);
                let call = builder.ins().call(fref, &[n]);
                builder.inst_results(call)[0]
            }
            PrintKind::Float => {
                let d = if ty == types::F32 {
                    builder.ins().fpromote(types::F64, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(f64_to_str, builder.func);
                let call = builder.ins().call(fref, &[d]);
                builder.inst_results(call)[0]
            }
            PrintKind::Bool => {
                let b = if ty.bits() < 32 {
                    builder.ins().uextend(types::I32, value)
                } else if ty.bits() > 32 {
                    builder.ins().ireduce(types::I32, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(bool_to_str, builder.func);
                let call = builder.ins().call(fref, &[b]);
                builder.inst_results(call)[0]
            }
            PrintKind::Char => {
                let c = if ty.bits() > 32 {
                    builder.ins().ireduce(types::I32, value)
                } else if ty.bits() < 32 {
                    builder.ins().uextend(types::I32, value)
                } else {
                    value
                };
                let fref = module.declare_func_in_func(char_to_str, builder.func);
                let call = builder.ins().call(fref, &[c]);
                builder.inst_results(call)[0]
            }
            PrintKind::Unsupported(_) => unreachable!("checked above"),
        };
        Ok(ptr)
    }

    if args.is_empty() {
        let data_ref = module.declare_data_in_func(empty_data, builder.func);
        return Ok(builder.ins().global_value(ptr_ty, data_ref));
    }
    let mut acc = arg_to_str_ptr(
        module, builder, locals, body, tcx, &args[0], intrinsics,
        i64_to_str, f64_to_str, bool_to_str, char_to_str,
    )?;
    for arg in &args[1..] {
        if let Some(data) = sep_data {
            let data_ref = module.declare_data_in_func(data, builder.func);
            let sep_ptr = builder.ins().global_value(ptr_ty, data_ref);
            let fref = module.declare_func_in_func(str_concat, builder.func);
            let call = builder.ins().call(fref, &[acc, sep_ptr]);
            acc = builder.inst_results(call)[0];
        }
        let next = arg_to_str_ptr(
            module, builder, locals, body, tcx, arg, intrinsics,
            i64_to_str, f64_to_str, bool_to_str, char_to_str,
        )?;
        let fref = module.declare_func_in_func(str_concat, builder.func);
        let call = builder.ins().call(fref, &[acc, next]);
        acc = builder.inst_results(call)[0];
    }
    Ok(acc)
}

#[allow(clippy::too_many_arguments)]
fn lower_intrinsic_outcome(
    name: &str,
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    destination: &gossamer_mir::Place,
    intrinsics: &mut IntrinsicContext,
) -> Result<IntrinsicOutcome> {
    let handled = lower_intrinsic_call(
        module, builder, locals, body, tcx, args, name, destination, intrinsics,
    )?;
    let noreturn = handled && matches!(name, "panic");
    Ok(IntrinsicOutcome { handled, noreturn })
}

/// Emits a call into the C-ABI native runtime for a recognised
/// prelude name. Returns `Ok(true)` when the call was routed;
/// `Ok(false)` when `name` is not a known intrinsic (the caller
/// then falls back to the generic call path).
#[allow(
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::cognitive_complexity,
    reason = "intrinsic dispatch table — splitting it hides the one-arm-per-symbol structure",
)]
fn lower_intrinsic_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    name: &str,
    destination: &gossamer_mir::Place,
    intrinsics: &mut IntrinsicContext,
) -> Result<bool> {
    let ptr_ty = module.target_config().pointer_type();
    match name {
        "__concat" => {
            // Parser lowers `println!("a={}", n)` to
            // `println(__concat("a=", n))`. Instead of building an
            // intermediate String, emit one `printf`-family call
            // per argument — each prints directly to stdout in
            // order, and the surrounding `println`/`print`/…
            // consumes an empty-string pointer (so `println` still
            // contributes the trailing newline via `puts("")`).
            //
            // `format!` with multiple args does not currently reach
            // native codegen cleanly — its return value is a
            // pointer to the empty rodata slot, which is incorrect
            // for programs that consume the formatted string beyond
            // `println`. Documented as a known gap.
            //
            // Empty separator: `__concat` is a tight join (used to
            // expand `println!("a={n}")` into `__concat("a=", n)`).
            // `println(a, b, c)` uses the space-separated form
            // below.
            emit_per_arg_print(module, builder, locals, body, tcx, args, intrinsics, "")?;
            // The destination local holds a String pointer; feed it
            // an empty rodata string so the surrounding
            // `println`/`print`/… can still call puts/fputs on it.
            let empty = intrinsics.intern_string(module, "")?;
            let empty_ref = module.declare_data_in_func(empty, builder.func);
            let empty_ptr = builder.ins().global_value(ptr_ty, empty_ref);
            if !destination.projection.is_empty() {
                bail!("native codegen: __concat destination cannot have projections");
            }
            define_var_to(builder, locals, body, tcx, module, destination.local, empty_ptr);
            Ok(true)
        }
        // `io::stdout()` / `io::stderr()` / `io::stdin()` —
        // return an opaque pointer to a static `GosStream`.
        // Method dispatch on the returned value routes to the
        // `gos_rt_stream_*` helpers below.
        "io::stdout" | "io::stderr" | "io::stdin" => {
            let rt_name = match name {
                "io::stdout" => "gos_rt_io_stdout",
                "io::stderr" => "gos_rt_io_stderr",
                "io::stdin" => "gos_rt_io_stdin",
                _ => unreachable!(),
            };
            let rt_fn = intrinsics.extern_fn(module, rt_name, &[], &[ptr_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        // Method-side routing for stream values. The MIR
        // method-dispatch table maps `stream.write_byte(b)`
        // etc. to these symbols (`receiver` is arg 0).
        "gos_rt_stream_write_byte" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_stream_write_byte",
                &[ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let b64 = coerce_arg_to(builder, b, types::I64)?;
            let _ = builder.ins().call(fref, &[stream, b64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_stream_write_byte_array" => {
            // Bulk byte write — `out.write_byte_array(arr, len)`.
            // `arr` is a `[i64; N]` whose flat-slot layout
            // means each byte sits in the low 8 bits of an
            // `i64`; the runtime walks it once and packs into
            // the stdout buffer.
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_stream_write_byte_array",
                &[ptr_ty, ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let arr = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let len = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let _ = builder.ins().call(fref, &[stream, arr, len64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_stream_write_str" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_stream_write_str",
                &[ptr_ty, ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[stream, s]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_stream_flush" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_stream_flush",
                &[ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[stream]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_stream_read_line" | "gos_rt_stream_read_to_string" => {
            let rt_name: &'static str = match name {
                "gos_rt_stream_read_line" => "gos_rt_stream_read_line",
                _ => "gos_rt_stream_read_to_string",
            };
            let rt_fn = intrinsics.extern_fn(
                module,
                rt_name,
                &[ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[stream]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "println" | "print" | "eprintln" | "eprint" => {
            // Per-arg dispatch: each operand is printed through
            // the runtime helper matching its MIR type
            // (`gos_rt_print_str` for strings, `_i64` for
            // integers, `_f64` for floats, `_bool` / `_char`).
            // This is the same machinery `__concat` uses; bare
            // `println(5i64)` and interpolated `println!("{n}")`
            // therefore share one code path.
            //
            // (The runtime doesn't yet split stderr, so `eprint*`
            // currently shares the stdout writer; that's a
            // separate gap, not this one.)
            //
            // Only `println` / `eprintln` append the trailing
            // newline + flush via `gos_rt_println()`.
            //
            // Spec: each arg is space-separated. Mirror the
            // interpreter's `render_args` (which inserts a `' '`
            // between each pair).
            emit_per_arg_print(module, builder, locals, body, tcx, args, intrinsics, " ")?;
            if matches!(name, "println" | "eprintln") {
                let println_fn = intrinsics.extern_fn(
                    module,
                    "gos_rt_println",
                    &[],
                    &[],
                )?;
                let pl_ref = module.declare_func_in_func(println_fn, builder.func);
                let _ = builder.ins().call(pl_ref, &[]);
            }
            if !destination.projection.is_empty() {
                bail!("native codegen: intrinsic destination cannot have projections");
            }
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, zero);
            Ok(true)
        }
        "gos_fn_addr" => {
            // Returns the address of a named function as an i64 so
            // closures and other first-class callable values can
            // stash a function pointer in their heap env. The
            // argument is a `Const(Str(name))` naming the target.
            let Some(Operand::Const(ConstValue::Str(name))) = args.first() else {
                bail!("native codegen: gos_fn_addr requires a const-string name argument");
            };
            let func_id = *intrinsics
                .functions
                .get(name)
                .ok_or_else(|| anyhow!("gos_fn_addr: unknown fn `{name}`"))?;
            let func_ref = module.declare_func_in_func(func_id, builder.func);
            let addr = builder.ins().func_addr(ptr_ty, func_ref);
            let as_i64 = if ptr_ty == types::I64 {
                addr
            } else {
                builder.ins().uextend(types::I64, addr)
            };
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_fn_addr destination cannot have projections");
            }
            define_var_to(builder, locals, body, tcx, module, destination.local, as_i64);
            Ok(true)
        }
        "gos_alloc" => {
            // Heap allocator primitive: forwards to libc `malloc`.
            // Single argument is the size in bytes; the return value
            // is a raw pointer (i64 on 64-bit, zero-extended on 32-bit).
            let malloc = intrinsics.extern_fn(module, "malloc", &[ptr_ty], &[ptr_ty])?;
            let malloc_ref = module.declare_func_in_func(malloc, builder.func);
            let size_val = match args.first() {
                Some(arg) => lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let size_ptr = if ptr_ty == types::I64 {
                size_val
            } else {
                builder.ins().ireduce(ptr_ty, size_val)
            };
            let call_inst = builder.ins().call(malloc_ref, &[size_ptr]);
            let raw_ptr = builder.inst_results(call_inst)[0];
            let as_i64 = if ptr_ty == types::I64 {
                raw_ptr
            } else {
                builder.ins().uextend(types::I64, raw_ptr)
            };
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_alloc destination cannot have projections");
            }
            define_var_to(builder, locals, body, tcx, module, destination.local, as_i64);
            Ok(true)
        }
        "gos_store" => {
            // Raw heap store: `gos_store(ptr, offset, value)` writes
            // `value` as an i64 at `ptr + offset`. Companion to
            // `gos_load` + `gos_alloc`.
            if args.len() < 3 {
                bail!("native codegen: gos_store requires (ptr, offset, value)");
            }
            let ptr_val = lower_operand(module, builder, locals, body, tcx, &args[0], None, intrinsics)?;
            let offset_val = lower_operand(module, builder, locals, body, tcx, &args[1], None, intrinsics)?;
            let value = lower_operand(module, builder, locals, body, tcx, &args[2], None, intrinsics)?;
            let addr_i64 = builder.ins().iadd(ptr_val, offset_val);
            let addr = if ptr_ty == types::I64 {
                addr_i64
            } else {
                builder.ins().ireduce(ptr_ty, addr_i64)
            };
            builder.ins().store(
                MemFlags::trusted(),
                value,
                addr,
                ir::immediates::Offset32::new(0),
            );
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, zero);
            Ok(true)
        }
        "gos_load" => {
            // Raw heap load: `gos_load(ptr, offset)` reads an i64 at
            // `ptr + offset`.
            if args.len() < 2 {
                bail!("native codegen: gos_load requires (ptr, offset)");
            }
            let ptr_val = lower_operand(module, builder, locals, body, tcx, &args[0], None, intrinsics)?;
            let offset_val = lower_operand(module, builder, locals, body, tcx, &args[1], None, intrinsics)?;
            let addr_i64 = builder.ins().iadd(ptr_val, offset_val);
            let addr = if ptr_ty == types::I64 {
                addr_i64
            } else {
                builder.ins().ireduce(ptr_ty, addr_i64)
            };
            let loaded = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                addr,
                ir::immediates::Offset32::new(0),
            );
            define_var_to(builder, locals, body, tcx, module, destination.local, loaded);
            Ok(true)
        }
        "panic" => {
            // Route through `gos_rt_panic(msg)` after building a
            // single concatenated message from all arguments
            // (mirrors `render_args` in the interpreter — pieces
            // joined by a single space). Multi-arg
            // `panic("code=", 42)` previously dropped every arg
            // after the first.
            let panic_fn = intrinsics.extern_fn(
                module,
                "gos_rt_panic",
                &[ptr_ty],
                &[],
            )?;
            let panic_ref = module.declare_func_in_func(panic_fn, builder.func);
            let msg = if args.is_empty() {
                builder.ins().iconst(ptr_ty, 0)
            } else {
                emit_args_to_concat_string(
                    module, builder, locals, body, tcx, args, intrinsics, " ",
                )?
            };
            let _ = builder.ins().call(panic_ref, &[msg]);
            // `gos_rt_panic` is noreturn but Cranelift needs the
            // block to end in a terminator; emit an unreachable
            // trap so downstream jumps are correctly dead.
            builder.ins().trap(ir::TrapCode::user(4).unwrap());
            Ok(true)
        }
        // ----- Gossamer C-ABI runtime helpers -----
        // String concatenation delegates to the runtime shim.
        "gos_rt_str_concat" => {
            let concat_fn = intrinsics.extern_fn(
                module,
                "gos_rt_str_concat",
                &[ptr_ty, ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(concat_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let ptr = builder.inst_results(call)[0];
            if !destination.projection.is_empty() {
                bail!(
                    "native codegen: gos_rt_str_concat destination cannot have projections"
                );
            }
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        // Byte-at: `s[i]` on a `String` loads the `i`-th byte and
        // zero-extends to `i64` (matching the interpreter's
        // convention of returning byte codes as `i64`).
        "gos_rt_str_byte_at" => {
            let ptr = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, None, intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let idx = match args.get(1) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx_ptr = match value_type(idx, builder) {
                t if t == ptr_ty => idx,
                t if t == types::I64 && ptr_ty == types::I32 => {
                    builder.ins().ireduce(ptr_ty, idx)
                }
                t if t == types::I32 && ptr_ty == types::I64 => {
                    builder.ins().uextend(ptr_ty, idx)
                }
                _ => idx,
            };
            let addr = builder.ins().iadd(ptr, idx_ptr);
            let byte = builder.ins().load(types::I8, MemFlags::trusted(), addr, 0);
            let value = builder.ins().uextend(types::I64, byte);
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_rt_str_byte_at destination cannot have projections");
            }
            define_var_to(builder, locals, body, tcx, module, destination.local, value);
            Ok(true)
        }
        // String length: we treat `String` at the native ABI as a
        // nul-terminated pointer today, so `.len()` is plain
        // `strlen(ptr)`. Once the real `{ptr, len, cap}` header
        // ships this will route to a proper runtime symbol.
        "gos_rt_str_len" => {
            let strlen = intrinsics.extern_fn(
                module,
                "strlen",
                &[ptr_ty],
                &[types::I64],
            )?;
            let strlen_ref = module.declare_func_in_func(strlen, builder.func);
            let ptr = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, None, intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(strlen_ref, &[ptr]);
            let len = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, len);
            Ok(true)
        }
        // `os::args()` returns the program's argv as a
        // Vec<String>. The native runtime isn't wired yet; for
        // the build-to-native envelope we need a shape the
        // downstream `.len()`/`[0]` calls can consume. Returning
        // a null pointer and having `gos_rt_vec_len(null)` be 0
        // lets programs default their args.
        "gos_rt_os_args" | "os::args" => {
            // Forward to the runtime's `gos_rt_os_args`, which
            // returns a pointer to the first user argument
            // (`argv + 1`). Downstream `args.len()` routes
            // through `gos_rt_arr_len`, which short-circuits on
            // that pointer and returns the stashed `argc - 1`.
            // `args[i]` is a plain stride-8 Place projection
            // reading successive `char*` entries.
            let args_fn = intrinsics.extern_fn(
                module,
                "gos_rt_os_args",
                &[],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(args_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ret = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ret);
            Ok(true)
        }
        // `std::time::now()` — opaque monotonic clock value. Cast
        // a `libc::clock_gettime` result into an i64 ns-since-
        // epoch. For now, return 0 so programs that print the
        // current instant compile; the interpreter path already
        // returns a real value.
        "time::now" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_time_now",
                &[],
                &[types::F64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, v);
            Ok(true)
        }
        "time::now_ms" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_time_now_ms",
                &[],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, v);
            Ok(true)
        }
        // `std::math::*` — all (f64) -> f64 except where noted.
        "math::sqrt"
        | "math::sin"
        | "math::cos"
        | "math::ln"
        | "math::log"
        | "math::exp"
        | "math::abs"
        | "math::floor"
        | "math::ceil" => {
            let rt_name = match name {
                "math::sqrt" => "gos_rt_math_sqrt",
                "math::sin" => "gos_rt_math_sin",
                "math::cos" => "gos_rt_math_cos",
                "math::ln" | "math::log" => "gos_rt_math_log",
                "math::exp" => "gos_rt_math_exp",
                "math::abs" => "gos_rt_math_abs",
                "math::floor" => "gos_rt_math_floor",
                "math::ceil" => "gos_rt_math_ceil",
                _ => unreachable!(),
            };
            let rt_fn = intrinsics.extern_fn(
                module,
                rt_name,
                &[types::F64],
                &[types::F64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::F64), intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let call = builder.ins().call(fref, &[x64]);
            let v = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, v);
            Ok(true)
        }
        "math::pow" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_math_pow",
                &[types::F64, types::F64],
                &[types::F64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::F64), intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let y = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::F64), intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let y64 = coerce_arg_to(builder, y, types::F64)?;
            let call = builder.ins().call(fref, &[x64, y64]);
            let v = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, v);
            Ok(true)
        }
        "time::now_ns" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_now_ns",
                &[],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, v);
            Ok(true)
        }
        "gos_rt_go_spawn_call_0" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_0",
                &[ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[fn_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_1" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_1",
                &[ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0_i64 = coerce_arg_to(builder, a0, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0_i64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_2" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_2",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0_i64 = coerce_arg_to(builder, a0, types::I64)?;
            let a1_i64 = coerce_arg_to(builder, a1, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0_i64, a1_i64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_3" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_3",
                &[ptr_ty, types::I64, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a2 = match args.get(3) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0 = coerce_arg_to(builder, a0, types::I64)?;
            let a1 = coerce_arg_to(builder, a1, types::I64)?;
            let a2 = coerce_arg_to(builder, a2, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0, a1, a2]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_5" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_5",
                &[ptr_ty, types::I64, types::I64, types::I64, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let mut vals = Vec::with_capacity(5);
            for i in 1..=5 {
                let v = match args.get(i) {
                    Some(a) => lower_operand(
                        module, builder, locals, body, tcx, a, None, intrinsics,
                    )?,
                    None => builder.ins().iconst(types::I64, 0),
                };
                vals.push(coerce_arg_to(builder, v, types::I64)?);
            }
            let mut all_args = vec![fn_addr];
            all_args.extend(vals);
            let _ = builder.ins().call(fref, &all_args);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_6" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_6",
                &[
                    ptr_ty, types::I64, types::I64, types::I64, types::I64, types::I64,
                    types::I64,
                ],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let mut vals = Vec::with_capacity(6);
            for i in 1..=6 {
                let v = match args.get(i) {
                    Some(a) => lower_operand(
                        module, builder, locals, body, tcx, a, None, intrinsics,
                    )?,
                    None => builder.ins().iconst(types::I64, 0),
                };
                vals.push(coerce_arg_to(builder, v, types::I64)?);
            }
            let mut all_args = vec![fn_addr];
            all_args.extend(vals);
            let _ = builder.ins().call(fref, &all_args);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_go_spawn_call_4" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_4",
                &[ptr_ty, types::I64, types::I64, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a2 = match args.get(3) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a3 = match args.get(4) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0 = coerce_arg_to(builder, a0, types::I64)?;
            let a1 = coerce_arg_to(builder, a1, types::I64)?;
            let a2 = coerce_arg_to(builder, a2, types::I64)?;
            let a3 = coerce_arg_to(builder, a3, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0, a1, a2, a3]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "sync::yield_now" | "runtime::yield_now" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_yield",
                &[],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let _ = builder.ins().call(fref, &[]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "time::sleep" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_sleep_ns",
                &[types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let ns = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let ns = coerce_arg_to(builder, ns, types::I64)?;
            let _ = builder.ins().call(fref, &[ns]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // `std::strconv::parse_i64(s)` / `parse_f64(s)` — route
        // to the runtime. Ignore the `ok` out-parameter the
        // runtime exposes; callers that care about success take
        // the interpreter path. A real `Result<T, ParseError>`
        // path needs enum-with-payload support.
        // Numeric-to-String formatters (used by `42.to_string()`
        // and `3.14.to_string()`).
        "gos_rt_i64_to_str" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_i64_to_str",
                &[types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let n = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let call = builder.ins().call(fref, &[n64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_f64_to_str" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_f64_to_str",
                &[types::F64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::F64), intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let call = builder.ins().call(fref, &[x64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "strconv::parse_i64" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_parse_i64",
                &[ptr_ty, ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let null = builder.ins().iconst(ptr_ty, 0);
            let call = builder.ins().call(fref, &[s, null]);
            let n = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, n);
            Ok(true)
        }
        "strconv::parse_f64" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_parse_f64",
                &[ptr_ty, ptr_ty],
                &[types::F64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let null = builder.ins().iconst(ptr_ty, 0);
            let call = builder.ins().call(fref, &[s, null]);
            let x = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, x);
            Ok(true)
        }
        // `std::http::serve(addr, handler)` — start a blocking
        // TCP listener on `addr`. The handler is ignored today;
        // every request gets a static 200 response. The runtime
        // function itself never returns, but we leave the outer
        // terminator path (jump to next block) in place so
        // Cranelift's verifier stays happy — the jump is dead.
        "http::serve" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_http_serve",
                &[ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let addr = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // `std::os::exit(code)` — route through `gos_rt_exit`
        // (which calls `std::process::exit` — identical behavior
        // to libc's `exit`, but keeps every syscall that touches
        // process state inside the runtime crate).
        "os::exit" => {
            let exit = intrinsics.extern_fn(
                module,
                "gos_rt_exit",
                &[types::I32],
                &[],
            )?;
            let exit_ref = module.declare_func_in_func(exit, builder.func);
            let code = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I32, 0),
            };
            let code32 = match value_type(code, builder) {
                t if t == types::I32 => code,
                t if t.is_int() && t.bits() > 32 => builder.ins().ireduce(types::I32, code),
                _ => code,
            };
            let _ = builder.ins().call(exit_ref, &[code32]);
            builder.ins().trap(ir::TrapCode::user(1).unwrap());
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, zero);
            Ok(true)
        }
        // `Vec::new` / `Vec::with_capacity` — elem width is
        // hard-coded to 8 bytes (one word). All scalars and all
        // GC pointers fit; matches the flat slot layout the
        // codegen uses for aggregates.
        "Vec::new" | "gos_rt_vec_new" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_new",
                &[types::I32],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let eb = builder.ins().iconst(types::I32, 8);
            let call = builder.ins().call(fref, &[eb]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "Vec::with_capacity" | "gos_rt_vec_with_capacity" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_with_capacity",
                &[types::I32, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let eb = builder.ins().iconst(types::I32, 8);
            let cap = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let cap64 = coerce_arg_to(builder, cap, types::I64)?;
            let call = builder.ins().call(fref, &[eb, cap64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        // HashMap runtime. Key/value widths are hard-coded to 8
        // bytes (one word each) — matches the codegen's flat-
        // slot representation. Real per-type sizing needs MIR
        // plumbing that L3 didn't cover.
        "HashMap::new" | "collections::HashMap::new" | "gos_rt_map_new" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_new",
                &[types::I32, types::I32],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let k = builder.ins().iconst(types::I32, 8);
            let v = builder.ins().iconst(types::I32, 8);
            let call = builder.ins().call(fref, &[k, v]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_map_len" => {
            let len_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_len",
                &[ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(len_fn, builder.func);
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[m]);
            let n = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, n);
            Ok(true)
        }
        "gos_rt_map_insert" => {
            let ins_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_insert",
                &[ptr_ty, ptr_ty, ptr_ty],
                &[],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let v64 = coerce_arg_to(builder, v_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot, 8, 3,
            ));
            let v_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot, 8, 3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            let v_addr = builder.ins().stack_addr(ptr_ty, v_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            builder.ins().store(MemFlags::trusted(), v64, v_addr, 0);
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_addr, v_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_map_get" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get",
                &[ptr_ty, ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot, 8, 3,
            ));
            let out_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot, 8, 3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            let out_addr = builder.ins().stack_addr(ptr_ty, out_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_addr, out_addr]);
            let loaded = builder.ins().load(types::I64, MemFlags::trusted(), out_addr, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, loaded);
            Ok(true)
        }
        "gos_rt_map_remove" => {
            let rm_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_remove",
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot, 8, 3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            let fref = module.declare_func_in_func(rm_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_addr]);
            let ok = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ok);
            Ok(true)
        }
        // Channels delegate to the gossamer-runtime staticlib.
        // Element size is hard-coded to i64-equivalent (8 bytes) —
        // every scalar and every GC pointer fits in that word.
        // Unbounded capacity via `cap = 0`.
        //
        // The frontend types `channel()` as a tuple
        // `(Sender<T>, Receiver<T>)` — two slots — so the user's
        // `let (tx, rx) = channel()` / `pair.0` / `pair.1`
        // pattern projects with a 0/8-byte offset. We allocate
        // a 16-byte stack slot here and store the channel
        // pointer at *both* offsets so subsequent
        // `pair.0` / `pair.1` projections hand the same
        // channel handle to send and receive sites. Without
        // this, `pair.1` reads garbage from the second tuple
        // slot and `recv` no-ops on a null channel pointer.
        "channel" | "channel::new" | "sync::channel" | "sync::Channel::new"
        | "gos_rt_chan_new" | "Channel::new" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_new",
                &[types::I32, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let elem = builder.ins().iconst(types::I32, 8);
            let cap = builder.ins().iconst(types::I64, 0);
            let call = builder.ins().call(fref, &[elem, cap]);
            let chan_ptr = builder.inst_results(call)[0];
            // 16-byte tuple slot; write chan_ptr to offsets 0
            // and 8 so both `Sender` and `Receiver` projections
            // observe the same handle.
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                16,
                3, // 8-byte alignment
            ));
            let base = builder.ins().stack_addr(ptr_ty, slot, 0);
            builder.ins().store(
                MemFlags::trusted(),
                chan_ptr,
                base,
                ir::immediates::Offset32::new(0),
            );
            builder.ins().store(
                MemFlags::trusted(),
                chan_ptr,
                base,
                ir::immediates::Offset32::new(8),
            );
            // Mark the destination as a 2-slot aggregate so
            // projections lower as memory loads from `base + N*8`
            // rather than reading a Variable directly.
            intrinsics.local_slots.insert(destination.local, 2);
            define_var_to(
                builder, locals, body, tcx, module, destination.local, base,
            );
            Ok(true)
        }
        "gos_rt_chan_send" | "send" => {
            // Stack-spill the value word so the runtime's
            // `gos_rt_chan_send(chan, *const u8)` can memcpy it in.
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("chan_send: missing channel arg"),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v64 = coerce_arg_to(builder, value, types::I64)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            builder.ins().store(MemFlags::trusted(), v64, slot_addr, 0);
            let send_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_send",
                &[ptr_ty, ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(send_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan, slot_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_chan_try_send" | "try_send" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("chan_try_send: missing channel arg"),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v64 = coerce_arg_to(builder, value, types::I64)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            builder.ins().store(MemFlags::trusted(), v64, slot_addr, 0);
            let send_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_try_send",
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let fref = module.declare_func_in_func(send_fn, builder.func);
            let call = builder.ins().call(fref, &[chan, slot_addr]);
            let ok = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ok);
            Ok(true)
        }
        "gos_rt_chan_try_recv" | "try_recv" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("chan_try_recv: missing channel arg"),
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            let recv_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_try_recv",
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let fref = module.declare_func_in_func(recv_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan, slot_addr]);
            let loaded = builder.ins().load(types::I64, MemFlags::trusted(), slot_addr, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, loaded);
            Ok(true)
        }
        "gos_rt_chan_close" | "close" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("chan_close: missing channel arg"),
            };
            let close_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_close",
                &[ptr_ty],
                &[],
            )?;
            let fref = module.declare_func_in_func(close_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_chan_recv" | "recv" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("chan_recv: missing channel arg"),
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            let recv_fn = intrinsics.extern_fn(
                module,
                "gos_rt_chan_recv",
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let fref = module.declare_func_in_func(recv_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan, slot_addr]);
            let loaded = builder.ins().load(
                types::I64,
                MemFlags::trusted(),
                slot_addr,
                0,
            );
            define_var_to(builder, locals, body, tcx, module, destination.local, loaded);
            Ok(true)
        }
        // ---- Mutex<T> primitive ----
        "Mutex::new" | "sync::Mutex::new" | "mutex::new" | "gos_rt_mutex_new" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_mutex_new",
                &[],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_mutex_lock" => {
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("mutex_lock: missing receiver"),
            };
            let f = intrinsics.extern_fn(module, "gos_rt_mutex_lock", &[ptr_ty], &[])?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_mutex_unlock" => {
            let m = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("mutex_unlock: missing receiver"),
            };
            let f = intrinsics.extern_fn(module, "gos_rt_mutex_unlock", &[ptr_ty], &[])?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // ---- WaitGroup primitive ----
        "WaitGroup::new" | "sync::WaitGroup::new" | "wg::new" | "gos_rt_wg_new" => {
            let f = intrinsics.extern_fn(module, "gos_rt_wg_new", &[], &[ptr_ty])?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_wg_add" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("wg_add: missing receiver"),
            };
            let n = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_wg_add",
                &[ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[wg, n64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_wg_done" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("wg_done: missing receiver"),
            };
            let f = intrinsics.extern_fn(module, "gos_rt_wg_done", &[ptr_ty], &[])?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[wg]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_wg_wait" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("wg_wait: missing receiver"),
            };
            let f = intrinsics.extern_fn(module, "gos_rt_wg_wait", &[ptr_ty], &[])?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[wg]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // ---- Heap [i64] primitive ----
        "I64Vec::new" | "heap_i64::new" | "gos_rt_heap_i64_new" => {
            let len = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_new",
                &[types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[len64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_heap_i64_get" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_i64_get: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx64 = coerce_arg_to(builder, idx, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_get",
                &[ptr_ty, types::I64],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v, idx64]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_heap_i64_set" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_i64_set: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let val = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx64 = coerce_arg_to(builder, idx, types::I64)?;
            let val64 = coerce_arg_to(builder, val, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_set",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, idx64, val64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_heap_i64_len" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_i64_len: missing receiver"),
            };
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_len",
                &[ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_heap_i64_write_lines_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_i64_write_lines: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let w = match args.get(3) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 60),
            };
            let s64 = coerce_arg_to(builder, s, types::I64)?;
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let w64 = coerce_arg_to(builder, w, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_write_lines_to_stdout",
                &[ptr_ty, types::I64, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, s64, n64, w64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_heap_i64_write_bytes_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_i64_write: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let s64 = coerce_arg_to(builder, s, types::I64)?;
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_i64_write_bytes_to_stdout",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, s64, n64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // ---- Heap [u8] primitive (`U8Vec`) — 1 byte per element ----
        "U8Vec::new" | "heap_u8::new" | "gos_rt_heap_u8_new" => {
            let len = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_new",
                &[types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[len64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_heap_u8_get" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_u8_get: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx64 = coerce_arg_to(builder, idx, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_get",
                &[ptr_ty, types::I64],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v, idx64]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_heap_u8_set" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_u8_set: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let val = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx64 = coerce_arg_to(builder, idx, types::I64)?;
            let val64 = coerce_arg_to(builder, val, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_set",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, idx64, val64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_heap_u8_len" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_u8_len: missing receiver"),
            };
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_len",
                &[ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_heap_u8_write_lines_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_u8_write_lines: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let w = match args.get(3) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 60),
            };
            let s64 = coerce_arg_to(builder, s, types::I64)?;
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let w64 = coerce_arg_to(builder, w, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_write_lines_to_stdout",
                &[ptr_ty, types::I64, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, s64, n64, w64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        "gos_rt_heap_u8_write_bytes_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("heap_u8_write: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let s64 = coerce_arg_to(builder, s, types::I64)?;
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_write_bytes_to_stdout",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[v, s64, n64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // ---- Atomic<i64> primitive ----
        "Atomic::new" | "sync::Atomic::new" | "atomic::new" | "gos_rt_atomic_i64_new" => {
            let initial = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let i64 = coerce_arg_to(builder, initial, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_atomic_i64_new",
                &[types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[i64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        "gos_rt_atomic_i64_load" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("atomic_load: missing receiver"),
            };
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_atomic_i64_load",
                &[ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[a]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_atomic_i64_store" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("atomic_store: missing receiver"),
            };
            let v = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v64 = coerce_arg_to(builder, v, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_atomic_i64_store",
                &[ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[a, v64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // LCG jump-ahead helper. Used by fasta-style
        // multi-threaded programs to seed each worker at the
        // right point in the random stream so the per-worker
        // streams interleave back into the same sequence the
        // single-thread reference produces.
        "gos_rt_lcg_jump" | "lcg::jump" | "lcg_jump" => {
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_lcg_jump",
                &[types::I64, types::I64, types::I64, types::I64, types::I64],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let args_v: Vec<_> = (0..5)
                .map(|i| match args.get(i) {
                    Some(a) => lower_operand(
                        module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                    ),
                    None => Ok(builder.ins().iconst(types::I64, 0)),
                })
                .collect::<Result<Vec<_>>>()?;
            let coerced: Vec<_> = args_v
                .into_iter()
                .map(|v| coerce_arg_to(builder, v, types::I64))
                .collect::<Result<Vec<_>>>()?;
            let call = builder.ins().call(fref, &coerced);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        "gos_rt_atomic_i64_fetch_add" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => bail!("atomic_fetch_add: missing receiver"),
            };
            let d = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(types::I64), intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let d64 = coerce_arg_to(builder, d, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_atomic_i64_fetch_add",
                &[ptr_ty, types::I64],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[a, d64]);
            let val = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, val);
            Ok(true)
        }
        // `Vec<T>::len()` — the pointer today is either a stack-
        // slot base or NULL (from `gos_rt_os_args`). Treat NULL
        // as 0; any non-NULL pointer is an aggregate owned by
        // the user and lacks a header, so hand back 0 until the
        // real Vec runtime lands.
        "gos_rt_vec_len" => {
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, zero);
            Ok(true)
        }
        // Array length: forward to the runtime shim, which reads
        // the first i64 slot of the passed pointer (GosArgs and
        // other len-prefixed buffers share that layout).
        "gos_rt_arr_len" | "gos_rt_len" => {
            let len_fn = intrinsics.extern_fn(
                module,
                "gos_rt_arr_len",
                &[ptr_ty],
                &[types::I64],
            )?;
            let len_ref = module.declare_func_in_func(len_fn, builder.func);
            let p = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(len_ref, &[p]);
            let n = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, n);
            Ok(true)
        }
        // Unary string helpers that return a fresh String
        // (allocated by the runtime). Signatures are `(ptr) -> ptr`.
        "gos_rt_str_trim" | "gos_rt_str_to_lower" | "gos_rt_str_to_upper" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                match name {
                    "gos_rt_str_trim" => "gos_rt_str_trim",
                    "gos_rt_str_to_lower" => "gos_rt_str_to_lower",
                    "gos_rt_str_to_upper" => "gos_rt_str_to_upper",
                    _ => unreachable!(),
                },
                &[ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[s]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        // Predicate string helpers: `(ptr, ptr) -> i32`.
        "gos_rt_str_contains" | "gos_rt_str_starts_with" | "gos_rt_str_ends_with" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                match name {
                    "gos_rt_str_contains" => "gos_rt_str_contains",
                    "gos_rt_str_starts_with" => "gos_rt_str_starts_with",
                    "gos_rt_str_ends_with" => "gos_rt_str_ends_with",
                    _ => unreachable!(),
                },
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let result = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, result);
            Ok(true)
        }
        "gos_rt_str_find" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_str_find",
                &[ptr_ty, ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let n = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, n);
            Ok(true)
        }
        "gos_rt_str_replace" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_str_replace",
                &[ptr_ty, ptr_ty, ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let c = match args.get(2) {
                Some(arg) => lower_operand(
                    module, builder, locals, body, tcx, arg, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b, c]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(builder, locals, body, tcx, module, destination.local, ptr);
            Ok(true)
        }
        // `v.push(x)` on a Vec<T>: spill x to a stack slot and
        // call the runtime's typed push. We hard-code elem_bytes=8
        // because every scalar + every GC pointer fits in a word,
        // matching the aggregate layout the codegen already uses.
        "gos_rt_vec_push" => {
            let push_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_push",
                &[ptr_ty, ptr_ty],
                &[],
            )?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, None, intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v64 = coerce_arg_to(builder, value, types::I64)?;
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            builder.ins().store(MemFlags::trusted(), v64, slot_addr, 0);
            let fref = module.declare_func_in_func(push_fn, builder.func);
            let _ = builder.ins().call(fref, &[vec_p, slot_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, unit);
            Ok(true)
        }
        // `v.pop()` — pops the last element through an 8-byte
        // stack slot and returns it. Returns 0 when the vec is
        // empty; callers that care about emptiness should check
        // `.len()` first.
        "gos_rt_vec_pop" => {
            let pop_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_pop",
                &[ptr_ty, ptr_ty],
                &[types::I32],
            )?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module, builder, locals, body, tcx, a, Some(ptr_ty), intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let slot_addr = builder.ins().stack_addr(ptr_ty, slot, 0);
            let fref = module.declare_func_in_func(pop_fn, builder.func);
            let _ = builder.ins().call(fref, &[vec_p, slot_addr]);
            let loaded = builder.ins().load(types::I64, MemFlags::trusted(), slot_addr, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, loaded);
            Ok(true)
        }
        // `.split(...)` / `.iter()` still need allocation
        // scaffolding (Vec<String> / iterator objects) — keep
        // the zero stub until that lands.
        "gos_rt_str_split" | "gos_rt_arr_iter" => {
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(builder, locals, body, tcx, module, destination.local, zero);
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn resolve_callee(
    operand: &Operand,
    callees_by_def: &HashMap<u32, ir::FuncRef>,
    callees_by_name: &HashMap<String, ir::FuncRef>,
) -> Result<ir::FuncRef> {
    match operand {
        Operand::FnRef { def, substs } => {
            // Specialised monomorphised bodies live in
            // `callees_by_name` under a `fn#{def}__mono__{hash}`
            // mangled key; fall back to the plain `def` lookup when
            // the substitution is empty (monomorphic callee).
            if !substs.is_empty() {
                let mangled = gossamer_mir::mangled_name(*def, substs);
                if let Some(r) = callees_by_name.get(&mangled).copied() {
                    return Ok(r);
                }
            }
            callees_by_def
                .get(&def.local)
                .copied()
                .or_else(|| callees_by_name.get(&format!("fn#{}", def.local)).copied())
                .ok_or_else(|| anyhow!("native codegen: unknown callee def#{}", def.local))
        }
        other => bail!("native codegen: call target must be FnRef, got {other:?}"),
    }
}

#[allow(clippy::too_many_arguments)]
fn lower_rvalue(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    rvalue: &Rvalue,
    dst_hint: Option<ir::Type>,
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    Ok(match rvalue {
        Rvalue::Use(operand) => {
            lower_operand(module, builder, locals, body, tcx, operand, dst_hint, intrinsics)?
        }
        Rvalue::BinaryOp { op, lhs, rhs } => {
            // For arithmetic, both operands share the result's cl
            // type, so forward `dst_hint` down. For comparisons the
            // result is I8 (bool) but operands aren't — fall through
            // to MIR-local inference by leaving `hint` as None. An
            // operand-side cross-hint (lhs's lowered type seeds
            // rhs's hint) handles comparisons of projected places
            // whose MIR local type is an opaque ADT.
            let arith_hint = match op {
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => None,
                _ => dst_hint,
            };
            let a =
                lower_operand(module, builder, locals, body, tcx, lhs, arith_hint, intrinsics)?;
            let b_hint = arith_hint.or_else(|| Some(value_type(a, builder)));
            let b =
                lower_operand(module, builder, locals, body, tcx, rhs, b_hint, intrinsics)?;
            // Float `%` on f64 → `libc::fmod`. Cranelift has no
            // direct opcode. Intercept before the generic binop
            // dispatch so the rest stays module-free.
            if matches!(op, BinOp::Rem) && value_type(a, builder).is_float() {
                let fmod_fn = intrinsics.extern_fn(
                    module,
                    "fmod",
                    &[types::F64, types::F64],
                    &[types::F64],
                )?;
                let fref = module.declare_func_in_func(fmod_fn, builder.func);
                let a64 = if value_type(a, builder) == types::F32 {
                    builder.ins().fpromote(types::F64, a)
                } else {
                    a
                };
                let b64 = if value_type(b, builder) == types::F32 {
                    builder.ins().fpromote(types::F64, b)
                } else {
                    b
                };
                let call = builder.ins().call(fref, &[a64, b64]);
                builder.inst_results(call)[0]
            } else {
                lower_binop(builder, *op, a, b)?
            }
        }
        Rvalue::UnaryOp { op, operand } => {
            let v =
                lower_operand(module, builder, locals, body, tcx, operand, dst_hint, intrinsics)?;
            match op {
                UnOp::Neg => {
                    if value_type(v, builder).is_float() {
                        builder.ins().fneg(v)
                    } else {
                        builder.ins().ineg(v)
                    }
                }
                UnOp::Not => builder.ins().bnot(v),
            }
        }
        Rvalue::Cast { operand, target } => {
            // Same-kind casts (i64 → usize, i32 → i64) are no-ops at
            // this level; cross-kind numeric casts (f64 ↔ i64) are
            // still future work and fall through to the generic
            // `unsupported` bail at the statement layer.
            let _ = target;
            lower_operand(module, builder, locals, body, tcx, operand, dst_hint, intrinsics)?
        }
        Rvalue::Aggregate { kind, operands } => {
            // Aggregates live in a stack slot N*8 bytes wide. Each
            // scalar field occupies an 8-byte slot. Arrays of
            // structs stride by (#struct-fields) slots so that
            // `a[i].f` projects correctly.
            //
            // Structs (`AggregateKind::Adt`) with struct variant
            // shapes use the same flat layout — the field order
            // matches the ADT declaration. Enum variant payloads
            // are not yet distinguished (no discriminant slot).
            let elem_slots: u32 = match kind {
                gossamer_mir::AggregateKind::Array => operands
                    .first()
                    .map_or(1, |op| {
                        if let Operand::Copy(place) = op {
                            intrinsics
                                .local_slots
                                .get(&place.local)
                                .copied()
                                .unwrap_or(1)
                        } else {
                            1
                        }
                    }),
                _ => 1,
            };
            let total_slots: u32 = match kind {
                gossamer_mir::AggregateKind::Array => {
                    (operands.len() as u32) * elem_slots
                }
                _ => operands.len() as u32,
            };
            let size = total_slots * 8;
            let align_log2 = 3; // 8-byte alignment.
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size.max(8),
                align_log2,
            ));
            let ptr_ty = module.target_config().pointer_type();
            let base = builder.ins().stack_addr(ptr_ty, slot, 0);
            for (i, operand) in operands.iter().enumerate() {
                // A `Copy(local)` operand whose source local is a
                // pointer-to-aggregate (has `local_slots` metadata)
                // must be memcpy'd into the destination: the source
                // Variable's value is the source's base address, not
                // its contents. The simple `store` path is only
                // correct for scalar operands (ints/floats/booleans
                // — values that live directly in the local's SSA
                // Variable).
                let operand_aggregate_slots: Option<u32> = match operand {
                    Operand::Copy(place) if place.projection.is_empty() => {
                        intrinsics.local_slots.get(&place.local).copied()
                    }
                    _ => None,
                };
                let dst_off = match kind {
                    gossamer_mir::AggregateKind::Array => {
                        (i as u32) * elem_slots * 8
                    }
                    _ => (i as u32) * 8,
                };
                if let Some(copy_slots) = operand_aggregate_slots {
                    let src = lower_operand(
                        module, builder, locals, body, tcx, operand, None, intrinsics,
                    )?;
                    for slot_idx in 0..copy_slots {
                        let off = (slot_idx as i32) * 8;
                        let word = builder.ins().load(
                            types::I64,
                            MemFlags::trusted(),
                            src,
                            ir::immediates::Offset32::new(off),
                        );
                        builder.ins().store(
                            MemFlags::trusted(),
                            word,
                            base,
                            ir::immediates::Offset32::new((dst_off as i32) + off),
                        );
                    }
                } else {
                    let value = lower_operand(
                        module, builder, locals, body, tcx, operand, None, intrinsics,
                    )?;
                    builder.ins().store(
                        MemFlags::trusted(),
                        value,
                        base,
                        ir::immediates::Offset32::new(dst_off as i32),
                    );
                }
            }
            let _ = kind;
            base
        }
        Rvalue::Len(place) => {
            // With the flat-8-byte layout we can't recover the
            // aggregate length from the pointer alone. Emit a
            // placeholder zero — callers that actually need `len`
            // will use it with arrays of known size via MIR opt.
            let _ = place;
            builder.ins().iconst(types::I64, 0)
        }
        Rvalue::Repeat { value, count } => {
            let elem_slots: u32 = if let Operand::Copy(place) = value {
                intrinsics
                    .local_slots
                    .get(&place.local)
                    .copied()
                    .unwrap_or(1)
            } else {
                1
            };
            let total_slots = u32::try_from(*count)
                .map_err(|_| anyhow!("native codegen: repeat count too large"))?
                .saturating_mul(elem_slots);
            let size = total_slots.saturating_mul(8);
            let slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                size.max(8),
                3,
            ));
            let ptr_ty = module.target_config().pointer_type();
            let base = builder.ins().stack_addr(ptr_ty, slot, 0);
            if elem_slots > 1 {
                if let Operand::Copy(place) = value {
                    let src = lower_place_read(
                        module,
                        builder,
                        locals,
                        body,
                        tcx,
                        &Place::local(place.local),
                        Some(ptr_ty),
                        intrinsics,
                    )?;
                    for i in 0..*count {
                        let dst_offset = (i as u32) * elem_slots * 8;
                        for slot_idx in 0..elem_slots {
                            let off = (slot_idx as i32) * 8;
                            let word = builder.ins().load(
                                types::I64,
                                MemFlags::trusted(),
                                src,
                                ir::immediates::Offset32::new(off),
                            );
                            builder.ins().store(
                                MemFlags::trusted(),
                                word,
                                base,
                                ir::immediates::Offset32::new((dst_offset as i32) + off),
                            );
                        }
                    }
                }
            } else {
                let element =
                    lower_operand(module, builder, locals, body, tcx, value, None, intrinsics)?;
                for i in 0..*count {
                    let offset = ir::immediates::Offset32::new(
                        i32::try_from(i.saturating_mul(8)).map_err(|_| {
                            anyhow!("native codegen: repeat offset too large")
                        })?,
                    );
                    builder.ins().store(MemFlags::trusted(), element, base, offset);
                }
            }
            base
        }
        // `&place` / `&mut place` → the address of `place`. For a
        // bare local, that's the Variable's SSA value (which is
        // already a pointer when the local holds an aggregate);
        // for a projected place, it's the computed projection
        // address.
        Rvalue::Ref { place, .. } => {
            if place.projection.is_empty() {
                let var = ensure_var(builder, locals, body, tcx, module, place.local);
                builder.use_var(var)
            } else {
                lower_place_address(
                    module, builder, locals, body, tcx, place, intrinsics,
                )?
            }
        }
        // `CallIntrinsic` as an Rvalue is dispatched at the
        // `Assign` statement layer; reaching it here means the
        // statement path already returned. Unreachable in
        // practice.
        Rvalue::CallIntrinsic { .. } => {
            unreachable!("CallIntrinsic must be routed through the statement path")
        }
    })
}

#[allow(clippy::too_many_arguments)]
fn lower_operand(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    operand: &Operand,
    hint: Option<ir::Type>,
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    Ok(match operand {
        Operand::Copy(place) => {
            // For projected reads through a known aggregate root,
            // prefer the root's recorded element type over any
            // hint from the caller — the hint is an approximation,
            // the element table is ground truth.
            let effective_hint = if place.projection.is_empty() {
                hint
            } else {
                intrinsics.elem_cl_ty.get(&place.local).copied().or(hint)
            };
            lower_place_read(
                module,
                builder,
                locals,
                body,
                tcx,
                place,
                effective_hint,
                intrinsics,
            )?
        }
        Operand::Const(value) => lower_const(module, builder, value, hint, intrinsics)?,
        Operand::FnRef { def, .. } => {
            // `let f = some_fn; f(x)` passes the function by
            // reference. Emit a `func_addr` whose value is a
            // pointer-typed SSA value; the indirect-call path
            // picks it up through the local's variable.
            let ptr_ty = module.target_config().pointer_type();
            match intrinsics.functions_by_def.get(&def.local).copied() {
                Some(func_id) => {
                    let fr = module.declare_func_in_func(func_id, builder.func);
                    builder.ins().func_addr(ptr_ty, fr)
                }
                None => builder.ins().iconst(ptr_ty, 0),
            }
        }
    })
}

/// Reads the value stored at `place`. When the place has no
/// projections this is just the local's Variable contents. When it
/// carries a `Projection::Field(i)` or `Projection::Index(local)`
/// chain it walks through each projection, picking the leaf's
/// cranelift type for the final load.
#[allow(clippy::too_many_arguments)]
fn lower_place_read(
    module: &dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    hint: Option<ir::Type>,
    intrinsics: &IntrinsicContext,
) -> Result<ir::Value> {
    if place.projection.is_empty() {
        let var = ensure_var(builder, locals, body, tcx, module, place.local);
        return Ok(builder.use_var(var));
    }
    let addr = lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?;
    let leaf_ty = resolve_place_cl_type(tcx, body, place, module, hint);
    Ok(builder.ins().load(leaf_ty, MemFlags::trusted(), addr, 0))
}

fn lower_const(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    value: &ConstValue,
    hint: Option<ir::Type>,
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    Ok(match value {
        ConstValue::Int(n) => {
            let ty = hint
                .filter(|t| t.is_int())
                .unwrap_or(types::I64);
            builder.ins().iconst(ty, i64_truncate(*n))
        }
        ConstValue::Bool(b) => {
            let ty = hint
                .filter(|t| t.is_int())
                .unwrap_or(types::I8);
            builder.ins().iconst(ty, i64::from(*b))
        }
        ConstValue::Char(c) => {
            let ty = hint
                .filter(|t| t.is_int())
                .unwrap_or(types::I32);
            builder.ins().iconst(ty, i64::from(u32::from(*c)))
        }
        ConstValue::Unit => builder.ins().iconst(types::I64, 0),
        ConstValue::Str(text) => {
            // String constants live in `.rodata` as null-terminated
            // bytes; the value we return is the address of those
            // bytes, sized as the target's pointer type.
            let data_id = intrinsics.intern_string(module, text)?;
            let global = module.declare_data_in_func(data_id, builder.func);
            let ptr_ty = module.target_config().pointer_type();
            builder.ins().global_value(ptr_ty, global)
        }
        ConstValue::Float(bits) => {
            let ty = hint
                .filter(|t| t.is_float())
                .unwrap_or(types::F64);
            let val = f64::from_bits(*bits);
            if ty == types::F32 {
                builder.ins().f32const(val as f32)
            } else {
                builder.ins().f64const(val)
            }
        }
    })
}

fn i64_truncate(n: i128) -> i64 {
    if n > i128::from(i64::MAX) {
        i64::MAX
    } else if n < i128::from(i64::MIN) {
        i64::MIN
    } else {
        n as i64
    }
}

/// Dispatches a binary op based on the operand type. Integer ops
/// use signed semantics (matches MIR's signed-int assumption for
/// the default widths); float ops use IEEE-754 semantics and
/// compares use `Ordered` `FloatCC` so NaN propagates to `false`.
fn lower_binop(
    builder: &mut FunctionBuilder<'_>,
    op: BinOp,
    a: ir::Value,
    b: ir::Value,
) -> Result<ir::Value> {
    let mut a_ty = value_type(a, builder);
    let mut b_ty = value_type(b, builder);
    let mut a = a;
    let mut b = b;
    if a_ty != b_ty {
        // Reinterpret where possible: a common mismatch pattern is
        // a projected read whose MIR element type was left as an
        // unresolved inference variable, defaulting to `i64`,
        // paired with a concrete `f64` operand. Aggregates store
        // every scalar in an 8-byte slot, so the 8 bytes loaded
        // as an i64 are the same bits that were stored as an f64,
        // and a `bitcast` is a zero-cost reinterpret.
        if a_ty == types::I64 && b_ty == types::F64 {
            a = builder.ins().bitcast(types::F64, ir::MemFlags::new(), a);
            a_ty = types::F64;
        } else if a_ty == types::F64 && b_ty == types::I64 {
            b = builder.ins().bitcast(types::F64, ir::MemFlags::new(), b);
            b_ty = types::F64;
        } else {
            bail!(
                "native codegen: binop operand type mismatch (op={op:?}, {a_ty:?} vs {b_ty:?})"
            );
        }
        let _ = b_ty;
    }
    if a_ty.is_float() {
        return Ok(match op {
            BinOp::Add => builder.ins().fadd(a, b),
            BinOp::Sub => builder.ins().fsub(a, b),
            BinOp::Mul => builder.ins().fmul(a, b),
            BinOp::Div => builder.ins().fdiv(a, b),
            // Float `%` is intercepted in lower_rvalue and routed
            // through libc::fmod before this match runs; reaching
            // here on a float means the caller bypassed that path
            // — a compiler bug.
            BinOp::Rem => unreachable!("float Rem handled in lower_rvalue"),
            BinOp::Eq => fcmp_bool(builder, ir::condcodes::FloatCC::Equal, a, b),
            BinOp::Ne => fcmp_bool(builder, ir::condcodes::FloatCC::NotEqual, a, b),
            BinOp::Lt => fcmp_bool(builder, ir::condcodes::FloatCC::LessThan, a, b),
            BinOp::Le => fcmp_bool(builder, ir::condcodes::FloatCC::LessThanOrEqual, a, b),
            BinOp::Gt => fcmp_bool(builder, ir::condcodes::FloatCC::GreaterThan, a, b),
            BinOp::Ge => {
                fcmp_bool(builder, ir::condcodes::FloatCC::GreaterThanOrEqual, a, b)
            }
            // Bitwise on float is a typecheck error; reaching
            // here is a compiler bug.
            BinOp::BitAnd | BinOp::BitOr | BinOp::BitXor | BinOp::Shl | BinOp::Shr => {
                unreachable!("bitwise op on float — should be a type error")
            }
        });
    }
    Ok(match op {
        BinOp::Add => builder.ins().iadd(a, b),
        BinOp::Sub => builder.ins().isub(a, b),
        BinOp::Mul => builder.ins().imul(a, b),
        BinOp::Div => builder.ins().sdiv(a, b),
        BinOp::Rem => builder.ins().srem(a, b),
        BinOp::BitAnd => builder.ins().band(a, b),
        BinOp::BitOr => builder.ins().bor(a, b),
        BinOp::BitXor => builder.ins().bxor(a, b),
        BinOp::Shl => builder.ins().ishl(a, b),
        BinOp::Shr => builder.ins().sshr(a, b),
        BinOp::Eq => compare_bool(builder, ir::condcodes::IntCC::Equal, a, b),
        BinOp::Ne => compare_bool(builder, ir::condcodes::IntCC::NotEqual, a, b),
        BinOp::Lt => compare_bool(builder, ir::condcodes::IntCC::SignedLessThan, a, b),
        BinOp::Le => compare_bool(builder, ir::condcodes::IntCC::SignedLessThanOrEqual, a, b),
        BinOp::Gt => compare_bool(builder, ir::condcodes::IntCC::SignedGreaterThan, a, b),
        BinOp::Ge => compare_bool(builder, ir::condcodes::IntCC::SignedGreaterThanOrEqual, a, b),
    })
}

fn compare_bool(
    builder: &mut FunctionBuilder<'_>,
    cc: ir::condcodes::IntCC,
    a: ir::Value,
    b: ir::Value,
) -> ir::Value {
    // Cranelift `icmp` returns an `i8` boolean in Cranelift's
    // newer API; keep the same width so downstream stores into a
    // bool slot don't need an extra coercion.
    builder.ins().icmp(cc, a, b)
}

fn fcmp_bool(
    builder: &mut FunctionBuilder<'_>,
    cc: ir::condcodes::FloatCC,
    a: ir::Value,
    b: ir::Value,
) -> ir::Value {
    builder.ins().fcmp(cc, a, b)
}

/// Emits a C-ABI `main(i32, **i8) -> i32` that calls the Gossamer
/// `main` (which returns `i64`) and truncates the result into the
/// process exit code.
fn emit_c_main_shim(module: &mut dyn Module, gos_main: FuncId) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(types::I32));
    sig.params.push(AbiParam::new(ptr_ty));
    sig.returns.push(AbiParam::new(types::I32));
    let shim = module
        .declare_function("main", Linkage::Export, &sig)
        .map_err(|e| anyhow!("declare main shim: {e}"))?;
    // Import the set-args helper from the runtime shim so argc/argv
    // reach `gos_rt_os_args` before `gossamer_main` starts executing.
    let mut set_args_sig = module.make_signature();
    set_args_sig.params.push(AbiParam::new(types::I32));
    set_args_sig.params.push(AbiParam::new(ptr_ty));
    let set_args = module
        .declare_function("gos_rt_set_args", Linkage::Import, &set_args_sig)
        .map_err(|e| anyhow!("declare set_args: {e}"))?;
    let flush_sig = module.make_signature();
    let flush_stdout = module
        .declare_function("gos_rt_flush_stdout", Linkage::Import, &flush_sig)
        .map_err(|e| anyhow!("declare flush_stdout: {e}"))?;
    let mut func = Function::with_name_signature(UserFuncName::user(0, shim.as_u32()), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let argc = builder.block_params(entry)[0];
        let argv = builder.block_params(entry)[1];
        let set_args_ref = module.declare_func_in_func(set_args, builder.func);
        let _ = builder.ins().call(set_args_ref, &[argc, argv]);
        let gos_main_ref = module.declare_func_in_func(gos_main, builder.func);
        let call = builder.ins().call(gos_main_ref, &[]);
        let result64 = builder.inst_results(call)[0];
        // Drain the runtime's line-buffered stdout cache so any
        // trailing output (no final `println!`) reaches the
        // terminal before the process exits.
        let flush_ref = module.declare_func_in_func(flush_stdout, builder.func);
        let _ = builder.ins().call(flush_ref, &[]);
        let result32 = builder.ins().ireduce(types::I32, result64);
        builder.ins().return_(&[result32]);
        builder.seal_all_blocks();
        builder.finalize();
    }
    let mut ctx = Context::for_function(func);
    module
        .define_function(shim, &mut ctx)
        .map_err(|e| anyhow!("define main shim: {e}"))?;
    Ok(())
}
