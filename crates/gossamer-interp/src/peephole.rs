//! Compile-time peephole pass for the bytecode VM.
//!
//! Fuses the universal `<arith op> dst=tmp, ...; <kind-specific
//! Move> dst=local, src=tmp` shape that the lowerer emits for every
//! `local = expr` assignment into a single op writing the result
//! directly into `local`. Saves one dispatch + one register write
//! per fused pair on every `s = s + i` / `s.x = s.x + 1` / etc.
//!
//! Profiling on a `while i < N { s = s + i; i = i + 1 }` loop
//! showed `MoveI64` at ~22% of executed instructions, with a 1:1
//! `(arith) -> MoveI64` adjacency — the peephole removes that
//! whole class.
//!
//! ## Safety
//!
//! Fusion is only applied when ALL of the following hold:
//! 1. The move's source register is read EXACTLY once across the
//!    entire instruction stream (the move itself).
//! 2. The previous op writes to the move's source register and is
//!    a member of the whitelisted "pure arith / load" set.
//! 3. The move is NOT the target of any branch/jump/exception
//!    handler, so fusion can't cause us to miss the writer.
//! 4. The previous op is NOT a branch/jump target either — fusion
//!    leaves its position in the stream, but its destination is
//!    still rewritten so any jump that lands on it gets the
//!    fused behaviour, which is what we want.
//!
//! Together these guarantee that observable behaviour is
//! unchanged: the only instructions we drop produce a register
//! value that is read exactly once and only by the dropped move.

use crate::bytecode::{InstrIdx, Op, Reg};

/// Run the peephole on `instrs` in-place. `arity` is the number
/// of value-file parameters — registers `[0, arity)` are read
/// implicitly at every `Return` for `&mut` param propagation, so
/// the dead-load pass refuses to drop writes that target them.
/// Idempotent: applying it twice produces the same output as once.
pub(crate) fn run(instrs: &mut Vec<Op>, arity: u16) {
    if instrs.len() < 2 {
        return;
    }
    // Op::Wide indexes into the side `wide_ops` table — the
    // payload holds registers (e.g. `BuildFloatArray` reads
    // `first_f..first_f+stride*elem_count`) that this pass does
    // not introspect. Bailing out keeps the optimisation sound:
    // we'd otherwise miscount reads and possibly drop a load
    // that feeds a wide op. The vast majority of compiled
    // functions don't emit any `Op::Wide`, so this guard is
    // cheap and rarely fires.
    if instrs.iter().any(|op| matches!(op, Op::Wide { .. })) {
        return;
    }
    fuse_into_dest(instrs, arity);
    drop_dead_const_loads(instrs, arity);
}

fn fuse_into_dest(instrs: &mut Vec<Op>, arity: u16) {
    let _ = arity;
    // Per-register read counts across the whole stream, split by
    // register file. A move can only be fused when the move's
    // source is read once total — by the move itself.
    let (max_v, max_f, max_i) = max_regs(instrs);
    let mut value_reads: Vec<u32> = vec![0; max_v as usize + 1];
    let mut float_reads: Vec<u32> = vec![0; max_f as usize + 1];
    let mut int_reads: Vec<u32> = vec![0; max_i as usize + 1];
    for op in instrs.iter().copied() {
        for r in op_value_reads(op) {
            value_reads[r as usize] = value_reads[r as usize].saturating_add(1);
        }
        for r in op_float_reads(op) {
            float_reads[r as usize] = float_reads[r as usize].saturating_add(1);
        }
        for r in op_int_reads(op) {
            int_reads[r as usize] = int_reads[r as usize].saturating_add(1);
        }
    }

    // Mark every PC that is a jump / branch target so we can
    // refuse to drop or stand-in for an instruction that some
    // other point in the function is going to land on.
    let mut is_target: Vec<bool> = vec![false; instrs.len() + 1];
    for op in instrs.iter().copied() {
        if let Some(t) = jump_target(op) {
            let t = t as usize;
            if t < is_target.len() {
                is_target[t] = true;
            }
        }
    }

    // Pass 1: build the drop-set + the new (possibly rewritten)
    // op for each surviving slot. A "drop" means: the move at
    // pc[i] is fused into pc[i-1]; we keep pc[i-1] (rewritten)
    // and discard pc[i].
    let n = instrs.len();
    let mut drop: Vec<bool> = vec![false; n];
    let mut rewritten: Vec<Option<Op>> = vec![None; n];
    let mut i = 1usize;
    while i < n {
        if is_target[i] {
            // The move (or its successor) is jumped to — fusion
            // would skip the move's effect on that branch path.
            i += 1;
            continue;
        }
        let move_op = instrs[i];
        let prev_op = instrs[i - 1];
        let Some((kind, dst_local, src_temp)) = move_pair(move_op) else {
            i += 1;
            continue;
        };
        // Source must have exactly one reader (this move).
        let read_count = match kind {
            FileKind::Value => value_reads.get(src_temp as usize).copied().unwrap_or(0),
            FileKind::F64 => float_reads.get(src_temp as usize).copied().unwrap_or(0),
            FileKind::I64 => int_reads.get(src_temp as usize).copied().unwrap_or(0),
        };
        if read_count != 1 {
            i += 1;
            continue;
        }
        // Previous op must write to `src_temp` in the matching
        // file AND be in the whitelist of safe-to-fuse ops.
        let Some(new_prev) = retarget_dst_if_fuseable(prev_op, kind, src_temp, dst_local) else {
            i += 1;
            continue;
        };
        // Don't fuse across a frame-entry boundary: pc==0 has no
        // real predecessor in dispatch order. We already test
        // `i >= 1` so the predecessor exists in the stream.
        rewritten[i - 1] = Some(new_prev);
        drop[i] = true;
        i += 2; // skip the dropped move
    }

    // Pass 2: rebuild `instrs` and a pc-remap table.
    let mut new_pc: Vec<InstrIdx> = vec![0; n + 1];
    let mut out: Vec<Op> = Vec::with_capacity(n);
    for old in 0..n {
        new_pc[old] = u32::try_from(out.len()).expect("instr overflow");
        if drop[old] {
            continue;
        }
        let op = rewritten[old].unwrap_or(instrs[old]);
        out.push(op);
    }
    new_pc[n] = u32::try_from(out.len()).expect("instr overflow");

    // Pass 3: patch every jump's target through the remap. A
    // dropped op's slot in `new_pc` points at the next surviving
    // op, which is exactly what we want for any branch that
    // targeted the dropped slot directly (none should, since we
    // filtered `is_target[i] == true` above, but the remap
    // handles it safely either way).
    for op in &mut out {
        if let Some(t) = jump_target_mut(op) {
            let old = *t as usize;
            let mapped = if old < new_pc.len() {
                new_pc[old]
            } else {
                new_pc[n]
            };
            *t = mapped;
        }
    }

    *instrs = out;
}

