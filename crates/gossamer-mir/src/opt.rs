//! Simple MIR optimisations.
//! Commits to three lightweight passes: constant folding,
//! copy propagation, and dead-store elimination. Each pass is
//! idempotent so callers can run them in any order.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use gossamer_types::{TyCtxt, TyKind};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, Operand, Place, Projection, Rvalue,
    Statement, StatementKind, Terminator,
};

/// Returns `false` when `GOSSAMER_INLINE=0` (or `false`) is set. Lets
/// the differential test assert that inlining never changes observable
/// program behaviour: run a program with the inliner on and off and
/// compare output. Read live (not memoised) so a test can flip it.
fn inlining_enabled() -> bool {
    !matches!(
        std::env::var("GOSSAMER_INLINE").ok().as_deref(),
        Some("0" | "false")
    )
}

/// Maps each body's resolver `DefId` (its `local` index) to the body
/// name, so an `Operand::FnRef` call site can be resolved to the
/// callee body the same way the JIT compile-set BFS resolves it.
fn def_to_name_map(bodies: &[Body]) -> HashMap<u32, String> {
    bodies
        .iter()
        .filter_map(|b| b.def.map(|d| (d.local, b.name.clone())))
        .collect()
}

/// Resolves a call terminator's `callee` to the name of the user body
/// it targets, if any. Handles both by-name (`Const(Str)`) calls and
/// monomorphic `FnRef` calls (resolved through `def_to_name`). Generic
/// `FnRef` calls (non-empty `substs`) target a mangled specialisation
/// and are left for the call-site lowering to resolve.
fn callee_body_name(callee: &Operand, def_to_name: &HashMap<u32, String>) -> Option<String> {
    match callee {
        Operand::Const(ConstValue::Str(name)) => Some(name.clone()),
        Operand::FnRef { def, substs } if substs.is_empty() => def_to_name.get(&def.local).cloned(),
        _ => None,
    }
}

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
    if !inlining_enabled() {
        return;
    }
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

/// Per-callee inlining cost ceiling, in weighted MIR units
/// (statements + one per block terminator). Replaces the old flat
/// 4-statement rule: small leaf helpers up to this weight inline.
/// Lowering emits roughly two statements per source operation (a temp
/// plus its named binding), so this admits leaves of about five to
/// eight source statements.
const INLINE_COST_LIMIT: usize = 24;

/// Total weighted growth one caller may accrue from inlining before
/// further inlines into it are skipped - caps caller blow-up when a
/// hot function calls many small helpers.
const INLINE_CALLER_BUDGET: usize = 96;

/// Weighted size of a body: one unit per statement plus one per block
/// terminator.
fn body_cost(body: &Body) -> usize {
    body.blocks.iter().map(|b| b.stmts.len() + 1).sum()
}

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
    /// Weighted cost of the callee body (see [`body_cost`]).
    cost: usize,
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
    if !inlining_enabled() {
        return;
    }
    let mut inlineables: HashMap<String, InlineableCallee> = HashMap::new();
    for body in bodies.iter() {
        if let Some(ic) = try_build_inlineable(body) {
            inlineables.insert(body.name.clone(), ic);
        }
    }
    if inlineables.is_empty() {
        return;
    }
    let def_to_name = def_to_name_map(bodies);
    for body in bodies.iter_mut() {
        inline_into_body(body, &inlineables, &def_to_name);
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
        // Aggregate-building callees stay out of the single-block path:
        // it remaps the return slot to the caller's destination without
        // copying the callee's typed return local, so a later nested
        // field read would resolve its leaf type against an untyped
        // local. `inline_general` is the aggregate-capable path - it
        // copies every callee `LocalDecl` (with its `ty`), preserving
        // the return type by construction.
        if matches!(rvalue, Rvalue::Aggregate { .. } | Rvalue::Repeat { .. }) {
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
    let cost = body_cost(body);
    if cost > INLINE_COST_LIMIT {
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
        cost,
    })
}

