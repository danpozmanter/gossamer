//! MIR `Ty` → LLVM IR type string.
//!
//! The emitter works in textual IR so types are rendered as
//! the short strings LLVM expects (`i64`, `double`, `i1`,
//! `ptr`, …). Aggregates that don't fit in a register
//! (strings, slices, arbitrary structs) flow through the
//! runtime as opaque `ptr` — same choice the Cranelift
//! backend makes in `lower_ty`.

use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

/// LLVM type rendering for a MIR type. Returns the short
/// textual form (`i64`, `double`, `i1`, `ptr`, `void`).
pub(crate) fn render_ty(tcx: &TyCtxt, ty: Ty) -> String {
    match tcx.kind(ty) {
        Some(TyKind::Unit) => "void".to_string(),
        Some(TyKind::Bool) => "i1".to_string(),
        Some(TyKind::Int(IntTy::I8 | IntTy::U8)) => "i8".to_string(),
        Some(TyKind::Int(IntTy::I16 | IntTy::U16)) => "i16".to_string(),
        Some(TyKind::Int(IntTy::I32 | IntTy::U32)) => "i32".to_string(),
        Some(TyKind::Int(IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize)) => {
            "i64".to_string()
        }
        Some(TyKind::Int(IntTy::I128 | IntTy::U128)) => "i128".to_string(),
        Some(TyKind::Float(FloatTy::F32)) => "float".to_string(),
        Some(TyKind::Float(FloatTy::F64)) => "double".to_string(),
        Some(TyKind::Char) => "i32".to_string(),
        Some(TyKind::String) => "ptr".to_string(),
        Some(TyKind::Ref { .. }) => "ptr".to_string(),
        Some(TyKind::FnPtr(_) | TyKind::FnDef { .. }) => "ptr".to_string(),
        Some(
            TyKind::Array { .. }
            | TyKind::Slice(_)
            | TyKind::Vec(_)
            | TyKind::Adt { .. }
            | TyKind::Tuple(_)
            | TyKind::Dyn(_)
            | TyKind::HashMap { .. }
            | TyKind::Sender(_)
            | TyKind::Receiver(_),
        ) => "ptr".to_string(),
        // `Never` / `Error` / `Var` / `Param` / `Closure` /
        // `Alias` — treated as opaque pointers by the runtime
        // so the backend can still emit a signature that
        // typechecks.
        _ => "ptr".to_string(),
    }
}

/// Convenience: returns `true` when the type is `()`, i.e.
/// should be elided in LLVM (no return value, no parameter).
pub(crate) fn is_unit(tcx: &TyCtxt, ty: Ty) -> bool {
    matches!(tcx.kind(ty), Some(TyKind::Unit))
}

/// Returns the LLVM IR integer width for an integer type,
/// used by `Cast` to pick `trunc` / `zext` / `sext`.
pub(crate) fn int_width(int_ty: IntTy) -> u32 {
    match int_ty {
        IntTy::I8 | IntTy::U8 => 8,
        IntTy::I16 | IntTy::U16 => 16,
        IntTy::I32 | IntTy::U32 => 32,
        IntTy::I64 | IntTy::U64 | IntTy::Isize | IntTy::Usize => 64,
        IntTy::I128 | IntTy::U128 => 128,
    }
}

/// Returns `true` when the integer type is signed — controls
/// `sdiv`/`udiv`, `srem`/`urem`, `icmp slt` vs `icmp ult`
/// selection.
pub(crate) fn int_signed(int_ty: IntTy) -> bool {
    matches!(
        int_ty,
        IntTy::I8 | IntTy::I16 | IntTy::I32 | IntTy::I64 | IntTy::I128 | IntTy::Isize
    )
}

/// Classifies the numeric family of a [`Ty`] for `BinaryOp`
/// dispatch (int vs float vs other).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumericKind {
    Int(IntTy),
    Float(FloatTy),
    Other,
}

pub(crate) fn numeric_kind(tcx: &TyCtxt, ty: Ty) -> NumericKind {
    match tcx.kind(ty) {
        Some(TyKind::Int(i)) => NumericKind::Int(*i),
        Some(TyKind::Float(f)) => NumericKind::Float(*f),
        _ => NumericKind::Other,
    }
}

