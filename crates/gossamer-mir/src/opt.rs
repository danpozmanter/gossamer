//! Simple MIR optimisations.
//! Commits to three lightweight passes: constant folding,
//! copy propagation, and dead-store elimination. Each pass is
//! idempotent so callers can run them in any order.

#![forbid(unsafe_code)]

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
        if !reserve_bound_available_at_entry(body, &bound) {
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

fn reserve_bound_available_at_entry(body: &Body, bound: &Operand) -> bool {
    match bound {
        Operand::Const(_) => true,
        Operand::Copy(place) if place.projection.is_empty() => {
            place.local.0 != 0 && place.local.0 <= body.arity
        }
        _ => false,
    }
}

fn counted_push_loop_bound(body: &Body, start: BlockId, vec_local: Local) -> Option<Operand> {
    let mut seen = HashSet::new();
    let mut work = VecDeque::from([(start, 0usize)]);
    while let Some((bid, depth)) = work.pop_front() {
        if depth > 64 || !seen.insert(bid) {
            continue;
        }
        let block = body.blocks.get(bid.0 as usize)?;
        if let Some((body_entry, bound)) = counted_loop_body_and_bound(block)
            && loop_body_pushes_vec(body, body_entry, bid, vec_local)
        {
            return Some(bound);
        }
        for succ in terminator_successors(&block.terminator) {
            work.push_back((succ, depth + 1));
        }
    }
    None
}

fn counted_loop_body_and_bound(block: &BasicBlock) -> Option<(BlockId, Operand)> {
    let Terminator::SwitchInt {
        discriminant,
        arms,
        default,
    } = &block.terminator
    else {
        return None;
    };
    let cond_local = whole_copy_local(discriminant)?;
    let body_entry = *default;
    if !arms
        .iter()
        .any(|(value, target)| *value == 0 && *target != body_entry)
    {
        return None;
    }
    for stmt in &block.stmts {
        let StatementKind::Assign { place, rvalue } = &stmt.kind else {
            continue;
        };
        if place.local != cond_local || !place.projection.is_empty() {
            continue;
        }
        let Rvalue::BinaryOp {
            op: BinOp::Lt,
            lhs,
            rhs,
        } = rvalue
        else {
            continue;
        };
        if whole_copy_local(lhs).is_some() {
            return Some((body_entry, rhs.clone()));
        }
    }
    None
}

fn loop_body_pushes_vec(body: &Body, entry: BlockId, loop_head: BlockId, vec_local: Local) -> bool {
    let mut seen = HashSet::new();
    let mut work = VecDeque::from([(entry, 0usize)]);
    while let Some((bid, depth)) = work.pop_front() {
        if bid == loop_head || depth > 128 || !seen.insert(bid) {
            continue;
        }
        let Some(block) = body.blocks.get(bid.0 as usize) else {
            continue;
        };
        if terminator_pushes_vec(&block.terminator, vec_local) {
            return true;
        }
        for succ in terminator_successors(&block.terminator) {
            work.push_back((succ, depth + 1));
        }
    }
    false
}

fn terminator_pushes_vec(term: &Terminator, vec_local: Local) -> bool {
    let Terminator::Call { callee, args, .. } = term else {
        return false;
    };
    if !matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "gos_rt_vec_push" || name == "gos_rt_vec_push_i64")
    {
        return false;
    }
    args.first().and_then(whole_copy_local) == Some(vec_local)
}

fn terminator_successors(term: &Terminator) -> Vec<BlockId> {
    match term {
        Terminator::Goto { target } => vec![*target],
        Terminator::SwitchInt { arms, default, .. } => {
            let mut out = Vec::with_capacity(arms.len() + 1);
            out.push(*default);
            out.extend(arms.iter().map(|(_, target)| *target));
            out
        }
        Terminator::Assert { target, .. } => vec![*target],
        Terminator::Call {
            target: Some(target),
            ..
        } => vec![*target],
        Terminator::Return
        | Terminator::Unreachable
        | Terminator::Panic { .. }
        | Terminator::Drop { .. }
        | Terminator::Call { target: None, .. } => Vec::new(),
    }
}

/// Fuses `strings::slice(input,start,end)?` immediately consumed by
/// `strconv::parse_i64` / `parse_f64` into a range parse helper. The helper
/// performs the same range and UTF-8 validation as `strings::slice`, then
/// parses the borrowed bytes without allocating a temporary runtime `String`.
///
/// This runs after drop insertion, when `?` has been lowered to result-disc /
/// payload blocks. It only fires when the unwrapped string local is otherwise
/// used for parse/release/zeroing, so user-visible `String` values still
/// materialise normally.
pub(crate) fn fuse_slice_parse_ranges(body: &mut Body) {
    let slice_by_result = slice_calls_by_result(body);
    if slice_by_result.is_empty() {
        return;
    }

    let mut payload_defs: HashMap<Local, Local> = HashMap::new();
    let mut copy_defs: HashMap<Local, Option<Local>> = HashMap::new();
    let mut bindings: Vec<(Local, usize, Local)> = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if !place.projection.is_empty() {
                continue;
            }
            match rvalue {
                Rvalue::CallIntrinsic { name, args }
                    if *name == "gos_rt_result_payload" && args.len() == 1 =>
                {
                    if let Some(src) = whole_copy_local(&args[0]) {
                        payload_defs.insert(place.local, src);
                    }
                }
                Rvalue::Use(op) => {
                    if let Some(src) = whole_copy_local(op) {
                        let prior = copy_defs.insert(place.local, Some(src));
                        if prior.is_some() {
                            copy_defs.insert(place.local, None);
                        }
                        bindings.push((place.local, bi, src));
                    }
                }
                _ => {}
            }
        }
    }

    let mut candidates: HashMap<Local, (usize, Vec<Operand>, usize)> = HashMap::new();
    for (str_local, bind_block, src) in bindings {
        let Some(result_local) = trace_payload_result(src, &copy_defs, &payload_defs) else {
            continue;
        };
        let Some((slice_block, range_args)) = slice_by_result.get(&result_local) else {
            continue;
        };
        if slice_parse_local_is_private(body, str_local) {
            candidates.insert(str_local, (*slice_block, range_args.clone(), bind_block));
        }
    }
    if candidates.is_empty() {
        return;
    }

    let mut changed: HashMap<Local, bool> = HashMap::new();
    for block in &mut body.blocks {
        let Terminator::Call { callee, args, .. } = &mut block.terminator else {
            continue;
        };
        let Some(str_local) = args.first().and_then(whole_copy_local) else {
            continue;
        };
        let Some((_, range_args, _)) = candidates.get(&str_local) else {
            continue;
        };
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        let helper = match name.as_str() {
            "gos_rt_strconv_parse_i64" => "gos_rt_strconv_parse_i64_range",
            "gos_rt_strconv_parse_f64" => "gos_rt_strconv_parse_f64_range",
            _ => continue,
        };
        *name = helper.to_string();
        args.clone_from(range_args);
        changed.insert(str_local, true);
    }

    for (str_local, (slice_block, _, bind_block)) in candidates {
        if !changed.get(&str_local).copied().unwrap_or(false) {
            continue;
        }
        let target = body.blocks[bind_block].id;
        body.blocks[slice_block].terminator = Terminator::Goto { target };
        for stmt in &mut body.blocks[bind_block].stmts {
            if assigns_whole_local(stmt, str_local) {
                stmt.kind = StatementKind::Nop;
            }
        }
    }
}

fn slice_calls_by_result(body: &Body) -> HashMap<Local, (usize, Vec<Operand>)> {
    let mut slice_by_result = HashMap::new();
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
        if *target == Some(block.id) || args.len() != 3 {
            continue;
        }
        if !matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "gos_rt_str_slice") {
            continue;
        }
        if destination.projection.is_empty() {
            slice_by_result.insert(destination.local, (bi, args.clone()));
        }
    }
    slice_by_result
}

fn whole_copy_local(op: &Operand) -> Option<Local> {
    match op {
        Operand::Copy(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    }
}

fn trace_payload_result(
    mut local: Local,
    copy_defs: &HashMap<Local, Option<Local>>,
    payload_defs: &HashMap<Local, Local>,
) -> Option<Local> {
    for _ in 0..16 {
        if let Some(result) = payload_defs.get(&local) {
            return Some(*result);
        }
        match copy_defs.get(&local).copied().flatten() {
            Some(next) if next != local => local = next,
            _ => return None,
        }
    }
    None
}

fn assigns_whole_local(stmt: &Statement, local: Local) -> bool {
    matches!(
        &stmt.kind,
        StatementKind::Assign { place, .. } if place.local == local && place.projection.is_empty()
    )
}

fn slice_parse_local_is_private(body: &Body, local: Local) -> bool {
    for block in &body.blocks {
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.local == local && place.projection.is_empty() {
                match rvalue {
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))) => {}
                    Rvalue::Use(Operand::Copy(_)) => {}
                    _ => return false,
                }
            } else if rvalue_mentions_local(rvalue, local) {
                if !matches!(
                    rvalue,
                    Rvalue::CallIntrinsic { name, args }
                        if *name == "gos_rt_rc_release"
                            && args.len() == 1
                            && whole_copy_local(&args[0]) == Some(local)
                ) {
                    return false;
                }
            }
        }
        if terminator_mentions_local_forbidden(&block.terminator, local) {
            return false;
        }
    }
    true
}

