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
//   - `doc_markdown` flags identifiers like `i64`, `f64`,
//     etc. in plain-prose docs. Backticking every numeric
//     type name in every comment is noise.
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
    clippy::comparison_chain
)]

use std::collections::HashMap;

use std::collections::HashSet;

use anyhow::{Result, anyhow, bail};
use cranelift_codegen::ir::{
    AbiParam, ExtFuncData, Function, GlobalValueData, InstBuilder, MemFlags, Signature,
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

/// Globally-scoped rodata + intrinsic function handles accumulated
/// across every body in a single [`compile_to_object`] run. Keeps
/// the per-function lowering paths from having to thread the
/// module's mutation needs through themselves.
#[derive(Clone)]
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
    /// Per-function: pre-computed cranelift type for every local,
    /// indexed by `local.0`. Populated once via `infer_body_cl_types`
    /// before the body's lowering begins; `define_var_to` and
    /// `ensure_var` read from here instead of re-running the full
    /// body scan on every assignment. Cleared between bodies.
    pub(crate) body_cl_types: Vec<Option<ir::Type>>,
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
            body_cl_types: Vec::new(),
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

    /// Declares an imported `gos_rt_*` C-ABI function using the
    /// typed signature from the ABI registry.
    ///
    /// Panics if `name` is not in the registry — this turns typos in
    /// symbol names into a build-time panic instead of a silent
    /// wrong-code or segfault at runtime.
    fn extern_fn_by_name(&mut self, module: &mut dyn Module, name: &'static str) -> Result<FuncId> {
        if let Some(id) = self.externs.get(name).copied() {
            return Ok(id);
        }
        let entry = gossamer_abi::lookup(name).unwrap_or_else(|| {
            panic!("extern_fn_by_name: unknown runtime symbol {name:?} — add it to gossamer-abi/src/registry.rs")
        });
        let mut sig = module.make_signature();
        for abi_ty in entry.sig.params {
            let cl_ty = abi_type_to_cranelift(*abi_ty);
            if let Some(t) = cl_ty {
                sig.params.push(AbiParam::new(t));
            }
        }
        if let Some(t) = abi_type_to_cranelift(entry.sig.ret) {
            sig.returns.push(AbiParam::new(t));
        }
        let id = module
            .declare_function(name, Linkage::Import, &sig)
            .map_err(|e| anyhow!("declare extern {name}: {e}"))?;
        self.externs.insert(name, id);
        Ok(id)
    }
}

/// Read-only snapshot of a declared `ObjectModule` that can be cheaply
/// cloned and sent to rayon worker threads. Body IR building reads the
/// pre-populated function/data maps via [`Module`] trait dispatch;
/// methods that would mutate the module (`declare_function`,
/// `define_function`, …) are unreachable during the parallel phase and
/// call `unreachable!()`.
///
/// Built once after the sequential pre-declaration phase, then cloned
/// per rayon thread by [`lower_program_full`].
#[derive(Clone)]
struct OfflineModule {
    frontend_config: TargetFrontendConfig,
    default_call_conv: CallConv,
    /// FuncId.as_u32() → (signature, colocated) snapshot from the real module.
    func_sigs: HashMap<u32, (Signature, bool)>,
    /// DataId.as_u32() → (colocated, tls) snapshot from the real module.
    data_info: HashMap<u32, (bool, bool)>,
}

impl Module for OfflineModule {
    fn isa(&self) -> &dyn cranelift_codegen::isa::TargetIsa {
        unreachable!("OfflineModule: isa() must not be called in parallel IR phase")
    }
    fn declarations(&self) -> &ModuleDeclarations {
        unreachable!("OfflineModule: declarations() must not be called in parallel IR phase")
    }
    fn target_config(&self) -> TargetFrontendConfig {
        self.frontend_config
    }
    fn make_signature(&self) -> Signature {
        Signature::new(self.default_call_conv)
    }
    fn declare_function(
        &mut self,
        _name: &str,
        _linkage: Linkage,
        _sig: &Signature,
    ) -> cranelift_module::ModuleResult<FuncId> {
        unreachable!("OfflineModule: declare_function called in parallel phase — pre-declare first")
    }
    fn declare_anonymous_function(
        &mut self,
        _sig: &Signature,
    ) -> cranelift_module::ModuleResult<FuncId> {
        unreachable!("OfflineModule: declare_anonymous_function called in parallel phase")
    }
    fn declare_data(
        &mut self,
        _name: &str,
        _linkage: Linkage,
        _writable: bool,
        _tls: bool,
    ) -> cranelift_module::ModuleResult<DataId> {
        unreachable!(
            "OfflineModule: declare_data called in parallel phase — pre-intern strings first"
        )
    }
    fn declare_anonymous_data(
        &mut self,
        _writable: bool,
        _tls: bool,
    ) -> cranelift_module::ModuleResult<DataId> {
        unreachable!("OfflineModule: declare_anonymous_data called in parallel phase")
    }
    fn define_function_with_control_plane(
        &mut self,
        _func: FuncId,
        _ctx: &mut Context,
        _ctrl_plane: &mut cranelift_codegen::control::ControlPlane,
    ) -> cranelift_module::ModuleResult<()> {
        unreachable!("OfflineModule: define_function called in parallel phase")
    }
    fn define_function_bytes(
        &mut self,
        _func_id: FuncId,
        _alignment: u64,
        _bytes: &[u8],
        _relocs: &[cranelift_module::ModuleReloc],
    ) -> cranelift_module::ModuleResult<()> {
        unreachable!("OfflineModule: define_function_bytes called in parallel phase")
    }
    fn define_data(
        &mut self,
        _data_id: DataId,
        _ctx: &DataDescription,
    ) -> cranelift_module::ModuleResult<()> {
        unreachable!("OfflineModule: define_data called in parallel phase")
    }
    /// Override the default implementation so we never call `declarations()`.
    fn declare_func_in_func(&mut self, func_id: FuncId, func: &mut ir::Function) -> ir::FuncRef {
        let (sig, colocated) = self.func_sigs.get(&func_id.as_u32()).unwrap_or_else(|| {
            panic!(
                "OfflineModule: FuncId {} not pre-declared",
                func_id.as_u32()
            )
        });
        let signature = func.import_signature(sig.clone());
        let user_name_ref = func.declare_imported_user_function(UserExternalName {
            namespace: 0,
            index: func_id.as_u32(),
        });
        func.import_function(ExtFuncData {
            name: ir::ExternalName::user(user_name_ref),
            signature,
            colocated: *colocated,
        })
    }
    /// Override the default implementation so we never call `declarations()`.
    fn declare_data_in_func(&self, data_id: DataId, func: &mut ir::Function) -> ir::GlobalValue {
        let (colocated, tls) = self
            .data_info
            .get(&data_id.as_u32())
            .copied()
            .unwrap_or((true, false));
        let user_name_ref = func.declare_imported_user_function(UserExternalName {
            namespace: 1,
            index: data_id.as_u32(),
        });
        func.create_global_value(GlobalValueData::Symbol {
            name: ir::ExternalName::user(user_name_ref),
            offset: Imm64::new(0),
            colocated,
            tls,
        })
    }
}

/// Collects every `ConstValue::Str` string from all operand positions in `body`.
/// Used during the N9 pre-declaration phase to intern all string data objects
/// before the parallel IR-building pass begins.
fn collect_body_str_consts(body: &Body) -> Vec<String> {
    fn op_str(op: &Operand) -> Option<String> {
        if let Operand::Const(ConstValue::Str(s)) = op {
            Some(s.clone())
        } else {
            None
        }
    }
    fn rvalue_strs(rv: &Rvalue) -> Vec<String> {
        match rv {
            Rvalue::Use(op)
            | Rvalue::UnaryOp { operand: op, .. }
            | Rvalue::Cast { operand: op, .. }
            | Rvalue::Repeat { value: op, .. } => op_str(op).into_iter().collect(),
            Rvalue::BinaryOp { lhs, rhs, .. } => {
                op_str(lhs).into_iter().chain(op_str(rhs)).collect()
            }
            Rvalue::Aggregate { operands, .. } => operands.iter().filter_map(op_str).collect(),
            Rvalue::CallIntrinsic { args, .. } => args.iter().filter_map(op_str).collect(),
            Rvalue::Len(_) | Rvalue::Ref { .. } => vec![],
        }
    }
    let mut out: Vec<String> = Vec::new();
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                out.extend(rvalue_strs(rvalue));
            }
        }
        match &block.terminator {
            Terminator::Call { callee, args, .. } => {
                out.extend(op_str(callee));
                out.extend(args.iter().filter_map(op_str));
            }
            Terminator::SwitchInt { discriminant, .. } => {
                out.extend(op_str(discriminant));
            }
            Terminator::Assert { cond, .. } => {
                out.extend(op_str(cond));
            }
            Terminator::Goto { .. }
            | Terminator::Return
            | Terminator::Unreachable
            | Terminator::Panic { .. }
            | Terminator::Drop { .. } => {}
        }
    }
    out
}

/// Builds an [`OfflineModule`] by snapshotting the signature and
/// colocated flag for every declared function/data from the real module.
///
/// Called once after the sequential pre-declaration phase in
/// [`lower_program_full`] to produce the cloneable offline representation
/// each rayon worker thread uses during parallel IR building.
fn build_offline_module(
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

/// Maps an [`gossamer_abi::AbiType`] to the corresponding Cranelift IR type.
/// Returns `None` for `Void` (no return value).
fn abi_type_to_cranelift(ty: gossamer_abi::AbiType) -> Option<ir::Type> {
    match ty {
        gossamer_abi::AbiType::Void => None,
        gossamer_abi::AbiType::I8 => Some(types::I8),
        gossamer_abi::AbiType::I32 => Some(types::I32),
        gossamer_abi::AbiType::I64 | gossamer_abi::AbiType::U64 => Some(types::I64),
        gossamer_abi::AbiType::F64 => Some(types::F64),
        gossamer_abi::AbiType::Ptr => Some(types::I64),
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
    #[allow(
        dead_code,
        reason = "exposed for the LLVM backend to populate without an extra pass"
    )]
    pub function_ids_by_def: HashMap<u32, FuncId>,
}

