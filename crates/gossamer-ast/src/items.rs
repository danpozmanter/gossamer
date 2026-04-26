//! Item-level declarations: `fn`, `struct`, `enum`, `trait`, `impl`, ...

#![forbid(unsafe_code)]

use gossamer_lex::Span;

use crate::common::{Ident, Mutability, Visibility};
use crate::expr::{Expr, PathExpr};
use crate::node_id::NodeId;
use crate::pattern::Pattern;
use crate::ty::{Type, TypePath};

/// An item-level declaration.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Item {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this item.
    pub span: Span,
    /// Attributes attached to the item (`#[...]` and `#![...]`).
    pub attrs: Attrs,
    /// Declared visibility.
    pub visibility: Visibility,
    /// The kind of item being declared.
    pub kind: ItemKind,
}

impl Item {
    /// Constructs a new item node with the given id, span, attrs, visibility, and kind.
    #[must_use]
    pub fn new(
        id: NodeId,
        span: Span,
        attrs: Attrs,
        visibility: Visibility,
        kind: ItemKind,
    ) -> Self {
        Self {
            id,
            span,
            attrs,
            visibility,
            kind,
        }
    }
}

impl PartialEq for Item {
    fn eq(&self, other: &Self) -> bool {
        self.attrs == other.attrs && self.visibility == other.visibility && self.kind == other.kind
    }
}

/// Every item production in the grammar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ItemKind {
    /// `fn name<G>(params) -> ret where ... { body }`.
    Fn(FnDecl),
    /// `struct Name { ... }`, `struct Name(T, U);`, or `struct Name;`.
    Struct(StructDecl),
    /// `enum Name<G> { V1, V2(T), V3 { x: T } }`.
    Enum(EnumDecl),
    /// `trait Name<G>: Bounds { items }`.
    Trait(TraitDecl),
    /// `impl<G> Type { items }` or `impl<G> Trait for Type { items }`.
    Impl(ImplDecl),
    /// `type Name<G> = Type;`.
    TypeAlias(TypeAliasDecl),
    /// `const NAME: Type = Expr;`.
    Const(ConstDecl),
    /// `static [mut] NAME: Type = Expr;`.
    Static(StaticDecl),
    /// `mod name { items }` or `mod name;`.
    Mod(ModDecl),
    /// A free-standing attribute item `#![attr]` — uncommon outside
    /// crate-level headers but included for completeness.
    AttrItem(Attribute),
}

/// Attributes attached to a declaration.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attrs {
    /// Outer attributes written as `#[...]` before the item.
    pub outer: Vec<Attribute>,
    /// Inner attributes written as `#![...]` inside the item.
    pub inner: Vec<Attribute>,
}

impl Attrs {
    /// Returns `true` when no attributes are attached.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.outer.is_empty() && self.inner.is_empty()
    }
}

/// A single `#[...]` or `#![...]` attribute.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Attribute {
    /// Path naming the attribute (e.g. `derive`, `allow`).
    pub path: PathExpr,
    /// Raw delimited token contents preserved verbatim, without the outer
    /// delimiters. `None` means the attribute had no argument list.
    pub tokens: Option<String>,
}

/// Generic parameter list `<A, B: Bound, const N: usize>`.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Generics {
    /// Parameters in source order.
    pub params: Vec<GenericParam>,
}

impl Generics {
    /// Returns `true` when no generic parameters are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.params.is_empty()
    }
}

/// A single generic parameter.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum GenericParam {
    /// Lifetime parameter `'a`. Parsed for FFI compatibility and otherwise
    /// ignored by the type checker (see SPEC §3.10).
    Lifetime {
        /// Name of the lifetime without the leading apostrophe.
        name: String,
    },
    /// Type parameter `T: Bound = Default`.
    Type {
        /// Parameter name.
        name: Ident,
        /// Trait bounds applied to this parameter.
        bounds: Vec<TraitBound>,
        /// Optional default type.
        default: Option<Type>,
    },
    /// Const parameter `const N: Type = default`.
    Const {
        /// Parameter name.
        name: Ident,
        /// Type of the constant.
        ty: Type,
        /// Optional default value.
        default: Option<Expr>,
    },
}

/// `where T: Bound, U: Bound + Bound, ...` clause.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WhereClause {
    /// Individual predicates.
    pub predicates: Vec<WherePredicate>,
}

impl WhereClause {
    /// Returns `true` when no predicates are declared.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.predicates.is_empty()
    }
}

/// A single `where` clause predicate.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WherePredicate {
    /// Type being constrained.
    pub bounded: Type,
    /// Bounds applied to that type.
    pub bounds: Vec<TraitBound>,
}

/// A single trait bound `Path<Args>` as used in generics, supertraits, or where clauses.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TraitBound {
    /// Path naming the trait.
    pub path: TypePath,
}

/// A function declaration: signature plus optional body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct FnDecl {
    /// `true` when the function is declared `unsafe`.
    pub is_unsafe: bool,
    /// Function name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Generics,
    /// Parameter list (including an optional leading `self`).
    pub params: Vec<FnParam>,
    /// Optional return type; `None` is syntactically `()`.
    pub ret: Option<Type>,
    /// Optional `where` clause.
    pub where_clause: WhereClause,
    /// Function body. `None` means the signature is a trait-item declaration.
    pub body: Option<Box<Expr>>,
}