fn terminator_mentions_local_forbidden(term: &Terminator, local: Local) -> bool {
    match term {
        Terminator::Call { callee, args, .. } => {
            let is_parse = matches!(
                callee,
                Operand::Const(ConstValue::Str(name))
                    if name == "gos_rt_strconv_parse_i64" || name == "gos_rt_strconv_parse_f64"
            );
            for (idx, arg) in args.iter().enumerate() {
                if operand_mentions_local(arg, local) && !(is_parse && idx == 0) {
                    return true;
                }
            }
            operand_mentions_local(callee, local)
        }
        Terminator::SwitchInt { discriminant, .. } => operand_mentions_local(discriminant, local),
        Terminator::Assert { cond, .. } => operand_mentions_local(cond, local),
        Terminator::Drop { place, .. } => place_mentions_local(place, local),
        Terminator::Goto { .. }
        | Terminator::Return
        | Terminator::Unreachable
        | Terminator::Panic { .. } => false,
    }
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
/// plus its named binding), so this admits leaves of about ten to
/// fifteen source statements.
const INLINE_COST_LIMIT: usize = 40;

/// Promotion ceiling for callees that would otherwise exceed
/// [`INLINE_COST_LIMIT`]: a callee whose cost falls in the window
/// `(INLINE_COST_LIMIT, INLINE_CONST_ARG_COST_LIMIT]` is inlined only at
/// call sites passing at least one constant argument, where folding the
/// constant through the spliced body collapses constant-conditioned
/// branches (e.g. the JSON parser's `parse_val` / `parse_str` / `parse_num`
/// dispatched on a constant mode). Mid-window callees are still registered
/// as candidates so the application pass can make this per-call-site choice.
const INLINE_CONST_ARG_COST_LIMIT: usize = 60;

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
    // A `Projection::Index(local)` carries a runtime index local that lives in
    // the callee's local space; it must be remapped into the caller's appended
    // locals like the place root, or the spliced access reads a colliding
    // caller local (`a[i]` in a small inlined callee read the wrong index).
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
    // Register candidates up to the const-arg promotion ceiling; the
    // application pass admits the mid-window ones only at constant-fed
    // call sites.
    if cost > INLINE_CONST_ARG_COST_LIMIT {
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
    let def_to_name = def_to_name_map(bodies);
    let mut callees: HashMap<String, GeneralCallee> = HashMap::new();
    for b in bodies.iter() {
        // A self-recursive callee would re-inline its own body one level per
        // splice into every caller, bounded only by the growth budget. Refuse
        // to register it so recursion stays a real call. (Caller
        // self-recursion is already skipped in `inline_general_into`.)
        let self_recursive = b.blocks.iter().any(|blk| {
            matches!(&blk.terminator, Terminator::Call { callee, .. }
                if callee_body_name(callee, &def_to_name).as_deref() == Some(b.name.as_str()))
        });
        if self_recursive {
            continue;
        }
        if let Some(gc) = try_build_general(b) {
            callees.insert(b.name.clone(), gc);
        }
    }
    if callees.is_empty() {
        return;
    }
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
        // Callees over the base limit are in the promotion window: inline
        // them only when a call-site argument is a constant. The constant
        // flows into the spliced body and the follow-up `const_fold`
        // collapses the now-constant-conditioned branches that make the
        // larger body worth pulling in.
        let promoted = gc.cost > INLINE_COST_LIMIT;
        if promoted && !args.iter().any(|a| matches!(a, Operand::Const(_))) {
            bi += 1;
            continue;
        }
        if gc.cost > budget {
            bi += 1;
            continue;
        }
        budget -= gc.cost;
        splice_callee(body, bi, &gc.body, &args, &destination, continuation);
        if promoted {
            const_fold(body);
        }
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

/// If `stmt` is a whole-local `gos_rt_rc_release(S)` call, return `S`.
fn whole_release_local(stmt: &Statement) -> Option<Local> {
    let StatementKind::Assign {
        rvalue: Rvalue::CallIntrinsic { name, args },
        ..
    } = &stmt.kind
    else {
        return None;
    };
    if *name != "gos_rt_rc_release" || args.len() != 1 {
        return None;
    }
    match &args[0] {
        Operand::Copy(p) if p.projection.is_empty() => Some(p.local),
        _ => None,
    }
}

/// If `stmt` is `D = gos_rc_alloc(size, meta)` / `gos_rc_alloc_tagged(...)` to a
/// plain local, return `D`. Both lower to a header-ed payload whose
/// discriminant is applied by a later `gos_enum_set_disc` / `gos_enum_tag`
/// statement (untouched by the rewrite), so one reuse intrinsic serves both: a
/// reused or freshly-allocated block is filled and tagged identically.
fn rc_alloc_dest(stmt: &Statement) -> Option<Local> {
    let StatementKind::Assign {
        place,
        rvalue: Rvalue::CallIntrinsic { name, args },
    } = &stmt.kind
    else {
        return None;
    };
    if !matches!(*name, "gos_rc_alloc" | "gos_rc_alloc_tagged")
        || args.len() != 2
        || !place.projection.is_empty()
    {
        return None;
    }
    Some(place.local)
}

/// Perceus reuse pairing (`GOS_RC_NO_REUSE` disables it). Within a block, pair a
/// whole-local `gos_rt_rc_release(S)` with a later same-type
/// `D = gos_rc_alloc(size, meta)` constructor and rewrite the pair so S's block
/// is recycled in place:
///
/// ```text
///   gos_rt_rc_release(S)            token = gos_rt_rc_drop_reuse(S)
///   ...                       =>    ...
///   D = gos_rc_alloc(sz, m)         D = gos_rc_alloc_reuse(token, sz, m)
/// ```
///
/// Sound by construction: `gos_rt_rc_drop_reuse` does exactly what
/// `gos_rt_rc_release` does to S and its children (releasing them, cascading),
/// and only RETURNS S's block instead of freeing it - and only when S is the
/// unique thread-local, weak-free, unbuffered owner; otherwise it frees
/// normally and returns null, so `gos_rc_alloc_reuse` falls back to a fresh
/// allocation. A mis-pairing can therefore never corrupt, only forgo the reuse.
/// The pairing requires S unreferenced between the release and the constructor
/// (so the release simply moves to a drop-reuse at the constructor) and S dead
/// afterwards (guaranteed: the drop pass releases at last use). Covers both the
/// header-discriminant (`gos_rc_alloc`) and tagged (`gos_rc_alloc_tagged`)
/// enum constructors - the discriminant is applied by a separate later
/// statement, so one reuse intrinsic serves both. Region objects are skipped at
/// runtime (`drop_reuse` returns null for them). Compiled-tier only - the
/// bytecode VM never consumes this MIR.
// `ri` indexes both `used_release` and the block's statements and feeds
// `pairs`, so an index loop is the clear form here, not an iterator.
#[allow(clippy::needless_range_loop)]
// A cohesive three-phase pass (find pairs / mint tokens / rebuild block); it
// reads most clearly as one function.
#[allow(clippy::too_many_lines)]
pub(crate) fn insert_rc_reuse(body: &mut Body, tcx: &TyCtxt) {
    for bi in 0..body.blocks.len() {
        let n = body.blocks[bi].stmts.len();
        if n < 2 {
            continue;
        }
        // Phase A (read-only): find (release_idx, ctor_idx, S, type) pairs.
        let mut used_release = vec![false; n];
        let mut pairs: Vec<(usize, usize, Local, Ty)> = Vec::new();
        for ci in 0..n {
            let Some(d_local) = rc_alloc_dest(&body.blocks[bi].stmts[ci]) else {
                continue;
            };
            if (d_local.0 as usize) >= body.locals.len() {
                continue;
            }
            let dty = body.locals[d_local.0 as usize].ty;
            if !tcx.is_rc_managed(dty) {
                continue;
            }
            // The constructor must not itself reference a reuse candidate (it
            // names only the dest, size, and meta), so a stale candidate value
            // is never read by the construction.
            // Closest unused whole-release of the same type (not D), on EITHER
            // side of the constructor, with the candidate unreferenced in the
            // span between - so the release simply moves to a drop-reuse right
            // before the constructor. Releases land after the construct in the
            // common reassignment shape (`x = T::new(..)` releases the old `x`
            // after building the new value), so both directions matter.
            let mut best: Option<usize> = None;
            let mut best_dist = usize::MAX;
            for ri in 0..body.blocks[bi].stmts.len() {
                if ri == ci || used_release[ri] {
                    continue;
                }
                let Some(s_local) = whole_release_local(&body.blocks[bi].stmts[ri]) else {
                    continue;
                };
                if s_local == d_local || (s_local.0 as usize) >= body.locals.len() {
                    continue;
                }
                let sdecl = &body.locals[s_local.0 as usize];
                if sdecl.ty != dty || sdecl.region {
                    continue;
                }
                // Where the drop-reuse lands (just before the constructor)
                // versus where the release was determines what must be clear:
                //
                // * release BEFORE the constructor: the release only moves
                //   LATER, which can only extend a child's life, so just the
                //   span between is checked.
                // * release AFTER the constructor: the release moves EARLIER,
                //   which could free a child a constructor argument borrows
                //   (e.g. `x = T::new(x.child, ..)`), so S must be untouched
                //   from the block start through the whole window - excluding
                //   exactly that aliasing shape.
                let clash = if ri < ci {
                    (ri + 1..ci).any(|k| stmt_mentions_local(&body.blocks[bi].stmts[k], s_local))
                        || stmt_mentions_local(&body.blocks[bi].stmts[ci], s_local)
                } else {
                    (0..ri).any(|k| stmt_mentions_local(&body.blocks[bi].stmts[k], s_local))
                };
                if clash {
                    continue;
                }
                let dist = ci.abs_diff(ri);
                if dist < best_dist {
                    best_dist = dist;
                    best = Some(ri);
                }
            }
            if let Some(ri) = best {
                let s_local = whole_release_local(&body.blocks[bi].stmts[ri]).unwrap();
                used_release[ri] = true;
                pairs.push((ri, ci, s_local, dty));
            }
        }
        if pairs.is_empty() {
            continue;
        }
        // Phase B (mint token locals; typed as the constructor's RC type so they
        // lower to pointers, and minted after the drop passes so nothing
        // releases them).
        let mut ctor_to_token: HashMap<usize, (Local, Local)> = HashMap::new();
        let mut release_remove: std::collections::HashSet<usize> = std::collections::HashSet::new();
        for &(ri, ci, s_local, dty) in &pairs {
            let token = Local(u32::try_from(body.locals.len()).expect("local overflow"));
            body.locals.push(LocalDecl {
                ty: dty,
                debug_name: None,
                mutable: false,
                region: false,
            });
            ctor_to_token.insert(ci, (token, s_local));
            release_remove.insert(ri);
        }
        // Phase C (rebuild the block).
        let orig = std::mem::take(&mut body.blocks[bi].stmts);
        let mut out: Vec<Statement> = Vec::with_capacity(orig.len() + pairs.len());
        for (idx, stmt) in orig.into_iter().enumerate() {
            if release_remove.contains(&idx) {
                continue;
            }
            if let Some(&(token, s_local)) = ctor_to_token.get(&idx) {
                let span = stmt.span;
                out.push(Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(token),
                        rvalue: Rvalue::CallIntrinsic {
                            name: "gos_rt_rc_drop_reuse",
                            args: vec![Operand::Copy(Place::local(s_local))],
                        },
                    },
                    span,
                });
                let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::CallIntrinsic { args, .. },
                } = stmt.kind
                else {
                    unreachable!("ctor_to_token only indexes gos_rc_alloc statements");
                };
                let mut new_args = Vec::with_capacity(3);
                new_args.push(Operand::Copy(Place::local(token)));
                new_args.extend(args); // original [size, meta]
                out.push(Statement {
                    kind: StatementKind::Assign {
                        place,
                        rvalue: Rvalue::CallIntrinsic {
                            name: "gos_rc_alloc_reuse",
                            args: new_args,
                        },
                    },
                    span,
                });
                continue;
            }
            out.push(stmt);
        }
        body.blocks[bi].stmts = out;
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

/// Removes basic blocks unreachable from the entry block, renumbers the
/// survivors consecutively, and rewrites every terminator target through
/// the old-id -> new-id map. Runs after [`const_branch_elim`], which turns
/// constant `SwitchInt`s into `Goto`s and orphans the arms no longer taken;
/// without this sweep those arms linger as dead blocks that later passes
/// and codegen still walk. Preserves the `block.id == position` invariant
/// the verifier enforces.
pub fn dead_block_sweep(body: &mut Body) {
    use std::collections::VecDeque;
    if body.blocks.is_empty() {
        return;
    }
    let id_to_index: HashMap<u32, usize> = body
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id.0, i))
        .collect();
    let mut reachable = vec![false; body.blocks.len()];
    let entry = body.blocks[0].id;
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    if let Some(&i) = id_to_index.get(&entry.0) {
        reachable[i] = true;
        queue.push_back(entry);
    }
    while let Some(b) = queue.pop_front() {
        let Some(&i) = id_to_index.get(&b.0) else {
            continue;
        };
        for succ in block_successors(&body.blocks[i].terminator) {
            if let Some(&j) = id_to_index.get(&succ.0)
                && !reachable[j]
            {
                reachable[j] = true;
                queue.push_back(succ);
            }
        }
    }
    if reachable.iter().all(|&r| r) {
        return;
    }
    // Renumber the survivors consecutively in their current order, so the
    // entry block (always reachable, always first) stays `BlockId(0)`.
    let mut old_to_new: HashMap<u32, BlockId> = HashMap::new();
    let mut next = 0u32;
    for (i, block) in body.blocks.iter().enumerate() {
        if reachable[i] {
            old_to_new.insert(block.id.0, BlockId(next));
            next += 1;
        }
    }
    let blocks = std::mem::take(&mut body.blocks);
    let mut new_blocks = Vec::with_capacity(next as usize);
    for (i, mut block) in blocks.into_iter().enumerate() {
        if !reachable[i] {
            continue;
        }
        block.id = old_to_new[&block.id.0];
        remap_terminator_targets(&mut block.terminator, &old_to_new);
        new_blocks.push(block);
    }
    body.blocks = new_blocks;
}

/// Successor block ids of a terminator (every block it may transfer
/// control to).
fn block_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto { target } => vec![*target],
        Terminator::SwitchInt { arms, default, .. } => {
            let mut out: Vec<BlockId> = arms.iter().map(|(_, b)| *b).collect();
            out.push(*default);
            out
        }
        Terminator::Call { target, .. } => target.iter().copied().collect(),
        Terminator::Assert { target, .. } => vec![*target],
        Terminator::Drop { target, .. } => vec![*target],
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => Vec::new(),
    }
}

