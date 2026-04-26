//! Abstract syntax tree types for the Gossamer language.
//! This crate models every production in SPEC §15 as owned `Debug + Clone`
//! Rust types. Every AST node (expression, pattern, type, item, statement)
//! carries a stable [`node_id::NodeId`] and a `gossamer_lex::Span`, but
//! [`PartialEq`] implementations ignore both so structural comparisons work
//! across parser runs. A pretty-printer module (`printer`) renders any node
//! back into idiomatic Gossamer source.

#![forbid(unsafe_code)]

pub mod common;
pub mod expr;
pub mod items;
pub mod node_id;
pub mod path;
pub mod pattern;
pub mod printer;
pub mod source_file;
pub mod stmt;
pub mod ty;
pub mod visitor;

pub use common::{AssignOp, BinaryOp, Ident, Mutability, RangeKind, UnaryOp, Visibility};
pub use expr::{
    ArrayExpr, Block, ClosureParam, Expr, ExprKind, FieldSelector, Label, Literal, MacroCall,
    MacroDelim, MatchArm, PathExpr, PathSegment, SelectArm, SelectOp, StructExprField,
};
pub use items::{
    Attribute, Attrs, ConstDecl, EnumDecl, EnumVariant, FnDecl, FnParam, GenericParam, Generics,
    ImplDecl, ImplItem, Item, ItemKind, ModBody, ModDecl, Receiver, StaticDecl, StructBody,
    StructDecl, StructField, TraitBound, TraitDecl, TraitItem, TupleField, TypeAliasDecl,
    WhereClause, WherePredicate,
};
pub use node_id::{NodeId, NodeIdGenerator};
pub use path::{Path, Segment};
pub use pattern::{FieldPattern, Pattern, PatternKind};
pub use printer::Printer;
pub use source_file::{ModulePath, SourceFile, UseDecl, UseListEntry, UseTarget};
pub use stmt::{Stmt, StmtKind};
pub use ty::{FnTypeKind, GenericArg, Type, TypeKind, TypePath, TypePathSegment};
pub use visitor::{Visitor, VisitorMut};