/// Drops `LoadConst*` ops whose destination register is never
/// read anywhere in the function. The lowerer emits one of these
/// for every statement-context expression that evaluates to a
/// `()` (assignments, loops, blocks ending in `;`); the result
/// register is then ignored. In tight loops these accumulate to
/// 20-30% of executed instructions.
///
/// Safety: only `LoadConst` / `LoadConstI64` / `LoadConstF64` are
/// considered, all of which are pure — dropping them changes no
/// observable state. We refuse to drop an op that is the target
/// of any branch / jump (someone might land directly on it),
/// and we patch jump targets through the resulting remap so
/// transitive jumps stay coherent.
fn drop_dead_const_loads(instrs: &mut Vec<Op>, arity: u16) {
    let (max_v, max_f, max_i) = max_regs(instrs);
    let mut value_reads: Vec<u32> = vec![0; max_v as usize + 1];
    let mut float_reads: Vec<u32> = vec![0; max_f as usize + 1];
    let mut int_reads: Vec<u32> = vec![0; max_i as usize + 1];
    for op in instrs.iter().copied() {
        for r in op_value_reads(op) {
            value_reads[r as usize] = value_reads[r as usize].saturating_add(1);
        }
        for r in op_float_reads(op) {
            float_reads[r as usize] = float_reads[r as usize].saturating_add(1);
        }
        for r in op_int_reads(op) {
            int_reads[r as usize] = int_reads[r as usize].saturating_add(1);
        }
    }
    let mut is_target: Vec<bool> = vec![false; instrs.len() + 1];
    for op in instrs.iter().copied() {
        if let Some(t) = jump_target(op) {
            let t = t as usize;
            if t < is_target.len() {
                is_target[t] = true;
            }
        }
    }

    let n = instrs.len();
    let mut drop: Vec<bool> = vec![false; n];
    for (pc, op) in instrs.iter().copied().enumerate() {
        if is_target[pc] {
            continue;
        }
        match op {
            Op::LoadConst { dst, .. } => {
                // Refuse to drop writes into the value-file
                // parameter range; `Op::Return`'s `params` snapshot
                // reads `registers[0..arity]` for `&mut` param
                // propagation back to the caller's arg slots.
                if dst < arity {
                    continue;
                }
                if value_reads.get(dst as usize).copied().unwrap_or(0) == 0 {
                    drop[pc] = true;
                }
            }
            Op::LoadConstI64 { dst_i, .. }
                if int_reads.get(dst_i as usize).copied().unwrap_or(0) == 0 =>
            {
                drop[pc] = true;
            }
            Op::LoadConstF64 { dst_f, .. }
                if float_reads.get(dst_f as usize).copied().unwrap_or(0) == 0 =>
            {
                drop[pc] = true;
            }
            _ => {}
        }
    }

    let mut new_pc: Vec<InstrIdx> = vec![0; n + 1];
    let mut out: Vec<Op> = Vec::with_capacity(n);
    for old in 0..n {
        new_pc[old] = u32::try_from(out.len()).expect("instr overflow");
        if drop[old] {
            continue;
        }
        out.push(instrs[old]);
    }
    new_pc[n] = u32::try_from(out.len()).expect("instr overflow");
    for op in &mut out {
        if let Some(t) = jump_target_mut(op) {
            let old = *t as usize;
            let mapped = if old < new_pc.len() {
                new_pc[old]
            } else {
                new_pc[n]
            };
            *t = mapped;
        }
    }
    *instrs = out;
}

