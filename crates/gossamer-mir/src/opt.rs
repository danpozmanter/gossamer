//! Simple MIR optimisations.
//! Commits to three lightweight passes: constant folding,
//! copy propagation, and dead-store elimination. Each pass is
//! idempotent so callers can run them in any order.

#![forbid(unsafe_code)]
#![allow(clippy::match_same_arms)]

use std::collections::HashMap;

use gossamer_types::{TyCtxt, TyKind};

use crate::ir::{
    BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, Statement, StatementKind,
    Terminator,
};

/// Runs the full optimisation pipeline on `body`. Copy propagation
/// runs before constant folding so that temporaries introduced by the
/// lowerer (`tmp = Const(1); out = BinaryOp(Copy(tmp), ...)`) collapse
/// into the two-constant form folding recognises. A second copy-prop
/// pass after folding propagates the newly-created constants.
pub fn optimise(body: &mut Body, tcx: &TyCtxt) {
    copy_propagate(body, tcx);
    const_fold(body);
    copy_propagate(body, tcx);
    const_branch_elim(body);
    dead_store_elim(body);
}

/// Replaces `SwitchInt` terminators whose discriminant is a known
/// constant with a direct `Goto` to the matching target. Runs after
/// constant folding so that simple `if false { ... } else { ... }`
/// branches fold away entirely. Stream E.2.
pub fn const_branch_elim(body: &mut Body) {
    use crate::ir::Terminator;
    let const_locals: HashMap<u32, i128> = const_int_locals(body);
    for block in &mut body.blocks {
        let Terminator::SwitchInt {
            discriminant,
            arms,
            default,
        } = &block.terminator
        else {
            continue;
        };
        let known = match discriminant {
            Operand::Const(ConstValue::Int(n)) => Some(*n),
            Operand::Const(ConstValue::Bool(b)) => Some(i128::from(*b)),
            Operand::Copy(place) => {
                if place.projection.is_empty() {
                    const_locals.get(&place.local.0).copied()
                } else {
                    None
                }
            }
            _ => None,
        };
        let Some(value) = known else { continue };
        let value_i128 = value;
        let mut target = *default;
        for (arm_value, arm_target) in arms {
            if *arm_value == value_i128 {
                target = *arm_target;
                break;
            }
        }
        block.terminator = Terminator::Goto { target };
    }
}

fn const_int_locals(body: &Body) -> HashMap<u32, i128> {
    // A local is treated as a known constant only when *every*
    // store to it (across all blocks) writes the same constant
    // value — otherwise control-flow-sensitive code such as
    // `let mut neg = false; if cond { neg = true }; if neg { ... }`
    // would mistake the second assignment for unconditional and
    // collapse the second `if` into a direct goto, miscompiling
    // the conditional branch.
    let mut candidates: HashMap<u32, Option<i128>> = HashMap::new();
    let mut tainted: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for block in &body.blocks {
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if !place.projection.is_empty() {
                continue;
            }
            let local_id = place.local.0;
            if tainted.contains(&local_id) {
                continue;
            }
            let value = match rvalue {
                Rvalue::Use(Operand::Const(ConstValue::Int(n))) => Some(*n),
                Rvalue::Use(Operand::Const(ConstValue::Bool(b))) => Some(i128::from(*b)),
                _ => None,
            };
            match (value, candidates.get(&local_id).copied()) {
                (None, _) => {
                    tainted.insert(local_id);
                    candidates.remove(&local_id);
                }
                (Some(v), None) => {
                    candidates.insert(local_id, Some(v));
                }
                (Some(v), Some(Some(prev))) if prev == v => {}
                _ => {
                    tainted.insert(local_id);
                    candidates.remove(&local_id);
                }
            }
        }
    }
    candidates
        .into_iter()
        .filter_map(|(k, v)| v.map(|n| (k, n)))
        .collect()
}

/// Folds `BinaryOp` / `UnaryOp` rvalues whose operands are both
/// [`Operand::Const`].
pub fn const_fold(body: &mut Body) {
    for block in &mut body.blocks {
        for stmt in &mut block.stmts {
            if let StatementKind::Assign {
                rvalue: ref mut rv, ..
            } = stmt.kind
            {
                if let Some(folded) = try_fold(rv) {
                    *rv = Rvalue::Use(Operand::Const(folded));
                }
            }
        }
    }
}

fn try_fold(rvalue: &Rvalue) -> Option<ConstValue> {
    match rvalue {
        Rvalue::BinaryOp {
            op,
            lhs: Operand::Const(a),
            rhs: Operand::Const(b),
        } => fold_binary(*op, a, b),
        Rvalue::UnaryOp {
            op,
            operand: Operand::Const(c),
        } => fold_unary(*op, c),
        _ => None,
    }
}

