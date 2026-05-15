//! GC safepoint elision analysis for codegen backends.
//!
//! The 0.6.0 tracing GC needs codegen to emit a function-prologue
//! safepoint hook + raw-pointer shadow-stack save/restore at every
//! function boundary so allocations in goroutines reach the
//! collector. The hook is cheap in isolation (atomic-load + compare)
//! but the *external function call* it lowers to is opaque to the
//! optimiser — `opt -O3` refuses to vectorise inner loops past
//! one and the prologue overhead dwarfs the body of a pure leaf
//! math function (`fn mat_a(i, j) -> f64 = 1.0 / ...`).
//!
//! Eliding the hook from non-allocating functions is sound: the
//! collector only needs to run when the heap grows, and a function
//! that does not call `gos_rt_aggr_alloc` / a string / vec / map
//! constructor cannot grow the heap. The conservative test below
//! returns `true` for any body that:
//!
//! - constructs an aggregate (`Rvalue::Aggregate`);
//! - emits a repeat expression `[v; N]` (`Rvalue::Repeat`);
//! - calls any function (we cannot tell from a single MIR body
//!   whether the callee allocates; assume yes).
//!
//! Leaf functions doing pure scalar arithmetic — the
//! perf-critical inner-loop helpers in `spectral-norm`,
//! `n-body`, `fannkuch-redux` — fall through to `false`, and the
//! codegen backends skip the prologue safepoint + shadow stack
//! save/restore. With those gone, `opt -O3` is free to vectorise
//! the calling loop and the 0.6.0 regression vanishes.

use crate::ir::{Body, Rvalue, StatementKind, Terminator};

/// Returns `true` when the function body might allocate — either
/// directly via an aggregate/repeat or indirectly via a `Call`
/// (callees may allocate; without whole-program analysis we can't
/// know, so we assume yes).
///
/// Codegen backends use this to elide the function-prologue
/// safepoint and the matching shadow-stack save/restore when
/// the body cannot allocate.
#[must_use]
pub fn body_might_allocate(body: &Body) -> bool {
    for block in &body.blocks {
        for stmt in &block.stmts {
            if let StatementKind::Assign { rvalue, .. } = &stmt.kind {
                match rvalue {
                    Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } => return true,
                    Rvalue::CallIntrinsic { name, .. } if intrinsic_might_allocate(name) => {
                        return true;
                    }
                    _ => {}
                }
            }
        }
        if matches!(&block.terminator, Terminator::Call { .. }) {
            return true;
        }
    }
    false
}

/// Whitelist of intrinsic names that are guaranteed not to allocate.
/// Anything outside the whitelist is treated as a potential
/// allocator (conservative).
fn intrinsic_might_allocate(name: &str) -> bool {
    !matches!(
        name,
        // Pure-arithmetic intrinsics that the lowerer emits as
        // direct ops; none of these touch the heap.
        "__abs_i64"
            | "__abs_f64"
            | "__min_i64"
            | "__max_i64"
            | "__min_f64"
            | "__max_f64"
            | "__sqrt_f64"
            | "__floor_f64"
            | "__ceil_f64"
            | "__round_f64"
            | "__trunc_f64"
            | "__sin_f64"
            | "__cos_f64"
            | "__tan_f64"
            | "__pow_f64"
            | "__exp_f64"
            | "__log_f64"
    )
}