/// One of the three register files the VM keeps separate.
#[derive(Copy, Clone, Eq, PartialEq, Debug)]
enum FileKind {
    Value,
    F64,
    I64,
}

/// Decode an `Op::Move*` instruction into `(file, dst, src)`.
/// Returns `None` for non-move ops.
fn move_pair(op: Op) -> Option<(FileKind, Reg, Reg)> {
    match op {
        Op::Move { dst, src } => Some((FileKind::Value, dst, src)),
        Op::MoveF64 { dst_f, src_f } => Some((FileKind::F64, dst_f, src_f)),
        Op::MoveI64 { dst_i, src_i } => Some((FileKind::I64, dst_i, src_i)),
        _ => None,
    }
}

/// Retarget the destination of `op` to `new_dst`, but only when
/// `op`'s declared destination is in the matching `kind` file
/// AND its current dst equals `expected_dst`. The whitelist
/// inside is conservative: we only rewrite ops whose only
/// observable effect is "set `dst` to a function of the operands"
/// (no side channels through static state, no nested calls, no
/// branches). Returns `None` to refuse the rewrite.
#[allow(
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    reason = "exhaustive op match"
)]
fn retarget_dst_if_fuseable(op: Op, kind: FileKind, expected_dst: Reg, new_dst: Reg) -> Option<Op> {
    macro_rules! retarget_i {
        ($variant:ident, $($field:ident),*) => {
            if let Op::$variant { dst_i, $($field),* } = op {
                if kind == FileKind::I64 && dst_i == expected_dst {
                    return Some(Op::$variant { dst_i: new_dst, $($field),* });
                }
            }
        };
    }
    macro_rules! retarget_f {
        ($variant:ident, $($field:ident),*) => {
            if let Op::$variant { dst_f, $($field),* } = op {
                if kind == FileKind::F64 && dst_f == expected_dst {
                    return Some(Op::$variant { dst_f: new_dst, $($field),* });
                }
            }
        };
    }
    macro_rules! retarget_v {
        ($variant:ident, $($field:ident),*) => {
            if let Op::$variant { dst, $($field),* } = op {
                if kind == FileKind::Value && dst == expected_dst {
                    return Some(Op::$variant { dst: new_dst, $($field),* });
                }
            }
        };
    }

    // Whitelisted I64 producers.
    retarget_i!(LoadConstI64, idx);
    retarget_i!(AddI64, lhs_i, rhs_i);
    retarget_i!(SubI64, lhs_i, rhs_i);
    retarget_i!(MulI64, lhs_i, rhs_i);
    retarget_i!(DivI64, lhs_i, rhs_i);
    retarget_i!(RemI64, lhs_i, rhs_i);
    retarget_i!(NegI64, src_i);
    retarget_i!(BitAndI64, lhs_i, rhs_i);
    retarget_i!(BitOrI64, lhs_i, rhs_i);
    retarget_i!(BitXorI64, lhs_i, rhs_i);
    retarget_i!(ShlI64, lhs_i, rhs_i);
    retarget_i!(ShrI64, lhs_i, rhs_i);

    // Whitelisted F64 producers.
    retarget_f!(LoadConstF64, idx);
    retarget_f!(AddF64, lhs_f, rhs_f);
    retarget_f!(SubF64, lhs_f, rhs_f);
    retarget_f!(MulF64, lhs_f, rhs_f);
    retarget_f!(DivF64, lhs_f, rhs_f);
    retarget_f!(NegF64, src_f);
    retarget_f!(SqrtF64, src_f);
    retarget_f!(SinF64, src_f);
    retarget_f!(CosF64, src_f);
    retarget_f!(AbsF64, src_f);
    retarget_f!(FloorF64, src_f);
    retarget_f!(CeilF64, src_f);
    retarget_f!(ExpF64, src_f);
    retarget_f!(LnF64, src_f);
    retarget_f!(MulAddF64, a_f, b_f, c_f);
    retarget_f!(MulSubF64, a_f, b_f, c_f);

    // Whitelisted Value producers. We avoid Call / MethodCall /
    // Spawn / IndexGet / FieldGet / IndexedFieldGet / ANY op that
    // could allocate or side-effect, even though `dst` is the
    // single observable result, because we want fusion to be a
    // surgical-no-risk pass. The misses we leave on the table
    // here are tiny relative to the arith-into-local case.
    retarget_v!(LoadConst, idx);
    // Box ops use `dst_v` rather than `dst` for the destination.
    if let Op::BoxF64 { dst_v, src_f } = op {
        if kind == FileKind::Value && dst_v == expected_dst {
            return Some(Op::BoxF64 {
                dst_v: new_dst,
                src_f,
            });
        }
    }
    if let Op::BoxI64 { dst_v, src_i } = op {
        if kind == FileKind::Value && dst_v == expected_dst {
            return Some(Op::BoxI64 {
                dst_v: new_dst,
                src_i,
            });
        }
    }
    if let Op::BoxU64 { dst_v, src_i } = op {
        if kind == FileKind::Value && dst_v == expected_dst {
            return Some(Op::BoxU64 {
                dst_v: new_dst,
                src_i,
            });
        }
    }

    None
}

