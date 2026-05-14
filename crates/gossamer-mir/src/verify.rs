//! Structural invariant checker for [`Body`].
//!
//! The verifier runs after every MIR-level pass behind a
//! `debug_assertions` gate (`verify_body`). It catches the class
//! of bug where an optimisation rewrites the CFG and leaves
//! something dangling: a `BlockId` that no longer exists, a
//! `Local` index past the end of the locals table, a block whose
//! `id` field disagrees with its position in `body.blocks`. The
//! copy-prop aggregate-aliasing miscompile that shipped in 0.4
//! is exactly this class — `verify_body` would have caught it at
//! the point the rewrite happened, not at codegen time when the
//! produced object segfaulted under user code.
//!
//! The checks are intentionally cheap: the verifier walks every
//! block once, every statement once, every operand once. No
//! type-level reasoning, no exhaustiveness logic. The contract is
//! "the produced `Body` is well-formed enough that the codegen
//! crates can lower it without panicking on out-of-range
//! indices."

#![forbid(unsafe_code)]

use crate::ir::{
    BasicBlock, BlockId, Body, Local, Operand, Place, Projection, Rvalue, StatementKind, Terminator,
};

/// Single failure recorded by [`verify_body`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VerifyError {
    /// `body.blocks` is empty. Every function has at least an
    /// entry block holding `Return`.
    EmptyBlocks {
        /// Function whose blocks vector was empty.
        body: String,
    },
    /// A block's stored `id` doesn't match its position in
    /// `body.blocks`. Optimisation passes that splice or
    /// reorder blocks must keep this invariant.
    BlockIdMismatch {
        /// Function name.
        body: String,
        /// Position in `body.blocks`.
        position: u32,
        /// Id stored on the block.
        stored: BlockId,
    },
    /// A `BlockId` referenced by a terminator points past the end
    /// of `body.blocks`.
    BlockOutOfRange {
        /// Function name.
        body: String,
        /// Block whose terminator referenced an invalid target.
        block: BlockId,
        /// Bad target id.
        target: BlockId,
        /// Length of `body.blocks` at audit time.
        n_blocks: u32,
    },
    /// A `Local` referenced by a statement, terminator, or place
    /// projection points past the end of `body.locals`.
    LocalOutOfRange {
        /// Function name.
        body: String,
        /// Block containing the offending reference.
        block: BlockId,
        /// Bad local id.
        local: Local,
        /// Length of `body.locals` at audit time.
        n_locals: u32,
    },
    /// A `Call::target = Some(t)` references a block beyond the
    /// CFG. Out-of-range call targets are flagged via
    /// [`VerifyError::BlockOutOfRange`] from the same pass.
    CallTargetMissing {
        /// Function name.
        body: String,
        /// Block whose call had no continuation.
        block: BlockId,
    },
}

