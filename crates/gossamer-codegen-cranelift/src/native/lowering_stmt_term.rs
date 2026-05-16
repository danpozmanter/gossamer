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

pub(super) fn lower_statement(
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

pub(super) fn lower_terminator(
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
            // Loop-back-edge safepoint elided — see the matching
            // comment in the block-prefix lowering. Allocation-
            // driven safepoint dispatch keeps the collector
            // responsive without an opaque runtime call on every
            // iteration that would block inner-loop vectorisation.
            let _ = src_block;
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
            // Skipped when the prologue was elided (body can't
            // allocate, so no roots were pushed); the call would be
            // a no-op but its FFI overhead dominates leaf-math
            // functions.
            if gossamer_mir::body_might_allocate(body) {
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