/// Compute the highest register index referenced in any file.
fn max_regs(instrs: &[Op]) -> (Reg, Reg, Reg) {
    let mut mv: Reg = 0;
    let mut mf: Reg = 0;
    let mut mi: Reg = 0;
    for op in instrs.iter().copied() {
        for r in op_value_reads(op).into_iter().chain(op_value_writes(op)) {
            if r > mv {
                mv = r;
            }
        }
        for r in op_float_reads(op).into_iter().chain(op_float_writes(op)) {
            if r > mf {
                mf = r;
            }
        }
        for r in op_int_reads(op).into_iter().chain(op_int_writes(op)) {
            if r > mi {
                mi = r;
            }
        }
    }
    (mv, mf, mi)
}

/// Returns the jump target if `op` is a branch / jump.
fn jump_target(op: Op) -> Option<InstrIdx> {
    match op {
        Op::Jump { target }
        | Op::BranchIf { target, .. }
        | Op::BranchIfNot { target, .. }
        | Op::BranchIfLtI64 { target, .. }
        | Op::BranchIfGeI64 { target, .. }
        | Op::BranchIfGtI64 { target, .. }
        | Op::BranchIfLtF64 { target, .. }
        | Op::BranchIfGeF64 { target, .. }
        | Op::IncJumpIfLtI64 { target, .. }
        | Op::IncJumpIfLeI64 { target, .. }
        | Op::TryRecv {
            on_empty: target, ..
        } => Some(target),
        Op::ForLoopI64 { target_top, .. } | Op::ForLoopInclusiveI64 { target_top, .. } => {
            Some(target_top)
        }
        _ => None,
    }
}

/// Mutable accessor for jump targets so the remap can patch
/// them in place after fusion drops some instructions.
fn jump_target_mut(op: &mut Op) -> Option<&mut InstrIdx> {
    match op {
        Op::Jump { target }
        | Op::BranchIf { target, .. }
        | Op::BranchIfNot { target, .. }
        | Op::BranchIfLtI64 { target, .. }
        | Op::BranchIfGeI64 { target, .. }
        | Op::BranchIfGtI64 { target, .. }
        | Op::BranchIfLtF64 { target, .. }
        | Op::BranchIfGeF64 { target, .. }
        | Op::IncJumpIfLtI64 { target, .. }
        | Op::IncJumpIfLeI64 { target, .. }
        | Op::TryRecv {
            on_empty: target, ..
        } => Some(target),
        Op::ForLoopI64 { target_top, .. } | Op::ForLoopInclusiveI64 { target_top, .. } => {
            Some(target_top)
        }
        _ => None,
    }
}

// ---- Per-op read/write tables for the three register files ----
//
// Conservative: when in doubt, we list the register so the
// peephole's safety check stays sound. The fusion is gated by
// "`src` is read EXACTLY once" so over-reporting reads can only
// reduce fusion opportunities, never break correctness.

