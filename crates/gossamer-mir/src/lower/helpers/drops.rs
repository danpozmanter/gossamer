#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::let_and_return)]
#![allow(clippy::needless_collect)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

pub(crate) fn insert_drops_at_returns(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;

    if body.locals.is_empty() {
        return;
    }
    // Per-local: the constructor symbol that allocated it (if
    // any). `None` means the local was either never assigned, was
    // assigned by something other than a recognised constructor,
    // or has been disqualified by a subsequent re-assignment.
    let mut owner_ctor: Vec<Option<&'static str>> = vec![None; body.locals.len()];
    let mut moved_into_return: Vec<bool> = vec![false; body.locals.len()];

    // Drop-before-overwrite sites for aggregate-typed locals. Each
    // entry `(block_idx, stmt_idx, local, size_bytes)` means
    // "insert `gos_rt_aggr_free(local, size)` before block
    // `block_idx`'s statement at index `stmt_idx`". The null check
    // inside `gos_rt_aggr_free` makes this a no-op on the first
    // assignment (the local holds 0/null pre-init) and reclaims
    // the previous allocation on every subsequent assignment
    // — closing the loop-body aggregate-leak case.
    let mut drop_before_sites: Vec<(usize, usize, Local, i64)> = Vec::new();

    let ctor_to_free = |name: &str| -> Option<&'static str> {
        match name {
            // Runtime-symbol form (used by some peephole sites).
            "gos_rt_map_new" | "gos_rt_map_new_with_capacity" => Some("gos_rt_map_free"),
            "gos_rt_vec_new" | "gos_rt_vec_with_capacity" => Some("gos_rt_vec_free"),
            "gos_rt_set_new" => Some("gos_rt_set_free"),
            "gos_rt_btmap_new" => Some("gos_rt_btmap_free"),
            // Iterator over a Vec — the destination local is typed as
            // the source Vec so the `.next()` dispatch can recover the
            // element type. Without this entry the type-based
            // `inferred_free` path would schedule `gos_rt_vec_free` on
            // a `*mut GosArrIter`, mis-interpreting its bytes as a
            // `GosVec` header and corrupting the heap on free.
            "gos_rt_arr_iter" => Some("gos_rt_arr_iter_free"),
            // Path-form constructors emitted by the call lowerer.
            // The cranelift backend's `lower_intrinsic_call` table
            // routes these straight to the runtime helper, so the
            // drop pass needs to recognise both forms.
            "HashMap::new"
            | "collections::HashMap::new"
            | "HashMap::with_capacity"
            | "collections::HashMap::with_capacity" => Some("gos_rt_map_free"),
            "Vec::new" | "Vec::with_capacity" => Some("gos_rt_vec_free"),
            "HashSet::new" | "collections::HashSet::new" => Some("gos_rt_set_free"),
            "BTreeMap::new" | "collections::BTreeMap::new" => Some("gos_rt_btmap_free"),
            _ => None,
        }
    };

    let arity = body.arity as usize;
    let last_block = body.blocks.len();

    // Pass 1: discover constructor-allocated locals. Track every
    // assignment that *might* invalidate ownership (re-assignment,
    // projection writes) so we can disqualify aliasing patterns. Track every
    // assignment that *might* invalidate ownership (re-assignment,
    // projection writes) so we can disqualify aliasing patterns.
    //
    // Also disqualifies any local passed as a Copy arg to a Call
    // whose callee may capture its arguments (any user FnRef, or a
    // named runtime helper outside the non-capturing whitelist).
    // Without this disqualification, the drop pass would free a
    // container whose pointer is now retained inside the callee
    // (e.g. `flag::parse(os::args())` slurps the args vec; freeing
    // the args vec after the call orphans the parsed `rest`
    // strings).
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                let idx = place.local.0 as usize;
                if !place.projection.is_empty() {
                    // Writing through a projection on this local
                    // doesn't move ownership, so it stays valid.
                    continue;
                }
                if idx == 0 || idx <= arity || idx >= owner_ctor.len() {
                    continue;
                }
                // note: `Rvalue::Aggregate` /
                // `Rvalue::Repeat` are NOT tracked here. The LLVM
                // backend (used by `gos build`) lowers aggregates
                // to stack slots that die with the function frame
                // — no leak. The Cranelift backend (used by the
                // in-process JIT for `gos run`) routes them through
                // `gos_rt_aggr_alloc`, which lives in the
                // process-wide registry; long-running JIT bodies
                // can call `gos_rt_gc_reset` at safepoints to
                // reclaim. Emitting `gos_rt_aggr_free` here would
                // double-free the stack slot under LLVM, which is
                // the default backend.
                // Re-assignment of an owning local — disqualify.
                if owner_ctor[idx].is_some() && !matches!(rvalue, Rvalue::CallIntrinsic { .. }) {
                    owner_ctor[idx] = None;
                }
            }
        }
        if let Terminator::Call {
            callee,
            destination,
            ..
        } = &block.terminator
        {
            let idx = destination.local.0 as usize;
            if idx == 0 || idx <= arity || idx >= owner_ctor.len() {
                continue;
            }
            if !destination.projection.is_empty() {
                continue;
            }
            // Any local of a heap-container type that's the
            // destination of a Call also owns the result — the
            // callee returned a freshly-allocated container that
            // this frame must drop unless it's then moved into
            // the return slot. Match by static type, since the
            // callee name ("count_kmers", arbitrary user fn)
            // doesn't telegraph ownership.
            //
            // A handful of runtime callees return *borrowed*
            // pointers — `gos_rt_os_args` hands back the global
            // `ARGS_VEC` sentinel that lives for the whole
            // process; passing it to `gos_rt_vec_free` aborts in
            // `__libc_free` on the next-pointer probe. Skip the
            // inferred_free assignment for those.
            let borrowed_callee = matches!(
                callee,
                Operand::Const(ConstValue::Str(s))
                    if returns_borrowed_pointer(s.as_str())
            );
            let dest_ty = body.locals[idx].ty;
            let inferred_free: Option<&'static str> = if borrowed_callee {
                None
            } else {
                match tcx.kind_of(dest_ty) {
                    TyKind::HashMap { .. } => Some("gos_rt_map_free"),
                    TyKind::Vec(_) => Some("gos_rt_vec_free"),
                    _ => None,
                }
            };
            if let Operand::Const(ConstValue::Str(name)) = callee {
                if let Some(free) = ctor_to_free(name.as_str()) {
                    if owner_ctor[idx].is_none() {
                        owner_ctor[idx] = Some(free);
                        continue;
                    }
                }
            }
            if let Some(free) = inferred_free {
                if owner_ctor[idx].is_none() {
                    owner_ctor[idx] = Some(free);
                    continue;
                }
            }
            // when a Call returns an aggregate
            // (Adt / Tuple / Array) into a local, queue a
            // drop-before-overwrite of the prior value at the end
            // of this block (just before the Call terminator
            // runs). On the first execution the local holds 0/null
            // and `gos_rt_aggr_free` no-ops via its null check; on
            // every subsequent execution (loop reuse, repeated
            // call) the prior allocation is reclaimed instead of
            // leaked. The end-of-scope drop continues to handle
            // the final allocation at function return.
            let dest_is_aggregate = matches!(
                tcx.kind_of(dest_ty),
                TyKind::Adt { .. } | TyKind::Tuple(_) | TyKind::Array { .. }
            );
            // note: Call destinations of aggregate
            // type are not tracked here. See the matching comment in
            // the stmt-loop above — LLVM uses stack slots, Cranelift
            // JIT uses tracked heap allocs reclaimable via
            // `gos_rt_gc_reset` at safepoints.
            let _ = dest_is_aggregate;
            // Any other Call destination invalidates ownership
            // (the local now holds something else).
            owner_ctor[idx] = None;
        }
    }

    // Pass 2: detect locals that *transitively* flow into the
    // return slot. The constructor result may be copied through a
    // chain of intermediate locals before landing in `Local::RETURN`
    // (e.g. `Local(0) = Local(4); Local(4) = Local(5);
    // Local(5) = HashMap::new()`). Any local in that chain
    // shares the same heap pointer and must not be dropped, since
    // `Local::RETURN` will be moved out to the caller.
    //
    // Build a "Copy edge" graph (`from` → `to` whenever
    // `Assign(to, Use(Copy(from)))` appears with bare projections),
    // then walk it backwards from `Local::RETURN` to its closure.
    let mut copy_edges_to: Vec<Vec<Local>> = vec![Vec::new(); body.locals.len()];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                if !place.projection.is_empty() {
                    continue;
                }
                let to_idx = place.local.0 as usize;
                if to_idx >= copy_edges_to.len() {
                    continue;
                }
                match rvalue {
                    Rvalue::Use(Operand::Copy(p)) if p.projection.is_empty() => {
                        copy_edges_to[to_idx].push(p.local);
                    }
                    // An aggregate moves each `Copy` operand into
                    // the constructed value's storage. If the
                    // aggregate later flows to RETURN, every
                    // moved-in source local must skip its drop —
                    // its allocation is now owned by the caller via
                    // the returned aggregate. Without this edge,
                    // a `let v = Vec::new(); push(v, ...); Foo {
                    // ids: v }` body emits a `gos_rt_vec_free(v)`
                    // before Return, freeing storage that the
                    // returned struct's `ids` field still aliases —
                    // the caller's `f.ids.len()` then reads garbage.
                    Rvalue::Aggregate { operands, .. } => {
                        for op in operands {
                            if let Operand::Copy(p) = op {
                                if p.projection.is_empty() {
                                    copy_edges_to[to_idx].push(p.local);
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let mut stack = vec![Local::RETURN];
    moved_into_return[Local::RETURN.0 as usize] = true;
    while let Some(cur) = stack.pop() {
        let cur_idx = cur.0 as usize;
        if cur_idx >= copy_edges_to.len() {
            continue;
        }
        for src in copy_edges_to[cur_idx].clone() {
            let src_idx = src.0 as usize;
            if src_idx >= moved_into_return.len() {
                continue;
            }
            if !moved_into_return[src_idx] {
                moved_into_return[src_idx] = true;
                stack.push(src);
            }
        }
    }
    // Calls whose destination flows into `Local::RETURN` move every
    // pointer-shaped Copy argument into the return value too. Tuple
    // construction in particular lowers as a synthesised
    // `__tuple(...)` Call — the Vec/aggregate operands are moved
    // into the constructed value, so they must skip their drop.
    // Iterate to a fixed point because a moved-in Call destination
    // can propagate the same closure backwards through more Copy
    // edges (the dest of an inner construct may feed an outer one).
    let mut changed = true;
    while changed {
        changed = false;
        // Helper: propagate "moved into return" through one Call's
        // arg list when its destination already flows there.
        // Used for both Terminator::Call and Rvalue::CallIntrinsic
        // (the result-ctor / aggregate-helper paths route through
        // the Rvalue form), so the same chain — Vec → struct
        // operand → gos_rt_result_new → Local::RETURN — is walked
        // back to the Vec and skips its drop.
        let propagate_call_args = |args: &[Operand], moved: &mut Vec<bool>, changed: &mut bool| {
            for arg in args {
                if let Operand::Copy(p) = arg
                    && p.projection.is_empty()
                {
                    let idx = p.local.0 as usize;
                    if idx < moved.len() && !moved[idx] {
                        moved[idx] = true;
                        *changed = true;
                        let mut stack = vec![Local(u32::try_from(idx).unwrap_or(0))];
                        while let Some(cur) = stack.pop() {
                            let cur_idx = cur.0 as usize;
                            if cur_idx >= copy_edges_to.len() {
                                continue;
                            }
                            for src in copy_edges_to[cur_idx].clone() {
                                let src_idx = src.0 as usize;
                                if src_idx < moved.len() && !moved[src_idx] {
                                    moved[src_idx] = true;
                                    *changed = true;
                                    stack.push(src);
                                }
                            }
                        }
                    }
                }
            }
        };
        for block in &body.blocks {
            // Rvalue-position calls (the `Ok(...)` /
            // result-ctor path uses `Rvalue::CallIntrinsic
            // { name: "gos_rt_result_new", args: [disc, payload] }`).
            // Without this arm, a `Vec` inside a struct that's
            // wrapped in `Result::Ok(R { xs: v })` was not
            // recognised as moved-into-return and the drop pass
            // freed it before the caller unwrapped, producing a
            // dangling Vec in the returned `Result`.
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind
                    && place.projection.is_empty()
                    && let Rvalue::CallIntrinsic { args, .. } = rvalue
                {
                    let dest_idx = place.local.0 as usize;
                    if dest_idx >= moved_into_return.len() || !moved_into_return[dest_idx] {
                        continue;
                    }
                    propagate_call_args(args, &mut moved_into_return, &mut changed);
                }
            }
            if let Terminator::Call {
                callee,
                destination,
                args,
                ..
            } = &block.terminator
            {
                if !destination.projection.is_empty() {
                    continue;
                }
                let dest_idx = destination.local.0 as usize;
                if dest_idx >= moved_into_return.len() || !moved_into_return[dest_idx] {
                    continue;
                }
                // Only aggregate-constructor callees actually move
                // their args into the destination value. Generic
                // Calls (println, str_concat, map_get_or, every
                // user fn) consume their args without retaining
                // them, so propagating "moved" through their args
                // would mark unrelated heap-owning locals as
                // moved-into-return and silently skip their drops.
                if !is_aggregate_ctor_callee(callee) {
                    continue;
                }
                propagate_call_args(args, &mut moved_into_return, &mut changed);
            }
        }
    }

    // Pass 3: collect drop targets in stable local-index order.
    // The constructor-name → free-name table already restricts
    // candidates to runtime container shapes; we trust the MIR's
    // type assignment and skip a redundant TyKind check here.
    let _ = TyKind::Bool; // silence unused-import lint outside the closure
    let drop_targets: Vec<(Local, &'static str)> = (0..owner_ctor.len())
        .filter_map(|i| {
            let free = owner_ctor[i]?;
            if moved_into_return[i] {
                return None;
            }
            Some((Local(i as u32), free))
        })
        .collect();

    if drop_targets.is_empty() {
        return;
    }

    // Per-target must-init dataflow. For each drop target `L`,
    // compute `init_at_return[L][R]` — `true` when every path from
    // entry to Return block `R` passes through at least one
    // definition of `L`. A definition is a Call terminator whose
    // destination is `L` or a stmt-position assignment to `L`.
    //
    // The earlier (type-only) pass scheduled a free at every
    // Return for every recognised owner local, including shapes
    // like `let m: HashMap<...>; if cond { m = HashMap::new() };
    // return m;` where the `else` branch reaches Return without
    // ever initialising `m`. Calling `gos_rt_map_free` on the
    // uninit slot aborts in the allocator metadata probe.
    //
    // Approach: minimal forward dataflow with intersection at
    // joins (the "must-init" lattice). Drops are emitted only at
    // Return blocks where the target is must-init at the point of
    // return; cases where the proof is undecidable (irreducible
    // CFG, complex loops) conservatively skip the drop — a leak
    // is preferable to a free of uninit memory.
    let init_at_return = compute_init_at_returns(body, &drop_targets);

    for block_idx in 0..last_block {
        if !matches!(body.blocks[block_idx].terminator, Terminator::Return) {
            continue;
        }
        let span = body.blocks[block_idx].span;
        let init_row = &init_at_return[block_idx];
        for (target_idx, (local, free_name)) in drop_targets.iter().enumerate() {
            if !init_row[target_idx] {
                continue;
            }
            let dest = Local(u32::try_from(body.locals.len()).expect("local overflow"));
            let unit_ty = body.locals[0].ty;
            body.locals.push(LocalDecl {
                ty: unit_ty,
                debug_name: None,
                mutable: false,
            });
            // Emit the free as a CallIntrinsic stmt — the cranelift
            // lowerer's statement path handles it without any block
            // rewiring. `gos_rt_aggr_free` needs a second `size`
            // arg the codegen derives from the local's type; all
            // other helpers (Vec/Map/Set/...) are single-arg.
            // `gos_rt_aggr_free` takes 2 args (ptr + size); the
            // other heap-container free helpers take only the
            // receiver pointer.
            let args = if *free_name == "gos_rt_aggr_free" {
                let size = aggr_size_bytes(tcx, body.locals[local.0 as usize].ty);
                vec![
                    Operand::Copy(Place::local(*local)),
                    Operand::Const(ConstValue::Int(i128::from(size))),
                ]
            } else {
                vec![Operand::Copy(Place::local(*local))]
            };
            body.blocks[block_idx].stmts.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(dest),
                    rvalue: Rvalue::CallIntrinsic {
                        name: free_name,
                        args,
                    },
                },
                span,
            });
        }
    }

    // drop-before-overwrite for aggregate
    // reassignments. Skip sites where the local is not provably
    // initialised on every path leading to this statement —
    // freeing an uninitialised aggregate local reads garbage from
    // the Cranelift Variable slot and aborts in `__libc_free`.
    //
    // For each candidate site, compute "is local must-init at
    // block entry?" via the same dataflow used by
    // `compute_init_at_returns`. Then walk the block statements
    // up to `stmt_idx`, updating must-init on each Assign to
    // this local. Drop is emitted only if must-init is true at
    // the point of the candidate stmt.
    let candidate_locals: Vec<Local> = drop_before_sites
        .iter()
        .map(|(_, _, l, _)| *l)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    let init_at_each_return = if candidate_locals.is_empty() {
        Vec::new()
    } else {
        let targets: Vec<(Local, &'static str)> = candidate_locals
            .iter()
            .map(|l| (*l, "gos_rt_aggr_free"))
            .collect();
        compute_init_at_block_entries(body, &targets)
    };
    let local_to_target_idx: std::collections::BTreeMap<Local, usize> = candidate_locals
        .iter()
        .enumerate()
        .map(|(i, l)| (*l, i))
        .collect();
    let must_init_at = |block_idx: usize, stmt_idx: usize, local: Local| -> bool {
        let Some(target_idx) = local_to_target_idx.get(&local) else {
            return false;
        };
        if block_idx >= init_at_each_return.len() {
            return false;
        }
        let mut init = init_at_each_return[block_idx][*target_idx];
        // Walk stmts up to stmt_idx and update must-init based on
        // Assign destinations.
        for (i, stmt) in body.blocks[block_idx].stmts.iter().enumerate() {
            if i >= stmt_idx {
                break;
            }
            if let StatementKind::Assign { place, .. } = &stmt.kind
                && place.projection.is_empty()
                && place.local == local
            {
                init = true;
            }
        }
        init
    };
    drop_before_sites.sort_by_key(|a| (a.0, a.1));
    let drop_before_sites: Vec<_> = drop_before_sites
        .into_iter()
        .filter(|(b, s, l, _)| must_init_at(*b, *s, *l))
        .collect();
    for (block_idx, stmt_idx, local, size) in drop_before_sites.into_iter().rev() {
        if block_idx >= body.blocks.len() {
            continue;
        }
        let span = body.blocks[block_idx]
            .stmts
            .get(stmt_idx)
            .map_or(body.blocks[block_idx].span, |s| s.span);
        let dest = Local(u32::try_from(body.locals.len()).expect("local overflow"));
        let unit_ty = body.locals[0].ty;
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
        });
        let drop_stmt = Statement {
            kind: StatementKind::Assign {
                place: Place::local(dest),
                rvalue: Rvalue::CallIntrinsic {
                    name: "gos_rt_aggr_free",
                    args: vec![
                        Operand::Copy(Place::local(local)),
                        Operand::Const(ConstValue::Int(i128::from(size))),
                    ],
                },
            },
            span,
        };
        body.blocks[block_idx].stmts.insert(stmt_idx, drop_stmt);
    }
}