fn inline_into_body(
    body: &mut Body,
    inlineables: &HashMap<String, InlineableCallee>,
    def_to_name: &HashMap<u32, String>,
) {
    // Iterate over block indices because we mutate `body.locals` (to
    // add callee temps) during the loop. The block list itself does
    // not grow - we splice statements into existing blocks.
    let mut budget = INLINE_CALLER_BUDGET;
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
        let Some(callee_name) = callee_body_name(&callee, def_to_name) else {
            bi += 1;
            continue;
        };
        if callee_name.as_str() == body.name.as_str() {
            bi += 1;
            continue;
        }
        let Some(ic) = inlineables.get(&callee_name) else {
            bi += 1;
            continue;
        };
        if ic.cost > budget {
            bi += 1;
            continue;
        }
        // Guard: arity must match (defensive against a resolved
        // `FnRef` whose call site disagrees with the callee).
        if ic.arity as usize != args.len() {
            bi += 1;
            continue;
        }
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
        // All guards passed - inline.
        budget -= ic.cost;
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
        // No local operands to remap; the static is referenced by symbol.
        Rvalue::StaticLoad(_) => rv.clone(),
    }
}

/// A callee eligible for general inlining: a clone of its whole body
/// plus its weighted cost.
#[derive(Clone)]
struct GeneralCallee {
    body: Body,
    cost: usize,
}

/// Decides whether `body` may be inlined as a whole-CFG splice.
/// Requires a real (non-diverging) `Return` path, bounded cost, and no
/// `Drop` terminator: drop semantics depend on the callee's own scope
/// boundaries, so relocating a `Drop` into the caller is unsound.
fn try_build_general(body: &Body) -> Option<GeneralCallee> {
    if body.blocks.is_empty() {
        return None;
    }
    let cost = body_cost(body);
    if cost > INLINE_COST_LIMIT {
        return None;
    }
    let mut has_return = false;
    for b in &body.blocks {
        match &b.terminator {
            Terminator::Return => has_return = true,
            Terminator::Drop { .. } => return None,
            _ => {}
        }
    }
    if !has_return {
        return None;
    }
    Some(GeneralCallee {
        body: body.clone(),
        cost,
    })
}

/// Inlines user-function call sites whose callee is a registered
/// `GeneralCallee` - splicing the callee's whole CFG so multi-block,
/// call-containing, and aggregate-returning callees flatten into the
/// caller. This is the strongest safe lever for JIT coverage: the JIT
/// compile-set BFS drops any body that calls an excluded body, so
/// dissolving the call edge promotes whole chains.
///
/// Each caller has a single weighted growth budget; a direct
/// self-recursive call is never inlined. Spliced blocks are rescanned
/// within the same caller (bounded by the budget), so a chain
/// `top -> mid -> lo` flattens in one pass.
pub fn inline_general(bodies: &mut [Body]) {
    if !inlining_enabled() {
        return;
    }
    let mut callees: HashMap<String, GeneralCallee> = HashMap::new();
    for b in bodies.iter() {
        if let Some(gc) = try_build_general(b) {
            callees.insert(b.name.clone(), gc);
        }
    }
    if callees.is_empty() {
        return;
    }
    let def_to_name = def_to_name_map(bodies);
    for body in bodies.iter_mut() {
        inline_general_into(body, &callees, &def_to_name);
    }
}

fn inline_general_into(
    body: &mut Body,
    callees: &HashMap<String, GeneralCallee>,
    def_to_name: &HashMap<u32, String>,
) {
    let mut budget = INLINE_CALLER_BUDGET;
    // The block list grows as callees are spliced; scanning the
    // appended blocks too flattens a call chain transitively, and the
    // shared `budget` (each splice costs >= 1) guarantees termination.
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
        let Some(name) = callee_body_name(&callee, def_to_name) else {
            bi += 1;
            continue;
        };
        if name.as_str() == body.name.as_str() {
            bi += 1;
            continue;
        }
        let Some(gc) = callees.get(name.as_str()) else {
            bi += 1;
            continue;
        };
        let Some(continuation) = target else {
            bi += 1;
            continue;
        };
        if gc.body.arity as usize != args.len() {
            bi += 1;
            continue;
        }
        if gc.cost > budget {
            bi += 1;
            continue;
        }
        budget -= gc.cost;
        splice_callee(body, bi, &gc.body, &args, &destination, continuation);
        bi += 1;
    }
}