/// Rewrites every block target in `t` through `map` (old id -> new id).
/// Targets absent from the map belong to removed-unreachable blocks and
/// so cannot be reached from `t`; they are left as-is.
fn remap_terminator_targets(t: &mut Terminator, map: &HashMap<u32, BlockId>) {
    let remap = |b: &mut BlockId| {
        if let Some(&new) = map.get(&b.0) {
            *b = new;
        }
    };
    match t {
        Terminator::Goto { target } => remap(target),
        Terminator::SwitchInt { arms, default, .. } => {
            for (_, b) in arms.iter_mut() {
                remap(b);
            }
            remap(default);
        }
        Terminator::Call { target, .. } => {
            if let Some(b) = target {
                remap(b);
            }
        }
        Terminator::Assert { target, .. } => remap(target),
        Terminator::Drop { target, .. } => remap(target),
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => {}
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
            // Div/rem signedness is carried by the typed lowering context, not
            // by `ConstValue`; folding here would make `u64`/`usize` operands
            // at or above 2^63 indistinguishable from signed i64 values.
            BinOp::Div | BinOp::Rem => None,
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

// ---------------------------------------------------------------------------
// RC retain/release last-use elision (item 3).
// ---------------------------------------------------------------------------

/// The release helper that balances a given retain helper. `None` for any
/// name that is not a plain strong retain - weak / field / aggregate
/// retains are never paired here (a wrong pairing would unbalance a
/// reference count and free reachable memory).
fn rc_paired_release(retain: &str) -> Option<&'static str> {
    match retain {
        "gos_rt_rc_retain" => Some("gos_rt_rc_release"),
        "gos_rt_vec_retain" => Some("gos_rt_vec_free"),
        _ => None,
    }
}

/// The single bare-local argument of an RC accounting call, or `None`
/// when the call takes anything other than exactly one projection-free
/// `Copy(local)` (a field/weak/aggregate op, or a constant).
fn rc_bare_local_arg(args: &[Operand]) -> Option<Local> {
    if let [Operand::Copy(p)] = args
        && p.projection.is_empty()
    {
        return Some(p.local);
    }
    None
}

/// `true` when `place` reads or writes through `local` (root or an
/// `Index` projection local).
fn place_mentions_local(place: &Place, local: Local) -> bool {
    place.local == local
        || place
            .projection
            .iter()
            .any(|p| matches!(p, Projection::Index(l) if *l == local))
}

fn operand_mentions_local(op: &Operand, local: Local) -> bool {
    matches!(op, Operand::Copy(p) if place_mentions_local(p, local))
}

fn rvalue_mentions_local(rv: &Rvalue, local: Local) -> bool {
    match rv {
        Rvalue::Use(op)
        | Rvalue::UnaryOp { operand: op, .. }
        | Rvalue::Cast { operand: op, .. } => operand_mentions_local(op, local),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            operand_mentions_local(lhs, local) || operand_mentions_local(rhs, local)
        }
        Rvalue::Aggregate { operands, .. } => {
            operands.iter().any(|o| operand_mentions_local(o, local))
        }
        Rvalue::Repeat { value, .. } => operand_mentions_local(value, local),
        Rvalue::Len(p) | Rvalue::Ref { place: p, .. } => place_mentions_local(p, local),
        Rvalue::CallIntrinsic { args, .. } => args.iter().any(|o| operand_mentions_local(o, local)),
        Rvalue::StaticLoad(_) => false,
    }
}

/// `true` when `stmt` reads or writes `local` in any position.
fn stmt_mentions_local(stmt: &Statement, local: Local) -> bool {
    match &stmt.kind {
        StatementKind::Assign { place, rvalue } => {
            place_mentions_local(place, local) || rvalue_mentions_local(rvalue, local)
        }
        StatementKind::SetDiscriminant { place, .. } => place_mentions_local(place, local),
        StatementKind::StaticStore { value, .. } => operand_mentions_local(value, local),
        StatementKind::StorageLive(l) | StatementKind::StorageDead(l) => *l == local,
        StatementKind::Nop => false,
    }
}

/// `true` when `stmt` assigns the bare local `local` (a full
/// reassignment, not a projected field/element write).
fn stmt_writes_bare(stmt: &Statement, local: Local) -> bool {
    matches!(&stmt.kind, StatementKind::Assign { place, .. }
        if place.projection.is_empty() && place.local == local)
}

fn term_mentions_local(t: &Terminator, local: Local) -> bool {
    let m = |op: &Operand| operand_mentions_local(op, local);
    match t {
        Terminator::SwitchInt { discriminant, .. } => m(discriminant),
        Terminator::Call {
            callee,
            args,
            destination,
            ..
        } => m(callee) || args.iter().any(m) || place_mentions_local(destination, local),
        Terminator::Assert { cond, .. } => m(cond),
        _ => false,
    }
}

fn term_writes_bare(t: &Terminator, local: Local) -> bool {
    matches!(t, Terminator::Call { destination, .. }
        if destination.projection.is_empty() && destination.local == local)
}

/// Block successor indices.
fn successor_indices(t: &Terminator) -> Vec<usize> {
    match t {
        Terminator::Goto { target } => vec![target.0 as usize],
        Terminator::SwitchInt { arms, default, .. } => {
            let mut v: Vec<usize> = arms.iter().map(|(_, b)| b.0 as usize).collect();
            v.push(default.0 as usize);
            v
        }
        Terminator::Call { target, .. } => target.iter().map(|t| t.0 as usize).collect(),
        Terminator::Assert { target, .. } | Terminator::Drop { target, .. } => {
            vec![target.0 as usize]
        }
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => Vec::new(),
    }
}

/// Forward liveness probe: starting just after statement `after_stmt` in
/// `start_block`, returns `true` when `x` is read on some path before it
/// is overwritten. A bare reassignment (or a call destination) kills the
/// path; any other appearance of `x` - including an RC accounting call on
/// it - counts as a read and makes `x` live. Conservative: any uncertain
/// reach reports live so the caller keeps the retain/release pair.
fn local_live_after(
    body: &Body,
    succs: &[Vec<usize>],
    start_block: usize,
    after_stmt: usize,
    x: Local,
) -> bool {
    let n = body.blocks.len();
    let mut stack: Vec<(usize, usize)> = vec![(start_block, after_stmt + 1)];
    let mut visited = vec![false; n];
    while let Some((b, from)) = stack.pop() {
        let blk = &body.blocks[b];
        let mut killed = false;
        for sj in from..blk.stmts.len() {
            let st = &blk.stmts[sj];
            if stmt_writes_bare(st, x) {
                // `x = f(x)` reads the old value before overwriting.
                if let StatementKind::Assign { rvalue, .. } = &st.kind
                    && rvalue_mentions_local(rvalue, x)
                {
                    return true;
                }
                killed = true;
                break;
            }
            if stmt_mentions_local(st, x) {
                return true;
            }
        }
        if killed {
            continue;
        }
        let t = &blk.terminator;
        if term_writes_bare(t, x) {
            if let Terminator::Call { callee, args, .. } = t
                && (operand_mentions_local(callee, x)
                    || args.iter().any(|o| operand_mentions_local(o, x)))
            {
                return true;
            }
            continue;
        }
        if term_mentions_local(t, x) {
            return true;
        }
        for &s in &succs[b] {
            if !visited[s] {
                visited[s] = true;
                stack.push((s, 0));
            }
        }
    }
    false
}

/// RC retain/release last-use elision (item 3). Cancels a tightly
/// bracketed `retain(x)` / `release(x)` pair on a non-shared,
/// non-region, RC-managed local `x` whose reference is moved into a
/// surviving holder.
///
/// A pair is cancelled only when, conservatively, all hold:
/// - the retain is a plain strong retain (`gos_rt_rc_retain` /
///   `gos_rt_vec_retain`) on a bare local - never a field / weak /
///   aggregate accounting op;
/// - `x` is RC-managed, not a `region` local, and the goroutine-share
///   analysis ([`crate::ownership::ShareFacts`]) reports it not
///   goroutine-shared (a shared object carries the `SHARED_BIT` atomic
///   boundary, where another goroutine may concurrently adjust the count,
///   so the balanced pair is load-bearing for that protocol);
/// - the statement directly before the retain reads `x` (the forwarding
///   use whose new reference the retain accounts for) and does not
///   reassign it;
/// - the matching release (the type-paired opposite name) follows in the
///   same block with no other mention of `x` between the two - a tight
///   bracket on one object;
/// - `x` is dead on every path after the release.
///
/// Because the holder created by the forwarding use keeps its own
/// balanced release and `x` is dead, removing both members moves `x`'s
/// single share into that holder without changing the object's reference
/// count at any point outside the bracket. Both members are removed or
/// neither; a missed pair only keeps the original (correct) timing.
pub(crate) fn elide_redundant_rc_pairs(body: &mut Body, tcx: &TyCtxt) {
    let n_blocks = body.blocks.len();
    if n_blocks == 0 {
        return;
    }
    let n_locals = body.locals.len();
    let share = crate::ownership::ShareFacts::compute(body);
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| successor_indices(&b.terminator))
        .collect();

    let is_rc_local = |x: Local| -> bool {
        let i = x.0 as usize;
        i < n_locals && tcx.is_rc_managed(body.locals[i].ty) && !body.locals[i].region
    };

    let mut cancels: Vec<(usize, usize, usize)> = Vec::new();
    for bi in 0..n_blocks {
        let stmts = &body.blocks[bi].stmts;
        for ir in 0..stmts.len() {
            let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } = &stmts[ir].kind
            else {
                continue;
            };
            let Some(rel_name) = rc_paired_release(name) else {
                continue;
            };
            let Some(x) = rc_bare_local_arg(args) else {
                continue;
            };
            if !is_rc_local(x) || share.is_goroutine_shared(x) {
                continue;
            }
            // The forwarding use that the retain accounts for must sit
            // immediately before it and read (not reassign) `x`.
            if ir == 0 {
                continue;
            }
            let prev = &stmts[ir - 1];
            if stmt_writes_bare(prev, x) || !stmt_mentions_local(prev, x) {
                continue;
            }
            // Find the paired release: the first matching release of `x`
            // in this block, with no other mention of `x` in between.
            let mut paired: Option<usize> = None;
            for (j, stmt) in stmts.iter().enumerate().skip(ir + 1) {
                if let StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name: dn, args: da },
                    ..
                } = &stmt.kind
                    && *dn == rel_name
                    && rc_bare_local_arg(da) == Some(x)
                {
                    paired = Some(j);
                    break;
                }
                if stmt_mentions_local(stmt, x) {
                    break;
                }
            }
            let Some(id) = paired else {
                continue;
            };
            if local_live_after(body, &succs, bi, id, x) {
                continue;
            }
            // Collector-vs-stack-live contract (F13): a cycle-capable value
            // (a user struct/enum that can hold a back-reference) is a
            // potential trial-deletion candidate. Its retain accounts for the
            // stack reference; cancelling the retain/release pair leaves that
            // reference uncounted, so a trial deletion triggered by an
            // allocation safepoint inside the window would treat the value as
            // cycle-internal and reclaim a still-stack-live member. Keep the
            // pair when an allocation lies between the retain and its release.
            // Strings, vecs, and maps cannot form a collectable cycle, so
            // their pairs still cancel.
            let cycle_capable = matches!(
                body.locals.get(x.0 as usize).map(|d| tcx.kind_of(d.ty)),
                Some(gossamer_types::TyKind::Adt { .. })
            );
            if cycle_capable && block_allocates_between(&body.blocks[bi], ir, id) {
                continue;
            }
            cancels.push((bi, ir, id));
        }
    }
    if !cancels.is_empty() && std::env::var_os("GOS_RC_ELIDE_STATS").is_some() {
        eprintln!(
            "[rc-elide] {}: cancelled {} pair(s)",
            body.name,
            cancels.len()
        );
    }
    for (bi, ir, id) in cancels {
        body.blocks[bi].stmts[ir].kind = StatementKind::Nop;
        body.blocks[bi].stmts[id].kind = StatementKind::Nop;
    }
}

/// True when block `block` performs an RC allocation between statement
/// indices `from` and `to` (exclusive). A `gos_rc_alloc` can trip the
/// cycle collector's allocation-pressure trigger, so it is a collection
/// safepoint within the retain/release window.
fn block_allocates_between(block: &BasicBlock, from: usize, to: usize) -> bool {
    block
        .stmts
        .iter()
        .take(to)
        .skip(from + 1)
        .any(|s| match &s.kind {
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, .. },
                ..
            } => *name == "gos_rc_alloc" || *name == "gos_rc_alloc_tagged",
            _ => false,
        })
}

// ---------------------------------------------------------------------------
// Bounds-check elision (item 5).
// ---------------------------------------------------------------------------

/// Recognised counted-loop header: a `SwitchInt` on `counter < bound`
/// whose false arm exits and whose default re-enters the body.
struct CountedHeader {
    counter: Local,
    bound: Local,
    body_entry: usize,
    exit: usize,
}

/// Matches the `for i in 0..bound` header shape produced by the lowerer:
/// a final `cmp = Lt(Copy(counter), Copy(bound))` statement followed by
/// `SwitchInt(cmp, arms:[(0, exit)], default: body)`. Inclusive (`Le`)
/// comparisons and any other arm layout are rejected.
fn recognise_counted_header(block: &BasicBlock) -> Option<CountedHeader> {
    let Terminator::SwitchInt {
        discriminant: Operand::Copy(disc),
        arms,
        default,
    } = &block.terminator
    else {
        return None;
    };
    if !disc.projection.is_empty() || arms.len() != 1 || arms[0].0 != 0 {
        return None;
    }
    let cmp = disc.local;
    let exit = arms[0].1.0 as usize;
    let body_entry = default.0 as usize;
    if exit == body_entry {
        return None;
    }
    // The discriminant must be defined by the block's last statement as a
    // strict-less-than over two bare locals.
    let last = block.stmts.last()?;
    let StatementKind::Assign {
        place,
        rvalue:
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(lhs),
                rhs: Operand::Copy(rhs),
            },
    } = &last.kind
    else {
        return None;
    };
    if place.local != cmp
        || !place.projection.is_empty()
        || !lhs.projection.is_empty()
        || !rhs.projection.is_empty()
    {
        return None;
    }
    Some(CountedHeader {
        counter: lhs.local,
        bound: rhs.local,
        body_entry,
        exit,
    })
}