fn fold_binary(op: BinOp, lhs: &ConstValue, rhs: &ConstValue) -> Option<ConstValue> {
    match (lhs, rhs) {
        (ConstValue::Int(x), ConstValue::Int(y)) => match op {
            BinOp::Add => Some(ConstValue::Int(x.wrapping_add(*y))),
            BinOp::Sub => Some(ConstValue::Int(x.wrapping_sub(*y))),
            BinOp::Mul => Some(ConstValue::Int(x.wrapping_mul(*y))),
            BinOp::Div if *y != 0 => Some(ConstValue::Int(x.wrapping_div(*y))),
            BinOp::Rem if *y != 0 => Some(ConstValue::Int(x.wrapping_rem(*y))),
            BinOp::BitAnd => Some(ConstValue::Int(x & y)),
            BinOp::BitOr => Some(ConstValue::Int(x | y)),
            BinOp::BitXor => Some(ConstValue::Int(x ^ y)),
            BinOp::Eq => Some(ConstValue::Bool(x == y)),
            BinOp::Ne => Some(ConstValue::Bool(x != y)),
            BinOp::Lt => Some(ConstValue::Bool(x < y)),
            BinOp::Le => Some(ConstValue::Bool(x <= y)),
            BinOp::Gt => Some(ConstValue::Bool(x > y)),
            BinOp::Ge => Some(ConstValue::Bool(x >= y)),
            _ => None,
        },
        (ConstValue::Bool(x), ConstValue::Bool(y)) => match op {
            BinOp::Eq => Some(ConstValue::Bool(x == y)),
            BinOp::Ne => Some(ConstValue::Bool(x != y)),
            BinOp::BitAnd => Some(ConstValue::Bool(*x && *y)),
            BinOp::BitOr => Some(ConstValue::Bool(*x || *y)),
            BinOp::BitXor => Some(ConstValue::Bool(x ^ y)),
            _ => None,
        },
        _ => None,
    }
}

fn fold_unary(op: crate::ir::UnOp, operand: &ConstValue) -> Option<ConstValue> {
    match (op, operand) {
        (crate::ir::UnOp::Neg, ConstValue::Int(x)) => Some(ConstValue::Int(-x)),
        (crate::ir::UnOp::Not, ConstValue::Bool(b)) => Some(ConstValue::Bool(!b)),
        _ => None,
    }
}

/// Replaces `Copy(place)` operands with the rvalue that flowed into
/// the place, when that rvalue is itself a `Use(Const|Copy)`. Operates
/// block-local only.
///
/// Aggregate locals (`Array`/`Tuple`/`Adt`) are excluded from
/// propagation: an assignment `_X = Use(Copy(_Y))` for an aggregate
/// is a memcpy between distinct storage slots, not an alias. Forwarding
/// `Copy(_X)` to `Copy(_Y)` would route a later `&mut _X` borrow at
/// the wrong slot, so writes through the borrow would land on `_Y`'s
/// (now stale) storage instead of the user's named binding.
///
/// Bindings whose RHS reads through a projection (`Copy(a[i])`,
/// `Copy(s.f)`) are also unsafe to forward across an intervening
/// write: in `let t = a[lo]; a[lo] = a[hi]; a[hi] = t`, propagating
/// `t -> Copy(a[lo])` into the third statement reads the freshly-
/// stored `a[hi]` value instead of the original `a[lo]`. We only
/// retain bindings whose RHS is a `Const` or `Copy(simple-local)`.
pub fn copy_propagate(body: &mut Body, tcx: &TyCtxt) {
    let aggregate_locals: Vec<bool> = body
        .locals
        .iter()
        .map(|decl| {
            matches!(
                tcx.kind(decl.ty),
                Some(TyKind::Array { .. } | TyKind::Tuple(_) | TyKind::Adt { .. })
            )
        })
        .collect();
    for block in &mut body.blocks {
        let mut bindings: HashMap<Local, Operand> = HashMap::new();
        for stmt in &mut block.stmts {
            if let StatementKind::Assign { place, rvalue } = &mut stmt.kind {
                if let Rvalue::Use(operand) = rvalue {
                    substitute_operand(operand, &bindings);
                    let dest_aggregate = aggregate_locals
                        .get(place.local.0 as usize)
                        .copied()
                        .unwrap_or(false);
                    let operand_is_simple = match operand {
                        Operand::Const(_) | Operand::FnRef { .. } => true,
                        Operand::Copy(p) => p.is_simple(),
                    };
                    if place.is_simple() && !dest_aggregate && operand_is_simple {
                        bindings.insert(place.local, operand.clone());
                    }
                } else {
                    substitute_rvalue(rvalue, &bindings);
                }
            }
        }
    }
}

fn substitute_rvalue(rvalue: &mut Rvalue, bindings: &HashMap<Local, Operand>) {
    match rvalue {
        Rvalue::Use(op) => substitute_operand(op, bindings),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            substitute_operand(lhs, bindings);
            substitute_operand(rhs, bindings);
        }
        Rvalue::UnaryOp { operand, .. } => substitute_operand(operand, bindings),
        Rvalue::Cast { operand, .. } => substitute_operand(operand, bindings),
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                substitute_operand(op, bindings);
            }
        }
        Rvalue::CallIntrinsic { args, .. } => {
            for op in args {
                substitute_operand(op, bindings);
            }
        }
        Rvalue::Repeat { value, .. } => substitute_operand(value, bindings),
        Rvalue::Len(_) | Rvalue::Ref { .. } => {}
    }
}

