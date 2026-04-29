//! Heap-allocator cleanup analysis.
//!
//! Identifies locals that own the result of a runtime heap-allocator
//! call (`gos_rt_heap_i64_new`, `gos_rt_heap_u8_new`, `gos_rt_chan_new`)
//! and that do not escape the current body. Both codegen tiers query
//! this set so they can emit a matching `gos_rt_*_free` /
//! `gos_rt_chan_drop` call at every `Return` terminator — closing the
//! "every `Vec<i64>` / `Vec<u8>` / `Channel` leaks until process exit"
//! finding (C2 in `~/dev/contexts/lang/adversarial_analysis.md`).
//!
//! The analysis is deliberately conservative: an allocation is only
//! cleaned up when its destination local is known not to escape and
//! the allocating call's block dominates *every* `Return` block. The
//! second condition rules out conditional allocations whose value the
//! return path may never have observed.

#![forbid(unsafe_code)]

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use crate::escape::analyse as analyse_escape;
use crate::ir::{BlockId, Body, ConstValue, Local, Operand, Terminator};

/// Names of runtime functions that allocate on the heap and return
/// an owning pointer. Each entry maps a constructor symbol to the
/// matching reclamation symbol the codegen should call when the
/// destination local is dropped.
pub const HEAP_ALLOCATOR_PAIRS: &[(&str, &str)] = &[
    ("gos_rt_heap_i64_new", "gos_rt_heap_i64_free"),
    ("gos_rt_heap_u8_new", "gos_rt_heap_u8_free"),
    ("gos_rt_chan_new", "gos_rt_chan_drop"),
];

/// Per-body cleanup plan: every entry says "before each `Return`,
/// load the value of `local` and call the runtime function named
/// `free_fn`". The list is in deterministic order (locals ascending)
/// so the codegens emit the same byte stream on repeated runs.
#[derive(Debug, Clone, Default)]
pub struct CleanupPlan {
    entries: Vec<CleanupEntry>,
}

/// One owning heap local that the cleanup pass found.
#[derive(Debug, Clone, Copy)]
pub struct CleanupEntry {
    /// Local that holds the owning pointer.
    pub local: Local,
    /// Runtime symbol the codegen should call to free `local`.
    pub free_fn: &'static str,
}

impl CleanupPlan {
    /// Returns every cleanup entry in stable order.
    #[must_use]
    pub fn entries(&self) -> &[CleanupEntry] {
        &self.entries
    }

