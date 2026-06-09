//! Cranelift intrinsic lowering — IO / math / time / OS family.
//!
//! Holds `lower_intrinsic_call_io_math`, the first partition in
//! the cranelift dispatch chain. Covers `gos_rt_print_*`,
//! `gos_rt_eprint*`, `gos_rt_fmt_prec`, `__concat`, the math
//! shims (`sqrt`/`sin`/`cos`/`exp`/`ln`/`abs`/`floor`/`ceil`/`pow`),
//! `gos_rt_time_*`, and the `gos_rt_os_*` / `gos_rt_env_*`
//! entries. See sibling files for the other partitions
//! (`intrinsic_collections`, `intrinsic_handles`,
//! `intrinsic_string`); the four files are walked in declaration
//! order from `intrinsic::lower_intrinsic_call`.

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
//! anything that needs a GC heap are not yet lowered — those
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

pub(super) fn lower_intrinsic_call_io_math(
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
    #![allow(clippy::too_many_lines, clippy::too_many_arguments)]
    let ptr_ty = module.target_config().pointer_type();
    let _ = ptr_ty; // suppress unused if all arms inline
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
                    | PrintKind::VecVecI64
                    | PrintKind::VecVecString => {
                        let helper = match kind {
                            PrintKind::VecI64 => "gos_rt_vec_format_i64",
                            PrintKind::VecF64 => "gos_rt_vec_format_f64",
                            PrintKind::VecBool => "gos_rt_vec_format_bool",
                            PrintKind::VecString => "gos_rt_vec_format_string",
                            PrintKind::VecVecI64 => "gos_rt_vec_format_vec_i64",
                            PrintKind::VecVecString => "gos_rt_vec_format_vec_string",
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
                        let p = intrinsics.static_string_body_ptr(module, builder, placeholder);
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
        "gos_rc_alloc" => {
            // Reference-counted allocator: `gos_rc_alloc(size, meta)`
            // -> ptr with strong count 1. `size` is the payload byte
            // count; `meta` names the module-global child-layout blob
            // (empty name => leaf => null meta).
            let size_val = match args.first() {
                Some(arg) => {
                    lower_operand(module, builder, locals, body, tcx, arg, None, intrinsics)?
                }
                None => builder.ins().iconst(types::I64, 0),
            };
            let size_i64 = if ptr_ty == types::I64 {
                size_val
            } else {
                builder.ins().sextend(types::I64, size_val)
            };
            let meta_val = match args.get(1) {
                Some(Operand::Const(ConstValue::Str(sym))) if !sym.is_empty() => {
                    let Some(blob) = tcx.rc_meta(sym) else {
                        bail!("native codegen: gos_rc_alloc references unknown meta `{sym}`");
                    };
                    let data_id = intrinsics.intern_rc_meta(module, sym, blob)?;
                    let gv = module.declare_data_in_func(data_id, builder.func);
                    builder.ins().symbol_value(ptr_ty, gv)
                }
                _ => builder.ins().iconst(ptr_ty, 0),
            };
            let rc_alloc = intrinsics.extern_fn(
                module,
                "gos_rt_rc_alloc",
                &[types::I64, ptr_ty],
                &[ptr_ty],
            )?;
            let rc_alloc_ref = module.declare_func_in_func(rc_alloc, builder.func);
            let call_inst = builder.ins().call(rc_alloc_ref, &[size_i64, meta_val]);
            let raw_ptr = builder.inst_results(call_inst)[0];
            let as_i64 = if ptr_ty == types::I64 {
                raw_ptr
            } else {
                builder.ins().uextend(types::I64, raw_ptr)
            };
            if !destination.projection.is_empty() {
                bail!("native codegen: gos_rc_alloc destination cannot have projections");
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
        "time::now_ns" | "time::now_nanos" => {
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
        "time::monotonic_ms" | "time::monotonic_nanos" => {
            let rt_name = if name == "time::monotonic_ms" {
                "gos_rt_monotonic_ms"
            } else {
                "gos_rt_monotonic_nanos"
            };
            let rt_fn = intrinsics.extern_fn_by_name(module, rt_name)?;
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
        _ => Ok(false),
    }
}