#[allow(clippy::too_many_lines, reason = "exhaustive op match")]
fn op_int_reads(op: Op) -> Vec<Reg> {
    match op {
        Op::AddI64 { lhs_i, rhs_i, .. }
        | Op::SubI64 { lhs_i, rhs_i, .. }
        | Op::MulI64 { lhs_i, rhs_i, .. }
        | Op::DivI64 { lhs_i, rhs_i, .. }
        | Op::RemI64 { lhs_i, rhs_i, .. }
        | Op::BitAndI64 { lhs_i, rhs_i, .. }
        | Op::BitOrI64 { lhs_i, rhs_i, .. }
        | Op::BitXorI64 { lhs_i, rhs_i, .. }
        | Op::ShlI64 { lhs_i, rhs_i, .. }
        | Op::ShrI64 { lhs_i, rhs_i, .. }
        | Op::LtI64 { lhs_i, rhs_i, .. }
        | Op::LeI64 { lhs_i, rhs_i, .. }
        | Op::GtI64 { lhs_i, rhs_i, .. }
        | Op::GeI64 { lhs_i, rhs_i, .. }
        | Op::EqI64 { lhs_i, rhs_i, .. }
        | Op::NeI64 { lhs_i, rhs_i, .. }
        | Op::BranchIfLtI64 { lhs_i, rhs_i, .. }
        | Op::BranchIfGeI64 { lhs_i, rhs_i, .. }
        | Op::BranchIfGtI64 { lhs_i, rhs_i, .. } => vec![lhs_i, rhs_i],
        Op::ForLoopI64 {
            counter_i, end_i, ..
        }
        | Op::ForLoopInclusiveI64 {
            counter_i, end_i, ..
        }
        | Op::IncJumpIfLtI64 {
            counter_i, end_i, ..
        }
        | Op::IncJumpIfLeI64 {
            counter_i, end_i, ..
        } => vec![counter_i, end_i],
        Op::IntArraySwap { i_idx, j_idx, .. } => vec![i_idx, j_idx],
        Op::NegI64 { src_i, .. }
        | Op::BoxI64 { src_i, .. }
        | Op::BoxU64 { src_i, .. }
        | Op::MoveI64 { src_i, .. }
        | Op::IntToFloatF64 { src_i, .. } => vec![src_i],
        Op::U8VecGetByte { idx_reg, .. }
        | Op::IntArrayGetI64 {
            index_i: idx_reg, ..
        } => {
            vec![idx_reg]
        }
        Op::FloatVecGetF64 { index_i, .. } | Op::FloatVecSetF64 { index_i, .. } => vec![index_i],
        Op::BuildIntArray { first_i, count, .. } => {
            (0..count).map(|i| first_i.saturating_add(i)).collect()
        }
        Op::IntMapInc { key_i, by_i, .. } => vec![key_i, by_i],
        Op::IntMapGetOr {
            key_i, default_i, ..
        } => vec![key_i, default_i],
        Op::IntMapInsert { key_i, value_i, .. } => vec![key_i, value_i],
        Op::IntMapContainsKey { key_i, .. } => vec![key_i],
        // CallI64 reads `argc` int args from `args_i..args_i+argc`.
        // Without this declared, the dead-load pass drops the
        // `LoadConstI64` / arith ops feeding the args, leaving the
        // call to read garbage from the int file.
        Op::CallI64 { args_i, argc, .. } | Op::CallTypedF64 { args_i, argc, .. } => {
            (0..argc).map(|i| args_i.saturating_add(i)).collect()
        }
        _ => vec![],
    }
}

#[allow(clippy::too_many_lines, reason = "exhaustive op match")]
fn op_int_writes(op: Op) -> Vec<Reg> {
    match op {
        Op::LoadConstI64 { dst_i, .. }
        | Op::AddI64 { dst_i, .. }
        | Op::SubI64 { dst_i, .. }
        | Op::MulI64 { dst_i, .. }
        | Op::DivI64 { dst_i, .. }
        | Op::RemI64 { dst_i, .. }
        | Op::BitAndI64 { dst_i, .. }
        | Op::BitOrI64 { dst_i, .. }
        | Op::BitXorI64 { dst_i, .. }
        | Op::ShlI64 { dst_i, .. }
        | Op::ShrI64 { dst_i, .. }
        | Op::NegI64 { dst_i, .. }
        | Op::UnboxI64 { dst_i, .. }
        | Op::MoveI64 { dst_i, .. }
        | Op::FloatToIntI64 { dst_i, .. }
        | Op::U8VecGetByte { dst_i, .. }
        | Op::IntArrayGetI64 { dst_i, .. }
        | Op::IntMapInc { dst_i, .. }
        | Op::IntMapGetOr { dst_i, .. }
        | Op::IntMapLen { dst_i, .. }
        | Op::CallI64 { dst_i, .. } => vec![dst_i],
        Op::ForLoopI64 { counter_i, .. }
        | Op::ForLoopInclusiveI64 { counter_i, .. }
        | Op::IncJumpIfLtI64 { counter_i, .. }
        | Op::IncJumpIfLeI64 { counter_i, .. } => {
            vec![counter_i]
        }
        _ => vec![],
    }
}

