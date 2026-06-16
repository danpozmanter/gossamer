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

/// Inserts balanced `gos_rt_rc_retain` / `gos_rt_rc_release` calls for
/// reference-counted heap values so the compiled tier matches the
/// interpreter tier's `Arc` clone/drop semantics. This is the sound RC
/// model: the strong count always equals the number of live references,
/// so aliasing (`let b = a; let c = a`), returning a borrowed argument,
/// storing into a struct, etc. are all handled by the counts — there is
/// no fragile move/escape/ownership inference to get wrong.
///
/// Acquisitions (`+1`, emit a retain at the site) — any operation that
/// creates a new reference to an RC value:
/// - `to = Copy(from)` (binding/assignment, including into the return
///   slot — that mints the caller's reference),
/// - `gos_store(obj, off, val)` (the heap object gains a child reference;
///   freed transitively when the object's refcount hits zero),
/// - an aggregate operand / `Repeat` element (the struct/tuple/array
///   gains a reference),
/// - a consuming container/channel call argument.
///
/// Releases (`-1`): every RC-managed local that is neither a parameter
/// nor the return slot, at every return and before every reassignment.
/// Such locals are zeroed at entry so each release is null-safe on any
/// path. Parameters are borrowed (the caller owns and releases them) and
/// the return slot is transferred to the caller, so neither is released
/// here — and because every new reference retains, this is balanced with
/// no callee-signature analysis.
/// One field-level retain/release in the by-value-aggregate teardown:
/// `(is_retain, aggregate_local, field_index, is_weak)`.
type FieldGap = (bool, Local, u32, bool);

/// Forward-propagates a concrete local type through `B = Copy(A)` chains: when
/// `A` has a resolved type but `B` was left an inference variable, `B` takes
/// `A`'s type. A fixpoint, so chains (`A -> B -> C`) settle fully. Run before
/// the RC passes so a `?` / `unwrap` extraction (typed from the scrutinee's
/// substs) copied into an otherwise-`Var` binding is recognised as RC-managed
/// and released — without it the extracted `String` leaks.
pub(crate) fn propagate_copy_types(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;
    let n = body.locals.len();
    let unresolved = |ty| matches!(tcx.kind_of(ty), TyKind::Var(_) | TyKind::Error);
    // Type of `base.Field(idx)`, seeing through one `&`. Used to flow a
    // resolved aggregate's field type onto an otherwise-`Var` destination —
    // e.g. `inner = Copy(a.Field(0))` once `a` is known to be a struct.
    let field_ty = |base_ty: gossamer_types::Ty, idx: u32| -> Option<gossamer_types::Ty> {
        let mut t = base_ty;
        if let TyKind::Ref { inner, .. } = tcx.kind_of(t) {
            t = *inner;
        }
        match tcx.kind_of(t) {
            TyKind::Adt { def, .. } => tcx
                .struct_field_tys(*def)
                .and_then(|tys| tys.get(idx as usize).copied()),
            TyKind::Tuple(elems) => elems.get(idx as usize).copied(),
            _ => None,
        }
    };
    let mut changed = true;
    while changed {
        changed = false;
        let updates: Vec<(usize, gossamer_types::Ty)> = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter_map(|stmt| {
                let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(p)),
                } = &stmt.kind
                else {
                    return None;
                };
                if !place.projection.is_empty()
                    || (place.local.0 as usize) >= n
                    || (p.local.0 as usize) >= n
                    || !unresolved(body.locals[place.local.0 as usize].ty)
                {
                    return None;
                }
                let src_ty = body.locals[p.local.0 as usize].ty;
                if unresolved(src_ty) {
                    return None;
                }
                // Bare copy: destination inherits the source type directly.
                if p.projection.is_empty() {
                    return Some((place.local.0 as usize, src_ty));
                }
                // Single field projection: destination inherits the field type,
                // so a chain `a.inner.tag` resolves one level per fixpoint pass.
                if let [crate::ir::Projection::Field(idx)] = p.projection.as_slice() {
                    if let Some(ft) = field_ty(src_ty, *idx) {
                        if !unresolved(ft) {
                            return Some((place.local.0 as usize, ft));
                        }
                    }
                }
                None
            })
            .collect();
        for (d, ty) in updates {
            if unresolved(body.locals[d].ty) {
                body.locals[d].ty = ty;
                changed = true;
            }
        }
    }
}