/// Splices `callee` into `body` at `call_block`. Appends fresh copies
/// of every callee local, clones+remaps the callee's blocks, routes
/// each callee `Return` to a landing block that writes `destination`,
/// binds params via injected `param = Use(arg)` statements, and turns
/// the original call into a `Goto` to the callee entry.
fn splice_callee(
    body: &mut Body,
    call_block: usize,
    callee: &Body,
    args: &[Operand],
    destination: &Place,
    continuation: BlockId,
) {
    let base_local = body.locals.len() as u32;
    for decl in &callee.locals {
        body.locals.push(decl.clone());
    }
    let base_block = body.blocks.len() as u32;
    let remap_local = move |l: Local| Local(base_local + l.0);
    let remap_block = move |b: BlockId| BlockId(base_block + b.0);
    // Landing block sits after the callee's own blocks.
    let landing_id = BlockId(base_block + callee.blocks.len() as u32);

    for cb in &callee.blocks {
        let stmts = cb
            .stmts
            .iter()
            .map(|s| remap_statement_full(s, &remap_local))
            .collect();
        let terminator =
            remap_terminator_full(&cb.terminator, &remap_local, &remap_block, landing_id);
        body.blocks.push(BasicBlock {
            id: remap_block(cb.id),
            stmts,
            terminator,
            span: cb.span,
        });
    }

    body.blocks.push(BasicBlock {
        id: landing_id,
        stmts: vec![Statement {
            kind: StatementKind::Assign {
                place: destination.clone(),
                rvalue: Rvalue::Use(Operand::Copy(Place {
                    local: remap_local(Local::RETURN),
                    projection: Vec::new(),
                })),
            },
            span: callee.span,
        }],
        terminator: Terminator::Goto {
            target: continuation,
        },
        span: callee.span,
    });

    let entry = remap_block(callee.blocks[0].id);
    let bind: Vec<Statement> = (0..callee.arity)
        .map(|i| Statement {
            kind: StatementKind::Assign {
                place: Place {
                    local: remap_local(Local(i + 1)),
                    projection: Vec::new(),
                },
                rvalue: Rvalue::Use(args[i as usize].clone()),
            },
            span: callee.span,
        })
        .collect();
    body.blocks[call_block].stmts.extend(bind);
    body.blocks[call_block].terminator = Terminator::Goto { target: entry };
}

/// Full statement remap that, unlike [`remap_statement`], also remaps
/// `StorageLive`/`StorageDead`/`SetDiscriminant` locals and every
/// `Projection::Index` local - required for general inlining where the
/// callee may carry storage annotations and indexed accesses.
fn remap_statement_full(stmt: &Statement, remap: &impl Fn(Local) -> Local) -> Statement {
    let kind = match &stmt.kind {
        StatementKind::Assign { place, rvalue } => StatementKind::Assign {
            place: remap_place_full(place, remap),
            rvalue: remap_rvalue_full(rvalue, remap),
        },
        StatementKind::StorageLive(l) => StatementKind::StorageLive(remap(*l)),
        StatementKind::StorageDead(l) => StatementKind::StorageDead(remap(*l)),
        StatementKind::SetDiscriminant { place, variant } => StatementKind::SetDiscriminant {
            place: remap_place_full(place, remap),
            variant: *variant,
        },
        StatementKind::StaticStore { target, value } => StatementKind::StaticStore {
            target: target.clone(),
            value: remap_operand_full(value, remap),
        },
        StatementKind::Nop => StatementKind::Nop,
    };
    Statement {
        kind,
        span: stmt.span,
    }
}

