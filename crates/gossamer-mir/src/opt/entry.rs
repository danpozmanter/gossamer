// Simple MIR optimisations.
// Commits to three lightweight passes: constant folding,
// copy propagation, and dead-store elimination. Each pass is
// idempotent so callers can run them in any order.


use std::collections::{HashMap, HashSet, VecDeque};

use gossamer_lex::Span;
use gossamer_types::{TyCtxt, TyKind};

use gossamer_types::Ty;

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Projection,
    Rvalue, Statement, StatementKind, Terminator,
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
    dead_block_sweep(body);
    crate::verify::debug_verify_body(body);
    dead_store_elim(body, tcx);
    crate::verify::debug_verify_body(body);
    bounds_check_elim(body, tcx);
    crate::verify::debug_verify_body(body);
    local_branch_bounds_check_elim(body, tcx);
    crate::verify::debug_verify_body(body);
    bounds_check_versioning(body, tcx);
    crate::verify::debug_verify_body(body);
}

/// Rewrites `Vec::new(elem_bytes)` into `gos_rt_vec_with_capacity(elem_bytes,
/// bound)` when the fresh vector is immediately populated by a counted loop
/// whose condition is `i < bound` and whose body pushes into that same vector.
///
/// This is intentionally allocation-only: it does not change the loop, element
/// values, container type, or visible semantics. A negative / zero bound is
/// harmless because the runtime capacity helper treats it like an empty reserve.
pub(crate) fn reserve_vecs_for_counted_push_loops(body: &mut Body) {
    let mut rewrites: Vec<(usize, Vec<Operand>)> = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        let Terminator::Call {
            callee,
            args,
            destination,
            target,
        } = &block.terminator
        else {
            continue;
        };
        if !destination.projection.is_empty() || args.len() != 1 {
            continue;
        }
        if !matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "Vec::new" || name == "gos_rt_vec_new")
        {
            continue;
        }
        let Some(start) = target else { continue };
        let Some(bound) = counted_push_loop_bound(body, *start, destination.local) else {
            continue;
        };
        if !reserve_bound_available_at_entry(body, &bound, block.id) {
            continue;
        }
        rewrites.push((bi, vec![args[0].clone(), bound]));
    }
    for (bi, args) in rewrites {
        if let Terminator::Call {
            callee,
            args: call_args,
            ..
        } = &mut body.blocks[bi].terminator
        {
            *callee = Operand::Const(ConstValue::Str("gos_rt_vec_with_capacity".to_string()));
            *call_args = args;
        }
    }
}

/// Rewrites a fresh word-layout `HashMap::new()` into
/// `gos_rt_map_new_with_capacity` when a following counted loop performs
/// exactly one insert into that map on every iteration. The native capacity
/// constructor carries a proven capacity; native backends select the typed
/// storage layout from the destination map type. Unsupported aggregate layouts
/// retain lazy storage while preserving the same constructor semantics.
/// Duplicate keys are harmless: the loop bound is an upper bound, never an
/// assertion about the final map length.
pub(crate) fn reserve_hashmaps_for_counted_insert_loops(body: &mut Body, tcx: &TyCtxt) {
    let mut rewrites: Vec<(usize, Vec<Operand>)> = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        let Terminator::Call {
            callee,
            args,
            destination,
            target,
        } = &block.terminator
        else {
            continue;
        };
        // Surface `HashMap::new()` has no arguments. The native backends
        // derive its fixed word-sized key/value layout from the destination,
        // and their `*_with_capacity` lowering likewise expects just the
        // capacity operand. Keep accepting the historical runtime-shaped
        // form too, but discard its layout operands rather than accidentally
        // treating the key width as the capacity.
        if !destination.projection.is_empty()
            || !(args.is_empty()
                || (matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "gos_rt_map_new")
                    && args.len() == 2))
        {
            continue;
        }
        if !matches!(callee, Operand::Const(ConstValue::Str(name)) if matches!(name.as_str(), "HashMap::new" | "collections::HashMap::new" | "gos_rt_map_new"))
        {
            continue;
        }
        if !is_hashmap_local(body, tcx, destination.local) {
            continue;
        }
        let Some(start) = target else { continue };
        let Some(bound) = counted_insert_loop_bound(body, *start, destination.local) else {
            continue;
        };
        if !reserve_bound_available_at_entry(body, &bound, block.id) {
            continue;
        }
        rewrites.push((bi, vec![bound]));
    }
    for (bi, args) in rewrites {
        if let Terminator::Call {
            callee,
            args: call_args,
            ..
        } = &mut body.blocks[bi].terminator
        {
            *callee = Operand::Const(ConstValue::Str("gos_rt_map_new_with_capacity".to_string()));
            *call_args = args;
        }
    }
}