/// Walks `body` and accumulates every structural invariant
/// violation. Returns `Ok(())` when the body is well-formed.
///
/// The verifier is allocation-free in the common (well-formed)
/// case: errors are collected into a single `Vec<VerifyError>`
/// which stays empty for clean inputs. The result is an
/// `Err(Vec<...>)` so callers can dump every error at once
/// instead of one-shot fail-fast.
///
/// # Examples
///
/// Constructs a trivial single-block body that just `return`s and
/// runs it through the verifier. Most callers operate on bodies
/// produced by `lower_program`; this shape is what the verifier
/// expects as the minimum well-formed CFG (one block, in-range
/// locals, a `Return` terminator).
///
/// ```rust
/// use gossamer_lex::SourceMap;
/// use gossamer_mir::verify::verify_body;
/// use gossamer_mir::{BasicBlock, BlockId, Body, LocalDecl, Terminator};
/// use gossamer_types::{IntTy, TyCtxt};
///
/// let mut sources = SourceMap::new();
/// let file = sources.add_file("doc.gos", String::new());
/// let span = gossamer_lex::Span::new(file, 0, 0);
///
/// let mut tcx = TyCtxt::new();
/// let i64_ty = tcx.int_ty(IntTy::I64);
///
/// let body = Body {
///     name: "id".to_string(),
///     def: None,
///     arity: 1,
///     locals: vec![
///         LocalDecl { ty: i64_ty, debug_name: None, mutable: false },
///         LocalDecl { ty: i64_ty, debug_name: None, mutable: false },
///     ],
///     blocks: vec![BasicBlock {
///         id: BlockId::ENTRY,
///         stmts: Vec::new(),
///         terminator: Terminator::Return,
///         span,
///     }],
///     span,
/// };
///
/// verify_body(&body).expect("MIR verifier rejected body");
/// ```
pub fn verify_body(body: &Body) -> Result<(), Vec<VerifyError>> {
    let mut errors: Vec<VerifyError> = Vec::new();
    let n_blocks: u32 = body.blocks.len().try_into().unwrap_or(u32::MAX);
    let n_locals: u32 = body.locals.len().try_into().unwrap_or(u32::MAX);
    if body.blocks.is_empty() {
        errors.push(VerifyError::EmptyBlocks {
            body: body.name.clone(),
        });
        // No blocks to walk. Surface the single error and stop;
        // the rest of the pass would unconditionally underflow.
        return Err(errors);
    }
    for (position, block) in body.blocks.iter().enumerate() {
        let position = position as u32;
        if block.id.0 != position {
            errors.push(VerifyError::BlockIdMismatch {
                body: body.name.clone(),
                position,
                stored: block.id,
            });
        }
        check_block(body, block, n_blocks, n_locals, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// Walks one block, collecting invariant violations into
/// `errors`.
fn check_block(
    body: &Body,
    block: &BasicBlock,
    n_blocks: u32,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    for stmt in &block.stmts {
        check_statement(body, block.id, &stmt.kind, n_locals, errors);
    }
    check_terminator(
        body,
        block.id,
        &block.terminator,
        n_blocks,
        n_locals,
        errors,
    );
}

fn check_statement(
    body: &Body,
    block: BlockId,
    kind: &StatementKind,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    match kind {
        StatementKind::Assign { place, rvalue } => {
            check_place(body, block, place, n_locals, errors);
            check_rvalue(body, block, rvalue, n_locals, errors);
        }
        StatementKind::StorageLive(local) | StatementKind::StorageDead(local) => {
            check_local(body, block, *local, n_locals, errors);
        }
        StatementKind::SetDiscriminant { place, .. } => {
            check_place(body, block, place, n_locals, errors);
        }
        StatementKind::GcWriteBarrier { place, value } => {
            check_place(body, block, place, n_locals, errors);
            check_operand(body, block, value, n_locals, errors);
        }
        StatementKind::Nop => {}
    }
}

fn check_terminator(
    body: &Body,
    block: BlockId,
    term: &Terminator,
    n_blocks: u32,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    match term {
        Terminator::Goto { target } => check_block_id(body, block, *target, n_blocks, errors),
        Terminator::SwitchInt {
            discriminant,
            arms,
            default,
        } => {
            check_operand(body, block, discriminant, n_locals, errors);
            for (_value, target) in arms {
                check_block_id(body, block, *target, n_blocks, errors);
            }
            check_block_id(body, block, *default, n_blocks, errors);
        }
        Terminator::Return => {}
        Terminator::Call {
            callee,
            args,
            destination,
            target,
        } => {
            check_operand(body, block, callee, n_locals, errors);
            for arg in args {
                check_operand(body, block, arg, n_locals, errors);
            }
            check_place(body, block, destination, n_locals, errors);
            match target {
                Some(t) => check_block_id(body, block, *t, n_blocks, errors),
                None => {
                    // Diverging call: no continuation required.
                    // Surface only if a future revision tightens the
                    // shape. Today this is well-formed.
                    let _ = block;
                }
            }
        }
        Terminator::Assert { cond, target, .. } => {
            check_operand(body, block, cond, n_locals, errors);
            check_block_id(body, block, *target, n_blocks, errors);
        }
        Terminator::Unreachable | Terminator::Panic { .. } => {}
        Terminator::Drop { place, target } => {
            check_place(body, block, place, n_locals, errors);
            check_block_id(body, block, *target, n_blocks, errors);
        }
    }
}

fn check_rvalue(
    body: &Body,
    block: BlockId,
    rvalue: &Rvalue,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    // Exhaustive over the public enum.
    match rvalue {
        Rvalue::Use(op) => check_operand(body, block, op, n_locals, errors),
        Rvalue::Ref { place, .. } => check_place(body, block, place, n_locals, errors),
        Rvalue::BinaryOp { lhs, rhs, .. } => {
            check_operand(body, block, lhs, n_locals, errors);
            check_operand(body, block, rhs, n_locals, errors);
        }
        Rvalue::UnaryOp { operand, .. } => check_operand(body, block, operand, n_locals, errors),
        Rvalue::Cast { operand, .. } => check_operand(body, block, operand, n_locals, errors),
        Rvalue::Aggregate { operands, .. } => {
            for op in operands {
                check_operand(body, block, op, n_locals, errors);
            }
        }
        Rvalue::Len(place) => check_place(body, block, place, n_locals, errors),
        Rvalue::Repeat { value, .. } => check_operand(body, block, value, n_locals, errors),
        Rvalue::CallIntrinsic { args, .. } => {
            for op in args {
                check_operand(body, block, op, n_locals, errors);
            }
        }
    }
}

fn check_operand(
    body: &Body,
    block: BlockId,
    operand: &Operand,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    match operand {
        Operand::Copy(place) => {
            check_place(body, block, place, n_locals, errors);
        }
        Operand::Const(_) | Operand::FnRef { .. } => {}
    }
}

fn check_place(
    body: &Body,
    block: BlockId,
    place: &Place,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    check_local(body, block, place.local, n_locals, errors);
    for proj in &place.projection {
        if let Projection::Index(local) = proj {
            check_local(body, block, *local, n_locals, errors);
        }
    }
}

fn check_local(
    body: &Body,
    block: BlockId,
    local: Local,
    n_locals: u32,
    errors: &mut Vec<VerifyError>,
) {
    if local.0 >= n_locals {
        errors.push(VerifyError::LocalOutOfRange {
            body: body.name.clone(),
            block,
            local,
            n_locals,
        });
    }
}

fn check_block_id(
    body: &Body,
    block: BlockId,
    target: BlockId,
    n_blocks: u32,
    errors: &mut Vec<VerifyError>,
) {
    if target.0 >= n_blocks {
        errors.push(VerifyError::BlockOutOfRange {
            body: body.name.clone(),
            block,
            target,
            n_blocks,
        });
    }
}

/// Convenience: panics if `verify_body` returns errors. Intended
/// for `debug_assertions`-only call sites after passes.
///
/// # Panics
///
/// Panics with the rendered error list when verification fails.
pub fn debug_verify_body(body: &Body) {
    if !cfg!(debug_assertions) {
        return;
    }
    if let Err(errors) = verify_body(body) {
        panic!(
            "gossamer-mir verifier rejected `{}`:\n{}",
            body.name,
            errors
                .iter()
                .map(|e| format!("  {e:?}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
    }
}