fn op_float_reads(op: Op) -> Vec<Reg> {
    match op {
        Op::AddF64 { lhs_f, rhs_f, .. }
        | Op::SubF64 { lhs_f, rhs_f, .. }
        | Op::MulF64 { lhs_f, rhs_f, .. }
        | Op::DivF64 { lhs_f, rhs_f, .. }
        | Op::LtF64 { lhs_f, rhs_f, .. }
        | Op::LeF64 { lhs_f, rhs_f, .. }
        | Op::GtF64 { lhs_f, rhs_f, .. }
        | Op::GeF64 { lhs_f, rhs_f, .. }
        | Op::EqF64 { lhs_f, rhs_f, .. }
        | Op::NeF64 { lhs_f, rhs_f, .. }
        | Op::BranchIfLtF64 { lhs_f, rhs_f, .. }
        | Op::BranchIfGeF64 { lhs_f, rhs_f, .. } => vec![lhs_f, rhs_f],
        Op::NegF64 { src_f, .. }
        | Op::SqrtF64 { src_f, .. }
        | Op::SinF64 { src_f, .. }
        | Op::CosF64 { src_f, .. }
        | Op::AbsF64 { src_f, .. }
        | Op::FloorF64 { src_f, .. }
        | Op::CeilF64 { src_f, .. }
        | Op::ExpF64 { src_f, .. }
        | Op::LnF64 { src_f, .. }
        | Op::BoxF64 { src_f, .. }
        | Op::MoveF64 { src_f, .. }
        | Op::FloatToIntI64 { src_f, .. } => vec![src_f],
        Op::MulAddF64 { a_f, b_f, c_f, .. } | Op::MulSubF64 { a_f, b_f, c_f, .. } => {
            vec![a_f, b_f, c_f]
        }
        Op::FloatVecSetF64 { value_f, .. }
        | Op::IndexedFieldSetF64 { value_f, .. }
        | Op::IndexedFieldSetF64ByOffset { value_f, .. }
        | Op::FlatSetF64 { value_f, .. } => vec![value_f],
        Op::BuildFloatVec { first_f, count, .. } => {
            (0..count).map(|i| first_f.saturating_add(i)).collect()
        }
        _ => vec![],
    }
}

fn op_float_writes(op: Op) -> Vec<Reg> {
    match op {
        Op::LoadConstF64 { dst_f, .. }
        | Op::AddF64 { dst_f, .. }
        | Op::SubF64 { dst_f, .. }
        | Op::MulF64 { dst_f, .. }
        | Op::DivF64 { dst_f, .. }
        | Op::NegF64 { dst_f, .. }
        | Op::SqrtF64 { dst_f, .. }
        | Op::SinF64 { dst_f, .. }
        | Op::CosF64 { dst_f, .. }
        | Op::AbsF64 { dst_f, .. }
        | Op::FloorF64 { dst_f, .. }
        | Op::CeilF64 { dst_f, .. }
        | Op::ExpF64 { dst_f, .. }
        | Op::LnF64 { dst_f, .. }
        | Op::UnboxF64 { dst_f, .. }
        | Op::IntToFloatF64 { dst_f, .. }
        | Op::MoveF64 { dst_f, .. }
        | Op::MulAddF64 { dst_f, .. }
        | Op::MulSubF64 { dst_f, .. }
        | Op::FieldGetF64 { dst_f, .. }
        | Op::IndexedFieldGetF64 { dst_f, .. }
        | Op::IndexedFieldGetF64ByOffset { dst_f, .. }
        | Op::FieldGetF64ByOffset { dst_f, .. }
        | Op::FlatGetF64 { dst_f, .. }
        | Op::FloatVecGetF64 { dst_f, .. }
        | Op::CallTypedF64 { dst_f, .. } => vec![dst_f],
        _ => vec![],
    }
}