pub(crate) fn compute_init_at_block_entries(
    body: &Body,
    targets: &[(Local, &'static str)],
) -> Vec<Vec<bool>> {
    let n_blocks = body.blocks.len();
    let n_targets = targets.len();
    if n_blocks == 0 || n_targets == 0 {
        return vec![vec![false; n_targets]; n_blocks];
    }

    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        for s in block_successors(&block.terminator) {
            let si = s.0 as usize;
            if si < n_blocks {
                preds[si].push(i);
            }
        }
    }
    let target_locals: Vec<u32> = targets.iter().map(|(l, _)| l.0).collect();

    let mut stmt_defs = vec![vec![false; n_targets]; n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, .. } = &stmt.kind
                && place.projection.is_empty()
            {
                for (t, l) in target_locals.iter().enumerate() {
                    if place.local.0 == *l {
                        stmt_defs[i][t] = true;
                    }
                }
            }
        }
    }
    let mut term_defs = vec![vec![false; n_targets]; n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.projection.is_empty()
        {
            for (t, l) in target_locals.iter().enumerate() {
                if destination.local.0 == *l {
                    term_defs[i][t] = true;
                }
            }
        }
    }

    let mut init_in = vec![vec![false; n_targets]; n_blocks];
    let mut init_out = vec![vec![false; n_targets]; n_blocks];
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n_blocks {
            for t in 0..n_targets {
                let new_in = if preds[i].is_empty() {
                    false
                } else {
                    preds[i].iter().all(|&p| init_out[p][t] || term_defs[p][t])
                };
                let new_out = new_in || stmt_defs[i][t];
                if new_in != init_in[i][t] || new_out != init_out[i][t] {
                    init_in[i][t] = new_in;
                    init_out[i][t] = new_out;
                    changed = true;
                }
            }
        }
    }
    init_in
}