pub(crate) fn insert_rc_releases(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    let n_locals = body.locals.len();
    if n_locals == 0 {
        return;
    }
    let arity = body.arity as usize;

    // An RC-managed local that is neither the return slot (0) nor a
    // parameter (1..=arity). `i > arity` excludes both.
    // Region-owned locals are excluded everywhere: their values are freed
    // wholesale at `arena_pop`, so emitting a retain/release would touch
    // freed memory after the pop.
    let is_rc = |i: usize| {
        i > arity && i < n_locals && tcx.is_rc_managed(body.locals[i].ty) && !body.locals[i].region
    };
    let rc_operand = |op: &Operand| -> Option<Local> {
        if let Operand::Copy(p) = op
            && p.projection.is_empty()
            && (p.local.0 as usize) < n_locals
            && tcx.is_rc_managed(body.locals[p.local.0 as usize].ty)
            && !body.locals[p.local.0 as usize].region
        {
            Some(p.local)
        } else {
            None
        }
    };
    // RC-managed field slots of a by-value aggregate (struct / tuple), as
    // (field_index, is_weak). In the LLVM backend such aggregates are stack
    // slots with no heap teardown, so the RC fields they retain at
    // construction/copy must be released when the local dies.
    let agg_rc_fields = |ty: Ty| -> Vec<(u32, bool)> {
        use gossamer_types::TyKind;
        let collect = |tys: &[Ty]| -> Vec<(u32, bool)> {
            tys.iter()
                .enumerate()
                .filter(|(_, t)| {
                    tcx.is_rc_managed(**t)
                        && !matches!(
                            tcx.kind_of(**t),
                            gossamer_types::TyKind::Vec(_) | gossamer_types::TyKind::Slice(_)
                        )
                })
                .map(|(i, t)| (u32::try_from(i).unwrap_or(0), tcx.is_weak_ty(*t)))
                .collect()
        };
        match tcx.kind_of(ty) {
            // The whole sentinel range is excluded, not just
            // Result/Option: `http::Response` (u32::MAX - 5) lowers as
            // an OPAQUE one-slot runtime handle and the heap-blob
            // sentinels (DirInfo/Output/ResponseStream) as one-slot
            // pointers — per-field stack accounting over their
            // DECLARED field lists reads and releases past the 1-slot
            // alloca, clobbering adjacent stack memory.
            TyKind::Adt { def, .. } if def.local < u32::MAX - 16 && !tcx.is_inline_enum_ty(ty) => {
                tcx.struct_field_tys(*def).map(collect).unwrap_or_default()
            }
            // A by-value tuple is a stack slot like a struct: each RC-managed
            // element retained when the tuple (or a destructured binding) takes
            // a reference must be released when that owner dies. `is_rc_managed`
            // already excludes Result/Option/inline-enum elements, so a packed
            // 2-word element never enters the per-field accounting.
            TyKind::Tuple(elems) => collect(elems),
            _ => Vec::new(),
        }
    };
    // (No early-out on `is_rc` locals alone: a function may only copy a
    // borrowed RC *parameter* into its return slot — e.g. `fn id(t: Tree)
    // -> Tree { t }` — which still needs a return-copy retain. The
    // empty-work check after collecting retain/release sites handles the
    // genuine no-op case.)

    // Retain sites within statement sequences: `(block, stmt_idx,
    // local, count)` — insert `count` retains of `local` just after the
    // statement. Collected first, applied after the release edits so
    // statement indices stay valid.
    // Self-accumulation copy-backs from the in-place string builder:
    // `tmp = gos_rt_str_concat_drop_a(s, frag)` (a block's Call terminator)
    // whose result is copied straight back — `s = Copy(tmp)` as the first
    // statement of the successor block. `concat_drop_a` consumes `s`'s old
    // buffer (appends in place, or reallocates and frees it) and returns the
    // new one, so this copy-back is a move that *replaces* `s`: it must NOT
    // retain `tmp` (that would drive the reused buffer's count above 1 and
    // force every append onto the copy-on-write path — O(n^2)) and must NOT
    // release the old `s` (already owned/freed by the call — double-free).
    // The `(succ_block, 0)` of each such copy-back is recorded here.
    let mut copyback_sites: std::collections::HashSet<(usize, usize)> =
        std::collections::HashSet::new();
    for block in &body.blocks {
        if let Terminator::Call {
            callee,
            args,
            destination,
            target: Some(succ),
        } = &block.terminator
            && matches!(callee, Operand::Const(ConstValue::Str(n))
                if n == "gos_rt_str_concat_drop_a"
                    || n == "gos_rt_str_append_i64"
                    || n == "gos_rt_str_append_f64")
            && destination.projection.is_empty()
            && let Some(Operand::Copy(arg0)) = args.first()
        {
            // The self-consuming append accumulator is `arg0` — a bare local
            // (`acc`) or a `&mut String` deref place (`*s`). The copy-back
            // stores `tmp` straight back into that same place; recognising it
            // (by matching both the local AND the projection) keeps the
            // accumulator off the retain-of-result / release-of-old paths.
            let (tmp, succ_idx) = (destination.local, succ.0 as usize);
            if succ_idx < body.blocks.len()
                && let Some(first) = body.blocks[succ_idx].stmts.first()
                && let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(src)),
                } = &first.kind
                && place.local == arg0.local
                && place.projection == arg0.projection
                && src.local == tmp
                && src.projection.is_empty()
            {
                copyback_sites.insert((succ_idx, 0));
            }
        }
    }
    // Post-call reload of a `&mut String` writeback (`L = *R`, where `R` is the
    // `&mut String` ref produced for the call): a copy-back, not a fresh
    // reassignment. The callee already released the value `L` previously held
    // (its `*R = …` displaced it through the slot), so the release-before-
    // reassignment that would otherwise fire for `L` must be suppressed — else
    // it double-frees. The reload itself takes no retain (its source is a
    // borrowed deref), so adding it here only cancels the spurious release.
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Copy(src)),
            } = &stmt.kind
                && place.projection.is_empty()
                && (src.local.0 as usize) < n_locals
                && matches!(src.projection.as_slice(), [crate::ir::Projection::Deref])
                && matches!(
                    tcx.kind_of(body.locals[src.local.0 as usize].ty),
                    gossamer_types::TyKind::Ref { inner, .. }
                        if matches!(tcx.kind_of(*inner), gossamer_types::TyKind::String)
                )
            {
                copyback_sites.insert((bi, si));
            }
        }
    }

    let mut retain_sites: Vec<(usize, usize, Local, usize)> = Vec::new();
    // Retains to emit at the end of a block (just before a consuming
    // terminator call), `(block, local)`.
    let mut terminator_retains: Vec<(usize, Local)> = Vec::new();
    // By-value enum locals loaded from a container slot the CONTAINER
    // still owns (`row[i]` via `gos_rt_vec_get_i128`, `xs.first()`,
    // `xs.last()`): their payload word is an interior borrow of the
    // vec's element, not the transferred single reference a consumed
    // `Result` hands to `?` / `unwrap()`. A `String` payload extracted
    // from one of these must RETAIN (the binding takes its own share;
    // the vec's `elem_kind` deep-free keeps the vec's), never move —
    // moving released the vec's only share and the deep-free at
    // `gos_rt_vec_free` then double-freed it.
    let mut borrowed_enum_src = vec![false; n_locals];
    for block in &body.blocks {
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            destination,
            ..
        } = &block.terminator
            && destination.projection.is_empty()
            && (destination.local.0 as usize) < n_locals
            && matches!(
                name.as_str(),
                "gos_rt_vec_get_i128" | "gos_rt_vec_first" | "gos_rt_vec_last"
            )
        {
            borrowed_enum_src[destination.local.0 as usize] = true;
        }
    }
    // Propagate forward through plain copies (`let opt = row[0]` then
    // matching on a scrutinee temp copied from `opt`).
    {
        let copy_edges: Vec<(usize, usize)> = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter_map(|stmt| {
                if let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(p)),
                } = &stmt.kind
                    && place.projection.is_empty()
                    && p.projection.is_empty()
                    && (place.local.0 as usize) < n_locals
                    && (p.local.0 as usize) < n_locals
                {
                    Some((place.local.0 as usize, p.local.0 as usize))
                } else {
                    None
                }
            })
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for &(dest, src) in &copy_edges {
                if borrowed_enum_src[src] && !borrowed_enum_src[dest] {
                    borrowed_enum_src[dest] = true;
                    changed = true;
                }
            }
        }
    }
    let enum_arg_is_borrowed = |args: &[Operand]| -> bool {
        matches!(
            args.first(),
            Some(Operand::Copy(p))
                if p.projection.is_empty()
                    && (p.local.0 as usize) < n_locals
                    && borrowed_enum_src[p.local.0 as usize]
        )
    };
    // Word-slot element loads out of a vec the CONTAINER still owns: the
    // `gos_rt_vec_get_i64` destination of a for-loop / index read. When the
    // loop lowering's element-type pin did not reach (`for s in
    // strings::split(...)` — a free-call iter expression), the destination
    // local is typed i64 and `rc_operand` cannot see the String underneath,
    // so the copy into a String-typed binding neither mints a share nor
    // schedules a release. The binding's release (or the caller's, when the
    // value is returned) then collides with the vec's `elem_kind` deep-free
    // — a double free. The retain/owned arms below mint a share for any
    // String-typed binding copied from one of these destinations, mirroring
    // the `borrowed_enum_src` extraction contract.
    let mut borrowed_word_elem_src = vec![false; n_locals];
    for block in &body.blocks {
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            destination,
            ..
        } = &block.terminator
            && destination.projection.is_empty()
            && (destination.local.0 as usize) < n_locals
            && matches!(
                name.as_str(),
                "gos_rt_vec_get_i64" | "gos_rt_vec_get_i64_unchecked"
            )
        {
            borrowed_word_elem_src[destination.local.0 as usize] = true;
        }
    }
    let is_string_local = |l: Local| -> bool {
        (l.0 as usize) < n_locals
            && matches!(
                tcx.kind_of(body.locals[l.0 as usize].ty),
                gossamer_types::TyKind::String
            )
    };
    // Locals holding a `String` payload freshly extracted from a consumed
    // by-value `Result`/`Option` (`f()?`, `r.unwrap()`). The extraction yields
    // the single owning reference the enum held, so copying it into the binding
    // (`let s = f()?`) must MOVE rather than retain — a retain there leaves the
    // extracted reference dangling once the binding is released (a leak).
    // Restricted to `String` payloads: an aggregate (`Adt`) payload carries
    // nested-RC fields whose release is balanced by the copy retain, so moving
    // it would double-free (the `from_json -> Config` path).
    let mut extraction_results = vec![false; n_locals];
    // Locals whose every whole-local assignment is a CONSTANT (the
    // tagged-null unit-variant representation, null-outs): such values
    // are immortal-by-construction — retaining/releasing them is a
    // guaranteed runtime no-op, so skip emitting the calls at all.
    let mut saw_const_assign = vec![false; n_locals];
    let mut saw_other_assign = vec![false; n_locals];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
                && (place.local.0 as usize) < n_locals
            {
                // Only INTEGER constants qualify: the tagged-null
                // unit-variant representation and null-outs. A string
                // literal is a real heap-shaped value whose holders
                // retain it — eliding those desynchronizes the
                // accounting.
                if matches!(rvalue, Rvalue::Use(Operand::Const(ConstValue::Int(_)))) {
                    saw_const_assign[place.local.0 as usize] = true;
                } else {
                    saw_other_assign[place.local.0 as usize] = true;
                }
            }
        }
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.projection.is_empty()
            && (destination.local.0 as usize) < n_locals
        {
            saw_other_assign[destination.local.0 as usize] = true;
        }
    }
    // Parameters and the return slot receive their values from the
    // caller — never const-only, regardless of body-local assignments.
    let const_init_only: Vec<bool> = (0..n_locals)
        .map(|i| i > body.arity as usize && saw_const_assign[i] && !saw_other_assign[i])
        .collect();

    // Locals stored into a heap aggregate (`gos_store` value argument). The
    // store gives the aggregate a reference; the copy that fed the store is
    // therefore load-bearing (extract -> retain -> store keeps one, the binding
    // release drops back to one). Such a binding must NOT have its retain
    // skipped, or the aggregate's later release double-frees (the synthesized
    // `from_json` parses a field String and stores it into the struct).
    let mut stored_into_aggregate = vec![false; n_locals];
    {
        use gossamer_types::TyKind;
        // A `String` payload (or one left unresolved as `Var` — the nested
        // `?` in a function whose own return type doesn't pin the Ok type, so
        // inference never settles the extraction local). Aggregate (`Adt`)
        // payloads are excluded; even if one slips through as `Var`, the
        // transitive `stored_into_aggregate` gate keeps its retain.
        let is_str = |l: Local| {
            (l.0 as usize) < n_locals
                && matches!(
                    tcx.kind_of(body.locals[l.0 as usize].ty),
                    TyKind::String | TyKind::Var(_)
                )
        };
        let mark_stored = |op: &Operand, set: &mut [bool]| {
            if let Operand::Copy(p) = op
                && p.projection.is_empty()
                && (p.local.0 as usize) < n_locals
            {
                set[p.local.0 as usize] = true;
            }
        };
        for block in &body.blocks {
            for stmt in &block.stmts {
                let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                    continue;
                };
                match rvalue {
                    Rvalue::CallIntrinsic { name, args } => {
                        // Borrowed-slot extractions are NOT moves — see
                        // `borrowed_enum_src`.
                        if *name == "gos_rt_result_payload"
                            && place.projection.is_empty()
                            && is_str(place.local)
                            && !enum_arg_is_borrowed(args)
                        {
                            extraction_results[place.local.0 as usize] = true;
                        }
                        // `gos_store` (object field write) and `gos_rt_result_new`
                        // (`Ok`/`Some` payload) both take ownership of the value
                        // argument; the copy feeding them keeps its retain.
                        let stored_args: &[Operand] = if *name == "gos_store" {
                            args.get(2).map(std::slice::from_ref).unwrap_or(&[])
                        } else if *name == "gos_rt_result_new" {
                            args
                        } else {
                            &[]
                        };
                        for op in stored_args {
                            mark_stored(op, &mut stored_into_aggregate);
                        }
                    }
                    // Struct / tuple / enum construction owns each operand.
                    Rvalue::Aggregate { operands, .. } => {
                        for op in operands {
                            mark_stored(op, &mut stored_into_aggregate);
                        }
                    }
                    _ => {}
                }
            }
            if let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                destination,
                args,
                ..
            } = &block.terminator
                && *name == "gos_rt_result_unwrap"
                && destination.projection.is_empty()
                && is_str(destination.local)
                && !enum_arg_is_borrowed(args)
            {
                extraction_results[destination.local.0 as usize] = true;
            }
        }
        // Propagate "stored" backward through `dest = Copy(src)` edges: a value
        // copied into a binding that is itself stored is also (transitively)
        // stored, so its own copy retain is load-bearing. This catches the
        // multi-hop flow the synthesized `from_json` uses (parse a field String,
        // copy it through temporaries, then place it in the result struct).
        let copy_edges: Vec<(usize, usize)> = body
            .blocks
            .iter()
            .flat_map(|b| &b.stmts)
            .filter_map(|stmt| {
                if let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(p)),
                } = &stmt.kind
                    && place.projection.is_empty()
                    && p.projection.is_empty()
                    && (place.local.0 as usize) < n_locals
                    && (p.local.0 as usize) < n_locals
                {
                    Some((place.local.0 as usize, p.local.0 as usize))
                } else {
                    None
                }
            })
            .collect();
        let mut changed = true;
        while changed {
            changed = false;
            for &(dest, src) in &copy_edges {
                if stored_into_aggregate[dest] && !stored_into_aggregate[src] {
                    stored_into_aggregate[src] = true;
                    changed = true;
                }
            }
        }
    }
    for (block_idx, block) in body.blocks.iter().enumerate() {
        for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            match rvalue {
                // New binding/alias to an RC value (covers `RETURN =
                // Copy(x)`, which mints the caller's reference).
                Rvalue::Use(op) => {
                    // Skip the retain only for a by-value enum-payload extraction
                    // moved into a binding that is NOT itself stored into an
                    // aggregate. A stored binding keeps the retain (the store
                    // consumes one reference, the binding release drops the
                    // other) — see `stored_into_aggregate`.
                    let skip_extraction_move = rc_operand(op).is_some_and(|l| {
                        extraction_results[l.0 as usize]
                            && !(place.projection.is_empty()
                                && stored_into_aggregate[place.local.0 as usize])
                    });
                    if let Some(l) = rc_operand(op)
                        && !copyback_sites.contains(&(block_idx, stmt_idx))
                        && !skip_extraction_move
                    {
                        retain_sites.push((block_idx, stmt_idx, l, 1));
                    }
                    // A String-typed binding copied from an untyped borrowed
                    // word-slot element (see `borrowed_word_elem_src`): mint
                    // the binding's share here — the vec's deep-free keeps
                    // the container's. `rc_operand` is None for these (the
                    // source local is typed i64), so this never doubles the
                    // retain above.
                    if rc_operand(op).is_none()
                        && let Operand::Copy(p) = op
                        && p.projection.is_empty()
                        && (p.local.0 as usize) < n_locals
                        && borrowed_word_elem_src[p.local.0 as usize]
                        && place.projection.is_empty()
                        && is_string_local(place.local)
                        && !copyback_sites.contains(&(block_idx, stmt_idx))
                    {
                        retain_sites.push((block_idx, stmt_idx, p.local, 1));
                    }
                }
                // Storing an RC child into a heap object — the object
                // gains a reference (released via its type-meta on free).
                Rvalue::CallIntrinsic { name, args } if *name == "gos_store" => {
                    if let Some(l) = args.get(2).and_then(&rc_operand) {
                        retain_sites.push((block_idx, stmt_idx, l, 1));
                    }
                }
                // `dest = gos_enum_tag(src, disc)` is an IDENTITY alias of
                // the same allocation (the tag bits live in the pointer):
                // ownership-wise it is `dest = Copy(src)` — retain the
                // source (move elision transfers instead when this is its
                // only read).
                Rvalue::CallIntrinsic { name, args } if *name == "gos_enum_tag" => {
                    if let Some(l) = args.first().and_then(&rc_operand) {
                        retain_sites.push((block_idx, stmt_idx, l, 1));
                    }
                }
                // Wrapping an RC value into a `Result` (`Ok(v)` / `Err(v)`).
                // The Result carries the reference out (it flows into the
                // return or is unwrapped by `?`), so the payload is
                // acquired here. Without this, `Ok(J::Obj(ps))` released the
                // enum payload while the returned Result still pointed at
                // it — a use-after-free that dropped a node from every
                // `self.parse()?`-built tree.
                Rvalue::CallIntrinsic { name, args } if *name == "gos_rt_result_new" => {
                    if let Some(l) = args.get(1).and_then(&rc_operand) {
                        retain_sites.push((block_idx, stmt_idx, l, 1));
                    }
                }
                // Aggregate fields / repeated elements — the
                // struct/tuple/array gains a reference per slot.
                Rvalue::Aggregate { operands, .. } => {
                    for op in operands {
                        if let Some(l) = rc_operand(op) {
                            retain_sites.push((block_idx, stmt_idx, l, 1));
                        }
                    }
                }
                Rvalue::Repeat { value, count } => {
                    if let Some(l) = rc_operand(value) {
                        retain_sites.push((block_idx, stmt_idx, l, *count as usize));
                    }
                }
                _ => {}
            }
        }
        if let Terminator::Call { callee, args, .. } = &block.terminator
            && let Operand::Const(ConstValue::Str(name)) = callee
            && is_consuming_call(name)
        {
            // arg0 is the container/channel/closure RECEIVER (borrowed, mutated
            // in place) — only the value argument(s) (arg1..) are consumed and
            // gain a stored reference. Retaining the receiver too (now that it
            // is RC-managed) would over-retain it and leak it.
            for arg in args.iter().skip(1) {
                if let Some(l) = rc_operand(arg) {
                    terminator_retains.push((block_idx, l));
                }
            }
        }
    }

    // A local is *owned* (holds a reference this function must release)
    // only when an assignment gives it ownership:
    // - `gos_rc_alloc` (fresh allocation),
    // - a user-function call that returns an RC value (the callee minted
    //   the caller's reference via its return-copy retain),
    // - `to = Copy(from)` of an RC value (retained above).
    // Values *loaded* from a structure (`gos_load`, match-arm bindings,
    // field/index reads) or returned by a runtime accessor are interior
    // borrows — the containing object still owns them, so releasing them
    // here would double-free. They are excluded.
    // Locals that are the source of a bare `x = Copy(y)` statement: the copy
    // *target* becomes the owner. Used below to decide whether a by-value enum
    // payload extraction is owned by this frame (used inline) or by a binding
    // it was copied into (`let x = to_json()?`).
    let mut copy_sourced = vec![false; n_locals];
    // Locals that are the destination of a bare `L = Copy(..)`. With
    // `copy_sourced` this flags an enum value that is aliased (copied to/from
    // another binding); its by-value payload pointer is then shared, so no
    // extraction may own/release it — matching both aliases would otherwise
    // double-free the one payload.
    let mut copy_target = vec![false; n_locals];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::Use(Operand::Copy(p)),
            } = &stmt.kind
                && p.projection.is_empty()
                && (p.local.0 as usize) < n_locals
            {
                copy_sourced[p.local.0 as usize] = true;
                if place.projection.is_empty() && (place.local.0 as usize) < n_locals {
                    copy_target[place.local.0 as usize] = true;
                }
            }
        }
    }
    // A `String` payload extracted out of a BORROWED container slot
    // (see `borrowed_enum_src`) into a binding this frame will own and
    // release (the `owned` `gos_rt_result_payload` arm below) needs its
    // own share: retain at the extraction site so the binding's release
    // and the container's element deep-free are both balanced. The
    // gating mirrors that `owned` arm exactly — retain iff a release
    // will be scheduled.
    for (block_idx, block) in body.blocks.iter().enumerate() {
        for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::CallIntrinsic { name, args },
            } = &stmt.kind
                && *name == "gos_rt_result_payload"
                && place.projection.is_empty()
                && (place.local.0 as usize) < n_locals
                && !copy_sourced[place.local.0 as usize]
                && matches!(
                    tcx.kind_of(body.locals[place.local.0 as usize].ty),
                    gossamer_types::TyKind::String
                )
                && enum_arg_is_borrowed(args)
                && match args.first() {
                    Some(Operand::Copy(p)) if p.projection.is_empty() => {
                        let e = p.local.0 as usize;
                        e >= n_locals || (!copy_sourced[e] && !copy_target[e])
                    }
                    _ => true,
                }
            {
                retain_sites.push((block_idx, stmt_idx, place.local, 1));
            }
        }
    }

    let mut owned = vec![false; n_locals];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
            {
                let i = place.local.0 as usize;
                if i >= n_locals {
                    continue;
                }
                if body.locals[i].region {
                    // Region-owned: freed wholesale at pop, never released here.
                    continue;
                }
                match rvalue {
                    Rvalue::CallIntrinsic { name, .. }
                        if *name == "gos_rc_alloc" || *name == "gos_rc_alloc_tagged" =>
                    {
                        owned[i] = true;
                    }
                    // A `String` payload moved out of a consumed by-value
                    // `Result`/`Option`/inline enum (`match o { Some(s) => … }`)
                    // and used INLINE (not copied into an owning binding). The
                    // frame owns it and must release it; the enum value itself
                    // frees nothing. When the extraction is copied into a
                    // binding (`let x = to_json()?`), that binding owns it (the
                    // `Use(Copy)` arm below) — so this arm excludes
                    // `copy_sourced` to avoid double-freeing the autoderive path.
                    // A `String` payload moved out of a consumed by-value
                    // `Result`/`Option`/inline enum (`match o { Some(s) => … }`)
                    // and used INLINE (not copied into an owning binding). The
                    // frame owns it and must release it; the enum value itself
                    // frees nothing. When the extraction is copied into a
                    // binding (`let x = to_json()?`), that binding owns it (the
                    // `Use(Copy)` arm below) — so this arm excludes
                    // `copy_sourced` to avoid double-freeing the autoderive path.
                    Rvalue::CallIntrinsic { name, args }
                        if *name == "gos_rt_result_payload"
                            && !copy_sourced[i]
                            && matches!(
                                tcx.kind_of(body.locals[i].ty),
                                gossamer_types::TyKind::String
                            )
                            && match args.first() {
                                Some(Operand::Copy(p)) if p.projection.is_empty() => {
                                    let e = p.local.0 as usize;
                                    e >= n_locals || (!copy_sourced[e] && !copy_target[e])
                                }
                                _ => true,
                            } =>
                    {
                        owned[i] = true;
                    }
                    Rvalue::Use(Operand::Copy(p))
                        if p.projection.is_empty()
                            && (p.local.0 as usize) < n_locals
                            && tcx.is_rc_managed(body.locals[p.local.0 as usize].ty) =>
                    {
                        owned[i] = true;
                    }
                    // A String binding copied from an untyped borrowed
                    // word-slot element: the retain arm above minted its
                    // share, so the frame owns and releases it like any
                    // RC copy (the source local is typed i64, so the
                    // rc-managed arm above cannot see it).
                    Rvalue::Use(Operand::Copy(p))
                        if p.projection.is_empty()
                            && (p.local.0 as usize) < n_locals
                            && borrowed_word_elem_src[p.local.0 as usize]
                            && matches!(
                                tcx.kind_of(body.locals[i].ty),
                                gossamer_types::TyKind::String
                            ) =>
                    {
                        owned[i] = true;
                    }
                    // Identity tag of an RC enum pointer: same ownership
                    // shape as `Copy`.
                    Rvalue::CallIntrinsic { name, args }
                        if *name == "gos_enum_tag"
                            && matches!(
                                args.first(),
                                Some(Operand::Copy(p))
                                    if p.projection.is_empty()
                                        && (p.local.0 as usize) < n_locals
                                        && tcx.is_rc_managed(
                                            body.locals[p.local.0 as usize].ty
                                        )
                            ) =>
                    {
                        owned[i] = true;
                    }
                    // `s = Copy(payload)` where the `?` / match extraction left
                    // the source local typed `Var` but the binding settled on a
                    // concrete RC type (`let s = f()?` for `f -> Result<String,
                    // _>`): the consumed enum transferred the payload's
                    // ownership to this binding, so the frame must release it.
                    // Field-extract `X = Copy(Y.field)` of an RC field: X owns a
                    // new reference to that value (retained at the extract site
                    // in the field pass), released at scope like any RC local.
                    Rvalue::Use(Operand::Copy(p))
                        if p.projection.len() == 1 && (p.local.0 as usize) < n_locals =>
                    {
                        if let crate::ir::Projection::Field(fidx) = p.projection[0] {
                            let base_ty = body.locals[p.local.0 as usize].ty;
                            if agg_rc_fields(base_ty).iter().any(|(f, _)| *f == fidx) {
                                owned[i] = true;
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
        if let Terminator::Call {
            callee,
            destination,
            ..
        } = &block.terminator
            && destination.projection.is_empty()
        {
            let i = destination.local.0 as usize;
            // A user function transfers ownership of its RC return value;
            // a runtime accessor (`gos_rt_*`) or a raw `gos_load` /
            // `gos_store` may hand back an interior borrow it still owns,
            // so do not treat that as owned. `gos_load` appears in
            // terminator position (not just as a `CallIntrinsic`
            // statement) when it sits at a block boundary — e.g. the
            // element load of a `for x in xs` loop body. Releasing such a
            // borrow frees a value the container still owns (double-free /
            // use-after-free on the next iteration).
            // `gos_rt_rc_downgrade` is the one runtime call that hands
            // back an *owned* reference (a fresh weak count) rather than
            // an interior borrow: the local owns that weak count and must
            // weak_release it at scope end. Every other `gos_rt_*` return
            // is a borrow the runtime still owns.
            let owns_return = match callee {
                Operand::FnRef { .. } => true,
                Operand::Const(ConstValue::Str(name)) => {
                    (!name.starts_with("gos_rt_") && name != "gos_load" && name != "gos_store")
                        || name == "gos_rt_rc_downgrade"
                        || mints_owned_string(name)
                }
                _ => true,
            };
            // Region-owned call results (e.g. a tree built inside a region
            // block) are freed at pop — never release them here.
            if owns_return && i < n_locals && !body.locals[i].region {
                owned[i] = true;
            }
        }
    }

    // Move elision. An owned local that is *read exactly once*, and whose
    // single read is a consuming acquisition (copy / store / aggregate /
    // container-push), transfers its single reference to the new owner:
    // no retain at that site and no release of the source. This collapses
    // the common construct-and-move pattern (build a child, store it into
    // a node, return the node) to zero refcount traffic, while genuine
    // aliasing (`let b = a; let c = a`, two reads) still retains.
    //
    // `total_reads` must never *under*-count, or a still-aliased value
    // would be elided and double-freed; counting every operand and
    // place-base appearance (writes excepted) keeps it conservative.
    let mut total_reads = vec![0u32; n_locals];
    let bump = |reads: &mut [u32], op: &Operand| {
        // Only a bare (unprojected) Copy aliases the value itself; a
        // projected copy reads a field, which is a separate value.
        if let Operand::Copy(p) = op
            && p.projection.is_empty()
        {
            let i = p.local.0 as usize;
            if i < n_locals {
                reads[i] = reads[i].saturating_add(1);
            }
        }
    };
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                match rvalue {
                    Rvalue::Use(op)
                    | Rvalue::UnaryOp { operand: op, .. }
                    | Rvalue::Cast { operand: op, .. }
                    | Rvalue::Repeat { value: op, .. } => bump(&mut total_reads, op),
                    Rvalue::BinaryOp { lhs, rhs, .. } => {
                        bump(&mut total_reads, lhs);
                        bump(&mut total_reads, rhs);
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        for op in operands {
                            bump(&mut total_reads, op);
                        }
                    }
                    Rvalue::CallIntrinsic { name, args } => {
                        if *name == "gos_store" {
                            // Only the stored value (arg 2) flows; the
                            // object (arg 0) is merely written through.
                            if let Some(op) = args.get(2) {
                                bump(&mut total_reads, op);
                            }
                        } else if *name == "gos_enum_set_disc" {
                            // Writes the discriminant byte through the
                            // pointer; aliases nothing.
                        } else if *name != "gos_load" && *name != "gos_enum_disc" {
                            // `gos_load` / `gos_enum_disc` only access
                            // their object; every other intrinsic
                            // consumes its args.
                            for op in args {
                                bump(&mut total_reads, op);
                            }
                        }
                    }
                    // `Ref`/`Len`/projected reads access memory, they do
                    // not alias the bare value.
                    Rvalue::Ref { .. } | Rvalue::Len(_) => {}
                    // Reads a scalar global by symbol; aliases no local.
                    Rvalue::StaticLoad(_) => {}
                }
            }
        }
        match &block.terminator {
            Terminator::SwitchInt { discriminant, .. } => bump(&mut total_reads, discriminant),
            Terminator::Call { callee, args, .. } => {
                bump(&mut total_reads, callee);
                for op in args {
                    bump(&mut total_reads, op);
                }
            }
            Terminator::Assert { cond, .. } => bump(&mut total_reads, cond),
            _ => {}
        }
    }
    // A local has a consuming read iff it sources a retain site.
    let mut consuming_read = vec![false; n_locals];
    for (_, _, l, _) in &retain_sites {
        let i = l.0 as usize;
        if i < n_locals {
            consuming_read[i] = true;
        }
    }
    for (_, l) in &terminator_retains {
        let i = l.0 as usize;
        if i < n_locals {
            consuming_read[i] = true;
        }
    }
    let moved: Vec<bool> = (0..n_locals)
        .map(|i| owned[i] && total_reads[i] == 1 && consuming_read[i])
        .collect();

    // Drop retains whose source is moved (the single reference transfers
    // to the new owner; no `+1`).
    retain_sites.retain(|(_, _, l, _)| !moved[l.0 as usize]);
    terminator_retains.retain(|(_, l)| !moved[l.0 as usize]);
    // Drop retains of immortal-by-construction constants.
    retain_sites.retain(|(_, _, l, _)| !const_init_only[l.0 as usize]);
    terminator_retains.retain(|(_, l)| !const_init_only[l.0 as usize]);

    // Releasable owners: RC locals (not parameter / return slot) that are
    // owned here and not moved out. Each surviving new reference was
    // retained above, so releasing every owner keeps the count balanced.
    // A local whose value flows into the return slot must NOT be released here
    // — the caller receives and owns it (else an owned producer result that is
    // returned would be freed at scope AND by the caller). Backward closure
    // from `Local::RETURN` over bare `Copy` and aggregate-operand edges.
    let mut flows_to_return = vec![false; n_locals];
    flows_to_return[Local::RETURN.0 as usize] = true;
    let mut rf_changed = true;
    while rf_changed {
        rf_changed = false;
        for block in &body.blocks {
            for stmt in &block.stmts {
                let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                    continue;
                };
                if !place.projection.is_empty() || (place.local.0 as usize) >= n_locals {
                    continue;
                }
                if !flows_to_return[place.local.0 as usize] {
                    continue;
                }
                let mut mark = |l: Local, ch: &mut bool| {
                    let f = l.0 as usize;
                    if f < n_locals && !flows_to_return[f] {
                        flows_to_return[f] = true;
                        *ch = true;
                    }
                };
                match rvalue {
                    Rvalue::Use(Operand::Copy(pp)) if pp.projection.is_empty() => {
                        mark(pp.local, &mut rf_changed);
                    }
                    // Identity tag: the source IS the returned allocation.
                    Rvalue::CallIntrinsic { name, args } if *name == "gos_enum_tag" => {
                        if let Some(Operand::Copy(pp)) = args.first()
                            && pp.projection.is_empty()
                        {
                            mark(pp.local, &mut rf_changed);
                        }
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        for op in operands {
                            if let Operand::Copy(pp) = op
                                && pp.projection.is_empty()
                            {
                                mark(pp.local, &mut rf_changed);
                            }
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    let releasable: Vec<Local> = (0..n_locals)
        .filter(|&i| is_rc(i) && owned[i] && !moved[i] && !flows_to_return[i])
        .map(|i| Local(u32::try_from(i).unwrap_or(0)))
        .collect();

    // Aggregate locals whose every whole-local assignment is a
    // `gos_rt_result_payload` extraction are BORROWS: the source
    // Result owns the payload's fields, the extraction never retained
    // them, so it must not release them at death either.
    let mut extraction_seed = vec![false; n_locals];
    {
        let mut non_extraction = vec![false; n_locals];
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind
                    && place.projection.is_empty()
                    && (place.local.0 as usize) < n_locals
                {
                    if matches!(
                        rvalue,
                        Rvalue::CallIntrinsic { name, .. } if *name == "gos_rt_result_payload"
                    ) {
                        extraction_seed[place.local.0 as usize] = true;
                    } else {
                        non_extraction[place.local.0 as usize] = true;
                    }
                }
            }
            if let Terminator::Call { destination, .. } = &block.terminator
                && destination.projection.is_empty()
                && (destination.local.0 as usize) < n_locals
            {
                non_extraction[destination.local.0 as usize] = true;
            }
        }
        for i in 0..n_locals {
            extraction_seed[i] = extraction_seed[i] && !non_extraction[i];
        }
    }
    // Field-extract `X = Copy(Y.field)` of an RC field: X holds a fresh
    // reference to the field value, so retain it. Added after move-elision
    // filtering so it always fires — Y still owns its own copy of the field
    // and releases it when Y dies.
    for (block_idx, block) in body.blocks.iter().enumerate() {
        for (stmt_idx, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
                && (place.local.0 as usize) < n_locals
                && let Rvalue::Use(Operand::Copy(src)) = rvalue
                && src.projection.len() == 1
                && let crate::ir::Projection::Field(fidx) = src.projection[0]
                && (src.local.0 as usize) < n_locals
                && agg_rc_fields(body.locals[src.local.0 as usize].ty)
                    .iter()
                    .any(|(f, _)| *f == fidx)
            {
                retain_sites.push((block_idx, stmt_idx, place.local, 1));
            }
        }
    }

    // Aggregate locals that are BORROWS of a container element: the
    // `for p in &v` loop variable, whose value is `Copy`-ed from a
    // `gos_rt_vec_get_ptr` interior pointer the vec still owns. Such a
    // local must NOT release the element's RC fields — the container (or
    // the by-value aggregate that was pushed into it) owns them, so a
    // per-field release here double-frees with the owner's release. The
    // get_ptr result type is a raw element pointer, so the copy-on-load
    // never minted a balancing retain; treat the whole local as a
    // non-owning view. Mirrors `extraction_seed`, but propagates through
    // the `loopvar = Copy(get_ptr_result)` edge the loop lowering emits.
    let vec_borrow_agg = {
        let mut get_ptr_dest = vec![false; n_locals];
        for block in &body.blocks {
            if let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                destination,
                ..
            } = &block.terminator
                && destination.projection.is_empty()
                && (destination.local.0 as usize) < n_locals
                && name == "gos_rt_vec_get_ptr"
            {
                get_ptr_dest[destination.local.0 as usize] = true;
            }
        }
        // A whole-local assignment that is neither a bare `Copy` nor the
        // get_ptr terminator gives the local an owned value — disqualify.
        // Collect the copy sources so the fixpoint can require every one
        // to itself be a borrow.
        let mut disqualified = vec![false; n_locals];
        let mut copy_srcs: Vec<Vec<usize>> = vec![Vec::new(); n_locals];
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind
                    && place.projection.is_empty()
                    && (place.local.0 as usize) < n_locals
                {
                    let i = place.local.0 as usize;
                    match rvalue {
                        Rvalue::Use(Operand::Copy(src))
                            if src.projection.is_empty() && (src.local.0 as usize) < n_locals =>
                        {
                            copy_srcs[i].push(src.local.0 as usize);
                        }
                        _ => disqualified[i] = true,
                    }
                }
            }
            // A non-get_ptr call destination is an owned result.
            if let Terminator::Call { destination, .. } = &block.terminator
                && destination.projection.is_empty()
                && (destination.local.0 as usize) < n_locals
                && !get_ptr_dest[destination.local.0 as usize]
            {
                disqualified[destination.local.0 as usize] = true;
            }
        }
        let mut borrow = get_ptr_dest.clone();
        let mut changed = true;
        while changed {
            changed = false;
            for i in 0..n_locals {
                if borrow[i] || disqualified[i] || copy_srcs[i].is_empty() {
                    continue;
                }
                if copy_srcs[i].iter().all(|&s| borrow[s]) {
                    borrow[i] = true;
                    changed = true;
                }
            }
        }
        borrow
    };

    // By-value aggregate locals (struct / tuple, not a parameter / region)
    // carrying RC fields that need per-field retain (on copy) + release (on
    // drop), since the stack-slot aggregate itself has no heap teardown.
    let agg_locals: Vec<(usize, Vec<(u32, bool)>)> = ((arity + 1)..n_locals)
        .filter(|&i| !body.locals[i].region && !extraction_seed[i] && !vec_borrow_agg[i])
        .filter_map(|i| {
            let fields = agg_rc_fields(body.locals[i].ty);
            if fields.is_empty() {
                None
            } else {
                Some((i, fields))
            }
        })
        .collect();

    if releasable.is_empty()
        && retain_sites.is_empty()
        && terminator_retains.is_empty()
        && agg_locals.is_empty()
    {
        return;
    }

    let releasable_set: std::collections::HashSet<u32> = releasable.iter().map(|l| l.0).collect();
    let n_blocks = body.blocks.len();

    // Per-block, per-gap insertions. `gaps[b][g]` lists the retain/
    // release calls to emit just before the original statement at index
    // `g` (gap `len` = just before the terminator). Building all
    // insertions against the *original* indices and then rebuilding each
    // block in one pass keeps positions valid regardless of how many
    // statements are inserted.
    let mut gaps: Vec<Vec<Vec<(bool, Local)>>> = body
        .blocks
        .iter()
        .map(|b| vec![Vec::new(); b.stmts.len() + 1])
        .collect();

    // Parallel to `gaps`, but each entry is (is_retain, local, field_index,
    // is_weak) — a retain/release of one RC field of a by-value aggregate.
    let mut field_gaps: Vec<Vec<Vec<FieldGap>>> = body
        .blocks
        .iter()
        .map(|b| vec![Vec::new(); b.stmts.len() + 1])
        .collect();

    for bi in 0..n_blocks {
        let len = body.blocks[bi].stmts.len();
        // Release before each stmt-position reassignment of an owner — for
        // ANY rvalue, not just `gos_rc_alloc`. A named binding rebound in a
        // loop (`let t = build(d)`, where the build result is `Copy`-ed into
        // `t`) must release the previous iteration's value before it is
        // overwritten, or every iteration's value leaks until the function
        // returns. The entry zero-init makes the first release (of the
        // null initial value) safe; on the loop back-edge the incoming value
        // is the previous iteration's owned object, which is then freed.
        for (si, stmt) in body.blocks[bi].stmts.iter().enumerate() {
            if let StatementKind::Assign { place, .. } = &stmt.kind
                && place.projection.is_empty()
                && releasable_set.contains(&place.local.0)
                && !copyback_sites.contains(&(bi, si))
            {
                gaps[bi][si].push((false, place.local));
            }
        }
        // Release before a Call-terminator reassignment of an owner — unless
        // the call *consumes* the old value of that same local. The in-place
        // string builder `s = gos_rt_str_concat_drop_a(s, frag)` reads `s`,
        // appends in place (or reallocates and frees the old buffer), and
        // returns the result: it already owns/frees the old `s`, so releasing
        // it here would read freed memory and double-free.
        if let Terminator::Call {
            destination,
            callee,
            args,
            ..
        } = &body.blocks[bi].terminator
            && destination.projection.is_empty()
            && releasable_set.contains(&destination.local.0)
        {
            let self_consuming = matches!(callee, Operand::Const(ConstValue::Str(n))
                if n == "gos_rt_str_concat_drop_a"
                    || n == "gos_rt_str_append_i64"
                    || n == "gos_rt_str_append_f64")
                && matches!(args.first(), Some(Operand::Copy(p)) if p.projection.is_empty() && p.local == destination.local);
            if !self_consuming {
                gaps[bi][len].push((false, destination.local));
            }
        }
        // Retain element/value before a consuming container/channel call.
        // (recorded in `terminator_retains`)
        // Release every owner at each return.
        if matches!(body.blocks[bi].terminator, Terminator::Return) {
            for &local in &releasable {
                gaps[bi][len].push((false, local));
            }
        }
    }
    // Retain after each acquisition statement (gap = stmt_idx + 1).
    for (bi, si, local, count) in &retain_sites {
        for _ in 0..*count {
            gaps[*bi][*si + 1].push((true, *local));
        }
    }
    // Retain consuming-call arguments just before the terminator.
    for (bi, local) in &terminator_retains {
        let len = body.blocks[*bi].stmts.len();
        gaps[*bi][len].push((true, *local));
    }

    // Field-level retain/release for by-value aggregate locals: release the
    // previous value's RC fields before any reassignment (null-safe on the
    // first assignment via the entry zero-init), retain the shared fields after
    // a struct copy, and release every aggregate's fields at return.
    for (bi, block) in body.blocks.iter().enumerate() {
        let len = block.stmts.len();
        for (si, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
            {
                // Release the previous value's RC fields before reassigning an
                // owned aggregate local (null-safe first time via zero-init).
                if let Some((_, fields)) = agg_locals
                    .iter()
                    .find(|(l, _)| *l == place.local.0 as usize)
                {
                    for (f, w) in fields {
                        field_gaps[bi][si].push((false, place.local, *f, *w));
                    }
                }
                // Struct copy `dest = Copy(src)` where `src` is an aggregate:
                // `dest` shares each RC field pointer, so retain them after the
                // copy. Keyed on the SOURCE being an aggregate (not on `dest`
                // being a managed local) so a copy into the return slot — which
                // transfers the value to the caller while the source local is
                // released at this return — keeps the fields alive.
                if let Rvalue::Use(Operand::Copy(src)) = rvalue
                    && src.projection.is_empty()
                    && (src.local.0 as usize) < body.locals.len()
                {
                    for (f, w) in agg_rc_fields(body.locals[src.local.0 as usize].ty) {
                        field_gaps[bi][si + 1].push((true, place.local, f, w));
                    }
                }
            }
        }
        if matches!(block.terminator, Terminator::Return) {
            for (li, fields) in &agg_locals {
                for (f, w) in fields {
                    field_gaps[bi][len].push((
                        false,
                        Local(u32::try_from(*li).unwrap_or(0)),
                        *f,
                        *w,
                    ));
                }
            }
        }
        // A call that reassigns an owned aggregate local (`h = make()`) must
        // release the previous value's RC fields first — the statement-position
        // release above only sees `Assign`, not a call-terminator destination.
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.projection.is_empty()
            && let Some((_, fields)) = agg_locals
                .iter()
                .find(|(l, _)| *l == destination.local.0 as usize)
        {
            for (f, w) in fields {
                field_gaps[bi][len].push((false, destination.local, *f, *w));
            }
        }
    }

    // Pre-allocate one unit-typed local per emitted retain/release call.
    let total_calls: usize = gaps.iter().flatten().map(Vec::len).sum::<usize>()
        + field_gaps.iter().flatten().map(Vec::len).sum::<usize>();
    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut next_unit = body.locals.len();
    for _ in 0..total_calls {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }

    // Rebuild each block: zero-init owners at entry, then interleave the
    // gap insertions with the original statements.
    for bi in 0..n_blocks {
        let span = body.blocks[bi].span;
        let orig: Vec<Statement> = std::mem::take(&mut body.blocks[bi].stmts);
        let block_gaps = std::mem::take(&mut gaps[bi]);
        let block_field_gaps = std::mem::take(&mut field_gaps[bi]);
        let mut new_stmts: Vec<Statement> = Vec::with_capacity(orig.len() + total_calls);
        // Entry block: zero-init releasable owners so every release is
        // null-safe regardless of the path taken to it.
        if bi == 0 {
            for &local in &releasable {
                new_stmts.push(Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(local),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                    span,
                });
            }
            // Zero-init each aggregate local's RC field slots so the
            // release-before-reassignment reads null (a no-op) on the first
            // assignment instead of dereferencing an uninitialised slot.
            for (li, fields) in &agg_locals {
                for (f, _) in fields {
                    new_stmts.push(Statement {
                        kind: StatementKind::Assign {
                            place: Place {
                                local: Local(u32::try_from(*li).unwrap_or(0)),
                                projection: vec![crate::ir::Projection::Field(*f)],
                            },
                            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                        },
                        span,
                    });
                }
            }
        }
        let mut orig_iter = orig.into_iter();
        for g in 0..block_gaps.len() {
            // Emit retains before releases at each gap: a value copied
            // out (e.g. into the return slot) must be retained before the
            // at-return releases of its aliasing locals, or those
            // releases would free it before the caller's reference is
            // minted.
            for pass_retain in [true, false] {
                for &(is_retain, local) in &block_gaps[g] {
                    if is_retain != pass_retain {
                        continue;
                    }
                    // A `Weak<T>` local is weak-counted: route its
                    // retain/release through the weak helpers so the
                    // payload's strong lifetime is unaffected and the
                    // allocation frees only when both counts reach zero.
                    let name = if (local.0 as usize) < body.locals.len() {
                        rc_helper(tcx, body.locals[local.0 as usize].ty, is_retain)
                    } else if is_retain {
                        "gos_rt_rc_retain"
                    } else {
                        "gos_rt_rc_release"
                    };
                    let dest = Local(u32::try_from(next_unit).expect("local overflow"));
                    next_unit += 1;
                    new_stmts.push(rc_call_stmt(name, dest, local, span));
                }
                for &(is_retain, local, fidx, is_weak) in &block_field_gaps[g] {
                    if is_retain != pass_retain {
                        continue;
                    }
                    let name = match (is_retain, is_weak) {
                        (true, false) => "gos_rt_rc_retain",
                        (false, false) => "gos_rt_rc_release",
                        (true, true) => "gos_rt_rc_weak_retain",
                        (false, true) => "gos_rt_rc_weak_release",
                    };
                    let dest = Local(u32::try_from(next_unit).expect("local overflow"));
                    next_unit += 1;
                    new_stmts.push(field_rc_call_stmt(name, dest, local, fidx, span));
                }
            }
            if let Some(stmt) = orig_iter.next() {
                new_stmts.push(stmt);
            }
        }
        body.blocks[bi].stmts = new_stmts;
    }
}

