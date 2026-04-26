//! Expression nodes covering every production in SPEC §15.

#![forbid(unsafe_code)]

use gossamer_lex::Span;

use crate::common::{AssignOp, BinaryOp, Ident, RangeKind, UnaryOp};
use crate::node_id::NodeId;
use crate::pattern::Pattern;
use crate::stmt::Stmt;
use crate::ty::{GenericArg, Type};

/// A syntactic expression, carrying a stable id and source span.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Expr {
    /// Unique id within the enclosing source file.
    pub id: NodeId,
    /// Source range covered by this expression.
    pub span: Span,
    /// The kind of expression being represented.
    pub kind: ExprKind,
}

impl Expr {
    /// Constructs a new expression node with the given id, span, and kind.
    #[must_use]
    pub fn new(id: NodeId, span: Span, kind: ExprKind) -> Self {
        Self { id, span, kind }
    }
}

impl PartialEq for Expr {
    fn eq(&self, other: &Self) -> bool {
        self.kind == other.kind
    }
}

/// Every expression production in the grammar.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ExprKind {
    /// Literal value.
    Literal(Literal),
    /// Name path `a::b::c` or `a::b::<T>::c`.
    Path(PathExpr),
    /// Function call `callee(arg1, arg2, ...)`.
    Call {
        /// Callee expression.
        callee: Box<Expr>,
        /// Call arguments.
        args: Vec<Expr>,
    },
    /// Method call `receiver.name::<T1, T2>(args)`.
    MethodCall {
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Method name.
        name: Ident,
        /// Turbofish generic arguments.
        generics: Vec<GenericArg>,
        /// Call arguments.
        args: Vec<Expr>,
    },
    /// Field access `receiver.name` or tuple index `receiver.0`.
    FieldAccess {
        /// Receiver expression.
        receiver: Box<Expr>,
        /// Field selector.
        field: FieldSelector,
    },
    /// Index expression `base[index]`.
    Index {
        /// Base expression.
        base: Box<Expr>,
        /// Index expression.
        index: Box<Expr>,
    },
    /// Prefix unary expression `-x`, `!x`, `&x`, `&mut x`, `*x`.
    Unary {
        /// Operator applied.
        op: UnaryOp,
        /// Operand expression.
        operand: Box<Expr>,
    },
    /// Infix binary expression `lhs op rhs`.
    Binary {
        /// Operator applied.
        op: BinaryOp,
        /// Left-hand operand.
        lhs: Box<Expr>,
        /// Right-hand operand.
        rhs: Box<Expr>,
    },
    /// Assignment expression `place op rhs`.
    Assign {
        /// Assignment operator (`=`, `+=`, ...).
        op: AssignOp,
        /// Place being assigned to.
        place: Box<Expr>,
        /// Right-hand value.
        value: Box<Expr>,
    },
    /// Cast expression `expr as Type`.
    Cast {
        /// Expression being cast.
        value: Box<Expr>,
        /// Target type.
        ty: Box<Type>,
    },
    /// `if` / `else if` / `else` chain.
    If {
        /// Condition expression.
        condition: Box<Expr>,
        /// Then-branch block expression.
        then_branch: Box<Expr>,
        /// Optional else branch (another `If` or a block).
        else_branch: Option<Box<Expr>>,
    },
    /// `match scrutinee { arms }`.
    Match {
        /// Scrutinee expression.
        scrutinee: Box<Expr>,
        /// Arms in source order.
        arms: Vec<MatchArm>,
    },
    /// `loop { body }` with an optional label.
    Loop {
        /// Optional label `'ident:`.
        label: Option<Label>,
        /// Block body.
        body: Box<Expr>,
    },
    /// `while cond { body }` with an optional label.
    While {
        /// Optional label `'ident:`.
        label: Option<Label>,
        /// Loop condition.
        condition: Box<Expr>,
        /// Block body.
        body: Box<Expr>,
    },
    /// `for pat in iter { body }` with an optional label.
    For {
        /// Optional label `'ident:`.
        label: Option<Label>,
        /// Binding pattern for the iteration value.
        pattern: Pattern,
        /// Iterator expression.
        iter: Box<Expr>,
        /// Block body.
        body: Box<Expr>,
    },
    /// Block expression `{ stmts; tail? }`.
    Block(Block),
    /// Closure expression `|params| body` or `|params| -> Ret { body }`.
    ///
    /// Gossamer has no ownership transfer, so there is no `move`
    /// qualifier: closures always capture by GC reference for heap
    /// types and by copy for `Copy` types.
    Closure {
        /// Parameter patterns with optional type annotations.
        params: Vec<ClosureParam>,
        /// Optional explicit return type.
        ret: Option<Type>,
        /// Closure body expression.
        body: Box<Expr>,
    },
    /// `return expr?`.
    Return(Option<Box<Expr>>),
    /// `break 'label? expr?`.
    Break {
        /// Optional label to break to.
        label: Option<Label>,
        /// Optional value returned from a `loop`.
        value: Option<Box<Expr>>,
    },
    /// `continue 'label?`.
    Continue {
        /// Optional label to continue to.
        label: Option<Label>,
    },
    /// Tuple expression `(a, b, c)` with two or more elements. The empty tuple
    /// `()` is represented by `Literal(Literal::Unit)`.
    Tuple(Vec<Expr>),
    /// Struct literal `Path { field: value, .. }` with optional base expression.
    Struct {
        /// Path naming the struct.
        path: PathExpr,
        /// Field initializers.
        fields: Vec<StructExprField>,
        /// Optional `..base` functional update.
        base: Option<Box<Expr>>,
    },
    /// Array expression `[a, b, c]` or `[value; count]`.
    Array(ArrayExpr),
    /// Range expression `lo..hi` / `lo..=hi`; bounds may be omitted.
    Range {
        /// Lower bound, if present.
        start: Option<Box<Expr>>,
        /// Upper bound, if present.
        end: Option<Box<Expr>>,
        /// Whether the upper bound is inclusive.
        kind: RangeKind,
    },
    /// `unsafe { ... }` block.
    Unsafe(Block),
    /// `expr?` — the `?` operator.
    Try(Box<Expr>),
    /// `select { arms }` expression.
    Select(Vec<SelectArm>),
    /// `name!(...)` or `name!{...}` macro invocation.
    MacroCall(MacroCall),
    /// `go expr` statement-expression form. When `expr` is a closure with no
    /// arguments, pretty-printers emit the sugared `go fn() { body }` form.
    Go(Box<Expr>),
}

