//! LLVM-backed release codegen.
//!
//! The Cranelift backend in `gossamer-codegen-cranelift` is the
//! default for `gos build`. This crate mirrors its
//! `compile_to_object` signature but emits LLVM IR text and
//! shells out to `llc -O3` for aggressive optimisation
//! (auto-vectorisation, loop unrolling, instcombine, SLP) —
//! the scalar-only scenarios where Cranelift leaves perf on
//! the table.
//!
//! Requires `llc` on `PATH` or via the `GOS_LLC` env var. We
//! shell out to it so this crate stays FFI-free: no
//! `inkwell`/`llvm-sys` dependency, no unsafe Rust, no
//! build-time LLVM header requirements. The runtime `.a`
//! staticlib is unchanged — the linker stage (`cc`) wires
//! the LLVM-produced object against it the same way as
//! Cranelift's.
//!
//! Coverage today is an MVP: `i64` / `f64` / `bool` / `()`
//! primitives, arithmetic and comparison `BinaryOp`, `Neg`
//! and `Not` `UnaryOp`, numeric `Cast`, direct calls to user
//! functions and `gos_rt_*` intrinsics, and `Goto` /
//! `SwitchInt` / `Return`. Bodies that exercise shapes
//! outside this set (closures, heap-backed aggregates, field
//! projections through `Arc`ed values) return
//! `BuildError::Unsupported` so the driver can fall back to
//! Cranelift for those programs.

// Allow patterns this backend deliberately uses:
//   - `doc_markdown` flags every reference to `i64`, `fasta_mt`,
//     `x86_64`, etc. in plain-prose docstrings.
//   - `nonminimal_bool` flags `if !cond { early_return; } else
//     { ... }` shapes that read more naturally than the
//     positively-phrased alternative.
//   - `too_many_lines` / `cognitive_complexity` fire on the
//     intrinsic-name / runtime-symbol dispatch arms.
//   - `if_not_else` is the same pattern as `nonminimal_bool`.
#![forbid(unsafe_code)]
#![allow(
    clippy::doc_markdown,
    clippy::nonminimal_bool,
    clippy::too_many_lines,
    clippy::cognitive_complexity,
    clippy::if_not_else,
    clippy::comparison_chain
)]

mod emit;
mod lower;
mod ty;

pub use emit::{
    BuildError, CompileOutcome, NativeObject, compile_to_object, compile_with_fallback,
    set_debug_info, set_reproducible,
};