/// Computes the loop body region for a header. Returns the set of body
/// block indices (excluding the header and the exit) and the single
/// latch block whose terminator jumps back to the header. Bails (`None`)
/// on any irregular shape: more than one back edge to the header, or a
/// region block that escapes to a block other than the header or exit.
fn counted_loop_region(
    body: &Body,
    succs: &[Vec<usize>],
    header: usize,
    body_entry: usize,
    exit: usize,
) -> Option<(Vec<usize>, usize)> {
    if body_entry == header || body_entry == exit {
        return None;
    }
    let n = body.blocks.len();
    let mut in_region = vec![false; n];
    let mut stack = vec![body_entry];
    in_region[body_entry] = true;
    let mut order = Vec::new();
    while let Some(b) = stack.pop() {
        order.push(b);
        for &s in &succs[b] {
            if s == header || s == exit {
                continue;
            }
            if s >= n {
                return None;
            }
            if !in_region[s] {
                in_region[s] = true;
                stack.push(s);
            }
        }
    }
    // Closed-loop check + locate the single latch (back edge to header).
    let mut latch: Option<usize> = None;
    for &b in &order {
        let mut targets_header = false;
        for &s in &succs[b] {
            if s == header {
                targets_header = true;
            } else if s == exit {
                // a `break`-style early exit is fine
            } else if !in_region.get(s).copied().unwrap_or(false) {
                // escapes the loop to a third block - not a clean loop.
                return None;
            }
        }
        if targets_header {
            if latch.is_some() {
                return None;
            }
            latch = Some(b);
        }
    }
    let latch = latch?;
    // The latch must jump unconditionally back to the header.
    if !matches!(&body.blocks[latch].terminator, Terminator::Goto { target } if target.0 as usize == header)
    {
        return None;
    }
    Some((order, latch))
}

/// `true` when `local` is assigned (bare) by `stmt` or by a call
/// destination, inside the given block.
fn block_writes_local(block: &BasicBlock, local: Local) -> bool {
    block.stmts.iter().any(|s| stmt_writes_bare(s, local))
        || term_writes_bare(&block.terminator, local)
}

/// Verifies the counter is a monotone non-negative induction variable:
/// its only in-loop write is one `counter = counter + positive_const`
/// in the latch, and every definition outside the loop traces to a
/// non-negative value (so the monotone counter stays in `[0, bound)`).
fn verify_counter(
    body: &Body,
    region: &[usize],
    header: usize,
    latch: usize,
    counter: Local,
) -> bool {
    let in_loop = |b: usize| b == header || region.contains(&b);
    let mut latch_increment = false;
    let mut saw_outside_init = false;
    for (bi, block) in body.blocks.iter().enumerate() {
        let inside = in_loop(bi);
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.local != counter || !place.projection.is_empty() {
                continue;
            }
            if inside {
                // The only legal in-loop write is the latch increment.
                if bi != latch {
                    return false;
                }
                let positive_step = match rvalue {
                    Rvalue::BinaryOp {
                        op: BinOp::Add,
                        lhs: Operand::Copy(p),
                        rhs: Operand::Const(ConstValue::Int(k)),
                    }
                    | Rvalue::BinaryOp {
                        op: BinOp::Add,
                        lhs: Operand::Const(ConstValue::Int(k)),
                        rhs: Operand::Copy(p),
                    } => p.projection.is_empty() && p.local == counter && *k >= 1,
                    _ => false,
                };
                if !positive_step || latch_increment {
                    return false;
                }
                latch_increment = true;
            } else {
                // Pre-loop initialisation must be provably non-negative.
                if !value_traces_nonneg(body, &in_loop, place.local, &mut Vec::new()) {
                    return false;
                }
                saw_outside_init = true;
            }
        }
        // A call destination writing the counter (in or out of loop) is
        // not a provable induction variable.
        if term_writes_bare(&block.terminator, counter) {
            return false;
        }
    }
    latch_increment && saw_outside_init
}

/// `true` when every out-of-loop definition of `local` is a provably
/// non-negative value: a non-negative integer constant, a length-helper
/// result (always `>= 0`), or a bare copy of another such local. In-loop
/// definitions are ignored (the caller proves the in-loop write is a
/// positive increment separately). Bails on cycles, multiple-source
/// ambiguity, or any non-traceable definition.
fn value_traces_nonneg(
    body: &Body,
    in_loop: &impl Fn(usize) -> bool,
    local: Local,
    visited: &mut Vec<Local>,
) -> bool {
    if visited.contains(&local) {
        return false;
    }
    visited.push(local);
    let mut saw_def = false;
    for (bi, block) in body.blocks.iter().enumerate() {
        if in_loop(bi) {
            continue;
        }
        for stmt in &block.stmts {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.local != local || !place.projection.is_empty() {
                continue;
            }
            saw_def = true;
            match rvalue {
                Rvalue::Use(Operand::Const(ConstValue::Int(n))) if *n >= 0 => {}
                Rvalue::Use(Operand::Copy(p)) if p.projection.is_empty() => {
                    if !value_traces_nonneg(body, in_loop, p.local, visited) {
                        return false;
                    }
                }
                _ => return false,
            }
        }
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            destination,
            ..
        } = &block.terminator
            && destination.local == local
            && destination.projection.is_empty()
        {
            saw_def = true;
            if !is_len_helper(name) {
                return false;
            }
        }
    }
    saw_def
}

/// Length-helper runtime names: a call `name(vec)` returns `vec.len()`.
fn is_len_helper(name: &str) -> bool {
    matches!(name, "gos_rt_vec_len" | "gos_rt_len")
}

/// Returns the vec local that `bound` is the length of, following a
/// chain of bare copies back to a `len(vec)` call. The bound must have a
/// single definition along the chain and must not be written anywhere in
/// the loop. Rejects len arithmetic, other vecs (the caller pins the
/// receiver), and parameters (no in-body definition).
fn bound_traces_to_len(
    body: &Body,
    region: &[usize],
    header: usize,
    bound: Local,
) -> Option<Local> {
    let in_loop = |b: usize| b == header || region.contains(&b);
    // The bound must be loop-invariant.
    for (bi, block) in body.blocks.iter().enumerate() {
        if in_loop(bi) && block_writes_local(block, bound) {
            return None;
        }
    }
    let mut current = bound;
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 64 {
            return None;
        }
        // Find the unique definition of `current`.
        let mut def: Option<&Rvalue> = None;
        let mut call_len_arg: Option<Local> = None;
        let mut def_count = 0;
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind
                    && place.local == current
                    && place.projection.is_empty()
                {
                    def = Some(rvalue);
                    def_count += 1;
                }
            }
            if let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                destination,
                ..
            } = &block.terminator
                && destination.local == current
                && destination.projection.is_empty()
            {
                def_count += 1;
                if is_len_helper(name)
                    && let [Operand::Copy(p)] = args.as_slice()
                    && p.projection.is_empty()
                {
                    call_len_arg = Some(p.local);
                }
            }
        }
        if def_count != 1 {
            return None;
        }
        if let Some(vec) = call_len_arg {
            return Some(vec);
        }
        match def {
            Some(Rvalue::Use(Operand::Copy(p))) if p.projection.is_empty() => current = p.local,
            _ => return None,
        }
    }
}

/// Verifies the indexed vec `xs` is not mutated, reassigned, aliased, or
/// captured anywhere in the loop. Inside the loop `xs` may appear only as
/// the receiver (first argument) of an indexed get/set or a length read;
/// any other appearance (a push/pop, a copy, a borrow, a user call, a
/// reassignment) is disqualifying.
fn verify_vec_unmodified(body: &Body, region: &[usize], header: usize, xs: Local) -> bool {
    let in_loop = |b: usize| b == header || region.contains(&b);
    for (bi, block) in body.blocks.iter().enumerate() {
        if !in_loop(bi) {
            continue;
        }
        for stmt in &block.stmts {
            if stmt_mentions_local(stmt, xs) {
                return false;
            }
        }
        match &block.terminator {
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                destination,
                ..
            } if is_receiver_safe_call(name) => {
                if destination.local == xs && destination.projection.is_empty() {
                    return false;
                }
                // `xs` may appear only as arg0 (the receiver).
                for (i, a) in args.iter().enumerate() {
                    if i == 0 {
                        continue;
                    }
                    if operand_mentions_local(a, xs) {
                        return false;
                    }
                }
            }
            other => {
                if term_mentions_local(other, xs) {
                    return false;
                }
            }
        }
    }
    true
}

/// Runtime calls that read or write `xs` in place without changing its
/// length or capturing it - the only forms allowed to reference the
/// indexed vec inside a bounds-elided loop.
fn is_receiver_safe_call(name: &str) -> bool {
    matches!(
        name,
        "gos_rt_vec_get_i64"
            | "gos_rt_vec_set_i64"
            | "gos_rt_vec_get_i64_unchecked"
            | "gos_rt_vec_set_i64_unchecked"
            | "gos_rt_vec_len"
            | "gos_rt_len"
    )
}

/// `true` when `op` is exactly `Copy(counter)`, or a bare copy of a
/// local whose sole definition (in a loop block) is `Copy(counter)`. The
/// counter is only written at the latch, so any in-body snapshot of it
/// equals the header-checked value, which lies in `[0, bound)`.
fn index_is_counter(body: &Body, region: &[usize], counter: Local, op: &Operand) -> bool {
    let Operand::Copy(p) = op else {
        return false;
    };
    if !p.projection.is_empty() {
        return false;
    }
    if p.local == counter {
        return true;
    }
    // Otherwise: a single in-loop definition `idx = Copy(counter)`.
    let idx = p.local;
    let mut def_in_region = false;
    let mut def_count = 0;
    for (bi, block) in body.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            if stmt_writes_bare(stmt, idx) {
                def_count += 1;
                let is_counter_copy = matches!(&stmt.kind,
                    StatementKind::Assign { rvalue: Rvalue::Use(Operand::Copy(s)), .. }
                        if s.projection.is_empty() && s.local == counter);
                if is_counter_copy && region.contains(&bi) {
                    def_in_region = true;
                }
            }
        }
        if term_writes_bare(&block.terminator, idx) {
            def_count += 1;
        }
    }
    def_count == 1 && def_in_region
}

/// Element kind of `xs` is a single-slot scalar safe for the unchecked
/// i64-shaped path: integer, bool, char, or `f64` (all read/written as an
/// 8-byte or 1-byte word by the unchecked inline). Excludes RC-managed
/// elements and `f32` (a 4-byte slot the word/byte inline does not cover).
fn vec_elem_is_unchecked_scalar(body: &Body, tcx: &TyCtxt, xs: Local) -> bool {
    let mut ty = body.local_ty(xs);
    if let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
        ty = *inner;
    }
    let elem = match tcx.kind_of(ty) {
        TyKind::Vec(e) | TyKind::Slice(e) => *e,
        _ => return false,
    };
    matches!(
        tcx.kind_of(elem),
        TyKind::Int(_) | TyKind::Bool | TyKind::Char | TyKind::Float(gossamer_types::FloatTy::F64)
    )
}