#[allow(clippy::too_many_lines, reason = "exhaustive op match")]
fn op_value_reads(op: Op) -> Vec<Reg> {
    match op {
        Op::Move { src, .. } | Op::Deref { src, .. } => vec![src],
        Op::Eq { lhs, rhs, .. }
        | Op::Ne { lhs, rhs, .. }
        | Op::Lt { lhs, rhs, .. }
        | Op::Le { lhs, rhs, .. }
        | Op::Gt { lhs, rhs, .. }
        | Op::Ge { lhs, rhs, .. } => vec![lhs, rhs],
        Op::Neg { operand, .. } | Op::Not { operand, .. } => vec![operand],
        Op::AddInt { lhs, rhs, .. }
        | Op::SubInt { lhs, rhs, .. }
        | Op::MulInt { lhs, rhs, .. }
        | Op::DivInt { lhs, rhs, .. }
        | Op::RemInt { lhs, rhs, .. } => vec![lhs, rhs],
        Op::BranchIf { cond, .. } | Op::BranchIfNot { cond, .. } => vec![cond],
        Op::Return { value } => vec![value],
        Op::IndexGet { base, index, .. } => vec![base, index],
        Op::IndexSet { base, index, value } => vec![base, index, value],
        Op::FieldGet { receiver, .. } | Op::FieldGetF64 { receiver, .. } => vec![receiver],
        Op::FieldGetF64ByOffset { receiver, .. } => vec![receiver],
        Op::FieldSet {
            receiver, value, ..
        } => vec![receiver, value],
        Op::TupleIndex { receiver, .. } => vec![receiver],
        Op::IndexedFieldSet {
            base, index, value, ..
        } => vec![base, index, value],
        Op::IndexedFieldGet { base, index, .. }
        | Op::IndexedFieldGetF64 { base, index, .. }
        | Op::IndexedFieldGetF64ByOffset { base, index, .. }
        | Op::IndexedFieldSetF64 { base, index, .. }
        | Op::IndexedFieldSetF64ByOffset { base, index, .. } => vec![base, index],
        Op::FlatGetF64 { base, index, .. } | Op::FlatSetF64 { base, index, .. } => {
            vec![base, index]
        }
        Op::IsVariant { value, .. }
        | Op::VariantField { value, .. }
        | Op::IsStruct { value, .. } => {
            vec![value]
        }
        Op::TryRecv { chan, .. } => vec![chan],
        Op::UnboxF64 { src_v, .. } | Op::UnboxI64 { src_v, .. } => vec![src_v],
        // Call ops read the entire `args..args+argc` span as well
        // as `callee` / `receiver`. Without those reads declared,
        // the dead-load pass below drops `LoadConst` instructions
        // that feed into call arguments — turning every literal
        // arg into `Value::Void` at run time.
        Op::Call {
            callee, args, argc, ..
        } => {
            let mut v = Vec::with_capacity(1 + argc as usize);
            v.push(callee);
            for i in 0..argc {
                v.push(args.saturating_add(i));
            }
            v
        }
        Op::CallStatic { args, argc, .. } => (0..argc).map(|i| args.saturating_add(i)).collect(),
        Op::IntArraySwap { receiver, .. } => vec![receiver],
        Op::MethodCall {
            receiver,
            args,
            argc,
            ..
        } => {
            let mut v = Vec::with_capacity(1 + argc as usize);
            v.push(receiver);
            for i in 0..argc {
                v.push(args.saturating_add(i));
            }
            v
        }
        Op::SpawnMethod {
            receiver,
            args,
            argc,
            ..
        } => {
            let mut v = Vec::with_capacity(1 + argc as usize);
            v.push(receiver);
            for i in 0..argc {
                v.push(args.saturating_add(i));
            }
            v
        }
        Op::Spawn { callee, args, argc } => {
            let mut v = Vec::with_capacity(1 + argc as usize);
            v.push(callee);
            for i in 0..argc {
                v.push(args.saturating_add(i));
            }
            v
        }
        Op::StreamWriteByte {
            stream_reg,
            byte_reg,
            ..
        } => vec![stream_reg, byte_reg],
        Op::U8VecSetByte {
            u8vec_reg,
            idx_reg,
            byte_reg,
            ..
        } => vec![u8vec_reg, idx_reg, byte_reg],
        Op::U8VecGetByte {
            u8vec_reg, idx_reg, ..
        } => vec![u8vec_reg, idx_reg],
        Op::MapInc {
            map_reg,
            key_reg,
            by_reg,
            ..
        } => vec![map_reg, key_reg, by_reg],
        Op::IntMapInc { map_reg, .. }
        | Op::IntMapGetOr { map_reg, .. }
        | Op::IntMapInsert { map_reg, .. }
        | Op::IntMapLen { map_reg, .. }
        | Op::IntMapContainsKey { map_reg, .. } => vec![map_reg],
        Op::FloatVecGetF64 { base, .. }
        | Op::FloatVecSetF64 { base, .. }
        | Op::IntArrayGetI64 { base, .. } => vec![base],
        Op::StaticSet { src, .. } => vec![src],
        // BuildArray / BuildTuple read `first..first+count`
        // from the Value register file. Without these declared
        // the dead-load pass drops their feeding `LoadConst`
        // instructions and the array reads as `[Void; N]`.
        Op::BuildArray { first, count, .. } | Op::BuildTuple { first, count, .. } => {
            (0..count).map(|i| first.saturating_add(i)).collect()
        }
        _ => vec![],
    }
}

