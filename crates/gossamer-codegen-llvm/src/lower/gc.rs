#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
#![forbid(unsafe_code)]
use super::*;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::BuildError;
use anyhow::Result;
use gossamer_abi as abi;
use gossamer_mir::{
    BasicBlock, BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

impl<'a> Lowerer<'a> {
    /// Computes the set of loop-header blocks for the current body
    /// (any block that is the target of a back-edge — a jump from
    /// a block whose id is >= the target's). Called once at the
    /// start of `lower` so `lower_block` can emit a safepoint at
    /// each header.
    pub(crate) fn compute_loop_headers(&mut self) {
        self.loop_headers.clear();
        for src in &self.body.blocks {
            let src_id = src.id.as_u32();
            match &src.terminator {
                gossamer_mir::Terminator::Goto { target } if target.as_u32() <= src_id => {
                    self.loop_headers.insert(target.as_u32());
                }
                gossamer_mir::Terminator::SwitchInt { arms, default, .. } => {
                    for (_, t) in arms {
                        if t.as_u32() <= src_id {
                            self.loop_headers.insert(t.as_u32());
                        }
                    }
                    if default.as_u32() <= src_id {
                        self.loop_headers.insert(default.as_u32());
                    }
                }
                gossamer_mir::Terminator::Call {
                    target: Some(t), ..
                } if t.as_u32() <= src_id => {
                    self.loop_headers.insert(t.as_u32());
                }
                gossamer_mir::Terminator::Assert { target, .. }
                | gossamer_mir::Terminator::Drop { target, .. }
                    if target.as_u32() <= src_id =>
                {
                    self.loop_headers.insert(target.as_u32());
                }
                _ => {}
            }
        }
    }

    /// Emits the function-prologue safepoint hook + raw-pointer
    /// shadow-stack save. The save's result is stored in a
    /// dedicated alloca'd i64 slot which the return path loads.
    /// Skipped for functions whose body can't allocate — the
    /// safepoint call is opaque to `opt -O3` and blocks inner-
    /// loop vectorisation. Pure leaf math functions are the
    /// hot-path victims (spectral-norm / n-body inner helpers
    /// are called > 10⁹ times).
    pub(crate) fn emit_gc_prologue(&mut self) {
        // The raw-pointer tracing GC has been replaced by intrusive
        // reference counting (see `gossamer-runtime` `c_abi::rc`). Its
        // per-call shadow-stack save/push/restore + safepoint hooks are
        // dead — no collector consumes the roots — but they cost an FFI
        // call (and block `opt -O3` inlining/vectorisation) on every
        // function invocation, which is catastrophic for deep recursion
        // and hot leaf math. Emit nothing.
        self.gc_prologue_emitted = false;
    }

    /// No-op: the tracing GC's shadow stack is retired (RC owns heap
    /// lifetime). Kept as a call site so the aggregate-assignment path
    /// stays unchanged.
    pub(crate) fn emit_gc_root_push(&mut self, _ptr_ssa: &str) {}

    /// No-op companion to [`Self::emit_gc_prologue`].
    pub(crate) fn emit_gc_root_restore(&mut self) {}
}
