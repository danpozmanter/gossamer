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

/// Whether `local` has one, acyclic definition that is exactly zero. This is
/// deliberately narrower than general constant propagation: a `len > 0`
/// guard proves only index zero (and `len - 1`) without relying on arithmetic
/// overflow or on a benchmark's input range.
fn local_is_known_zero(body: &Body, local: Local, visited: &mut Vec<Local>) -> bool {
    if visited.contains(&local) {
        return false;
    }
    visited.push(local);
    match unique_def_rvalue(body, local) {
        Some(Rvalue::Use(Operand::Const(ConstValue::Int(0)))) => true,
        Some(Rvalue::Use(Operand::Copy(place))) if place.projection.is_empty() => {
            local_is_known_zero(body, place.local, visited)
        }
        _ => false,
    }
}

fn guarded_successor_has_no_vec_side_effects(block: &BasicBlock, xs: Local) -> bool {
    block
        .stmts
        .iter()
        .all(|stmt| !stmt_mentions_local(stmt, xs))
}

/// Branch-local bounds facts for heap-style code. Rewrites checked scalar Vec
/// get/set calls in the proven true successor of either `idx < xs.len()` or
/// `xs.len() > 0` followed by `index = 0` or `last = len - 1`. The fact flows
/// through a straight-line chain of side-effect-free access blocks, so
/// repeated heap sift/BFS accesses do not each pay the same guard. It stops
/// before any mutation, aliasing/unknown call, branch, or cycle.
pub(crate) fn local_branch_bounds_check_elim(body: &mut Body, tcx: &TyCtxt) {
    let mut rewrites: Vec<(usize, &'static str)> = Vec::new();
    for h in 0..body.blocks.len() {
        let Some((successor, fact)) = branch_bound_fact(body, h) else {
            continue;
        };
        let mut current = successor;
        let mut seen = HashSet::new();
        while seen.insert(current) {
            let block = &body.blocks[current];
            // An empty single-successor bridge has no operation that can
            // mutate, alias, or otherwise invalidate the established bounds
            // fact. Lowering emits these around cleanup/source-span joins in
            // heap-sift/BFS-shaped control flow; following them keeps the
            // proof local without trying to merge facts across a branch.
            if block.stmts.is_empty()
                && let Terminator::Goto { target } = &block.terminator
            {
                // Do not carry a path fact through a join: another incoming
                // edge might not have evaluated the guard at all.
                if body
                    .blocks
                    .iter()
                    .enumerate()
                    .find(|(pred, candidate)| {
                        *pred != current
                            && successor_indices(&candidate.terminator)
                                .contains(&(target.0 as usize))
                    })
                    .is_some()
                {
                    break;
                }
                current = target.0 as usize;
                continue;
            }
            let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                args,
                target,
                ..
            } = &block.terminator
            else {
                break;
            };
            let Some(unchecked) = unchecked_variant(name) else {
                break;
            };
            let idx_arg = match (name.as_str(), args.as_slice()) {
                ("gos_rt_vec_get_i64", [_, idx]) => idx,
                ("gos_rt_vec_set_i64", [_, idx, _]) => idx,
                _ => break,
            };
            let Some(Operand::Copy(receiver)) = args.first() else {
                break;
            };
            if !receiver.projection.is_empty()
                || !guarded_successor_has_no_vec_side_effects(block, receiver.local)
                || block_writes_local(block, receiver.local)
                || !vec_elem_is_unchecked_scalar(body, tcx, receiver.local)
            {
                break;
            }
            let Operand::Copy(idx_place) = idx_arg else {
                break;
            };
            if !idx_place.projection.is_empty() {
                break;
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
                        && (local_is_len_minus_one(body, idx_place.local, len)
                            || local_is_known_zero(body, idx_place.local, &mut Vec::new()))
                }
            };
            if !proven {
                break;
            }
            rewrites.push((current, unchecked));
            let Some(next) = target else { break };
            current = next.0 as usize;
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