/// Conservative bounds-check elision (item 5). For each recognised
/// counted loop `for i in 0..xs.len()` whose counter is a monotone
/// non-negative induction variable, whose bound is exactly `xs.len()`,
/// and whose `xs` is provably not mutated / aliased / captured in the
/// loop, rewrites every `gos_rt_vec_get_i64` / `gos_rt_vec_set_i64` whose
/// receiver is `xs` and whose index is the counter to the `_unchecked`
/// callee. The index is provably in `[0, len)` and the receiver non-null
/// (a null vec has length 0, so the body never runs), so the unchecked
/// store/load is behaviour-identical to the checked one. Bails closed on
/// anything unprovable; the compiled tiers that do not honour the
/// unchecked form resolve it back to the checked symbol.
pub(crate) fn bounds_check_elim(body: &mut Body, tcx: &TyCtxt) {
    let n_blocks = body.blocks.len();
    if n_blocks == 0 {
        return;
    }
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| successor_indices(&b.terminator))
        .collect();

    // (block_index, new_callee_name) for each rewritable get/set.
    let mut rewrites: Vec<(usize, &'static str)> = Vec::new();
    for h in 0..n_blocks {
        let Some(header) = recognise_counted_header(&body.blocks[h]) else {
            continue;
        };
        let Some((region, latch)) =
            counted_loop_region(body, &succs, h, header.body_entry, header.exit)
        else {
            continue;
        };
        if !verify_counter(body, &region, h, latch, header.counter) {
            continue;
        }
        let Some(xs) = bound_traces_to_len(body, &region, h, header.bound) else {
            continue;
        };
        if !verify_vec_unmodified(body, &region, h, xs) {
            continue;
        }
        if !vec_elem_is_unchecked_scalar(body, tcx, xs) {
            continue;
        }
        for &b in &region {
            let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                ..
            } = &body.blocks[b].terminator
            else {
                continue;
            };
            let (unchecked, idx_arg) = match name.as_str() {
                "gos_rt_vec_get_i64" if args.len() == 2 => {
                    ("gos_rt_vec_get_i64_unchecked", &args[1])
                }
                "gos_rt_vec_set_i64" if args.len() == 3 => {
                    ("gos_rt_vec_set_i64_unchecked", &args[1])
                }
                _ => continue,
            };
            if !operand_mentions_local(&args[0], xs)
                || !matches!(&args[0], Operand::Copy(p) if p.local == xs && p.projection.is_empty())
            {
                continue;
            }
            if !index_is_counter(body, &region, header.counter, idx_arg) {
                continue;
            }
            rewrites.push((b, unchecked));
        }
    }
    for (b, name) in rewrites {
        if let Terminator::Call { callee, .. } = &mut body.blocks[b].terminator {
            *callee = Operand::Const(ConstValue::Str(name.to_string()));
        }
    }
}

fn switch_true_successor(arms: &[(i128, BlockId)], default: BlockId) -> usize {
    let mut true_target: Option<BlockId> = None;
    for (value, target) in arms {
        if *value == 1 {
            true_target = Some(*target);
            break;
        }
    }
    let target = true_target.unwrap_or(default);
    target.0 as usize
}

enum BranchBoundFact {
    IndexLtLen { index: Local, xs: Local },
    LenPositive { len: Local, xs: Local },
}

fn branch_bound_fact(body: &Body, block_index: usize) -> Option<(usize, BranchBoundFact)> {
    let block = &body.blocks[block_index];
    let Terminator::SwitchInt {
        discriminant: Operand::Copy(disc),
        arms,
        default,
    } = &block.terminator
    else {
        return None;
    };
    if !disc.projection.is_empty() {
        return None;
    }
    let true_successor = switch_true_successor(arms, *default);
    if true_successor >= body.blocks.len() {
        return None;
    }
    let cmp = unique_def_rvalue(body, disc.local)?;
    let Rvalue::BinaryOp { op, lhs, rhs } = cmp else {
        return None;
    };
    match (op, lhs, rhs) {
        (BinOp::Lt, Operand::Copy(idx), Operand::Copy(bound))
            if idx.projection.is_empty() && bound.projection.is_empty() =>
        {
            let xs = bound_traces_to_len(body, &[], block_index, bound.local)?;
            let in_loop = |_| false;
            if value_traces_nonneg(body, &in_loop, idx.local, &mut Vec::new()) {
                return Some((
                    true_successor,
                    BranchBoundFact::IndexLtLen {
                        index: idx.local,
                        xs,
                    },
                ));
            }
            None
        }
        (BinOp::Lt, Operand::Const(ConstValue::Int(0)), Operand::Copy(bound))
        | (BinOp::Gt, Operand::Copy(bound), Operand::Const(ConstValue::Int(0)))
            if bound.projection.is_empty() =>
        {
            let xs = bound_traces_to_len(body, &[], block_index, bound.local)?;
            Some((
                true_successor,
                BranchBoundFact::LenPositive {
                    len: bound.local,
                    xs,
                },
            ))
        }
        _ => None,
    }
}

fn local_is_len_minus_one(body: &Body, local: Local, len: Local) -> bool {
    matches!(
        unique_def_rvalue(body, local),
        Some(Rvalue::BinaryOp {
            op: BinOp::Sub,
            lhs: Operand::Copy(lhs),
            rhs: Operand::Const(ConstValue::Int(1)),
        }) if lhs.projection.is_empty() && lhs.local == len
    )
}

fn guarded_successor_has_no_vec_side_effects(block: &BasicBlock, xs: Local) -> bool {
    block
        .stmts
        .iter()
        .all(|stmt| !stmt_mentions_local(stmt, xs))
}

/// Branch-local bounds facts for heap-style code. Rewrites a checked scalar
/// Vec get/set in the proven true successor of either `idx < xs.len()` or
/// `xs.len() > 0` followed by `last = len - 1`. This deliberately does not
/// clone blocks or reason across arbitrary statements; it only removes the
/// runtime guard when local control flow proves the single terminator access.
pub(crate) fn local_branch_bounds_check_elim(body: &mut Body, tcx: &TyCtxt) {
    let mut rewrites: Vec<(usize, &'static str)> = Vec::new();
    for h in 0..body.blocks.len() {
        let Some((successor, fact)) = branch_bound_fact(body, h) else {
            continue;
        };
        let block = &body.blocks[successor];
        let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            args,
            ..
        } = &block.terminator
        else {
            continue;
        };
        let Some(unchecked) = unchecked_variant(name) else {
            continue;
        };
        let idx_arg = match (name.as_str(), args.as_slice()) {
            ("gos_rt_vec_get_i64", [_, idx]) => idx,
            ("gos_rt_vec_set_i64", [_, idx, _]) => idx,
            _ => continue,
        };
        let Some(Operand::Copy(receiver)) = args.first() else {
            continue;
        };
        if !receiver.projection.is_empty()
            || !guarded_successor_has_no_vec_side_effects(block, receiver.local)
        {
            continue;
        }
        if !vec_elem_is_unchecked_scalar(body, tcx, receiver.local) {
            continue;
        }
        let Operand::Copy(idx_place) = idx_arg else {
            continue;
        };
        if !idx_place.projection.is_empty() {
            continue;
        }
        let proven = match fact {
            BranchBoundFact::IndexLtLen { index, xs } => {
                receiver.local == xs
                    && idx_place.local == index
                    && !block_writes_local(block, index)
            }
            BranchBoundFact::LenPositive { len, xs } => {
                receiver.local == xs
                    && !block_writes_local(block, len)
                    && local_is_len_minus_one(body, idx_place.local, len)
            }
        };
        if proven {
            rewrites.push((successor, unchecked));
        }
    }
    for (b, name) in rewrites {
        if let Terminator::Call { callee, .. } = &mut body.blocks[b].terminator {
            *callee = Operand::Const(ConstValue::Str(name.to_string()));
        }
    }
}

/// A scalar vec element access `xs[base + counter]` inside a counted loop
/// whose index is an affine function of the loop counter with coefficient
/// one. `base` is a loop-invariant operand: `Const(0)` for a bare
/// `xs[counter]`, `Const(-k)` for `xs[counter - k]`, or a copy of a
/// loop-invariant local for `xs[inv + counter]`.
struct AffineAccess {
    block: usize,
    xs: Local,
    base: Operand,
}

/// Returns the single `Assign` rvalue defining `local` as a bare place,
/// or `None` when there is not exactly one such definition (a call
/// destination writing `local` also counts and disqualifies it).
fn unique_def_rvalue(body: &Body, local: Local) -> Option<&Rvalue> {
    let mut found: Option<&Rvalue> = None;
    let mut count = 0usize;
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.local == local
                && place.projection.is_empty()
            {
                found = Some(rvalue);
                count += 1;
            }
        }
        if term_writes_bare(&block.terminator, local) {
            count += 1;
        }
    }
    if count == 1 { found } else { None }
}

/// `true` when `l` is neither the counter nor written anywhere in the loop
/// (header + region) - i.e. loop-invariant.
fn local_is_loop_invariant(
    body: &Body,
    header: usize,
    region: &[usize],
    counter: Local,
    l: Local,
) -> bool {
    if l == counter {
        return false;
    }
    let in_loop = |b: usize| b == header || region.contains(&b);
    !body
        .blocks
        .iter()
        .enumerate()
        .any(|(bi, blk)| in_loop(bi) && block_writes_local(blk, l))
}

/// If `op` is a loop-invariant base operand (an integer constant or a copy
/// of a loop-invariant local), returns it cloned; otherwise `None`.
fn invariant_base(
    body: &Body,
    header: usize,
    region: &[usize],
    counter: Local,
    op: &Operand,
) -> Option<Operand> {
    match op {
        Operand::Const(ConstValue::Int(_)) => Some(op.clone()),
        Operand::Copy(p)
            if p.projection.is_empty()
                && local_is_loop_invariant(body, header, region, counter, p.local) =>
        {
            Some(op.clone())
        }
        _ => None,
    }
}