fn substitute_operand(operand: &mut Operand, bindings: &HashMap<Local, Operand>) {
    let Operand::Copy(Place {
        local,
        ref projection,
    }) = *operand
    else {
        return;
    };
    if !projection.is_empty() {
        return;
    }
    if let Some(replacement) = bindings.get(&local) {
        *operand = replacement.clone();
    }
}

/// Removes assignments whose destination local is never read again and
/// is not observable (no projections, no exported writes). A simple
/// forward-use count keeps it local to each block.
pub fn dead_store_elim(body: &mut Body) {
    // Walk the whole body once and tally cross-block reads, then drop
    // const-producing assignments whose destination local is read
    // nowhere. A per-block counter misses the common case where a
    // match/if-join writes a temporary in the arm blocks and reads it
    // back in the join block.
    let mut use_count: HashMap<Local, usize> = HashMap::new();
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                if !place.projection.is_empty() {
                    count_place_reads(place, &mut use_count);
                }
                count_rvalue_reads(rvalue, &mut use_count);
            }
        }
        count_terminator_reads(&block.terminator, &mut use_count);
    }
    // The return slot is implicitly read by `Terminator::Return` even
    // though we do not surface the operand in the terminator itself.
    // Pin its use count so dead-store-elim never drops writes into it.
    *use_count.entry(Local::RETURN).or_insert(0) += 1;

    for block in &mut body.blocks {
        let mut retained = Vec::with_capacity(block.stmts.len());
        for stmt in std::mem::take(&mut block.stmts) {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Const(_)),
            } = &stmt.kind
            {
                if place.is_simple() && use_count.get(&place.local).copied().unwrap_or(0) == 0 {
                    continue;
                }
            }
            retained.push(stmt);
        }
        block.stmts = retained;
    }
}

fn count_rvalue_reads(rvalue: &Rvalue, uses: &mut HashMap<Local, usize>) {
    match rvalue {
        Rvalue::Use(op) => count_operand_reads(op, uses),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            count_operand_reads(lhs, uses);
            count_operand_reads(rhs, uses);
        }
        Rvalue::UnaryOp { operand, .. } => count_operand_reads(operand, uses),
        Rvalue::Cast { operand, .. } => count_operand_reads(operand, uses),
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                count_operand_reads(op, uses);
            }
        }
        Rvalue::CallIntrinsic { args, .. } => {
            for op in args {
                count_operand_reads(op, uses);
            }
        }
        Rvalue::Repeat { value, .. } => count_operand_reads(value, uses),
        Rvalue::Len(place) | Rvalue::Ref { place, .. } => {
            count_place_reads(place, uses);
        }
    }
}

fn count_operand_reads(operand: &Operand, uses: &mut HashMap<Local, usize>) {
    if let Operand::Copy(place) = operand {
        count_place_reads(place, uses);
    }
}

/// Counts a read of the root local plus every local referenced by a
/// [`Projection::Index`] inside `place.projection`. Without this, an
/// index expression such as `xs[i]` only registers `xs` as read,
/// letting dead-store elimination drop the `i = Const(...)` store and
/// leaving the projection pointing at an uninitialised slot.
fn count_place_reads(place: &Place, uses: &mut HashMap<Local, usize>) {
    *uses.entry(place.local).or_insert(0) += 1;
    for proj in &place.projection {
        if let Projection::Index(idx) = proj {
            *uses.entry(*idx).or_insert(0) += 1;
        }
    }
}

fn count_terminator_reads(terminator: &Terminator, uses: &mut HashMap<Local, usize>) {
    match terminator {
        Terminator::SwitchInt { discriminant, .. } => count_operand_reads(discriminant, uses),
        Terminator::Call {
            callee,
            args,
            destination,
            ..
        } => {
            count_operand_reads(callee, uses);
            for op in args {
                count_operand_reads(op, uses);
            }
            if !destination.projection.is_empty() {
                count_place_reads(destination, uses);
            }
        }
        Terminator::Assert { cond, .. } => count_operand_reads(cond, uses),
        _ => {}
    }
}

/// Returns the number of [`Statement`]s across all blocks.
#[must_use]
pub fn statement_count(body: &Body) -> usize {
    body.blocks.iter().map(|b| b.stmts.len()).sum()
}

/// Returns the [`ConstValue`] flowing into `local` in the entry block,
/// if any direct assignment records one. Convenience accessor for
/// tests that want to inspect post-const-fold state.
#[must_use]
pub fn const_value_of(body: &Body, local: Local) -> Option<ConstValue> {
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Const(value)),
            } = &stmt.kind
            {
                if place.local == local && place.is_simple() {
                    return Some(value.clone());
                }
            }
        }
    }
    None
}

#[allow(dead_code)]
fn _used_in_phase_20(_: Statement) {}
