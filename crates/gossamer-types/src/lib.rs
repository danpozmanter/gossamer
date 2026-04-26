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

mod checker;
mod context;
mod error;
mod exhaustiveness;
mod infer;
mod printer;
mod subst;
mod table;
mod trait_index;
mod traits;
mod ty;

pub use checker::typecheck_source_file;
pub use context::TyCtxt;
pub use error::{TypeDiagnostic, TypeError};
pub use exhaustiveness::{ExhaustivenessDiagnostic, ExhaustivenessError, check_exhaustiveness};
pub use infer::{InferCtxt, UnifyError};
pub use printer::render_ty;
pub use subst::{GenericArg, Substs};
pub use table::TypeTable;
pub use trait_index::{
    ImplEntry, ImplFnId, ImplId, ImplIndex, ImplMethod, MethodResolution, TraitDiagnostic,
    TraitEntry, TraitError,
};
pub use traits::{Predicate, TraitRef};
pub use ty::{ClosureKind, FloatTy, FnSig, IntTy, Mutbl, ParamIdx, Ty, TyKind, TyVid};
