//! Bytecode chunk validator.
//!
//! The VM's dispatch loop relies on a "compiler always emits in-bounds
//! register / const / jump indices" invariant to skip per-op bounds
//! checks (see the unsafe-block doc in [`crate::vm`]). When that
//! invariant is silently broken by a compile.rs regression, the
//! result is UB rather than a clean panic. [`validate_chunk`] runs at
//! [`Vm::load`](crate::vm::Vm::load) time under `debug_assertions` so
//! malformed bytecode surfaces as a clear `RuntimeError` instead of a
//! segfault.
//!
//! Release builds skip validation entirely - the goal is to catch
//! compiler regressions during development; production execution
//! still trusts the unverified invariant for speed.

use std::fmt;

use crate::bytecode::{FnChunk, Op, Reg, WideOp};

/// Diagnostics from [`validate_chunk`]. Each variant points to the
/// offending instruction by linear index plus the specific bound it
/// violated.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(
    clippy::enum_variant_names,
    reason = "every violation is shape `XOutOfBounds`; the prefix is the discriminator"
)]
pub(crate) enum ValidationError {
    /// A jump-shaped op targets an instruction past the end of the
    /// chunk's instruction stream.
    PcOutOfBounds {
        /// Index of the offending op within `chunk.instrs`.
        op_idx: usize,
        /// Target instruction index named by the op.
        target: usize,
        /// Number of instructions actually in the chunk.
        instr_count: usize,
    },
    /// A register-shaped operand exceeds the chunk's declared
    /// register-file size for its file (value / f64 / i64).
    RegisterOutOfBounds {
        /// Index of the offending op within `chunk.instrs`.
        op_idx: usize,
        /// Operand register number.
        reg: u32,
        /// Declared size of the register file the operand targets.
        count: u32,
        /// Register-file kind (for diagnostics).
        file: RegFile,
    },
    /// A constant-pool index is out of range for the targeted pool
    /// (boxed value pool / f64 pool / i64 pool / globals / deferred
    /// expressions / wide-ops side table).
    ConstantOutOfBounds {
        /// Index of the offending op within `chunk.instrs`.
        op_idx: usize,
        /// The index named by the op.
        idx: u32,
        /// Length of the targeted pool.
        len: usize,
        /// Pool kind (for diagnostics).
        pool: PoolKind,
    },
}

/// Register-file discriminator for [`ValidationError::RegisterOutOfBounds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RegFile {
    /// Boxed `Value` register file.
    Value,
    /// Unboxed `f64` register file.
    Float,
    /// Unboxed `i64` register file.
    Int,
}

impl fmt::Display for RegFile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Value => f.write_str("value"),
            Self::Float => f.write_str("f64"),
            Self::Int => f.write_str("i64"),
        }
    }
}

/// Constant-pool discriminator for [`ValidationError::ConstantOutOfBounds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PoolKind {
    /// Boxed value constant pool (`consts`).
    Consts,
    /// `f64` constant pool (`f64_consts`).
    F64Consts,
    /// `i64` constant pool (`i64_consts`).
    I64Consts,
    /// Global-name table (`globals`).
    Globals,
    /// Wide-op side table (`wide_ops`).
    WideOps,
    /// Closure-proto table (`closure_protos`).
    ClosureProtos,
    /// `select` arm metadata table (`select_arms`).
    SelectArms,
}

