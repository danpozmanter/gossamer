//! Simple MIR optimisations.
//! Commits to three lightweight passes: constant folding,
//! copy propagation, and dead-store elimination. Each pass is
//! idempotent so callers can run them in any order.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_types::{TyCtxt, TyKind};

use crate::ir::{
    BinOp, BlockId, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, StatementKind,
    Terminator,
};

/// Runs the full optimisation pipeline on `body`. Copy propagation
/// runs before constant folding so that temporaries introduced by the
/// lowerer (`tmp = Const(1); out = BinaryOp(Copy(tmp), ...)`) collapse
/// into the two-constant form folding recognises. A second copy-prop
/// pass after folding propagates the newly-created constants.
pub fn optimise(body: &mut Body, tcx: &TyCtxt) {
    crate::verify::debug_verify_body(body);
    copy_propagate(body, tcx);
    crate::verify::debug_verify_body(body);
    const_fold(body);
    crate::verify::debug_verify_body(body);
    copy_propagate(body, tcx);
    crate::verify::debug_verify_body(body);
    const_branch_elim(body);
    crate::verify::debug_verify_body(body);
    dead_store_elim(body, tcx);
    crate::verify::debug_verify_body(body);
}

/// Detects trivial wrapper functions (a two-block body whose entry block
/// immediately calls another function with all params forwarded in order)
/// and rewrites every call site to invoke the inner function directly,
/// eliminating the intermediate call frame.
///
/// Must be called before [`optimise`] so that subsequent copy-propagation
/// and dead-store passes see the tightened call graph.
pub fn inline_trivial_wrappers(bodies: &mut [Body]) {
    let mut wrappers: HashMap<String, Operand> = HashMap::new();
    for body in bodies.iter() {
        if let Some(inner_callee) = detect_wrapper_callee(body) {
            wrappers.insert(body.name.clone(), inner_callee);
        }
    }
    if wrappers.is_empty() {
        return;
    }
    for body in bodies.iter_mut() {
        for block in &mut body.blocks {
            if let Terminator::Call { callee, .. } = &mut block.terminator {
                if let Operand::Const(ConstValue::Str(name)) = callee {
                    if let Some(inner) = wrappers.get(name.as_str()) {
                        *callee = inner.clone();
                    }
                }
            }
        }
    }
}

/// Returns the inner callee if `body` is a trivial wrapper, `None` otherwise.
/// A trivial wrapper has exactly two blocks:
/// - bb0: no statements, `Call` terminator whose args are exactly the
///   params `Local(1)..=Local(arity)` forwarded in source order.
/// - bb1: no statements, `Return` terminator.
fn detect_wrapper_callee(body: &Body) -> Option<Operand> {
    if body.blocks.len() != 2 {
        return None;
    }
    let bb0 = &body.blocks[0];
    let bb1 = &body.blocks[1];
    if !bb0.stmts.is_empty() || !bb1.stmts.is_empty() {
        return None;
    }
    if !matches!(bb1.terminator, Terminator::Return) {
        return None;
    }
    let Terminator::Call {
        callee,
        args,
        destination,
        target,
    } = &bb0.terminator
    else {
        return None;
    };
    if *target != Some(bb1.id) {
        return None;
    }
    if destination.local != Local::RETURN || !destination.projection.is_empty() {
        return None;
    }
    if args.len() != body.arity as usize {
        return None;
    }
    for (i, arg) in args.iter().enumerate() {
        let expected = Local((i as u32) + 1);
        match arg {
            Operand::Copy(place) if place.local == expected && place.projection.is_empty() => {}
            _ => return None,
        }
    }
    Some(callee.clone())
}

/// Maximum number of statements in a callee body that we are willing to inline.
const INLINE_STMT_LIMIT: usize = 4;

/// Snapshot of an inlineable callee body: its arity, its locals
/// (for splicing into the caller's local table), and its one
/// computation block's statements.
#[derive(Clone)]
struct InlineableCallee {
    arity: u32,
    /// Callee locals from index `arity + 1` onward (the temps only;
    /// params and the return slot are remapped to caller locals).
    extra_locals: Vec<crate::ir::LocalDecl>,
    /// Statements from the callee's single computation block, before
    /// remapping.
    stmts: Vec<crate::ir::Statement>,
}

