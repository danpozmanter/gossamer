//! Type representation for the Gossamer compiler.
//! This crate models every type production in SPEC §3: primitives,
//! tuples, arrays, slices, built-in collections (`Vec`, `HashMap`),
//! channel endpoints, GC references, function pointers and closures,
//! named ADTs, type aliases, trait objects, inference variables, and
//! bound type parameters.
//! Type handles are issued by the [`TyCtxt`] interner. Two structurally
//! identical types always intern to the same [`Ty`], so later passes
//! can compare types with a single `u32` comparison. The [`InferCtxt`]
//! sits on top of the interner and provides Hindley-Milner unification
//! with an occurs check.
//! See SPEC §3 for the full type system.

#![forbid(unsafe_code)]

mod arena_escape;
mod checker;
mod context;
mod error;
mod exhaustiveness;
mod infer;
pub mod printer;
pub mod std_fn_values;
pub mod stdlib_signatures;
mod subst;
mod table;
mod trait_index;
mod traits;
mod ty;

pub use arena_escape::{
    ArenaEscapeDiagnostic, ArenaEscapeError, ArenaEscapeKind, check_arena_escapes,
};
pub use checker::{
    is_array_sequence_method, is_iterator_method, is_slice_sequence_method,
    is_tuple_rejected_method, is_vec_only_sequence_method, typecheck_source_file,
    typecheck_source_file_for_repl_inspection, typecheck_source_file_with_edition,
    typecheck_source_file_with_lazy_iterators,
};
pub use context::TyCtxt;
pub use error::{TypeDiagnostic, TypeError};
pub use exhaustiveness::{ExhaustivenessDiagnostic, ExhaustivenessError, check_exhaustiveness};
pub use infer::{InferCtxt, UnifyError};
pub use printer::{render_public_ty, render_ty};
pub use stdlib_signatures::{
    STD_FUNCTION_SIGNATURES, StdFunctionSignature, function_shape as stdlib_function_shape,
    function_signature as stdlib_function_signature,
};
pub use subst::{GenericArg, Substs};
pub use table::TypeTable;
pub use trait_index::{
    ImplEntry, ImplFnId, ImplId, ImplIndex, ImplMethod, MethodResolution, TraitDiagnostic,
    TraitEntry, TraitError,
};
pub use traits::{Predicate, TraitRef};
pub use ty::{ArrayLen, ClosureKind, FloatTy, FnSig, IntTy, Mutbl, ParamIdx, Ty, TyKind, TyVid};

/// Method names whose built-in dispatch mutates and writes the receiver back
/// into its source place. The type checker and execution tiers share this
/// list so an immutable receiver cannot reach an in-place mutation through a
/// method call.
#[must_use]
pub fn is_mutating_method_name(name: &str) -> bool {
    matches!(
        name,
        "push"
            | "push_str"
            | "push_char"
            | "push_byte"
            | "push_back"
            | "push_front"
            | "pop"
            | "pop_back"
            | "pop_front"
            | "insert"
            | "or_insert"
            | "inc"
            | "inc_at"
            | "inc_batch"
            | "remove"
            | "clear"
            | "extend"
            | "extend_from_slice"
            | "truncate"
            | "reserve"
            | "reserve_exact"
            | "sort"
            | "sort_by"
            | "sort_by_key"
            | "reverse"
            | "swap"
            | "fill"
            | "append"
            | "resize"
            | "resize_with"
            | "split_off"
            | "drain"
            | "retain"
            | "shrink_to_fit"
    )
}
