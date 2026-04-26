//! Name resolution for the Gossamer compiler.
//! The resolver turns a parsed [`gossamer_ast::SourceFile`] into a
//! [`Resolutions`] side table keyed by `NodeId`, plus a vector of
//! [`ResolveDiagnostic`]s for any names that could not be resolved.
//! Name lookup runs in two passes over the top-level items of the crate
//! root. The first pass allocates [`DefId`]s and registers every item in
//! the module namespace so that forward references work. The second pass
//! walks each item body, pushing block/function/pattern scopes as it
//! goes, and records a [`Resolution`] for every path occurrence.
//! Imports brought in by `use` declarations are represented as
//! [`Resolution::Import`] and the consumer (HIR lowering) is responsible
//! for following the full module path externally. The resolver does not
//! validate that the target of a `use` actually exists; see SPEC §6.

#![forbid(unsafe_code)]

mod cfg;
mod def_id;
mod diagnostic;
mod resolutions;
mod resolver;
mod scope;

pub use cfg::{item_is_active, set_test_cfg};

pub use def_id::{CrateId, DefId, DefIdGenerator, DefKind, ModId};
pub use diagnostic::{ResolveDiagnostic, ResolveError};
pub use resolutions::{FloatWidth, IntWidth, PrimitiveTy, Resolution, Resolutions};
pub use resolver::resolve_source_file;
