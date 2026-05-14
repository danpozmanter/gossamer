//! GC write-barrier insertion pass.
//!
//! Walks every `Statement::Assign` whose destination is a *projected*
//! place (field write, index write, deref write) and whose rvalue
//! produces a heap-traceable reference. After each such assign, inserts
//! a [`StatementKind::GcWriteBarrier`] so the concurrent collector's
//! mark phase greys the new target before the mutator can hide it
//! from the marker (classic insertion / Dijkstra barrier).
//!
//! Bare-local writes (`_l = rvalue`) skip the barrier: a freshly
//! produced local is not yet reachable from a black object, so the
//! invariant the barrier protects is intact without it.

#![forbid(unsafe_code)]

use gossamer_types::{Ty, TyCtxt, TyKind};

use crate::ir::{Body, Operand, Place, Projection, Rvalue, Statement, StatementKind};

/// Inserts GC write barriers after every projected pointer-store in
/// `body`. Idempotent: re-running is a no-op once every required
/// barrier already follows its store. Linear in statement count.
pub fn insert_gc_barriers(body: &mut Body, tcx: &TyCtxt) {
    for block_idx in 0..body.blocks.len() {
        // Two-pass per block: collect insertion points first, then
        // splice them in so the loop indices don't shift mid-pass.
        // `(insert_after_idx, barrier_stmt)` pairs are sorted in
        // discovery order; we splice them in reverse so each
        // splice's index remains valid.
        let mut to_insert: Vec<(usize, Statement)> = Vec::new();
        let block = &body.blocks[block_idx];
        for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.projection.is_empty() {
                continue;
            }
            // A heap-pointer store happens when the rvalue produces
            // a reference value AND the projected leaf is the same
            // shape. Both halves are required: storing an i64 into
            // a `[i64]` slot doesn't need a barrier, and so doesn't
            // a non-aggregate destination.
            if !rvalue_produces_pointer(tcx, body, rvalue) {
                continue;
            }
            // Skip if the very next statement is already a matching
            // barrier (idempotency).
            if let Some(next) = block.stmts.get(stmt_idx + 1) {
                if matches!(&next.kind, StatementKind::GcWriteBarrier { .. }) {
                    continue;
                }
            }
            // The barrier records the target (the value being
            // stored). For Use(Copy(p)) we pass the same operand;
            // for the other shapes we'd need a fresh local to
            // capture the rvalue's result — but the dominant heap
            // store in user code is `_aggregate.field = source`,
            // which the typechecker normalises into
            // `_aggregate.field = Use(Copy(source))`. Skip other
            // shapes for now; they can be added incrementally as
            // the collector hits them.
            let Some(target) = pointer_target_of(rvalue) else {
                continue;
            };
            to_insert.push((
                stmt_idx,
                Statement {
                    kind: StatementKind::GcWriteBarrier {
                        place: place.clone(),
                        value: target,
                    },
                    span: stmt.span,
                },
            ));
        }
        let block = &mut body.blocks[block_idx];
        for (idx, barrier) in to_insert.into_iter().rev() {
            block.stmts.insert(idx + 1, barrier);
        }
    }
}

/// Returns `Some(target_operand)` when the rvalue produces a single
/// heap-pointer value we can pass to the barrier. The cases that map
/// cleanly: `Use(Copy/Const/FnRef)` — the operand IS the produced
/// value. Other rvalue shapes (`BinaryOp`, `Aggregate`, …) either don't
/// produce pointer values (`BinaryOp` arithmetic) or build them inline
/// via constructor sequences we don't intercept here.
fn pointer_target_of(rvalue: &Rvalue) -> Option<Operand> {
    match rvalue {
        Rvalue::Use(op) => Some(op.clone()),
        _ => None,
    }
}

/// `true` when the rvalue's result type renders as a heap-traceable
/// reference. Drives the per-statement barrier decision.
fn rvalue_produces_pointer(tcx: &TyCtxt, body: &Body, rvalue: &Rvalue) -> bool {
    match rvalue {
        Rvalue::Use(op) => operand_is_pointer(tcx, body, op),
        _ => false,
    }
}

fn operand_is_pointer(tcx: &TyCtxt, body: &Body, op: &Operand) -> bool {
    let ty = match op {
        Operand::Copy(place) => place_leaf_ty(tcx, body, place),
        // String literals are heap c-strings; FnRef captures a
        // function pointer (not a GC ref). Const-int/-float/-bool
        // aren't references.
        Operand::Const(crate::ir::ConstValue::Str(_)) => return true,
        Operand::Const(_) => return false,
        Operand::FnRef { .. } => return false,
    };
    ty_is_pointer(tcx, ty)
}

fn place_leaf_ty(tcx: &TyCtxt, body: &Body, place: &Place) -> Ty {
    let mut ty = body.local_ty(place.local);
    for projection in &place.projection {
        ty = match projection {
            Projection::Field(idx) => field_ty_at(tcx, ty, *idx).unwrap_or(ty),
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

fn field_ty_at(tcx: &TyCtxt, ty: Ty, idx: u32) -> Option<Ty> {
    match tcx.kind_of(ty) {
        TyKind::Tuple(elems) => elems.get(idx as usize).copied(),
        TyKind::Adt { def, .. } => tcx
            .struct_field_tys(*def)
            .and_then(|tys| tys.get(idx as usize).copied()),
        _ => None,
    }
}

/// `true` if a runtime value of `ty` is a heap reference the
/// concurrent collector must trace. Mirrors the operand-kind decision
/// the backends already use to filter values into the barrier path.
fn ty_is_pointer(tcx: &TyCtxt, ty: Ty) -> bool {
    match tcx.kind_of(ty) {
        TyKind::String
        | TyKind::Ref { .. }
        | TyKind::Slice(_)
        | TyKind::Vec(_)
        | TyKind::HashMap { .. }
        | TyKind::Sender(_)
        | TyKind::Receiver(_)
        | TyKind::JsonValue
        | TyKind::DynError
        | TyKind::Closure { .. }
        | TyKind::FnTrait(_)
        | TyKind::Dyn(_) => true,
        TyKind::Tuple(elems) => elems.iter().any(|t| ty_is_pointer(tcx, *t)),
        TyKind::Array { elem, .. } => ty_is_pointer(tcx, *elem),
        TyKind::Adt { def, .. } => {
            // Sentinel Adts (Result / Option with u32::MAX / MAX-1
            // DefIds) are heap pointers themselves.
            if def.local == u32::MAX || def.local == u32::MAX - 1 {
                return true;
            }
            tcx.struct_field_tys(*def)
                .is_some_and(|tys| tys.iter().any(|t| ty_is_pointer(tcx, *t)))
        }
        _ => false,
    }
}

// Unit-test scaffolding is awkward inside this crate (TyCtxt setup
// needs a real SourceMap + resolver). The pass is exercised
// end-to-end by `crates/gossamer-mir/tests/gc_barrier.rs` integration
// tests instead.