/// Extracts the loop-invariant `base` of an affine index `base + counter`
/// from the index operand of a vec get/set. Handles `xs[counter]`
/// (base 0), `xs[inv + counter]` / `xs[counter + inv]`, `xs[counter + k]`,
/// and `xs[counter - k]` (base `-k`). Returns `None` for anything else.
fn affine_base(
    body: &Body,
    header: usize,
    region: &[usize],
    counter: Local,
    idx: &Operand,
) -> Option<Operand> {
    let Operand::Copy(p) = idx else {
        return None;
    };
    if !p.projection.is_empty() {
        return None;
    }
    if p.local == counter {
        return Some(Operand::Const(ConstValue::Int(0)));
    }
    let def = unique_def_rvalue(body, p.local)?;
    let is_counter = |op: &Operand| index_is_counter(body, region, counter, op);
    match def {
        Rvalue::Use(op) if is_counter(op) => Some(Operand::Const(ConstValue::Int(0))),
        Rvalue::BinaryOp {
            op: BinOp::Add,
            lhs,
            rhs,
        } => {
            if is_counter(lhs) {
                invariant_base(body, header, region, counter, rhs)
            } else if is_counter(rhs) {
                invariant_base(body, header, region, counter, lhs)
            } else {
                None
            }
        }
        Rvalue::BinaryOp {
            op: BinOp::Sub,
            lhs,
            rhs,
        } if is_counter(lhs) => {
            if let Operand::Const(ConstValue::Int(k)) = rhs {
                Some(Operand::Const(ConstValue::Int(k.checked_neg()?)))
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The `_unchecked` runtime symbol paired with a checked scalar vec
/// get/set, or `None` for any other callee.
fn unchecked_variant(name: &str) -> Option<&'static str> {
    match name {
        "gos_rt_vec_get_i64" => Some("gos_rt_vec_get_i64_unchecked"),
        "gos_rt_vec_set_i64" => Some("gos_rt_vec_set_i64_unchecked"),
        _ => None,
    }
}

/// Rewrites every block reference in `term` through `map` (old index ->
/// new index); references not in `map` are left unchanged.
fn remap_terminator_blocks(term: &mut Terminator, map: &HashMap<usize, usize>) {
    let f = |id: &mut BlockId| {
        if let Some(&n) = map.get(&(id.0 as usize)) {
            *id = BlockId(n as u32);
        }
    };
    match term {
        Terminator::Goto { target } => f(target),
        Terminator::SwitchInt { arms, default, .. } => {
            for (_, t) in arms.iter_mut() {
                f(t);
            }
            f(default);
        }
        Terminator::Call { target, .. } => {
            if let Some(t) = target {
                f(t);
            }
        }
        Terminator::Assert { target, .. } => f(target),
        Terminator::Drop { target, .. } => f(target),
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => {}
    }
}

/// Redirects every block reference in `term` equal to `from` to `to`.
fn redirect_terminator_target(term: &mut Terminator, from: usize, to: BlockId) {
    let f = |id: &mut BlockId| {
        if id.0 as usize == from {
            *id = to;
        }
    };
    match term {
        Terminator::Goto { target } => f(target),
        Terminator::SwitchInt { arms, default, .. } => {
            for (_, t) in arms.iter_mut() {
                f(t);
            }
            f(default);
        }
        Terminator::Call { target, .. } => {
            if let Some(t) = target {
                f(t);
            }
        }
        Terminator::Assert { target, .. } => f(target),
        Terminator::Drop { target, .. } => f(target),
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => {}
    }
}

/// Bounds-check elision via loop versioning - the general affine form of
/// [`bounds_check_elim`]. For each innermost counted loop
/// `for counter in lo..bound`, collects every scalar vec access
/// `xs[base + counter]` with a loop-invariant `base` and an unmodified
/// receiver, then emits a guarded duplicate of the loop. A preheader
/// computes, per distinct `(xs, base)`, the runtime precondition
/// `base + lo >= 0 && base + bound <= xs.len()` - rearranged to
/// `base >= -lo` and `base <= xs.len() - bound` so the sums never overflow
/// while the loop runs - and branches to an unchecked clone of the loop
/// when every precondition holds, or the original checked loop otherwise.
/// Inside the clone the affine accesses use the `_unchecked` runtime
/// symbols, which both compiled tiers lower to a branch-free load/store the
/// vectoriser can prove independent. Semantics are preserved: the
/// unchecked path runs only when every index is proven in `[0, len)`,
/// matching the checked path's in-bounds behaviour exactly.
pub(crate) fn bounds_check_versioning(body: &mut Body, tcx: &TyCtxt) {
    let headers: Vec<usize> = (0..body.blocks.len())
        .filter(|&h| recognise_counted_header(&body.blocks[h]).is_some())
        .collect();
    for h in headers {
        try_version_loop(body, tcx, h);
    }
}

/// Attempts to version the counted loop headed at block `h`. A no-op when
/// the loop is not a clean innermost counted loop, has no versionable
/// affine access, or sits at the function entry.
fn try_version_loop(body: &mut Body, tcx: &TyCtxt, h: usize) {
    if h == 0 {
        return;
    }
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| successor_indices(&b.terminator))
        .collect();
    let Some(header) = recognise_counted_header(&body.blocks[h]) else {
        return;
    };
    let Some((region, latch)) =
        counted_loop_region(body, &succs, h, header.body_entry, header.exit)
    else {
        return;
    };
    if !verify_counter(body, &region, h, latch, header.counter) {
        return;
    }
    let counter = header.counter;
    let bound = header.bound;
    if !local_is_loop_invariant(body, h, &region, counter, bound) {
        return;
    }
    // Innermost loops only: versioning nested outer loops would duplicate
    // an already-versioned inner loop and tangle the region analysis.
    if region
        .iter()
        .any(|&b| recognise_counted_header(&body.blocks[b]).is_some())
    {
        return;
    }

    let loop_blocks: Vec<usize> = std::iter::once(h).chain(region.iter().copied()).collect();
    let cands = collect_affine_candidates(body, tcx, h, counter, &region, &loop_blocks);
    if cands.is_empty() {
        return;
    }
    emit_loop_version(body, h, counter, bound, &loop_blocks, &cands);
}

/// Collects every scalar vec access `xs[base + counter]` in the loop with a
/// loop-invariant `base` and a receiver that is provably unmodified and of
/// an unchecked-scalar element type.
fn collect_affine_candidates(
    body: &Body,
    tcx: &TyCtxt,
    h: usize,
    counter: Local,
    region: &[usize],
    loop_blocks: &[usize],
) -> Vec<AffineAccess> {
    let mut cands: Vec<AffineAccess> = Vec::new();
    let mut verified: HashMap<Local, bool> = HashMap::new();
    for &b in loop_blocks {
        let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            args,
            ..
        } = &body.blocks[b].terminator
        else {
            continue;
        };
        let idx_i = match name.as_str() {
            "gos_rt_vec_get_i64" if args.len() == 2 => 1,
            "gos_rt_vec_set_i64" if args.len() == 3 => 1,
            _ => continue,
        };
        let Operand::Copy(recv) = &args[0] else {
            continue;
        };
        if !recv.projection.is_empty() {
            continue;
        }
        let xs = recv.local;
        let ok = *verified.entry(xs).or_insert_with(|| {
            verify_vec_unmodified(body, region, h, xs)
                && vec_elem_is_unchecked_scalar(body, tcx, xs)
        });
        if !ok {
            continue;
        }
        let Some(base) = affine_base(body, h, region, counter, &args[idx_i]) else {
            continue;
        };
        cands.push(AffineAccess { block: b, xs, base });
    }
    cands
}

/// Clones the loop blocks `loop_blocks` into fresh appended blocks starting
/// at index `n0`, remapping in-loop successors and routing every candidate
/// access through its `_unchecked` runtime symbol.
fn clone_loop_unchecked(
    body: &Body,
    loop_blocks: &[usize],
    n0: usize,
    cand_blocks: &std::collections::HashSet<usize>,
) -> Vec<BasicBlock> {
    let mut map: HashMap<usize, usize> = HashMap::new();
    for (i, &ob) in loop_blocks.iter().enumerate() {
        map.insert(ob, n0 + i);
    }
    let mut clones: Vec<BasicBlock> = Vec::with_capacity(loop_blocks.len());
    for &ob in loop_blocks {
        let mut blk = body.blocks[ob].clone();
        blk.id = BlockId(map[&ob] as u32);
        remap_terminator_blocks(&mut blk.terminator, &map);
        if cand_blocks.contains(&ob)
            && let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                ..
            } = &mut blk.terminator
            && let Some(u) = unchecked_variant(name)
        {
            *name = u.to_string();
        }
        clones.push(blk);
    }
    clones
}

/// Allocates a fresh, immutable, non-region local of type `ty`.
fn fresh_local(body: &mut Body, ty: Ty) -> Local {
    let l = Local(u32::try_from(body.locals.len()).expect("local overflow"));
    body.locals.push(LocalDecl {
        ty,
        debug_name: None,
        mutable: false,
        region: false,
    });
    l
}

/// Stable context shared by every preheader comparison block.
struct PreheaderCtx {
    i64t: Ty,
    boolt: Ty,
    sp: Span,
    checked: BlockId,
}

/// One precondition comparison `base <cmp> (arith_lhs - arith_rhs)`. A false
/// result short-circuits to the checked loop.
struct RangeCheck {
    arith_lhs: Operand,
    arith_rhs: Operand,
    base: Operand,
    cmp: BinOp,
}

/// Builds one preheader comparison block at index `idx`: computes
/// `tmp = arith_lhs - arith_rhs`, then `c = base <cmp> tmp`, and branches to
/// the checked loop when `c` is false or to `next` otherwise.
fn range_check_block(
    body: &mut Body,
    ctx: &PreheaderCtx,
    idx: usize,
    next: BlockId,
    check: RangeCheck,
) -> BasicBlock {
    let tmp = fresh_local(body, ctx.i64t);
    let c = fresh_local(body, ctx.boolt);
    BasicBlock {
        id: BlockId(idx as u32),
        stmts: vec![
            Statement {
                kind: StatementKind::Assign {
                    place: Place::local(tmp),
                    rvalue: Rvalue::BinaryOp {
                        op: BinOp::Sub,
                        lhs: check.arith_lhs,
                        rhs: check.arith_rhs,
                    },
                },
                span: ctx.sp,
            },
            Statement {
                kind: StatementKind::Assign {
                    place: Place::local(c),
                    rvalue: Rvalue::BinaryOp {
                        op: check.cmp,
                        lhs: check.base,
                        rhs: Operand::Copy(Place::local(tmp)),
                    },
                },
                span: ctx.sp,
            },
        ],
        terminator: Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(c)),
            arms: vec![(0, ctx.checked)],
            default: next,
        },
        span: ctx.sp,
    }
}

/// Emits the versioned form of the loop: an unchecked clone, a preheader
/// that proves every candidate's affine index in range, and a redirect of
/// the loop's external entry edges into that preheader.
fn emit_loop_version(
    body: &mut Body,
    h: usize,
    counter: Local,
    bound: Local,
    loop_blocks: &[usize],
    cands: &[AffineAccess],
) {
    // Distinct `(xs, base)` precondition checks and the distinct receivers
    // whose length the preheader must read.
    let mut checks: Vec<(Local, Operand)> = Vec::new();
    for c in cands {
        if !checks.iter().any(|(x, bs)| *x == c.xs && bs == &c.base) {
            checks.push((c.xs, c.base.clone()));
        }
    }
    let mut xs_list: Vec<Local> = Vec::new();
    for (x, _) in &checks {
        if !xs_list.contains(x) {
            xs_list.push(*x);
        }
    }

    let ctx = PreheaderCtx {
        i64t: body.local_ty(counter),
        boolt: match &body.blocks[h].terminator {
            Terminator::SwitchInt {
                discriminant: Operand::Copy(d),
                ..
            } => body.local_ty(d.local),
            _ => return,
        },
        sp: body.blocks[h].span,
        checked: BlockId(h as u32),
    };

    let n0 = body.blocks.len();
    let cand_blocks: std::collections::HashSet<usize> = cands.iter().map(|c| c.block).collect();
    let clone_blocks = clone_loop_unchecked(body, loop_blocks, n0, &cand_blocks);

    // Preheader: one length read per distinct receiver, then a short-circuit
    // chain of comparisons dispatching to the unchecked clone (index `n0`)
    // when every precondition holds or the original checked loop otherwise.
    let mut len_of: HashMap<Local, Local> = HashMap::new();
    for &x in &xs_list {
        let l = fresh_local(body, ctx.i64t);
        len_of.insert(x, l);
    }
    let pbase = n0 + clone_blocks.len();
    let total = xs_list.len() + 2 * checks.len();
    let unchecked_header = BlockId(n0 as u32);
    let next_of = |p: usize| -> BlockId {
        if p + 1 < total {
            BlockId((pbase + p + 1) as u32)
        } else {
            unchecked_header
        }
    };
    let mut pre: Vec<BasicBlock> = Vec::with_capacity(total);
    for (i, &x) in xs_list.iter().enumerate() {
        pre.push(BasicBlock {
            id: BlockId((pbase + i) as u32),
            stmts: Vec::new(),
            terminator: Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
                args: vec![Operand::Copy(Place::local(x))],
                destination: Place::local(len_of[&x]),
                target: Some(next_of(i)),
            },
            span: ctx.sp,
        });
    }
    for (j, (x, base)) in checks.iter().enumerate() {
        let p_hi = xs_list.len() + 2 * j;
        // Upper bound: base <= len(xs) - bound  <=>  base + bound <= len.
        let hi = range_check_block(
            body,
            &ctx,
            pbase + p_hi,
            next_of(p_hi),
            RangeCheck {
                arith_lhs: Operand::Copy(Place::local(len_of[x])),
                arith_rhs: Operand::Copy(Place::local(bound)),
                base: base.clone(),
                cmp: BinOp::Le,
            },
        );
        pre.push(hi);
        // Lower bound: base >= -lo, with lo = counter's value at the
        // preheader (its non-negative loop-entry init).
        let lo = range_check_block(
            body,
            &ctx,
            pbase + p_hi + 1,
            next_of(p_hi + 1),
            RangeCheck {
                arith_lhs: Operand::Const(ConstValue::Int(0)),
                arith_rhs: Operand::Copy(Place::local(counter)),
                base: base.clone(),
                cmp: BinOp::Ge,
            },
        );
        pre.push(lo);
    }

    body.blocks.extend(clone_blocks);
    body.blocks.extend(pre);

    // External entry edges (original blocks outside the loop) now enter the
    // preheader; the latch's back edge to the checked header is untouched.
    let entry_target = BlockId(pbase as u32);
    let loop_set: std::collections::HashSet<usize> = loop_blocks.iter().copied().collect();
    for bi in 0..n0 {
        if loop_set.contains(&bi) {
            continue;
        }
        redirect_terminator_target(&mut body.blocks[bi].terminator, h, entry_target);
    }
}

/// Argument index of the value operand for a container-insert runtime
/// call whose stored entry aliases the value's heap subgraph, or `None`
/// for any other callee. The map-insert family and `BTreeMap::insert`
/// carry the value at index 2 (`m, key, value`); `HashSet::insert` at
/// index 1 (`set, element`).
fn container_insert_value_arg(callee: &str) -> Option<usize> {
    match callee {
        "gos_rt_map_insert"
        | "gos_rt_map_insert_i64_i64"
        | "gos_rt_map_insert_str_i64"
        | "gos_rt_map_insert_i64_str"
        | "gos_rt_map_insert_str_str"
        | "gos_rt_btmap_insert" => Some(2),
        "gos_rt_set_insert" => Some(1),
        _ => None,
    }
}

