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

// ---------------------------------------------------------------------------
// Borrowed-holder RC elision.
// ---------------------------------------------------------------------------

/// The runtime helpers that read the heap value at argument `index` and
/// neither keep nor release it, so a call through one is a borrow.
fn helper_borrows_arg(name: &str, index: usize) -> bool {
    match index {
        0 => matches!(
            name,
            "gos_rt_vec_len"
                | "gos_rt_len"
                | "gos_rt_vec_get_i64"
                | "gos_rt_vec_get_i64_unchecked"
                | "gos_rt_vec_get_f64"
                | "gos_rt_vec_get_i128"
                | "gos_rt_vec_get_ptr"
                | "gos_rt_vec_is_empty"
                | "gos_rt_str_len"
                | "gos_rt_str_byte_len"
                | "gos_rt_str_char_at"
                | "gos_rt_str_byte_at"
                | "gos_rt_str_is_empty"
                | "gos_rt_hash_crc32_checksum"
                | "gos_rt_hash_crc32_checksum_string"
        ),
        1 => matches!(
            name,
            "gos_rt_hash_crc32_update"
                | "gos_rt_hash_crc32_update_window"
                | "gos_rt_str_push_utf8"
                | "gos_rt_str_concat_drop_a"
                | "gos_rt_str_concat"
        ),
        _ => false,
    }
}

/// The retain and release helper names that account for a local of `ty`,
/// for the kinds a borrowed holder can hold. `None` for every other type.
fn holder_rc_names(tcx: &TyCtxt, ty: Ty) -> Option<(&'static [&'static str], &'static [&'static str])> {
    match tcx.kind_of(ty) {
        TyKind::String => Some((
            &["gos_rt_str_retain_typed", "gos_rt_str_retain"],
            &["gos_rt_str_free_typed", "gos_rt_str_free"],
        )),
        TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } => {
            Some((&["gos_rt_vec_retain"], &["gos_rt_vec_free"]))
        }
        _ => None,
    }
}

fn is_rc_retain_name(name: &str) -> bool {
    matches!(
        name,
        "gos_rt_rc_retain"
            | "gos_rt_vec_retain"
            | "gos_rt_str_retain_typed"
            | "gos_rt_str_retain"
            | "gos_rt_map_retain"
            | "gos_rt_rc_weak_retain"
    )
}

fn is_rc_release_name(name: &str) -> bool {
    rc_release_only(name) || matches!(name, "gos_rt_rc_weak_release" | "gos_rt_map_field_release")
}

/// How a local is written, for resolving which value it aliases.
#[derive(Clone, Copy, PartialEq, Eq)]
enum AliasDef {
    /// Never written other than by a constant.
    None,
    /// One write, copying `Place` (bare or projected).
    Copy(Local),
    /// One write, the element address `gos_rt_vec_get_ptr` answers for the Vec in `Local`.
    ElementPtr(Local),
    /// One write of some other shape, or several writes.
    Opaque,
}

fn alias_defs(body: &Body) -> Vec<AliasDef> {
    let mut defs = vec![AliasDef::None; body.locals.len()];
    let mut note = |local: Local, def: AliasDef| {
        let slot = &mut defs[local.0 as usize];
        *slot = if *slot == AliasDef::None { def } else { AliasDef::Opaque };
    };
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
            {
                match rvalue {
                    Rvalue::Use(Operand::Const(_)) => {}
                    Rvalue::Use(Operand::Copy(src)) if src.local != place.local => {
                        note(place.local, AliasDef::Copy(src.local));
                    }
                    _ => note(place.local, AliasDef::Opaque),
                }
            }
        }
        if let Terminator::Call {
            callee,
            args,
            destination,
            ..
        } = &block.terminator
            && destination.projection.is_empty()
        {
            let def = match (callee, args.first()) {
                (Operand::Const(ConstValue::Str(name)), Some(Operand::Copy(vec)))
                    if name == "gos_rt_vec_get_ptr" =>
                {
                    AliasDef::ElementPtr(vec.local)
                }
                _ => AliasDef::Opaque,
            };
            note(destination.local, def);
        }
    }
    defs
}

/// The local whose value `local` is an alias of: the origin of its chain of
/// copies and element addresses, or `local` itself.
fn alias_root(defs: &[AliasDef], local: Local) -> Local {
    let mut current = local;
    let mut steps = 0;
    loop {
        let next = match defs[current.0 as usize] {
            AliasDef::Copy(src) | AliasDef::ElementPtr(src) => src,
            AliasDef::None | AliasDef::Opaque => return current,
        };
        steps += 1;
        if next == current || steps > defs.len() {
            return current;
        }
        current = next;
    }
}