/// Literal values appearing in expressions and patterns.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum Literal {
    /// Integer literal preserved in source form (e.g. `0x2a`, `1_000`).
    Int(String),
    /// Floating-point literal preserved in source form.
    Float(String),
    /// Double-quoted string literal with lexer-decoded contents.
    ///
    /// Pretty-printing escapes the contents back into a valid string literal.
    String(String),
    /// Raw string literal `r"..."` with the given number of surrounding `#` marks.
    RawString {
        /// Number of `#` characters on each side.
        hashes: u8,
        /// Raw contents, unescaped.
        value: String,
    },
    /// Char literal `'a'` decoded to a single scalar value.
    Char(char),
    /// Byte literal `b'a'`.
    Byte(u8),
    /// Byte string literal `b"..."`.
    ByteString(Vec<u8>),
    /// Raw byte string literal `br"..."`.
    RawByteString {
        /// Number of `#` characters on each side.
        hashes: u8,
        /// Raw bytes, unescaped.
        value: Vec<u8>,
    },
    /// Boolean literal `true` or `false`.
    Bool(bool),
    /// The unit value `()`.
    Unit,
}

/// A label identifier like `'outer` attached to a loop or referenced by `break`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct Label {
    /// Label name without the leading apostrophe.
    pub name: String,
}

impl Label {
    /// Constructs a label from its textual name (no apostrophe).
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// A field selector on the right-hand side of `.`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum FieldSelector {
    /// Named field `foo.bar`.
    Named(Ident),
    /// Tuple index `foo.0`.
    Index(u32),
}

/// A `.`-separated path used as an expression.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathExpr {
    /// Path segments in order.
    pub segments: Vec<PathSegment>,
}