/// Runtime calls that take ownership of an RC-managed argument (it
/// outlives the call), so the argument is a move, not a borrow. Missing
/// one would free a value the container/channel still references; an
/// extra one only leaks. Keep this list complete for RC-managed payloads.
/// Runtime calls that return a freshly ALLOCATED, owned `String` (no aliasing
/// of any argument). The caller owns the result and must release it at scope
/// unless it is moved out. Deliberately EXCLUDES `__concat` /
/// `gos_rt_str_concat*` (handled by the binding `Copy` and prone to in-place
/// aliasing in `s += …`) and `gos_rt_result_payload` (the payload may already be
/// owned by its binding). A missing entry only leaks; a wrong one double-frees.
fn mints_owned_string(name: &str) -> bool {
    matches!(
        name,
        "gos_rt_str_repeat"
            | "gos_rt_str_to_upper"
            | "gos_rt_str_to_lower"
            | "gos_rt_str_to_title"
            | "gos_rt_str_slice"
            | "gos_rt_str_substring"
            | "gos_rt_str_trim"
            | "gos_rt_str_trim_start"
            | "gos_rt_str_trim_end"
            | "gos_rt_str_replace"
            | "gos_rt_str_replacen"
            | "gos_rt_str_pad_left"
            | "gos_rt_str_pad_right"
            | "gos_rt_http_response_content_type"
            | "gos_rt_http_response_location"
    )
}