/// Remaps a place's root local AND any `Projection::Index` local, which
/// [`remap_place`] does not - needed for indexed accesses in a spliced
/// callee.
fn remap_place_full(place: &Place, remap: &impl Fn(Local) -> Local) -> Place {
    let projection = place
        .projection
        .iter()
        .map(|p| match p {
            Projection::Index(idx) => Projection::Index(remap(*idx)),
            other => other.clone(),
        })
        .collect();
    Place {
        local: remap(place.local),
        projection,
    }
}

/// Operand remap that walks `Index` projection locals (via
/// [`remap_place_full`]).
fn remap_operand_full(op: &Operand, remap: &impl Fn(Local) -> Local) -> Operand {
    match op {
        Operand::Copy(place) => Operand::Copy(remap_place_full(place, remap)),
        other => other.clone(),
    }
}

/// Rvalue remap that walks `Index` projection locals in every operand
/// and place (via [`remap_place_full`] / [`remap_operand_full`]).
fn remap_rvalue_full(rv: &Rvalue, remap: &impl Fn(Local) -> Local) -> Rvalue {
    match rv {
        Rvalue::Use(op) => Rvalue::Use(remap_operand_full(op, remap)),
        Rvalue::BinaryOp { op, lhs, rhs } => Rvalue::BinaryOp {
            op: *op,
            lhs: remap_operand_full(lhs, remap),
            rhs: remap_operand_full(rhs, remap),
        },
        Rvalue::UnaryOp { op, operand } => Rvalue::UnaryOp {
            op: *op,
            operand: remap_operand_full(operand, remap),
        },
        Rvalue::Cast { operand, target } => Rvalue::Cast {
            operand: remap_operand_full(operand, remap),
            target: *target,
        },
        Rvalue::Aggregate { kind, operands } => Rvalue::Aggregate {
            kind: kind.clone(),
            operands: operands
                .iter()
                .map(|op| remap_operand_full(op, remap))
                .collect(),
        },
        Rvalue::Repeat { value, count } => Rvalue::Repeat {
            value: remap_operand_full(value, remap),
            count: *count,
        },
        Rvalue::Len(place) => Rvalue::Len(remap_place_full(place, remap)),
        Rvalue::Ref { place, mutable } => Rvalue::Ref {
            place: remap_place_full(place, remap),
            mutable: *mutable,
        },
        Rvalue::CallIntrinsic { name, args } => Rvalue::CallIntrinsic {
            name,
            args: args
                .iter()
                .map(|op| remap_operand_full(op, remap))
                .collect(),
        },
        // No local operands to remap; the static is referenced by symbol.
        Rvalue::StaticLoad(_) => rv.clone(),
    }
}

