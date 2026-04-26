//! Trait references and predicates used for bound checking and trait
//! resolution.

#![forbid(unsafe_code)]

use gossamer_resolve::DefId;

use crate::subst::Substs;
use crate::ty::Ty;

/// A concrete trait reference — the [`DefId`] of a trait together with
/// the substitutions that instantiate its generics.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraitRef {
    /// `DefId` of the trait being referenced.
    pub def: DefId,
    /// Generic substitutions for the trait's parameters. The first
    /// argument is conventionally the `Self` type the trait is applied
    /// to.
    pub substs: Substs,
}

impl TraitRef {
    /// Constructs a trait reference from its components.
    #[must_use]
    pub const fn new(def: DefId, substs: Substs) -> Self {
        Self { def, substs }
    }
}

/// Bound-style predicate attached to a generic parameter or `where`
/// clause.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Predicate {
    /// `T: Trait<Args>` — the type must implement the trait.
    Trait {
        /// The bounded self type.
        self_ty: Ty,
        /// Trait reference imposed on `self_ty`.
        trait_ref: TraitRef,
    },
    /// `Alias<Args> = Target` — equality between a projected type and
    /// a concrete type.
    Projection {
        /// The trait reference whose projection is being constrained.
        trait_ref: TraitRef,
        /// Name of the associated type.
        assoc: &'static str,
        /// Concrete right-hand type.
        ty: Ty,
    },
}
