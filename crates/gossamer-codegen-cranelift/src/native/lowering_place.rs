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

pub(super) fn lower_place_address(
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

pub(super) fn lower_place_store(
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

pub(super) fn lower_first_ptr_arg(
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

pub(super) fn lower_place_read(
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
