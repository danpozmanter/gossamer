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

pub(super) fn lower_rvalue(
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

pub(super) fn lower_operand(
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

pub(super) fn lower_const(
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

pub(super) fn lower_binop(
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
