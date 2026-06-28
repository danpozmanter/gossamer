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

pub(super) fn stride_slots_from_ty(tcx: &TyCtxt, ty: Ty) -> Option<u32> {
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

pub(super) fn type_slot_count(tcx: &TyCtxt, ty: Ty) -> u32 {
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems
            .iter()
            .map(|t| type_slot_count(tcx, *t))
            .sum::<u32>()
            .max(1),
        TyKind::Array { elem, len } => u32::try_from(len.to_usize())
            .unwrap_or(1)
            .saturating_mul(type_slot_count(tcx, elem)),
        TyKind::Adt { def, substs } => {
            // Result<T, E> and Option<T> use sentinel DefIds
            // (u32::MAX, u32::MAX-1) that don't appear in
            // `struct_field_tys`. Both have a 2-slot heap layout:
            // `[disc: i64, payload: i64]`. Without this special
            // case the by-value-aggregate return path copies only
            // the disc word and zeroes the payload - corrupting
            // every `Ok(v)` returned across a function boundary.
            if def.local == u32::MAX || def.local == u32::MAX - 1 || tcx.is_inline_enum_ty(ty) {
                return 2;
            }
            tcx.adt_field_tys(def, &substs).map_or(1, |tys| {
                tys.iter()
                    .map(|t| type_slot_count(tcx, *t))
                    .sum::<u32>()
                    .max(1)
            })
        }
        _ => 1,
    }
}

pub(super) fn field_byte_offset(tcx: &TyCtxt, ty: Ty, idx: u32) -> u32 {
    let target = idx as usize;
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems
            .iter()
            .take(target)
            .map(|t| type_slot_count(tcx, *t))
            .sum::<u32>()
            .saturating_mul(8),
        TyKind::Adt { def, substs } => {
            // Sentinels for Result/Option use a flat 2-slot
            // [disc, payload] layout where each field is one slot.
            if def.local == u32::MAX || def.local == u32::MAX - 1 || tcx.is_inline_enum_ty(ty) {
                return idx * 8;
            }
            tcx.adt_field_tys(def, &substs).map_or(idx * 8, |tys| {
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

pub(super) fn field_ty_at(tcx: &TyCtxt, ty: Ty, idx: u32) -> Option<Ty> {
    let target = idx as usize;
    match tcx.kind_of(ty).clone() {
        TyKind::Tuple(elems) => elems.get(target).copied(),
        TyKind::Adt { def, substs } => {
            if def.local == u32::MAX || def.local == u32::MAX - 1 || tcx.is_inline_enum_ty(ty) {
                return None;
            }
            tcx.adt_field_tys(def, &substs)
                .and_then(|tys| tys.get(target).copied())
        }
        TyKind::Ref { inner, .. } => field_ty_at(tcx, inner, idx),
        _ => None,
    }
}

pub(super) fn mir_ty_to_cabi(
    tcx: &TyCtxt,
    ty: gossamer_types::Ty,
    ptr_ty: ir::Type,
) -> Option<ir::Type> {
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