fn is_consuming_call(name: &str) -> bool {
    name.starts_with("gos_rt_vec_push")
        || name.starts_with("gos_rt_vec_insert")
        || name.starts_with("gos_rt_set_insert")
        || name.starts_with("gos_rt_btmap_insert")
        || name.starts_with("gos_rt_map_insert")
        || name.starts_with("gos_rt_omap_insert")
        || name.starts_with("gos_rt_ovec_insert")
        || name.starts_with("gos_rt_chan_send")
        || name == "gos_rt_go_spawn_closure"
}

/// Picks the retain/release runtime helper for a heap value by its type. Vecs
/// carry no RC header, so they route through the Vec allocator's reference
/// count (`gos_rt_vec_retain` / `gos_rt_vec_free`); `Weak<T>` routes through the
/// weak helpers; everything else (strings, enums, structs) uses the generic
/// `gos_rt_rc_retain` / `gos_rt_rc_release` (which tag-dispatches strings to the
/// string allocator).
fn rc_helper(
    tcx: &gossamer_types::TyCtxt,
    ty: gossamer_types::Ty,
    is_retain: bool,
) -> &'static str {
    use gossamer_types::TyKind;
    match tcx.kind_of(ty) {
        TyKind::Vec(_) | TyKind::Slice(_) => {
            if is_retain {
                "gos_rt_vec_retain"
            } else {
                "gos_rt_vec_free"
            }
        }
        _ if tcx.is_weak_ty(ty) => {
            if is_retain {
                "gos_rt_rc_weak_retain"
            } else {
                "gos_rt_rc_weak_release"
            }
        }
        _ => {
            if is_retain {
                "gos_rt_rc_retain"
            } else {
                "gos_rt_rc_release"
            }
        }
    }
}

/// Builds a `gos_rt_rc_retain` / `gos_rt_rc_release` call on one RC field of a
/// by-value aggregate local (`local.field_idx`).
fn field_rc_call_stmt(
    name: &'static str,
    dest: Local,
    local: Local,
    field_idx: u32,
    span: gossamer_lex::Span,
) -> Statement {
    Statement {
        kind: StatementKind::Assign {
            place: Place::local(dest),
            rvalue: Rvalue::CallIntrinsic {
                name,
                args: vec![Operand::Copy(Place {
                    local,
                    projection: vec![crate::ir::Projection::Field(field_idx)],
                })],
            },
        },
        span,
    }
}

fn rc_call_stmt(
    name: &'static str,
    dest: Local,
    local: Local,
    span: gossamer_lex::Span,
) -> Statement {
    Statement {
        kind: StatementKind::Assign {
            place: Place::local(dest),
            rvalue: Rvalue::CallIntrinsic {
                name,
                args: vec![Operand::Copy(Place::local(local))],
            },
        },
        span,
    }
}