impl fmt::Display for PoolKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Consts => f.write_str("consts"),
            Self::F64Consts => f.write_str("f64_consts"),
            Self::I64Consts => f.write_str("i64_consts"),
            Self::Globals => f.write_str("globals"),
            Self::WideOps => f.write_str("wide_ops"),
            Self::ClosureProtos => f.write_str("closure_protos"),
            Self::SelectArms => f.write_str("select_arms"),
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::PcOutOfBounds {
                op_idx,
                target,
                instr_count,
            } => write!(
                f,
                "bytecode validator: op #{op_idx} jumps to {target}, but chunk has {instr_count} instructions"
            ),
            Self::RegisterOutOfBounds {
                op_idx,
                reg,
                count,
                file,
            } => write!(
                f,
                "bytecode validator: op #{op_idx} references {file} register {reg}, but chunk declares {count}"
            ),
            Self::ConstantOutOfBounds {
                op_idx,
                idx,
                len,
                pool,
            } => write!(
                f,
                "bytecode validator: op #{op_idx} references {pool}[{idx}], but pool has length {len}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// Walks every instruction in `chunk` and confirms every register,
/// constant-pool, and jump target stays within the chunk's declared
/// counts. Returns the first violation; surrounding callers report it
/// as a `RuntimeError` so the failure is observable rather than
/// surfacing later as UB inside the dispatch loop.
#[allow(
    clippy::too_many_lines,
    clippy::similar_names,
    reason = "one giant match over every Op variant - splitting would harm readability of the validator's per-op invariants"
)]
pub(crate) fn validate_chunk(chunk: &FnChunk) -> Result<(), ValidationError> {
    let instr_count = chunk.instrs.len();
    let v_count = u32::from(chunk.register_count);
    let f_count = u32::from(chunk.float_count);
    let i_count = u32::from(chunk.int_count);
    let consts_len = chunk.consts.len();
    let f_consts_len = chunk.f64_consts.len();
    let i_consts_len = chunk.i64_consts.len();
    let globals_len = chunk.globals.len();
    let shape_names_len = chunk.shape_names.len();
    let closure_protos_len = chunk.closure_protos.len();
    let select_arms_len = chunk.select_arms.len();
    let wide_len = chunk.wide_ops.len();

    let check_v = |op_idx: usize, r: Reg| -> Result<(), ValidationError> {
        let r = u32::from(r);
        if r >= v_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: r,
                count: v_count,
                file: RegFile::Value,
            });
        }
        Ok(())
    };
    let check_f = |op_idx: usize, r: Reg| -> Result<(), ValidationError> {
        let r = u32::from(r);
        if r >= f_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: r,
                count: f_count,
                file: RegFile::Float,
            });
        }
        Ok(())
    };
    let check_i = |op_idx: usize, r: Reg| -> Result<(), ValidationError> {
        let r = u32::from(r);
        if r >= i_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: r,
                count: i_count,
                file: RegFile::Int,
            });
        }
        Ok(())
    };
    let check_v_span = |op_idx: usize, first: Reg, n: u16| -> Result<(), ValidationError> {
        if n == 0 {
            return Ok(());
        }
        let last = u32::from(first).saturating_add(u32::from(n) - 1);
        if last >= v_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: last,
                count: v_count,
                file: RegFile::Value,
            });
        }
        Ok(())
    };
    let check_i_span = |op_idx: usize, first: Reg, n: u16| -> Result<(), ValidationError> {
        if n == 0 {
            return Ok(());
        }
        let last = u32::from(first).saturating_add(u32::from(n) - 1);
        if last >= i_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: last,
                count: i_count,
                file: RegFile::Int,
            });
        }
        Ok(())
    };
    let check_f_span = |op_idx: usize, first: Reg, n: u32| -> Result<(), ValidationError> {
        if n == 0 {
            return Ok(());
        }
        let last = u32::from(first).saturating_add(n - 1);
        if last >= f_count {
            return Err(ValidationError::RegisterOutOfBounds {
                op_idx,
                reg: last,
                count: f_count,
                file: RegFile::Float,
            });
        }
        Ok(())
    };
    let check_target = |op_idx: usize, target: u32| -> Result<(), ValidationError> {
        if (target as usize) >= instr_count {
            return Err(ValidationError::PcOutOfBounds {
                op_idx,
                target: target as usize,
                instr_count,
            });
        }
        Ok(())
    };
    let check_pool =
        |op_idx: usize, idx: u32, len: usize, pool: PoolKind| -> Result<(), ValidationError> {
            if (idx as usize) >= len {
                return Err(ValidationError::ConstantOutOfBounds {
                    op_idx,
                    idx,
                    len,
                    pool,
                });
            }
            Ok(())
        };

    for (op_idx, op) in chunk.instrs.iter().enumerate() {
        match *op {
            // Loads / moves.
            Op::LoadConst { dst, idx } => {
                check_v(op_idx, dst)?;
                check_pool(op_idx, u32::from(idx), consts_len, PoolKind::Consts)?;
            }
            Op::LoadGlobal { dst, idx } => {
                check_v(op_idx, dst)?;
                check_pool(op_idx, u32::from(idx), globals_len, PoolKind::Globals)?;
            }
            Op::StoreStatic { name_idx, src } => {
                check_v(op_idx, src)?;
                check_pool(op_idx, u32::from(name_idx), globals_len, PoolKind::Globals)?;
            }
            Op::Move { dst, src } | Op::Deref { dst, src } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
            }

            // Adaptive arith - boxed value lhs/rhs/dst.
            Op::AddInt { dst, lhs, rhs, .. }
            | Op::SubInt { dst, lhs, rhs, .. }
            | Op::MulInt { dst, lhs, rhs, .. }
            | Op::DivInt { dst, lhs, rhs, .. }
            | Op::RemInt { dst, lhs, rhs, .. } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, lhs)?;
                check_v(op_idx, rhs)?;
            }
            Op::Neg { dst, operand } | Op::Not { dst, operand } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, operand)?;
            }
            Op::Eq { dst, lhs, rhs }
            | Op::Ne { dst, lhs, rhs }
            | Op::Lt { dst, lhs, rhs }
            | Op::Le { dst, lhs, rhs }
            | Op::Gt { dst, lhs, rhs }
            | Op::Ge { dst, lhs, rhs } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, lhs)?;
                check_v(op_idx, rhs)?;
            }

            // Jumps.
            Op::Jump { target } => check_target(op_idx, target)?,
            Op::BranchIf { cond, target } | Op::BranchIfNot { cond, target } => {
                check_v(op_idx, cond)?;
                check_target(op_idx, target)?;
            }

            // Calls.
            Op::Call {
                dst,
                callee,
                args,
                argc,
                ..
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, callee)?;
                check_v_span(op_idx, args, argc)?;
            }
            Op::Return { value } => check_v(op_idx, value)?,
            Op::ReturnUnit => {}
            Op::MakeClosure { dst, proto } => {
                check_v(op_idx, dst)?;
                check_pool(op_idx, proto, closure_protos_len, PoolKind::ClosureProtos)?;
            }
            Op::Select { first, count } => {
                if count > 0 {
                    let last = first.saturating_add(u32::from(count) - 1);
                    check_pool(op_idx, last, select_arms_len, PoolKind::SelectArms)?;
                    let start = first as usize;
                    for arm in &chunk.select_arms[start..start + count as usize] {
                        check_target(op_idx, arm.body_block)?;
                        match arm.kind {
                            crate::bytecode::SelectArmKind::Recv => {
                                check_v(op_idx, arm.channel_reg)?;
                                check_v(op_idx, arm.bind_reg)?;
                            }
                            crate::bytecode::SelectArmKind::Send => {
                                check_v(op_idx, arm.channel_reg)?;
                                check_v(op_idx, arm.value_reg)?;
                            }
                            crate::bytecode::SelectArmKind::Default => {}
                        }
                    }
                }
            }
            // `slot` indexes the process-global coverage table, not a
            // per-chunk pool, so there is nothing chunk-local to bound.
            Op::CovHit { .. } => {}
            Op::MethodCall {
                dst,
                receiver,
                name_idx,
                args,
                argc,
                ..
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, receiver)?;
                check_pool(op_idx, u32::from(name_idx), globals_len, PoolKind::Globals)?;
                check_v_span(op_idx, args, argc)?;
            }

            // Fused super-instructions over Value registers.
            Op::StreamWriteByte {
                dst,
                stream_reg,
                byte_reg,
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, stream_reg)?;
                check_v(op_idx, byte_reg)?;
            }
            Op::U8VecSetByte {
                dst,
                u8vec_reg,
                idx_reg,
                byte_reg,
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, u8vec_reg)?;
                check_v(op_idx, idx_reg)?;
                check_v(op_idx, byte_reg)?;
            }
            Op::U8VecGetByte {
                dst_i,
                u8vec_reg,
                idx_reg,
            } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, u8vec_reg)?;
                check_v(op_idx, idx_reg)?;
            }
            Op::MapInc {
                dst,
                map_reg,
                key_reg,
                by_reg,
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, map_reg)?;
                check_v(op_idx, key_reg)?;
                check_v(op_idx, by_reg)?;
            }
            Op::Wide { idx } => {
                check_pool(op_idx, u32::from(idx), wide_len, PoolKind::WideOps)?;
                validate_wide_op(
                    op_idx,
                    &chunk.wide_ops[idx as usize],
                    &check_v,
                    &check_f,
                    consts_len,
                    chunk,
                )?;
            }

            Op::BuildIntArray {
                dst_v,
                first_i,
                count,
            } => {
                check_v(op_idx, dst_v)?;
                check_i_span(op_idx, first_i, count)?;
            }
            Op::BuildTuple { dst, first, count } => {
                check_v(op_idx, dst)?;
                check_v_span(op_idx, first, count)?;
            }
            Op::BuildArray { dst, first, count } => {
                check_v(op_idx, dst)?;
                check_v_span(op_idx, first, count)?;
            }
            Op::BuildArrayRepeat { dst, value, count } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, value)?;
                check_v(op_idx, count)?;
            }
            Op::BuildRange {
                dst, start, end, ..
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, start)?;
                check_v(op_idx, end)?;
            }
            Op::IntToFloatF64 { dst_f, src_i } => {
                check_f(op_idx, dst_f)?;
                check_i(op_idx, src_i)?;
            }
            Op::FloatToIntI64 { dst_i, src_f } => {
                check_i(op_idx, dst_i)?;
                check_f(op_idx, src_f)?;
            }
            Op::TruncCastI64 { dst_i, src_i, .. } => {
                check_i(op_idx, dst_i)?;
                check_i(op_idx, src_i)?;
            }
            Op::CastScalar { dst, src, .. } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
            }
            Op::CellNew { dst, src } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
            }
            Op::CellTake { dst, cell } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, cell)?;
            }
            Op::IntArrayGetI64 {
                dst_i,
                base,
                index_i,
            } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
            }
            Op::IntArraySetI64 {
                base,
                index_i,
                value_i,
            } => {
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
                check_i(op_idx, value_i)?;
            }
            Op::IntArraySwap { base, i_i, j_i } | Op::FloatVecSwap { base, i_i, j_i } => {
                check_v(op_idx, base)?;
                check_i(op_idx, i_i)?;
                check_i(op_idx, j_i)?;
            }
            Op::BuildFloatVec {
                dst_v,
                first_f,
                count,
            } => {
                check_v(op_idx, dst_v)?;
                check_f_span(op_idx, first_f, u32::from(count))?;
            }
            Op::FloatVecGetF64 {
                dst_f,
                base,
                index_i,
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
            }
            Op::FloatVecSetF64 {
                base,
                index_i,
                value_f,
            } => {
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
                check_f(op_idx, value_f)?;
            }
            Op::BuildIntMap { dst_v } => check_v(op_idx, dst_v)?,
            Op::IntMapInc {
                dst_i,
                map_reg,
                key_i,
                by_i,
            } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, map_reg)?;
                check_i(op_idx, key_i)?;
                check_i(op_idx, by_i)?;
            }
            Op::IntMapGetOr {
                dst_i,
                map_reg,
                key_i,
                default_i,
            } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, map_reg)?;
                check_i(op_idx, key_i)?;
                check_i(op_idx, default_i)?;
            }
            Op::IntMapInsert {
                dst_v,
                map_reg,
                key_i,
                value_i,
            } => {
                check_v(op_idx, dst_v)?;
                check_v(op_idx, map_reg)?;
                check_i(op_idx, key_i)?;
                check_i(op_idx, value_i)?;
            }
            Op::IntMapLen { dst_i, map_reg } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, map_reg)?;
            }
            Op::IntMapContainsKey {
                dst_v,
                map_reg,
                key_i,
            } => {
                check_v(op_idx, dst_v)?;
                check_v(op_idx, map_reg)?;
                check_i(op_idx, key_i)?;
            }

            Op::Spawn { callee, args, argc } => {
                check_v(op_idx, callee)?;
                check_v_span(op_idx, args, argc)?;
            }
            Op::SpawnMethod {
                receiver,
                name_idx,
                args,
                argc,
            } => {
                check_v(op_idx, receiver)?;
                check_pool(op_idx, u32::from(name_idx), globals_len, PoolKind::Globals)?;
                check_v_span(op_idx, args, argc)?;
            }

            Op::IndexGet { dst, base, index } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
            }
            Op::IndexSet { base, index, value } => {
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_v(op_idx, value)?;
            }
            Op::FieldGet {
                dst,
                receiver,
                name_idx,
                ..
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, receiver)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
            }
            Op::FieldSet {
                receiver,
                name_idx,
                value,
            } => {
                check_v(op_idx, receiver)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
                check_v(op_idx, value)?;
            }
            Op::VecPush { receiver, value } => {
                check_v(op_idx, receiver)?;
                check_v(op_idx, value)?;
            }
            Op::VecPop { dst, receiver } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, receiver)?;
            }
            Op::VecInsert {
                receiver,
                index,
                value,
            } => {
                check_v(op_idx, receiver)?;
                check_v(op_idx, index)?;
                check_v(op_idx, value)?;
            }
            Op::VecRemove { receiver, index } => {
                check_v(op_idx, receiver)?;
                check_v(op_idx, index)?;
            }
            Op::TupleIndex { dst, receiver, .. } | Op::TupleTailIndex { dst, receiver, .. } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, receiver)?;
            }
            Op::IndexedFieldSet {
                base,
                index,
                name_idx,
                value,
            } => {
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
                check_v(op_idx, value)?;
            }

            // Unboxed f64 register-file ops.
            Op::LoadConstF64 { dst_f, idx } => {
                check_f(op_idx, dst_f)?;
                check_pool(op_idx, u32::from(idx), f_consts_len, PoolKind::F64Consts)?;
            }
            Op::AddF64 {
                dst_f,
                lhs_f,
                rhs_f,
            }
            | Op::SubF64 {
                dst_f,
                lhs_f,
                rhs_f,
            }
            | Op::MulF64 {
                dst_f,
                lhs_f,
                rhs_f,
            }
            | Op::DivF64 {
                dst_f,
                lhs_f,
                rhs_f,
            } => {
                check_f(op_idx, dst_f)?;
                check_f(op_idx, lhs_f)?;
                check_f(op_idx, rhs_f)?;
            }
            Op::NegF64 { dst_f, src_f }
            | Op::SqrtF64 { dst_f, src_f }
            | Op::SinF64 { dst_f, src_f }
            | Op::CosF64 { dst_f, src_f }
            | Op::AbsF64 { dst_f, src_f }
            | Op::FloorF64 { dst_f, src_f }
            | Op::CeilF64 { dst_f, src_f }
            | Op::ExpF64 { dst_f, src_f }
            | Op::LnF64 { dst_f, src_f }
            | Op::MoveF64 { dst_f, src_f } => {
                check_f(op_idx, dst_f)?;
                check_f(op_idx, src_f)?;
            }
            Op::LtF64 {
                dst_v,
                lhs_f,
                rhs_f,
            }
            | Op::LeF64 {
                dst_v,
                lhs_f,
                rhs_f,
            }
            | Op::GtF64 {
                dst_v,
                lhs_f,
                rhs_f,
            }
            | Op::GeF64 {
                dst_v,
                lhs_f,
                rhs_f,
            }
            | Op::EqF64 {
                dst_v,
                lhs_f,
                rhs_f,
            }
            | Op::NeF64 {
                dst_v,
                lhs_f,
                rhs_f,
            } => {
                check_v(op_idx, dst_v)?;
                check_f(op_idx, lhs_f)?;
                check_f(op_idx, rhs_f)?;
            }
            Op::UnboxF64 { dst_f, src_v } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, src_v)?;
            }
            Op::BoxF64 { dst_v, src_f } => {
                check_v(op_idx, dst_v)?;
                check_f(op_idx, src_f)?;
            }
            Op::MulAddF64 {
                dst_f,
                a_f,
                b_f,
                c_f,
            }
            | Op::MulSubF64 {
                dst_f,
                a_f,
                b_f,
                c_f,
            } => {
                check_f(op_idx, dst_f)?;
                check_f(op_idx, a_f)?;
                check_f(op_idx, b_f)?;
                check_f(op_idx, c_f)?;
            }

            // Unboxed i64 register-file ops.
            Op::LoadConstI64 { dst_i, idx } => {
                check_i(op_idx, dst_i)?;
                check_pool(op_idx, u32::from(idx), i_consts_len, PoolKind::I64Consts)?;
            }
            Op::AddI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::SubI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::MulI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::DivI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::RemI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::BitAndI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::BitOrI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::BitXorI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::ShlI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::ShrI64 {
                dst_i,
                lhs_i,
                rhs_i,
            }
            | Op::ShrU64 {
                dst_i,
                lhs_i,
                rhs_i,
            } => {
                check_i(op_idx, dst_i)?;
                check_i(op_idx, lhs_i)?;
                check_i(op_idx, rhs_i)?;
            }
            Op::NegI64 { dst_i, src_i } | Op::MoveI64 { dst_i, src_i } => {
                check_i(op_idx, dst_i)?;
                check_i(op_idx, src_i)?;
            }
            Op::LtI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::LeI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::GtI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::GeI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::EqI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::NeI64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::LtU64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::LeU64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::GtU64 {
                dst_v,
                lhs_i,
                rhs_i,
            }
            | Op::GeU64 {
                dst_v,
                lhs_i,
                rhs_i,
            } => {
                check_v(op_idx, dst_v)?;
                check_i(op_idx, lhs_i)?;
                check_i(op_idx, rhs_i)?;
            }
            Op::UnboxI64 { dst_i, src_v } => {
                check_i(op_idx, dst_i)?;
                check_v(op_idx, src_v)?;
            }
            Op::BoxI64 { dst_v, src_i } => {
                check_v(op_idx, dst_v)?;
                check_i(op_idx, src_i)?;
            }

            // Phase 2 fused / typed field access.
            Op::FieldGetF64 {
                dst_f,
                receiver,
                name_idx,
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, receiver)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
            }
            Op::IndexedFieldGet {
                dst,
                base,
                index,
                name_idx,
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
            }
            Op::IndexedFieldGetF64 {
                dst_f,
                base,
                index,
                name_idx,
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
            }
            Op::IndexedFieldSetF64 {
                base,
                index,
                name_idx,
                value_f,
            } => {
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_pool(op_idx, u32::from(name_idx), consts_len, PoolKind::Consts)?;
                check_f(op_idx, value_f)?;
            }
            Op::IndexedFieldGetF64ByOffset {
                dst_f, base, index, ..
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
            }
            Op::IndexedFieldSetF64ByOffset {
                base,
                index,
                value_f,
                ..
            } => {
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_f(op_idx, value_f)?;
            }

            // Fused compare-and-branch.
            Op::BranchIfLtI64 {
                lhs_i,
                rhs_i,
                target,
            }
            | Op::BranchIfGeI64 {
                lhs_i,
                rhs_i,
                target,
            }
            | Op::BranchIfGtI64 {
                lhs_i,
                rhs_i,
                target,
            } => {
                check_i(op_idx, lhs_i)?;
                check_i(op_idx, rhs_i)?;
                check_target(op_idx, target)?;
            }
            Op::BranchIfLtF64 {
                lhs_f,
                rhs_f,
                target,
            }
            | Op::BranchIfGeF64 {
                lhs_f,
                rhs_f,
                target,
            } => {
                check_f(op_idx, lhs_f)?;
                check_f(op_idx, rhs_f)?;
                check_target(op_idx, target)?;
            }
            Op::IncJumpIfLtI64 {
                counter_i,
                end_i,
                target,
            }
            | Op::IncJumpIfLeI64 {
                counter_i,
                end_i,
                target,
            } => {
                check_i(op_idx, counter_i)?;
                check_i(op_idx, end_i)?;
                check_target(op_idx, target)?;
            }

            Op::FieldGetF64ByOffset {
                dst_f, receiver, ..
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, receiver)?;
            }
            Op::FlatGetF64 {
                dst_f, base, index, ..
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
            }
            Op::FlatSetF64 {
                base,
                index,
                value_f,
                ..
            } => {
                check_v(op_idx, base)?;
                check_v(op_idx, index)?;
                check_f(op_idx, value_f)?;
            }
            Op::FlatGetF64I {
                dst_f,
                base,
                index_i,
                ..
            } => {
                check_f(op_idx, dst_f)?;
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
            }
            Op::FlatSetF64I {
                base,
                index_i,
                value_f,
                ..
            } => {
                check_v(op_idx, base)?;
                check_i(op_idx, index_i)?;
                check_f(op_idx, value_f)?;
            }
            Op::I64ToUint { dst_v, src_i } => {
                check_v(op_idx, dst_v)?;
                check_i(op_idx, src_i)?;
            }
            Op::VariantIs {
                dst, src, name_idx, ..
            } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
                check_pool(
                    op_idx,
                    u32::from(name_idx),
                    shape_names_len,
                    PoolKind::Consts,
                )?;
            }
            Op::VariantField { dst, src, .. } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
            }
            Op::StructIs { dst, src, name_idx } => {
                check_v(op_idx, dst)?;
                check_v(op_idx, src)?;
                check_pool(
                    op_idx,
                    u32::from(name_idx),
                    shape_names_len,
                    PoolKind::Consts,
                )?;
            }
        }
    }

    Ok(())
}