/// Inlines small (≤ `INLINE_STMT_LIMIT`-statement) single-block callee
/// bodies into their call sites. Only inlines when:
/// - The callee has 1 or 2 blocks with all statements in block 0.
/// - All statements are plain assignments (`Assign`) with no calls and
///   no projected places on the destination.
/// - All call-site arguments are simple bare-local copies or constants
///   (no projections on the arg places).
/// - The call destination place has no projections.
///
/// Run before `optimise` so the per-body passes see the flattened graph.
pub fn inline_small_callees(bodies: &mut [Body]) {
    let mut inlineables: HashMap<String, InlineableCallee> = HashMap::new();
    for body in bodies.iter() {
        if let Some(ic) = try_build_inlineable(body) {
            inlineables.insert(body.name.clone(), ic);
        }
    }
    if inlineables.is_empty() {
        return;
    }
    for body in bodies.iter_mut() {
        inline_into_body(body, &inlineables);
    }
}

fn try_build_inlineable(body: &Body) -> Option<InlineableCallee> {
    // Callee must have 1 or 2 blocks. With 2 blocks, block 1 is an
    // empty Return continuation (same shape as `detect_wrapper_callee`).
    if body.blocks.len() > 2 || body.blocks.is_empty() {
        return None;
    }
    let bb0 = &body.blocks[0];
    // The computation block must not contain nested calls.
    for stmt in &bb0.stmts {
        let StatementKind::Assign { place, rvalue } = &stmt.kind else {
            return None;
        };
        // Destination must be a bare local (no projections).
        if !place.projection.is_empty() {
            return None;
        }
        // Rvalue must not be a call or contain a call.
        if matches!(rvalue, Rvalue::CallIntrinsic { .. }) {
            return None;
        }
    }
    // Terminator of bb0: either Return or Goto{bb1}.
    match &bb0.terminator {
        Terminator::Return => {}
        Terminator::Goto { target } => {
            if body.blocks.len() < 2 {
                return None;
            }
            let bb1 = &body.blocks[1];
            if bb1.id != *target || !bb1.stmts.is_empty() {
                return None;
            }
            if !matches!(bb1.terminator, Terminator::Return) {
                return None;
            }
        }
        _ => return None,
    }
    let total_stmts = bb0.stmts.len();
    if total_stmts > INLINE_STMT_LIMIT {
        return None;
    }
    // Slice off the extra locals (temps beyond params + return slot).
    let param_end = (body.arity + 1) as usize;
    let extra_locals = if param_end < body.locals.len() {
        body.locals[param_end..].to_vec()
    } else {
        Vec::new()
    };
    Some(InlineableCallee {
        arity: body.arity,
        extra_locals,
        stmts: bb0.stmts.clone(),
    })
}

fn inline_into_body(body: &mut Body, inlineables: &HashMap<String, InlineableCallee>) {
    // Iterate over block indices because we mutate `body.locals` (to
    // add callee temps) during the loop. The block list itself does
    // not grow — we splice statements into existing blocks.
    let mut bi = 0;
    while bi < body.blocks.len() {
        let Terminator::Call {
            callee,
            args,
            destination,
            target,
        } = body.blocks[bi].terminator.clone()
        else {
            bi += 1;
            continue;
        };
        let callee_name = if let Operand::Const(ConstValue::Str(s)) = &callee {
            s.clone()
        } else {
            bi += 1;
            continue;
        };
        let Some(ic) = inlineables.get(&callee_name) else {
            bi += 1;
            continue;
        };
        // Guard: destination must be a bare local.
        if !destination.projection.is_empty() {
            bi += 1;
            continue;
        }
        // Guard: all args must be bare-local copies or consts.
        let arg_locals: Vec<Option<Local>> = args
            .iter()
            .map(|op| match op {
                Operand::Copy(p) if p.projection.is_empty() => Some(p.local),
                _ => None,
            })
            .collect();
        if arg_locals.iter().any(Option::is_none) {
            // Some arg is a const or projected place; fall back to
            // introducing a let binding for each such arg.
            // For simplicity, skip inlining this site.
            bi += 1;
            continue;
        }
        // All guards passed — inline.
        let orig_local_count = body.locals.len() as u32;
        body.locals.extend(ic.extra_locals.iter().cloned());

        // Build a remapping closure: callee Local → caller Local.
        let remap = |l: Local| -> Local {
            if l == Local::RETURN {
                return destination.local;
            }
            let idx = l.0;
            if idx >= 1 && idx <= ic.arity {
                // Param: map to the corresponding arg local.
                return arg_locals[(idx - 1) as usize].unwrap_or(Local(idx));
            }
            // Temp: shift by (orig_local_count - (arity + 1)).
            let temp_idx = idx - ic.arity - 1;
            Local(orig_local_count + temp_idx)
        };

        let remapped: Vec<crate::ir::Statement> = ic
            .stmts
            .iter()
            .map(|stmt| remap_statement(stmt, &remap))
            .collect();

        // Replace the Call terminator with Goto and splice statements.
        let continuation = target.unwrap_or(BlockId(bi as u32 + 1));
        body.blocks[bi].stmts.extend(remapped);
        body.blocks[bi].terminator = Terminator::Goto {
            target: continuation,
        };
        bi += 1;
    }
}