/// Size in 8-byte slots of a `Ty` when it's laid out as a
/// flat aggregate (matches what the Cranelift backend does —
/// every scalar field takes one i64-wide slot, structs /
/// tuples chain their fields, arrays stride by
/// `elem_count × elem_slots`). Scalars / opaque pointers
/// count as 1. When the shape isn't statically determinable
/// (an inference variable, an unknown `Adt` def) we return
/// `None` so the caller can fall back to scalar handling.
pub(crate) fn slot_count(tcx: &TyCtxt, ty: Ty) -> Option<u32> {
    match tcx.kind(ty)? {
        TyKind::Unit => Some(0),
        TyKind::Bool
        | TyKind::Char
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::String
        | TyKind::Ref { .. }
        | TyKind::FnPtr(_)
        | TyKind::FnDef { .. }
        | TyKind::Slice(_)
        | TyKind::Vec(_)
        | TyKind::HashMap { .. }
        | TyKind::Sender(_)
        | TyKind::Receiver(_) => Some(1),
        TyKind::Tuple(elems) => {
            let mut total = 0u32;
            for e in elems {
                total += slot_count(tcx, *e)?;
            }
            Some(total)
        }
        TyKind::Array { elem, len } => {
            // An array whose element type didn't resolve (e.g. the
            // typechecker leaked a `Var(...)`) still has a known
            // length. Assume the element is scalar (1 slot) instead
            // of returning `None`, which collapses the alloca to a
            // single i64 slot and makes a 3-element array literal
            // overflow into adjacent locals.
            let elem_slots = slot_count(tcx, *elem).unwrap_or(1).max(1);
            Some(elem_slots * (*len as u32))
        }
        TyKind::Adt { def, .. } => {
            let field_tys = tcx.struct_field_tys(*def)?;
            let mut total = 0u32;
            for t in field_tys {
                total += slot_count(tcx, *t)?;
            }
            Some(total)
        }
        _ => None,
    }
}

/// Size in slots of a *single element* of an aggregate type —
/// 1 for scalar arrays, `fields.len()` for arrays of structs,
/// used to compute the array stride when lowering
/// `a[i].field` projections.
pub(crate) fn elem_slots(tcx: &TyCtxt, ty: Ty) -> u32 {
    match tcx.kind(ty) {
        Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
            slot_count(tcx, *elem).unwrap_or(1)
        }
        _ => 1,
    }
}

/// Returns `true` when `ty` and every transitive component is a
/// primitive scalar (`bool`, integers, floats, `char`) — i.e. the
/// type contains no pointer to arena-allocated heap data. Used by
/// the call-site arena scoping pass to decide whether wrapping a
/// call with `gos_rt_arena_save`/`gos_rt_arena_restore` is safe:
/// after the callee's return aggregate is `memcpy`'d into the
/// caller's stack alloca, restoring the arena watermark cannot
/// dangle any reference.
///
/// Anything containing a `Vec<T>`, `String`, `HashMap<K,V>`, or a
/// channel handle is rejected because those carry pointers into
/// arena-managed memory; restoring the arena would free their
/// backing storage and leave the copied pointer dangling. References
/// (`&T`) are also rejected — they point at storage outside the
/// caller's slot, which the arena reset may invalidate.
pub(crate) fn is_pure_primitive_aggregate(tcx: &TyCtxt, ty: Ty) -> bool {
    match tcx.kind(ty) {
        Some(TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_) | TyKind::Unit) => {
            true
        }
        Some(TyKind::Array { elem, .. }) => is_pure_primitive_aggregate(tcx, *elem),
        Some(TyKind::Tuple(elems)) => elems.iter().all(|t| is_pure_primitive_aggregate(tcx, *t)),
        Some(TyKind::Adt { def, .. }) => {
            // Reject the Result/Option sentinel Adts up front —
            // they are pointer-shaped and not really aggregates.
            if def.local == u32::MAX || def.local == u32::MAX - 1 {
                return false;
            }
            match tcx.struct_field_tys(*def) {
                Some(fields) => fields.iter().all(|t| is_pure_primitive_aggregate(tcx, *t)),
                None => false,
            }
        }
        _ => false,
    }
}

/// True when the type is an aggregate whose memory lives in a
/// stack slot rather than a scalar SSA value. Drives the
/// choice between a scalar `alloca <ty>` and an aggregate
/// `alloca [N x i64]`.
pub(crate) fn is_aggregate(tcx: &TyCtxt, ty: Ty) -> bool {
    if let Some(TyKind::Adt { def, .. }) = tcx.kind(ty) {
        // Result/Option sentinel Adts (DefId::local == u32::MAX or
        // u32::MAX - 1) are heap-allocated `*mut GosResult` values
        // returned from runtime helpers. Treating them as flat-slot
        // aggregates here makes `emit_named_call` memcpy the first
        // 8 bytes of the runtime's 16-byte struct into a
        // `[1 x i64]` alloca and then pass `ptr %alloca` to the
        // next helper — which reads stack garbage as the payload.
        // Treat them as scalar `ptr`s so the caller stores the
        // returned pointer directly into the local slot.
        if def.local == u32::MAX || def.local == u32::MAX - 1 {
            return false;
        }
    }
    matches!(
        tcx.kind(ty),
        Some(TyKind::Array { .. } | TyKind::Tuple(_) | TyKind::Adt { .. })
    )
}