/// Deterministic reclamation for escaped value-aggregate heap copies.
///
/// The LLVM backend heap-copies a multi-slot struct that flows into a
/// `Some(..)`/`Ok(..)`/`Err(..)` payload (`gos_rt_rc_alloc_copy`, an RC
/// blob in the copy-blob provenance set). This pass gives every holder
/// of such a payload pointer exactly one share:
///
/// - an option-typed local (`{disc, payload}` by value) is a holder: it
///   retains after every initialisation except the `gos_rt_result_new`
///   mint itself and call destinations (the callee's return-copy mints
///   the caller's share), and releases before reassignment and at
///   return;
/// - a guarded slot of a stack aggregate is a holder: the aggregate
///   retains its children after every whole-local initialisation
///   (construction operands keep their own shares) and releases them
///   before reassignment, before a call-destination overwrite, and at
///   return;
/// - an option field store (`s.next = o`, directly or through a
///   reference) releases the slot's previous payload and retains the
///   new one in place;
/// - entry blocks zero the guarded slots and option locals so the first
///   release never reads stack garbage.
///
/// Every retain/release the runtime performs is gated on the copy-blob
/// provenance set, so pointers produced by anything other than
/// `gos_rt_rc_alloc_copy` (map gets, borrows, the Cranelift tier's
/// construction-allocated aggregates) are never touched: a missed entry
/// can only leak, never corrupt.
pub(crate) fn insert_aggr_copy_drops(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;
    let n_locals = body.locals.len();
    if n_locals == 0 {
        return;
    }
    let arity = body.arity as usize;

    // A guarded meta symbol with at least one (gate, disc, payload) entry.
    let walk_meta = |ty: gossamer_types::Ty| -> Option<String> {
        let sym = tcx.aggr_copy_meta(ty)?;
        let blob = tcx.rc_meta(sym)?;
        if blob.len() >= 2 && blob[1] > 0 {
            Some(sym.to_string())
        } else {
            None
        }
    };
    let guarded_locals: Vec<(Local, String)> = ((arity + 1)..n_locals)
        .filter(|&i| !body.locals[i].region)
        .filter_map(|i| {
            walk_meta(body.locals[i].ty).map(|sym| (Local(u32::try_from(i).unwrap_or(0)), sym))
        })
        .collect();
    // The return slot participates in retains only: a return-copy mints
    // the caller's share (released by the caller), but the slot itself
    // is never released here.
    let retain_meta_of = |l: Local| -> Option<String> {
        let i = l.0 as usize;
        if i >= n_locals || body.locals[i].region || (1..=arity).contains(&i) {
            return None;
        }
        walk_meta(body.locals[i].ty)
    };

    // By-value Option/Result locals whose payload type registered a
    // copy-blob meta on either side.
    let is_guarded_option = |ty: gossamer_types::Ty| -> bool {
        match tcx.kind_of(ty) {
            TyKind::Adt { def, substs } if def.local == u32::MAX || def.local == u32::MAX - 1 => {
                substs
                    .types()
                    .iter()
                    .take(2)
                    .any(|p| tcx.aggr_copy_meta(*p).is_some())
            }
            _ => false,
        }
    };
    let option_holder = |l: Local| -> bool {
        let i = l.0 as usize;
        i > arity && i < n_locals && !body.locals[i].region && is_guarded_option(body.locals[i].ty)
    };
    // `result_new` destinations whose payload type carries a copy-blob
    // meta are guarded option holders even when the typer left the
    // destination's type unresolved (`Ok(S { .. })` through a `Var`
    // temp): without the classification, the temp's sweep release is
    // never emitted and the payload blob leaves the function one count
    // high — pinned in the collector buffer, one leak per call.
    let mut mint_holders = vec![false; n_locals];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { place, rvalue } = &stmt.kind
                && place.projection.is_empty()
                && (place.local.0 as usize) < n_locals
                && let Rvalue::CallIntrinsic { name, args } = rvalue
                && (*name == "gos_rt_result_new" || *name == "gos_rt_result_new_f64")
                && let Some(Operand::Copy(pp)) = args.get(1)
                && pp.projection.is_empty()
                && (pp.local.0 as usize) < n_locals
                && tcx
                    .aggr_copy_meta(body.locals[pp.local.0 as usize].ty)
                    .is_some()
            {
                mint_holders[place.local.0 as usize] = true;
            }
        }
    }
    let option_holders: Vec<Local> = ((arity + 1)..n_locals)
        .filter(|&i| {
            !body.locals[i].region && (is_guarded_option(body.locals[i].ty) || mint_holders[i])
        })
        .map(|i| Local(u32::try_from(i).unwrap_or(0)))
        .collect();

    // A field store whose base resolves (through references) to a type
    // with a guarded meta, assigning an option-typed value: the slot's
    // old payload is released and the new one retained in place.
    let peel_ref = |mut ty: gossamer_types::Ty| -> gossamer_types::Ty {
        while let TyKind::Ref { inner, .. } = tcx.kind_of(ty) {
            ty = *inner;
        }
        ty
    };
    let is_option_field_store = |place: &Place, rvalue: &Rvalue| -> bool {
        if place.projection.is_empty()
            || !place
                .projection
                .iter()
                .all(|p| matches!(p, crate::ir::Projection::Field(_)))
        {
            return false;
        }
        let i = place.local.0 as usize;
        if i >= n_locals {
            return false;
        }
        if walk_meta(peel_ref(body.locals[i].ty)).is_none() {
            return false;
        }
        match rvalue {
            Rvalue::Use(Operand::Copy(src)) if src.projection.is_empty() => {
                option_holder(src.local) || is_guarded_option(body.locals[src.local.0 as usize].ty)
            }
            Rvalue::Use(_) | Rvalue::CallIntrinsic { .. } => {
                // Conservatively treat any other store into the slot as
                // an option write when the destination field could hold
                // one; release/retain on a non-member payload no-op.
                true
            }
            _ => false,
        }
    };

    let mut gaps: Vec<Vec<Vec<Statement>>> = body
        .blocks
        .iter()
        .map(|b| vec![Vec::new(); b.stmts.len() + 1])
        .collect();
    let mut next_unit = body.locals.len();
    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut extra_locals = 0usize;
    let call_stmt = |name: &'static str,
                     args: Vec<Operand>,
                     span: gossamer_lex::Span,
                     next_unit: &mut usize,
                     extra: &mut usize|
     -> Statement {
        let dest = Local(u32::try_from(*next_unit).expect("local overflow"));
        *next_unit += 1;
        *extra += 1;
        Statement {
            kind: StatementKind::Assign {
                place: Place::local(dest),
                rvalue: Rvalue::CallIntrinsic { name, args },
            },
            span,
        }
    };
    let walk_args = |l: Local, sym: &str| -> Vec<Operand> {
        vec![
            Operand::Copy(Place::local(l)),
            Operand::Const(ConstValue::Str(sym.to_string())),
        ]
    };

    for (bi, block) in body.blocks.iter().enumerate() {
        let len = block.stmts.len();
        let span = block.span;
        for (si, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            if place.projection.is_empty() {
                // Whole-local (re)initialisation of a guarded aggregate:
                // release the previous children, retain the new ones.
                if let Some((_, sym)) = guarded_locals.iter().find(|(l, _)| *l == place.local) {
                    gaps[bi][si].push(call_stmt(
                        "gos_rt_aggr_release_children",
                        walk_args(place.local, sym),
                        span,
                        &mut next_unit,
                        &mut extra_locals,
                    ));
                }
                if let Some(sym) = retain_meta_of(place.local)
                    && !matches!(rvalue, Rvalue::Use(Operand::Const(_)))
                {
                    gaps[bi][si + 1].push(call_stmt(
                        "gos_rt_aggr_retain_children",
                        walk_args(place.local, &sym),
                        span,
                        &mut next_unit,
                        &mut extra_locals,
                    ));
                }
                // Whole-local (re)initialisation of an option holder.
                if option_holder(place.local) || place.local == Local::RETURN {
                    let holder_ty_ok = if place.local == Local::RETURN {
                        is_guarded_option(body.locals[0].ty)
                    } else {
                        true
                    };
                    if holder_ty_ok {
                        if option_holder(place.local) {
                            gaps[bi][si].push(call_stmt(
                                "gos_rt_option_slot_release",
                                vec![Operand::Copy(Place::local(place.local))],
                                span,
                                &mut next_unit,
                                &mut extra_locals,
                            ));
                        }
                        let is_mint = matches!(
                            rvalue,
                            Rvalue::CallIntrinsic { name, .. }
                                if *name == "gos_rt_result_new"
                                    || *name == "gos_rt_result_new_f64"
                        );
                        let is_const = matches!(rvalue, Rvalue::Use(Operand::Const(_)));
                        if !is_mint && !is_const {
                            gaps[bi][si + 1].push(call_stmt(
                                "gos_rt_option_slot_retain",
                                vec![Operand::Copy(Place::local(place.local))],
                                span,
                                &mut next_unit,
                                &mut extra_locals,
                            ));
                        }
                    }
                }
            } else if is_option_field_store(place, rvalue) {
                // Overwriting an owning option slot in place: release the
                // old payload, store, retain the new one.
                gaps[bi][si].push(call_stmt(
                    "gos_rt_option_slot_release",
                    vec![Operand::Copy(place.clone())],
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
                gaps[bi][si + 1].push(call_stmt(
                    "gos_rt_option_slot_retain",
                    vec![Operand::Copy(place.clone())],
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
            }
        }
        // A call destination is minted by the callee: release the old
        // value, never retain the new one.
        if let Terminator::Call { destination, .. } = &block.terminator
            && destination.projection.is_empty()
        {
            if let Some((_, sym)) = guarded_locals.iter().find(|(l, _)| *l == destination.local) {
                gaps[bi][len].push(call_stmt(
                    "gos_rt_aggr_release_children",
                    walk_args(destination.local, sym),
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
            }
            if option_holder(destination.local) {
                gaps[bi][len].push(call_stmt(
                    "gos_rt_option_slot_release",
                    vec![Operand::Copy(Place::local(destination.local))],
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
            }
        }
        if matches!(block.terminator, Terminator::Return) {
            for (l, sym) in &guarded_locals {
                gaps[bi][len].push(call_stmt(
                    "gos_rt_aggr_release_children",
                    walk_args(*l, sym),
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
            }
            for l in &option_holders {
                gaps[bi][len].push(call_stmt(
                    "gos_rt_option_slot_release",
                    vec![Operand::Copy(Place::local(*l))],
                    span,
                    &mut next_unit,
                    &mut extra_locals,
                ));
            }
        }
    }

    if extra_locals == 0 && guarded_locals.is_empty() && option_holders.is_empty() {
        return;
    }

    // Entry-block zeroing: guarded slots via the runtime walk, option
    // holders via a plain zero store (both {disc, payload} words).
    let mut entry_inits: Vec<Statement> = Vec::new();
    if let Some(first) = body.blocks.first() {
        let span = first.span;
        for (l, sym) in &guarded_locals {
            entry_inits.push(call_stmt(
                "gos_rt_aggr_zero_guarded",
                walk_args(*l, sym),
                span,
                &mut next_unit,
                &mut extra_locals,
            ));
        }
        for l in &option_holders {
            entry_inits.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(*l),
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                },
                span,
            });
        }
    }

    for _ in 0..extra_locals {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }

    let n_blocks = body.blocks.len();
    for bi in 0..n_blocks {
        let orig: Vec<Statement> = std::mem::take(&mut body.blocks[bi].stmts);
        let block_gaps = std::mem::take(&mut gaps[bi]);
        let mut new_stmts: Vec<Statement> = Vec::with_capacity(orig.len() + 4);
        if bi == 0 {
            new_stmts.append(&mut entry_inits);
        }
        let mut orig_iter = orig.into_iter();
        for g in 0..block_gaps.len() {
            new_stmts.extend(block_gaps[g].iter().cloned());
            if let Some(stmt) = orig_iter.next() {
                new_stmts.push(stmt);
            }
        }
        body.blocks[bi].stmts = new_stmts;
    }
}

// Slot-child kinds, mirroring `gossamer_runtime::c_abi::vec::vec_elem_kind`
// (kept in sync by value). `RC_NODE` covers user enum / struct heap
// pointers (tag-bit-encoded) released via `gos_rt_rc_release`.
const SLOT_KIND_STRING: i64 = 1;
const SLOT_KIND_VEC: i64 = 2;
const SLOT_KIND_RC_NODE: i64 = 7;

/// Walks the flat slot layout of a by-value aggregate `ty`, appending one
/// `(gate, disc_word, word, kind)` entry per RC child pointer the vec must
/// own. `gate` is `-1` for an unconditional pointer field, or the
/// discriminant value gating an `Option`/`Result` payload word. Sets
/// `has_direct` when an unconditional (non-`Option`/`Result`) RC field is
/// present — the signal that the element needs the `AGGR_OWNED` path
/// rather than the copy-blob-only `AGGR_GUARDED` path. Recurses through
/// nested inline struct / tuple fields at absolute word offsets.
fn collect_slot_rc_children(
    tcx: &gossamer_types::TyCtxt,
    ty: gossamer_types::Ty,
    base_word: i64,
    depth: u32,
    out: &mut Vec<(i64, i64, i64, i64)>,
    has_direct: &mut bool,
) {
    use gossamer_types::TyKind;
    if depth > 8 {
        return;
    }
    let field_tys: Vec<gossamer_types::Ty> = match tcx.kind_of(ty) {
        TyKind::Tuple(elems) => elems.clone(),
        TyKind::Adt { def, .. } => {
            // Opaque stdlib handles / Weak / Option / Result sentinels have
            // their own teardown; never walk their declared field lists here.
            if def.local >= u32::MAX - 16 {
                return;
            }
            match tcx.struct_field_tys(*def) {
                Some(f) => f.to_vec(),
                None => return,
            }
        }
        _ => return,
    };
    let mut word = base_word;
    for fty in field_tys {
        let fwords = i64::from(tcx.slot_bytes(fty).max(8) / 8);
        collect_field_rc(tcx, fty, word, depth, out, has_direct);
        word += fwords;
    }
}

/// Classifies one aggregate field at absolute `word`, appending its RC
/// child entry (or recursing into a nested inline aggregate).
fn collect_field_rc(
    tcx: &gossamer_types::TyCtxt,
    fty: gossamer_types::Ty,
    word: i64,
    depth: u32,
    out: &mut Vec<(i64, i64, i64, i64)>,
    has_direct: &mut bool,
) {
    use gossamer_types::TyKind;
    match tcx.kind_of(fty) {
        TyKind::String => {
            out.push((-1, 0, word, SLOT_KIND_STRING));
            *has_direct = true;
        }
        TyKind::Vec(_) | TyKind::Slice(_) => {
            out.push((-1, 0, word, SLOT_KIND_VEC));
            *has_direct = true;
        }
        // `Option`/`Result`: the payload word holds a heap pointer only on
        // the side(s) whose inner type is heap-managed. Gate each side on
        // its discriminant (0 = Ok/Some, 1 = Err). Copy-blob and enum
        // payloads carry an `RcHeader`, so `gos_rt_rc_release` reclaims
        // them; a bare `String`/`Vec` payload uses its own kind.
        TyKind::Adt { def, substs } if def.local == u32::MAX || def.local == u32::MAX - 1 => {
            let payload_kind = |t: gossamer_types::Ty| -> Option<i64> {
                match tcx.kind_of(t) {
                    TyKind::String => Some(SLOT_KIND_STRING),
                    TyKind::Vec(_) | TyKind::Slice(_) => Some(SLOT_KIND_VEC),
                    TyKind::Adt { .. } | TyKind::Tuple(_)
                        if tcx.is_rc_managed(t) || tcx.slot_bytes(t) > 8 =>
                    {
                        Some(SLOT_KIND_RC_NODE)
                    }
                    _ => None,
                }
            };
            let tys = substs.types();
            if let Some(k) = tys.first().copied().and_then(payload_kind) {
                out.push((0, word, word + 1, k));
            }
            if let Some(k) = tys.get(1).copied().and_then(payload_kind) {
                out.push((1, word, word + 1, k));
            }
        }
        TyKind::Adt { .. } => {
            if tcx.is_rc_managed(fty) {
                // Heap user enum (a single tag-encoded pointer slot).
                out.push((-1, 0, word, SLOT_KIND_RC_NODE));
                *has_direct = true;
            } else {
                // Inline struct / newtype: its fields occupy these slots.
                collect_slot_rc_children(tcx, fty, word, depth + 1, out, has_direct);
            }
        }
        TyKind::Tuple(_) => collect_slot_rc_children(tcx, fty, word, depth + 1, out, has_direct),
        _ => {}
    }
}

/// Registers (idempotently) the `AGGR_OWNED` slot-children meta for vec
/// element type `elem` and returns its symbol, or `None` when the element
/// carries no unconditional RC child pointer (in which case the copy-blob
/// `AGGR_GUARDED` path, if any, applies instead). Blob layout:
/// `[count, (gate, disc_word, word, kind) * count]`.
fn ensure_slot_children_meta(
    tcx: &mut gossamer_types::TyCtxt,
    elem: gossamer_types::Ty,
) -> Option<String> {
    let mut children = Vec::new();
    let mut has_direct = false;
    collect_slot_rc_children(tcx, elem, 0, 0, &mut children, &mut has_direct);
    if !has_direct || children.is_empty() {
        return None;
    }
    let symbol = format!("gos_rc_slotchildren_{}", elem.as_u32());
    let mut blob = Vec::with_capacity(1 + children.len() * 4);
    blob.push(children.len() as i64);
    for (gate, disc_word, word, kind) in &children {
        blob.push(*gate);
        blob.push(*disc_word);
        blob.push(*word);
        blob.push(*kind);
    }
    tcx.register_rc_meta(symbol.clone(), blob);
    Some(symbol)
}

/// Tags vecs whose element type carries a guarded copy-blob meta, right
/// after their construction, so the runtime retains each pushed
/// element's copy-blob children and releases them when the vec dies
/// (`gos_rt_vec_set_elem_meta` -> push/free/clone/slice handling).
/// Type-driven on the construction destination, so it covers literals,
/// `Vec::new`, `with_capacity`, and array->Vec coercions uniformly.
///
/// Elements that carry an unconditional (non-`Option`/`Result`) RC field
/// — a `String`, nested vec, or user enum/struct heap pointer — instead
/// take the `AGGR_OWNED` path (`gos_rt_vec_set_slot_children`): the vec
/// owns those children, retaining them on push and deep-freeing them on
/// free, so a by-value element pushed in and then dropped at its source
/// scope (or returned inside the vec) is reclaimed exactly once.
pub(crate) fn insert_vec_elem_metas(body: &mut Body, tcx: &mut gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;
    let n_locals = body.locals.len();
    let is_vec_ctor = |name: &str| -> bool {
        matches!(
            name,
            "Vec::new"
                | "gos_rt_vec_new"
                | "gos_rt_vec_new_typed"
                | "gos_rt_vec_with_capacity"
                | "gos_rt_vec_with_capacity_typed"
                | "gos_rt_vec_from_arr"
                | "gos_rt_nested_arr_to_vec"
        )
    };
    let is_map_ctor = |name: &str| -> bool {
        matches!(
            name,
            "HashMap::new" | "gos_rt_map_new" | "gos_rt_map_new_with_capacity"
        )
    };
    let elem_ty_of = |l: Local, tcx: &gossamer_types::TyCtxt| -> Option<gossamer_types::Ty> {
        let i = l.0 as usize;
        if i >= n_locals {
            return None;
        }
        match tcx.kind_of(body.locals[i].ty) {
            TyKind::Vec(e) | TyKind::Slice(e) => Some(*e),
            _ => None,
        }
    };

    // Register the AGGR_OWNED slot-children meta for every vec-ctor whose
    // element carries an unconditional RC field. Done first, while `tcx`
    // can be borrowed mutably, before the immutable detection closures.
    let mut owned_meta: std::collections::HashMap<u32, String> = std::collections::HashMap::new();
    {
        let mut ctor_dests: Vec<Local> = Vec::new();
        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign {
                    place,
                    rvalue: Rvalue::CallIntrinsic { name, .. },
                } = &stmt.kind
                    && place.projection.is_empty()
                    && is_vec_ctor(name)
                {
                    ctor_dests.push(place.local);
                }
            }
            if let Terminator::Call {
                callee: Operand::Const(ConstValue::Str(name)),
                destination,
                ..
            } = &block.terminator
                && destination.projection.is_empty()
                && is_vec_ctor(name)
            {
                ctor_dests.push(destination.local);
            }
        }
        for l in ctor_dests {
            if owned_meta.contains_key(&l.0) {
                continue;
            }
            if let Some(elem) = elem_ty_of(l, tcx)
                && let Some(sym) = ensure_slot_children_meta(tcx, elem)
            {
                owned_meta.insert(l.0, sym);
            }
        }
    }

    // Guarded copy-blob meta of a vec element — but only when the element
    // did NOT take the owned path (the owned layout already covers every
    // RC child, including `Option`/`Result` payloads).
    let elem_meta_of = |l: Local| -> Option<String> {
        if owned_meta.contains_key(&l.0) {
            return None;
        }
        let elem = elem_ty_of(l, tcx)?;
        let sym = tcx.aggr_copy_meta(elem)?;
        let blob = tcx.rc_meta(sym)?;
        if blob.len() >= 2 && blob[1] > 0 {
            Some(sym.to_string())
        } else {
            None
        }
    };
    let map_value_blob = |l: Local| -> bool {
        let i = l.0 as usize;
        if i >= n_locals {
            return false;
        }
        let TyKind::HashMap { value, .. } = tcx.kind_of(body.locals[i].ty) else {
            return false;
        };
        tcx.aggr_copy_meta(*value).is_some()
    };

    // The teardown call to schedule for one vec/map construction.
    enum VecMeta {
        Guarded(String),
        Owned(String),
        MapBlob,
    }
    let vec_meta_of = |l: Local| -> Option<VecMeta> {
        if let Some(sym) = owned_meta.get(&l.0) {
            return Some(VecMeta::Owned(sym.clone()));
        }
        elem_meta_of(l).map(VecMeta::Guarded)
    };

    // (block, stmt-gap, dest local, meta) for statement ctors; block-head
    // inserts at the call target for terminator ctors.
    let mut stmt_inserts: Vec<(usize, usize, Local, VecMeta)> = Vec::new();
    let mut head_inserts: Vec<(usize, Local, VecMeta)> = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            if let StatementKind::Assign {
                place,
                rvalue: Rvalue::CallIntrinsic { name, .. },
            } = &stmt.kind
                && place.projection.is_empty()
            {
                if is_vec_ctor(name)
                    && let Some(meta) = vec_meta_of(place.local)
                {
                    stmt_inserts.push((bi, si + 1, place.local, meta));
                }
                if is_map_ctor(name) && map_value_blob(place.local) {
                    stmt_inserts.push((bi, si + 1, place.local, VecMeta::MapBlob));
                }
            }
        }
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            destination,
            target: Some(t),
            ..
        } = &block.terminator
            && destination.projection.is_empty()
        {
            if is_vec_ctor(name)
                && let Some(meta) = vec_meta_of(destination.local)
            {
                head_inserts.push((t.0 as usize, destination.local, meta));
            }
            if is_map_ctor(name) && map_value_blob(destination.local) {
                head_inserts.push((t.0 as usize, destination.local, VecMeta::MapBlob));
            }
        }
    }
    if stmt_inserts.is_empty() && head_inserts.is_empty() {
        return;
    }

    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut next_unit = body.locals.len();
    let mk =
        |l: Local, meta: &VecMeta, span: gossamer_lex::Span, next_unit: &mut usize| -> Statement {
            let dest = Local(u32::try_from(*next_unit).expect("local overflow"));
            *next_unit += 1;
            let rvalue = match meta {
                VecMeta::MapBlob => Rvalue::CallIntrinsic {
                    name: "gos_rt_map_set_blob_values",
                    args: vec![Operand::Copy(Place::local(l))],
                },
                VecMeta::Guarded(sym) => Rvalue::CallIntrinsic {
                    name: "gos_rt_vec_set_elem_meta",
                    args: vec![
                        Operand::Copy(Place::local(l)),
                        Operand::Const(ConstValue::Str(sym.clone())),
                    ],
                },
                VecMeta::Owned(sym) => Rvalue::CallIntrinsic {
                    name: "gos_rt_vec_set_slot_children",
                    args: vec![
                        Operand::Copy(Place::local(l)),
                        Operand::Const(ConstValue::Str(sym.clone())),
                    ],
                },
            };
            Statement {
                kind: StatementKind::Assign {
                    place: Place::local(dest),
                    rvalue,
                },
                span,
            }
        };

    for (bi, l, meta) in &head_inserts {
        let span = body.blocks[*bi].span;
        let stmt = mk(*l, meta, span, &mut next_unit);
        body.blocks[*bi].stmts.insert(0, stmt);
        // Shift any statement-gap inserts in the same block.
        for ins in &mut stmt_inserts {
            if ins.0 == *bi {
                ins.1 += 1;
            }
        }
    }
    // Insert in descending gap order so earlier indices stay valid.
    let mut by_block: Vec<(usize, usize, Local, VecMeta)> = stmt_inserts;
    by_block.sort_by_key(|ins| std::cmp::Reverse((ins.0, ins.1)));
    for (bi, gap, l, meta) in by_block {
        let span = body.blocks[bi].span;
        let stmt = mk(l, &meta, span, &mut next_unit);
        body.blocks[bi].stmts.insert(gap, stmt);
    }
    for _ in body.locals.len()..next_unit {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }
}

/// Releases owned heap values at their last use instead of at function
/// return, so peak RSS tracks the live set rather than the frame's
/// lifetime (a function that builds a large tree, prints a summary, and
/// then loops for seconds was holding the tree the whole time).
///
/// The pass piggybacks on the ownership judgments the earlier passes
/// already encoded in the IR: a local is a candidate exactly when a
/// return block carries a release for it (`gos_rt_rc_release` /
/// `gos_rt_rc_weak_release` from `insert_rc_releases`,
/// `gos_rt_aggr_release_children` / `gos_rt_option_slot_release` from
/// `insert_aggr_copy_drops`). For each candidate it finds the blocks
/// from whose exit no further *real* mention of the local is reachable
/// (accounting intrinsics and constant stores don't count), inserts the
/// matching release right after the last mention — or at the head of
/// each successor when the last mention is the terminator — and nulls
/// the local out. The return-block releases stay in place as a null-safe
/// backstop, so a path this analysis misses leaks nothing and a path it
/// covers cannot double-release.
///
/// Locals that appear in an `Rvalue::Ref` are pinned (released at
/// return only): the borrow's pointer value could outlive the last
/// direct mention.
/// One pending early release: insert after statement `usize` for `Local`,
/// via the named release intrinsic with an optional meta symbol.
type PendingRelease = (usize, Local, &'static str, Option<String>);

pub(crate) fn insert_early_releases(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    // Locals whose by-value Result/Option payload is extracted with
    // `gos_rt_result_payload` anywhere in the body — their slot
    // releases must stay at the return sweep (see the candidate match
    // below).
    let extracted_from: std::collections::HashSet<u32> = body
        .blocks
        .iter()
        .flat_map(|b| b.stmts.iter())
        .filter_map(|stmt| {
            let StatementKind::Assign { rvalue, .. } = &stmt.kind else {
                return None;
            };
            let Rvalue::CallIntrinsic { name, args } = rvalue else {
                return None;
            };
            if *name != "gos_rt_result_payload" && *name != "gos_rt_result_payload_f64" {
                return None;
            }
            match args.first() {
                Some(Operand::Copy(p)) if p.projection.is_empty() => Some(p.local.0),
                _ => None,
            }
        })
        .collect();

    let n_locals = body.locals.len();
    let n_blocks = body.blocks.len();
    if n_locals == 0 || n_blocks == 0 {
        return;
    }

    // RELEASE-side accounting only. A retain READS its argument (it
    // hands a fresh share to a holder that was just initialised from
    // this local), so retains MUST count as mentions: inserting the
    // early release+null between a store and its follow-up retain made
    // the retain see null — the new holder never got its share and the
    // node freed while still referenced.
    let accounting = |name: &str| -> bool {
        matches!(
            name,
            "gos_rt_rc_release"
                | "gos_rt_rc_weak_release"
                | "gos_rt_aggr_release_children"
                | "gos_rt_aggr_zero_guarded"
                | "gos_rt_option_slot_release"
        )
    };

    // Candidates: (local, release-intrinsic, optional meta symbol),
    // harvested from release calls sitting in Return blocks.
    let mut candidates: Vec<(Local, &'static str, Option<String>)> = Vec::new();
    let mut seen: std::collections::HashSet<u32> = std::collections::HashSet::new();
    for block in &body.blocks {
        if !matches!(block.terminator, Terminator::Return) {
            continue;
        }
        for stmt in &block.stmts {
            let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } = &stmt.kind
            else {
                continue;
            };
            let Some(Operand::Copy(p)) = args.first() else {
                continue;
            };
            if !p.projection.is_empty() {
                continue;
            }
            let release: &'static str = match *name {
                "gos_rt_rc_release" => "gos_rt_rc_release",
                "gos_rt_rc_weak_release" => "gos_rt_rc_weak_release",
                "gos_rt_aggr_release_children" => "gos_rt_aggr_release_children",
                // Early-relocating an option-slot release is unsound
                // when the result's payload is EXTRACTED somewhere in
                // the body: the extraction BORROWS the payload blob's
                // children (shared field pointers, no retains), and
                // that borrow's lifetime is invisible to the mention
                // analysis — the relocated release (typically right at
                // the extraction) frees the blob under the borrower.
                // Results that are never extracted-from keep early
                // placement (Option-chain workloads rely on it to keep
                // RAM flat).
                "gos_rt_option_slot_release" if !extracted_from.contains(&p.local.0) => {
                    "gos_rt_option_slot_release"
                }
                _ => continue,
            };
            if !seen.insert(p.local.0) {
                continue;
            }
            let meta = if release == "gos_rt_aggr_release_children" {
                match args.get(1) {
                    Some(Operand::Const(ConstValue::Str(sym))) => Some(sym.clone()),
                    _ => continue,
                }
            } else {
                None
            };
            candidates.push((p.local, release, meta));
        }
    }
    if candidates.is_empty() {
        return;
    }

    // Weak references make drop timing observable: a `Weak` created from
    // a local in this frame must keep observing it alive until the frame
    // ends, exactly as the VM does. When the body creates any weak
    // reference, the RC locals keep their at-return placement; guarded
    // aggregates and option holders cannot be downgraded and stay
    // eligible.
    let has_downgrade = body.blocks.iter().any(|b| {
        b.stmts.iter().any(|st| {
            matches!(
                &st.kind,
                StatementKind::Assign {
                    rvalue: Rvalue::CallIntrinsic { name, .. },
                    ..
                } if *name == "gos_rt_rc_downgrade"
            )
        }) || matches!(
            &b.terminator,
            Terminator::Call {
                callee: Operand::Const(ConstValue::Str(n)),
                ..
            } if n == "gos_rt_rc_downgrade" || n == "downgrade"
        )
    });
    if has_downgrade {
        candidates.retain(|(_, release, _)| {
            *release != "gos_rt_rc_release" && *release != "gos_rt_rc_weak_release"
        });
        if candidates.is_empty() {
            return;
        }
    }

    // Real mentions per block, and the Ref pin. A mention is any
    // appearance of the bare local in a non-accounting statement or in
    // a terminator. Constant stores (the zero-inits) don't count.
    let mut pinned: Vec<bool> = vec![false; n_locals];
    let mut mention_stmt: Vec<Vec<Option<usize>>> = vec![vec![None; n_locals]; n_blocks];
    let mut mention_term: Vec<Vec<bool>> = vec![vec![false; n_locals]; n_blocks];
    {
        let mark = |l: Local,
                    bi: usize,
                    si: Option<usize>,
                    mention_stmt: &mut Vec<Vec<Option<usize>>>,
                    mention_term: &mut Vec<Vec<bool>>| {
            let i = l.0 as usize;
            if i >= n_locals {
                return;
            }
            match si {
                Some(si) => mention_stmt[bi][i] = Some(si),
                None => mention_term[bi][i] = true,
            }
        };
        let locals_in_operand = |op: &Operand, out: &mut Vec<Local>| {
            if let Operand::Copy(p) = op {
                out.push(p.local);
            }
        };
        for (bi, block) in body.blocks.iter().enumerate() {
            for (si, stmt) in block.stmts.iter().enumerate() {
                let (place, rvalue) = match &stmt.kind {
                    StatementKind::Assign { place, rvalue } => (place, rvalue),
                    StatementKind::StorageLive(_)
                    | StatementKind::StorageDead(_)
                    | StatementKind::Nop => {
                        // Storage markers / no-ops, not value uses.
                        continue;
                    }
                    StatementKind::SetDiscriminant { place, .. } => {
                        mark(
                            place.local,
                            bi,
                            Some(si),
                            &mut mention_stmt,
                            &mut mention_term,
                        );
                        continue;
                    }
                    StatementKind::StaticStore { value, .. } => {
                        // The stored value is used here; mark its local.
                        let mut ls: Vec<Local> = Vec::new();
                        locals_in_operand(value, &mut ls);
                        for l in ls {
                            mark(l, bi, Some(si), &mut mention_stmt, &mut mention_term);
                        }
                        continue;
                    }
                };
                let mut ls: Vec<Local> = Vec::new();
                match rvalue {
                    Rvalue::CallIntrinsic { name, args } if accounting(name) => {
                        // Accounting calls are not program uses.
                        let _ = args;
                    }
                    Rvalue::Use(Operand::Const(_)) => {
                        // Constant (re)initialisation — the zero-init
                        // pattern; not a use of the heap value.
                    }
                    Rvalue::Ref { place: rp, .. } => {
                        pinned[rp.local.0 as usize] = true;
                        ls.push(rp.local);
                        ls.push(place.local);
                    }
                    Rvalue::Use(op) => {
                        locals_in_operand(op, &mut ls);
                        ls.push(place.local);
                    }
                    Rvalue::BinaryOp { lhs, rhs, .. } => {
                        locals_in_operand(lhs, &mut ls);
                        locals_in_operand(rhs, &mut ls);
                        ls.push(place.local);
                    }
                    Rvalue::UnaryOp { operand, .. } => {
                        locals_in_operand(operand, &mut ls);
                        ls.push(place.local);
                    }
                    Rvalue::CallIntrinsic { args, .. } => {
                        for a in args {
                            locals_in_operand(a, &mut ls);
                        }
                        ls.push(place.local);
                    }
                    Rvalue::Aggregate { operands, .. } => {
                        // An aggregate literal (fixed array, tuple) copies
                        // heap POINTERS out of its operands without
                        // retaining them — the aggregate borrows the
                        // operand locals' shares. Releasing an operand at
                        // its last textual mention would free a node the
                        // aggregate still references, so pin operands to
                        // the return-site release.
                        for a in operands {
                            locals_in_operand(a, &mut ls);
                            if let Operand::Copy(p) = a {
                                pinned[p.local.0 as usize] = true;
                            }
                        }
                        ls.push(place.local);
                    }
                    Rvalue::Repeat { value, .. } => {
                        locals_in_operand(value, &mut ls);
                        ls.push(place.local);
                    }
                    _ => {
                        // Unmodelled rvalue shapes: pin everything they
                        // could mention by pinning the destination and
                        // bailing on precision for this statement.
                        ls.push(place.local);
                    }
                }
                for l in ls {
                    mark(l, bi, Some(si), &mut mention_stmt, &mut mention_term);
                }
            }
            let mut ls: Vec<Local> = Vec::new();
            match &block.terminator {
                Terminator::Call {
                    args, destination, ..
                } => {
                    for a in args {
                        locals_in_operand(a, &mut ls);
                    }
                    ls.push(destination.local);
                }
                Terminator::SwitchInt { discriminant, .. } => {
                    locals_in_operand(discriminant, &mut ls);
                }
                Terminator::Assert { cond, .. } => {
                    locals_in_operand(cond, &mut ls);
                }
                Terminator::Drop { place, .. } => {
                    ls.push(place.local);
                }
                _ => {}
            }
            for l in ls {
                mark(l, bi, None, &mut mention_stmt, &mut mention_term);
            }
        }
    }

    // Successor map.
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| match &b.terminator {
            Terminator::Goto { target } => vec![target.0 as usize],
            Terminator::SwitchInt { arms, default, .. } => {
                let mut v: Vec<usize> = arms.iter().map(|(_, t)| t.0 as usize).collect();
                v.push(default.0 as usize);
                v
            }
            Terminator::Call { target, .. } => target.iter().map(|t| t.0 as usize).collect(),
            Terminator::Assert { target, .. } => vec![target.0 as usize],
            Terminator::Drop { target, .. } => vec![target.0 as usize],
            _ => Vec::new(),
        })
        .collect();

    // Per candidate: blocks from whose EXIT a mention is reachable.
    // Fixpoint over the reversed edges.
    let mut inserts_after_stmt: Vec<Vec<PendingRelease>> = vec![Vec::new(); n_blocks];
    let mut inserts_at_head: Vec<Vec<(Local, &'static str, Option<String>)>> =
        vec![Vec::new(); n_blocks];
    for (l, release, meta) in &candidates {
        let li = l.0 as usize;
        if li >= n_locals || pinned[li] {
            continue;
        }
        let mentions: Vec<bool> = (0..n_blocks)
            .map(|bi| mention_stmt[bi][li].is_some() || mention_term[bi][li])
            .collect();
        let mut reach: Vec<bool> = vec![false; n_blocks];
        let mut changed = true;
        while changed {
            changed = false;
            for bi in 0..n_blocks {
                if reach[bi] {
                    continue;
                }
                let r = succs[bi].iter().any(|&s| mentions[s] || reach[s]);
                if r {
                    reach[bi] = true;
                    changed = true;
                }
            }
        }
        for bi in 0..n_blocks {
            if !mentions[bi] || reach[bi] {
                continue;
            }
            if matches!(body.blocks[bi].terminator, Terminator::Return) {
                // The backstop already covers this block.
                continue;
            }
            if mention_term[bi][li] {
                for &s in &succs[bi] {
                    inserts_at_head[s].push((*l, release, meta.clone()));
                }
            } else if let Some(si) = mention_stmt[bi][li] {
                inserts_after_stmt[bi].push((si, *l, release, meta.clone()));
            }
        }
    }

    let total: usize = inserts_after_stmt.iter().map(Vec::len).sum::<usize>()
        + inserts_at_head.iter().map(Vec::len).sum::<usize>();
    if total == 0 {
        return;
    }

    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut next_unit = body.locals.len();
    let release_stmts = |l: Local,
                         release: &'static str,
                         meta: &Option<String>,
                         span: gossamer_lex::Span,
                         next_unit: &mut usize|
     -> Vec<Statement> {
        let dest = Local(u32::try_from(*next_unit).expect("local overflow"));
        *next_unit += 1;
        let mut args = vec![Operand::Copy(Place::local(l))];
        if let Some(sym) = meta {
            args.push(Operand::Const(ConstValue::Str(sym.clone())));
        }
        let mut v = vec![Statement {
            kind: StatementKind::Assign {
                place: Place::local(dest),
                rvalue: Rvalue::CallIntrinsic {
                    name: release,
                    args,
                },
            },
            span,
        }];
        // Null out so the at-return backstop (and any
        // release-before-reassign) reads an empty value. Guarded
        // aggregates zero their option slots through the meta walk;
        // scalar holders zero the whole slot.
        if release == "gos_rt_aggr_release_children" {
            let dest2 = Local(u32::try_from(*next_unit).expect("local overflow"));
            *next_unit += 1;
            v.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(dest2),
                    rvalue: Rvalue::CallIntrinsic {
                        name: "gos_rt_aggr_zero_guarded",
                        args: vec![
                            Operand::Copy(Place::local(l)),
                            Operand::Const(ConstValue::Str(meta.clone().unwrap_or_default())),
                        ],
                    },
                },
                span,
            });
        } else {
            v.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(l),
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                },
                span,
            });
        }
        v
    };

    let mut new_unit_locals = 0usize;
    for bi in 0..n_blocks {
        let head = std::mem::take(&mut inserts_at_head[bi]);
        let mut after = std::mem::take(&mut inserts_after_stmt[bi]);
        if head.is_empty() && after.is_empty() {
            continue;
        }
        after.sort_by_key(|(si, ..)| *si);
        let span = body.blocks[bi].span;
        let orig: Vec<Statement> = std::mem::take(&mut body.blocks[bi].stmts);
        let mut new_stmts: Vec<Statement> =
            Vec::with_capacity(orig.len() + 2 * (head.len() + after.len()));
        for (l, release, meta) in &head {
            let before = next_unit;
            new_stmts.extend(release_stmts(*l, release, meta, span, &mut next_unit));
            new_unit_locals += next_unit - before;
        }
        for (si, stmt) in orig.into_iter().enumerate() {
            new_stmts.push(stmt);
            for (asi, l, release, meta) in &after {
                if *asi == si {
                    let before = next_unit;
                    new_stmts.extend(release_stmts(*l, release, meta, span, &mut next_unit));
                    new_unit_locals += next_unit - before;
                }
            }
        }
        body.blocks[bi].stmts = new_stmts;
    }
    for _ in 0..new_unit_locals {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }
}

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
            "gos_rt_set_new"
            | "gos_rt_set_union"
            | "gos_rt_set_intersection"
            | "gos_rt_set_difference"
            | "gos_rt_set_symmetric_difference" => Some("gos_rt_set_free"),
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
                let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                    continue;
                };
                if let Rvalue::CallIntrinsic { name, args } = rvalue {
                    // `gos_store(obj, off, val)`: storing `val` into heap
                    // object `obj`. When `obj` escapes into the return
                    // value (a recursive-enum payload, e.g.
                    // `J::Arr(v)` stored as `gos_store(arr, 8, v)` then
                    // `return arr`), `val` escapes with it. Freeing `val`
                    // here would dangle the returned object's child
                    // pointer — exactly the `Vec`-in-enum crash.
                    if *name == "gos_store"
                        && let Some(Operand::Copy(obj_p)) = args.first()
                        && obj_p.projection.is_empty()
                    {
                        let obj_idx = obj_p.local.0 as usize;
                        if obj_idx < moved_into_return.len() && moved_into_return[obj_idx] {
                            if let Some(val) = args.get(2) {
                                propagate_call_args(
                                    std::slice::from_ref(val),
                                    &mut moved_into_return,
                                    &mut changed,
                                );
                            }
                        }
                        continue;
                    }
                }
                if place.projection.is_empty()
                    && let Rvalue::CallIntrinsic { args, .. } = rvalue
                {
                    let dest_idx = place.local.0 as usize;
                    if dest_idx >= moved_into_return.len() || !moved_into_return[dest_idx] {
                        continue;
                    }
                    propagate_call_args(args, &mut moved_into_return, &mut changed);
                }
            }
            // `gos_rt_vec_push(container, elem)`: the element's heap
            // ownership moves into the container, which deep-frees its
            // elements on drop or carries them to the caller when
            // returned — either way an independent drop here would
            // double-free / dangle. Mark unconditionally. Done inside the
            // fixpoint (not a separate pass) so a pushed enum's own
            // escaped children — `inner` in `outer.push(J::Arr(inner))`,
            // reached via the `gos_store` rule above — propagate through
            // arbitrarily deep nesting.
            if let Terminator::Call { callee, args, .. } = &block.terminator
                && let Operand::Const(ConstValue::Str(name)) = callee
                && name == "gos_rt_vec_push"
                && let Some(Operand::Copy(p)) = args.get(1)
                && p.projection.is_empty()
            {
                let idx = p.local.0 as usize;
                if idx < moved_into_return.len() && !moved_into_return[idx] {
                    moved_into_return[idx] = true;
                    changed = true;
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

    // (`gos_rt_vec_push` element-ownership transfer is handled inside
    // the fixpoint above so it composes with the `gos_store` rule for
    // arbitrarily deep enum/container nesting.)

    // Pass 3: collect drop targets in stable local-index order.
    // The constructor-name → free-name table already restricts
    // candidates to runtime container shapes; we trust the MIR's
    // type assignment and skip a redundant TyKind check here.
    let _ = TyKind::Bool; // silence unused-import lint outside the closure
    let drop_targets_all: Vec<(Local, &'static str)> = (0..owner_ctor.len())
        .filter_map(|i| {
            let free = owner_ctor[i]?;
            if moved_into_return[i] {
                return None;
            }
            Some((Local(i as u32), free))
        })
        .collect();

    // Non-aliased Vec/Map ctor locals get full per-site management below
    // (zero-init + drop-before-overwrite + at-return, all null-safe) so a
    // container rebuilt each loop iteration frees every prior allocation
    // instead of leaking all but the last. Aliased locals (the source of a
    // bare `Copy`) are left to the conservative return-only path — freeing one
    // before its reassignment could dangle the alias. Locals captured by a
    // call were already disqualified from `owner_ctor` in pass 1.
    // Conservative aliasing: a local that is the source of a bare `Copy`, or a
    // value element (arg1..) of a consuming container/channel/closure call, may
    // outlive this frame, so the per-iteration reuse free must not reclaim it.
    let mut aliased = vec![false; body.locals.len()];
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign {
                rvalue: Rvalue::Use(Operand::Copy(p)),
                ..
            } = &stmt.kind
                && p.projection.is_empty()
                && (p.local.0 as usize) < aliased.len()
            {
                aliased[p.local.0 as usize] = true;
            }
        }
        if let Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name)),
            args,
            ..
        } = &block.terminator
            && is_consuming_call(name)
        {
            for arg in args.iter().skip(1) {
                if let Operand::Copy(p) = arg
                    && p.projection.is_empty()
                    && (p.local.0 as usize) < aliased.len()
                {
                    aliased[p.local.0 as usize] = true;
                }
            }
        }
    }
    let reuse: Vec<(Local, &'static str)> = drop_targets_all
        .iter()
        .filter(|(l, free)| {
            !aliased[l.0 as usize] && matches!(*free, "gos_rt_vec_free" | "gos_rt_map_free")
        })
        .copied()
        .collect();
    let reuse_set: std::collections::BTreeSet<u32> = reuse.iter().map(|(l, _)| l.0).collect();
    let drop_targets: Vec<(Local, &'static str)> = drop_targets_all
        .into_iter()
        .filter(|(l, _)| !reuse_set.contains(&l.0))
        .collect();

    if drop_targets.is_empty() && reuse.is_empty() {
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
            let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
            body.locals.push(LocalDecl {
                ty: unit_ty,
                debug_name: None,
                mutable: false,
                region: false,
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
        let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
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

    // Dedicated lifetime for non-aliased Vec/Map ctor locals: zero-init at
    // entry (null), free the previous value before each ctor-Call that
    // reassigns the local (loop reuse), and free the final value at every
    // Return. Every free is null-safe (`gos_rt_vec_free` / `gos_rt_map_free`
    // no-op on null), so this needs no path-sensitive must-init proof and never
    // double-frees: the drop-before frees prior allocations, the at-Return
    // frees the last one, and a never-constructed local stays null.
    if !reuse.is_empty() {
        let span0 = body.blocks[0].span;
        for (local, _) in reuse.iter().rev() {
            body.blocks[0].stmts.insert(
                0,
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(*local),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                    span: span0,
                },
            );
        }
        let free_of: std::collections::BTreeMap<u32, &'static str> =
            reuse.iter().map(|(l, f)| (l.0, *f)).collect();
        // (block_idx, free_name, local) — each appended to the block's stmts,
        // i.e. just before its terminator.
        let mut sites: Vec<(usize, &'static str, Local)> = Vec::new();
        for (block_idx, block) in body.blocks.iter().enumerate() {
            match &block.terminator {
                Terminator::Call { destination, .. } if destination.projection.is_empty() => {
                    if let Some(&free_name) = free_of.get(&destination.local.0) {
                        sites.push((block_idx, free_name, destination.local));
                    }
                }
                Terminator::Return => {
                    for (local, free_name) in &reuse {
                        sites.push((block_idx, *free_name, *local));
                    }
                }
                _ => {}
            }
        }
        let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
        for (block_idx, free_name, local) in sites {
            let dest = Local(u32::try_from(body.locals.len()).expect("local overflow"));
            body.locals.push(LocalDecl {
                ty: unit_ty,
                debug_name: None,
                mutable: false,
                region: false,
            });
            let span = body.blocks[block_idx].span;
            body.blocks[block_idx].stmts.push(Statement {
                kind: StatementKind::Assign {
                    place: Place::local(dest),
                    rvalue: Rvalue::CallIntrinsic {
                        name: free_name,
                        args: vec![Operand::Copy(Place::local(local))],
                    },
                },
                span,
            });
        }
    }
}

/// Rewrites `gos_rt_str_concat` calls to the consuming variant when the MIR
/// emits the copy-back pattern: `tmp = str_concat(out, frag); out = Copy(tmp)`.
///
/// The Gossamer MIR builder lowers `out += frag` as two instructions across
/// two basic blocks:
///
/// ```text
/// bb_n:  Call { gos_rt_str_concat, [Copy(out), Copy(frag)] → tmp, target: bb_succ }
/// bb_succ: Assign { out ← Use(Copy(tmp)) }; …
/// ```
///
/// After the copy-back, the OLD value of `out` is unreachable. Without the consuming
/// variant, that allocation leaks on every loop iteration, producing O(n²) total
/// allocations for an accumulation loop over n elements.
///
/// `gos_rt_str_concat_drop_a(out, frag)` reads both args, allocates the result,
/// then frees `out` — safe because the free happens after the read. It no-ops
/// silently on null and rodata/literal `out` values.
pub(crate) fn rewrite_str_concat_consuming(body: &mut Body) {
    let n_blocks = body.blocks.len();
    // Collect rename targets: (block_idx) where the Call should be renamed.
    let mut targets: Vec<usize> = Vec::new();
    for block_idx in 0..n_blocks {
        let Terminator::Call {
            callee,
            args,
            destination,
            target,
        } = &body.blocks[block_idx].terminator
        else {
            continue;
        };
        // Must be a str_concat call.
        let Operand::Const(ConstValue::Str(name)) = callee else {
            continue;
        };
        if name != "gos_rt_str_concat" {
            continue;
        }
        // Destination must be a bare local (no projection).
        if !destination.projection.is_empty() {
            continue;
        }
        let tmp_local = destination.local;
        // First arg must be a bare Copy of some local `src`.
        let Some(Operand::Copy(src_place)) = args.first() else {
            continue;
        };
        if !src_place.projection.is_empty() {
            continue;
        }
        let src_local = src_place.local;
        // If first-arg == destination (no copy-back needed), rename directly.
        if src_local == tmp_local {
            targets.push(block_idx);
            continue;
        }
        // Otherwise: check that the successor block's FIRST statement copies
        // `tmp` back into `src` — the copy-back pattern.
        let Some(succ_id) = target else { continue };
        let succ_idx = succ_id.0 as usize;
        if succ_idx >= n_blocks {
            continue;
        }
        let first_stmt = body.blocks[succ_idx].stmts.first();
        let is_copy_back = matches!(
            first_stmt,
            Some(Statement {
                kind: StatementKind::Assign {
                    place,
                    rvalue: Rvalue::Use(Operand::Copy(src_of_copy)),
                },
                ..
            }) if place.local == src_local
                && place.projection.is_empty()
                && src_of_copy.local == tmp_local
                && src_of_copy.projection.is_empty()
        );
        if is_copy_back {
            targets.push(block_idx);
        }
    }
    // Apply the renames.
    for block_idx in targets {
        if let Terminator::Call { callee, .. } = &mut body.blocks[block_idx].terminator {
            *callee = Operand::Const(ConstValue::Str("gos_rt_str_concat_drop_a".to_string()));
        }
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

/// Hoists loop-carried release-before-reassign pairs to the value's
/// last mention in the previous iteration.
///
/// `insert_rc_releases` anchors the release of a reassigned local's
/// OLD value to the reassignment itself. In the ubiquitous loop shape
///
/// ```text
/// loop { tree = build(d); use(&tree) }
/// ```
///
/// the reassignment sits AFTER the next value has been built, so the
/// old and new structures coexist — for binary-trees-style workloads
/// that doubles transient RSS. This pass walks back from each
/// `release(x); x = Copy(tmp)` pair through the unique-predecessor
/// chain to x's last mention, and inserts `release(x); x = null`
/// right after it. The original release stays as a null-safe
/// backstop (releasing null is a no-op), so a missed hoist can only
/// keep the old timing — never double-free.
pub(crate) fn hoist_loop_carried_releases(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    let n_locals = body.locals.len();
    let n_blocks = body.blocks.len();
    if n_blocks == 0 {
        return;
    }
    let is_rc = |l: Local| -> bool {
        let i = l.0 as usize;
        i < n_locals && tcx.is_rc_managed(body.locals[i].ty) && !body.locals[i].region
    };
    // Predecessor map (multi-pred blocks stop the backward walk).
    let mut preds: Vec<Vec<usize>> = vec![Vec::new(); n_blocks];
    for (bi, block) in body.blocks.iter().enumerate() {
        let mut add = |t: &BlockId| preds[t.0 as usize].push(bi);
        match &block.terminator {
            Terminator::Goto { target } => add(target),
            Terminator::SwitchInt { arms, default, .. } => {
                for (_, t) in arms {
                    add(t);
                }
                add(default);
            }
            Terminator::Call {
                target: Some(t), ..
            } => add(t),
            Terminator::Assert { target, .. } | Terminator::Drop { target, .. } => add(target),
            _ => {}
        }
    }
    // Successor map for the forward-liveness safety check below.
    let succs: Vec<Vec<usize>> = body
        .blocks
        .iter()
        .map(|b| match &b.terminator {
            Terminator::Goto { target } => vec![target.0 as usize],
            Terminator::SwitchInt { arms, default, .. } => {
                let mut v: Vec<usize> = arms.iter().map(|(_, t)| t.0 as usize).collect();
                v.push(default.0 as usize);
                v
            }
            Terminator::Call { target, .. } => target.iter().map(|t| t.0 as usize).collect(),
            Terminator::Assert { target, .. } => vec![target.0 as usize],
            Terminator::Drop { target, .. } => vec![target.0 as usize],
            _ => Vec::new(),
        })
        .collect();

    // The release-side accounting names whose args are not value READS.
    let accounting_release = |name: &str| -> bool {
        matches!(
            name,
            "gos_rt_rc_release"
                | "gos_rt_rc_weak_release"
                | "gos_rt_aggr_release_children"
                | "gos_rt_aggr_zero_guarded"
                | "gos_rt_option_slot_release"
        )
    };
    // True when the statement READS local x (writes excepted; the
    // backstop release of x itself excepted).
    let stmt_mentions = |stmt: &Statement, x: Local| -> bool {
        let StatementKind::Assign { place, rvalue } = &stmt.kind else {
            return false;
        };
        if !place.projection.is_empty() && place.local == x {
            return true;
        }
        let in_op = |op: &Operand| matches!(op, Operand::Copy(p) if p.local == x);
        match rvalue {
            Rvalue::Use(op) => in_op(op),
            Rvalue::BinaryOp { lhs, rhs, .. } => in_op(lhs) || in_op(rhs),
            Rvalue::UnaryOp { operand, .. } | Rvalue::Cast { operand, .. } => in_op(operand),
            Rvalue::Aggregate { operands, .. } => operands.iter().any(in_op),
            Rvalue::Repeat { value, .. } => in_op(value),
            Rvalue::Ref { place: rp, .. } => rp.local == x,
            Rvalue::Len(p) => p.local == x,
            Rvalue::CallIntrinsic { name, args } => {
                if accounting_release(name) {
                    false
                } else {
                    args.iter().any(in_op)
                }
            }
            // Reads a scalar global by symbol; mentions no local.
            Rvalue::StaticLoad(_) => false,
        }
    };
    let stmt_writes = |stmt: &Statement, x: Local| -> bool {
        matches!(&stmt.kind, StatementKind::Assign { place, .. }
            if place.projection.is_empty() && place.local == x)
    };
    let term_mentions = |t: &Terminator, x: Local| -> bool {
        let in_op = |op: &Operand| matches!(op, Operand::Copy(p) if p.local == x);
        match t {
            Terminator::SwitchInt { discriminant, .. } => in_op(discriminant),
            Terminator::Call {
                callee,
                args,
                destination,
                ..
            } => {
                in_op(callee)
                    || args.iter().any(in_op)
                    || (!destination.projection.is_empty() && destination.local == x)
            }
            Terminator::Assert { cond, .. } => in_op(cond),
            _ => false,
        }
    };
    let term_writes = |t: &Terminator, x: Local| -> bool {
        matches!(t, Terminator::Call { destination, .. }
            if destination.projection.is_empty() && destination.local == x)
    };

    // Collect the hoists: (target block, insert-after stmt index or
    // None for "after terminator-mention is unsupported"), the local.
    struct Hoist {
        at_block: usize,
        after_stmt: usize,
        local: Local,
    }
    let mut hoists: Vec<Hoist> = Vec::new();
    for (bi, block) in body.blocks.iter().enumerate() {
        for si in 0..block.stmts.len().saturating_sub(1) {
            // Pattern: release(x) immediately followed by x = Copy(_).
            let StatementKind::Assign {
                rvalue: Rvalue::CallIntrinsic { name, args },
                ..
            } = &block.stmts[si].kind
            else {
                continue;
            };
            if *name != "gos_rt_rc_release" {
                continue;
            }
            let Some(Operand::Copy(xp)) = args.first() else {
                continue;
            };
            if !xp.projection.is_empty() {
                continue;
            }
            let x = xp.local;
            if !is_rc(x) {
                continue;
            }
            let reassign = matches!(&block.stmts[si + 1].kind,
                StatementKind::Assign { place, rvalue }
                    if place.projection.is_empty()
                        && place.local == x
                        && matches!(rvalue, Rvalue::Use(Operand::Copy(_))));
            if !reassign {
                continue;
            }
            // Walk backward to x's last mention, through unique-pred
            // edges, without crossing a write to x or another release
            // of x (an existing earlier release means this one is
            // already a backstop).
            let mut cur = bi;
            let mut start = si; // exclusive upper bound within cur
            let mut found: Option<(usize, usize)> = None;
            let mut steps = 0;
            'walk: loop {
                let blk = &body.blocks[cur];
                for sj in (0..start).rev() {
                    let st = &blk.stmts[sj];
                    if let StatementKind::Assign {
                        rvalue: Rvalue::CallIntrinsic { name, args },
                        ..
                    } = &st.kind
                        && *name == "gos_rt_rc_release"
                        && matches!(args.first(), Some(Operand::Copy(p)) if p.local == x)
                    {
                        // Already released earlier on this path.
                        break 'walk;
                    }
                    if stmt_writes(st, x) {
                        break 'walk;
                    }
                    if stmt_mentions(st, x) {
                        found = Some((cur, sj));
                        break 'walk;
                    }
                }
                steps += 1;
                if steps > 64 {
                    break;
                }
                // At a join (e.g. a loop head: entry edge + back edge),
                // follow the back edge — the highest-numbered
                // predecessor, i.e. the loop body's bottom. This is
                // sound because the original release stays in place as
                // a null-safe backstop: paths that bypass the hoisted
                // release (the loop-entry edge) release the old value
                // exactly where they always did, and every block on
                // the walked segment has been verified mention-free in
                // full, so no path through it can read the nulled
                // local.
                let Some(&p) = preds[cur].iter().max() else {
                    break;
                };
                if p == cur {
                    break;
                }
                let pterm = &body.blocks[p].terminator;
                if term_writes(pterm, x) {
                    break;
                }
                if term_mentions(pterm, x) {
                    // Terminator-position mention (e.g. a call arg):
                    // inserting after a terminator means a successor
                    // head, and `cur`'s head IS that point — but only
                    // when the mention is the unique pred's terminator
                    // and x is not its destination. Insert at the head
                    // of `cur`.
                    found = Some((cur, usize::MAX));
                    break;
                }
                cur = p;
                start = body.blocks[p].stmts.len();
            }
            let Some((mb, ms)) = found else {
                continue;
            };
            // Hoisting to the immediate predecessor position of the
            // original release is a no-op; skip. (`usize::MAX` is the
            // head-of-block sentinel for terminator mentions — always
            // a real hoist, and `+ 1` on it would overflow.)
            if mb == bi && ms != usize::MAX && ms + 1 >= si {
                continue;
            }
            // Forward-liveness guard. The hoisted release NULLS `x`, so it
            // is only sound when `x` is dead from the insertion point until
            // its next write on EVERY path — not just the single back-edge
            // path the walk above verified. With a branch inside the loop
            // body (e.g. a group-match `for` loop that reads the key in one
            // arm and pushes it in another), `x` is read again past the
            // chosen mention; nulling it there frees a still-live value.
            // Walk forward from the insertion point; skip the hoist if any
            // path reads `x` before rewriting it.
            let start_stmt = if ms == usize::MAX { 0 } else { ms + 1 };
            let mut live = false;
            {
                let mut stack: Vec<(usize, usize)> = vec![(mb, start_stmt)];
                let mut visited_from0 = vec![false; n_blocks];
                'fwd: while let Some((b, from)) = stack.pop() {
                    let blk = &body.blocks[b];
                    let mut killed = false;
                    for sj in from..blk.stmts.len() {
                        let st = &blk.stmts[sj];
                        if stmt_mentions(st, x) {
                            live = true;
                            break 'fwd;
                        }
                        if stmt_writes(st, x) {
                            killed = true;
                            break;
                        }
                    }
                    if killed {
                        continue;
                    }
                    if term_mentions(&blk.terminator, x) {
                        live = true;
                        break 'fwd;
                    }
                    // A terminator call whose destination is `x` reissues it
                    // on return: the old value is dead past this block.
                    if term_writes(&blk.terminator, x) {
                        continue;
                    }
                    for &s in &succs[b] {
                        if !visited_from0[s] {
                            visited_from0[s] = true;
                            stack.push((s, 0));
                        }
                    }
                }
            }
            if live {
                continue;
            }
            hoists.push(Hoist {
                at_block: mb,
                after_stmt: ms,
                local: x,
            });
        }
    }
    if hoists.is_empty() {
        return;
    }

    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut next_local = body.locals.len();
    // Descending insertion order keeps earlier indices valid.
    hoists.sort_by_key(|h| std::cmp::Reverse((h.at_block, h.after_stmt)));
    for h in hoists {
        let span = body.blocks[h.at_block].span;
        let rel_dest = Local(u32::try_from(next_local).expect("local overflow"));
        next_local += 1;
        let release = Statement {
            kind: StatementKind::Assign {
                place: Place::local(rel_dest),
                rvalue: Rvalue::CallIntrinsic {
                    name: "gos_rt_rc_release",
                    args: vec![Operand::Copy(Place::local(h.local))],
                },
            },
            span,
        };
        let null_out = Statement {
            kind: StatementKind::Assign {
                place: Place::local(h.local),
                rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            },
            span,
        };
        let at = if h.after_stmt == usize::MAX {
            0
        } else {
            h.after_stmt + 1
        };
        body.blocks[h.at_block].stmts.insert(at, null_out);
        body.blocks[h.at_block].stmts.insert(at, release);
    }
    for _ in body.locals.len()..next_local {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }
}

/// Frees provably single-owner `json::Value` handle locals.
///
/// `gos_rt_json_parse` / `gos_rt_json_get` mint one heap handle per
/// call (a `Box<GosJson>` holding an `Arc` share of the parsed tree);
/// nothing reclaimed them, so every parse in a loop leaked the whole
/// document. A local qualifies when every whole-local write is a call
/// destination or a null/zero init, and its value never escapes: it
/// may only be read as an argument to `gos_rt_json_*` runtime entries
/// (which borrow). Qualifying locals get `gos_rt_json_free` before
/// each re-initialising call and at every return. Aliased, stored,
/// returned, or user-call-passed handles keep today's (leaking)
/// behaviour — a leak is recoverable, a dangling handle is not.
pub(crate) fn insert_json_frees(body: &mut Body, tcx: &gossamer_types::TyCtxt) {
    use gossamer_types::TyKind;
    let n_locals = body.locals.len();
    let arity = body.arity as usize;
    let mut candidate = vec![false; n_locals];
    let mut any = false;
    for i in (arity + 1)..n_locals {
        if matches!(tcx.kind_of(body.locals[i].ty), TyKind::JsonValue) && !body.locals[i].region {
            candidate[i] = true;
            any = true;
        }
    }
    if !any {
        return;
    }
    let is_json_rt = |name: &str| name.starts_with("gos_rt_json_");
    // Whole-local handle moves (`v = Copy(tmp)` with both sides
    // JSON-typed): ownership transfers when the move is the source's
    // ONLY value read and its only such move — the destination owns
    // the handle, the source is never freed. Pre-scan to identify
    // them so the escape check below can treat the move as allowed.
    let jv: Vec<bool> = (0..n_locals)
        .map(|i| matches!(tcx.kind_of(body.locals[i].ty), TyKind::JsonValue))
        .collect();
    let mut value_reads = vec![0usize; n_locals];
    let mut move_edges: Vec<(usize, usize, usize, usize)> = Vec::new(); // (src, dest, bi, si)
    for (bi, block) in body.blocks.iter().enumerate() {
        let mut count_op = |op: &Operand| {
            if let Operand::Copy(p) = op
                && p.projection.is_empty()
                && (p.local.0 as usize) < n_locals
            {
                value_reads[p.local.0 as usize] += 1;
            }
        };
        for (si, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            match rvalue {
                Rvalue::Use(Operand::Copy(src)) => {
                    if place.projection.is_empty()
                        && src.projection.is_empty()
                        && (place.local.0 as usize) < n_locals
                        && (src.local.0 as usize) < n_locals
                        && jv[place.local.0 as usize]
                        && jv[src.local.0 as usize]
                    {
                        move_edges.push((src.local.0 as usize, place.local.0 as usize, bi, si));
                    } else {
                        count_op(&Operand::Copy(src.clone()));
                    }
                }
                Rvalue::CallIntrinsic { name, args } if is_json_rt(name) => {
                    // Borrowing json-runtime args are not value reads.
                    let _ = args;
                }
                Rvalue::CallIntrinsic { args, .. } => {
                    for a in args {
                        count_op(a);
                    }
                }
                Rvalue::BinaryOp { lhs, rhs, .. } => {
                    count_op(lhs);
                    count_op(rhs);
                }
                Rvalue::UnaryOp { operand, .. } | Rvalue::Cast { operand, .. } => count_op(operand),
                Rvalue::Aggregate { operands, .. } => {
                    for a in operands {
                        count_op(a);
                    }
                }
                Rvalue::Repeat { value, .. } => count_op(value),
                Rvalue::Ref { .. } | Rvalue::Len(_) => {}
                Rvalue::Use(_) => {}
                Rvalue::StaticLoad(_) => {}
            }
        }
        if let Terminator::Call { callee, args, .. } = &block.terminator {
            let allowed = matches!(callee, Operand::Const(ConstValue::Str(n)) if is_json_rt(n));
            if !allowed {
                for a in args {
                    count_op(a);
                }
            }
        }
    }
    // A source moves cleanly when it has exactly one outgoing move and
    // no other value reads.
    let mut moved_from = vec![false; n_locals];
    let mut move_inits: Vec<(usize, usize, usize)> = Vec::new(); // (dest, bi, si)
    {
        let mut out_moves = vec![0usize; n_locals];
        for &(src, _, _, _) in &move_edges {
            out_moves[src] += 1;
        }
        for &(src, dest, bi, si) in &move_edges {
            if out_moves[src] == 1 && value_reads[src] == 0 {
                moved_from[src] = true;
                move_inits.push((dest, bi, si));
            }
        }
    }
    fn check_op(op: &Operand, allowed: bool, c: &mut [bool]) {
        if let Operand::Copy(p) = op
            && (p.local.0 as usize) < c.len()
            && c[p.local.0 as usize]
            && !allowed
        {
            c[p.local.0 as usize] = false;
        }
    }
    // Init sites per local: (block, stmt-or-terminator marker).
    let mut init_sites: Vec<Vec<(usize, usize)>> = vec![Vec::new(); n_locals];
    for (bi, block) in body.blocks.iter().enumerate() {
        for (si, stmt) in block.stmts.iter().enumerate() {
            let StatementKind::Assign { place, rvalue } = &stmt.kind else {
                continue;
            };
            // Reads: any appearance as a Copy operand outside a
            // json-runtime call argument escapes the handle.
            match rvalue {
                Rvalue::CallIntrinsic { name, args } => {
                    let allowed = is_json_rt(name);
                    for a in args {
                        check_op(a, allowed, &mut candidate);
                    }
                }
                Rvalue::Use(op) => {
                    let clean_move = matches!(
                        op,
                        Operand::Copy(p)
                            if p.projection.is_empty()
                                && (p.local.0 as usize) < n_locals
                                && moved_from[p.local.0 as usize]
                                && place.projection.is_empty()
                                && (place.local.0 as usize) < n_locals
                                && jv[place.local.0 as usize]
                    );
                    check_op(op, clean_move, &mut candidate);
                }
                Rvalue::BinaryOp { lhs, rhs, .. } => {
                    check_op(lhs, false, &mut candidate);
                    check_op(rhs, false, &mut candidate);
                }
                Rvalue::UnaryOp { operand, .. } | Rvalue::Cast { operand, .. } => {
                    check_op(operand, false, &mut candidate);
                }
                Rvalue::Aggregate { operands, .. } => {
                    for a in operands {
                        check_op(a, false, &mut candidate);
                    }
                }
                Rvalue::Repeat { value, .. } => check_op(value, false, &mut candidate),
                Rvalue::Ref { place: rp, .. } => {
                    if candidate.get(rp.local.0 as usize).copied().unwrap_or(false) {
                        candidate[rp.local.0 as usize] = false;
                    }
                }
                Rvalue::Len(_) => {}
                Rvalue::StaticLoad(_) => {}
            }
            // Writes to the candidate itself.
            if place.projection.is_empty() && (place.local.0 as usize) < n_locals {
                let i = place.local.0 as usize;
                if candidate[i] {
                    match rvalue {
                        Rvalue::CallIntrinsic { .. } => init_sites[i].push((bi, si)),
                        Rvalue::Use(Operand::Const(ConstValue::Int(_))) => {}
                        Rvalue::Use(Operand::Copy(src))
                            if src.projection.is_empty()
                                && (src.local.0 as usize) < n_locals
                                && moved_from[src.local.0 as usize] =>
                        {
                            init_sites[i].push((bi, si));
                        }
                        _ => candidate[i] = false,
                    }
                }
            } else if !place.projection.is_empty()
                && (place.local.0 as usize) < n_locals
                && candidate[place.local.0 as usize]
            {
                candidate[place.local.0 as usize] = false;
            }
        }
        match &block.terminator {
            Terminator::Call {
                callee,
                args,
                destination,
                ..
            } => {
                let callee_name = match callee {
                    Operand::Const(ConstValue::Str(n)) => Some(n.as_str()),
                    _ => None,
                };
                let allowed = callee_name.is_some_and(is_json_rt);
                for a in args {
                    if let Operand::Copy(p) = a
                        && (p.local.0 as usize) < n_locals
                        && candidate[p.local.0 as usize]
                        && !allowed
                    {
                        candidate[p.local.0 as usize] = false;
                    }
                }
                if destination.projection.is_empty()
                    && (destination.local.0 as usize) < n_locals
                    && candidate[destination.local.0 as usize]
                {
                    init_sites[destination.local.0 as usize].push((bi, usize::MAX));
                }
            }
            Terminator::SwitchInt { discriminant, .. } => {
                if let Operand::Copy(p) = discriminant
                    && (p.local.0 as usize) < n_locals
                    && candidate[p.local.0 as usize]
                {
                    candidate[p.local.0 as usize] = false;
                }
            }
            _ => {}
        }
    }
    let qualified: Vec<usize> = (0..n_locals)
        .filter(|&i| candidate[i] && !moved_from[i])
        .collect();
    if qualified.is_empty() {
        return;
    }
    let unit_ty = tcx.unit_interned().unwrap_or(body.locals[0].ty);
    let mut next_local = body.locals.len();
    let free_stmt = |l: usize, span: gossamer_lex::Span, next: &mut usize| -> Statement {
        let dest = Local(u32::try_from(*next).expect("local overflow"));
        *next += 1;
        Statement {
            kind: StatementKind::Assign {
                place: Place::local(dest),
                rvalue: Rvalue::CallIntrinsic {
                    name: "gos_rt_json_free",
                    args: vec![Operand::Copy(Place::local(Local(
                        u32::try_from(l).unwrap_or(0),
                    )))],
                },
            },
            span,
        }
    };
    // Per-block gap lists: stmt-index -> stmts to insert before it,
    // plus an end-of-block list for Return frees.
    let nb = body.blocks.len();
    let mut pre_gaps: Vec<Vec<(usize, Statement)>> = vec![Vec::new(); nb];
    let mut end_gaps: Vec<Vec<Statement>> = vec![Vec::new(); nb];
    for &l in &qualified {
        // Free the previous value before each re-initialising call
        // (first execution frees the zero init, which is null-safe).
        for &(bi, si) in &init_sites[l] {
            let span = body.blocks[bi].span;
            if si == usize::MAX {
                end_gaps[bi].push(free_stmt(l, span, &mut next_local));
            } else {
                pre_gaps[bi].push((si, free_stmt(l, span, &mut next_local)));
            }
        }
    }
    for (bi, block) in body.blocks.iter().enumerate() {
        if matches!(block.terminator, Terminator::Return) {
            let span = block.span;
            for &l in &qualified {
                end_gaps[bi].push(free_stmt(l, span, &mut next_local));
            }
        }
    }
    for bi in (0..nb).rev() {
        pre_gaps[bi].sort_by_key(|(si, _)| std::cmp::Reverse(*si));
        let drained: Vec<(usize, Statement)> = std::mem::take(&mut pre_gaps[bi]);
        for (si, stmt) in drained {
            body.blocks[bi].stmts.insert(si, stmt);
        }
        for stmt in std::mem::take(&mut end_gaps[bi]) {
            body.blocks[bi].stmts.push(stmt);
        }
    }
    // The pre-init frees below read the local's previous value; only
    // some locals get the MIR zero-init, so make it explicit for every
    // qualified local (free of null is a no-op).
    if !body.blocks.is_empty() {
        let span = body.blocks[0].span;
        for (k, &l) in qualified.iter().enumerate() {
            body.blocks[0].stmts.insert(
                k,
                Statement {
                    kind: StatementKind::Assign {
                        place: Place::local(Local(u32::try_from(l).unwrap_or(0))),
                        rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    },
                    span,
                },
            );
        }
    }
    for _ in body.locals.len()..next_local {
        body.locals.push(LocalDecl {
            ty: unit_ty,
            debug_name: None,
            mutable: false,
            region: false,
        });
    }
}