impl PathExpr {
    /// Constructs a single-segment path with no generic arguments.
    #[must_use]
    pub fn single(name: impl Into<String>) -> Self {
        Self {
            segments: vec![PathSegment::new(name)],
        }
    }

    /// Constructs a multi-segment path from an iterator of segment names.
    #[must_use]
    pub fn from_names<I, S>(segments: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            segments: segments.into_iter().map(PathSegment::new).collect(),
        }
    }
}

/// A single `::`-delimited segment in an expression path.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PathSegment {
    /// Segment name.
    pub name: Ident,
    /// Generic arguments applied at this segment (turbofish style).
    pub generics: Vec<GenericArg>,
}

impl PathSegment {
    /// Constructs a segment with no generic arguments.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            generics: Vec::new(),
        }
    }
}

/// One arm of a `match` expression.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MatchArm {
    /// Pattern to match.
    pub pattern: Pattern,
    /// Optional `if` guard.
    pub guard: Option<Expr>,
    /// Right-hand side of `=>`.
    pub body: Expr,
}

/// Block expression consisting of statements and an optional tail expression.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Block {
    /// Statements in source order, excluding the optional tail expression.
    pub stmts: Vec<Stmt>,
    /// Optional tail expression that becomes the block's value.
    pub tail: Option<Box<Expr>>,
}

impl Block {
    /// Constructs an empty block with no statements or tail expression.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            stmts: Vec::new(),
            tail: None,
        }
    }
}

/// A closure parameter: pattern, optional type annotation.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ClosureParam {
    /// Binding pattern for the parameter.
    pub pattern: Pattern,
    /// Optional type annotation.
    pub ty: Option<Type>,
}

/// A single initializer inside a struct literal.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StructExprField {
    /// Field name being initialized.
    pub name: Ident,
    /// Initializer expression; `None` means shorthand (name is both field and value).
    pub value: Option<Expr>,
}

impl StructExprField {
    /// Constructs a shorthand initializer `name`.
    #[must_use]
    pub fn shorthand(name: impl Into<String>) -> Self {
        Self {
            name: Ident::new(name),
            value: None,
        }
    }

    /// Constructs an explicit initializer `name: value`.
    #[must_use]
    pub fn explicit(name: impl Into<String>, value: Expr) -> Self {
        Self {
            name: Ident::new(name),
            value: Some(value),
        }
    }
}

/// Array expression form.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum ArrayExpr {
    /// Explicit element list `[a, b, c]`.
    List(Vec<Expr>),
    /// Repeat form `[value; count]`.
    Repeat {
        /// Value to repeat.
        value: Box<Expr>,
        /// Count expression.
        count: Box<Expr>,
    },
}

/// One arm of a `select` expression.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct SelectArm {
    /// The communication or default operation.
    pub op: SelectOp,
    /// Right-hand side body after `=>`.
    pub body: Expr,
}

/// Operation performed by a `select` arm.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum SelectOp {
    /// `pat = chan.recv()`.
    Recv {
        /// Pattern bound to the received value.
        pattern: Pattern,
        /// Channel expression.
        channel: Expr,
    },
    /// `chan.send(value)`.
    Send {
        /// Channel expression.
        channel: Expr,
        /// Value to send.
        value: Expr,
    },
    /// `default`.
    Default,
}

/// A macro invocation such as `format!("hello {}", name)` or `vec![1, 2, 3]`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct MacroCall {
    /// Path naming the macro (e.g. `format`, `std::vec`).
    pub path: PathExpr,
    /// Delimiter used at the call site.
    pub delim: MacroDelim,
    /// Raw token-stream contents preserved as a string. A macro's body is not
    /// parsed as a Gossamer expression at this layer; later passes expand it.
    pub tokens: String,
}

/// Delimiter surrounding a macro invocation's token stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum MacroDelim {
    /// `name!(...)`.
    Paren,
    /// `name![...]`.
    Bracket,
    /// `name!{...}`.
    Brace,
}

impl MacroDelim {
    /// Returns the opening and closing delimiters for this form.
    #[must_use]
    pub const fn pair(self) -> (&'static str, &'static str) {
        match self {
            Self::Paren => ("(", ")"),
            Self::Bracket => ("[", "]"),
            Self::Brace => ("{", "}"),
        }
    }
}
