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
        StatementKind::IterSource { dst, source, .. } => {
            place_mentions_local(dst, local) || operand_mentions_local(source, local)
        }
        StatementKind::IterAdapter {
            dst,
            upstream,
            closure_or_arg,
            ..
        } => {
            place_mentions_local(dst, local)
                || place_mentions_local(upstream, local)
                || closure_or_arg
                    .as_ref()
                    .is_some_and(|arg| operand_mentions_local(arg, local))
        }
        StatementKind::IterNext {
            dst_option,
            iter_place,
            ..
        } => place_mentions_local(dst_option, local) || place_mentions_local(iter_place, local),
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
/// Drops RC accounting on a place the block just set to the null constant.
/// Every `gos_rt_*` release null-checks its argument, so such a call cannot do
/// anything; removing it is a pure win at the drop-elaboration entry a
/// constructor emits. Block-local and conservative: any write reaching the
/// place, or its root local, forgets the fact.
pub(crate) fn elide_null_rc_accounting(body: &mut Body) {
    for block in &mut body.blocks {
        let mut null_places: Vec<Place> = Vec::new();
        let mut drop_at: Vec<usize> = Vec::new();
        for (index, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if let Rvalue::CallIntrinsic { name, args } = rvalue
                && rc_release_only(name)
                && let [Operand::Copy(target)] = args.as_slice()
                && null_places.iter().any(|known| known == target)
            {
                drop_at.push(index);
                continue;
            }
            // The statement writes `place`; anything previously known about
            // it, or about a place rooted in the same local, no longer holds.
            null_places.retain(|known| known.local != place.local);
            if matches!(rvalue, Rvalue::Use(Operand::Const(ConstValue::Int(0)))) {
                null_places.push(place.clone());
            }
        }
        for index in drop_at.into_iter().rev() {
            block.stmts.remove(index);
        }
    }
}

/// Whether `name` is an RC release whose argument the runtime null-checks.
fn rc_release_only(name: &str) -> bool {
    matches!(
        name,
        "gos_rt_rc_release"
            | "gos_rt_vec_free"
            | "gos_rt_str_free"
            | "gos_rt_str_free_typed"
            | "gos_rt_map_free"
            | "gos_rt_error_free"
    )
}

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