/// Helper for [`validate_chunk`] - descends into the wide-op side
/// table since `Op::Wide` only carries the side-table index.
fn validate_wide_op(
    op_idx: usize,
    wide: &WideOp,
    check_v: &dyn Fn(usize, Reg) -> Result<(), ValidationError>,
    check_f: &dyn Fn(usize, Reg) -> Result<(), ValidationError>,
    consts_len: usize,
    chunk: &FnChunk,
) -> Result<(), ValidationError> {
    let f_count = u32::from(chunk.float_count);
    match *wide {
        WideOp::MapIncAt {
            dst,
            map_reg,
            seq_reg,
            start_reg,
            len_reg,
            by_reg,
        } => {
            check_v(op_idx, dst)?;
            check_v(op_idx, map_reg)?;
            check_v(op_idx, seq_reg)?;
            check_v(op_idx, start_reg)?;
            check_v(op_idx, len_reg)?;
            check_v(op_idx, by_reg)?;
        }
        WideOp::BuildFloatArray {
            dst_v,
            name_idx,
            fields_idx,
            stride,
            elem_count,
            first_f,
        } => {
            check_v(op_idx, dst_v)?;
            if (u32::from(name_idx) as usize) >= consts_len {
                return Err(ValidationError::ConstantOutOfBounds {
                    op_idx,
                    idx: u32::from(name_idx),
                    len: consts_len,
                    pool: PoolKind::Consts,
                });
            }
            if (u32::from(fields_idx) as usize) >= consts_len {
                return Err(ValidationError::ConstantOutOfBounds {
                    op_idx,
                    idx: u32::from(fields_idx),
                    len: consts_len,
                    pool: PoolKind::Consts,
                });
            }
            let n = u32::from(stride).saturating_mul(u32::from(elem_count));
            if n > 0 {
                let last = u32::from(first_f).saturating_add(n - 1);
                if last >= f_count {
                    return Err(ValidationError::RegisterOutOfBounds {
                        op_idx,
                        reg: last,
                        count: f_count,
                        file: RegFile::Float,
                    });
                }
            }
            check_f(op_idx, first_f).ok();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bytecode::Op;
    use crate::value::Value;

    fn minimal_chunk() -> FnChunk {
        FnChunk {
            name: "test",
            arity: 0,
            register_count: 2,
            float_count: 1,
            int_count: 1,
            instrs: Vec::new(),
            wide_ops: Vec::new(),
            consts: vec![Value::Int(0)],
            f64_consts: vec![0.0],
            i64_consts: vec![0],
            globals: vec!["g".to_string()],
            shape_names: vec!["TestVariant"],
            call_cache_count: 0,
            arith_cache_count: 0,
            field_cache_count: 0,
            mut_ref_params: Vec::new(),
            closure_protos: Vec::new(),
            select_arms: Vec::new(),
        }
    }

    #[test]
    fn validate_chunk_accepts_well_formed_bytecode() {
        let mut chunk = minimal_chunk();
        chunk.instrs.push(Op::LoadConst { dst: 0, idx: 0 });
        chunk.instrs.push(Op::Move { dst: 1, src: 0 });
        chunk.instrs.push(Op::Jump { target: 0 });
        chunk.instrs.push(Op::Return { value: 1 });
        assert!(validate_chunk(&chunk).is_ok());
    }

    #[test]
    fn validate_chunk_rejects_out_of_range_register() {
        let mut chunk = minimal_chunk();
        chunk.instrs.push(Op::Move { dst: 99, src: 0 });
        let err = validate_chunk(&chunk).expect_err("must reject");
        match err {
            ValidationError::RegisterOutOfBounds {
                reg,
                file: RegFile::Value,
                ..
            } => assert_eq!(reg, 99),
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn validate_chunk_rejects_jump_past_end() {
        let mut chunk = minimal_chunk();
        chunk.instrs.push(Op::Jump { target: 42 });
        let err = validate_chunk(&chunk).expect_err("must reject");
        match err {
            ValidationError::PcOutOfBounds {
                target,
                instr_count,
                ..
            } => {
                assert_eq!(target, 42);
                assert_eq!(instr_count, 1);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }

    #[test]
    fn validate_chunk_rejects_constant_pool_overflow() {
        let mut chunk = minimal_chunk();
        chunk.instrs.push(Op::LoadConst { dst: 0, idx: 7 });
        let err = validate_chunk(&chunk).expect_err("must reject");
        match err {
            ValidationError::ConstantOutOfBounds {
                idx,
                len,
                pool: PoolKind::Consts,
                ..
            } => {
                assert_eq!(idx, 7);
                assert_eq!(len, 1);
            }
            other => panic!("unexpected error variant: {other:?}"),
        }
    }
}