/// Builds the cranelift settings + native ISA used by both the
/// object and JIT pipelines. `pic` differs by backend: the AOT
/// object emitter needs `is_pic=true` so the produced relocations
/// match what `cc` expects when linking, while `cranelift-jit`
/// hard-rejects PIC at finalisation time (see
/// [the JIT backend's assertion](https://github.com/bytecodealliance/wasmtime/blob/v36.0.7/cranelift/jit/src/backend.rs#L348)).
pub(crate) fn build_native_isa(
    pic: bool,
) -> Result<std::sync::Arc<dyn cranelift_codegen::isa::TargetIsa>> {
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
    let isa_builder = cranelift_native::builder().map_err(|e| anyhow!("native isa: {e}"))?;
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
    define_only: Option<&HashSet<String>>,
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

    // N9-A: Seed the IntrinsicContext with all function maps so that
    // clones sent to rayon threads carry complete function-pointer tables.
    let mut intrinsics = IntrinsicContext::new();
    intrinsics.functions.clone_from(&function_ids_by_name);
    intrinsics.functions_by_def.clone_from(&function_ids_by_def);

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
    for body in bodies {
        for s in collect_body_str_consts(body) {
            if s.starts_with("__fn_thunk_") {
                if !intrinsics.functions.contains_key(&s) {
                    define_shape_thunk(module, &mut intrinsics, &s)?;
                }
            } else {
                intrinsics.intern_string(module, &s)?;
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

    // N9-D: Build every function's IR in parallel. Each rayon thread
    // receives its own clone of `offline` and `intrinsics`; per-body
    // mutable state starts cleared because those maps are empty at
    // clone time (they are only filled during lower_body).
    let dump_clif = std::env::var("GOS_DUMP_CLIF").is_ok();
    let ir_pairs: Vec<(FuncId, String, Function)> = bodies
        .par_iter()
        .zip(body_type_vecs.par_iter())
        .filter(|(body, _)| body_should_be_defined(&body.name))
        .map(|(body, bct)| -> Result<(FuncId, String, Function)> {
            let id = function_ids_by_name
                .get(&body.name)
                .copied()
                .ok_or_else(|| anyhow!("function id missing: {}", body.name))?;
            let mut offline_clone = offline.clone();
            let mut local_intrinsics = intrinsics.clone();
            local_intrinsics.body_cl_types.clone_from(bct);
            let signature = build_signature_from_types(&offline_clone, body, tcx, bct);
            let mut func =
                Function::with_name_signature(UserFuncName::user(0, id.as_u32()), signature);
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
        })
        .collect::<Result<Vec<_>>>()?;

    // N9-E: Emit each compiled function into the real ObjectModule
    // sequentially (ObjectModule is not Sync). Cranelift compilation
    // happens here too, but the IR construction above (the expensive
    // allocation-heavy work) ran in parallel.
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
    let define_only_set: Option<HashSet<String>> =
        options.define_only.map(|v| v.into_iter().collect());
    let lowered = lower_program_full(
        &mut module,
        bodies,
        tcx,
        Some(main_rename),
        options.omit_c_main_shim,
        define_only_set.as_ref(),
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

/// Path-oriented variant: writes the freshly emitted object
/// directly to `obj_out` instead of returning the bytes through
/// `NativeObject`. Build paths that immediately persist the
/// object to disk (the AOT pipeline) avoid the redundant
/// `Vec<u8>` heap retention by going through this entry point.
pub fn compile_to_object_at_path(
    bodies: &[Body],
    tcx: &TyCtxt,
    obj_out: &std::path::Path,
) -> Result<String> {
    compile_to_object_at_path_with_options(bodies, tcx, obj_out, CompileOptions::default())
}

/// Path-oriented + options variant. Mirrors
/// [`compile_to_object_with_options`] except the produced object
/// is written to `obj_out`; only the resolved triple comes back.
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

fn build_signature_from_types(
    module: &dyn Module,
    body: &Body,
    tcx: &TyCtxt,
    bct: &[Option<ir::Type>],
) -> Signature {
    let mut sig = module.make_signature();
    for pidx in 1..=body.arity {
        let local = Local(pidx);
        let cl = bct
            .get(local.0 as usize)
            .copied()
            .flatten()
            .unwrap_or_else(|| cl_type_of(tcx, body.local_ty(local), module));
        sig.params.push(AbiParam::new(cl));
    }
    let ret_cl = bct
        .get(Local::RETURN.0 as usize)
        .copied()
        .flatten()
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
            Projection::Field(idx) => {
                if let Some(next) = field_ty_at(tcx, ty, *idx) {
                    ty = next;
                } else {
                    hit_opaque = true;
                }
            }
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
    reason = "lowering plumbing — every parameter is needed by Cranelift's API"
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
    capture_summary: &gossamer_mir::CaptureSummary,
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
            define_var_to(
                &mut builder,
                &mut locals,
                &intrinsics.body_cl_types,
                local,
                param_value,
            );
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

    // Legacy GcRef-handle shadow stack — `gos_rt_gc_shadow_save` /
    // `gos_rt_gc_shadow_restore` from `gossamer-runtime::gc`. Used by
    // the opt-in rooted-allocation API. Production codegen does not
    // push anything onto it today; keeping the frame at 0 makes the
    // matching restore a no-op.
    let shadow_frame_var = builder.declare_var(types::I64);
    // Raw-pointer tracing-GC shadow stack — `gos_rt_gc_root_save` /
    // `gos_rt_gc_root_restore` from `gossamer-runtime::c_abi`.
    // Codegen emits a `gos_rt_gc_root_push(ptr)` after every aggregate
    // allocation site, and `gos_rt_gc_root_restore(raw_frame)` at
    // every return.
    let raw_shadow_frame_var = builder.declare_var(types::I64);

    // Pre-scan the body to identify loop-header blocks (blocks that
    // are the target of a back-edge — a jump from a successor whose
    // id is >= the target's). Codegen emits a `gos_rt_gc_safepoint`
    // at the start of each such block so long-running loops give
    // the collector a chance to advance.
    let mut loop_headers: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for src in &body.blocks {
        let src_id = src.id.as_u32();
        match &src.terminator {
            Terminator::Goto { target } if target.as_u32() <= src_id => {
                loop_headers.insert(target.as_u32());
            }
            Terminator::SwitchInt { arms, default, .. } => {
                for (_, t) in arms {
                    if t.as_u32() <= src_id {
                        loop_headers.insert(t.as_u32());
                    }
                }
                if default.as_u32() <= src_id {
                    loop_headers.insert(default.as_u32());
                }
            }
            Terminator::Call {
                target: Some(t), ..
            } if t.as_u32() <= src_id => {
                loop_headers.insert(t.as_u32());
            }
            Terminator::Assert { target, .. } | Terminator::Drop { target, .. }
                if target.as_u32() <= src_id =>
            {
                loop_headers.insert(target.as_u32());
            }
            _ => {}
        }
    }

    let cleanup_plan = gossamer_mir::plan_cleanup_with_summary(body, capture_summary);
    let entry_block_id = body.blocks.first().map(|b| b.id.as_u32());
    let mut emitted_prologue = false;
    for block in &body.blocks {
        let cl_block = blocks[&block.id.as_u32()];
        // The entry block is already current from the parameter-
        // binding section above. Cranelift's debug-assert trips if we
        // call `switch_to_block` on an unfilled current block, so skip
        // the redundant switch on that one iteration.
        if Some(block.id.as_u32()) != entry_block_id || emitted_prologue {
            builder.switch_to_block(cl_block);
        }

        // Initialise both shadow-frame variables in the entry block,
        // immediately after parameter binding and before any user
        // statement runs. Legacy frame stays at 0; the raw frame
        // captures the calling thread's current shadow-stack depth so
        // we can restore to it at return.
        if !emitted_prologue {
            let zero = builder.ins().iconst(types::I64, 0);
            builder.def_var(shadow_frame_var, zero);
            let save_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_root_save")?;
            let save_ref = module.declare_func_in_func(save_id, builder.func);
            let call = builder.ins().call(save_ref, &[]);
            let frame = builder.inst_results(call)[0];
            builder.def_var(raw_shadow_frame_var, frame);
            // Function-prologue safepoint: cheap atomic-load + compare
            // in the common (under-threshold) case.
            let safepoint_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_safepoint")?;
            let safepoint_ref = module.declare_func_in_func(safepoint_id, builder.func);
            builder.ins().call(safepoint_ref, &[]);
            emitted_prologue = true;
        }

        // Loop back-edge safepoint: emit at the start of any block
        // that is a back-edge target. Cheap atomic-load + compare.
        if loop_headers.contains(&block.id.as_u32()) {
            let safepoint_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_safepoint")?;
            let safepoint_ref = module.declare_func_in_func(safepoint_id, builder.func);
            builder.ins().call(safepoint_ref, &[]);
        }

        if !cleanup_plan.is_empty() {
            for entry in cleanup_plan.at_block_entry(block.id) {
                emit_cleanup_drop(module, &mut builder, &mut locals, intrinsics, entry)?;
            }
        }

        for statement in &block.stmts {
            lower_statement(
                module,
                &mut builder,
                &mut locals,
                body,
                tcx,
                statement,
                intrinsics,
            )?;
        }

        if !cleanup_plan.is_empty() {
            for entry in cleanup_plan.at_block_exit(block.id) {
                emit_cleanup_drop(module, &mut builder, &mut locals, intrinsics, entry)?;
            }
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
            block.id.as_u32(),
            shadow_frame_var,
            raw_shadow_frame_var,
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
    body_cl_types: &[Option<ir::Type>],
    local: Local,
) -> Variable {
    if let Some(var) = locals.get(&local).copied() {
        return var;
    }
    // Read-before-write fallback: prefer the inferred effective type
    // (from body scanning) and only fall back to the MIR-declared
    // type if the inference turned up nothing.
    let inferred = body_cl_types.get(local.0 as usize).copied().flatten();
    let cl = inferred.unwrap_or_else(|| cl_type_of(tcx, body.local_ty(local), module));
    let var = builder.declare_var(cl);
    locals.insert(local, var);
    var
}

/// Propagates concrete cranelift types across every local in a body
/// by iterating to a fixed point. Seeds are the MIR-recorded types
/// that map directly to a cranelift scalar; then each `Copy`,
/// `BinaryOp`, and `Cast` assignment propagates the RHS's inferred
/// type to the destination (preferring float over int when an int
/// seed later gets rewritten by a float store — common when a
/// parameter's MIR type came out as `Error` but its body uses are
/// all floating-point).
fn infer_body_cl_types(body: &Body, tcx: &TyCtxt, module: &dyn Module) -> Vec<Option<ir::Type>> {
    let n = body.locals.len();
    let mut table: HashMap<Local, ir::Type> = HashMap::with_capacity(n);
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
                Operand::Const(ConstValue::Str(_)) => Some(module.target_config().pointer_type()),
                Operand::Const(ConstValue::Unit) => None,
                Operand::Copy(place) => {
                    if place.projection.is_empty() {
                        table.get(&place.local).copied()
                    } else {
                        cl_type_of_if_concrete(tcx, resolve_place_ty(tcx, body, place), module)
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
            Rvalue::Cast { operand, target } => {
                cl_type_of_if_concrete(tcx, *target, module).or_else(|| op_ty(operand))
            }
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
                            Some(current) if current == types::I64 && cl == types::F64 => {
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
    // Convert to Vec indexed by local.0 for O(1) lookup.
    let mut vec = vec![None; n];
    for (local, ty) in table {
        if (local.0 as usize) < n {
            vec[local.0 as usize] = Some(ty);
        }
    }
    vec
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
    body_cl_types: &[Option<ir::Type>],
    local: Local,
    value: ir::Value,
) {
    let preferred = body_cl_types.get(local.0 as usize).copied().flatten();
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
    let new_decl_ty = preferred_ty.unwrap_or(value_ty);
    let (var, decl_ty) = if let Some(v) = locals.get(&local).copied() {
        // Variable was declared earlier — its type is locked
        // for the rest of the function. Read the type back
        // from the builder rather than trusting the caller's
        // hint; mismatches here are the leading cause of
        // verifier panics.
        let actual = builder.try_use_var(v).map(|val| value_type(val, builder));
        (v, actual.unwrap_or(new_decl_ty))
    } else {
        let v = builder.declare_var(new_decl_ty);
        locals.insert(local, v);
        (v, new_decl_ty)
    };
    // Coerce the value to the declared variable width when they
    // disagree (e.g. we declared the local as F64 from inference,
    // but this particular value was loaded as I64 because the MIR
    // path still considered the source as an inference variable).
    let coerced = if decl_ty == value_ty {
        value
    } else if decl_ty == types::F64 && value_ty == types::I64 {
        builder
            .ins()
            .bitcast(types::F64, ir::MemFlags::new(), value)
    } else if decl_ty == types::I64 && value_ty == types::F64 {
        builder
            .ins()
            .bitcast(types::I64, ir::MemFlags::new(), value)
    } else if decl_ty == types::F32 && value_ty == types::I64 {
        let truncated = builder.ins().ireduce(types::I32, value);
        builder
            .ins()
            .bitcast(types::F32, ir::MemFlags::new(), truncated)
    } else if decl_ty == types::F32 && value_ty == types::F64 {
        builder.ins().fdemote(types::F32, value)
    } else if decl_ty == types::F64 && value_ty == types::F32 {
        builder.ins().fpromote(types::F64, value)
    } else if decl_ty.is_int() && value_ty.is_int() {
        if decl_ty.bits() > value_ty.bits() {
            builder.ins().sextend(decl_ty, value)
        } else {
            builder.ins().ireduce(decl_ty, value)
        }
    } else if decl_ty.is_int() && value_ty.is_float() {
        // Float→int through a bitcast at the same width then
        // resize as needed. Used when the MIR has assigned a
        // float-shaped value to an int-shaped local (rare —
        // typically a fallback path miscalculated the kind).
        let int_form = if value_ty == types::F64 {
            builder
                .ins()
                .bitcast(types::I64, ir::MemFlags::new(), value)
        } else {
            builder
                .ins()
                .bitcast(types::I32, ir::MemFlags::new(), value)
        };
        let int_ty = value_type(int_form, builder);
        if decl_ty.bits() > int_ty.bits() {
            builder.ins().sextend(decl_ty, int_form)
        } else if decl_ty.bits() < int_ty.bits() {
            builder.ins().ireduce(decl_ty, int_form)
        } else {
            int_form
        }
    } else if decl_ty.is_float() && value_ty.is_int() {
        // Int→float: resize to match width, then bitcast.
        let resized = if value_ty.bits() > decl_ty.bits() {
            builder.ins().ireduce(
                if decl_ty == types::F64 {
                    types::I64
                } else {
                    types::I32
                },
                value,
            )
        } else if value_ty.bits() < decl_ty.bits() {
            builder.ins().sextend(
                if decl_ty == types::F64 {
                    types::I64
                } else {
                    types::I32
                },
                value,
            )
        } else {
            value
        };
        builder.ins().bitcast(decl_ty, ir::MemFlags::new(), resized)
    } else {
        // Last-ditch: bitcast through equal-width types when we
        // can; otherwise drop the value and substitute a typed
        // zero so the def_var doesn't trap the verifier.
        if decl_ty.bits() == value_ty.bits() {
            builder.ins().bitcast(decl_ty, ir::MemFlags::new(), value)
        } else if decl_ty.is_int() {
            builder.ins().iconst(decl_ty, 0)
        } else if decl_ty == types::F64 {
            builder.ins().f64const(0.0)
        } else if decl_ty == types::F32 {
            builder.ins().f32const(0.0)
        } else {
            value
        }
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
                // Unrecognised intrinsic name in rvalue position.
                // The MIR lowerer has been audited to never emit
                // names without a matching cranelift dispatch, so
                // hitting this path is a real gap in the runtime
                // table — surface it loudly rather than silently
                // miscompiling.
                let fn_name = &body.name;
                bail!(
                    "native codegen: unrecognised intrinsic `{name}` in fn `{fn_name}`\n  \
                     add a dispatch arm in `lower_intrinsic_call` (and a runtime \
                     symbol in `gossamer-runtime/src/c_abi.rs`) for `{name}`"
                );
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
                Rvalue::Repeat { value, .. } => operand_cl_type(body, tcx, value, module),
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
                                        intrinsics.local_slots.get(&p.local).copied()
                                    } else {
                                        None
                                    }
                                })
                                .unwrap_or(1),
                            _ => 1,
                        };
                        let total = match kind {
                            gossamer_mir::AggregateKind::Array => (operands.len() as u32) * elem,
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
                        let total = u32::try_from(*count).unwrap_or(1).saturating_mul(elem);
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
            // For `Use(Copy(src.field…))` where the leaf type is a
            // multi-slot aggregate (struct/tuple embedded inline),
            // the rvalue lowering returns the field's address. Mark
            // the destination local as holding an aggregate of the
            // leaf's slot width so subsequent projections off of it
            // stride correctly instead of treating the address as
            // a scalar pointer to be dereferenced once.
            let projected_aggregate_slots: Option<u32> = match rvalue {
                Rvalue::Use(Operand::Copy(p)) if !p.projection.is_empty() => {
                    let leaf = resolve_place_ty(tcx, body, p);
                    let count = type_slot_count(tcx, leaf);
                    if count > 1 { Some(count) } else { None }
                }
                _ => None,
            };
            let value = lower_rvalue(
                module,
                builder,
                locals,
                body,
                tcx,
                rvalue,
                Some(dst_hint),
                intrinsics,
            )?;
            if place.projection.is_empty() {
                define_var_to(
                    builder,
                    locals,
                    &intrinsics.body_cl_types,
                    place.local,
                    value,
                );
                if let Some(elem) = aggregate_elem_ty {
                    intrinsics.elem_cl_ty.insert(place.local, elem);
                }
                if let Some(slots) = aggregate_elem_slots {
                    intrinsics.elem_slots.insert(place.local, slots);
                }
                if let Some(total) = aggregate_total_slots {
                    intrinsics.local_slots.insert(place.local, total);
                }
                if let Some(slots) = projected_aggregate_slots {
                    intrinsics.local_slots.insert(place.local, slots);
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
        StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => {}
        // SetDiscriminant: store variant index at offset 0 of the
        // enum's backing place. Matches the Downcast convention
        // (tag at slot 0, payload at +8).
        StatementKind::SetDiscriminant { place, variant } => {
            let addr = if place.projection.is_empty() {
                let var = ensure_var(
                    builder,
                    locals,
                    body,
                    tcx,
                    module,
                    &intrinsics.body_cl_types,
                    place.local,
                );
                builder.use_var(var)
            } else {
                lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?
            };
            let tag = builder.ins().iconst(types::I64, i64::from(*variant));
            builder.ins().store(
                MemFlags::trusted(),
                tag,
                addr,
                ir::immediates::Offset32::new(0),
            );
        }
        // GcWriteBarrier: emit a call to the runtime's barrier
        // entry point so the concurrent collector's mark phase
        // greys the target GcRef. The runtime helper takes a u32
        // GcRef index (the flat-ABI shape); only i64-encoded
        // value operands reach this path. Raw pointer-typed
        // values (Vec / String / HashMap / etc) are tracked
        // through the GC's allocation registry without an
        // explicit barrier.
        StatementKind::GcWriteBarrier { value, .. } => {
            let operand_ty = operand_cl_type(body, tcx, value, module);
            if operand_ty == Some(types::I64) {
                let v = lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    value,
                    Some(types::I64),
                    intrinsics,
                )?;
                let truncated = builder.ins().ireduce(types::I32, v);
                let barrier_fn =
                    intrinsics.extern_fn(module, "gos_rt_write_barrier", &[types::I32], &[])?;
                let fref = module.declare_func_in_func(barrier_fn, builder.func);
                let _ = builder.ins().call(fref, &[truncated]);
            }
        }
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
    /// Unsigned integer (any width). Routed through
    /// `gos_rt_print_u64` so values >= 2^63 print without a
    /// leading `-` (the bug `Int` would have).
    Uint,
    Float,
    Bool,
    Char,
    /// `Vec<i64>` (or any 8-byte-elem Vec): formatted at runtime
    /// via `gos_rt_vec_format_i64` into a `[v0, v1, …]` string.
    VecI64,
    /// `Vec<f64>` formatted via `gos_rt_vec_format_f64`.
    VecF64,
    /// `Vec<bool>` formatted via `gos_rt_vec_format_bool`.
    VecBool,
    /// `Vec<String>` formatted via `gos_rt_vec_format_string`.
    VecString,
    /// `Vec<Vec<i64>>` formatted via `gos_rt_vec_format_vec_i64`.
    VecVecI64,
    /// `[i64; N]` flat-buffer literal (no GosVec header). Formatted
    /// via `gos_rt_arr_format_i64(ptr, len)`.
    ArrI64(i64),
    /// `[f64; N]` flat-buffer literal.
    ArrF64(i64),
    /// `[bool; N]` flat-buffer literal.
    ArrBool(i64),
    /// `[String; N]` flat-buffer literal.
    ArrString(i64),
    /// `json::Value` — rendered via `gos_rt_json_render`.
    JsonValue,
    /// `errors::Error` — calls `gos_rt_error_message` then prints as string.
    ErrorMessage,
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

/// `true` if `t` is one of `u8 / u16 / u32 / u64 / u128 / usize`.
/// Unsigned scalars route to a separate print path so values
/// >= 2^63 don't appear with a leading `-` (the bug `Int` had).
fn int_ty_is_unsigned(t: IntTy) -> bool {
    matches!(
        t,
        IntTy::U8 | IntTy::U16 | IntTy::U32 | IntTy::U64 | IntTy::U128 | IntTy::Usize
    )
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
                TyKind::Int(int_ty) => {
                    if int_ty_is_unsigned(*int_ty) {
                        PrintKind::Uint
                    } else {
                        PrintKind::Int
                    }
                }
                TyKind::Unit | TyKind::Never => PrintKind::Int,
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
                // Fixed-size arrays: flat slot storage. The runtime
                // helpers that print `VecI64` / `VecF64` / etc. read
                // a `*mut GosVec` header, but a fixed array is just
                // a stack slot of `N * elem_bytes`. Routing fixed
                // arrays through the same Vec print kind works
                // because `nums` ends up as a `*mut GosVec` after
                // the typed-array promotion (`BuildIntArray` etc.)
                // — without this, `let nums = [1, 2, 3]; println!
                // ("{:?}", nums)` printed `<value>` even though
                // the array is fully typed and the helper exists.
                TyKind::Array { elem, len } => {
                    let n = i64::try_from(*len).unwrap_or(0);
                    match tcx.kind_of(*elem) {
                        TyKind::Int(_) => PrintKind::ArrI64(n),
                        TyKind::Float(_) => PrintKind::ArrF64(n),
                        TyKind::Bool => PrintKind::ArrBool(n),
                        TyKind::String => PrintKind::ArrString(n),
                        _ => PrintKind::Unsupported("array"),
                    }
                }
                TyKind::Slice(elem) => match tcx.kind_of(*elem) {
                    TyKind::Int(_) => PrintKind::VecI64,
                    TyKind::Float(_) => PrintKind::VecF64,
                    TyKind::Bool => PrintKind::VecBool,
                    TyKind::String => PrintKind::VecString,
                    TyKind::Vec(inner) => match tcx.kind_of(*inner) {
                        TyKind::Int(_) => PrintKind::VecVecI64,
                        _ => PrintKind::Unsupported("nested slice"),
                    },
                    _ => PrintKind::Unsupported("slice"),
                },
                TyKind::Vec(elem) => match tcx.kind_of(*elem) {
                    TyKind::Int(_) => PrintKind::VecI64,
                    TyKind::Float(_) => PrintKind::VecF64,
                    TyKind::Bool => PrintKind::VecBool,
                    TyKind::String => PrintKind::VecString,
                    TyKind::Vec(inner) => match tcx.kind_of(*inner) {
                        TyKind::Int(_) => PrintKind::VecVecI64,
                        _ => PrintKind::Unsupported("nested Vec"),
                    },
                    _ => PrintKind::Unsupported("Vec"),
                },
                TyKind::HashMap { .. } => PrintKind::Unsupported("HashMap"),
                TyKind::Sender(_) | TyKind::Receiver(_) => PrintKind::Unsupported("channel"),
                TyKind::JsonValue => PrintKind::JsonValue,
                TyKind::Adt { .. } => PrintKind::Unsupported("struct or enum"),
                TyKind::Closure { .. } => PrintKind::Unsupported("closure"),
                TyKind::FnDef { .. } | TyKind::FnPtr(_) | TyKind::FnTrait(_) => {
                    PrintKind::Unsupported("function")
                }
                TyKind::Dyn(_) => PrintKind::Unsupported("dyn Trait"),
                TyKind::DynError => PrintKind::ErrorMessage,
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
            Projection::Field(idx) => field_ty_at(tcx, ty, *idx).unwrap_or(ty),
            Projection::Index(_) => {
                // `&[(i64, f64); 15][j]` keeps the indexed type as the
                // tuple element, not as the reference. Without peeling
                // here, downstream `type_slot_count` sees `Ref` (1 slot)
                // and the by-value read path drops the second half of
                // every tuple element. Peel through any chain of `Ref`
                // wrappers first, then walk into the array/slice/vec.
                let mut peeled = ty;
                while let TyKind::Ref { inner, .. } = tcx.kind_of(peeled) {
                    peeled = *inner;
                }
                match tcx.kind_of(peeled) {
                    TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => *elem,
                    _ => ty,
                }
            }
            Projection::Deref => match tcx.kind_of(ty) {
                TyKind::Ref { inner, .. } => *inner,
                _ => ty,
            },
            Projection::Downcast(_) | Projection::Discriminant => ty,
        };
    }
    ty
}

/// Returns `true` when the operand has `String` or `&String` type,
/// indicating that comparison ops must use `gos_rt_str_compare`.
fn operand_is_string(tcx: &TyCtxt, body: &Body, operand: &Operand) -> bool {
    match operand {
        // After copy-propagation, string locals may be substituted with
        // Const(Str(...)) inline. These are always strings.
        Operand::Const(gossamer_mir::ConstValue::Str(_)) => return true,
        Operand::Copy(p) => {
            let ty = resolve_place_ty(tcx, body, p);
            return match tcx.kind_of(ty) {
                TyKind::String => true,
                TyKind::Ref { inner, .. } => matches!(tcx.kind_of(*inner), TyKind::String),
                _ => false,
            };
        }
        _ => {}
    }
    false
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
        TyKind::Tuple(elems) => elems
            .iter()
            .map(|t| type_slot_count(tcx, *t))
            .sum::<u32>()
            .max(1),
        TyKind::Array { elem, len } => u32::try_from(len)
            .unwrap_or(1)
            .saturating_mul(type_slot_count(tcx, elem)),
        TyKind::Adt { def, .. } => {
            // Result<T, E> and Option<T> use sentinel DefIds
            // (u32::MAX, u32::MAX-1) that don't appear in
            // `struct_field_tys`. Both have a 2-slot heap layout:
            // `[disc: i64, payload: i64]`. Without this special
            // case the by-value-aggregate return path copies only
            // the disc word and zeroes the payload — corrupting
            // every `Ok(v)` returned across a function boundary.
            if def.local == u32::MAX || def.local == u32::MAX - 1 {
                return 2;
            }
            tcx.struct_field_tys(def).map_or(1, |tys| {
                tys.iter()
                    .map(|t| type_slot_count(tcx, *t))
                    .sum::<u32>()
                    .max(1)
            })
        }
        _ => 1,
    }
}

/// Returns the byte offset of field `idx` within `ty`, summing the
/// slot widths of every preceding field. Falls back to `idx * 8` for
/// types whose field layout cannot be looked up (opaque ADTs, refs).
fn field_byte_offset(tcx: &TyCtxt, ty: Ty, idx: u32) -> u32 {
    let target = idx as usize;
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems
            .iter()
            .take(target)
            .map(|t| type_slot_count(tcx, *t))
            .sum::<u32>()
            .saturating_mul(8),
        TyKind::Adt { def, .. } => {
            // Sentinels for Result/Option use a flat 2-slot
            // [disc, payload] layout where each field is one slot.
            if def.local == u32::MAX || def.local == u32::MAX - 1 {
                return idx * 8;
            }
            tcx.struct_field_tys(def).map_or(idx * 8, |tys| {
                tys.iter()
                    .take(target)
                    .map(|t| type_slot_count(tcx, *t))
                    .sum::<u32>()
                    .saturating_mul(8)
            })
        }
        TyKind::Ref { inner, .. } => field_byte_offset(tcx, inner, idx),
        _ => idx * 8,
    }
}

/// Returns the type of the `idx`-th field of `ty`, or `None` when
/// the layout is opaque to the type interner (refs, generics).
fn field_ty_at(tcx: &TyCtxt, ty: Ty, idx: u32) -> Option<Ty> {
    let target = idx as usize;
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems.get(target).copied(),
        TyKind::Adt { def, .. } => {
            if def.local == u32::MAX || def.local == u32::MAX - 1 {
                return None;
            }
            tcx.struct_field_tys(def)
                .and_then(|tys| tys.get(target).copied())
        }
        TyKind::Ref { inner, .. } => field_ty_at(tcx, inner, idx),
        _ => None,
    }
}

/// Audit C6 — dynamic array-index bounds check.
///
/// Emits a compare-and-trap for `arr[i]` against the statically-
/// known length of a fixed-size array. Negative indices wrap to a
/// large `u64` under the unsigned compare, so a single `>=` covers
/// both ends without a separate sign branch. On failure we jump to
/// a side block that calls `gos_rt_panic_oob` and falls into an
/// unreachable `trap` so the rest of the block remains
/// well-formed.
///
/// Only fires when `ty` (after peeling any `Ref` wrappers) is a
/// fixed `TyKind::Array { len, .. }`. `Vec` / `Slice` shapes reach
/// element storage through the `gos_rt_vec_get_*` intrinsics whose
/// own implementations validate the index, so this projection path
/// only needs to cover the flat-stack-slot case.
///
/// The check is skipped when `GOSSAMER_DISABLE_BOUNDS_CHECK=1` is
/// set in the build environment — a release-only opt-out for
/// programs that can prove safety statically.
fn emit_array_bounds_check(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    intrinsics: &mut IntrinsicContext,
    current_ty: Ty,
    idx_val: ir::Value,
    tcx: &TyCtxt,
) -> Result<()> {
    if std::env::var_os("GOSSAMER_DISABLE_BOUNDS_CHECK").is_some() {
        return Ok(());
    }
    let mut peeled = current_ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(peeled).clone() {
        peeled = inner;
    }
    let TyKind::Array { len, .. } = tcx.kind_of(peeled).clone() else {
        return Ok(());
    };
    let len_i64 = i64::try_from(len).unwrap_or(i64::MAX);
    // Widen the index to i64 for both the compare and the
    // helper-call payload. Cranelift requires both icmp operands to
    // share a type.
    let idx64 = match value_type(idx_val, builder) {
        t if t == types::I64 => idx_val,
        t if t.is_int() && t.bits() < 64 => builder.ins().sextend(types::I64, idx_val),
        _ => idx_val,
    };
    let len_val = builder.ins().iconst(types::I64, len_i64);
    // Unsigned >=: i64 compared as u64 also catches negative idx
    // (which wrap to >= 2^63 — strictly greater than any sane len).
    let oob = builder
        .ins()
        .icmp(IntCC::UnsignedGreaterThanOrEqual, idx64, len_val);
    let ok = builder.create_block();
    let fail = builder.create_block();
    builder.ins().brif(oob, fail, &[], ok, &[]);
    builder.switch_to_block(fail);
    // Call `gos_rt_panic_oob("array index", idx, len)` then fall
    // into an `unreachable` trap so the verifier sees a terminator
    // even though the helper is `-> !`. Blocks are sealed
    // collectively at function-end via `seal_all_blocks`.
    let panic_fn = intrinsics.extern_fn_by_name(module, "gos_rt_panic_oob")?;
    let panic_ref = module.declare_func_in_func(panic_fn, builder.func);
    let what_data = intrinsics.intern_string(module, "array index")?;
    let what_global = module.declare_data_in_func(what_data, builder.func);
    let ptr_ty = module.target_config().pointer_type();
    let what_ptr = builder.ins().global_value(ptr_ty, what_global);
    let _ = builder.ins().call(panic_ref, &[what_ptr, idx64, len_val]);
    builder.ins().trap(ir::TrapCode::user(5).unwrap());
    builder.switch_to_block(ok);
    Ok(())
}

/// Computes the byte address of the projected slot within its root
/// aggregate, returning a pointer-typed value suitable for a
/// `load` / `store`. Works for `Field(i)` (offset `i*8`) and
/// `Index(local)` (offset `idx*8`). Deref/Downcast/Discriminant
/// remain unimplemented.
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
fn lower_place_address(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    let var = ensure_var(
        builder,
        locals,
        body,
        tcx,
        module,
        &intrinsics.body_cl_types,
        place.local,
    );
    let ptr_ty = module.target_config().pointer_type();
    let root_value = builder.use_var(var);
    // The root local holds a pointer (an aggregate's stack-slot
    // address). Widen it to the target's pointer type so later
    // `iadd`s don't fail on mismatched operand widths.
    let mut current = match value_type(root_value, builder) {
        t if t == ptr_ty => root_value,
        t if t == types::I64 && ptr_ty == types::I32 => builder.ins().ireduce(ptr_ty, root_value),
        t if t == types::I32 && ptr_ty == types::I64 => builder.ins().uextend(ptr_ty, root_value),
        _ => root_value,
    };
    // Track the type at each step so nested struct/tuple projections
    // can compute their byte offsets from the actual field layout
    // (each prior field's slot count) rather than a flat `idx * 8`.
    let mut current_ty = body.local_ty(place.local);
    // Track the per-element stride in slots for `Index(_)`. Seeded
    // from the root local's recorded metadata (or the type's
    // element type when no metadata exists), then re-derived from
    // the live `current_ty` after each projection step.
    let mut stride_slots = intrinsics
        .elem_slots
        .get(&place.local)
        .copied()
        .or_else(|| stride_slots_from_ty(tcx, body.local_ty(place.local)))
        .unwrap_or(1);
    for projection in &place.projection {
        match projection {
            Projection::Field(idx) => {
                let off_bytes = field_byte_offset(tcx, current_ty, *idx);
                let offset = builder.ins().iconst(ptr_ty, i64::from(off_bytes));
                current = builder.ins().iadd(current, offset);
                if let Some(ft) = field_ty_at(tcx, current_ty, *idx) {
                    current_ty = ft;
                    stride_slots = stride_slots_from_ty(tcx, current_ty).unwrap_or(1);
                } else {
                    stride_slots = 1;
                }
            }
            Projection::Index(index_local) => {
                let index_var = ensure_var(
                    builder,
                    locals,
                    body,
                    tcx,
                    module,
                    &intrinsics.body_cl_types,
                    *index_local,
                );
                let idx_val = builder.use_var(index_var);
                // Audit C6: bounds-check every dynamic Index against
                // the statically-known length of a fixed-size array.
                // Negative indices are caught by the unsigned compare
                // (i64-as-u64 wraps to a large value that trips the
                // `>=` test). The check is opt-out via
                // `GOSSAMER_DISABLE_BOUNDS_CHECK=1` for micro-bench
                // programs that can prove safety. Vec/Slice indexing
                // does not reach this path — those go through
                // `gos_rt_vec_get_*` intrinsics which check internally.
                emit_array_bounds_check(module, builder, intrinsics, current_ty, idx_val, tcx)?;
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
                let stride = builder.ins().iconst(ptr_ty, i64::from(stride_slots) * 8);
                let byte_offset = builder.ins().imul(idx_ptr, stride);
                current = builder.ins().iadd(current, byte_offset);
                // After indexing, the cursor sits inside a single
                // element; advance `current_ty` to the element type
                // so subsequent Field projections compute their
                // offsets relative to that element's layout. Peel
                // any `Ref` wrappers first so `&[(T, U); N][j].0`
                // descends into the tuple instead of treating the
                // element as opaque.
                let mut peeled = current_ty;
                while let TyKind::Ref { inner, .. } = tcx.kind_of(peeled).clone() {
                    peeled = inner;
                }
                current_ty = match tcx.kind_of(peeled).clone() {
                    TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => elem,
                    _ => current_ty,
                };
                stride_slots = 1;
            }
            Projection::Deref => {
                // `*ptr`: the local already holds a pointer; after
                // this projection the address is just that pointer
                // value. Subsequent Field/Index projections
                // compute offsets off of it.
                //
                // only emit the indirect load
                // when the source is a heap-pointer-shaped Adt
                // (slot_count = None). Inline multi-slot
                // aggregates already hold the slot address in the
                // Cranelift Variable — loading would dereference
                // the stack slot's first 8 bytes (typically a
                // field, possibly 0) as if it were the pointer,
                // segfaulting at the next projection. This
                // mirrors the LLVM fix recorded in
                // `llvm_call_arg_ref_aggregate_fix.md`.
                let peeled = match tcx.kind_of(current_ty) {
                    TyKind::Ref { inner, .. } => *inner,
                    _ => current_ty,
                };
                let inline_aggregate =
                    matches!(tcx.kind_of(peeled), TyKind::Tuple(_) | TyKind::Array { .. })
                        || (matches!(tcx.kind_of(peeled), TyKind::Adt { .. })
                            && type_slot_count(tcx, peeled) > 1);
                if !inline_aggregate {
                    let loaded = builder.ins().load(ptr_ty, MemFlags::trusted(), current, 0);
                    current = loaded;
                }
                if let TyKind::Ref { inner, .. } = tcx.kind_of(current_ty).clone() {
                    current_ty = inner;
                }
                stride_slots = stride_slots_from_ty(tcx, current_ty).unwrap_or(1);
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
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
fn lower_place_store(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    value: ir::Value,
    leaf_ty: ir::Type,
    intrinsics: &mut IntrinsicContext,
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
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
        intrinsics
            .local_declared_ty
            .insert(destination.local, ret_ty);
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            value,
        );
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
        module,
        builder,
        locals,
        body,
        tcx,
        destination,
        value,
        leaf_ty,
        intrinsics,
    )
}

/// Lowers `args[0]` as a pointer-typed call argument, defaulting to
/// the null pointer when the operand is missing. Used by the
/// single-arg JSON intrinsics so the per-call boilerplate stays
/// readable.
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
fn lower_first_ptr_arg(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    let ptr_ty = module.target_config().pointer_type();
    let value = match args.first() {
        Some(a) => lower_operand(
            module,
            builder,
            locals,
            body,
            tcx,
            a,
            Some(ptr_ty),
            intrinsics,
        )?,
        None => builder.ins().iconst(ptr_ty, 0),
    };
    coerce_arg_to(builder, value, ptr_ty)
}

/// Coerce a value to the cranelift type expected by a call-site or
/// Emits a `<free_fn>(local)` call for a cleanup entry. Used by
/// the per-block drop sites and the at-Return fallback. Skips
/// silently if the local has no cranelift backing variable yet
/// (defensive: cleanup should only schedule allocator destinations,
/// which always have a backing var by the time their block runs).
fn emit_cleanup_drop(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    intrinsics: &mut IntrinsicContext,
    entry: &gossamer_mir::CleanupEntry,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let Some(&var) = locals.get(&entry.local) else {
        return Ok(());
    };
    let raw = builder.use_var(var);
    let ptr = coerce_arg_to(builder, raw, ptr_ty).unwrap_or(raw);
    let free_fn = intrinsics.extern_fn(module, entry.free_fn, &[ptr_ty], &[])?;
    let free_ref = module.declare_func_in_func(free_fn, builder.func);
    builder.ins().call(free_ref, &[ptr]);
    Ok(())
}

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
        return Ok(builder
            .ins()
            .bitcast(types::F64, ir::MemFlags::new(), value));
    }
    if have == types::F64 && want == types::I64 {
        return Ok(builder
            .ins()
            .bitcast(types::I64, ir::MemFlags::new(), value));
    }
    if have.is_int() && want.is_int() {
        if have.bits() > want.bits() {
            return Ok(builder.ins().ireduce(want, value));
        }
        if have.bits() < want.bits() {
            // Gossamer integer types are signed by default (`i8..i128`,
            // `isize`). Sign-extend on narrow→wide widening so a
            // negative narrow value preserves its value at the wider
            // width. The unsigned-widening path is handled by callers
            // that explicitly hold an unsigned MIR type and route
            // through `coerce_arg_to_unsigned`.
            return Ok(builder.ins().sextend(want, value));
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
    // Same-width bit reinterpret (i32 ↔ f32, i8 ↔ ints, etc.).
    if have.bits() == want.bits() {
        return Ok(builder.ins().bitcast(want, ir::MemFlags::new(), value));
    }
    if have.is_float() && want.is_int() {
        let int_form = if have == types::F64 {
            builder
                .ins()
                .bitcast(types::I64, ir::MemFlags::new(), value)
        } else {
            builder
                .ins()
                .bitcast(types::I32, ir::MemFlags::new(), value)
        };
        let int_ty = value_type(int_form, builder);
        if want.bits() > int_ty.bits() {
            return Ok(builder.ins().sextend(want, int_form));
        }
        if want.bits() < int_ty.bits() {
            return Ok(builder.ins().ireduce(want, int_form));
        }
        return Ok(int_form);
    }
    if have.is_int() && want.is_float() {
        let int_ty = if want == types::F64 {
            types::I64
        } else {
            types::I32
        };
        let resized = if have.bits() > int_ty.bits() {
            builder.ins().ireduce(int_ty, value)
        } else if have.bits() < int_ty.bits() {
            builder.ins().sextend(int_ty, value)
        } else {
            value
        };
        return Ok(builder.ins().bitcast(want, ir::MemFlags::new(), resized));
    }
    // Last resort: typed zero of the wanted shape so the call
    // doesn't fail the verifier.
    if want.is_int() {
        Ok(builder.ins().iconst(want, 0))
    } else if want == types::F64 {
        Ok(builder.ins().f64const(0.0))
    } else if want == types::F32 {
        Ok(builder.ins().f32const(0.0))
    } else {
        Ok(value)
    }
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
            // Caller wrote a narrower value into a wider slot.
            // Gossamer integer types are signed by default, so sign-
            // extend the bits. Same-width by construction is the common
            // case; this branch defends against a typeck-emitted
            // narrower source feeding a wider aggregate slot.
            return Ok(builder.ins().sextend(leaf_ty, value));
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

#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "intrinsic dispatch / arg-marshal sequence — flat-table shape preserved for grep-ability"
)]
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
    src_block: u32,
    shadow_frame_var: Variable,
    raw_shadow_frame_var: Variable,
) -> Result<()> {
    match terminator {
        Terminator::Goto { target } => {
            // Loop back-edge — emit a unified safepoint so the
            // tracing GC threshold check fires and the concurrent
            // GC can advance. Cheap atomic-load + compare in the
            // common case.
            if target.as_u32() <= src_block {
                let safepoint_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_safepoint")?;
                let safepoint_ref = module.declare_func_in_func(safepoint_id, builder.func);
                builder.ins().call(safepoint_ref, &[]);
            }
            let block = blocks[&target.as_u32()];
            builder.ins().jump(block, &[]);
        }
        Terminator::Return => {
            // Legacy GcRef-handle shadow stack close (no-op when
            // nothing was pushed, which is the production path).
            {
                let restore_id =
                    intrinsics.extern_fn_by_name(module, "gos_rt_gc_shadow_restore")?;
                let restore_ref = module.declare_func_in_func(restore_id, builder.func);
                let frame = builder.use_var(shadow_frame_var);
                builder.ins().call(restore_ref, &[frame]);
            }
            // Raw-pointer tracing-GC shadow stack close: truncate
            // back to the depth captured at function entry so every
            // aggregate root pushed inside this body is removed.
            {
                let restore_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_root_restore")?;
                let restore_ref = module.declare_func_in_func(restore_id, builder.func);
                let frame = builder.use_var(raw_shadow_frame_var);
                builder.ins().call(restore_ref, &[frame]);
            }
            // Emit cleanup calls for every owning heap-typed local
            // identified by `gossamer_mir::plan_cleanup`. Each entry
            // is a `(local, free_fn)` pair where the local was
            // assigned the result of a runtime allocator call
            // (`gos_rt_heap_*_new` / `gos_rt_chan_new`) and the MIR
            // escape analysis confirmed the value never leaves this
            // body. Without this loop the `_free` symbols ship in
            // the runtime but are never called — every owning Vec /
            // Channel leaks to process exit (C2 in
            // `~/dev/contexts/lang/adversarial_analysis.md`).
            let cleanup = gossamer_mir::plan_cleanup(body);
            if !cleanup.is_empty() {
                let ptr_ty = module.target_config().pointer_type();
                for entry in cleanup.at_return() {
                    let Some(&var) = locals.get(&entry.local) else {
                        continue;
                    };
                    let raw = builder.use_var(var);
                    let ptr = coerce_arg_to(builder, raw, ptr_ty).unwrap_or(raw);
                    let free_fn = intrinsics.extern_fn(module, entry.free_fn, &[ptr_ty], &[])?;
                    let free_ref = module.declare_func_in_func(free_fn, builder.func);
                    builder.ins().call(free_ref, &[ptr]);
                }
            }
            let retval = match locals.get(&Local(0)).copied() {
                Some(var) => builder.use_var(var),
                None => builder.ins().iconst(types::I64, 0),
            };
            // Aggregate returns: the local-0 variable holds a pointer
            // into a stack slot owned by the current frame. Returning
            // it directly hands the caller a dangling pointer the
            // moment the frame pops. Heap-allocate via
            // `gos_rt_gc_alloc`, copy the inline data over, and return
            // the heap pointer instead. Mirrors the LLVM tier so both
            // backends agree on the by-value-aggregate ABI.
            let ret_ty_mir = body.local_ty(Local(0));
            let ret_is_aggregate = matches!(
                tcx.kind_of(ret_ty_mir),
                TyKind::Tuple(_) | TyKind::Array { .. } | TyKind::Adt { .. }
            );
            let ret_slots = type_slot_count(tcx, ret_ty_mir).max(1);
            // 1-slot values are themselves the value (an i64
            // / pointer / scalar). Copying through them by
            // dereferencing the local would treat the value as
            // a *pointer to data* and load 8 bytes from the
            // pointee — corrupting any function returning a
            // user-defined enum (heap pointer to a `[disc, ...]`
            // aggregate). Only the multi-slot aggregate cases
            // (real Tuple, Array, struct Adt) need the heap
            // copy that escapes the stack frame.
            let ret_is_aggregate = ret_is_aggregate && ret_slots > 1;
            if ret_is_aggregate {
                // Arrays are always heap-allocated (calloc'd by Rvalue::Repeat /
                // Rvalue::Aggregate). The local already holds a dedicated heap
                // pointer, so returning it directly is safe — no second copy
                // needed. Tuples and Adts may carry pointers into a containing
                // aggregate's buffer (field-projection assignments), so those
                // still need the gc_alloc + memcpy escape path.
                if matches!(tcx.kind_of(ret_ty_mir), TyKind::Array { .. }) {
                    builder.ins().return_(&[retval]);
                } else {
                    let bytes = u64::from(ret_slots) * 8;
                    let alloc_id = intrinsics.extern_fn_by_name(module, "gos_rt_gc_alloc")?;
                    let alloc_ref = module.declare_func_in_func(alloc_id, builder.func);
                    let bytes_v = builder.ins().iconst(types::I64, bytes as i64);
                    let call = builder.ins().call(alloc_ref, &[bytes_v]);
                    let heap = builder.inst_results(call)[0];
                    let slots = type_slot_count(tcx, ret_ty_mir).max(1);
                    for slot_idx in 0..slots {
                        let off = (slot_idx as i32) * 8;
                        let word = builder.ins().load(
                            types::I64,
                            MemFlags::trusted(),
                            retval,
                            ir::immediates::Offset32::new(off),
                        );
                        builder.ins().store(
                            MemFlags::trusted(),
                            word,
                            heap,
                            ir::immediates::Offset32::new(off),
                        );
                    }
                    // Aggregate-return: push the heap copy onto the
                    // shadow stack AFTER the function's restore so
                    // the entry persists into the caller's frame.
                    // The caller's own `gos_rt_gc_root_restore` at
                    // its return will pop this entry alongside its
                    // own pushes.
                    emit_root_push(module, builder, intrinsics, heap)?;
                    builder.ins().return_(&[heap]);
                }
            } else {
                // Coerce the return value to the function's declared
                // return type. The MIR may have stored a narrow
                // type (i8 for bool) into the RETURN local even when
                // the function signature is `i64`; cranelift's
                // verifier rejects width mismatches between the
                // returned value and the function signature.
                let want = builder
                    .func
                    .signature
                    .returns
                    .first()
                    .map_or(types::I64, |p| p.value_type);
                let coerced = coerce_arg_to(builder, retval, want)
                    .unwrap_or_else(|_| builder.ins().iconst(want, 0));
                builder.ins().return_(&[coerced]);
            }
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
                    &name,
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    args,
                    destination,
                    intrinsics,
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
                // `FnTrait` is the closure-trait callable shape. Its
                // value is an env_ptr (heap blob `[fn_addr, captures…]`),
                // so it routes through the same env+code dispatch the
                // MIR `Closure` shape uses. Bare `fn` and `fn item`
                // values stay on the raw single-pointer call path.
                // Direct call only when the local was assigned from
                // an `Operand::FnRef` (a `FnDef`-typed value): the
                // value IS the function address. `FnPtr` and
                // `FnTrait` locals are now uniformly env_ptr-shaped
                // — the MIR's coercion at let / return / assign
                // boundaries wraps every bare fn item into an
                // `[fn_addr, captures…]` heap blob first. Loading
                // through env[0] and forwarding `(env, args…)` is
                // the universal dispatch.
                let is_plain_fn = matches!(tcx.kind_of(callee_ty), TyKind::FnDef { .. });
                // Pull the FnTrait / FnPtr signature so the
                // indirect-call dispatch uses real argument and
                // return types, not flat i64. This is the fix
                // that lets capturing closures returning bool /
                // f64 / aggregates flow through Fn(...) params
                // without calling-convention drift.
                let fn_sig = match tcx.kind_of(callee_ty).clone() {
                    TyKind::FnTrait(s) | TyKind::FnPtr(s) => Some(s),
                    _ => None,
                };
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
                // Per-arg cranelift types: prefer the FnTrait /
                // FnPtr sig's input types over flat-i64 so f64 /
                // bool / aggregate args use the right register.
                let mut typed_param_tys: Vec<ir::Type> = Vec::with_capacity(args.len());
                for (i, _) in args.iter().enumerate() {
                    let want = match &fn_sig {
                        Some(sig_ref) if i < sig_ref.inputs.len() => {
                            cl_type_of(tcx, sig_ref.inputs[i], module)
                        }
                        _ => types::I64,
                    };
                    typed_param_tys.push(want);
                    sig.params.push(AbiParam::new(want));
                }
                let typed_ret_ty = match &fn_sig {
                    Some(sig_ref) if !matches!(tcx.kind_of(sig_ref.output), TyKind::Unit) => {
                        Some(cl_type_of(tcx, sig_ref.output, module))
                    }
                    _ => Some(types::I64),
                };
                if let Some(t) = typed_ret_ty {
                    sig.returns.push(AbiParam::new(t));
                }
                let sig_ref = builder.import_signature(sig);
                let mut arg_values = Vec::with_capacity(args.len() + 1);
                if !is_plain_fn {
                    arg_values.push(env_value);
                }
                for (i, op) in args.iter().enumerate() {
                    let v =
                        lower_operand(module, builder, locals, body, tcx, op, None, intrinsics)?;
                    let want = typed_param_tys.get(i).copied().unwrap_or(types::I64);
                    let coerced = coerce_arg_to(builder, v, want).unwrap_or(v);
                    arg_values.push(coerced);
                }
                let call = builder.ins().call_indirect(sig_ref, fn_ptr, &arg_values);
                let results = builder.inst_results(call).to_vec();
                if let Some(&ret) = results.first() {
                    store_call_result(
                        module,
                        builder,
                        locals,
                        body,
                        tcx,
                        destination,
                        ret,
                        intrinsics,
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
                    let ptr_ty_local = module.target_config().pointer_type();
                    for (idx, op) in args.iter().enumerate() {
                        let mut v = lower_operand(
                            module, builder, locals, body, tcx, op, None, intrinsics,
                        )?;
                        // Special case: char→ptr promotion for
                        // string-API helpers where the user
                        // passed a `char` literal where a
                        // `String` was expected (`s.split(',')`,
                        // `s.contains('-')`, …). The existing
                        // coerce extends i32 to i64 which is
                        // wrong — the runtime would dereference
                        // the char value as a pointer. Route
                        // through `gos_rt_char_to_str` instead.
                        if let Some(want) = expected.get(idx).copied() {
                            let have = value_type(v, builder);
                            if want == ptr_ty_local
                                && have == types::I32
                                && operand_is_char(body, tcx, op)
                            {
                                let cts = intrinsics.extern_fn(
                                    module,
                                    "gos_rt_char_to_str",
                                    &[types::I32],
                                    &[ptr_ty_local],
                                )?;
                                let cts_ref = module.declare_func_in_func(cts, builder.func);
                                let call = builder.ins().call(cts_ref, &[v]);
                                v = builder.inst_results(call)[0];
                            } else {
                                v = coerce_arg_to(builder, v, want)?;
                            }
                        }
                        arg_values.push(v);
                    }
                    let call = builder.ins().call(func_ref, &arg_values);
                    let results = builder.inst_results(call).to_vec();
                    if let Some(&ret) = results.first() {
                        store_call_result(
                            module,
                            builder,
                            locals,
                            body,
                            tcx,
                            destination,
                            ret,
                            intrinsics,
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
                // Registry-known `gos_rt_*` symbol that wasn't pre-bound
                // into `callees_by_name`. The ABI registry walk above
                // declares the function via `extern_fn_by_name`, so all
                // we have to do is fetch the FuncId, build a callsite
                // ext-func ref, lower args, and emit the call. Without
                // this branch every newly-added runtime helper that
                // MIR references by name silently zeros at codegen time
                // (cranelift_dispatch_table.md, 2026-04-28).
                if name.starts_with("gos_rt_") {
                    if let Some(entry) = gossamer_abi::lookup(name) {
                        let id = intrinsics.extern_fn_by_name(module, entry.name)?;
                        let func_ref = module.declare_func_in_func(id, builder.func);
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
                                module,
                                builder,
                                locals,
                                body,
                                tcx,
                                destination,
                                ret,
                                intrinsics,
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
                let is_variant = name.chars().next().is_some_and(char::is_uppercase);
                if name.contains("::") || is_variant {
                    // Option / Result variant constructors with a
                    // payload (`Ok(v)`, `Some(v)`, `Err(e)`) lower
                    // to identity: the wrapped value passes through
                    // unchanged so `r.unwrap()` (also identity)
                    // recovers it. Variants without a payload
                    // (`None`, no-payload user-enum constructors)
                    // continue to default to zero.
                    let result_value =
                        if matches!(name.as_str(), "Ok" | "Some" | "Err") && !args.is_empty() {
                            lower_operand(
                                module, builder, locals, body, tcx, &args[0], None, intrinsics,
                            )?
                        } else {
                            builder.ins().iconst(types::I64, 0)
                        };
                    store_call_result(
                        module,
                        builder,
                        locals,
                        body,
                        tcx,
                        destination,
                        result_value,
                        intrinsics,
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
                // Soft fallback: emit a typed zero into the
                // destination so the program still builds. Wrong
                // semantics but better than refusing to compile
                // when the user calls an unknown stdlib helper.
                //
                // 0.6.0 stability hardening: surface every soft-zero
                // call at compile time. With `GOSSAMER_STRICT_LOWER=1`
                // this becomes a hard error; otherwise the previous
                // behaviour (zero-stub) is preserved for in-flight
                // programs that depend on it, but a warning is
                // emitted so typos are not silently miscompiled.
                eprintln!(
                    "warning: gossamer codegen: emitting zero-stub for unknown call '{name}' — \
                    typos produce silent zeros that may segfault on later dereference; \
                    set GOSSAMER_STRICT_LOWER=1 to refuse compilation"
                );
                if std::env::var_os("GOSSAMER_STRICT_LOWER").is_some() {
                    bail!(
                        "native codegen: refusing to emit zero-stub for unknown call '{name}' (GOSSAMER_STRICT_LOWER set)"
                    );
                }
                let zero = builder.ins().iconst(types::I64, 0);
                store_call_result(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    destination,
                    zero,
                    intrinsics,
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
            // Soft fallback for unknown FnRef defs: zero out the
            // destination and continue. Common producer is enum
            // variant constructors whose DefId the resolver
            // allocates without ever emitting a body. Under
            // `GOSSAMER_STRICT_LOWER=1` this is an error (same
            // policy as the unknown-name path above).
            let Ok(func_ref) = resolve_callee(callee, callees_by_def, callees_by_name) else {
                if std::env::var_os("GOSSAMER_STRICT_LOWER").is_some() {
                    bail!(
                        "native codegen: refusing to emit zero-stub for unresolved FnRef callee (GOSSAMER_STRICT_LOWER set)"
                    );
                }
                let zero = builder.ins().iconst(types::I64, 0);
                store_call_result(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    destination,
                    zero,
                    intrinsics,
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
            };
            let expected = builder
                .func
                .dfg
                .signatures
                .get(builder.func.dfg.ext_funcs[func_ref].signature)
                .map(|s| s.params.iter().map(|p| p.value_type).collect::<Vec<_>>())
                .unwrap_or_default();
            let mut arg_values: Vec<ir::Value> = Vec::with_capacity(args.len());
            for (idx, op) in args.iter().enumerate() {
                let mut v =
                    lower_operand(module, builder, locals, body, tcx, op, None, intrinsics)?;
                // Defensive copy of by-value aggregate args: the
                // local holds a pointer into a heap slot the caller
                // owns. Forwarding it directly aliases the caller's
                // struct, so the callee's `mut p; p.x = …` mutates
                // the caller's value too. Allocate fresh storage,
                // memcpy the slots over, and pass the new pointer.
                if let Some(slots) = operand_aggregate_slots(body, tcx, op) {
                    v = clone_aggregate_value(module, builder, intrinsics, v, slots)?;
                }
                if let Some(want) = expected.get(idx).copied() {
                    v = coerce_arg_to(builder, v, want)?;
                }
                arg_values.push(v);
            }
            let call = builder.ins().call(func_ref, &arg_values);
            let results = builder.inst_results(call).to_vec();
            if let Some(&ret) = results.first() {
                store_call_result(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    destination,
                    ret,
                    intrinsics,
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
            // Loop back-edge → emit preempt check before dispatching.
            let has_back_edge =
                arms.iter().any(|(_, t)| t.as_u32() <= src_block) || default.as_u32() <= src_block;
            if has_back_edge {
                let _ = (&module, &builder, &intrinsics); // Track 3 / H11: preempt-check at back-edges lands separately.
            }
            let value = lower_operand(
                module,
                builder,
                locals,
                body,
                tcx,
                discriminant,
                None,
                intrinsics,
            )?;
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
                let matched = builder
                    .ins()
                    .icmp(ir::condcodes::IntCC::Equal, value, cmp_value);
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
            let expected_value = builder.ins().iconst(value_ty, i64::from(*expected));
            let pass = builder.create_block();
            let fail = builder.create_block();
            let matched = builder
                .ins()
                .icmp(ir::condcodes::IntCC::Equal, value, expected_value);
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
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
    let print_str = intrinsics.extern_fn_by_name(module, "gos_rt_print_str")?;
    let print_i64 = intrinsics.extern_fn_by_name(module, "gos_rt_print_i64")?;
    let print_f64 = intrinsics.extern_fn_by_name(module, "gos_rt_print_f64")?;
    let print_bool = intrinsics.extern_fn_by_name(module, "gos_rt_print_bool")?;
    let print_char = intrinsics.extern_fn_by_name(module, "gos_rt_print_char")?;
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
        if let PrintKind::Unsupported(_label) = kind {
            // Soft fallback: print the type's name as a
            // placeholder so the program runs. Programs that
            // need the actual stringification have to write it
            // by hand (or wait for proper Display dispatch).
            let placeholder = intrinsics.intern_string(module, "<value>")?;
            let data_ref = module.declare_data_in_func(placeholder, builder.func);
            let p = builder.ins().global_value(ptr_ty, data_ref);
            let fref = module.declare_func_in_func(print_str, builder.func);
            builder.ins().call(fref, &[p]);
            continue;
        }
        let value = lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?;
        let ty = value_type(value, builder);
        // When Var(_) resolves to StrPtr but the value is a non-pointer int (e.g. `!bool`
        // returning I8), use the correct formatter rather than passing a narrow int as a ptr.
        let kind = if matches!(kind, PrintKind::StrPtr) && ty.is_int() && ty != ptr_ty {
            if ty == types::I8 {
                PrintKind::Bool
            } else {
                PrintKind::Int
            }
        } else {
            kind
        };
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
            PrintKind::Uint => {
                // Zero-extend to 64 bits so we don't sign-extend
                // a sub-i64 unsigned value into a giant negative
                // number. Then route to `gos_rt_print_u64`.
                let n = if ty.bits() < 64 {
                    builder.ins().uextend(types::I64, value)
                } else {
                    value
                };
                let print_u64 = intrinsics.extern_fn_by_name(module, "gos_rt_print_u64")?;
                let fref = module.declare_func_in_func(print_u64, builder.func);
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
            PrintKind::VecI64 => emit_vec_print(
                module,
                builder,
                "gos_rt_vec_format_i64",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::VecF64 => emit_vec_print(
                module,
                builder,
                "gos_rt_vec_format_f64",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::VecBool => emit_vec_print(
                module,
                builder,
                "gos_rt_vec_format_bool",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::VecString => emit_vec_print(
                module,
                builder,
                "gos_rt_vec_format_string",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::VecVecI64 => emit_vec_print(
                module,
                builder,
                "gos_rt_vec_format_vec_i64",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::ArrI64(len) => emit_arr_print(
                module,
                builder,
                "gos_rt_arr_format_i64",
                value,
                len,
                print_str,
                intrinsics,
            )?,
            PrintKind::ArrF64(len) => emit_arr_print(
                module,
                builder,
                "gos_rt_arr_format_f64",
                value,
                len,
                print_str,
                intrinsics,
            )?,
            PrintKind::ArrBool(len) => emit_arr_print(
                module,
                builder,
                "gos_rt_arr_format_bool",
                value,
                len,
                print_str,
                intrinsics,
            )?,
            PrintKind::ArrString(len) => emit_arr_print(
                module,
                builder,
                "gos_rt_arr_format_string",
                value,
                len,
                print_str,
                intrinsics,
            )?,
            PrintKind::JsonValue => emit_vec_print(
                module,
                builder,
                "gos_rt_json_display",
                value,
                print_str,
                intrinsics,
            )?,
            PrintKind::ErrorMessage => {
                let error_msg_fn = intrinsics.extern_fn_by_name(module, "gos_rt_error_message")?;
                let fref = module.declare_func_in_func(error_msg_fn, builder.func);
                let call = builder.ins().call(fref, &[value]);
                let msg = builder.inst_results(call)[0];
                let fref2 = module.declare_func_in_func(print_str, builder.func);
                builder.ins().call(fref2, &[msg]);
            }
            PrintKind::Unsupported(_) => unreachable!("checked above"),
        }
    }
    Ok(())
}

/// Calls a `gos_rt_arr_format_*(ptr, len) -> *mut c_char` runtime
/// helper for a flat fixed-size array buffer and prints the
/// resulting string. Mirrors `emit_vec_print` but threads the
/// compile-time-known length explicitly.
fn emit_arr_print(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    helper_name: &'static str,
    value: ir::Value,
    len: i64,
    print_str: cranelift_module::FuncId,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let f = intrinsics.extern_fn(module, helper_name, &[ptr_ty, types::I64], &[ptr_ty])?;
    let fref = module.declare_func_in_func(f, builder.func);
    let len_v = builder.ins().iconst(types::I64, len);
    let call = builder.ins().call(fref, &[value, len_v]);
    let result = builder.inst_results(call)[0];
    let print_ref = module.declare_func_in_func(print_str, builder.func);
    builder.ins().call(print_ref, &[result]);
    Ok(())
}

fn emit_vec_print(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    helper_name: &'static str,
    value: ir::Value,
    print_str: cranelift_module::FuncId,
    intrinsics: &mut IntrinsicContext,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let f = intrinsics.extern_fn(module, helper_name, &[ptr_ty], &[ptr_ty])?;
    let fref = module.declare_func_in_func(f, builder.func);
    let call = builder.ins().call(fref, &[value]);
    let s = builder.inst_results(call)[0];
    let pref = module.declare_func_in_func(print_str, builder.func);
    builder.ins().call(pref, &[s]);
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
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
    let empty_data = intrinsics.intern_string(module, "")?;
    if args.is_empty() {
        let data_ref = module.declare_data_in_func(empty_data, builder.func);
        return Ok(builder.ins().global_value(ptr_ty, data_ref));
    }

    // Use the runtime's thread-local concat buffer (the same path
    // `__concat` takes for `format!`) instead of chaining N-1
    // `gos_rt_str_concat` calls. Each pairwise concat allocates a
    // throwaway String and then drops the previous accumulator; the
    // batched buffer appends bytes into one growing buffer and
    // hands back a single owned String at the end.
    let init = intrinsics.extern_fn_by_name(module, "gos_rt_concat_init")?;
    let init_ref = module.declare_func_in_func(init, builder.func);
    builder.ins().call(init_ref, &[]);

    let sep_data = if separator.is_empty() {
        None
    } else {
        Some(intrinsics.intern_string(module, separator)?)
    };

    for (idx, arg) in args.iter().enumerate() {
        if idx > 0 {
            if let Some(data) = sep_data {
                let data_ref = module.declare_data_in_func(data, builder.func);
                let sep_ptr = builder.ins().global_value(ptr_ty, data_ref);
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[sep_ptr]);
            }
        }
        let kind = operand_print_kind(body, tcx, arg);
        if let PrintKind::Unsupported(label) = kind {
            bail!(
                "native codegen: cannot stringify a value of {label} type — \
                 the compiled tier has no Display dispatch yet"
            );
        }
        let value = lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?;
        let ty = value_type(value, builder);
        // Same guard as in emit_per_arg_print: Var(_) → StrPtr but value is a narrow int.
        let kind = if matches!(kind, PrintKind::StrPtr) && ty.is_int() && ty != ptr_ty {
            if ty == types::I8 {
                PrintKind::Bool
            } else {
                PrintKind::Int
            }
        } else {
            kind
        };
        match kind {
            PrintKind::StrPtr => {
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[value]);
            }
            PrintKind::Int => {
                let n = if ty.bits() < 64 {
                    builder.ins().sextend(types::I64, value)
                } else {
                    value
                };
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_i64")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[n]);
            }
            PrintKind::Uint => {
                let n = if ty.bits() < 64 {
                    builder.ins().uextend(types::I64, value)
                } else {
                    value
                };
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_u64")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[n]);
            }
            PrintKind::Float => {
                let d = if ty == types::F32 {
                    builder.ins().fpromote(types::F64, value)
                } else {
                    value
                };
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_f64")?;
                let fref = module.declare_func_in_func(f, builder.func);
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
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_bool")?;
                let fref = module.declare_func_in_func(f, builder.func);
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
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_char")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[c]);
            }
            PrintKind::VecI64
            | PrintKind::VecF64
            | PrintKind::VecBool
            | PrintKind::VecString
            | PrintKind::VecVecI64 => {
                let helper = match kind {
                    PrintKind::VecI64 => "gos_rt_vec_format_i64",
                    PrintKind::VecF64 => "gos_rt_vec_format_f64",
                    PrintKind::VecBool => "gos_rt_vec_format_bool",
                    PrintKind::VecString => "gos_rt_vec_format_string",
                    PrintKind::VecVecI64 => "gos_rt_vec_format_vec_i64",
                    _ => unreachable!(),
                };
                let format_fn = intrinsics.extern_fn(module, helper, &[ptr_ty], &[ptr_ty])?;
                let format_ref = module.declare_func_in_func(format_fn, builder.func);
                let call = builder.ins().call(format_ref, &[value]);
                let s = builder.inst_results(call)[0];
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[s]);
            }
            PrintKind::ArrI64(_)
            | PrintKind::ArrF64(_)
            | PrintKind::ArrBool(_)
            | PrintKind::ArrString(_) => {
                let (helper, len) = match kind {
                    PrintKind::ArrI64(n) => ("gos_rt_arr_format_i64", n),
                    PrintKind::ArrF64(n) => ("gos_rt_arr_format_f64", n),
                    PrintKind::ArrBool(n) => ("gos_rt_arr_format_bool", n),
                    PrintKind::ArrString(n) => ("gos_rt_arr_format_string", n),
                    _ => unreachable!(),
                };
                let format_fn =
                    intrinsics.extern_fn(module, helper, &[ptr_ty, types::I64], &[ptr_ty])?;
                let format_ref = module.declare_func_in_func(format_fn, builder.func);
                let len_v = builder.ins().iconst(types::I64, len);
                let call = builder.ins().call(format_ref, &[value, len_v]);
                let s = builder.inst_results(call)[0];
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[s]);
            }
            PrintKind::JsonValue => {
                let render_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_display")?;
                let render_ref = module.declare_func_in_func(render_fn, builder.func);
                let call = builder.ins().call(render_ref, &[value]);
                let s = builder.inst_results(call)[0];
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[s]);
            }
            PrintKind::ErrorMessage => {
                let error_msg_fn = intrinsics.extern_fn_by_name(module, "gos_rt_error_message")?;
                let err_ref = module.declare_func_in_func(error_msg_fn, builder.func);
                let call = builder.ins().call(err_ref, &[value]);
                let s = builder.inst_results(call)[0];
                let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                let fref = module.declare_func_in_func(f, builder.func);
                builder.ins().call(fref, &[s]);
            }
            PrintKind::Unsupported(_) => unreachable!("filtered above"),
        }
    }

    let finish = intrinsics.extern_fn_by_name(module, "gos_rt_concat_finish")?;
    let finish_ref = module.declare_func_in_func(finish, builder.func);
    let call = builder.ins().call(finish_ref, &[]);
    Ok(builder.inst_results(call)[0])
}

/// 0.6.0 deep-free element-kind tags. Mirrors `vec_elem_kind` in
/// `gossamer-runtime/src/c_abi.rs` so the codegen can pass the
/// right discriminator to `gos_rt_vec_new_typed`. Keep these in
/// sync with the runtime constants.
mod vec_elem_kind_codegen {
    pub(super) const PRIMITIVE: i32 = 0;
    pub(super) const STRING: i32 = 1;
    pub(super) const VEC: i32 = 2;
    pub(super) const MAP: i32 = 3;
    #[allow(dead_code, reason = "reserved for errors::Error deep-free wiring")]
    pub(super) const ERROR: i32 = 4;
}

/// Derives the `elem_kind` discriminator for a `Vec<T>` destination
/// local. Inspects the local's MIR type and returns the tag the
/// runtime's deep-free path uses to reclaim element payloads.
///
/// Returns `PRIMITIVE` for unresolved types and non-Vec shapes —
/// the runtime treats PRIMITIVE as shallow-free, which is correct
/// for any element type that owns no further heap memory.
fn vec_elem_kind_from_dest(body: &Body, tcx: &TyCtxt, dest_local: gossamer_mir::Local) -> i32 {
    let ty = body.local_ty(dest_local);
    let inner = match tcx.kind_of(ty) {
        TyKind::Vec(inner) => *inner,
        _ => return vec_elem_kind_codegen::PRIMITIVE,
    };
    match tcx.kind_of(inner) {
        TyKind::String => vec_elem_kind_codegen::STRING,
        TyKind::Vec(_) => vec_elem_kind_codegen::VEC,
        TyKind::HashMap { .. } => vec_elem_kind_codegen::MAP,
        // `errors::Error` is a pointer-bearing opaque type whose
        // payload (message + cause chain) lives on the heap. The
        // runtime's deep-free path drops the outer Box; the inner
        // chain's Drop impl reclaims the rest.
        TyKind::Adt { .. } => {
            // No structural way to tell "this Adt is `errors::Error`"
            // from a TyKind::Adt without DefId comparison. Default
            // to PRIMITIVE — Adts whose payload is reference-only
            // (i.e. every field is a primitive) won't leak, and
            // Adts containing heap fields will leak the inner
            // payload either way (the codegen doesn't currently
            // emit aggregate-typed vec elements). This is an
            // additional safety boundary, not the primary leak fix.
            vec_elem_kind_codegen::PRIMITIVE
        }
        _ => vec_elem_kind_codegen::PRIMITIVE,
    }
}

/// Returns `true` when the operand's MIR type is an unsigned
/// integer (`u8..u128` or `usize`). Used by binop dispatch to pick
/// logical vs arithmetic right-shift. Conservative: returns `false`
/// for projected reads, constants, fn refs, or unresolved types so
/// the signed default applies; the bare-local case covers the
/// overwhelming majority of real shift call sites.
fn operand_is_unsigned_int(body: &Body, tcx: &TyCtxt, op: &Operand) -> bool {
    let Operand::Copy(p) = op else { return false };
    if !p.projection.is_empty() {
        return false;
    }
    matches!(
        tcx.kind_of(body.local_ty(p.local)),
        TyKind::Int(int_ty) if !int_ty.is_signed()
    )
}

/// True when the operand's MIR type / constant value is a `char`
/// — used to detect call sites where the user passed a `char`
/// literal where a `String` was expected.
fn operand_is_char(body: &Body, tcx: &TyCtxt, op: &Operand) -> bool {
    match op {
        Operand::Const(ConstValue::Char(_)) => true,
        Operand::Copy(p) => matches!(tcx.kind_of(body.local_ty(p.local)), TyKind::Char),
        _ => false,
    }
}

/// Returns true when an operand carries a by-value aggregate worth
/// defensively copying at a call boundary. Currently restricted to
/// `Copy(local)` whose root local is a multi-slot aggregate the
/// caller owns; constants and projected reads route through the
/// aggregate-aware paths already.
fn operand_aggregate_slots(body: &Body, tcx: &TyCtxt, op: &Operand) -> Option<u32> {
    match op {
        Operand::Copy(place) if place.projection.is_empty() => {
            let ty = body.local_ty(place.local);
            if matches!(
                tcx.kind_of(ty),
                TyKind::Tuple(_) | TyKind::Adt { .. } | TyKind::Array { .. }
            ) {
                let slots = type_slot_count(tcx, ty);
                if slots > 1 {
                    return Some(slots);
                }
            }
            None
        }
        _ => None,
    }
}

/// Push `ptr` onto the calling thread's raw-pointer tracing-GC
/// shadow stack so the next safepoint-driven collect treats it as
/// a root. Emitted after every `gos_rt_aggr_alloc` /
/// `gos_rt_gc_alloc` call site and after every aggregate-typed
/// `Terminator::Call` return value lands in a destination local.
fn emit_root_push(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    intrinsics: &mut IntrinsicContext,
    ptr: ir::Value,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    let push_id = intrinsics.extern_fn(module, "gos_rt_gc_root_push", &[ptr_ty], &[])?;
    let push_ref = module.declare_func_in_func(push_id, builder.func);
    let coerced = coerce_arg_to(builder, ptr, ptr_ty).unwrap_or(ptr);
    builder.ins().call(push_ref, &[coerced]);
    Ok(())
}

/// Allocates a fresh `slots * 8` heap region via `gos_rt_gc_alloc`
/// and copies `slots` 8-byte words from `src` into it. Returns the
/// new heap pointer. Used by call lowering to defensively copy
/// by-value aggregate args so the callee can mutate its parameter
/// without aliasing the caller's struct.
fn clone_aggregate_value(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    intrinsics: &mut IntrinsicContext,
    src: ir::Value,
    slots: u32,
) -> Result<ir::Value> {
    let ptr_ty = module.target_config().pointer_type();
    let bytes = u64::from(slots) * 8;
    let alloc_fn = intrinsics.extern_fn_by_name(module, "gos_rt_gc_alloc")?;
    let alloc_ref = module.declare_func_in_func(alloc_fn, builder.func);
    let bytes_v = builder.ins().iconst(types::I64, bytes as i64);
    let call = builder.ins().call(alloc_ref, &[bytes_v]);
    let dst = builder.inst_results(call)[0];
    emit_root_push(module, builder, intrinsics, dst)?;
    let src_ptr = match value_type(src, builder) {
        t if t == ptr_ty => src,
        t if t == types::I64 && ptr_ty == types::I32 => builder.ins().ireduce(ptr_ty, src),
        t if t == types::I32 && ptr_ty == types::I64 => builder.ins().uextend(ptr_ty, src),
        _ => src,
    };
    for slot_idx in 0..slots {
        let off = (slot_idx as i32) * 8;
        let word = builder.ins().load(
            types::I64,
            MemFlags::trusted(),
            src_ptr,
            ir::immediates::Offset32::new(off),
        );
        builder.ins().store(
            MemFlags::trusted(),
            word,
            dst,
            ir::immediates::Offset32::new(off),
        );
    }
    Ok(dst)
}

/// Generic-helper signature lookup so the new helpers added in
/// later rounds can be added by name only.
fn name_to_static(name: &str, set: &[&'static str]) -> Option<&'static str> {
    for s in set {
        if *s == name {
            return Some(*s);
        }
    }
    None
}

/// Promotes a runtime helper name from the dispatch table into a
/// `&'static str` (cranelift's intrinsic table keys on
/// `&'static str`). Returns `None` for names not in the
/// generic-helper set.
fn generic_rt_static_name(name: &str) -> Option<&'static str> {
    if let Some(s) = name_to_static(
        name,
        &[
            "gos_rt_http_request_path",
            "gos_rt_http_request_method",
            "gos_rt_http_request_query",
            "gos_rt_http_request_body_str",
            "gos_rt_http_response_text_new",
            "gos_rt_http_response_json_new",
        ],
    ) {
        return Some(s);
    }
    match name {
        "gos_rt_error_new" => Some("gos_rt_error_new"),
        "gos_rt_error_wrap" => Some("gos_rt_error_wrap"),
        "gos_rt_error_message" => Some("gos_rt_error_message"),
        "gos_rt_error_cause" => Some("gos_rt_error_cause"),
        "gos_rt_error_is" => Some("gos_rt_error_is"),
        "gos_rt_regex_compile" => Some("gos_rt_regex_compile"),
        "gos_rt_regex_is_match" => Some("gos_rt_regex_is_match"),
        "gos_rt_regex_find" => Some("gos_rt_regex_find"),
        "gos_rt_regex_find_opt" => Some("gos_rt_regex_find_opt"),
        "gos_rt_regex_captures" => Some("gos_rt_regex_captures"),
        "gos_rt_regex_find_all" => Some("gos_rt_regex_find_all"),
        "gos_rt_regex_captures_all" => Some("gos_rt_regex_captures_all"),
        "gos_rt_regex_replace" => Some("gos_rt_regex_replace"),
        "gos_rt_regex_replace_all" => Some("gos_rt_regex_replace_all"),
        "gos_rt_regex_split" => Some("gos_rt_regex_split"),
        "gos_rt_fs_read_to_string" => Some("gos_rt_fs_read_to_string"),
        "gos_rt_fs_write" => Some("gos_rt_fs_write"),
        "gos_rt_fs_create_dir_all" => Some("gos_rt_fs_create_dir_all"),
        "gos_rt_path_join" => Some("gos_rt_path_join"),
        "gos_rt_flag_set_new" => Some("gos_rt_flag_set_new"),
        "gos_rt_flag_set_string" => Some("gos_rt_flag_set_string"),
        "gos_rt_flag_set_int" => Some("gos_rt_flag_set_int"),
        "gos_rt_flag_set_uint" => Some("gos_rt_flag_set_uint"),
        "gos_rt_flag_set_float" => Some("gos_rt_flag_set_float"),
        "gos_rt_flag_set_bool" => Some("gos_rt_flag_set_bool"),
        "gos_rt_flag_set_duration" => Some("gos_rt_flag_set_duration"),
        "gos_rt_flag_set_string_list" => Some("gos_rt_flag_set_string_list"),
        "gos_rt_flag_set_short" => Some("gos_rt_flag_set_short"),
        "gos_rt_flag_set_usage" => Some("gos_rt_flag_set_usage"),
        "gos_rt_flag_set_parse" => Some("gos_rt_flag_set_parse"),
        "gos_rt_duration_from_secs" => Some("gos_rt_duration_from_secs"),
        "gos_rt_duration_from_millis" => Some("gos_rt_duration_from_millis"),
        "gos_rt_time_format_rfc3339" => Some("gos_rt_time_format_rfc3339"),
        "gos_rt_time_parse_rfc3339" => Some("gos_rt_time_parse_rfc3339"),
        "gos_rt_flag_parse" => Some("gos_rt_flag_parse"),
        "gos_rt_flag_map_get" => Some("gos_rt_flag_map_get"),
        "gos_rt_os_env" => Some("gos_rt_os_env"),
        "gos_rt_os_program_name" => Some("gos_rt_os_program_name"),
        "gos_rt_env_temp_dir" => Some("gos_rt_env_temp_dir"),
        "gos_rt_env_home_dir" => Some("gos_rt_env_home_dir"),
        "gos_rt_os_cwd" => Some("gos_rt_os_cwd"),
        "gos_rt_os_exists" => Some("gos_rt_os_exists"),
        "gos_rt_os_is_file" => Some("gos_rt_os_is_file"),
        "gos_rt_os_is_dir" => Some("gos_rt_os_is_dir"),
        "gos_rt_os_is_symlink" => Some("gos_rt_os_is_symlink"),
        "gos_rt_os_file_size" => Some("gos_rt_os_file_size"),
        "gos_rt_os_remove_file" => Some("gos_rt_os_remove_file"),
        "gos_rt_result_map_bare" => Some("gos_rt_result_map_bare"),
        "gos_rt_result_map_err_bare" => Some("gos_rt_result_map_err_bare"),
        "gos_rt_os_write_file_result" => Some("gos_rt_os_write_file_result"),
        "gos_rt_os_mkdir_all_result" => Some("gos_rt_os_mkdir_all_result"),
        "gos_rt_os_remove_file_result" => Some("gos_rt_os_remove_file_result"),
        "gos_rt_os_remove_dir_all_result" => Some("gos_rt_os_remove_dir_all_result"),
        "gos_rt_http_stream" => Some("gos_rt_http_stream"),
        "gos_rt_http_get" => Some("gos_rt_http_get"),
        "gos_rt_http_stream_next_line" => Some("gos_rt_http_stream_next_line"),
        "gos_rt_fs_list_dir" => Some("gos_rt_fs_list_dir"),
        "gos_rt_fs_walk_dir" => Some("gos_rt_fs_walk_dir"),
        "gos_rt_exec_run" => Some("gos_rt_exec_run"),
        "gos_rt_exec_spawn" => Some("gos_rt_exec_spawn"),
        "gos_rt_exec_kill" => Some("gos_rt_exec_kill"),
        "gos_rt_signal_on" => Some("gos_rt_signal_on"),
        "gos_rt_signal_wait" => Some("gos_rt_signal_wait"),
        "gos_rt_signal_try_wait" => Some("gos_rt_signal_try_wait"),
        "gos_rt_os_set_env" => Some("gos_rt_os_set_env"),
        "gos_rt_os_unset_env" => Some("gos_rt_os_unset_env"),
        "gos_rt_bufio_scanner_new" => Some("gos_rt_bufio_scanner_new"),
        "gos_rt_bufio_scanner_scan" => Some("gos_rt_bufio_scanner_scan"),
        "gos_rt_bufio_scanner_text" => Some("gos_rt_bufio_scanner_text"),
        "gos_rt_http_client_new" => Some("gos_rt_http_client_new"),
        "gos_rt_http_client_get" => Some("gos_rt_http_client_get"),
        "gos_rt_http_client_post" => Some("gos_rt_http_client_post"),
        "gos_rt_http_request_header" => Some("gos_rt_http_request_header"),
        "gos_rt_http_request_body" => Some("gos_rt_http_request_body"),
        "gos_rt_http_request_send" => Some("gos_rt_http_request_send"),
        "gos_rt_http_response_status" => Some("gos_rt_http_response_status"),
        "gos_rt_http_response_body" => Some("gos_rt_http_response_body"),
        "gos_rt_vec_get_i64" => Some("gos_rt_vec_get_i64"),
        "gos_rt_vec_set_i64" => Some("gos_rt_vec_set_i64"),
        "gos_rt_vec_format_i64" => Some("gos_rt_vec_format_i64"),
        "gos_rt_chan_recv_option" => Some("gos_rt_chan_recv_option"),
        "gos_rt_chan_try_recv_option" => Some("gos_rt_chan_try_recv_option"),
        "gos_rt_result_new" => Some("gos_rt_result_new"),
        "gos_rt_result_disc" => Some("gos_rt_result_disc"),
        "gos_rt_result_payload" => Some("gos_rt_result_payload"),
        "gos_rt_result_unwrap" => Some("gos_rt_result_unwrap"),
        "gos_rt_result_unwrap_or" => Some("gos_rt_result_unwrap_or"),
        "gos_rt_result_ok" => Some("gos_rt_result_ok"),
        "gos_rt_result_err" => Some("gos_rt_result_err"),
        "gos_rt_result_ok_or" => Some("gos_rt_result_ok_or"),
        "gos_rt_result_is_ok" => Some("gos_rt_result_is_ok"),
        "gos_rt_result_is_err" => Some("gos_rt_result_is_err"),
        "gos_rt_set_new" => Some("gos_rt_set_new"),
        "gos_rt_set_insert" => Some("gos_rt_set_insert"),
        "gos_rt_set_contains" => Some("gos_rt_set_contains"),
        "gos_rt_set_remove" => Some("gos_rt_set_remove"),
        "gos_rt_set_len" => Some("gos_rt_set_len"),
        "gos_rt_btmap_new" => Some("gos_rt_btmap_new"),
        "gos_rt_btmap_insert" => Some("gos_rt_btmap_insert"),
        "gos_rt_btmap_get_or" => Some("gos_rt_btmap_get_or"),
        "gos_rt_btmap_len" => Some("gos_rt_btmap_len"),
        "gos_rt_btmap_keys" => Some("gos_rt_btmap_keys"),
        "gos_rt_str_as_bytes" => Some("gos_rt_str_as_bytes"),
        "gos_rt_vec_clone" => Some("gos_rt_vec_clone"),
        "gos_rt_map_inc_str_i64" => Some("gos_rt_map_inc_str_i64"),
        "gos_rt_map_or_insert_str_i64" => Some("gos_rt_map_or_insert_str_i64"),
        "gos_rt_map_or_insert_i64_i64" => Some("gos_rt_map_or_insert_i64_i64"),
        "gos_rt_errors_join_vec" => Some("gos_rt_errors_join_vec"),
        "gos_rt_errors_join" => Some("gos_rt_errors_join"),
        "gos_rt_json_value_object_n" => Some("gos_rt_json_value_object_n"),
        "gos_rt_http_response_set_header" => Some("gos_rt_http_response_set_header"),
        "gos_rt_http_response_get_header" => Some("gos_rt_http_response_get_header"),
        "gos_rt_http_request_set_header" => Some("gos_rt_http_request_set_header"),
        "gos_rt_http_request_get_header" => Some("gos_rt_http_request_get_header"),
        "gos_rt_gzip_encode" => Some("gos_rt_gzip_encode"),
        "gos_rt_gzip_decode" => Some("gos_rt_gzip_decode"),
        "gos_rt_chunked_encode" => Some("gos_rt_chunked_encode"),
        "gos_rt_chunked_decode" => Some("gos_rt_chunked_decode"),
        "gos_rt_sse_encode_event" => Some("gos_rt_sse_encode_event"),
        "gos_rt_sse_encode_comment" => Some("gos_rt_sse_encode_comment"),
        "gos_rt_sse_encode_retry" => Some("gos_rt_sse_encode_retry"),
        "gos_rt_mw_new_request_id" => Some("gos_rt_mw_new_request_id"),
        "gos_rt_mw_accepts_gzip" => Some("gos_rt_mw_accepts_gzip"),
        "gos_rt_ws_accept_key" => Some("gos_rt_ws_accept_key"),
        "gos_rt_static_mime_for_path" => Some("gos_rt_static_mime_for_path"),
        "gos_rt_router_new" => Some("gos_rt_router_new"),
        "gos_rt_router_add" => Some("gos_rt_router_add"),
        "gos_rt_router_get" => Some("gos_rt_router_get"),
        "gos_rt_router_post" => Some("gos_rt_router_post"),
        "gos_rt_router_put" => Some("gos_rt_router_put"),
        "gos_rt_router_delete" => Some("gos_rt_router_delete"),
        "gos_rt_router_patch" => Some("gos_rt_router_patch"),
        "gos_rt_router_head" => Some("gos_rt_router_head"),
        "gos_rt_router_options" => Some("gos_rt_router_options"),
        "gos_rt_router_add_fn" => Some("gos_rt_router_add_fn"),
        "gos_rt_router_get_fn" => Some("gos_rt_router_get_fn"),
        "gos_rt_router_post_fn" => Some("gos_rt_router_post_fn"),
        "gos_rt_router_put_fn" => Some("gos_rt_router_put_fn"),
        "gos_rt_router_delete_fn" => Some("gos_rt_router_delete_fn"),
        "gos_rt_router_patch_fn" => Some("gos_rt_router_patch_fn"),
        "gos_rt_router_head_fn" => Some("gos_rt_router_head_fn"),
        "gos_rt_router_options_fn" => Some("gos_rt_router_options_fn"),
        "gos_rt_router_serve" => Some("gos_rt_router_serve"),
        "gos_rt_file_server_new" => Some("gos_rt_file_server_new"),
        "gos_rt_file_server_serve" => Some("gos_rt_file_server_serve"),
        "gos_rt_native_client_new" => Some("gos_rt_native_client_new"),
        "gos_rt_native_client_get" => Some("gos_rt_native_client_get"),
        "gos_rt_proxy_new" => Some("gos_rt_proxy_new"),
        "gos_rt_proxy_forward" => Some("gos_rt_proxy_forward"),
        "gos_rt_ws_frame_text" => Some("gos_rt_ws_frame_text"),
        "gos_rt_slog_info" => Some("gos_rt_slog_info"),
        "gos_rt_slog_warn" => Some("gos_rt_slog_warn"),
        "gos_rt_slog_error" => Some("gos_rt_slog_error"),
        "gos_rt_slog_debug" => Some("gos_rt_slog_debug"),
        "gos_rt_testing_check" => Some("gos_rt_testing_check"),
        "gos_rt_testing_check_eq_i64" => Some("gos_rt_testing_check_eq_i64"),
        "gos_rt_parse_i64_result" => Some("gos_rt_parse_i64_result"),
        "gos_rt_result_map_err" => Some("gos_rt_result_map_err"),
        "gos_rt_result_map" => Some("gos_rt_result_map"),
        "gos_rt_flag_cell_load_str" => Some("gos_rt_flag_cell_load_str"),
        "gos_rt_flag_cell_load_i64" => Some("gos_rt_flag_cell_load_i64"),
        "gos_rt_flag_cell_load_bool" => Some("gos_rt_flag_cell_load_bool"),
        "gos_rt_flag_cell_load_f64" => Some("gos_rt_flag_cell_load_f64"),
        "gos_rt_flag_cell_load_vec" => Some("gos_rt_flag_cell_load_vec"),
        // Plain ascending sort for Vec<i64>.
        "gos_rt_vec_sort_i64" => Some("gos_rt_vec_sort_i64"),
        // Sort-by callbacks for fixed-array / Vec receivers.
        "gos_rt_arr_sort_by_i64" => Some("gos_rt_arr_sort_by_i64"),
        "gos_rt_vec_sort_by_i64" => Some("gos_rt_vec_sort_by_i64"),
        // Stride-aware sort_by for multi-slot aggregate elements
        // (Tuple / struct). The comparator receives element
        // pointers; the cranelift ABI passes aggregates that way
        // already so the user closure body works unchanged.
        "gos_rt_arr_sort_by_aggr" => Some("gos_rt_arr_sort_by_aggr"),
        "gos_rt_vec_sort_by_aggr" => Some("gos_rt_vec_sort_by_aggr"),
        "gos_rt_json_set" => Some("gos_rt_json_set"),
        "gos_rt_arr_iter" => Some("gos_rt_arr_iter"),
        "gos_rt_arr_iter_next" => Some("gos_rt_arr_iter_next"),
        _ => None,
    }
}

/// Generic wrapper for the round-3 stdlib helpers. Each helper has
/// a fixed signature derived from its name; declaring the extern
/// once per call site is fine because cranelift dedups by symbol.
#[allow(
    clippy::too_many_arguments,
    clippy::too_many_lines,
    reason = "intrinsic dispatch / arg-marshal sequence — flat-table shape preserved for grep-ability"
)]
fn lower_generic_rt_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    destination: &gossamer_mir::Place,
    name: &'static str,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();
    // Signature table: arg cl-types + return cl-type. `None`
    // return means void.
    let (params, ret): (&[ir::Type], Option<ir::Type>) = match name {
        "gos_rt_error_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_wrap" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_error_message" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_cause" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_error_is" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_regex_compile" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_is_match" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_regex_find" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_find_opt" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_captures" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_find_all" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_captures_all" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_replace" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_replace_all" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_regex_split" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_read_to_string" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_write" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_fs_create_dir_all" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_path_join" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_string" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_int" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_uint" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_float" => (&[ptr_ty, ptr_ty, types::F64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_bool" => (&[ptr_ty, ptr_ty, types::I8, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_duration" => (&[ptr_ty, ptr_ty, types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_string_list" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_short" => (&[ptr_ty, types::I64], None),
        "gos_rt_flag_set_usage" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_set_parse" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_duration_from_secs" => (&[types::I64], Some(types::I64)),
        "gos_rt_duration_from_millis" => (&[types::I64], Some(types::I64)),
        "gos_rt_time_format_rfc3339" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_time_parse_rfc3339" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_parse" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_map_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_env" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_program_name" => (&[], Some(ptr_ty)),
        "gos_rt_env_temp_dir" => (&[], Some(ptr_ty)),
        "gos_rt_env_home_dir" => (&[], Some(ptr_ty)),
        "gos_rt_os_cwd" => (&[], Some(ptr_ty)),
        "gos_rt_fs_list_dir" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_fs_walk_dir" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_run" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_spawn" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_exec_kill" => (&[types::I64], Some(types::I64)),
        "gos_rt_signal_on" => (&[types::I32], Some(types::I64)),
        "gos_rt_signal_wait" => (&[types::I64], None),
        "gos_rt_signal_try_wait" => (&[types::I64], Some(types::I32)),
        "gos_rt_os_set_env" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_unset_env" => (&[ptr_ty], None),
        "gos_rt_os_exists" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_file" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_dir" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_is_symlink" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_file_size" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_os_remove_file" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_map_bare" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_result_map_err_bare" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_os_write_file_result" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_os_mkdir_all_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_remove_file_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_os_remove_dir_all_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_stream" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_stream_next_line" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bufio_scanner_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_bufio_scanner_scan" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_bufio_scanner_text" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_new" => (&[], Some(ptr_ty)),
        "gos_rt_http_client_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_client_post" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_header" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_body" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_send" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_status" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_http_response_body" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_get_i64" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_vec_set_i64" => (&[ptr_ty, types::I64, types::I64], None),
        "gos_rt_vec_format_i64" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_chan_recv_option" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_chan_try_recv_option" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_result_new" => (&[types::I64, types::I64], Some(ptr_ty)),
        "gos_rt_result_disc" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_payload" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_unwrap" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_unwrap_or" => (&[ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_result_ok" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_err" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_ok_or" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_result_is_ok" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_result_is_err" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_set_new" => (&[], Some(ptr_ty)),
        "gos_rt_set_insert" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_contains" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_remove" => (&[ptr_ty, ptr_ty], Some(types::I64)),
        "gos_rt_set_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_btmap_new" => (&[], Some(ptr_ty)),
        "gos_rt_btmap_insert" => (&[ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_btmap_get_or" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_btmap_len" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_btmap_keys" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_str_as_bytes" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_clone" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_map_inc_str_i64" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_map_or_insert_str_i64" => (&[ptr_ty, ptr_ty, types::I64], Some(types::I64)),
        "gos_rt_map_or_insert_i64_i64" => (&[ptr_ty, types::I64, types::I64], Some(types::I64)),
        "gos_rt_errors_join_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_errors_join" => (&[ptr_ty, types::I64], Some(ptr_ty)),
        "gos_rt_json_value_object_n" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_json_value_float" => (&[types::F64], Some(ptr_ty)),
        "gos_rt_http_response_set_header" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_http_response_get_header" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_set_header" => (&[ptr_ty, ptr_ty, ptr_ty], None),
        "gos_rt_http_request_get_header" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_path" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_method" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_query" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_request_body_str" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_text_new" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_http_response_json_new" => (&[types::I64, ptr_ty], Some(ptr_ty)),
        "gos_rt_gzip_encode" | "gos_rt_gzip_decode" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_chunked_encode" | "gos_rt_chunked_decode" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_event" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_comment" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_sse_encode_retry" => (&[types::I64], Some(ptr_ty)),
        "gos_rt_mw_new_request_id" => (&[], Some(ptr_ty)),
        "gos_rt_mw_accepts_gzip" => (&[ptr_ty], Some(types::I32)),
        "gos_rt_ws_accept_key" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_static_mime_for_path" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_router_new" => (&[], Some(ptr_ty)),
        "gos_rt_router_add" => (&[ptr_ty, ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_get"
        | "gos_rt_router_post"
        | "gos_rt_router_put"
        | "gos_rt_router_delete"
        | "gos_rt_router_patch"
        | "gos_rt_router_head"
        | "gos_rt_router_options" => (&[ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_add_fn" => (&[ptr_ty, ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_get_fn"
        | "gos_rt_router_post_fn"
        | "gos_rt_router_put_fn"
        | "gos_rt_router_delete_fn"
        | "gos_rt_router_patch_fn"
        | "gos_rt_router_head_fn"
        | "gos_rt_router_options_fn" => (&[ptr_ty, ptr_ty, types::I64], None),
        "gos_rt_router_serve" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_file_server_new" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_file_server_serve" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_native_client_new" => (&[], Some(ptr_ty)),
        "gos_rt_native_client_get" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_proxy_new" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_proxy_forward" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_ws_frame_text" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_slog_info" | "gos_rt_slog_warn" | "gos_rt_slog_error" | "gos_rt_slog_debug" => {
            (&[ptr_ty], None)
        }
        "gos_rt_testing_check" => (&[types::I8, ptr_ty], Some(types::I8)),
        "gos_rt_testing_check_eq_i64" => (&[types::I64, types::I64, ptr_ty], Some(types::I8)),
        "gos_rt_parse_i64_result" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_result_map_err" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_result_map" => (&[ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_cell_load_str" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_flag_cell_load_i64" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_flag_cell_load_bool" => (&[ptr_ty], Some(types::I64)),
        "gos_rt_flag_cell_load_f64" => (&[ptr_ty], Some(types::F64)),
        "gos_rt_flag_cell_load_vec" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_vec_sort_i64" => (&[ptr_ty], None),
        "gos_rt_arr_sort_by_i64" => (&[ptr_ty, types::I64, ptr_ty], None),
        "gos_rt_vec_sort_by_i64" => (&[ptr_ty, ptr_ty], None),
        // Aggregate-stride variants. Vec form reads `elem_bytes`
        // from the GosVec header so it has no extra arg; array
        // form takes `(buf, len, elem_bytes, env)`.
        "gos_rt_arr_sort_by_aggr" => (&[ptr_ty, types::I64, types::I64, ptr_ty], None),
        "gos_rt_vec_sort_by_aggr" => (&[ptr_ty, ptr_ty], None),
        "gos_rt_json_set" => (&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty)),
        "gos_rt_arr_iter" => (&[ptr_ty], Some(ptr_ty)),
        "gos_rt_arr_iter_next" => (&[ptr_ty], Some(ptr_ty)),
        _ => unreachable!("unhandled rt name {name}"),
    };
    let returns = ret.map(|t| vec![t]).unwrap_or_default();
    let func_id = intrinsics.extern_fn(module, name, params, &returns)?;
    let fref = module.declare_func_in_func(func_id, builder.func);
    let mut arg_values = Vec::with_capacity(params.len());
    for (i, param_ty) in params.iter().enumerate() {
        let v = match args.get(i) {
            Some(a) => {
                let hint = if *param_ty == ptr_ty {
                    Some(ptr_ty)
                } else {
                    None
                };
                lower_operand(module, builder, locals, body, tcx, a, hint, intrinsics)?
            }
            None => {
                if param_ty.is_int() {
                    builder.ins().iconst(*param_ty, 0)
                } else {
                    builder.ins().iconst(ptr_ty, 0)
                }
            }
        };
        let coerced = coerce_arg_to(builder, v, *param_ty)?;
        arg_values.push(coerced);
    }
    let call = builder.ins().call(fref, &arg_values);
    if let Some(_ret_ty) = ret {
        let v = builder.inst_results(call)[0];
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            v,
        );
    } else {
        let zero = builder.ins().iconst(types::I64, 0);
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            zero,
        );
    }
    Ok(())
}

/// Lowers a call into a Rust binding declared under
/// `[rust-bindings]`. The MIR side picks the mangled symbol
/// (`gos_binding_<sym>__<name>`) emitted by the
/// `register_module!` macro; we derive the call's C-ABI
/// signature from the MIR types of the args + destination.
///
/// Type mapping mirrors `gossamer_binding::native::BindingAbi`:
///
/// | MIR type        | C-ABI shape         | cranelift type |
/// |-----------------|---------------------|----------------|
/// | `bool`          | `bool` (1-byte)     | `I8`           |
/// | `i64` / signed  | `int64_t`           | `I64`          |
/// | `f64`           | `double`            | `F64`          |
/// | `char`          | `uint32_t`          | `I32`          |
/// | `String`        | `*const c_char`     | `ptr_ty`       |
/// | `Vec<T>`        | `*mut GosVec`       | `ptr_ty`       |
/// | other (handle / | ptr-sized opaque    | `ptr_ty`       |
/// | option / result)|                     |                |
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
fn lower_external_binding_call(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    args: &[Operand],
    intrinsics: &mut IntrinsicContext,
    destination: &gossamer_mir::Place,
    name: &str,
) -> Result<()> {
    let ptr_ty = module.target_config().pointer_type();

    let dest_ty = body.local_ty(destination.local);
    let dest_cl_ty = mir_ty_to_cabi(tcx, dest_ty, ptr_ty);

    let mut params: Vec<ir::Type> = Vec::with_capacity(args.len());
    for arg in args {
        let ty = operand_cabi_ty(arg, body, tcx, ptr_ty);
        params.push(ty);
    }

    let returns: Vec<ir::Type> = match dest_cl_ty {
        Some(t) => vec![t],
        None => Vec::new(),
    };
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let extern_fn = intrinsics.extern_fn(module, static_name, &params, &returns)?;
    let fref = module.declare_func_in_func(extern_fn, builder.func);

    let mut arg_values: Vec<ir::Value> = Vec::with_capacity(args.len());
    for (arg, &param_ty) in args.iter().zip(params.iter()) {
        let v = lower_operand(
            module,
            builder,
            locals,
            body,
            tcx,
            arg,
            Some(param_ty),
            intrinsics,
        )?;
        let coerced = coerce_arg_to(builder, v, param_ty)?;
        arg_values.push(coerced);
    }

    let call = builder.ins().call(fref, &arg_values);
    if dest_cl_ty.is_some() {
        let v = builder.inst_results(call)[0];
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            v,
        );
    } else {
        let zero = builder.ins().iconst(types::I64, 0);
        define_var_to(
            builder,
            locals,
            &intrinsics.body_cl_types,
            destination.local,
            zero,
        );
    }
    Ok(())
}

fn operand_cabi_ty(operand: &Operand, body: &Body, tcx: &TyCtxt, ptr_ty: ir::Type) -> ir::Type {
    match operand {
        Operand::Copy(place) => {
            mir_ty_to_cabi(tcx, body.local_ty(place.local), ptr_ty).unwrap_or(types::I64)
        }
        Operand::Const(value) => match value {
            ConstValue::Bool(_) => types::I8,
            ConstValue::Float(_) => types::F64,
            ConstValue::Char(_) => types::I32,
            ConstValue::Str(_) => ptr_ty,
            _ => types::I64,
        },
        Operand::FnRef { .. } => ptr_ty,
    }
}

fn mir_ty_to_cabi(tcx: &TyCtxt, ty: gossamer_types::Ty, ptr_ty: ir::Type) -> Option<ir::Type> {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Unit => None,
        TyKind::Tuple(parts) if parts.is_empty() => None,
        TyKind::Bool => Some(types::I8),
        TyKind::Char => Some(types::I32),
        TyKind::Float(_) => Some(types::F64),
        TyKind::Int(_) => Some(types::I64),
        TyKind::String => Some(ptr_ty),
        TyKind::Vec(_) => Some(ptr_ty),
        // Option / Result / Adt / Tuple / FnDef / handles all flow
        // through as pointer-sized values at the C-ABI boundary.
        _ => Some(ptr_ty),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
        module,
        builder,
        locals,
        body,
        tcx,
        args,
        name,
        destination,
        intrinsics,
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
    reason = "intrinsic dispatch table — splitting it hides the one-arm-per-symbol structure"
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
            // Build the concatenated string into the runtime's
            // thread-local concat buffer, then return a fresh
            // String pointer. Lets `format!` produce a real value
            // that callers (errors::new, struct fields) can
            // consume past the surrounding `println`/`print`.
            if !destination.projection.is_empty() {
                bail!("native codegen: __concat destination cannot have projections");
            }
            let init = intrinsics.extern_fn_by_name(module, "gos_rt_concat_init")?;
            let init_ref = module.declare_func_in_func(init, builder.func);
            builder.ins().call(init_ref, &[]);
            for arg in args {
                let kind = operand_print_kind(body, tcx, arg);
                let value =
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?;
                let ty = value_type(value, builder);
                // Var(_) → StrPtr fallback, but value is a narrow int (e.g. `!bool` → I8).
                let kind = if matches!(kind, PrintKind::StrPtr) && ty.is_int() && ty != ptr_ty {
                    if ty == types::I8 {
                        PrintKind::Bool
                    } else {
                        PrintKind::Int
                    }
                } else {
                    kind
                };
                match kind {
                    PrintKind::StrPtr => {
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[value]);
                    }
                    PrintKind::Int => {
                        let n = if ty.bits() < 64 {
                            builder.ins().sextend(types::I64, value)
                        } else {
                            value
                        };
                        let f = intrinsics.extern_fn(
                            module,
                            "gos_rt_concat_i64",
                            &[types::I64],
                            &[],
                        )?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[n]);
                    }
                    PrintKind::Uint => {
                        // Zero-extend so values >= 2^63 don't get
                        // sign-flipped on the way to the i64 helper.
                        let n = if ty.bits() < 64 {
                            builder.ins().uextend(types::I64, value)
                        } else {
                            value
                        };
                        let f = intrinsics.extern_fn(
                            module,
                            "gos_rt_concat_u64",
                            &[types::I64],
                            &[],
                        )?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[n]);
                    }
                    PrintKind::Float => {
                        let d = if ty == types::F32 {
                            builder.ins().fpromote(types::F64, value)
                        } else {
                            value
                        };
                        let f = intrinsics.extern_fn(
                            module,
                            "gos_rt_concat_f64",
                            &[types::F64],
                            &[],
                        )?;
                        let fref = module.declare_func_in_func(f, builder.func);
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
                        let f = intrinsics.extern_fn(
                            module,
                            "gos_rt_concat_bool",
                            &[types::I32],
                            &[],
                        )?;
                        let fref = module.declare_func_in_func(f, builder.func);
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
                        let f = intrinsics.extern_fn(
                            module,
                            "gos_rt_concat_char",
                            &[types::I32],
                            &[],
                        )?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[c]);
                    }
                    PrintKind::VecI64
                    | PrintKind::VecF64
                    | PrintKind::VecBool
                    | PrintKind::VecString
                    | PrintKind::VecVecI64 => {
                        let helper = match kind {
                            PrintKind::VecI64 => "gos_rt_vec_format_i64",
                            PrintKind::VecF64 => "gos_rt_vec_format_f64",
                            PrintKind::VecBool => "gos_rt_vec_format_bool",
                            PrintKind::VecString => "gos_rt_vec_format_string",
                            PrintKind::VecVecI64 => "gos_rt_vec_format_vec_i64",
                            _ => unreachable!(),
                        };
                        let format_fn =
                            intrinsics.extern_fn(module, helper, &[ptr_ty], &[ptr_ty])?;
                        let format_ref = module.declare_func_in_func(format_fn, builder.func);
                        let call = builder.ins().call(format_ref, &[value]);
                        let s = builder.inst_results(call)[0];
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[s]);
                    }
                    PrintKind::ArrI64(_)
                    | PrintKind::ArrF64(_)
                    | PrintKind::ArrBool(_)
                    | PrintKind::ArrString(_) => {
                        let (helper, len) = match kind {
                            PrintKind::ArrI64(n) => ("gos_rt_arr_format_i64", n),
                            PrintKind::ArrF64(n) => ("gos_rt_arr_format_f64", n),
                            PrintKind::ArrBool(n) => ("gos_rt_arr_format_bool", n),
                            PrintKind::ArrString(n) => ("gos_rt_arr_format_string", n),
                            _ => unreachable!(),
                        };
                        let format_fn = intrinsics.extern_fn(
                            module,
                            helper,
                            &[ptr_ty, types::I64],
                            &[ptr_ty],
                        )?;
                        let format_ref = module.declare_func_in_func(format_fn, builder.func);
                        let len_v = builder.ins().iconst(types::I64, len);
                        let call = builder.ins().call(format_ref, &[value, len_v]);
                        let s = builder.inst_results(call)[0];
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[s]);
                    }
                    PrintKind::JsonValue => {
                        let render_fn = intrinsics.extern_fn(
                            module,
                            "gos_rt_json_display",
                            &[ptr_ty],
                            &[ptr_ty],
                        )?;
                        let render_ref = module.declare_func_in_func(render_fn, builder.func);
                        let call = builder.ins().call(render_ref, &[value]);
                        let s = builder.inst_results(call)[0];
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[s]);
                    }
                    PrintKind::ErrorMessage => {
                        let error_msg_fn = intrinsics.extern_fn(
                            module,
                            "gos_rt_error_message",
                            &[ptr_ty],
                            &[ptr_ty],
                        )?;
                        let err_ref = module.declare_func_in_func(error_msg_fn, builder.func);
                        let call = builder.ins().call(err_ref, &[value]);
                        let s = builder.inst_results(call)[0];
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[s]);
                    }
                    PrintKind::Unsupported(_) => {
                        let placeholder = intrinsics.intern_string(module, "<value>")?;
                        let data_ref = module.declare_data_in_func(placeholder, builder.func);
                        let p = builder.ins().global_value(ptr_ty, data_ref);
                        let f = intrinsics.extern_fn_by_name(module, "gos_rt_concat_str")?;
                        let fref = module.declare_func_in_func(f, builder.func);
                        builder.ins().call(fref, &[p]);
                    }
                }
            }
            let finish = intrinsics.extern_fn_by_name(module, "gos_rt_concat_finish")?;
            let finish_ref = module.declare_func_in_func(finish, builder.func);
            let call = builder.ins().call(finish_ref, &[]);
            let result = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                result,
            );
            Ok(true)
        }
        // `__fmt_prec(value, prec)` — emitted by macro expansion for
        // `{:.N}` specs. Routes through `gos_rt_f64_prec_to_str` so
        // the result is a String the surrounding `__concat` consumes.
        "__fmt_prec" => {
            if args.len() != 2 {
                bail!("native codegen: __fmt_prec expects exactly two arguments");
            }
            let value_raw = lower_operand(
                module, builder, locals, body, tcx, &args[0], None, intrinsics,
            )?;
            let value_ty = value_type(value_raw, builder);
            let value = if value_ty == types::F64 {
                value_raw
            } else if value_ty == types::F32 {
                builder.ins().fpromote(types::F64, value_raw)
            } else {
                builder.ins().fcvt_from_sint(types::F64, value_raw)
            };
            let prec_raw = lower_operand(
                module, builder, locals, body, tcx, &args[1], None, intrinsics,
            )?;
            let prec_ty = value_type(prec_raw, builder);
            let prec = if prec_ty.bits() < 64 {
                builder.ins().sextend(types::I64, prec_raw)
            } else if prec_ty.bits() > 64 {
                builder.ins().ireduce(types::I64, prec_raw)
            } else {
                prec_raw
            };
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_f64_prec_to_str",
                &[types::F64, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[value, prec]);
            let result = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                result,
            );
            Ok(true)
        }
        // `io::stdout()` / `io::stderr()` / `io::stdin()` —
        // return an opaque pointer to a static `GosStream`.
        // Method dispatch on the returned value routes to the
        // `gos_rt_stream_*` helpers below.
        "io::stdout" | "io::stderr" | "io::stdin" | "os::stdout" | "os::stderr" | "os::stdin" => {
            let rt_name = match name {
                "io::stdout" | "os::stdout" => "gos_rt_io_stdout",
                "io::stderr" | "os::stderr" => "gos_rt_io_stderr",
                "io::stdin" | "os::stdin" => "gos_rt_io_stdin",
                _ => unreachable!(),
            };
            let rt_fn = intrinsics.extern_fn(module, rt_name, &[], &[ptr_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let b64 = coerce_arg_to(builder, b, types::I64)?;
            let _ = builder.ins().call(fref, &[stream, b64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let arr = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let len = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let _ = builder.ins().call(fref, &[stream, arr, len64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_stream_write_str" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_stream_write_str")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[stream, s]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_stream_flush" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_stream_flush")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[stream]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_stream_read_line" | "gos_rt_stream_read_to_string" => {
            let rt_name: &'static str = match name {
                "gos_rt_stream_read_line" => "gos_rt_stream_read_line",
                _ => "gos_rt_stream_read_to_string",
            };
            let rt_fn = intrinsics.extern_fn(module, rt_name, &[ptr_ty], &[ptr_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let stream = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[stream]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "println" | "print" => {
            // Per-arg dispatch: each operand is printed through
            // the runtime helper matching its MIR type
            // (`gos_rt_print_str` for strings, `_i64` for
            // integers, `_f64` for floats, `_bool` / `_char`).
            // This is the same machinery `__concat` uses; bare
            // `println(5i64)` and interpolated `println!("{n}")`
            // therefore share one code path.
            //
            // The whole sequence runs under the process-global
            // stdout lock so concurrent goroutines on other OS
            // threads can't interleave bytes mid-line. The lock
            // is reentrant — each per-arg helper takes it again
            // — so this outer acquire merely extends the held
            // duration to cover the entire multi-call sequence.
            let acquire_fn = intrinsics.extern_fn_by_name(module, "gos_rt_stdout_acquire")?;
            let release_fn = intrinsics.extern_fn_by_name(module, "gos_rt_stdout_release")?;
            let acquire_ref = module.declare_func_in_func(acquire_fn, builder.func);
            let release_ref = module.declare_func_in_func(release_fn, builder.func);
            let _ = builder.ins().call(acquire_ref, &[]);
            emit_per_arg_print(module, builder, locals, body, tcx, args, intrinsics, " ")?;
            if name == "println" {
                let println_fn = intrinsics.extern_fn_by_name(module, "gos_rt_println")?;
                let pl_ref = module.declare_func_in_func(println_fn, builder.func);
                let _ = builder.ins().call(pl_ref, &[]);
            }
            let _ = builder.ins().call(release_ref, &[]);
            if !destination.projection.is_empty() {
                bail!("native codegen: intrinsic destination cannot have projections");
            }
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                zero,
            );
            Ok(true)
        }
        "eprintln" | "eprint" => {
            // Build the formatted message via the same per-arg
            // concat machinery `panic` uses, then drain it through
            // the stderr writer (which flushes stdout first so
            // diagnostic order is preserved). Keeps eprint output
            // off stdout without parallel `_err` versions of every
            // per-type print helper.
            let s = emit_args_to_concat_string(
                module, builder, locals, body, tcx, args, intrinsics, " ",
            )?;
            let eprint_fn = intrinsics.extern_fn_by_name(module, "gos_rt_eprint_str")?;
            let eprint_ref = module.declare_func_in_func(eprint_fn, builder.func);
            builder.ins().call(eprint_ref, &[s]);
            if name == "eprintln" {
                let nl_fn = intrinsics.extern_fn_by_name(module, "gos_rt_eprintln")?;
                let nl_ref = module.declare_func_in_func(nl_fn, builder.func);
                let _ = builder.ins().call(nl_ref, &[]);
            }
            if !destination.projection.is_empty() {
                bail!("native codegen: intrinsic destination cannot have projections");
            }
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                zero,
            );
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
            // Names starting with `gos_rt_` are runtime extern
            // symbols (the Fn-trait coercion trampolines plus a
            // handful of other one-off helpers MIR may stash into
            // a heap env). Declare them through the module's
            // intrinsic-fn machinery so the linker resolves them
            // against `gossamer-runtime`.
            let func_id = if let Some(id) = intrinsics.functions.get(name).copied() {
                id
            } else if let Some(id) = intrinsics.externs.get(name.as_str()).copied() {
                // Runtime extern symbol — `gos_rt_router_serve` and
                // the other stateful-type serve dispatchers are
                // declared via `extern_fn_by_name` at codegen init
                // (loop over `gossamer_abi::REGISTRY`) and live in
                // `intrinsics.externs`. Surface them here so
                // `gos_fn_addr` can hand back their address for
                // handler-fn-ptr indirection through
                // `gos_rt_http_serve` etc.
                id
            } else if name.starts_with("__fn_thunk_") {
                // Per-shape callable thunk. The name encodes the
                // typed FnTrait sig (`__fn_thunk_<inputs>_<ret>`);
                // synthesise a real function in this module that
                // takes (env, typed_args...) -> typed_ret and
                // forwards to the real fn at env+8 with the right
                // calling convention. Replaces the earlier
                // mono-i64 `gos_rt_fn_tramp_N` family which
                // silently mangled f64 / bool / aggregate args.
                define_shape_thunk(module, intrinsics, name)?
            } else {
                bail!("gos_fn_addr: unknown fn `{name}`")
            };
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                as_i64,
            );
            Ok(true)
        }
        "gos_alloc" => {
            // Heap allocator primitive: forwards to libc `malloc`.
            // Single argument is the size in bytes; the return value
            // is a raw pointer (i64 on 64-bit, zero-extended on 32-bit).
            let malloc = intrinsics.extern_fn(module, "malloc", &[ptr_ty], &[ptr_ty])?;
            let malloc_ref = module.declare_func_in_func(malloc, builder.func);
            let size_val = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                as_i64,
            );
            Ok(true)
        }
        "gos_store" => {
            // Raw heap store: `gos_store(ptr, offset, value)` writes
            // `value` as an i64 at `ptr + offset`. Companion to
            // `gos_load` + `gos_alloc`.
            if args.len() < 3 {
                bail!("native codegen: gos_store requires (ptr, offset, value)");
            }
            let ptr_raw = lower_operand(
                module, builder, locals, body, tcx, &args[0], None, intrinsics,
            )?;
            let offset_raw = lower_operand(
                module, builder, locals, body, tcx, &args[1], None, intrinsics,
            )?;
            let value = lower_operand(
                module, builder, locals, body, tcx, &args[2], None, intrinsics,
            )?;
            // Closures pass `__env` as the first param; if its
            // declared type is the closure's body-return type
            // (bool/i8/etc), the inferred cl-type can be narrower
            // than ptr-width. Promote both halves to i64 before
            // adding so the iadd doesn't trip the verifier.
            let ptr_val = coerce_arg_to(builder, ptr_raw, types::I64).unwrap_or(ptr_raw);
            let offset_val = coerce_arg_to(builder, offset_raw, types::I64).unwrap_or(offset_raw);
            let value = coerce_arg_to(builder, value, types::I64).unwrap_or(value);
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                zero,
            );
            Ok(true)
        }
        "gos_load" => {
            // Raw heap load: `gos_load(ptr, offset)` reads an i64 at
            // `ptr + offset`.
            if args.len() < 2 {
                bail!("native codegen: gos_load requires (ptr, offset)");
            }
            let ptr_raw = lower_operand(
                module, builder, locals, body, tcx, &args[0], None, intrinsics,
            )?;
            let offset_raw = lower_operand(
                module, builder, locals, body, tcx, &args[1], None, intrinsics,
            )?;
            // See `gos_store` above — coerce both operands to i64
            // so the env-param's narrower inferred cl-type can't
            // mismatch the offset constant.
            let ptr_val = coerce_arg_to(builder, ptr_raw, types::I64).unwrap_or(ptr_raw);
            let offset_val = coerce_arg_to(builder, offset_raw, types::I64).unwrap_or(offset_raw);
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                loaded,
            );
            Ok(true)
        }
        "panic" => {
            // Route through `gos_rt_panic(msg)` after building a
            // single concatenated message from all arguments
            // (mirrors `render_args` in the interpreter — pieces
            // joined by a single space). Multi-arg
            // `panic("code=", 42)` previously dropped every arg
            // after the first.
            let panic_fn = intrinsics.extern_fn_by_name(module, "gos_rt_panic")?;
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
            let concat_fn = intrinsics.extern_fn_by_name(module, "gos_rt_str_concat")?;
            let fref = module.declare_func_in_func(concat_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let ptr = builder.inst_results(call)[0];
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_rt_str_concat destination cannot have projections");
            }
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        // Byte-at: `s[i]` on a `String` loads the `i`-th byte and
        // zero-extends to `i64` (matching the interpreter's
        // convention of returning byte codes as `i64`).
        "gos_rt_os_read_dir" => {
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_os_read_dir")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let p = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[p]);
            let ret = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ret,
            );
            Ok(true)
        }
        "gos_rt_str_substring" => {
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_str_substring",
                &[ptr_ty, types::I64, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let s = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let start = match args.get(1) {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(types::I64, 0),
            };
            let end = match args.get(2) {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(types::I64, 0),
            };
            let call = builder.ins().call(fref, &[s, start, end]);
            let ret = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ret,
            );
            Ok(true)
        }
        "gos_rt_str_byte_at" => {
            let ptr = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let idx = match args.get(1) {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx_ptr = match value_type(idx, builder) {
                t if t == ptr_ty => idx,
                t if t == types::I64 && ptr_ty == types::I32 => builder.ins().ireduce(ptr_ty, idx),
                t if t == types::I32 && ptr_ty == types::I64 => builder.ins().uextend(ptr_ty, idx),
                _ => idx,
            };
            let addr = builder.ins().iadd(ptr, idx_ptr);
            let byte = builder.ins().load(types::I8, MemFlags::trusted(), addr, 0);
            let value = builder.ins().uextend(types::I64, byte);
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_rt_str_byte_at destination cannot have projections");
            }
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                value,
            );
            Ok(true)
        }
        // String length: we treat `String` at the native ABI as a
        // nul-terminated pointer today, so `.len()` is plain
        // `strlen(ptr)`. Once the real `{ptr, len, cap}` header
        // ships this will route to a proper runtime symbol.
        "gos_rt_str_len" => {
            let strlen = intrinsics.extern_fn(module, "strlen", &[ptr_ty], &[types::I64])?;
            let strlen_ref = module.declare_func_in_func(strlen, builder.func);
            let ptr = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(strlen_ref, &[ptr]);
            let len = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                len,
            );
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
            // returns a `*mut GosVec` view over `argv + 1`.
            // `args.len()` reads `len` at offset 0 (the standard
            // GosVec layout) and indexing reads the i-th
            // `*const c_char` through the GosVec `ptr` field.
            let args_fn = intrinsics.extern_fn_by_name(module, "gos_rt_os_args")?;
            let fref = module.declare_func_in_func(args_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ret = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ret,
            );
            Ok(true)
        }
        // `std::time::now()` — opaque monotonic clock value. Cast
        // a `libc::clock_gettime` result into an i64 ns-since-
        // epoch. For now, return 0 so programs that print the
        // current instant compile; the interpreter path already
        // returns a real value.
        "time::now" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_time_now")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "time::now_ms" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_time_now_ms")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // `std::math::*` — all (f64) -> f64 except where noted.
        "math::sqrt" | "math::sin" | "math::cos" | "math::ln" | "math::log" | "math::exp"
        | "math::abs" | "math::floor" | "math::ceil" => {
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
            let rt_fn = intrinsics.extern_fn(module, rt_name, &[types::F64], &[types::F64])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::F64),
                    intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let call = builder.ins().call(fref, &[x64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::F64),
                    intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let y = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::F64),
                    intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let y64 = coerce_arg_to(builder, y, types::F64)?;
            let call = builder.ins().call(fref, &[x64, y64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "time::now_ns" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_now_ns")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_go_spawn_call_0" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_go_spawn_call_0")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let _ = builder.ins().call(fref, &[fn_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0_i64 = coerce_arg_to(builder, a0, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0_i64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0_i64 = coerce_arg_to(builder, a0, types::I64)?;
            let a1_i64 = coerce_arg_to(builder, a1, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0_i64, a1_i64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a2 = match args.get(3) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0 = coerce_arg_to(builder, a0, types::I64)?;
            let a1 = coerce_arg_to(builder, a1, types::I64)?;
            let a2 = coerce_arg_to(builder, a2, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0, a1, a2]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_go_spawn_call_5" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_5",
                &[
                    ptr_ty,
                    types::I64,
                    types::I64,
                    types::I64,
                    types::I64,
                    types::I64,
                ],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let mut vals = Vec::with_capacity(5);
            for i in 1..=5 {
                let v = match args.get(i) {
                    Some(a) => {
                        lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?
                    }
                    None => builder.ins().iconst(types::I64, 0),
                };
                vals.push(coerce_arg_to(builder, v, types::I64)?);
            }
            let mut all_args = vec![fn_addr];
            all_args.extend(vals);
            let _ = builder.ins().call(fref, &all_args);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_go_spawn_call_6" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_go_spawn_call_6",
                &[
                    ptr_ty,
                    types::I64,
                    types::I64,
                    types::I64,
                    types::I64,
                    types::I64,
                    types::I64,
                ],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let fn_addr = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let mut vals = Vec::with_capacity(6);
            for i in 1..=6 {
                let v = match args.get(i) {
                    Some(a) => {
                        lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?
                    }
                    None => builder.ins().iconst(types::I64, 0),
                };
                vals.push(coerce_arg_to(builder, v, types::I64)?);
            }
            let mut all_args = vec![fn_addr];
            all_args.extend(vals);
            let _ = builder.ins().call(fref, &all_args);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let a0 = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a1 = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a2 = match args.get(3) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a3 = match args.get(4) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let a0 = coerce_arg_to(builder, a0, types::I64)?;
            let a1 = coerce_arg_to(builder, a1, types::I64)?;
            let a2 = coerce_arg_to(builder, a2, types::I64)?;
            let a3 = coerce_arg_to(builder, a3, types::I64)?;
            let _ = builder.ins().call(fref, &[fn_addr, a0, a1, a2, a3]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "sync::yield_now" | "runtime::yield_now" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_go_yield")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let _ = builder.ins().call(fref, &[]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "time::sleep" => {
            // `time::sleep(ms: i64)` matches the VM and the Go
            // reference — argument is milliseconds. Routes
            // through the runtime's `gos_rt_sleep_ms` shim that
            // multiplies by 1_000_000 internally; before the
            // shim landed the compiled tier called
            // `gos_rt_sleep_ns(ms)` directly and slept for
            // nanoseconds, busy-spinning every poll loop.
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_sleep_ms")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let ms = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let ms = coerce_arg_to(builder, ms, types::I64)?;
            let _ = builder.ins().call(fref, &[ms]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_i64_to_str")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let n = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let call = builder.ins().call(fref, &[n64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_f64_to_str" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_f64_to_str")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::F64),
                    intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let x64 = coerce_arg_to(builder, x, types::F64)?;
            let call = builder.ins().call(fref, &[x64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "strconv::parse_i64" | "gos_rt_parse_i64" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_parse_i64",
                &[ptr_ty, ptr_ty],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let null = builder.ins().iconst(ptr_ty, 0);
            let call = builder.ins().call(fref, &[s, null]);
            let n = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                n,
            );
            Ok(true)
        }
        "gos_rt_parse_i64_result" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_parse_i64_result")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[s]);
            let r = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                r,
            );
            Ok(true)
        }
        "gos_rt_result_map_err" | "gos_rt_result_map" => {
            let helper_name: &'static str = if name == "gos_rt_result_map_err" {
                "gos_rt_result_map_err"
            } else {
                "gos_rt_result_map"
            };
            let rt_fn = intrinsics.extern_fn(module, helper_name, &[ptr_ty, ptr_ty], &[ptr_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let recv = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let clos = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[recv, clos]);
            let r = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                r,
            );
            Ok(true)
        }
        "gos_rt_flag_cell_load_str"
        | "gos_rt_flag_cell_load_i64"
        | "gos_rt_flag_cell_load_bool"
        | "gos_rt_flag_cell_load_f64"
        | "gos_rt_flag_cell_load_vec" => {
            let helper_name: &'static str = match name {
                "gos_rt_flag_cell_load_str" => "gos_rt_flag_cell_load_str",
                "gos_rt_flag_cell_load_i64" => "gos_rt_flag_cell_load_i64",
                "gos_rt_flag_cell_load_f64" => "gos_rt_flag_cell_load_f64",
                "gos_rt_flag_cell_load_vec" => "gos_rt_flag_cell_load_vec",
                _ => "gos_rt_flag_cell_load_bool",
            };
            let ret_ty = match helper_name {
                "gos_rt_flag_cell_load_i64" | "gos_rt_flag_cell_load_bool" => types::I64,
                "gos_rt_flag_cell_load_f64" => types::F64,
                _ => ptr_ty,
            };
            let rt_fn = intrinsics.extern_fn(module, helper_name, &[ptr_ty], &[ret_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let raw_cell = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let cell = coerce_arg_to(builder, raw_cell, ptr_ty)?;
            let call = builder.ins().call(fref, &[cell]);
            let mut r = builder.inst_results(call)[0];
            // Bool destination is declared as i8 in cranelift (MIR
            // bool_ty maps to I8). The helper returns i64 so the
            // result needs an ireduce to fit the destination Variable.
            if helper_name == "gos_rt_flag_cell_load_bool" {
                r = builder.ins().ireduce(types::I8, r);
            }
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                r,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let null = builder.ins().iconst(ptr_ty, 0);
            let call = builder.ins().call(fref, &[s, null]);
            let x = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                x,
            );
            Ok(true)
        }
        // `std::http::serve(addr, handler)` — start a blocking
        // TCP listener on `addr`. The handler is ignored today;
        // every request gets a static 200 response. The runtime
        // function itself never returns, but we leave the outer
        // terminator path (jump to next block) in place so
        // Cranelift's verifier stays happy — the jump is dead.
        "http::serve" | "gos_rt_http_serve" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_http_serve",
                &[ptr_ty, ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let addr = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let env = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let env_ptr = coerce_arg_to(builder, env, ptr_ty)?;
            let fn_ptr = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let fn_ptr64 = coerce_arg_to(builder, fn_ptr, types::I64)?;
            let _ = builder.ins().call(fref, &[addr, env_ptr, fn_ptr64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // Same shape as http::serve but routes to the h2 server.
        "http2::bind_and_run_h2c" | "gos_rt_http2_bind_and_run_h2c" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_http2_bind_and_run_h2c",
                &[ptr_ty, ptr_ty, types::I64],
                &[],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let addr = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let env = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let env_ptr = coerce_arg_to(builder, env, ptr_ty)?;
            let fn_ptr = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let fn_ptr64 = coerce_arg_to(builder, fn_ptr, types::I64)?;
            let _ = builder.ins().call(fref, &[addr, env_ptr, fn_ptr64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // `os::exit(code)` / `process::exit(code)` — both spellings
        // route through `gos_rt_exit` (which calls
        // `std::process::exit` — identical behavior to libc's
        // `exit`, but keeps every syscall that touches process
        // state inside the runtime crate).
        "os::exit" | "process::exit" => {
            let exit = intrinsics.extern_fn_by_name(module, "gos_rt_exit")?;
            let exit_ref = module.declare_func_in_func(exit, builder.func);
            let code = match args.first() {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I32, 0),
            };
            let code32 = match value_type(code, builder) {
                t if t == types::I32 => code,
                t if t.is_int() && t.bits() > 32 => builder.ins().ireduce(types::I32, code),
                _ => code,
            };
            let _ = builder.ins().call(exit_ref, &[code32]);
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                zero,
            );
            Ok(true)
        }
        // `process::id()` -> u32. Calls the runtime helper that
        // wraps `std::process::id`. Width-widen to i64 for the
        // destination since the local slots are 8 bytes.
        "process::id" => {
            let id_fn = intrinsics.extern_fn_by_name(module, "gos_rt_process_id")?;
            let id_ref = module.declare_func_in_func(id_fn, builder.func);
            let call = builder.ins().call(id_ref, &[]);
            let result = builder.inst_results(call)[0];
            let widened = builder.ins().uextend(types::I64, result);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                widened,
            );
            Ok(true)
        }
        // `process::abort()` -> !. Routes through gos_rt_process_abort.
        "process::abort" => {
            let abort_fn = intrinsics.extern_fn_by_name(module, "gos_rt_process_abort")?;
            let abort_ref = module.declare_func_in_func(abort_fn, builder.func);
            let _ = builder.ins().call(abort_ref, &[]);
            let zero = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                zero,
            );
            Ok(true)
        }
        // `Vec::new(elem_bytes)` / `Vec::with_capacity(elem_bytes,
        // cap)`. The MIR builder passes the actual element width
        // as the leading argument (sized from the binding's
        // `Vec<T>` element type via `elem_bytes_of`). Reading that
        // arg through — rather than hard-coding 8 — lets multi-
        // slot elements like `(String, i64)` reach the runtime
        // with the right stride.
        "Vec::new" | "gos_rt_vec_new" => {
            let kind = vec_elem_kind_from_dest(body, tcx, destination.local);
            let eb_raw = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 8),
            };
            let eb_i64 = coerce_arg_to(builder, eb_raw, types::I64)?;
            let eb = builder.ins().ireduce(types::I32, eb_i64);
            let ptr = if kind == vec_elem_kind_codegen::PRIMITIVE {
                let new_fn = intrinsics.extern_fn_by_name(module, "gos_rt_vec_new")?;
                let fref = module.declare_func_in_func(new_fn, builder.func);
                let call = builder.ins().call(fref, &[eb]);
                builder.inst_results(call)[0]
            } else {
                // Typed-allocation path: the runtime's deep-free
                // walks element pointers at vec_free time so a
                // `Vec<String>` / `Vec<Vec<T>>` / `Vec<HashMap<...>>`
                // does not leak its element payloads.
                let new_fn = intrinsics.extern_fn_by_name(module, "gos_rt_vec_new_typed")?;
                let fref = module.declare_func_in_func(new_fn, builder.func);
                let kind_val = builder.ins().iconst(types::I32, i64::from(kind));
                let call = builder.ins().call(fref, &[eb, kind_val]);
                builder.inst_results(call)[0]
            };
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "Vec::with_capacity" | "gos_rt_vec_with_capacity" => {
            let kind = vec_elem_kind_from_dest(body, tcx, destination.local);
            let eb_raw = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 8),
            };
            let eb_i64 = coerce_arg_to(builder, eb_raw, types::I64)?;
            let eb = builder.ins().ireduce(types::I32, eb_i64);
            let cap = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let cap64 = coerce_arg_to(builder, cap, types::I64)?;
            let ptr = if kind == vec_elem_kind_codegen::PRIMITIVE {
                let new_fn = intrinsics.extern_fn(
                    module,
                    "gos_rt_vec_with_capacity",
                    &[types::I32, types::I64],
                    &[ptr_ty],
                )?;
                let fref = module.declare_func_in_func(new_fn, builder.func);
                let call = builder.ins().call(fref, &[eb, cap64]);
                builder.inst_results(call)[0]
            } else {
                let new_fn = intrinsics.extern_fn(
                    module,
                    "gos_rt_vec_with_capacity_typed",
                    &[types::I32, types::I64, types::I32],
                    &[ptr_ty],
                )?;
                let fref = module.declare_func_in_func(new_fn, builder.func);
                let kind_val = builder.ins().iconst(types::I32, i64::from(kind));
                let call = builder.ins().call(fref, &[eb, cap64, kind_val]);
                builder.inst_results(call)[0]
            };
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_vec_from_arr" => {
            // Wraps a fixed-size array `[T; N]` in a heap GosVec.
            // Args: (elem_bytes: i64 -> coerced to u32, data: ptr,
            // len: i64). The MIR side emits this at the binding-
            // call boundary when a Vec<T> param meets a [T; N]
            // arg.
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_from_arr",
                &[types::I32, ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let elem_bytes = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 8),
            };
            let eb_i32 = coerce_arg_to(builder, elem_bytes, types::I32)?;
            let data_ptr = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let data_coerced = coerce_arg_to(builder, data_ptr, ptr_ty)?;
            let len = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let call = builder.ins().call(fref, &[eb_i32, data_coerced, len64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_nested_arr_to_vec" => {
            // Converts `[Array{T,inner_len}; outer_len]` → `Vec<Vec<T>>`.
            // Args: (inner_elem_bytes: i64, inner_len: i64, raw: ptr, outer_len: i64)
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_nested_arr_to_vec",
                &[types::I64, types::I64, ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let inner_eb = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 8),
            };
            let inner_len_v = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let raw_ptr = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let outer_len_v = match args.get(3) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let raw_coerced = coerce_arg_to(builder, raw_ptr, ptr_ty)?;
            let call = builder
                .ins()
                .call(fref, &[inner_eb, inner_len_v, raw_coerced, outer_len_v]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "HashMap::with_capacity"
        | "collections::HashMap::with_capacity"
        | "gos_rt_map_new_with_capacity" => {
            let new_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_new_with_capacity",
                &[types::I32, types::I32, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let k = builder.ins().iconst(types::I32, 8);
            let v = builder.ins().iconst(types::I32, 8);
            let cap = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let cap64 = coerce_arg_to(builder, cap, types::I64)?;
            let call = builder.ins().call(fref, &[k, v, cap64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "HashSet::new" | "collections::HashSet::new" => {
            let new_fn = intrinsics.extern_fn_by_name(module, "gos_rt_set_new")?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "BTreeMap::new" | "collections::BTreeMap::new" => {
            let new_fn = intrinsics.extern_fn_by_name(module, "gos_rt_btmap_new")?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_map_len" => {
            let len_fn = intrinsics.extern_fn_by_name(module, "gos_rt_map_len")?;
            let fref = module.declare_func_in_func(len_fn, builder.func);
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[m]);
            let n = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                n,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let v64 = coerce_arg_to(builder, v_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let v_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            let v_addr = builder.ins().stack_addr(ptr_ty, v_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            builder.ins().store(MemFlags::trusted(), v64, v_addr, 0);
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_addr, v_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let out_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            let out_addr = builder.ins().stack_addr(ptr_ty, out_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_addr, out_addr]);
            let loaded = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), out_addr, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                loaded,
            );
            Ok(true)
        }
        // Scalar-ABI insert: `m.insert(k, v)` for HashMap<K, V>
        // whose key + value widths are 8 bytes. Avoids the
        // stack-pointer dance the byte-erased
        // `gos_rt_map_insert` requires.
        "gos_rt_map_insert_i64_i64" => {
            let ins_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_insert_i64_i64",
                &[ptr_ty, types::I64, types::I64],
                &[],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k = coerce_arg_to(builder, k_val, types::I64)?;
            let v = coerce_arg_to(builder, v_val, types::I64)?;
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k, v]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // Scalar-ABI lookup. Returns 0 when the key is absent
        // (matches the Option-flat happy-path encoding the rest
        // of the compiled tier already uses).
        "gos_rt_map_get_i64" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_i64",
                &[ptr_ty, types::I64],
                &[types::I64],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_map_remove_i64" => {
            let rm_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_remove_i64",
                &[ptr_ty, types::I64],
                &[types::I8],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(rm_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k]);
            let ok = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        "gos_rt_map_contains_key_i64" => {
            let ck_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_contains_key_i64",
                &[ptr_ty, types::I64],
                &[types::I8],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(ck_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k]);
            let ok = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        "gos_rt_map_insert_str_i64" => {
            let ins_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_insert_str_i64",
                &[ptr_ty, ptr_ty, types::I64],
                &[],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v = coerce_arg_to(builder, v_val, types::I64)?;
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_val, v]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_map_get_str_i64" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_str_i64",
                &[ptr_ty, ptr_ty],
                &[types::I64],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_map_insert_str_str" => {
            let ins_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_insert_str_str",
                &[ptr_ty, ptr_ty, ptr_ty],
                &[],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k_val, v_val]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_map_get_str_str" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_str_str",
                &[ptr_ty, ptr_ty],
                &[ptr_ty],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val]);
            let s = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                s,
            );
            Ok(true)
        }
        "gos_rt_map_contains_key_str" => {
            let ck_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_contains_key_str",
                &[ptr_ty, ptr_ty],
                &[types::I8],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(ck_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val]);
            let ok = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        "gos_rt_map_remove_str" => {
            let rm_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_remove_str",
                &[ptr_ty, ptr_ty],
                &[types::I8],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(rm_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val]);
            let ok = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        "gos_rt_map_clear" => {
            let cl_fn = intrinsics.extern_fn_by_name(module, "gos_rt_map_clear")?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(cl_fn, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // `m.inc_at(seq, start, len, by)` — zero-copy slice hash
        // for `HashMap<String, i64>`, matching Rust's
        // `*m.entry(&seq[i..i+k]).or_insert(0) += by`.
        "gos_rt_map_inc_at_str_i64" => {
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_map_inc_at_str_i64",
                &[ptr_ty, ptr_ty, types::I64, types::I64, types::I64],
                &[types::I64],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let seq = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let start_v = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len_v = match args.get(3) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let by_v = match args.get(4) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 1),
            };
            let start64 = coerce_arg_to(builder, start_v, types::I64)?;
            let len64 = coerce_arg_to(builder, len_v, types::I64)?;
            let by64 = coerce_arg_to(builder, by_v, types::I64)?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[m, seq, start64, len64, by64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // Drop helpers emitted by the MIR's drop-insertion pass.
        // Each frees a heap-owned runtime container so the
        // process doesn't leak its contents across calls.
        "gos_rt_map_free"
        | "gos_rt_vec_free"
        | "gos_rt_set_free"
        | "gos_rt_btmap_free"
        | "gos_rt_arr_iter_free" => {
            let static_name: &'static str = match name {
                "gos_rt_map_free" => "gos_rt_map_free",
                "gos_rt_vec_free" => "gos_rt_vec_free",
                "gos_rt_set_free" => "gos_rt_set_free",
                "gos_rt_btmap_free" => "gos_rt_btmap_free",
                "gos_rt_arr_iter_free" => "gos_rt_arr_iter_free",
                _ => unreachable!(),
            };
            let f = intrinsics.extern_fn(module, static_name, &[ptr_ty], &[])?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // HashMap iteration helpers — each returns a *mut GosVec
        // snapshot of the requested column so the for-loop lowerer
        // can iterate it through the regular gos_rt_vec_* helpers.
        // The btmap_keys helper is the BTreeMap equivalent and
        // shares the same dispatch shape (`m: *mut Tagged → *mut
        // GosVec`), enabling `for (k, v) in btmap.iter()` to work
        // in compiled mode (was infinite-looping before).
        "gos_rt_map_keys_i64"
        | "gos_rt_map_values_i64"
        | "gos_rt_map_keys_str"
        | "gos_rt_map_values_str"
        | "gos_rt_btmap_keys" => {
            let static_name: &'static str = match name {
                "gos_rt_map_keys_i64" => "gos_rt_map_keys_i64",
                "gos_rt_map_values_i64" => "gos_rt_map_values_i64",
                "gos_rt_map_keys_str" => "gos_rt_map_keys_str",
                "gos_rt_map_values_str" => "gos_rt_map_values_str",
                "gos_rt_btmap_keys" => "gos_rt_btmap_keys",
                _ => unreachable!(),
            };
            let rt_fn = intrinsics.extern_fn(module, static_name, &[ptr_ty], &[ptr_ty])?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[m]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_map_inc_i64" => {
            let inc_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_inc_i64",
                &[ptr_ty, types::I64, types::I64],
                &[types::I64],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let by_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 1),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let by64 = coerce_arg_to(builder, by_val, types::I64)?;
            let fref = module.declare_func_in_func(inc_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k64, by64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_map_inc_str_i64" => {
            let inc_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_inc_str_i64",
                &[ptr_ty, ptr_ty, types::I64],
                &[types::I64],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let by_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 1),
            };
            let k_ptr = coerce_arg_to(builder, k, ptr_ty)?;
            let by64 = coerce_arg_to(builder, by_val, types::I64)?;
            let fref = module.declare_func_in_func(inc_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_ptr, by64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_map_get_or_i64" => {
            let get_or_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_or_i64",
                &[ptr_ty, types::I64, types::I64],
                &[types::I64],
            )?;
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let d_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let d64 = coerce_arg_to(builder, d_val, types::I64)?;
            let fref = module.declare_func_in_func(get_or_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k64, d64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // String-keyed `get_or` for `HashMap<String, i64>`. The key
        // travels as a `*const c_char`, the default and the result
        // are both i64.
        "gos_rt_map_get_or_str_i64" => {
            let get_or_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_or_str_i64",
                &[ptr_ty, ptr_ty, types::I64],
                &[types::I64],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let d_val = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let d64 = coerce_arg_to(builder, d_val, types::I64)?;
            let fref = module.declare_func_in_func(get_or_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val, d64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // String-keyed, string-valued `get_or`. Default and result
        // travel as `*const c_char`.
        "gos_rt_map_get_or_str_str" => {
            let get_or_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_or_str_str",
                &[ptr_ty, ptr_ty, ptr_ty],
                &[ptr_ty],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let d_val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let fref = module.declare_func_in_func(get_or_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_val, d_val]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // i64-keyed, string-valued `get_or` for `HashMap<i64, String>`.
        "gos_rt_map_get_or_i64_str" => {
            let get_or_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_or_i64_str",
                &[ptr_ty, types::I64, ptr_ty],
                &[ptr_ty],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let d_val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(get_or_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k64, d_val]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        // `m.insert(k: i64, v: String)` for `HashMap<i64, String>`.
        "gos_rt_map_insert_i64_str" => {
            let ins_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_insert_i64_str",
                &[ptr_ty, types::I64, ptr_ty],
                &[],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v_val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(ins_fn, builder.func);
            let _ = builder.ins().call(fref, &[m, k64, v_val]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // `m.get(k: i64) -> String` for `HashMap<i64, String>`.
        "gos_rt_map_get_i64_str" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_map_get_i64_str",
                &[ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let m = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let k_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let k64 = coerce_arg_to(builder, k_val, types::I64)?;
            let k_slot = builder.create_sized_stack_slot(StackSlotData::new(
                StackSlotKind::ExplicitSlot,
                8,
                3,
            ));
            let k_addr = builder.ins().stack_addr(ptr_ty, k_slot, 0);
            builder.ins().store(MemFlags::trusted(), k64, k_addr, 0);
            let fref = module.declare_func_in_func(rm_fn, builder.func);
            let call = builder.ins().call(fref, &[m, k_addr]);
            let ok = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        // JSON runtime — every helper accepts an opaque
        // `*mut GosJson` pointer so the codegen treats them as
        // pointer-sized values. The MIR rewriter routes
        // `value.field` on a `json::Value` receiver into a
        // `gos_rt_json_get(value, "field")` call before this
        // backend sees it.
        "gos_rt_json_parse" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_parse")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_string" | "gos_rt_json_value_array" | "gos_rt_json_value_object" => {
            let helper: &'static str = match name {
                "gos_rt_json_value_string" => "gos_rt_json_value_string",
                "gos_rt_json_value_array" => "gos_rt_json_value_array",
                _ => "gos_rt_json_value_object",
            };
            let rt_fn = intrinsics.extern_fn(module, helper, &[ptr_ty], &[ptr_ty])?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_object_n" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_json_value_object_n",
                &[types::I64, ptr_ty],
                &[ptr_ty],
            )?;
            let n = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let pairs = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let pairs_ptr = coerce_arg_to(builder, pairs, ptr_ty)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[n64, pairs_ptr]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_int" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_value_int")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let n = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = coerce_arg_to(builder, n, types::I64)?;
            let call = builder.ins().call(fref, &[n]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_float" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_value_float")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let x = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::F64),
                    intrinsics,
                )?,
                None => builder.ins().f64const(0.0),
            };
            let call = builder.ins().call(fref, &[x]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_bool" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_value_bool")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let b = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I8),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I8, 0),
            };
            let b = coerce_arg_to(builder, b, types::I32)?;
            let call = builder.ins().call(fref, &[b]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_value_null" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_value_null")?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_render" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_render")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_as_str" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_as_str")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_get" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_get")?;
            let recv = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let key = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let key_ptr = coerce_arg_to(builder, key, ptr_ty)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[recv, key_ptr]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_get_opt" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_json_get_opt",
                &[ptr_ty, ptr_ty],
                &[ptr_ty],
            )?;
            let recv = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let key = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let key_ptr = coerce_arg_to(builder, key, ptr_ty)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[recv, key_ptr]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_keys_opt" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_keys_opt")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_as_array_opt" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_as_array_opt")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_at" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_at")?;
            let recv = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let idx = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let idx64 = coerce_arg_to(builder, idx, types::I64)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[recv, idx64]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_len" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_len")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_as_i64" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_as_i64")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_as_f64" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_as_f64")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_is_null" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_is_null")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_as_bool" => {
            let rt_fn = intrinsics.extern_fn_by_name(module, "gos_rt_json_as_bool")?;
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let call = builder.ins().call(fref, &[arg]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_json_identity" => {
            let arg = lower_first_ptr_arg(module, builder, locals, body, tcx, args, intrinsics)?;
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                arg,
            );
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
        "channel" | "channel::new" | "sync::channel" | "sync::Channel::new" | "gos_rt_chan_new"
        | "Channel::new" => {
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
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                base,
            );
            Ok(true)
        }
        "gos_rt_chan_send" | "send" => {
            // Stack-spill the value word so the runtime's
            // `gos_rt_chan_send(chan, *const u8)` can memcpy it in.
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("chan_send: missing channel arg"),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
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
            let send_fn = intrinsics.extern_fn_by_name(module, "gos_rt_chan_send")?;
            let fref = module.declare_func_in_func(send_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan, slot_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_chan_try_send" | "try_send" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("chan_try_send: missing channel arg"),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ok,
            );
            Ok(true)
        }
        "gos_rt_chan_try_recv_option" | "gos_rt_chan_try_recv" | "try_recv" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("chan_try_recv: missing channel arg"),
            };
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_chan_try_recv_option",
                &[ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[chan]);
            let opt_ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                opt_ptr,
            );
            Ok(true)
        }
        "gos_rt_chan_close" | "close" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("chan_close: missing channel arg"),
            };
            let close_fn = intrinsics.extern_fn_by_name(module, "gos_rt_chan_close")?;
            let fref = module.declare_func_in_func(close_fn, builder.func);
            let _ = builder.ins().call(fref, &[chan]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_chan_recv_option" | "gos_rt_chan_recv" | "recv" => {
            let chan = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("chan_recv: missing channel arg"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_chan_recv_option")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[chan]);
            let opt_ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                opt_ptr,
            );
            Ok(true)
        }
        // ---- Mutex<T> primitive ----
        "Mutex::new" | "sync::Mutex::new" | "mutex::new" | "gos_rt_mutex_new" => {
            let new_fn = intrinsics.extern_fn_by_name(module, "gos_rt_mutex_new")?;
            let fref = module.declare_func_in_func(new_fn, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_mutex_lock" => {
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("mutex_lock: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_mutex_lock")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_mutex_unlock" => {
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("mutex_unlock: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_mutex_unlock")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[m]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // ---- WaitGroup primitive ----
        "WaitGroup::new" | "sync::WaitGroup::new" | "wg::new" | "gos_rt_wg_new" => {
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_wg_new")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_wg_add" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("wg_add: missing receiver"),
            };
            let n = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n64 = coerce_arg_to(builder, n, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_wg_add",
                &[ptr_ty, types::I64],
                &[types::I64],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[wg, n64]);
            let result = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                result,
            );
            Ok(true)
        }
        "gos_rt_wg_done" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("wg_done: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_wg_done")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[wg]);
            let result = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                result,
            );
            Ok(true)
        }
        "gos_rt_wg_wait" => {
            let wg = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("wg_wait: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_wg_wait")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let _ = builder.ins().call(fref, &[wg]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // ---- Heap [i64] primitive ----
        "I64Vec::new" | "heap_i64::new" | "gos_rt_heap_i64_new" => {
            let len = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_heap_i64_new")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[len64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_heap_i64_get" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_i64_get: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_heap_i64_set" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_i64_set: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_heap_i64_len" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_i64_len: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_heap_i64_len")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v]);
            let val = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_heap_i64_write_lines_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_i64_write_lines: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let w = match args.get(3) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_heap_i64_write_bytes_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_i64_write: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // ---- Heap [u8] primitive (`U8Vec`) — 1 byte per element ----
        "U8Vec::new" | "heap_u8::new" | "gos_rt_heap_u8_new" => {
            let len = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len, types::I64)?;
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_heap_u8_new")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[len64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_heap_u8_get" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_get: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_heap_u8_set" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_set: missing receiver"),
            };
            let idx = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let val = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_heap_u8_len" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_len: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_heap_u8_len")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v]);
            let val = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        // `buf.to_string(len)` — freezes the first `len` bytes of
        // a `U8Vec` build buffer into an immutable `String`.
        "gos_rt_heap_u8_to_string" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_to_string: missing receiver"),
            };
            let len_v = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let len64 = coerce_arg_to(builder, len_v, types::I64)?;
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_heap_u8_to_string",
                &[ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[v, len64]);
            let val = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_heap_u8_write_lines_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_write_lines: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let w = match args.get(3) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        "gos_rt_heap_u8_write_bytes_to_stdout" => {
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("heap_u8_write: missing receiver"),
            };
            let s = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = match args.get(2) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // ---- Atomic<i64> primitive ----
        "Atomic::new"
        | "sync::Atomic::new"
        | "atomic::new"
        | "AtomicI64::new"
        | "sync::AtomicI64::new"
        | "AtomicU64::new"
        | "sync::AtomicU64::new"
        | "gos_rt_atomic_i64_new" => {
            let initial = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
                )?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let i64 = coerce_arg_to(builder, initial, types::I64)?;
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_atomic_i64_new")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[i64]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_atomic_i64_load" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("atomic_load: missing receiver"),
            };
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_atomic_i64_load")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let call = builder.ins().call(fref, &[a]);
            let val = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_atomic_i64_store" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("atomic_store: missing receiver"),
            };
            let v = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // LCG jump-ahead helper. Used by multi-threaded programs
        // to seed each worker at the
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
                        module,
                        builder,
                        locals,
                        body,
                        tcx,
                        a,
                        Some(types::I64),
                        intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        "gos_rt_atomic_i64_fetch_add" => {
            let a = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => bail!("atomic_fetch_add: missing receiver"),
            };
            let d = match args.get(1) {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(types::I64),
                    intrinsics,
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
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                val,
            );
            Ok(true)
        }
        // `Vec<T>::len()` — the runtime exposes `len` as the first
        // i64 of the `#[repr(C)] GosVec { len, cap, elem_bytes, ptr }`
        // header (see runtime/src/c_abi.rs:1791). Inline the read as
        // a null check + offset-0 load so the for-loop bound check
        // doesn't pay the C-ABI call cost on every iteration. The
        // null guard preserves the helper's `null -> 0` semantics
        // (relied on by the `os::args` placeholder shape and any
        // uninitialised-Vec carrier in the codegen).
        "gos_rt_vec_len" => {
            let m = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let zero = builder.ins().iconst(types::I64, 0);
            let null_ptr = builder.ins().iconst(ptr_ty, 0);
            let is_null = builder.ins().icmp(ir::condcodes::IntCC::Equal, m, null_ptr);
            let loaded = builder.ins().load(types::I64, MemFlags::trusted(), m, 0);
            let n = builder.ins().select(is_null, zero, loaded);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                n,
            );
            Ok(true)
        }
        // Array length: forward to the runtime shim, which reads
        // the first i64 slot of the passed pointer (GosArgs and
        // other len-prefixed buffers share that layout).
        "gos_rt_str_is_empty" => {
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_str_is_empty")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[p]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_len_is_zero" => {
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_len_is_zero")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[p]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_arr_len" | "gos_rt_len" => {
            let len_fn = intrinsics.extern_fn_by_name(module, "gos_rt_arr_len")?;
            let len_ref = module.declare_func_in_func(len_fn, builder.func);
            let p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(len_ref, &[p]);
            let n = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                n,
            );
            Ok(true)
        }
        // Unary string helpers that return a fresh String
        // (allocated by the runtime). Signatures are `(ptr) -> ptr`.
        "gos_rt_str_trim"
        | "gos_rt_str_to_lower"
        | "gos_rt_str_to_upper"
        | "gos_rt_str_as_bytes"
        | "gos_rt_vec_clone" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                match name {
                    "gos_rt_str_trim" => "gos_rt_str_trim",
                    "gos_rt_str_to_lower" => "gos_rt_str_to_lower",
                    "gos_rt_str_to_upper" => "gos_rt_str_to_upper",
                    "gos_rt_str_as_bytes" => "gos_rt_str_as_bytes",
                    "gos_rt_vec_clone" => "gos_rt_vec_clone",
                    _ => unreachable!(),
                },
                &[ptr_ty],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[s]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let result = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                result,
            );
            Ok(true)
        }
        "gos_rt_str_find" | "gos_rt_str_find_opt" => {
            // `find_opt` returns `*mut GosResult` (Option<i64>);
            // the bare `find` returns raw i64. Both share two
            // `*const c_char` argument shapes — pick the result
            // type by the symbol name.
            let (sym, ret_ty): (&'static str, _) = if name == "gos_rt_str_find_opt" {
                ("gos_rt_str_find_opt", ptr_ty)
            } else {
                ("gos_rt_str_find", types::I64)
            };
            let rt_fn = intrinsics.extern_fn(module, sym, &[ptr_ty, ptr_ty], &[ret_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let n = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                n,
            );
            Ok(true)
        }
        // `s.split(sep)`, `s.lines()`, `s.repeat(n)`. Each
        // returns a fresh GC-managed pointer (Vec or String).
        "gos_rt_str_eq" => {
            let f = intrinsics.extern_fn_by_name(module, "gos_rt_str_eq")?;
            let fref = module.declare_func_in_func(f, builder.func);
            let a = match args.first() {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b]);
            let v = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                v,
            );
            Ok(true)
        }
        "gos_rt_str_split" | "gos_rt_str_lines" => {
            let arity_two = name == "gos_rt_str_split";
            let params: &[ir::Type] = if arity_two {
                &[ptr_ty, ptr_ty]
            } else {
                &[ptr_ty]
            };
            // `extern_fn` keys on a `&'static str`; leak the
            // matched name once. Bounded leak — at most two
            // entries (split + lines) across the program.
            let static_name: &'static str = match name {
                "gos_rt_str_split" => "gos_rt_str_split",
                "gos_rt_str_lines" => "gos_rt_str_lines",
                _ => unreachable!(),
            };
            let rt_fn = intrinsics.extern_fn(module, static_name, params, &[ptr_ty])?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let result = if arity_two {
                let sep = match args.get(1) {
                    Some(arg) => {
                        let raw = lower_operand(
                            module,
                            builder,
                            locals,
                            body,
                            tcx,
                            arg,
                            Some(ptr_ty),
                            intrinsics,
                        )?;
                        if operand_is_char(body, tcx, arg) {
                            // Char separator: convert to a one-
                            // char c-string before passing to
                            // the runtime helper.
                            let cts = intrinsics.extern_fn(
                                module,
                                "gos_rt_char_to_str",
                                &[types::I32],
                                &[ptr_ty],
                            )?;
                            let cts_ref = module.declare_func_in_func(cts, builder.func);
                            let call = builder.ins().call(cts_ref, &[raw]);
                            builder.inst_results(call)[0]
                        } else {
                            coerce_arg_to(builder, raw, ptr_ty)?
                        }
                    }
                    None => builder.ins().iconst(ptr_ty, 0),
                };
                builder.ins().call(fref, &[s, sep])
            } else {
                builder.ins().call(fref, &[s])
            };
            let ptr = builder.inst_results(result)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        "gos_rt_str_repeat" => {
            let rt_fn = intrinsics.extern_fn(
                module,
                "gos_rt_str_repeat",
                &[ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(rt_fn, builder.func);
            let s = match args.first() {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let n_val = match args.get(1) {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(types::I64, 0),
            };
            let n = coerce_arg_to(builder, n_val, types::I64)?;
            let call = builder.ins().call(fref, &[s, n]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
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
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let b = match args.get(1) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let c = match args.get(2) {
                Some(arg) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    arg,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let call = builder.ins().call(fref, &[a, b, c]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        // `v.push(x)` on a Vec<T>: spill x to a stack slot and
        // call the runtime's typed push. The runtime reads
        // `vec.elem_bytes` bytes from the pointer we pass, so for
        // multi-slot aggregates (tuples / structs / inline arrays)
        // we must pass the address of the actual storage —
        // spilling the operand's pointer-value into an 8-byte
        // slot leaks only the first word and rereads adjacent
        // stack bytes for the rest. Scalars still go through the
        // 8-byte slot path so misaligned int / float types reach
        // the runtime as a clean little-endian 8-byte payload.
        "gos_rt_vec_push" => {
            let push_fn = intrinsics.extern_fn_by_name(module, "gos_rt_vec_push")?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let elem_arg = args.get(1);
            let agg_slots = elem_arg.and_then(|a| operand_aggregate_slots(body, tcx, a));
            let elem_addr = if let (Some(slots), Some(a)) = (agg_slots, elem_arg) {
                // Multi-slot aggregate operand. Take the address of
                // its backing storage and pass it through — the
                // runtime memcpys `slots * 8` bytes into the vec.
                let _ = slots;
                let Operand::Copy(place) = a else {
                    // operand_aggregate_slots only returns Some for
                    // Copy(place) — unreachable otherwise.
                    unreachable!("aggregate-slot operand must be Copy(place)")
                };
                lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?
            } else {
                let value = match elem_arg {
                    Some(a) => {
                        lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?
                    }
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
                slot_addr
            };
            let fref = module.declare_func_in_func(push_fn, builder.func);
            let _ = builder.ins().call(fref, &[vec_p, elem_addr]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // Typed-i64 push used by the dynamic-count `[value; n]`
        // lowering. The wrapper handles the stack-slot dance
        // inside the runtime so the codegen doesn't have to.
        "gos_rt_vec_push_i64" => {
            let push_fn = intrinsics.extern_fn_by_name(module, "gos_rt_vec_push_i64")?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let value = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let v64 = coerce_arg_to(builder, value, types::I64)?;
            let fref = module.declare_func_in_func(push_fn, builder.func);
            let _ = builder.ins().call(fref, &[vec_p, v64]);
            let unit = builder.ins().iconst(types::I64, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                unit,
            );
            Ok(true)
        }
        // `arr[lo..hi]` — copies a subrange into a new GosVec.
        "gos_rt_vec_slice" => {
            let f = intrinsics.extern_fn(
                module,
                "gos_rt_vec_slice",
                &[ptr_ty, types::I64, types::I64],
                &[ptr_ty],
            )?;
            let fref = module.declare_func_in_func(f, builder.func);
            let v = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let lo_v = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let hi_v = match args.get(2) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let lo = coerce_arg_to(builder, lo_v, types::I64)?;
            let hi = coerce_arg_to(builder, hi_v, types::I64)?;
            let call = builder.ins().call(fref, &[v, lo, hi]);
            let p = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                p,
            );
            Ok(true)
        }
        // `vec_get_ptr(v, i)` — returns a `*mut u8` pointer to
        // the i-th element's slot. Used by the for-vec loop
        // lowering to read each element via a follow-up
        // `gos_load(ptr, 0)` so the same code handles scalar
        // and pointer-shaped element types.
        "gos_rt_vec_get_ptr" => {
            let get_fn = intrinsics.extern_fn(
                module,
                "gos_rt_vec_get_ptr",
                &[ptr_ty, types::I64],
                &[ptr_ty],
            )?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
                )?,
                None => builder.ins().iconst(ptr_ty, 0),
            };
            let i_val = match args.get(1) {
                Some(a) => lower_operand(module, builder, locals, body, tcx, a, None, intrinsics)?,
                None => builder.ins().iconst(types::I64, 0),
            };
            let i = coerce_arg_to(builder, i_val, types::I64)?;
            let fref = module.declare_func_in_func(get_fn, builder.func);
            let call = builder.ins().call(fref, &[vec_p, i]);
            let ptr = builder.inst_results(call)[0];
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                ptr,
            );
            Ok(true)
        }
        // `v.pop()` — pops the last element through an 8-byte
        // stack slot and returns it. Returns 0 when the vec is
        // empty; callers that care about emptiness should check
        // `.len()` first.
        "gos_rt_vec_pop" => {
            let pop_fn = intrinsics.extern_fn_by_name(module, "gos_rt_vec_pop")?;
            let vec_p = match args.first() {
                Some(a) => lower_operand(
                    module,
                    builder,
                    locals,
                    body,
                    tcx,
                    a,
                    Some(ptr_ty),
                    intrinsics,
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
            let loaded = builder
                .ins()
                .load(types::I64, MemFlags::trusted(), slot_addr, 0);
            define_var_to(
                builder,
                locals,
                &intrinsics.body_cl_types,
                destination.local,
                loaded,
            );
            Ok(true)
        }
        // Generic forwarding for the new stdlib helpers added in
        // round 3 (errors / regex / fs / path / flag / bufio /
        // http / gzip / slog / testing). Each follows the same
        // shape: the MIR side picked a single runtime symbol
        // and supplies the args; we declare the extern with the
        // right signature based on the symbol name and call it.
        s if generic_rt_static_name(s).is_some() => {
            let static_name = generic_rt_static_name(s).expect("checked above");
            lower_generic_rt_call(
                module,
                builder,
                locals,
                body,
                tcx,
                args,
                intrinsics,
                destination,
                static_name,
            )?;
            Ok(true)
        }
        s if s.starts_with("gos_binding_") => {
            lower_external_binding_call(
                module,
                builder,
                locals,
                body,
                tcx,
                args,
                intrinsics,
                destination,
                s,
            )?;
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
            if let Some(r) = callees_by_def.get(&def.local).copied() {
                return Ok(r);
            }
            if let Some(r) = callees_by_name.get(&format!("fn#{}", def.local)).copied() {
                return Ok(r);
            }
            // Unknown DefId — fall back to a "missing-fn" stub so
            // the program still builds. The stub returns zero,
            // which is the right default for primitive returns
            // and a null pointer for callable shapes. Programs
            // that depend on the missing function's real
            // semantics produce wrong output but compile cleanly.
            // Common producers: enum variant constructor DefIds
            // that the resolver allocates but the MIR side never
            // emits a body for.
            Err(anyhow!("native codegen: unknown callee def#{}", def.local))
        }
        other => bail!("native codegen: call target must be FnRef, got {other:?}"),
    }
}

#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
        Rvalue::Use(operand) => lower_operand(
            module, builder, locals, body, tcx, operand, dst_hint, intrinsics,
        )?,
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
            let a = lower_operand(
                module, builder, locals, body, tcx, lhs, arith_hint, intrinsics,
            )?;
            let b_hint = arith_hint.or_else(|| Some(value_type(a, builder)));
            let b = lower_operand(module, builder, locals, body, tcx, rhs, b_hint, intrinsics)?;
            // String comparisons must go through `gos_rt_str_compare`
            // rather than comparing pointer addresses (which would
            // produce random ordering based on heap layout).
            let is_str_cmp = matches!(
                op,
                BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge
            ) && operand_is_string(tcx, body, lhs);
            if is_str_cmp {
                let ptr_ty = module.target_config().pointer_type();
                let cmp_fn = intrinsics.extern_fn(
                    module,
                    "gos_rt_str_compare",
                    &[ptr_ty, ptr_ty],
                    &[types::I32],
                )?;
                let cmp_ref = module.declare_func_in_func(cmp_fn, builder.func);
                let call = builder.ins().call(cmp_ref, &[a, b]);
                let cmp_result = builder.inst_results(call)[0];
                let zero = builder.ins().iconst(types::I32, 0);
                match op {
                    BinOp::Eq => builder.ins().icmp(IntCC::Equal, cmp_result, zero),
                    BinOp::Ne => builder.ins().icmp(IntCC::NotEqual, cmp_result, zero),
                    BinOp::Lt => builder.ins().icmp(IntCC::SignedLessThan, cmp_result, zero),
                    BinOp::Le => builder
                        .ins()
                        .icmp(IntCC::SignedLessThanOrEqual, cmp_result, zero),
                    BinOp::Gt => builder
                        .ins()
                        .icmp(IntCC::SignedGreaterThan, cmp_result, zero),
                    BinOp::Ge => {
                        builder
                            .ins()
                            .icmp(IntCC::SignedGreaterThanOrEqual, cmp_result, zero)
                    }
                    _ => unreachable!(),
                }
            // Float `%` on f64 → `libc::fmod`. Cranelift has no
            // direct opcode. Intercept before the generic binop
            // dispatch so the rest stays module-free.
            } else if matches!(op, BinOp::Rem) && value_type(a, builder).is_float() {
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
                // Operand signedness only matters for `Shr` today
                // (the unsigned types use the same `iadd`/`isub`
                // hardware ops as signed); pick the LHS as the
                // canonical signedness source for the dispatch.
                let unsigned_hint =
                    matches!(op, BinOp::Shr) && operand_is_unsigned_int(body, tcx, lhs);
                lower_binop(builder, *op, a, b, unsigned_hint)?
            }
        }
        Rvalue::UnaryOp { op, operand } => {
            let v = lower_operand(
                module, builder, locals, body, tcx, operand, dst_hint, intrinsics,
            )?;
            match op {
                UnOp::Neg => {
                    if value_type(v, builder).is_float() {
                        builder.ins().fneg(v)
                    } else {
                        builder.ins().ineg(v)
                    }
                }
                UnOp::Not => {
                    // For booleans (cranelift `I8`) `!` is *logical*
                    // negation — flip bit 0 only. `bnot` flips
                    // every bit (1 → 0xfe), which the downstream
                    // `if !flag` non-zero test then misreads as
                    // true and the user code's `if !first` arm
                    // always fires. For wider integer types the
                    // user wrote bitwise NOT (Rust convention),
                    // so keep `bnot` there.
                    let vt = value_type(v, builder);
                    if vt == types::I8 {
                        let one = builder.ins().iconst(types::I8, 1);
                        builder.ins().bxor(v, one)
                    } else {
                        builder.ins().bnot(v)
                    }
                }
            }
        }
        Rvalue::Cast { operand, target } => {
            // Determine source / destination cranelift type and
            // emit the right conversion. Without this i64 ↔ f64
            // bit-transmutes (the operand SSA value just gets
            // reused), which silently miscompiles benchmarks
            // that do `i as f64`.
            let src_v = lower_operand(
                module, builder, locals, body, tcx, operand, dst_hint, intrinsics,
            )?;
            let src_ty = builder.func.dfg.value_type(src_v);
            let dst_ty = cl_type_of(tcx, *target, module);
            match (src_ty, dst_ty) {
                // No-op when source and destination types coincide.
                (a, b) if a == b => src_v,
                // Integer → float (f32 / f64). Use signed
                // conversion since Gossamer's primary integer is
                // signed `i64`. Unsigned casts go through a same-
                // width int rebox before this point.
                (s, d) if s.is_int() && d.is_float() => builder.ins().fcvt_from_sint(d, src_v),
                // Float → integer. Saturating conversion matches
                // Rust's `as` (NaN → 0, ±Inf clamps to bounds).
                (s, d) if s.is_float() && d.is_int() => builder.ins().fcvt_to_sint_sat(d, src_v),
                // Integer width adjustments.
                (s, d) if s.is_int() && d.is_int() => {
                    if d.bits() > s.bits() {
                        // Use zero-extension for unsigned source types (u8/u16/u32)
                        // so that e.g. `255u8 as i32` yields 255, not -1.
                        let src_unsigned = if let Operand::Copy(place) = operand {
                            matches!(
                                tcx.kind_of(body.local_ty(place.local)),
                                TyKind::Int(IntTy::U8 | IntTy::U16 | IntTy::U32)
                            )
                        } else {
                            false
                        };
                        if src_unsigned {
                            builder.ins().uextend(d, src_v)
                        } else {
                            builder.ins().sextend(d, src_v)
                        }
                    } else if d.bits() < s.bits() {
                        builder.ins().ireduce(d, src_v)
                    } else {
                        src_v
                    }
                }
                // Float width adjustments (f32 ↔ f64).
                (s, d) if s.is_float() && d.is_float() => {
                    if d.bits() > s.bits() {
                        builder.ins().fpromote(d, src_v)
                    } else if d.bits() < s.bits() {
                        builder.ins().fdemote(d, src_v)
                    } else {
                        src_v
                    }
                }
                _ => src_v,
            }
        }
        Rvalue::Aggregate { kind, operands } => {
            // Aggregates live in a stack slot N*8 bytes wide. Each
            // scalar field occupies an 8-byte slot. Arrays of
            // structs stride by (#struct-fields) slots so that
            // `a[i].f` projects correctly.
            //
            // Structs (`AggregateKind::Adt`) with struct variant
            // shapes use the flat-slot layout where every nested
            // struct/tuple field expands inline — the running byte
            // offset is summed from `type_slot_count` of each prior
            // operand's source, so `outer.tag` lands past the
            // embedded `inner` instead of overlapping it.
            let elem_slots: u32 = match kind {
                gossamer_mir::AggregateKind::Array => operands.first().map_or(1, |op| {
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
            // Pre-compute per-operand slot widths for ADT/Tuple
            // aggregates. These drive both the total allocation
            // size and the running destination offset for each
            // field's store/memcpy below.
            let operand_slot_widths: Vec<u32> = match kind {
                gossamer_mir::AggregateKind::Adt { def, .. } => {
                    let from_tcx = tcx.struct_field_tys(*def).map(|tys| {
                        tys.iter()
                            .map(|t| type_slot_count(tcx, *t))
                            .collect::<Vec<_>>()
                    });
                    if let Some(widths) = from_tcx {
                        // Pad with 1-slot defaults if MIR provides
                        // more operands than the registered field
                        // list (defensive).
                        let mut widths = widths;
                        while widths.len() < operands.len() {
                            widths.push(1);
                        }
                        widths
                    } else {
                        operands
                            .iter()
                            .map(|op| match op {
                                Operand::Copy(place) if place.projection.is_empty() => intrinsics
                                    .local_slots
                                    .get(&place.local)
                                    .copied()
                                    .or_else(|| {
                                        let ty = body.local_ty(place.local);
                                        let n = type_slot_count(tcx, ty);
                                        if n > 1 { Some(n) } else { None }
                                    })
                                    .unwrap_or(1),
                                Operand::Copy(place) => {
                                    let leaf = resolve_place_ty(tcx, body, place);
                                    type_slot_count(tcx, leaf).max(1)
                                }
                                _ => 1,
                            })
                            .collect()
                    }
                }
                gossamer_mir::AggregateKind::Tuple => operands
                    .iter()
                    .map(|op| match op {
                        Operand::Copy(place) if place.projection.is_empty() => intrinsics
                            .local_slots
                            .get(&place.local)
                            .copied()
                            .or_else(|| {
                                let ty = body.local_ty(place.local);
                                let n = type_slot_count(tcx, ty);
                                if n > 1 { Some(n) } else { None }
                            })
                            .unwrap_or(1),
                        Operand::Copy(place) => {
                            let leaf = resolve_place_ty(tcx, body, place);
                            type_slot_count(tcx, leaf).max(1)
                        }
                        _ => 1,
                    })
                    .collect(),
                gossamer_mir::AggregateKind::Array => Vec::new(),
            };
            let total_slots: u32 = match kind {
                gossamer_mir::AggregateKind::Array => (operands.len() as u32) * elem_slots,
                _ => operand_slot_widths.iter().copied().sum::<u32>().max(1),
            };
            let size = total_slots * 8;
            let ptr_ty = module.target_config().pointer_type();
            // Heap-allocate (zeroed) via `gos_rt_aggr_alloc`. Stack-slot
            // allocation breaks the moment the aggregate address
            // escapes the constructing frame (returning a struct
            // from a method, storing it in a vec, …) — the slot
            // dies on epilogue and the next call overwrites it.
            // The runtime helper tracks every allocation so the
            // MIR drop pass can reclaim it via `gos_rt_aggr_free`
            // at scope exit
            let alloc_fn =
                intrinsics.extern_fn(module, "gos_rt_aggr_alloc", &[types::I64], &[ptr_ty])?;
            let alloc_ref = module.declare_func_in_func(alloc_fn, builder.func);
            let size_val = builder.ins().iconst(types::I64, i64::from(size.max(8)));
            let alloc_call = builder.ins().call(alloc_ref, &[size_val]);
            let base = builder.inst_results(alloc_call)[0];
            emit_root_push(module, builder, intrinsics, base)?;
            // Running destination offset (in bytes) for ADT/Tuple
            // aggregates so each prior nested struct's full slot
            // span shifts subsequent fields past its layout.
            let mut running_dst_off: u32 = 0;
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
                    Operand::Copy(place) => {
                        // Projected read of a multi-slot field
                        // (`..base` filling): the read returns the
                        // field's address; copy its slot span into
                        // the new aggregate's slot.
                        let leaf = resolve_place_ty(tcx, body, place);
                        let count = type_slot_count(tcx, leaf);
                        if count > 1 { Some(count) } else { None }
                    }
                    _ => None,
                };
                let dst_off = match kind {
                    gossamer_mir::AggregateKind::Array => (i as u32) * elem_slots * 8,
                    _ => running_dst_off,
                };
                if !matches!(kind, gossamer_mir::AggregateKind::Array) {
                    let width = operand_slot_widths.get(i).copied().unwrap_or(1).max(1);
                    running_dst_off = running_dst_off.saturating_add(width * 8);
                }
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
            let ptr_ty = module.target_config().pointer_type();
            // Heap-allocate (zeroed) via gos_rt_aggr_alloc (see
            // Aggregate path above; same tracking semantics).
            let alloc_fn =
                intrinsics.extern_fn(module, "gos_rt_aggr_alloc", &[types::I64], &[ptr_ty])?;
            let alloc_ref = module.declare_func_in_func(alloc_fn, builder.func);
            let size_val = builder.ins().iconst(types::I64, i64::from(size.max(8)));
            let alloc_call = builder.ins().call(alloc_ref, &[size_val]);
            let base = builder.inst_results(alloc_call)[0];
            emit_root_push(module, builder, intrinsics, base)?;
            // Threshold for switching from unrolled stores to a counted
            // loop. Unrolling beyond this generates O(count) Cranelift
            // instructions — for `[f64; 6000]` that inflates the JIT IR
            // to tens of thousands of ops, pushing peak RSS ~30 MB for a
            // single compilation. A loop keeps the IR size O(1).
            const UNROLL_LIMIT: u64 = 16;
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
                    if *count <= UNROLL_LIMIT {
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
                    } else {
                        // Counted loop: counter in loop_header block param.
                        let loop_hdr = builder.create_block();
                        let loop_body = builder.create_block();
                        let exit_blk = builder.create_block();
                        builder.append_block_param(loop_hdr, types::I64);
                        let zero = builder.ins().iconst(types::I64, 0);
                        builder.ins().jump(loop_hdr, &[ir::BlockArg::Value(zero)]);
                        builder.switch_to_block(loop_hdr);
                        let ctr = builder.block_params(loop_hdr)[0];
                        let cnt_v = builder.ins().iconst(types::I64, *count as i64);
                        let ok = builder.ins().icmp(IntCC::SignedLessThan, ctr, cnt_v);
                        builder.ins().brif(ok, loop_body, &[], exit_blk, &[]);
                        builder.switch_to_block(loop_body);
                        let stride = i64::from(elem_slots) * 8;
                        let dst_base = builder.ins().imul_imm(ctr, stride);
                        for slot_idx in 0..elem_slots {
                            let src_off = ir::immediates::Offset32::new(slot_idx as i32 * 8);
                            let word =
                                builder
                                    .ins()
                                    .load(types::I64, MemFlags::trusted(), src, src_off);
                            let slot_off =
                                builder.ins().iadd_imm(dst_base, i64::from(slot_idx) * 8);
                            let dst = builder.ins().iadd(base, slot_off);
                            builder.ins().store(
                                MemFlags::trusted(),
                                word,
                                dst,
                                ir::immediates::Offset32::new(0),
                            );
                        }
                        let next = builder.ins().iadd_imm(ctr, 1);
                        builder.ins().jump(loop_hdr, &[ir::BlockArg::Value(next)]);
                        builder.switch_to_block(exit_blk);
                    }
                }
            } else {
                // Scalar repeat (`[v; N]` where v is one slot wide).
                // calloc already zeroed the buffer, so zero constants need
                // no stores at all — skip the init entirely.
                let is_zero = matches!(
                    value,
                    Operand::Const(
                        ConstValue::Int(0)
                            | ConstValue::Float(0)
                            | ConstValue::Bool(false)
                            | ConstValue::Unit
                    )
                );
                if !is_zero {
                    let element =
                        lower_operand(module, builder, locals, body, tcx, value, None, intrinsics)?;
                    if *count <= UNROLL_LIMIT {
                        for i in 0..*count {
                            let offset = ir::immediates::Offset32::new(
                                i32::try_from(i.saturating_mul(8)).map_err(|_| {
                                    anyhow!("native codegen: repeat offset too large")
                                })?,
                            );
                            builder
                                .ins()
                                .store(MemFlags::trusted(), element, base, offset);
                        }
                    } else {
                        let loop_hdr = builder.create_block();
                        let loop_body = builder.create_block();
                        let exit_blk = builder.create_block();
                        builder.append_block_param(loop_hdr, types::I64);
                        let zero = builder.ins().iconst(types::I64, 0);
                        builder.ins().jump(loop_hdr, &[ir::BlockArg::Value(zero)]);
                        builder.switch_to_block(loop_hdr);
                        let ctr = builder.block_params(loop_hdr)[0];
                        let cnt_v = builder.ins().iconst(types::I64, *count as i64);
                        let ok = builder.ins().icmp(IntCC::SignedLessThan, ctr, cnt_v);
                        builder.ins().brif(ok, loop_body, &[], exit_blk, &[]);
                        builder.switch_to_block(loop_body);
                        let byte_off = builder.ins().imul_imm(ctr, 8_i64);
                        let dst = builder.ins().iadd(base, byte_off);
                        builder.ins().store(
                            MemFlags::trusted(),
                            element,
                            dst,
                            ir::immediates::Offset32::new(0),
                        );
                        let next = builder.ins().iadd_imm(ctr, 1);
                        builder.ins().jump(loop_hdr, &[ir::BlockArg::Value(next)]);
                        builder.switch_to_block(exit_blk);
                    }
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
                let var = ensure_var(
                    builder,
                    locals,
                    body,
                    tcx,
                    module,
                    &intrinsics.body_cl_types,
                    place.local,
                );
                builder.use_var(var)
            } else {
                lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?
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

#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
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
#[allow(
    clippy::too_many_arguments,
    reason = "cranelift codegen plumbing — module/builder/locals/body/tcx/intrinsics threaded through every helper"
)]
fn lower_place_read(
    module: &mut dyn Module,
    builder: &mut FunctionBuilder<'_>,
    locals: &mut HashMap<Local, Variable>,
    body: &Body,
    tcx: &TyCtxt,
    place: &Place,
    hint: Option<ir::Type>,
    intrinsics: &mut IntrinsicContext,
) -> Result<ir::Value> {
    if place.projection.is_empty() {
        let var = ensure_var(
            builder,
            locals,
            body,
            tcx,
            module,
            &intrinsics.body_cl_types,
            place.local,
        );
        return Ok(builder.use_var(var));
    }
    let addr = lower_place_address(module, builder, locals, body, tcx, place, intrinsics)?;
    // When the projected leaf is itself a multi-slot aggregate
    // (struct/tuple/array embedded inline), return the field's
    // address rather than reading a single i64 word. The receiving
    // local treats the value as a pointer-to-aggregate and walks
    // further projections off of it; loading would collapse the
    // sub-struct to its first slot and segfault on any subsequent
    // `Field`/`Index` step.
    let leaf_ty_mir = resolve_place_ty(tcx, body, place);
    if type_slot_count(tcx, leaf_ty_mir) > 1 {
        return Ok(addr);
    }
    let leaf_ty = resolve_place_cl_type(tcx, body, place, module, hint);
    // Use plain `MemFlags::new()` instead of `trusted()` — without
    // it cranelift's alias analysis was load-CSEing reads across
    // unrelated stores, e.g. in
    //   let t = arr[lo]
    //   let u = arr[hi]
    //   arr[hi] = t
    //   arr[lo] = u
    // the second store materialised `u` from a fresh load of
    // `arr+hi*8` *after* `arr+hi*8` had been overwritten with `t`,
    // collapsing the swap to a degenerate `arr[lo] = arr[lo]`.
    Ok(builder.ins().load(leaf_ty, MemFlags::new(), addr, 0))
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
            let ty = hint.filter(|t| t.is_int()).unwrap_or(types::I64);
            builder.ins().iconst(ty, i64_truncate(*n))
        }
        ConstValue::Bool(b) => {
            let ty = hint.filter(|t| t.is_int()).unwrap_or(types::I8);
            builder.ins().iconst(ty, i64::from(*b))
        }
        ConstValue::Char(c) => {
            let ty = hint.filter(|t| t.is_int()).unwrap_or(types::I32);
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
            let ty = hint.filter(|t| t.is_float()).unwrap_or(types::F64);
            let val = f64::from_bits(*bits);
            if ty == types::F32 {
                builder.ins().f32const(val as f32)
            } else {
                builder.ins().f64const(val)
            }
        }
    })
}

/// Reinterprets the low 64 bits of an i128 const as i64.
///
/// Cranelift's `iconst` only accepts an `i64`. The MIR const layer
/// stores integer literals as `i128` so values outside i64's range
/// (e.g. `u64::MAX`) survive type-check, but at codegen time both
/// the i64 and u64 paths read the same 64-bit machine register —
/// the `gos_rt_print_i64` / `gos_rt_print_u64` dispatch in
/// `operand_print_kind` is what decides how the bit pattern is
/// formatted. Saturating here would silently corrupt unsigned
/// values >= 2^63 (collapsing `u64::MAX` to `i64::MAX`); wrapping
/// preserves the bit pattern so the print path can interpret it.
fn i64_truncate(n: i128) -> i64 {
    n as i64
}

/// Dispatches a binary op based on the operand type. Integer ops
/// default to signed semantics (matches MIR's signed-int assumption
/// for the default widths); `unsigned_hint = true` switches `Shr`
/// to logical (`ushr`). Float ops use IEEE-754 semantics and
/// compares use `Ordered` `FloatCC` so NaN propagates to `false`.
fn lower_binop(
    builder: &mut FunctionBuilder<'_>,
    op: BinOp,
    a: ir::Value,
    b: ir::Value,
    unsigned_hint: bool,
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
        } else if a_ty.is_int() && b_ty.is_int() {
            // Integer width mismatch: extend the narrower side up
            // to the wider one. Common cause is a closure capture
            // whose env-stored value was loaded with a wider type
            // than its source bool/i8 width — `if pred(x)` or a
            // comparison whose other operand is a full i64.
            if a_ty.bits() < b_ty.bits() {
                a = builder.ins().sextend(b_ty, a);
                a_ty = b_ty;
            } else {
                b = builder.ins().sextend(a_ty, b);
                b_ty = a_ty;
            }
        } else {
            bail!("native codegen: binop operand type mismatch (op={op:?}, {a_ty:?} vs {b_ty:?})");
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
            BinOp::Ge => fcmp_bool(builder, ir::condcodes::FloatCC::GreaterThanOrEqual, a, b),
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
        BinOp::Shr => {
            if unsigned_hint {
                builder.ins().ushr(a, b)
            } else {
                builder.ins().sshr(a, b)
            }
        }
        BinOp::Eq => compare_bool(builder, ir::condcodes::IntCC::Equal, a, b),
        BinOp::Ne => compare_bool(builder, ir::condcodes::IntCC::NotEqual, a, b),
        BinOp::Lt => compare_bool(builder, ir::condcodes::IntCC::SignedLessThan, a, b),
        BinOp::Le => compare_bool(builder, ir::condcodes::IntCC::SignedLessThanOrEqual, a, b),
        BinOp::Gt => compare_bool(builder, ir::condcodes::IntCC::SignedGreaterThan, a, b),
        BinOp::Ge => compare_bool(
            builder,
            ir::condcodes::IntCC::SignedGreaterThanOrEqual,
            a,
            b,
        ),
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

/// Parses a single shape character produced by the MIR's
/// `mangle_callable_shape` into the cranelift type the thunk
/// uses for that slot. Pointer-shaped slots resolve to the
/// target's pointer type (`I64` on 64-bit hosts, `I32` on 32-bit
/// hosts) so cross-compilation works without further plumbing.
#[allow(
    clippy::needless_pass_by_value,
    reason = "kept by-value to match the upcoming arena-pinned cl-type variant"
)]
fn shape_char_to_cl_type(c: char, _ptr_ty: ir::Type) -> Option<ir::Type> {
    Some(match c {
        'b' | 'y' => types::I8,
        'k' => types::I16,
        'c' | 'j' => types::I32,
        'i' => types::I64,
        'f' => types::F64,
        'g' => types::F32,
        'u' => types::I64,
        _ => return None,
    })
}

/// Defines a per-shape callable thunk function in the cranelift
/// module. The thunk takes `(env: ptr, args...)` matching the
/// FnTrait sig and forwards typed args to the real function at
/// `env + 8` via `call_indirect` with the correct calling
/// convention.
///
/// Replaces the earlier mono-i64 `gos_rt_fn_tramp_N` runtime
/// trampolines, which silently mishandled f64 / bool / aggregate
/// arguments and returns. Each unique shape used in the program
/// gets one thunk; the cache key is the thunk's name (already
/// shape-encoded by `mangle_callable_shape`).
fn define_shape_thunk(
    module: &mut dyn Module,
    intrinsics: &mut IntrinsicContext,
    name: &str,
) -> Result<FuncId> {
    let ptr_ty = module.target_config().pointer_type();
    // Parse the shape encoding: `__fn_thunk_<inputs>_<ret>`.
    let suffix = name
        .strip_prefix("__fn_thunk_")
        .ok_or_else(|| anyhow!("define_shape_thunk: bad name `{name}`"))?;
    let mut split = suffix.rsplitn(2, '_');
    let ret_str = split
        .next()
        .ok_or_else(|| anyhow!("define_shape_thunk: missing ret in `{name}`"))?;
    let inputs_str = split
        .next()
        .ok_or_else(|| anyhow!("define_shape_thunk: missing inputs in `{name}`"))?;
    let mut input_tys: Vec<ir::Type> = Vec::with_capacity(inputs_str.len());
    for c in inputs_str.chars() {
        let t = shape_char_to_cl_type(c, ptr_ty)
            .ok_or_else(|| anyhow!("define_shape_thunk: unknown shape char `{c}` in `{name}`"))?;
        input_tys.push(t);
    }
    let ret_char = ret_str
        .chars()
        .next()
        .ok_or_else(|| anyhow!("define_shape_thunk: empty ret in `{name}`"))?;
    let ret_ty = shape_char_to_cl_type(ret_char, ptr_ty)
        .ok_or_else(|| anyhow!("define_shape_thunk: unknown ret shape `{ret_char}` in `{name}`"))?;
    let unit_ret = ret_char == 'u';
    // Thunk signature: (env: ptr, typed args...) -> typed ret.
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr_ty));
    for t in &input_tys {
        sig.params.push(AbiParam::new(*t));
    }
    if !unit_ret {
        sig.returns.push(AbiParam::new(ret_ty));
    }
    let static_name: &'static str = Box::leak(name.to_string().into_boxed_str());
    let thunk_id = module
        .declare_function(static_name, Linkage::Local, &sig)
        .map_err(|e| anyhow!("declare {static_name}: {e}"))?;
    intrinsics.functions.insert(name.to_string(), thunk_id);
    let mut func = Function::with_name_signature(UserFuncName::user(0, thunk_id.as_u32()), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let env_param = builder.block_params(entry)[0];
        let mut arg_values: Vec<ir::Value> = Vec::with_capacity(input_tys.len());
        for i in 0..input_tys.len() {
            arg_values.push(builder.block_params(entry)[i + 1]);
        }
        // Load the real fn address from env + 8.
        let real_fn_ptr = builder.ins().load(
            ptr_ty,
            MemFlags::trusted(),
            env_param,
            ir::immediates::Offset32::new(8),
        );
        // Build the call_indirect signature with the actual typed
        // args / return — no env, since the real fn is a bare fn
        // item that doesn't take an environment.
        let mut call_sig = module.make_signature();
        for t in &input_tys {
            call_sig.params.push(AbiParam::new(*t));
        }
        if !unit_ret {
            call_sig.returns.push(AbiParam::new(ret_ty));
        }
        let sig_ref = builder.import_signature(call_sig);
        let call = builder
            .ins()
            .call_indirect(sig_ref, real_fn_ptr, &arg_values);
        if unit_ret {
            builder.ins().return_(&[]);
        } else {
            let ret = builder.inst_results(call).first().copied();
            if let Some(v) = ret {
                builder.ins().return_(&[v]);
            } else {
                let zero = builder.ins().iconst(ret_ty, 0);
                builder.ins().return_(&[zero]);
            }
        }
        builder.seal_all_blocks();
        builder.finalize();
    }
    let mut ctx = Context::for_function(func);
    module
        .define_function(thunk_id, &mut ctx)
        .map_err(|e| anyhow!("define {static_name}: {e}"))?;
    Ok(thunk_id)
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
    let mut exit_sig = module.make_signature();
    exit_sig.params.push(AbiParam::new(types::I64));
    exit_sig.returns.push(AbiParam::new(types::I32));
    let exit_code = module
        .declare_function("gos_rt_main_exit_code", Linkage::Import, &exit_sig)
        .map_err(|e| anyhow!("declare exit_code: {e}"))?;
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
        let result_raw = builder.inst_results(call)[0];
        // The body-wide type inferer can narrow `Local::RETURN` to a
        // sub-i64 type (e.g. `i8` when the body's last RETURN store
        // came from a comparison). Coerce up to `exit_code`'s declared
        // i64 parameter so cranelift's verifier is happy regardless.
        let result64 = coerce_arg_to(&mut builder, result_raw, types::I64)
            .unwrap_or_else(|_| builder.ins().iconst(types::I64, 0));
        // Drain the runtime's line-buffered stdout cache so any
        // trailing output (no final `println!`) reaches the
        // terminal before the process exits.
        let flush_ref = module.declare_func_in_func(flush_stdout, builder.func);
        let _ = builder.ins().call(flush_ref, &[]);
        let exit_ref = module.declare_func_in_func(exit_code, builder.func);
        let exit_call = builder.ins().call(exit_ref, &[result64]);
        let result32 = builder.inst_results(exit_call)[0];
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