fn remap_statement(
    stmt: &crate::ir::Statement,
    remap: &impl Fn(Local) -> Local,
) -> crate::ir::Statement {
    let StatementKind::Assign { place, rvalue } = &stmt.kind else {
        return stmt.clone();
    };
    let new_place = remap_place(place, remap);
    let new_rvalue = remap_rvalue(rvalue, remap);
    crate::ir::Statement {
        kind: StatementKind::Assign {
            place: new_place,
            rvalue: new_rvalue,
        },
        span: stmt.span,
    }
}

fn remap_place(place: &Place, remap: &impl Fn(Local) -> Local) -> Place {
    Place {
        local: remap(place.local),
        projection: place.projection.clone(),
    }
}

fn remap_operand(op: &Operand, remap: &impl Fn(Local) -> Local) -> Operand {
    match op {
        Operand::Copy(place) => Operand::Copy(remap_place(place, remap)),
        other => other.clone(),
    }
}

fn remap_rvalue(rv: &Rvalue, remap: &impl Fn(Local) -> Local) -> Rvalue {
    match rv {
        Rvalue::Use(op) => Rvalue::Use(remap_operand(op, remap)),
        Rvalue::BinaryOp { op, lhs, rhs } => Rvalue::BinaryOp {
            op: *op,
            lhs: remap_operand(lhs, remap),
            rhs: remap_operand(rhs, remap),
        },
        Rvalue::UnaryOp { op, operand } => Rvalue::UnaryOp {
            op: *op,
            operand: remap_operand(operand, remap),
        },
        Rvalue::Cast { operand, target } => Rvalue::Cast {
            operand: remap_operand(operand, remap),
            target: *target,
        },
        Rvalue::Aggregate { kind, operands } => Rvalue::Aggregate {
            kind: kind.clone(),
            operands: operands.iter().map(|op| remap_operand(op, remap)).collect(),
        },
        Rvalue::Repeat { value, count } => Rvalue::Repeat {
            value: remap_operand(value, remap),
            count: *count,
        },
        Rvalue::Len(place) => Rvalue::Len(remap_place(place, remap)),
        Rvalue::Ref { place, mutable } => Rvalue::Ref {
            place: remap_place(place, remap),
            mutable: *mutable,
        },
        Rvalue::CallIntrinsic { name, args } => Rvalue::CallIntrinsic {
            name,
            args: args.iter().map(|op| remap_operand(op, remap)).collect(),
        },
    }
}

/// Identifies which locals hold aggregate types
/// (`Array` / `Tuple` / `Adt`). Aggregates have storage-identity
/// semantics: a `&mut _X` borrow is bound to `_X`'s slot, not to
/// the rvalue that flowed into it. Any optimisation that would
/// alias two aggregate locals (copy propagation forwarding
/// `_X -> _Y`, GVN/CSE folding two equal aggregate constructions
/// into one local, DCE dropping a write that "looks dead" but
/// whose slot a later borrow points at) must consult this map and
/// bail on aggregate-typed locals.
///
/// Factored out of [`copy_propagate`] so [`dead_store_elim`] and
/// any future GVN/CSE pass can share one source of truth — the
/// previous one-pass-only encoding had been a copy-prop fix that
/// the audit flagged as too narrow.
pub(crate) fn aggregate_locals(body: &Body, tcx: &TyCtxt) -> Vec<bool> {
    body.locals
        .iter()
        .map(|decl| {
            matches!(
                tcx.kind(decl.ty),
                Some(TyKind::Array { .. } | TyKind::Tuple(_) | TyKind::Adt { .. })
            )
        })
        .collect()
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
    let aggregate_locals = aggregate_locals(body, tcx);
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
///
/// Aggregate-typed locals are also exempt: even with `use_count` == 0,
/// a later `&mut _X`-style borrow may bind to `_X`'s storage slot,
/// and dropping the write would surface uninitialised data through
/// the borrow. The same guard `copy_propagate` uses applies here so
/// the two passes treat aggregate identity consistently.
pub fn dead_store_elim(body: &mut Body, tcx: &TyCtxt) {
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

    let aggregates = aggregate_locals(body, tcx);
    for block in &mut body.blocks {
        let mut retained = Vec::with_capacity(block.stmts.len());
        for stmt in std::mem::take(&mut block.stmts) {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Const(_)),
            } = &stmt.kind
            {
                let dest_aggregate = aggregates
                    .get(place.local.0 as usize)
                    .copied()
                    .unwrap_or(false);
                if place.is_simple()
                    && !dest_aggregate
                    && use_count.get(&place.local).copied().unwrap_or(0) == 0
                {
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
