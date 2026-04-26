//! Type expressions as they appear in the source grammar (SPEC §3).

#![forbid(unsafe_code)]

use gossamer_lex::Span;

use crate::common::{Ident, Mutability};
use crate::expr::Expr;
use crate::node_id::NodeId;

/// A syntactic type expression, carrying a stable id and source span.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Type {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this type expression.
    pub span: Span,
    /// The kind of type being represented.
    pub kind: TypeKind,
}

impl Type {
    /// Constructs a new type node with the given id, span, and kind.
    #[must_use]
    pub fn new(id: NodeId, span: Span, kind: TypeKind) -> Self {
        Self { id, span, kind }
    }
}

impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

/// Every type production in the grammar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TypeKind {
    /// The unit type `()`.
    Unit,
    /// The never type `!`.
    Never,
    /// Inferred type placeholder `_`.
    Infer,
    /// A named path type, e.g. `Vec<T>`, `std::io::Error`, `Self`.
    Path(TypePath),
    /// A tuple type `(T1, T2, ...)` with two or more elements.
    Tuple(Vec<Type>),
    /// Fixed-length array type `[T; N]` — `len` is the length expression.
    Array {
        /// Element type of the array.
        elem: Box<Type>,
        /// Length expression (typically an `IntLit` or const path).
        len: Box<Expr>,
    },
    /// Unsized slice type `[T]`, always seen through a reference in source.
    Slice(Box<Type>),
    /// GC reference type `&T` or `&mut T`.
    Ref {
        /// Mutability of the reference (`Mutable` for `&mut T`).
        mutability: Mutability,
        /// Referent type.
        inner: Box<Type>,
    },
    /// Function pointer or closure-trait type.
    Fn {
        /// Kind of function type (`fn`, `Fn`, `FnMut`, `FnOnce`).
        kind: FnTypeKind,
        /// Parameter types.
        params: Vec<Type>,
        /// Return type (`None` means unit).
        ret: Option<Box<Type>>,
    },
}

/// Kind of callable type used by `TypeKind::Fn`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FnTypeKind {
    /// Non-capturing function pointer (`fn`).
    Fn,
    /// Closure trait `Fn`.
    ClosureFn,
    /// Closure trait `FnMut`.
    ClosureFnMut,
    /// Closure trait `FnOnce`.
    ClosureFnOnce,
}

impl FnTypeKind {
    /// Returns the keyword spelling of this callable kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fn => "fn",
            Self::ClosureFn => "Fn",
            Self::ClosureFnMut => "FnMut",
            Self::ClosureFnOnce => "FnOnce",
        }
    }
}

/// A parsed type path such as `std::collections::HashMap<K, V>`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TypePath {
    /// Path segments in order from outermost namespace to leaf.
    pub segments: Vec<TypePathSegment>,
}

impl TypePath {
    /// Constructs a path consisting of a single segment with no generic arguments.
    #[must_use]
    pub fn single(name: impl Into<String>) -> Self {
        Self {
            segments: vec![TypePathSegment::new(name)],
        }
    }
}

/// One `::`-delimited segment of a type path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TypePathSegment {
    /// Segment name.
    pub name: Ident,
    /// Generic arguments applied at this segment, if any.
    pub generics: Vec<GenericArg>,
}

impl TypePathSegment {
    /// Constructs a segment with no generic arguments.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            generics: Vec::new(),
        }
    }

    /// Constructs a segment applying the given generic arguments.
    #[must_use]
    pub fn with_generics(name: impl Into<String>, generics: Vec<GenericArg>) -> Self {
        Self {
            name: Ident::new(name),
            generics,
        }
    }
}

/// A generic argument in a type path segment.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GenericArg {
    /// A type argument, e.g. the `i32` in `Vec<i32>`.
    Type(Type),
    /// A const expression argument, e.g. the `4` in `Array<T, 4>`.
    Const(Expr),
}
