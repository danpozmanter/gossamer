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

pub(super) fn operand_locals(rvalue: &Rvalue) -> Vec<Local> {
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

pub(super) enum PrintKind {
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
    /// `Vec<Vec<String>>` formatted via `gos_rt_vec_format_vec_string`.
    VecVecString,
    /// `[i64; N]` flat-buffer literal (no GosVec header). Formatted
    /// via `gos_rt_arr_format_i64(ptr, len)`.
    ArrI64(i64),
    /// `[f64; N]` flat-buffer literal.
    ArrF64(i64),
    /// `[bool; N]` flat-buffer literal.
    ArrBool(i64),
    /// `[String; N]` flat-buffer literal.
    ArrString(i64),
    /// `json::Value` - rendered via `gos_rt_json_render`.
    JsonValue,
    /// `errors::Error` - calls `gos_rt_error_message` then prints as string.
    ErrorMessage,
    /// A tuple of scalar elements - rendered via `gos_rt_tuple_format`
    /// with a per-element tag array computed from the element types.
    Tuple,
    /// A scalar-keyed, scalar/string-valued `HashMap` - rendered via
    /// `gos_rt_map_format`.
    Map,
    Unsupported(&'static str),
}

/// Per-element tag for `gos_rt_tuple_format`, or `None` when the
/// element type can't be rendered straight from a raw 8-byte tuple
/// slot. Integers are restricted to 64-bit width and floats to `f64`
/// (a narrower scalar writes fewer than 8 bytes into its slot, so
/// reading the slot back as an i64 / f64 bit pattern would pick up
/// adjacent bytes); `bool` (low bit) and `char` (low 32 bits) read
/// through a mask, so both are safe.
fn tuple_elem_tag(tcx: &TyCtxt, ty: Ty) -> Option<u8> {
    let mut ty = ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
        ty = *inner;
    }
    match tcx.kind_of(ty) {
        TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize) => Some(0),
        TyKind::Duration | TyKind::Instant => Some(0),
        TyKind::Float(FloatTy::F64) => Some(2),
        TyKind::Bool => Some(3),
        TyKind::Char => Some(4),
        TyKind::String => Some(5),
        _ => None,
    }
}

/// The per-element tag array for a tuple operand, or `None` when any
/// element type isn't formattable from a flat slot. Drives both the
/// `PrintKind::Tuple` gate and the emit-time tag blob.
pub(super) fn tuple_tags(tcx: &TyCtxt, body: &Body, operand: &Operand) -> Option<Vec<u8>> {
    let Operand::Copy(place) = operand else {
        return None;
    };
    let mut ty = resolve_place_ty(tcx, body, place);
    while let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
        ty = *inner;
    }
    let TyKind::Tuple(elems) = tcx.kind_of(ty) else {
        return None;
    };
    if elems.is_empty() {
        return None;
    }
    elems.iter().map(|e| tuple_elem_tag(tcx, *e)).collect()
}

/// True when a `HashMap` key/value type is one `gos_rt_map_format`
/// renders from its live storage: an integer (signed decimal, like
/// the VM) or a `String` (bare).
fn map_kv_supported(tcx: &TyCtxt, ty: Ty) -> bool {
    let mut ty = ty;
    while let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
        ty = *inner;
    }
    matches!(tcx.kind_of(ty), TyKind::Int(_) | TyKind::String)
}

pub(super) fn operand_cl_type(
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

pub(super) fn operand_print_kind(body: &Body, tcx: &TyCtxt, operand: &Operand) -> PrintKind {
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
                    // Every ≤64-bit int lives as a signed i64 at
                    // runtime and prints signed - the VM renders
                    // `0u64 - 1` as `-1`. The one exception the VM
                    // makes is display provenance: an explicit
                    // `as u64`/`as usize` cast result becomes
                    // `Value::Uint` and prints unsigned. Mirror that
                    // statically: a local prints unsigned only when
                    // all its writers are such casts. u128 keeps the
                    // unsigned printer outright.
                    let uint_provenance = matches!(int_ty, IntTy::U64 | IntTy::Usize)
                        && place.projection.is_empty()
                        && gossamer_mir::local_is_uint_cast(body, tcx, place.local);
                    if uint_provenance || matches!(int_ty, IntTy::U128) {
                        PrintKind::Uint
                    } else {
                        PrintKind::Int
                    }
                }
                // `time::Duration` / `time::Instant` are transparent
                // `i64`s; print the millisecond count they carry.
                TyKind::Unit | TyKind::Never | TyKind::Duration | TyKind::Instant => PrintKind::Int,
                TyKind::Float(_) => PrintKind::Float,
                TyKind::String | TyKind::Ref { .. } => PrintKind::StrPtr,
                // `Var(_)` means the typechecker did not resolve
                // this operand's type. The dominant producer of
                // unresolved-typed locals that flow into println
                // is `__concat` (whose return type is currently
                // not pinned by the typechecker - it returns a
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
                TyKind::Tuple(_) => {
                    if tuple_tags(tcx, body, operand).is_some() {
                        PrintKind::Tuple
                    } else {
                        PrintKind::Unsupported("tuple")
                    }
                }
                // Fixed-size arrays: flat slot storage. The runtime
                // helpers that print `VecI64` / `VecF64` / etc. read
                // a `*mut GosVec` header, but a fixed array is just
                // a stack slot of `N * elem_bytes`. Routing fixed
                // arrays through the same Vec print kind works
                // because `nums` ends up as a `*mut GosVec` after
                // the typed-array promotion (`BuildIntArray` etc.)
                // - without this, `let nums = [1, 2, 3]; println!
                // ("{:?}", nums)` printed `<value>` even though
                // the array is fully typed and the helper exists.
                TyKind::Array { elem, len } => {
                    let n = i64::try_from(len.to_usize()).unwrap_or(0);
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
                        TyKind::String => PrintKind::VecVecString,
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
                        TyKind::String => PrintKind::VecVecString,
                        _ => PrintKind::Unsupported("nested Vec"),
                    },
                    _ => PrintKind::Unsupported("Vec"),
                },
                TyKind::HashMap { key, value } => {
                    if map_kv_supported(tcx, *key) && map_kv_supported(tcx, *value) {
                        PrintKind::Map
                    } else {
                        PrintKind::Unsupported("HashMap")
                    }
                }
                TyKind::Sender(_) | TyKind::Receiver(_) | TyKind::JoinHandle(_) => {
                    PrintKind::Unsupported("channel")
                }
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

pub(super) fn operand_is_string(tcx: &TyCtxt, body: &Body, operand: &Operand) -> bool {
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

pub(super) fn operand_is_char(body: &Body, tcx: &TyCtxt, op: &Operand) -> bool {
    match op {
        Operand::Const(ConstValue::Char(_)) => true,
        Operand::Copy(p) => matches!(tcx.kind_of(body.local_ty(p.local)), TyKind::Char),
        _ => false,
    }
}

pub(super) fn operand_aggregate_slots(body: &Body, tcx: &TyCtxt, op: &Operand) -> Option<u32> {
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

pub(super) fn operand_cabi_ty(
    operand: &Operand,
    body: &Body,
    tcx: &TyCtxt,
    ptr_ty: ir::Type,
) -> ir::Type {
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