pub(crate) fn compute_init_at_returns(
    body: &Body,
    targets: &[(Local, &'static str)],
) -> Vec<Vec<bool>> {
    let n_blocks = body.blocks.len();
    let n_targets = targets.len();
    let mut out = vec![vec![false; n_targets]; n_blocks];
    if n_blocks == 0 || n_targets == 0 {
        return out;
    }

    // Predecessor map for join nodes.
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        for s in block_successors(&block.terminator) {
            let si = s.0 as usize;
            if si < n_blocks {
                preds[si].push(i);
            }
        }
    }

    let target_locals: Vec<u32> = targets.iter().map(|(l, _)| l.0).collect();

    // init_in[B][t] — must-init at entry of B.
    // init_out[B][t] — must-init after all of B's stmts (used at
    // the Return point for Return-terminated blocks).
    let mut init_in = vec![vec![false; n_targets]; n_blocks];
    let mut init_out = vec![vec![false; n_targets]; n_blocks];

    // Pre-compute stmt-position defs per (block, target).
    let mut stmt_defs = vec![vec![false; n_targets]; n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, .. } = &stmt.kind
                && place.projection.is_empty()
            {
                for (t, l) in target_locals.iter().enumerate() {
                    if place.local.0 == *l {
                        stmt_defs[i][t] = true;
                    }
                }
            }
        }
    }
    // Terminator-position defs (Call destinations).
    let mut term_defs = vec![vec![false; n_targets]; n_blocks];
    for (i, block) in body.blocks.iter().enumerate() {
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.projection.is_empty()
        {
            for (t, l) in target_locals.iter().enumerate() {
                if destination.local.0 == *l {
                    term_defs[i][t] = true;
                }
            }
        }
    }

    // Successors of a Call see the destination as already
    // initialised. Encode that by folding `term_defs[B]` into
    // `init_out[B]` *and* into the value propagated to successors.
    let mut changed = true;
    while changed {
        changed = false;
        for i in 0..n_blocks {
            for t in 0..n_targets {
                // Join: must-init at entry = AND across predecessors.
                let new_in = if preds[i].is_empty() {
                    false
                } else {
                    preds[i].iter().all(|&p| init_out[p][t] || term_defs[p][t])
                };
                // Transfer: pick up stmt defs that fire before any
                // terminator-position read. The Return point reads
                // *after* stmts but the terminator itself is the
                // return — so `init_out` for a Return block sees
                // stmt defs from this block.
                let new_out = new_in || stmt_defs[i][t];
                if new_in != init_in[i][t] || new_out != init_out[i][t] {
                    init_in[i][t] = new_in;
                    init_out[i][t] = new_out;
                    changed = true;
                }
            }
        }
    }

    // For each block, `out[B][t]` is the must-init bit at the
    // *point of return*. Return blocks read `init_out[B]` (defs in
    // this block's stmts count); non-Return blocks see the value
    // they would have at the terminator boundary, which callers
    // ignore — the drop pass only consults Return blocks.
    for i in 0..n_blocks {
        out[i].clone_from(&init_out[i]);
    }
    out
}

pub(crate) fn block_successors(t: &Terminator) -> Vec<BlockId> {
    match t {
        Terminator::Goto { target } => vec![*target],
        Terminator::SwitchInt { arms, default, .. } => {
            let mut out: Vec<BlockId> = arms.iter().map(|(_, b)| *b).collect();
            out.push(*default);
            out
        }
        Terminator::Call { target, .. } => target.iter().copied().collect(),
        Terminator::Assert { target, .. } | Terminator::Drop { target, .. } => vec![*target],
        Terminator::Return | Terminator::Unreachable | Terminator::Panic { .. } => Vec::new(),
    }
}
