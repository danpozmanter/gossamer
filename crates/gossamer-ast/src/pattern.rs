//! Pattern nodes used by `let`, `match`, function parameters, and `for`.

#![forbid(unsafe_code)]

use gossamer_lex::Span;

use crate::common::{Ident, Mutability, RangeKind};
use crate::expr::Literal;
use crate::node_id::NodeId;
use crate::ty::TypePath;

/// A syntactic pattern, carrying a stable id and source span.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Pattern {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this pattern.
    pub span: Span,
    /// The kind of pattern being represented.
    pub kind: PatternKind,
}

impl Pattern {
    /// Constructs a new pattern node with the given id, span, and kind.
    #[must_use]
    pub fn new(id: NodeId, span: Span, kind: PatternKind) -> Self {
        Self { id, span, kind }
    }
}

impl PartialEq for Pattern {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

/// Every pattern production in the grammar (SPEC §5).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PatternKind {
    /// Wildcard `_`.
    Wildcard,
    /// Literal pattern — integer, string, char, byte, or bool literal.
    Literal(Literal),
    /// Binding pattern `mut? name @ sub?`.
    Ident {
        /// Mutability of the binding.
        mutability: Mutability,
        /// Bound identifier.
        name: Ident,
        /// Optional sub-pattern to bind `@ <pattern>`.
        subpattern: Option<Box<Pattern>>,
    },
    /// Path pattern matching a unit variant or constant, e.g. `None` or
    /// `std::net::IpAddr::V4`.
    Path(TypePath),
    /// Tuple pattern `(p1, p2, ...)`.
    Tuple(Vec<Pattern>),
    /// Struct pattern `path::Name { f1: p1, f2, .. }`.
    Struct {
        /// Path naming the struct type.
        path: TypePath,
        /// Field patterns in source order.
        fields: Vec<FieldPattern>,
        /// `true` when the pattern ends with `..` signalling "ignore the rest".
        rest: bool,
    },
    /// Tuple-struct pattern `Name(p1, p2, ...)`.
    TupleStruct {
        /// Path naming the tuple-struct.
        path: TypePath,
        /// Sub-patterns, one per positional field.
        elems: Vec<Pattern>,
    },
    /// Range pattern `lo..hi` or `lo..=hi`.
    Range {
        /// Lower bound literal.
        lo: Literal,
        /// Upper bound literal.
        hi: Literal,
        /// Whether the range is inclusive of the upper bound.
        kind: RangeKind,
    },
    /// Or-pattern `a | b | c`; the vector holds two or more alternatives.
    Or(Vec<Pattern>),
    /// Rest pattern `..` inside a tuple or slice pattern.
    Rest,
    /// Reference pattern `&p` or `&mut p`.
    Ref {
        /// Mutability of the reference.
        mutability: Mutability,
        /// Inner pattern matched through the reference.
        inner: Box<Pattern>,
    },
}

/// A single field pattern inside a struct pattern.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FieldPattern {
    /// Field name being matched.
    pub name: Ident,
    /// Pattern for the field's value; `None` means shorthand (name binds itself).
    pub pattern: Option<Pattern>,
}

impl FieldPattern {
    /// Constructs a shorthand field pattern `{ name }`.
    #[must_use]
    pub fn shorthand(name: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            pattern: None,
        }
    }

    /// Constructs an explicit field pattern `{ name: pattern }`.
    #[must_use]
    pub fn explicit(name: impl Into<String>, pattern: Pattern) -> Self {
        Self {
            name: Ident::new(name),
            pattern: Some(pattern),
        }
    }
}