    /// Whether the plan has anything to emit. Hot-path test that
    /// lets codegens skip the cleanup-emit prologue when no
    /// allocations were found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Returns the cleanup plan for `body`. Pure inspection of the
/// body — the body itself is not mutated.
#[must_use]
pub fn plan(body: &Body) -> CleanupPlan {
    // Map from allocator symbol → free symbol so the lookup below
    // is constant-time.
    let alloc_pairs: BTreeMap<&str, &str> = HEAP_ALLOCATOR_PAIRS.iter().copied().collect();

    // Find every Call terminator whose callee is a known allocator
    // and whose destination is rooted in a single local with no
    // projections (i.e., a fresh local receiving the allocator's
    // return value).
    let mut candidates: Vec<(Local, &'static str, BlockId)> = Vec::new();
    for block in &body.blocks {
        if let Terminator::Call {
            callee,
            destination,
            ..
        } = &block.terminator
            && let Operand::Const(ConstValue::Str(name)) = callee
            && let Some(free_fn) = alloc_pairs.get(name.as_str()).copied()
            && destination.projection.is_empty()
        {
            candidates.push((destination.local, free_fn, block.id));
        }
    }
    if candidates.is_empty() {
        return CleanupPlan::default();
    }

    // Drop any candidate whose local escapes — return values, call
    // arguments, and aggregates are off-limits because the freed
    // memory may still be reachable by the caller.
    let escape = analyse_escape(body);
    candidates.retain(|(local, _, _)| escape.is_non_escaping(*local));
    if candidates.is_empty() {
        return CleanupPlan::default();
    }

    // Compute reverse-CFG reachability so we can verify that every
    // `Return` block is dominated (in the sense of "must-execute")
    // by the allocator's block. We approximate dominance with a
    // forward reachability test from `entry`: a block B dominates
    // every path through `Return` only when removing B from the CFG
    // makes those Returns unreachable from entry. The
    // simpler-and-equivalent test we use here: the allocator's
    // block must lie on *every* path from entry to *every* `Return`.
    // We compute this by checking that, after removing the
    // allocator's block from the graph, no `Return` block is still
    // reachable from entry.
    let return_blocks: Vec<BlockId> = body
        .blocks
        .iter()
        .filter(|b| matches!(b.terminator, Terminator::Return))
        .map(|b| b.id)
        .collect();
    if return_blocks.is_empty() {
        return CleanupPlan::default();
    }

    // Track every (local, free_fn) that survives the dominance check
    // and dedupe so a re-assigned local only frees once.
    let mut seen: BTreeSet<u32> = BTreeSet::new();
    let mut entries: Vec<CleanupEntry> = Vec::new();
    for (local, free_fn, alloc_block) in candidates {
        if seen.contains(&local.0) {
            continue;
        }
        if dominates_all(body, alloc_block, &return_blocks) {
            seen.insert(local.0);
            entries.push(CleanupEntry { local, free_fn });
        }
    }
    entries.sort_by_key(|e| e.local.0);
    CleanupPlan { entries }
}

/// Tests whether `gate` is on every path from the body's entry to
/// each block in `targets`. Implemented by forward BFS that skips
/// `gate`: if any target stays reachable, `gate` does not dominate
/// it.
fn dominates_all(body: &Body, gate: BlockId, targets: &[BlockId]) -> bool {
    let entry = match body.blocks.first() {
        Some(b) => b.id,
        None => return false,
    };
    if entry == gate {
        // The gate is the entry block — vacuously dominates every
        // reachable target.
        return true;
    }
    let mut visited: BTreeSet<u32> = BTreeSet::new();
    let mut queue: VecDeque<BlockId> = VecDeque::new();
    queue.push_back(entry);
    visited.insert(entry.as_u32());
    while let Some(b) = queue.pop_front() {
        if b == gate {
            continue;
        }
        let block = &body.blocks[b.as_u32() as usize];
        for succ in successors(&block.terminator) {
            if succ == gate {
                continue;
            }
            if visited.insert(succ.as_u32()) {
                queue.push_back(succ);
            }
        }
    }
    !targets.iter().any(|t| visited.contains(&t.as_u32()))
}

fn successors(t: &Terminator) -> Vec<BlockId> {
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

#[cfg(test)]
mod tests {
    use gossamer_hir::lower_source_file;
    use gossamer_lex::SourceMap;
    use gossamer_parse::parse_source_file;
    use gossamer_resolve::resolve_source_file;
    use gossamer_types::{TyCtxt, typecheck_source_file};

    use super::*;
    use crate::lower_program;

    fn build(source: &str) -> Vec<Body> {
        let mut map = SourceMap::new();
        let file = map.add_file("t.gos", source.to_string());
        let (sf, _) = parse_source_file(source, file);
        let (res, _) = resolve_source_file(&sf);
        let mut tcx = TyCtxt::new();
        let (tbl, _) = typecheck_source_file(&sf, &res, &mut tcx);
        let hir = lower_source_file(&sf, &res, &tbl, &mut tcx);
        lower_program(&hir, &mut tcx)
    }

    #[test]
    fn empty_plan_for_function_with_no_heap_allocations() {
        let bodies = build("fn f() -> i64 { 1i64 + 2i64 }\n");
        let plan = plan(&bodies[0]);
        assert!(plan.is_empty());
    }
}
