//! Mid-level SSA-lite IR.
//! The MIR sits between HIR and native-code generation. Its CFG-
//! oriented shape matches what Cranelift and LLVM want to consume, so
//! can translate MIR directly without a second
//! lowering pass. Each function becomes a [`Body`] holding typed
//! locals, basic blocks, and a terminator per block.

#![forbid(unsafe_code)]

mod cleanup;
mod escape;
mod ir;
mod lower;
mod monomorph;
mod opt;

pub use cleanup::{CleanupEntry, CleanupPlan, HEAP_ALLOCATOR_PAIRS, plan as plan_cleanup};
pub use escape::{EscapeSet, analyse as analyse_escape};
pub use ir::{
    AggregateKind, AssertMessage, BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl,
    Operand, Place, Projection, Rvalue, Statement, StatementKind, Terminator, UnOp,
};
pub use lower::lower_program;
pub use monomorph::{check_generic_layouts, mangled_name, monomorphise};
pub use opt::{
    const_branch_elim, const_fold, const_value_of, copy_propagate, dead_store_elim, optimise,
    statement_count,
};