fn operand_in_chain(op: &Operand, chain: &[bool]) -> bool {
    matches!(op, Operand::Copy(p) if chain[p.local.0 as usize])
}

/// What a call's callee is, for deciding whether an argument it reads could
/// be written through.
enum CalleeKind<'a> {
    User,
    Runtime(&'a str),
    Unknown,
}

fn callee_kind(callee: &Operand) -> CalleeKind<'_> {
    match callee {
        Operand::Const(ConstValue::Str(name)) => {
            if name.starts_with("gos_rt_") || name.starts_with("gos_rc") {
                CalleeKind::Runtime(name)
            } else {
                CalleeKind::User
            }
        }
        Operand::FnRef { .. } => CalleeKind::User,
        _ => CalleeKind::Unknown,
    }
}

/// `true` when a call passing `place` at argument `index` to `callee` could
/// write the value the place reaches. A user function is handed by-value
/// arguments as borrows or as clones and can write only through a reference,
/// so a bare reference-typed local is the one by-value shape that reaches it
/// writable. A runtime helper is a borrow only where it is known to read.
fn call_arg_may_write(
    body: &Body,
    tcx: &TyCtxt,
    callee: &CalleeKind<'_>,
    index: usize,
    place: &Place,
) -> bool {
    match callee {
        CalleeKind::User => {
            place.projection.is_empty()
                && matches!(
                    tcx.kind_of(body.locals[place.local.0 as usize].ty),
                    TyKind::Ref { .. }
                )
        }
        CalleeKind::Runtime(name) => !helper_borrows_arg(name, index),
        CalleeKind::Unknown => true,
    }
}

/// `true` when `stmt` could change or release the structure reached through
/// the chain rooted at `root`. Retains never do; a release of the root or of
/// an alias other than the holder itself may.
fn stmt_disturbs_chain(stmt: &Statement, chain: &[bool], root: Local, holder: Local) -> bool {
    let in_chain = |l: Local| chain[l.0 as usize];
    match &stmt.kind {
        StatementKind::Assign { place, rvalue } => {
            if in_chain(place.local) && (place.local == root || !place.projection.is_empty()) {
                return true;
            }
            match rvalue {
                Rvalue::Ref { place: p, .. } => in_chain(p.local),
                Rvalue::CallIntrinsic { name, args } => {
                    if is_rc_retain_name(name) {
                        false
                    } else if is_rc_release_name(name) {
                        args.iter().any(|a| match a {
                            Operand::Copy(p) => in_chain(p.local) && p.local != holder,
                            _ => false,
                        })
                    } else {
                        args.iter().enumerate().any(|(i, a)| {
                            operand_in_chain(a, chain) && !helper_borrows_arg(name, i)
                        })
                    }
                }
                _ => false,
            }
        }
        StatementKind::SetDiscriminant { place, .. } => in_chain(place.local),
        StatementKind::StaticStore { value, .. } => operand_in_chain(value, chain),
        StatementKind::IterSource { dst, source, .. } => {
            in_chain(dst.local) || operand_in_chain(source, chain)
        }
        StatementKind::IterAdapter {
            dst,
            upstream,
            closure_or_arg,
            ..
        } => {
            in_chain(dst.local)
                || in_chain(upstream.local)
                || closure_or_arg
                    .as_ref()
                    .is_some_and(|a| operand_in_chain(a, chain))
        }
        StatementKind::IterNext {
            dst_option,
            iter_place,
            ..
        } => in_chain(dst_option.local) || in_chain(iter_place.local),
        StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => false,
    }
}

fn term_disturbs_chain(body: &Body, tcx: &TyCtxt, t: &Terminator, chain: &[bool], root: Local) -> bool {
    let in_chain = |l: Local| chain[l.0 as usize];
    match t {
        Terminator::Call {
            callee,
            args,
            destination,
            ..
        } => {
            if in_chain(destination.local)
                && (destination.local == root || !destination.projection.is_empty())
            {
                return true;
            }
            let kind = callee_kind(callee);
            if matches!(kind, CalleeKind::Unknown) {
                return true;
            }
            args.iter().enumerate().any(|(i, a)| match a {
                Operand::Copy(p) if in_chain(p.local) => call_arg_may_write(body, tcx, &kind, i, p),
                _ => false,
            })
        }
        Terminator::Drop { place, .. } => in_chain(place.local),
        _ => false,
    }
}

/// `true` when `stmt` is a retain or release of the bare local `holder`.
fn stmt_is_rc_op_on(stmt: &Statement, holder: Local) -> bool {
    matches!(&stmt.kind, StatementKind::Assign {
        rvalue: Rvalue::CallIntrinsic { name, args },
        ..
    } if (is_rc_retain_name(name) || is_rc_release_name(name)) && rc_bare_local_arg(args) == Some(holder))
}