fn op_value_writes(op: Op) -> Vec<Reg> {
    match op {
        Op::LoadConst { dst, .. }
        | Op::LoadGlobal { dst, .. }
        | Op::StaticGet { dst, .. }
        | Op::Move { dst, .. }
        | Op::Deref { dst, .. }
        | Op::Eq { dst, .. }
        | Op::Ne { dst, .. }
        | Op::Lt { dst, .. }
        | Op::Le { dst, .. }
        | Op::Gt { dst, .. }
        | Op::Ge { dst, .. }
        | Op::Neg { dst, .. }
        | Op::Not { dst, .. }
        | Op::AddInt { dst, .. }
        | Op::SubInt { dst, .. }
        | Op::MulInt { dst, .. }
        | Op::DivInt { dst, .. }
        | Op::RemInt { dst, .. }
        | Op::BoxF64 { dst_v: dst, .. }
        | Op::BoxI64 { dst_v: dst, .. }
        | Op::BoxU64 { dst_v: dst, .. }
        | Op::Call { dst, .. }
        | Op::CallStatic { dst, .. }
        | Op::MethodCall { dst, .. }
        | Op::FieldGet { dst, .. }
        | Op::TupleIndex { dst, .. }
        | Op::IndexGet { dst, .. }
        | Op::IndexedFieldGet { dst, .. }
        | Op::IsVariant { dst, .. }
        | Op::IsStruct { dst, .. }
        | Op::VariantField { dst, .. }
        | Op::BuildIntArray { dst_v: dst, .. }
        | Op::BuildArray { dst, .. }
        | Op::BuildTuple { dst, .. }
        | Op::BuildFloatVec { dst_v: dst, .. }
        | Op::BuildIntMap { dst_v: dst, .. }
        | Op::IntMapInsert { dst_v: dst, .. }
        | Op::IntMapContainsKey { dst_v: dst, .. }
        | Op::TryRecv { dst, .. }
        | Op::StreamWriteByte { dst, .. }
        | Op::U8VecSetByte { dst, .. }
        | Op::MapInc { dst, .. } => vec![dst],
        Op::LtI64 { dst_v, .. }
        | Op::LeI64 { dst_v, .. }
        | Op::GtI64 { dst_v, .. }
        | Op::GeI64 { dst_v, .. }
        | Op::EqI64 { dst_v, .. }
        | Op::NeI64 { dst_v, .. }
        | Op::LtF64 { dst_v, .. }
        | Op::LeF64 { dst_v, .. }
        | Op::GtF64 { dst_v, .. }
        | Op::GeF64 { dst_v, .. }
        | Op::EqF64 { dst_v, .. }
        | Op::NeF64 { dst_v, .. } => vec![dst_v],
        _ => vec![],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;

    #[test]
    fn fuses_addi64_into_movei64_dst() {
        // ints[0] = ints[1] + ints[2]; ints[3] = ints[0] (move)
        let mut instrs = vec![
            Op::AddI64 {
                dst_i: 0,
                lhs_i: 1,
                rhs_i: 2,
            },
            Op::MoveI64 { dst_i: 3, src_i: 0 },
        ];
        run(&mut instrs, 0);
        assert_eq!(instrs.len(), 1);
        assert!(matches!(
            instrs[0],
            Op::AddI64 {
                dst_i: 3,
                lhs_i: 1,
                rhs_i: 2
            }
        ));
    }

    #[test]
    fn refuses_when_temp_is_read_twice() {
        // ints[0] = ints[1] + ints[2]; ints[3] = ints[0]; ints[4] = ints[0]
        let mut instrs = vec![
            Op::AddI64 {
                dst_i: 0,
                lhs_i: 1,
                rhs_i: 2,
            },
            Op::MoveI64 { dst_i: 3, src_i: 0 },
            Op::MoveI64 { dst_i: 4, src_i: 0 },
        ];
        run(&mut instrs, 0);
        // No fusion — two readers means dropping the first move
        // would leave the second reading from a dst that the
        // `MoveI64` writer no longer fills.
        assert_eq!(instrs.len(), 3);
    }

    #[test]
    fn refuses_fusion_across_jump_target() {
        // jump lands on the move — fusion would skip the writer.
        let mut instrs = vec![
            Op::Jump { target: 2 },
            Op::AddI64 {
                dst_i: 0,
                lhs_i: 1,
                rhs_i: 2,
            },
            Op::MoveI64 { dst_i: 3, src_i: 0 },
        ];
        run(&mut instrs, 0);
        assert_eq!(instrs.len(), 3);
        assert!(matches!(instrs[2], Op::MoveI64 { dst_i: 3, src_i: 0 }));
    }

    #[test]
    fn patches_jump_targets_after_fusion() {
        // Two fused pairs followed by a jump-to-end. After
        // fusion, `target=4` (the would-be slot past both moves)
        // collapses to slot 2.
        let mut instrs = vec![
            Op::AddI64 {
                dst_i: 0,
                lhs_i: 1,
                rhs_i: 2,
            },
            Op::MoveI64 { dst_i: 3, src_i: 0 },
            Op::AddI64 {
                dst_i: 5,
                lhs_i: 6,
                rhs_i: 7,
            },
            Op::MoveI64 { dst_i: 8, src_i: 5 },
            Op::Jump { target: 4 }, // points past everything
        ];
        run(&mut instrs, 0);
        assert_eq!(instrs.len(), 3);
        assert!(matches!(instrs[2], Op::Jump { target: 2 }));
    }
}