/// A single function parameter.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FnParam {
    /// `self` receiver (`self`, `&self`, `&mut self`).
    Receiver(Receiver),
    /// Regular parameter `pattern: type`.
    Typed {
        /// Binding pattern.
        pattern: Pattern,
        /// Parameter type.
        ty: Type,
    },
}

/// Kind of `self` receiver on a method.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum Receiver {
    /// `self`.
    Owned,
    /// `&self`.
    RefShared,
    /// `&mut self`.
    RefMut,
}

impl Receiver {
    /// Returns the canonical source spelling of this receiver form.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Owned => "self",
            Self::RefShared => "&self",
            Self::RefMut => "&mut self",
        }
    }
}

/// A struct declaration in one of its three syntactic forms.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StructDecl {
    /// Struct name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Generics,
    /// Optional `where` clause.
    pub where_clause: WhereClause,
    /// Shape of the struct's body.
    pub body: StructBody,
}

/// Body shape of a struct declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum StructBody {
    /// Named fields `{ a: T, b: U }`.
    Named(Vec<StructField>),
    /// Tuple fields `(T, U)`.
    Tuple(Vec<TupleField>),
    /// Unit struct `;`.
    Unit,
}

/// A named field declaration in a struct or enum variant.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StructField {
    /// Field attributes.
    pub attrs: Attrs,
    /// Field visibility.
    pub visibility: Visibility,
    /// Field name.
    pub name: Ident,
    /// Field type.
    pub ty: Type,
}

/// A positional field declaration in a tuple struct or tuple variant.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TupleField {
    /// Field attributes.
    pub attrs: Attrs,
    /// Field visibility.
    pub visibility: Visibility,
    /// Field type.
    pub ty: Type,
}

/// An enum declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnumDecl {
    /// Enum name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Generics,
    /// Optional `where` clause.
    pub where_clause: WhereClause,
    /// Variants in source order.
    pub variants: Vec<EnumVariant>,
}

/// A single enum variant.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct EnumVariant {
    /// Attributes on the variant.
    pub attrs: Attrs,
    /// Variant name.
    pub name: Ident,
    /// Variant payload shape.
    pub body: StructBody,
    /// Optional explicit discriminant `= expr`.
    pub discriminant: Option<Expr>,
}

/// A trait declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TraitDecl {
    /// Trait name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Generics,
    /// Supertrait bounds after `:`.
    pub supertraits: Vec<TraitBound>,
    /// Optional `where` clause.
    pub where_clause: WhereClause,
    /// Trait items (methods, associated types, associated constants).
    pub items: Vec<TraitItem>,
}

/// One item inside a `trait` body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum TraitItem {
    /// Method signature with optional default body.
    Fn(FnDecl),
    /// Associated type `type Name: Bounds = Default;`.
    Type {
        /// Attributes on the associated type.
        attrs: Attrs,
        /// Name of the associated type.
        name: Ident,
        /// Trait bounds applied to the associated type.
        bounds: Vec<TraitBound>,
        /// Optional default type.
        default: Option<Type>,
    },
    /// Associated constant `const NAME: Ty = Expr;`.
    Const {
        /// Attributes on the associated constant.
        attrs: Attrs,
        /// Constant name.
        name: Ident,
        /// Constant type.
        ty: Type,
        /// Optional default value.
        default: Option<Expr>,
    },
}

/// An `impl` block.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ImplDecl {
    /// Generic parameters on the impl.
    pub generics: Generics,
    /// Trait being implemented, if this is a trait impl.
    pub trait_ref: Option<TraitBound>,
    /// Self type the impl attaches to.
    pub self_ty: Type,
    /// Optional `where` clause.
    pub where_clause: WhereClause,
    /// Impl items in source order.
    pub items: Vec<ImplItem>,
}

/// One item inside an `impl` body.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ImplItem {
    /// Method or associated function.
    Fn(FnDecl),
    /// Associated type definition `type Name = Type;`.
    Type {
        /// Attributes on the associated type.
        attrs: Attrs,
        /// Name of the associated type.
        name: Ident,
        /// Concrete type.
        ty: Type,
    },
    /// Associated constant `const NAME: Ty = Expr;`.
    Const {
        /// Attributes on the associated constant.
        attrs: Attrs,
        /// Constant name.
        name: Ident,
        /// Constant type.
        ty: Type,
        /// Constant value.
        value: Expr,
    },
}

/// Type alias declaration `type Name<G> = Type;`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TypeAliasDecl {
    /// Alias name.
    pub name: Ident,
    /// Generic parameters.
    pub generics: Generics,
    /// Right-hand type.
    pub ty: Type,
}

/// `const` item declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ConstDecl {
    /// Constant name.
    pub name: Ident,
    /// Constant type.
    pub ty: Type,
    /// Constant value expression.
    pub value: Expr,
}

/// `static` item declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StaticDecl {
    /// Mutability of the static.
    pub mutability: Mutability,
    /// Static name.
    pub name: Ident,
    /// Static type.
    pub ty: Type,
    /// Static value expression.
    pub value: Expr,
}

/// `mod` item declaration — inline or external.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ModDecl {
    /// Module name.
    pub name: Ident,
    /// Module body.
    pub body: ModBody,
}

/// Body of a module declaration.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ModBody {
    /// Inline module: `mod name { items }`.
    Inline(Vec<Item>),
    /// External module reference: `mod name;`.
    External,
}