/// `true` when `stmt` reads `holder` in a shape a borrow answers: a
/// projected read, a call argument through a borrowing helper or a user
/// function, or storage bookkeeping. A bare copy into another place, an
/// aggregate capture, or a reference to it needs the holder's own share.
fn stmt_use_is_borrow(stmt: &Statement, holder: Local) -> bool {
    let bare = |op: &Operand| matches!(op, Operand::Copy(p) if p.local == holder && p.projection.is_empty());
    match &stmt.kind {
        StatementKind::Assign { place, rvalue } => {
            if place.local == holder {
                return place.projection.is_empty()
                    && matches!(rvalue, Rvalue::Use(Operand::Const(_)));
            }
            match rvalue {
                Rvalue::Use(op) | Rvalue::UnaryOp { operand: op, .. } | Rvalue::Cast { operand: op, .. } => {
                    !bare(op)
                }
                Rvalue::BinaryOp { lhs, rhs, .. } => !bare(lhs) && !bare(rhs),
                Rvalue::Len(_) => true,
                Rvalue::CallIntrinsic { name, args } => args
                    .iter()
                    .enumerate()
                    .all(|(i, a)| !bare(a) || helper_borrows_arg(name, i)),
                Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } | Rvalue::Ref { .. } => false,
                Rvalue::StaticLoad(_) => true,
            }
        }
        StatementKind::StorageLive(_) | StatementKind::StorageDead(_) | StatementKind::Nop => true,
        StatementKind::SetDiscriminant { .. }
        | StatementKind::StaticStore { .. }
        | StatementKind::IterSource { .. }
        | StatementKind::IterAdapter { .. }
        | StatementKind::IterNext { .. } => false,
    }
}

fn term_use_is_borrow(t: &Terminator, holder: Local) -> bool {
    let bare = |op: &Operand| matches!(op, Operand::Copy(p) if p.local == holder && p.projection.is_empty());
    match t {
        Terminator::Call {
            callee,
            args,
            destination,
            ..
        } => {
            if destination.local == holder || bare(callee) {
                return false;
            }
            let kind = callee_kind(callee);
            args.iter().enumerate().all(|(i, a)| {
                !bare(a)
                    || match kind {
                        CalleeKind::User => true,
                        CalleeKind::Runtime(name) => helper_borrows_arg(name, i),
                        CalleeKind::Unknown => false,
                    }
            })
        }
        Terminator::SwitchInt { discriminant, .. } => !bare(discriminant),
        Terminator::Assert { cond, .. } => !bare(cond),
        Terminator::Drop { place, .. } => place.local != holder,
        Terminator::Goto { .. }
        | Terminator::Return
        | Terminator::Unreachable
        | Terminator::Panic { .. } => true,
    }
}

/// A holder and the alias chain it borrows from.
struct Borrow<'a> {
    holder: Local,
    root: Local,
    chain: &'a [bool],
}

/// Walks forward from the holder's definition and answers `true` when every
/// read of the holder happens before anything could disturb the structure
/// it borrows from. A bare write of the holder ends the window on that path.
fn holder_window_is_clean(
    body: &Body,
    tcx: &TyCtxt,
    succs: &[Vec<usize>],
    def: (usize, usize),
    borrow: &Borrow<'_>,
) -> bool {
    let Borrow {
        holder,
        root,
        chain,
    } = *borrow;
    let n = body.blocks.len();
    let mut visited = vec![[false; 2]; n];
    let mut stack: Vec<(usize, usize, bool)> = vec![(def.0, def.1 + 1, false)];
    while let Some((b, from, mut dirty)) = stack.pop() {
        let blk = &body.blocks[b];
        let mut ended = false;
        for stmt in &blk.stmts[from..] {
            if stmt_is_rc_op_on(stmt, holder) {
                continue;
            }
            if stmt_writes_bare(stmt, holder) {
                ended = true;
                break;
            }
            if stmt_mentions_local(stmt, holder) && dirty {
                return false;
            }
            if stmt_disturbs_chain(stmt, chain, root, holder) {
                dirty = true;
            }
        }
        if ended {
            continue;
        }
        let t = &blk.terminator;
        if term_mentions_local(t, holder) && dirty {
            return false;
        }
        if term_writes_bare(t, holder) {
            continue;
        }
        if term_disturbs_chain(body, tcx, t, chain, root) {
            dirty = true;
        }
        for &s in &succs[b] {
            if !visited[s][usize::from(dirty)] {
                visited[s][usize::from(dirty)] = true;
                stack.push((s, 0, dirty));
            }
        }
    }
    true
}