/// Move-into-container ownership transfer. A value inserted into a
/// `HashMap` / `BTreeMap` / `HashSet` is heap-copied into the entry, and the
/// copy aliases the source value's `String` / `Vec` / nested-aggregate
/// children (the entry shares the single owning reference). The source
/// binding's drop must therefore not release those children, or the
/// stored entry is left pointing at freed memory. The container holds
/// the reference until the entry is popped - where the receiving binding
/// releases it - or the container itself is dropped.
///
/// Removing a release can only delay a free, never free early, so this
/// transform cannot introduce a use-after-free or double-free: an entry
/// that is never popped leaks its children rather than corrupting the
/// heap. It runs after the drop-insertion pipeline so the releases it
/// cancels are already materialised.
pub(crate) fn suppress_container_moved_releases(body: &mut Body) {
    let mut rooted: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();
    for block in &body.blocks {
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            args,
            ..
        } = &block.terminator
            && let Some(vidx) = container_insert_value_arg(name)
            && let Some(Operand::Copy(p)) = args.get(vidx)
            && p.projection.is_empty()
        {
            rooted.insert(p.local.0);
        }
    }
    if rooted.is_empty() {
        return;
    }
    for block in &mut body.blocks {
        for stmt in &mut block.stmts {
            if let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } = &stmt.kind
                && matches!(
                    *name,
                    "gos_rt_rc_release" | "gos_rt_str_free" | "gos_rt_vec_free"
                )
                && args.len() == 1
                && let Some(Operand::Copy(p)) = args.first()
                && rooted.contains(&p.local.0)
                && p.projection
                    .iter()
                    .all(|proj| matches!(proj, Projection::Field(_)))
            {
                stmt.kind = StatementKind::Nop;
            }
        }
    }
}

#[cfg(test)]
mod elision_tests {
    use gossamer_lex::{SourceMap, Span};
    use gossamer_types::TyCtxt;

    use super::{
        bounds_check_elim, elide_redundant_rc_pairs, fuse_slice_parse_ranges,
        local_branch_bounds_check_elim, reserve_vecs_for_counted_push_loops,
    };
    use crate::ir::{
        BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Projection,
        Rvalue, Statement, StatementKind, Terminator,
    };

    fn span() -> Span {
        let mut map = SourceMap::new();
        Span::new(map.add_file("t.gos", ""), 0, 0)
    }

    fn decl(ty: gossamer_types::Ty) -> LocalDecl {
        LocalDecl {
            ty,
            debug_name: None,
            mutable: false,
            region: false,
        }
    }

    fn assign(place: Place, rvalue: Rvalue) -> Statement {
        Statement {
            kind: StatementKind::Assign { place, rvalue },
            span: span(),
        }
    }

    fn copy(dst: u32, src: u32) -> Statement {
        assign(
            Place::local(Local(dst)),
            Rvalue::Use(Operand::Copy(Place::local(Local(src)))),
        )
    }

    fn rc_call(dst: u32, name: &'static str, arg: Place) -> Statement {
        assign(
            Place::local(Local(dst)),
            Rvalue::CallIntrinsic {
                name,
                args: vec![Operand::Copy(arg)],
            },
        )
    }

    fn is_nop(s: &Statement) -> bool {
        matches!(s.kind, StatementKind::Nop)
    }

