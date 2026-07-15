/// Capacity planning only applies to a statically known `HashMap` local.
fn is_hashmap_local(body: &Body, tcx: &TyCtxt, local: Local) -> bool {
    let Some(decl) = body.locals.get(local.0 as usize) else {
        return false;
    };
    matches!(tcx.kind_of(decl.ty), TyKind::HashMap { .. })
}

fn reserve_bound_available_at_entry(body: &Body, bound: &Operand, allocation: BlockId) -> bool {
    match bound {
        Operand::Const(_) => true,
        Operand::Copy(place) if place.projection.is_empty() => {
            if place.local.0 == 0 {
                return false;
            }
            // Parameters exist at function entry. A local must instead have
            // one whole-local definition that dominates the constructor; a
            // second write anywhere in the body means a loop back-edge or a
            // branch can change the reserve bound, so remain conservative.
            place.local.0 <= body.arity
                || single_local_definition(body, place.local)
                    .is_some_and(|definition| block_dominates(body, definition, allocation))
        }
        _ => false,
    }
}

/// Returns the sole block that defines `local`, counting both statement and
/// call-result writes. Projection writes count too: a reserve bound must be
/// immutable for the whole loop, not merely free of direct scalar writes.
fn single_local_definition(body: &Body, local: Local) -> Option<BlockId> {
    let mut definition = None;
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, .. } = &stmt.kind
                && place.local == local
            {
                if definition.replace(block.id).is_some() {
                    return None;
                }
            }
        }
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.local == local
        {
            if definition.replace(block.id).is_some() {
                return None;
            }
        }
    }
    definition
}

/// True when every entry-to-`target` path passes through `gate`.
fn block_dominates(body: &Body, gate: BlockId, target: BlockId) -> bool {
    let Some(entry) = body.blocks.first().map(|block| block.id) else {
        return false;
    };
    if gate == target || gate == entry {
        return true;
    }
    let mut seen = HashSet::from([entry]);
    let mut work = VecDeque::from([entry]);
    while let Some(block_id) = work.pop_front() {
        if block_id == gate {
            continue;
        }
        if block_id == target {
            return false;
        }
        let Some(block) = body.blocks.get(block_id.0 as usize) else {
            return false;
        };
        for successor in terminator_successors(&block.terminator) {
            if successor != gate && seen.insert(successor) {
                work.push_back(successor);
            }
        }
    }
    true
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
            && loop_body_has_exactly_one_vec_push(body, body_entry, bid, vec_local)
        {
            return Some(bound);
        }
        for succ in terminator_successors(&block.terminator) {
            work.push_back((succ, depth + 1));
        }
    }
    None
}

fn counted_insert_loop_bound(body: &Body, start: BlockId, map_local: Local) -> Option<Operand> {
    let mut seen = HashSet::new();
    let mut work = VecDeque::from([(start, 0usize)]);
    while let Some((bid, depth)) = work.pop_front() {
        if depth > 64 || !seen.insert(bid) {
            continue;
        }
        let block = body.blocks.get(bid.0 as usize)?;
        if let Some((body_entry, bound)) = counted_loop_body_and_bound(block)
            && loop_body_has_exactly_one_map_insert(body, body_entry, bid, map_local)
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

/// Proves that every loop-body path reaches the loop head after exactly one
/// push into `vec_local`. A reserve is only exact under that condition: seeing
/// one push somewhere is insufficient when another branch skips it or pushes
/// twice. Loops inside the candidate body and paths which leave the body are
/// deliberately rejected rather than guessed about.
fn loop_body_has_exactly_one_vec_push(
    body: &Body,
    entry: BlockId,
    loop_head: BlockId,
    vec_local: Local,
) -> bool {
    let mut seen = HashSet::new();
    let mut work = VecDeque::from([(entry, 0u8)]);
    let mut reached_head = false;
    while let Some((bid, pushes)) = work.pop_front() {
        if bid == loop_head {
            if pushes != 1 {
                return false;
            }
            reached_head = true;
            continue;
        }
        if !seen.insert((bid, pushes)) {
            // A cycle in the candidate body makes the number of pushes
            // unbounded or path-dependent, so it is not an exact reserve.
            return false;
        }
        let Some(block) = body.blocks.get(bid.0 as usize) else {
            return false;
        };
        let pushes = pushes.saturating_add(u8::from(terminator_pushes_vec(
            &block.terminator,
            vec_local,
        )));
        if pushes > 1 {
            return false;
        }
        let successors = terminator_successors(&block.terminator);
        if successors.is_empty() {
            return false;
        }
        for successor in successors {
            work.push_back((successor, pushes));
        }
    }
    reached_head
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

fn loop_body_has_exactly_one_map_insert(
    body: &Body,
    entry: BlockId,
    loop_head: BlockId,
    map_local: Local,
) -> bool {
    let mut seen = HashSet::new();
    let mut work = VecDeque::from([(entry, 0u8)]);
    let mut reached_head = false;
    while let Some((bid, inserts)) = work.pop_front() {
        if bid == loop_head {
            if inserts != 1 {
                return false;
            }
            reached_head = true;
            continue;
        }
        if !seen.insert((bid, inserts)) {
            return false;
        }
        let Some(block) = body.blocks.get(bid.0 as usize) else {
            return false;
        };
        let inserts = inserts.saturating_add(u8::from(terminator_inserts_map(
            &block.terminator,
            map_local,
        )));
        if inserts > 1 {
            return false;
        }
        let successors = terminator_successors(&block.terminator);
        if successors.is_empty() {
            return false;
        }
        for successor in successors {
            work.push_back((successor, inserts));
        }
    }
    reached_head
}

fn terminator_inserts_map(term: &Terminator, map_local: Local) -> bool {
    let Terminator::Call { callee, args, .. } = term else {
        return false;
    };
    if !matches!(callee, Operand::Const(ConstValue::Str(name)) if matches!(name.as_str(),
        "gos_rt_map_insert"
            | "gos_rt_map_insert_i64_i64"
            | "gos_rt_map_insert_str_i64"
            | "gos_rt_map_insert_i64_str"
            | "gos_rt_map_insert_str_str"
            | "gos_rt_map_insert_skey"))
    {
        return false;
    }
    args.first().and_then(whole_copy_local) == Some(map_local)
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

