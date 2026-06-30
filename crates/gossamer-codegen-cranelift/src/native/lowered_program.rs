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

use super::*;

use super::*;

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

pub(super) fn resolve_callee(
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
            // Unknown DefId - fall back to a "missing-fn" stub so
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

pub(super) fn i64_truncate(n: i128) -> i64 {
    n as i64
}

pub(super) fn compare_bool(
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

pub(super) fn fcmp_bool(
    builder: &mut FunctionBuilder<'_>,
    cc: ir::condcodes::FloatCC,
    a: ir::Value,
    b: ir::Value,
) -> ir::Value {
    builder.ins().fcmp(cc, a, b)
}

pub(super) fn shape_char_to_cl_type(c: char, _ptr_ty: ir::Type) -> Option<ir::Type> {
    Some(match c {
        'b' | 'y' => types::I8,
        'k' => types::I16,
        'c' | 'j' => types::I32,
        'i' => types::I64,
        'f' => types::F64,
        'g' => types::F32,
        'u' => types::I64,
        // 2-word packed Result/Option.
        'r' => types::I128,
        _ => return None,
    })
}

pub(super) fn define_shape_thunk(
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
        // args / return - no env, since the real fn is a bare fn
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

/// Emits an out-pointer wrapper for a `Result<Enum, _>`-returning body.
///
/// The body returns its two-word `[disc, payload]` carrier by value as an
/// `i128`. Within Cranelift-compiled code the `i128` return register
/// convention is self-consistent on every target, but a Rust
/// `extern "C" fn(..) -> i128` trampoline reads that return from a
/// different register than the body writes on Windows x64, so the
/// in-process JIT cannot read the carrier by value there. This thunk calls
/// the body (a Cranelift-to-Cranelift call, so both sides agree) and stores
/// the carrier through `out`, a plain pointer argument whose ABI is
/// identical on every target. The trampoline calls the thunk with a stack
/// buffer and reads `out[0]` (disc) / `out[1]` (payload) back from memory.
pub(crate) fn emit_carrier_outptr_thunk(
    module: &mut dyn Module,
    body_id: FuncId,
    body_name: &str,
) -> Result<FuncId> {
    let ptr_ty = module.target_config().pointer_type();
    let body_sig = module
        .declarations()
        .get_function_decl(body_id)
        .signature
        .clone();
    let param_tys: Vec<ir::Type> = body_sig.params.iter().map(|p| p.value_type).collect();
    // Thunk signature: (out: *mut i128, <body params>) -> ()
    let mut sig = module.make_signature();
    sig.params.push(AbiParam::new(ptr_ty));
    for t in &param_tys {
        sig.params.push(AbiParam::new(*t));
    }
    let static_name: &'static str =
        Box::leak(format!("__gos_jit_carrier_{body_name}").into_boxed_str());
    let thunk_id = module
        .declare_function(static_name, Linkage::Local, &sig)
        .map_err(|e| anyhow!("declare {static_name}: {e}"))?;
    let mut func = Function::with_name_signature(UserFuncName::user(0, thunk_id.as_u32()), sig);
    let mut fb_ctx = FunctionBuilderContext::new();
    {
        let mut builder = FunctionBuilder::new(&mut func, &mut fb_ctx);
        let entry = builder.create_block();
        builder.append_block_params_for_function_params(entry);
        builder.switch_to_block(entry);
        let out_ptr = builder.block_params(entry)[0];
        let args: Vec<ir::Value> = (0..param_tys.len())
            .map(|i| builder.block_params(entry)[i + 1])
            .collect();
        let body_ref = module.declare_func_in_func(body_id, builder.func);
        let call = builder.ins().call(body_ref, &args);
        let carrier = builder.inst_results(call)[0];
        // Split the carrier into its two 64-bit words and store each
        // separately, rather than a single `i128` store: the disc word at
        // +0, the payload word at +8 - the layout the trampoline reads back
        // as `out[0]` / `out[1]`. Two plain `i64` stores avoid relying on the
        // backend's 128-bit memory-access lowering.
        let disc = builder.ins().ireduce(types::I64, carrier);
        let high = builder.ins().ushr_imm(carrier, 64);
        let payload = builder.ins().ireduce(types::I64, high);
        builder.ins().store(
            MemFlags::new(),
            disc,
            out_ptr,
            ir::immediates::Offset32::new(0),
        );
        builder.ins().store(
            MemFlags::new(),
            payload,
            out_ptr,
            ir::immediates::Offset32::new(8),
        );
        builder.ins().return_(&[]);
        builder.seal_all_blocks();
        builder.finalize();
    }
    let mut ctx = Context::for_function(func);
    module
        .define_function(thunk_id, &mut ctx)
        .map_err(|e| anyhow!("define {static_name}: {e}"))?;
    Ok(thunk_id)
}

pub(super) fn emit_c_main_shim(module: &mut dyn Module, gos_main: FuncId) -> Result<()> {
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
