//! Statement nodes that appear inside block expressions.

#![forbid(unsafe_code)]

use gossamer_lex::Span;

use crate::expr::Expr;
use crate::items::Item;
use crate::node_id::NodeId;
use crate::pattern::Pattern;
use crate::ty::Type;

/// A statement inside a block expression.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Stmt {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this statement.
    pub span: Span,
    /// The kind of statement being represented.
    pub kind: StmtKind,
}

impl Stmt {
    /// Constructs a new statement node with the given id, span, and kind.
    #[must_use]
    pub fn new(id: NodeId, span: Span, kind: StmtKind) -> Self {
        Self { id, span, kind }
    }
}

impl PartialEq for Stmt {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

/// Every statement production in the grammar (SPEC §15).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StmtKind {
    /// `let [mut] pat [: ty] [= expr];`.
    Let {
        /// Binding pattern.
        pattern: Pattern,
        /// Optional type annotation.
        ty: Option<Type>,
        /// Optional initializer expression.
        init: Option<Box<Expr>>,
    },
    /// Expression used as a statement, with optional trailing `;`.
    Expr {
        /// Expression being evaluated.
        expr: Box<Expr>,
        /// `true` when the expression was followed by `;` in source.
        has_semi: bool,
    },
    /// A nested item declaration inside a block.
    Item(Box<Item>),
    /// `defer { block }` statement.
    Defer(Box<Expr>),
    /// `go expr` statement.
    Go(Box<Expr>),
}