/// Where a holder is defined and how many accounting calls it carries.
struct HolderUses {
    def: (usize, usize),
    retains: usize,
    releases: usize,
}

/// Classifies every mention of `holder`: its one projected copy out of
/// `source`, its retains and releases, and reads that are borrows. `None`
/// when any mention is of another shape.
fn classify_holder_uses(
    body: &Body,
    holder: Local,
    source: Local,
    retain_names: &[&str],
    release_names: &[&str],
) -> Option<HolderUses> {
    let mut def = None;
    let mut retains = 0usize;
    let mut releases = 0usize;
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } = &stmt.kind
                && rc_bare_local_arg(args) == Some(holder)
            {
                if retain_names.contains(name) {
                    retains += 1;
                    continue;
                }
                if release_names.contains(name) {
                    releases += 1;
                    continue;
                }
            }
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Copy(src)),
            } = &stmt.kind
                && place.local == holder
                && place.projection.is_empty()
            {
                if src.projection.is_empty() || src.local != source {
                    return None;
                }
                def = Some((bi, si));
                continue;
            }
            if stmt_mentions_local(stmt, holder) && !stmt_use_is_borrow(stmt, holder) {
                return None;
            }
        }
        let t = &block.terminator;
        if term_mentions_local(t, holder) && !term_use_is_borrow(t, holder) {
            return None;
        }
    }
    Some(HolderUses {
        def: def?,
        retains,
        releases,
    })
}

/// Borrowed-holder RC elision.
///
/// A read through a nested place (`st.tables[i].slots[j]`,
/// `st.buffer[p]`, `keys[c]`) is lowered by copying each heap-valued
/// intermediate into a holder local, and the drop schedule gives that holder
/// a share of its own: a retain at the copy and a release on every exit. The
/// holder only ever borrows: the value it names stays owned by the place it
/// was copied from, so its share is redundant for as long as that place is
/// neither written nor released.
///
/// This pass removes the retain and every release of such a holder when all
/// of the following hold:
/// - the holder is a `String` or `Vec` local, not a parameter, not a region
///   local, and neither it nor the root of its chain is goroutine-shared;
/// - its one non-constant definition copies a projected place, and exactly
///   one retain accounts for it;
/// - every other read of it is a borrow: a projected read, an argument to a
///   user function or to a runtime helper known to only read it, or storage
///   bookkeeping - never a bare copy into another place, a capture, or a
///   reference;
/// - from the definition to each read, on every path, nothing writes into,
///   references, or releases the structure the chain aliases, and no call
///   receives a writable path to it (a bare reference-typed local, or a
///   runtime helper not known to only read).
///
/// Without the holder's share the structure keeps the value alive across
/// the window, which is what the retain was for; removing it changes no
/// count outside the window.
pub(crate) fn elide_borrowed_holder_rc(body: &mut Body, tcx: &TyCtxt) {
    let n_blocks = body.blocks.len();
    let n_locals = body.locals.len();
    if n_blocks == 0 || n_locals == 0 {
        return;
    }
    let share = crate::ownership::ShareFacts::compute(body);
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| successor_indices(&b.terminator))
        .collect();
    let defs = alias_defs(body);
    let roots: Vec<Local> = (0..n_locals)
        .map(|i| alias_root(&defs, Local(i as u32)))
        .collect();

    let mut elided = 0usize;
    for i in (body.arity as usize + 1)..n_locals {
        let holder = Local(i as u32);
        let decl = &body.locals[i];
        if decl.region || share.is_goroutine_shared(holder) {
            continue;
        }
        let Some((retain_names, release_names)) = holder_rc_names(tcx, decl.ty) else {
            continue;
        };
        let AliasDef::Copy(source) = defs[i] else {
            continue;
        };
        let root = roots[i];
        if root == holder || share.is_goroutine_shared(root) {
            continue;
        }
        let Some(uses) = classify_holder_uses(body, holder, source, retain_names, release_names)
        else {
            continue;
        };
        if uses.retains != 1 || uses.releases == 0 {
            continue;
        }
        let chain: Vec<bool> = roots.iter().map(|r| *r == root).collect();
        let borrow = Borrow {
            holder,
            root,
            chain: &chain,
        };
        if !holder_window_is_clean(body, tcx, &succs, uses.def, &borrow) {
            continue;
        }
        elided += 1;
        for block in &mut body.blocks {
            for stmt in &mut block.stmts {
                if stmt_is_rc_op_on(stmt, holder) {
                    stmt.kind = StatementKind::Nop;
                }
            }
        }
    }
    if elided > 0 && std::env::var_os("GOS_RC_ELIDE_STATS").is_some() {
        eprintln!("[rc-elide] {}: elided {elided} borrowed holder(s)", body.name);
    }
}