    fn intrinsic_name(s: &Statement) -> Option<&str> {
        match &s.kind {
            StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, .. },
                ..
            } => Some(name),
            _ => None,
        }
    }

    /// A single block: `holder = Copy(x); <mid>; retain(x); <between>;
    /// release(x); <tail>` and a `Return`. `x` is `Local(1)` (a String),
    /// `holder` is `Local(2)`.
    fn body_with(
        tcx: &mut TyCtxt,
        mid: Vec<Statement>,
        between: Vec<Statement>,
        tail: Vec<Statement>,
    ) -> Body {
        let unit = tcx.unit();
        let s = tcx.string_ty();
        let locals = vec![
            decl(unit), // L0 return
            decl(s),    // L1 x
            decl(s),    // L2 holder
            decl(s),    // L3 spare String
            decl(unit), // L4 retain dest
            decl(unit), // L5 release dest
        ];
        let mut stmts = vec![copy(2, 1)];
        stmts.extend(mid);
        stmts.push(rc_call(4, "gos_rt_rc_retain", Place::local(Local(1))));
        stmts.extend(between);
        stmts.push(rc_call(5, "gos_rt_rc_release", Place::local(Local(1))));
        stmts.extend(tail);
        Body {
            name: "t".into(),
            def: None,
            arity: 0,
            locals,
            blocks: vec![BasicBlock {
                id: BlockId(0),
                stmts,
                terminator: Terminator::Return,
                span: span(),
            }],
            span: span(),
        }
    }

    #[test]
    fn cancels_tight_nonescaping_pair() {
        let mut tcx = TyCtxt::new();
        let mut body = body_with(&mut tcx, vec![], vec![], vec![]);
        elide_redundant_rc_pairs(&mut body, &tcx);
        let stmts = &body.blocks[0].stmts;
        // retain at index 1, release at index 2 are both cancelled.
        assert!(
            is_nop(&stmts[1]),
            "retain should be cancelled: {:?}",
            stmts[1].kind
        );
        assert!(
            is_nop(&stmts[2]),
            "release should be cancelled: {:?}",
            stmts[2].kind
        );
    }

    #[test]
    fn keeps_pair_when_value_used_between() {
        let mut tcx = TyCtxt::new();
        // `L3 = Copy(L1)` between the retain and the release reads `x`,
        // so the bracket is not tight - both ops are preserved.
        let mut body = body_with(&mut tcx, vec![], vec![copy(3, 1)], vec![]);
        elide_redundant_rc_pairs(&mut body, &tcx);
        let names: Vec<Option<&str>> = body.blocks[0].stmts.iter().map(intrinsic_name).collect();
        assert!(
            names.contains(&Some("gos_rt_rc_retain")) && names.contains(&Some("gos_rt_rc_release")),
            "pair must be kept when the value is used between retain and release"
        );
    }

    #[test]
    fn keeps_pair_when_value_live_after_release() {
        let mut tcx = TyCtxt::new();
        // `L3 = Copy(L1)` after the release keeps `x` live, so the
        // forward-liveness guard preserves both ops.
        let mut body = body_with(&mut tcx, vec![], vec![], vec![copy(3, 1)]);
        elide_redundant_rc_pairs(&mut body, &tcx);
        let names: Vec<Option<&str>> = body.blocks[0].stmts.iter().map(intrinsic_name).collect();
        assert!(
            names.contains(&Some("gos_rt_rc_retain")) && names.contains(&Some("gos_rt_rc_release")),
            "pair must be kept when the value is read after the release"
        );
    }

    #[test]
    fn keeps_field_projection_release() {
        let mut tcx = TyCtxt::new();
        // A field-projected release arg (`x.0`) is never a bare-local
        // pair: the aggregate-teardown accounting must be left intact.
        let mut body = body_with(&mut tcx, vec![], vec![], vec![]);
        if let StatementKind::Assign {
            rvalue: Rvalue::CallIntrinsic { args, .. },
            ..
        } = &mut body.blocks[0].stmts[2].kind
        {
            args[0] = Operand::Copy(Place {
                local: Local(1),
                projection: vec![Projection::Field(0)],
            });
        }
        elide_redundant_rc_pairs(&mut body, &tcx);
        assert!(
            !is_nop(&body.blocks[0].stmts[1]),
            "retain must be kept when the release is field-projected"
        );
    }

    #[test]
    fn cancels_value_moved_into_returned_holder() {
        let mut tcx = TyCtxt::new();
        // `x` (Local 1) is moved into the holder (`holder = Copy(x)`, the
        // forwarding use) and the holder is then returned (`L0 =
        // Copy(holder)`), so `x` transitively escapes the function. `x`
        // itself is dead after the release and not goroutine-shared, so
        // the move into the surviving holder is a pure ownership transfer
        // and the pair cancels. This is the binary-trees case: a child
        // moved into a returned node. The pair was previously kept because
        // the escape gate flagged the transitive escape.
        let mut body = body_with(&mut tcx, vec![], vec![], vec![copy(0, 2)]);
        elide_redundant_rc_pairs(&mut body, &tcx);
        assert!(
            is_nop(&body.blocks[0].stmts[1]),
            "retain should be cancelled for a value moved into a returned holder: {:?}",
            body.blocks[0].stmts[1].kind
        );
        assert!(
            is_nop(&body.blocks[0].stmts[2]),
            "release should be cancelled for a value moved into a returned holder: {:?}",
            body.blocks[0].stmts[2].kind
        );
    }

    #[test]
    fn keeps_goroutine_shared_value() {
        let mut tcx = TyCtxt::new();
        // `x` (Local 1) is marked shared (it crosses a goroutine
        // boundary), so another goroutine may concurrently adjust its
        // count and the balanced pair is load-bearing for the atomic
        // protocol - both ops must be preserved.
        let mut body = body_with(&mut tcx, vec![], vec![], vec![]);
        body.blocks[0]
            .stmts
            .push(rc_call(4, "gos_rt_rc_mark_shared", Place::local(Local(1))));
        elide_redundant_rc_pairs(&mut body, &tcx);
        let names: Vec<Option<&str>> = body.blocks[0].stmts.iter().map(intrinsic_name).collect();
        assert!(
            names.contains(&Some("gos_rt_rc_retain")) && names.contains(&Some("gos_rt_rc_release")),
            "pair must be kept for a goroutine-shared value"
        );
    }

    /// Builds a `for i in 0..len(xs)` loop over an `[i64]` vec that reads
    /// `xs[i]`, matching the lowerer's post-optimise shape, and returns
    /// the body plus a `TyCtxt`.
    fn counted_loop_body(tcx: &mut TyCtxt) -> Body {
        use gossamer_types::IntTy;
        let unit = tcx.unit();
        let i64t = tcx.int_ty(IntTy::I64);
        let slice = tcx.intern(gossamer_types::TyKind::Slice(i64t));
        let boolt = tcx.intern(gossamer_types::TyKind::Bool);
        // L0 ret(unit), L1 xs(slice), L2 bound, L3 counter, L4 cmp(bool),
        // L5 idx, L6 elem, L7 unit
        let locals = vec![
            decl(unit),
            decl(slice),
            decl(i64t),
            decl(i64t),
            decl(boolt),
            decl(i64t),
            decl(i64t),
            decl(unit),
        ];
        let sp = span();
        let call = |callee: &str, args: Vec<Operand>, dst: u32, target: u32| Terminator::Call {
            callee: Operand::Const(ConstValue::Str(callee.to_string())),
            args,
            destination: Place::local(Local(dst)),
            target: Some(BlockId(target)),
        };
        let blocks = vec![
            // bb0: bound = len(xs); init counter = 0
            BasicBlock {
                id: BlockId(0),
                stmts: vec![assign(
                    Place::local(Local(3)),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                )],
                terminator: call(
                    "gos_rt_vec_len",
                    vec![Operand::Copy(Place::local(Local(1)))],
                    2,
                    1,
                ),
                span: sp,
            },
            // bb1 header: cmp = counter < bound; switch
            BasicBlock {
                id: BlockId(1),
                stmts: vec![assign(
                    Place::local(Local(4)),
                    Rvalue::BinaryOp {
                        op: BinOp::Lt,
                        lhs: Operand::Copy(Place::local(Local(3))),
                        rhs: Operand::Copy(Place::local(Local(2))),
                    },
                )],
                terminator: Terminator::SwitchInt {
                    discriminant: Operand::Copy(Place::local(Local(4))),
                    arms: vec![(0, BlockId(3))],
                    default: BlockId(2),
                },
                span: sp,
            },
            // bb2 body: idx = counter; elem = xs[idx]; -> latch via call target
            BasicBlock {
                id: BlockId(2),
                stmts: vec![copy(5, 3)],
                terminator: call(
                    "gos_rt_vec_get_i64",
                    vec![
                        Operand::Copy(Place::local(Local(1))),
                        Operand::Copy(Place::local(Local(5))),
                    ],
                    6,
                    4,
                ),
                span: sp,
            },
            // bb3 exit
            BasicBlock {
                id: BlockId(3),
                stmts: vec![],
                terminator: Terminator::Return,
                span: sp,
            },
            // bb4 latch: counter += 1; goto header
            BasicBlock {
                id: BlockId(4),
                stmts: vec![assign(
                    Place::local(Local(3)),
                    Rvalue::BinaryOp {
                        op: BinOp::Add,
                        lhs: Operand::Copy(Place::local(Local(3))),
                        rhs: Operand::Const(ConstValue::Int(1)),
                    },
                )],
                terminator: Terminator::Goto { target: BlockId(1) },
                span: sp,
            },
        ];
        Body {
            name: "t".into(),
            def: None,
            arity: 1,
            locals,
            blocks,
            span: sp,
        }
    }

    #[test]
    fn bounds_rewrites_counted_loop_get() {
        let mut tcx = TyCtxt::new();
        let mut body = counted_loop_body(&mut tcx);
        bounds_check_elim(&mut body, &tcx);
        let Terminator::Call { callee, .. } = &body.blocks[2].terminator else {
            panic!("expected call terminator")
        };
        assert_eq!(
            callee,
            &Operand::Const(ConstValue::Str("gos_rt_vec_get_i64_unchecked".to_string())),
            "the proven in-range read should be rewritten to the unchecked callee"
        );
    }

    #[test]
    fn bounds_keeps_check_when_bound_is_not_len() {
        let mut tcx = TyCtxt::new();
        let mut body = counted_loop_body(&mut tcx);
        // Make the bound an opaque non-negative constant instead of
        // `len(xs)`: the read must stay checked.
        body.blocks[0].terminator = Terminator::Goto { target: BlockId(1) };
        body.blocks[0].stmts.push(assign(
            Place::local(Local(2)),
            Rvalue::Use(Operand::Const(ConstValue::Int(3))),
        ));
        bounds_check_elim(&mut body, &tcx);
        let Terminator::Call { callee, .. } = &body.blocks[2].terminator else {
            panic!("expected call terminator")
        };
        assert_eq!(
            callee,
            &Operand::Const(ConstValue::Str("gos_rt_vec_get_i64".to_string())),
            "a bound that is not the vec's length must keep the checked read"
        );
    }

    #[test]
    fn bounds_rewrites_direct_branch_guarded_get() {
        use gossamer_types::IntTy;
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        let i64t = tcx.int_ty(IntTy::I64);
        let vec_i64 = tcx.intern(gossamer_types::TyKind::Vec(i64t));
        let boolt = tcx.bool_ty();
        let locals = vec![
            decl(unit),
            decl(vec_i64), // xs
            decl(i64t),    // len
            decl(i64t),    // idx
            decl(boolt),   // cmp
            decl(i64t),    // elem
        ];
        let sp = span();
        let blocks = vec![
            BasicBlock {
                id: BlockId(0),
                stmts: vec![assign(
                    Place::local(Local(3)),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                )],
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
                    args: vec![Operand::Copy(Place::local(Local(1)))],
                    destination: Place::local(Local(2)),
                    target: Some(BlockId(1)),
                },
                span: sp,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: vec![assign(
                    Place::local(Local(4)),
                    Rvalue::BinaryOp {
                        op: BinOp::Lt,
                        lhs: Operand::Copy(Place::local(Local(3))),
                        rhs: Operand::Copy(Place::local(Local(2))),
                    },
                )],
                terminator: Terminator::SwitchInt {
                    discriminant: Operand::Copy(Place::local(Local(4))),
                    arms: vec![(0, BlockId(3))],
                    default: BlockId(2),
                },
                span: sp,
            },
            BasicBlock {
                id: BlockId(2),
                stmts: vec![],
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(Local(1))),
                        Operand::Copy(Place::local(Local(3))),
                    ],
                    destination: Place::local(Local(5)),
                    target: Some(BlockId(3)),
                },
                span: sp,
            },
            BasicBlock {
                id: BlockId(3),
                stmts: vec![],
                terminator: Terminator::Return,
                span: sp,
            },
        ];
        let mut body = Body {
            name: "branch".into(),
            def: None,
            arity: 1,
            locals,
            blocks,
            span: sp,
        };
        local_branch_bounds_check_elim(&mut body, &tcx);
        let Terminator::Call { callee, .. } = &body.blocks[2].terminator else {
            panic!("expected call terminator")
        };
        assert_eq!(
            callee,
            &Operand::Const(ConstValue::Str("gos_rt_vec_get_i64_unchecked".to_string()))
        );
    }

    #[test]
    #[allow(
        clippy::too_many_lines,
        reason = "synthetic MIR fixture needs explicit block structure"
    )]
    fn fuse_slice_parse_ranges_rewrites_parse_only_slice() {
        use gossamer_types::IntTy;
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        let i64t = tcx.int_ty(IntTy::I64);
        let string = tcx.string_ty();
        let result = tcx.intern(gossamer_types::TyKind::Tuple(vec![i64t, i64t]));
        let locals = vec![
            decl(unit),   // 0 return
            decl(string), // 1 input
            decl(i64t),   // 2 start
            decl(i64t),   // 3 end
            decl(result), // 4 slice result
            decl(i64t),   // 5 temp payload
            decl(string), // 6 unwrapped string temp
            decl(result), // 7 parse result
            decl(unit),   // 8 release unit
        ];
        let sp = span();
        let blocks = vec![
            BasicBlock {
                id: BlockId(0),
                stmts: vec![
                    assign(
                        Place::local(Local(6)),
                        Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    ),
                    assign(
                        Place::local(Local(2)),
                        Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                    ),
                    assign(
                        Place::local(Local(3)),
                        Rvalue::Use(Operand::Const(ConstValue::Int(3))),
                    ),
                ],
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_slice".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(Local(1))),
                        Operand::Copy(Place::local(Local(2))),
                        Operand::Copy(Place::local(Local(3))),
                    ],
                    destination: Place::local(Local(4)),
                    target: Some(BlockId(1)),
                },
                span: sp,
            },
            BasicBlock {
                id: BlockId(1),
                stmts: vec![
                    assign(
                        Place::local(Local(5)),
                        Rvalue::CallIntrinsic {
                            name: "gos_rt_result_payload",
                            args: vec![Operand::Copy(Place::local(Local(4)))],
                        },
                    ),
                    assign(
                        Place::local(Local(8)),
                        Rvalue::CallIntrinsic {
                            name: "gos_rt_rc_release",
                            args: vec![Operand::Copy(Place::local(Local(6)))],
                        },
                    ),
                    copy(6, 5),
                ],
                terminator: Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_strconv_parse_i64".to_string())),
                    args: vec![Operand::Copy(Place::local(Local(6)))],
                    destination: Place::local(Local(7)),
                    target: Some(BlockId(2)),
                },
                span: sp,
            },
            BasicBlock {
                id: BlockId(2),
                stmts: vec![assign(
                    Place::local(Local(8)),
                    Rvalue::CallIntrinsic {
                        name: "gos_rt_rc_release",
                        args: vec![Operand::Copy(Place::local(Local(6)))],
                    },
                )],
                terminator: Terminator::Return,
                span: sp,
            },
        ];
        let mut body = Body {
            name: "parse".into(),
            def: None,
            arity: 1,
            locals,
            blocks,
            span: sp,
        };

        fuse_slice_parse_ranges(&mut body);

        assert!(matches!(
            body.blocks[0].terminator,
            Terminator::Goto { target: BlockId(1) }
        ));
        assert!(is_nop(&body.blocks[1].stmts[2]));
        let Terminator::Call { callee, args, .. } = &body.blocks[1].terminator else {
            panic!("expected parse call")
        };
        assert_eq!(
            callee,
            &Operand::Const(ConstValue::Str(
                "gos_rt_strconv_parse_i64_range".to_string()
            ))
        );
        assert_eq!(
            args,
            &vec![
                Operand::Copy(Place::local(Local(1))),
                Operand::Copy(Place::local(Local(2))),
                Operand::Copy(Place::local(Local(3))),
            ]
        );
    }

    #[test]
    fn reserve_vecs_rewrites_counted_push_loop_constructor() {
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        let i64_ty = tcx.int_ty(gossamer_types::IntTy::I64);
        let locals = vec![
            decl(unit),   // L0 return
            decl(i64_ty), // L1 vec handle in this synthetic test
            decl(i64_ty), // L2 counter
            decl(i64_ty), // L3 bound
            decl(i64_ty), // L4 cond
            decl(i64_ty), // L5 value
            decl(unit),   // L6 push result
        ];
        let sp = span();
        let mut body = Body {
            name: "reserve".into(),
            def: None,
            arity: 3,
            locals,
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    stmts: vec![],
                    terminator: Terminator::Call {
                        callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
                        args: vec![Operand::Const(ConstValue::Int(8))],
                        destination: Place::local(Local(1)),
                        target: Some(BlockId(1)),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(1),
                    stmts: vec![assign(
                        Place::local(Local(4)),
                        Rvalue::BinaryOp {
                            op: BinOp::Lt,
                            lhs: Operand::Copy(Place::local(Local(2))),
                            rhs: Operand::Copy(Place::local(Local(3))),
                        },
                    )],
                    terminator: Terminator::SwitchInt {
                        discriminant: Operand::Copy(Place::local(Local(4))),
                        arms: vec![(0, BlockId(4))],
                        default: BlockId(2),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(2),
                    stmts: vec![],
                    terminator: Terminator::Call {
                        callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                        args: vec![
                            Operand::Copy(Place::local(Local(1))),
                            Operand::Copy(Place::local(Local(5))),
                        ],
                        destination: Place::local(Local(6)),
                        target: Some(BlockId(3)),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(3),
                    stmts: vec![assign(
                        Place::local(Local(2)),
                        Rvalue::BinaryOp {
                            op: BinOp::Add,
                            lhs: Operand::Copy(Place::local(Local(2))),
                            rhs: Operand::Const(ConstValue::Int(1)),
                        },
                    )],
                    terminator: Terminator::Goto { target: BlockId(1) },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(4),
                    stmts: vec![],
                    terminator: Terminator::Return,
                    span: sp,
                },
            ],
            span: sp,
        };

        reserve_vecs_for_counted_push_loops(&mut body);

        let Terminator::Call { callee, args, .. } = &body.blocks[0].terminator else {
            panic!("expected constructor call")
        };
        assert!(
            matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "gos_rt_vec_with_capacity")
        );
        assert_eq!(args.len(), 2);
        assert!(matches!(
            args[1],
            Operand::Copy(Place {
                local: Local(3),
                projection: _
            })
        ));
    }

    #[test]
    fn reserve_vecs_skips_bounds_computed_after_constructor() {
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        let i64_ty = tcx.int_ty(gossamer_types::IntTy::I64);
        let locals = vec![
            decl(unit),   // L0 return
            decl(i64_ty), // L1 vec
            decl(i64_ty), // L2 counter
            decl(i64_ty), // L3 bound, assigned after constructor
            decl(i64_ty), // L4 cond
            decl(i64_ty), // L5 value
            decl(unit),   // L6 push result
        ];
        let sp = span();
        let mut body = Body {
            name: "reserve_skip".into(),
            def: None,
            arity: 0,
            locals,
            blocks: vec![
                BasicBlock {
                    id: BlockId(0),
                    stmts: vec![],
                    terminator: Terminator::Call {
                        callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
                        args: vec![Operand::Const(ConstValue::Int(8))],
                        destination: Place::local(Local(1)),
                        target: Some(BlockId(1)),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(1),
                    stmts: vec![assign(
                        Place::local(Local(3)),
                        Rvalue::Use(Operand::Const(ConstValue::Int(10))),
                    )],
                    terminator: Terminator::Goto { target: BlockId(2) },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(2),
                    stmts: vec![assign(
                        Place::local(Local(4)),
                        Rvalue::BinaryOp {
                            op: BinOp::Lt,
                            lhs: Operand::Copy(Place::local(Local(2))),
                            rhs: Operand::Copy(Place::local(Local(3))),
                        },
                    )],
                    terminator: Terminator::SwitchInt {
                        discriminant: Operand::Copy(Place::local(Local(4))),
                        arms: vec![(0, BlockId(5))],
                        default: BlockId(3),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(3),
                    stmts: vec![],
                    terminator: Terminator::Call {
                        callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                        args: vec![
                            Operand::Copy(Place::local(Local(1))),
                            Operand::Copy(Place::local(Local(5))),
                        ],
                        destination: Place::local(Local(6)),
                        target: Some(BlockId(4)),
                    },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(4),
                    stmts: vec![assign(
                        Place::local(Local(2)),
                        Rvalue::BinaryOp {
                            op: BinOp::Add,
                            lhs: Operand::Copy(Place::local(Local(2))),
                            rhs: Operand::Const(ConstValue::Int(1)),
                        },
                    )],
                    terminator: Terminator::Goto { target: BlockId(2) },
                    span: sp,
                },
                BasicBlock {
                    id: BlockId(5),
                    stmts: vec![],
                    terminator: Terminator::Return,
                    span: sp,
                },
            ],
            span: sp,
        };

        reserve_vecs_for_counted_push_loops(&mut body);

        let Terminator::Call { callee, args, .. } = &body.blocks[0].terminator else {
            panic!("expected constructor call")
        };
        assert!(matches!(callee, Operand::Const(ConstValue::Str(name)) if name == "Vec::new"));
        assert_eq!(args.len(), 1);
    }
}
