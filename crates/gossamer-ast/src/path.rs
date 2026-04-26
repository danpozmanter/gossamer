//! Canonical path types shared by type paths and expression paths.

#![forbid(unsafe_code)]

pub use crate::expr::{PathExpr, PathSegment};
pub use crate::ty::{GenericArg, TypePath, TypePathSegment};

/// Canonical alias for a parsed path. The AST stores paths as one of
/// [`PathExpr`] (value position) or [`TypePath`] (type position); this alias
/// points at the type-position form for passes that prefer a single name.
pub type Path = TypePath;

/// Canonical alias for a parsed path segment (type-position form).
pub type Segment = TypePathSegment;
