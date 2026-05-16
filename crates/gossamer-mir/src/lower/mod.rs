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
//! HIR → MIR lowering.
//! Produces a [`Body`] per HIR function. The lowerer is intentionally
//! straightforward: every HIR expression of interest becomes either a
//! sequence of [`StatementKind::Assign`]s targeting fresh temporaries
//! or a [`Terminator`] that closes the current block. Control flow
//! (`if`, `while`, `loop`, `match`) drops into the CFG by allocating
//! join blocks and stitching them with [`Terminator::Goto`] /
//! [`Terminator::SwitchInt`].

#![forbid(unsafe_code)]
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

/// Lowers every function in `program` to a MIR [`Body`].
#[must_use]
/// Calling-convention shape of a `r.get(pattern, handler)` handler value
/// at the runtime ABI boundary. Returned by [`BodyBuilder::emit_router_handler_abi`]
/// so the caller can pick between the env-carrying and bare entry points.
pub(crate) enum RouterHandlerAbi {
    /// Top-level `fn(http::Request) -> Result<...>` value. Caller routes
    /// to the `_fn` runtime symbol and passes only `fn_addr`.
    Bare(Operand),
    /// Struct or closure value with a `(env, req) -> Result<...>` ABI.
    /// Caller keeps the original runtime symbol and pushes both operands.
    WithEnv {
        /// The handler env operand (struct value pointer or closure env).
        env: Operand,
        /// The handler `fn_addr` operand (`gos_fn_addr("…::serve")`).
        fn_addr: Operand,
    },
}

/// Lower an entire HIR program to MIR `Body`s, one per top-level function.
pub fn lower_program(program: &HirProgram, tcx: &mut TyCtxt) -> Vec<Body> {
    let (structs, struct_defs) = collect_struct_fields(program);
    let enums = collect_enum_variants(program);
    let impl_methods = collect_impl_methods(program);
    let fn_returns = collect_fn_returns(program);
    let fn_inputs = collect_fn_inputs(program);
    let consts = collect_const_values(program);
    let mut bodies = Vec::new();
    for item in &program.items {
        collect_item(
            item,
            tcx,
            &structs,
            &struct_defs,
            &enums,
            &impl_methods,
            &fn_returns,
            &fn_inputs,
            &consts,
            &mut bodies,
        );
    }
    for body in &mut bodies {
        insert_drops_at_returns(body, tcx);
    }
    #[cfg(debug_assertions)]
    crate::verify::debug_verify_program(&bodies, tcx);
    bodies
}

pub mod builder;
pub mod helpers;

pub(crate) use builder::Builder;
pub use helpers::*;