/// Remaps a terminator's locals and block targets; `Return` becomes a
/// `Goto` to the landing block.
fn remap_terminator_full(
    t: &Terminator,
    remap_local: &impl Fn(Local) -> Local,
    remap_block: &impl Fn(BlockId) -> BlockId,
    landing: BlockId,
) -> Terminator {
    match t {
        Terminator::Return => Terminator::Goto { target: landing },
        Terminator::Goto { target } => Terminator::Goto {
            target: remap_block(*target),
        },
        Terminator::SwitchInt {
            discriminant,
            arms,
            default,
        } => Terminator::SwitchInt {
            discriminant: remap_operand_full(discriminant, remap_local),
            arms: arms.iter().map(|(v, b)| (*v, remap_block(*b))).collect(),
            default: remap_block(*default),
        },
        Terminator::Call {
            callee,
            args,
            destination,
            target,
        } => Terminator::Call {
            callee: remap_operand_full(callee, remap_local),
            args: args
                .iter()
                .map(|a| remap_operand_full(a, remap_local))
                .collect(),
            destination: remap_place_full(destination, remap_local),
            target: target.map(remap_block),
        },
        Terminator::Assert {
            cond,
            expected,
            msg,
            target,
        } => Terminator::Assert {
            cond: remap_operand_full(cond, remap_local),
            expected: *expected,
            msg: msg.clone(),
            target: remap_block(*target),
        },
        Terminator::Unreachable => Terminator::Unreachable,
        Terminator::Panic { message } => Terminator::Panic {
            message: message.clone(),
        },
        // Excluded by `try_build_general`; kept total to satisfy the
        // compiler without relocating drop semantics into the caller.
        Terminator::Drop { .. } => Terminator::Unreachable,
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
/// any future GVN/CSE pass can share one source of truth - the
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
    // value - otherwise control-flow-sensitive code such as
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
/// [`Operand::Const`], and `BinaryOp`s where one operand is a
/// constant identity / absorbing element (`x + 0`, `x * 1`, `x & 0`,
/// `b | true`, ...).
pub fn const_fold(body: &mut Body) {
    for block in &mut body.blocks {
        for stmt in &mut block.stmts {
            if let StatementKind::Assign {
                rvalue: ref mut rv, ..
            } = stmt.kind
            {
                if let Some(folded) = try_fold(rv) {
                    *rv = Rvalue::Use(Operand::Const(folded));
                } else if let Some(simplified) = try_identity_fold(rv) {
                    *rv = simplified;
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

/// Simplifies a `BinaryOp` whose lhs or rhs is a constant identity
/// or absorbing element, rewriting to a plain `Use` of the surviving
/// operand (or the absorbed constant). Integer and bool shapes only:
/// float identities are unsound under IEEE-754 (`-0.0 + 0.0 == +0.0`,
/// `x * 0.0` with NaN/∞), and a non-constant divisor keeps its
/// runtime division so `0 / x` still faults when `x == 0`. Width
/// independence: only the constants `0` and `1` qualify - an
/// all-ones mask (`x & !0`) would depend on the operand's bit width,
/// which `ConstValue::Int(i128)` does not carry.
fn try_identity_fold(rvalue: &Rvalue) -> Option<Rvalue> {
    let Rvalue::BinaryOp { op, lhs, rhs } = rvalue else {
        return None;
    };
    if let Operand::Const(c) = rhs {
        if let Some(rv) = identity_fold_with_const(*op, lhs, c, true) {
            return Some(rv);
        }
    }
    if let Operand::Const(c) = lhs {
        if let Some(rv) = identity_fold_with_const(*op, rhs, c, false) {
            return Some(rv);
        }
    }
    None
}

/// One side of [`try_identity_fold`]: `other OP c` when `const_is_rhs`,
/// `c OP other` otherwise.
fn identity_fold_with_const(
    op: BinOp,
    other: &Operand,
    c: &ConstValue,
    const_is_rhs: bool,
) -> Option<Rvalue> {
    let keep = || Some(Rvalue::Use(other.clone()));
    let int_const = |n: i128| Some(Rvalue::Use(Operand::Const(ConstValue::Int(n))));
    let bool_const = |b: bool| Some(Rvalue::Use(Operand::Const(ConstValue::Bool(b))));
    match c {
        ConstValue::Int(n) => match (op, *n) {
            (BinOp::Add | BinOp::BitOr | BinOp::BitXor, 0) => keep(),
            (BinOp::Mul, 1) => keep(),
            (BinOp::Mul, 0) => int_const(0),
            (BinOp::BitAnd, 0) => int_const(0),
            // Right-hand-side-only identities: subtraction and
            // division are not commutative, and a zero shift *amount*
            // is an identity while a zero shifted *value* would erase
            // the runtime shift-amount check.
            (BinOp::Sub | BinOp::Shl | BinOp::Shr, 0) if const_is_rhs => keep(),
            (BinOp::Div, 1) if const_is_rhs => keep(),
            (BinOp::Rem, 1) if const_is_rhs => int_const(0),
            _ => None,
        },
        ConstValue::Bool(b) => match (op, *b) {
            (BinOp::BitAnd, true) | (BinOp::BitOr | BinOp::BitXor, false) => keep(),
            (BinOp::BitAnd, false) => bool_const(false),
            (BinOp::BitOr, true) => bool_const(true),
            _ => None,
        },
        _ => None,
    }
}

fn fold_unary(op: crate::ir::UnOp, operand: &ConstValue) -> Option<ConstValue> {
    match (op, operand) {
        // `i128::MIN`'s negation overflows; folding through `-x` would
        // panic in debug builds. Skip the fold and leave the runtime to
        // produce the wrapping result if the program reaches that path.
        (crate::ir::UnOp::Neg, ConstValue::Int(x)) => x.checked_neg().map(ConstValue::Int),
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
            if let StatementKind::StaticStore { value, .. } = &mut stmt.kind {
                substitute_operand(value, &bindings);
                continue;
            }
            if let StatementKind::Assign { place, rvalue } = &mut stmt.kind {
                // Substitute reads first (covers `Use` and every other
                // rvalue shape uniformly).
                substitute_rvalue(rvalue, &bindings);
                if !place.is_simple() {
                    // A projected write (`*p`, `p.field`) does not
                    // reassign the local's own value; leave bindings.
                    continue;
                }
                // Any assignment to `place.local` invalidates its prior
                // binding and any binding that referenced it - otherwise
                // a stale value (e.g. a null-init constant) would be
                // propagated past the reassignment into later uses.
                bindings.remove(&place.local);
                bindings.retain(|_, v| {
                    !matches!(v, Operand::Copy(p) if p.local == place.local && p.projection.is_empty())
                });
                // Record a fresh copy/const binding only for `Use` of a
                // simple, non-aggregate operand.
                if let Rvalue::Use(operand) = rvalue {
                    let dest_aggregate = aggregate_locals
                        .get(place.local.0 as usize)
                        .copied()
                        .unwrap_or(false);
                    let operand_is_simple = match operand {
                        Operand::Const(_) | Operand::FnRef { .. } => true,
                        Operand::Copy(p) => p.is_simple(),
                    };
                    if !dest_aggregate && operand_is_simple {
                        bindings.insert(place.local, operand.clone());
                    }
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
        Rvalue::Len(_) | Rvalue::Ref { .. } | Rvalue::StaticLoad(_) => {}
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
            match &stmt.kind {
                StatementKind::Assign { place, rvalue } => {
                    if !place.projection.is_empty() {
                        count_place_reads(place, &mut use_count);
                    }
                    count_rvalue_reads(rvalue, &mut use_count);
                }
                StatementKind::StaticStore { value, .. } => {
                    count_operand_reads(value, &mut use_count);
                }
                _ => {}
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
        // No local reads; the static is referenced by symbol.
        Rvalue::StaticLoad(_) => {}
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

/// Returns the number of [`crate::ir::Statement`]s across all blocks.
#[must_use]
pub fn statement_count(body: &Body) -> usize {
    body.blocks.iter().map(|b| b.stmts.len()).sum()
}

/// Returns the [`ConstValue`] flowing into `local` in the entry block,
/// if any direct assignment records one. Convenience accessor for
/// tests that want to inspect post-const-fold state.
#[must_use]
pub fn const_value_of(body: &Body, local: Local) -> Option<ConstValue> {
    // A local is a known constant only if it is assigned EXACTLY ONCE,
    // and that single assignment is `Use(Const)`. A local reassigned
    // later (e.g. zeroed for null-safety then overwritten by a heap
    // allocation) is not constant - propagating the first const value
    // past the reassignment would miscompile every later use.
    let mut found: Option<ConstValue> = None;
    for block in &body.blocks {
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.local != local || !place.is_simple() {
                continue;
            }
            match rvalue {
                Rvalue::Use(Operand::Const(value)) if found.is_none() => {
                    found = Some(value.clone());
                }
                // A second assignment of any kind (including another
                // const) disqualifies constant folding for this local.
                _ => return None,
            }
        }
    }
    // Terminator-position definitions (Call destinations) also reassign
    // the local; if any exists, the local is not a fixed constant.
    for block in &body.blocks {
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.local == local
            && destination.is_simple()
        {
            return None;
        }
    }
    found
}
