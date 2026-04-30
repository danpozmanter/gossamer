//! HIR data types.
//! The HIR is a structurally simplified form of the AST. Control-flow
//! sugar such as `for`, `?`, and the forward-pipe `|>` has been
//! desugared; every node carries a [`HirId`] and an optional [`Ty`]
//! annotation propagated.

#![forbid(unsafe_code)]

use gossamer_ast::Ident;
use gossamer_lex::Span;
use gossamer_resolve::DefId;
use gossamer_types::Ty;

use crate::ids::HirId;

/// Whole program — the collection of items lowered from a source file.
#[derive(Debug, Clone, Default)]
pub struct HirProgram {
    /// Items in source order.
    pub items: Vec<HirItem>,
}

/// One top-level item.
#[derive(Debug, Clone)]
pub struct HirItem {
    /// Stable id for this item.
    pub id: HirId,
    /// Source range.
    pub span: Span,
    /// `DefId` associated with the item, when resolver assigned one.
    pub def: Option<DefId>,
    /// Item variant.
    pub kind: HirItemKind,
}

/// HIR item kinds mirror [`gossamer_ast::ItemKind`] but drop forms that
/// don't produce lowered bodies (attributes, external modules).
#[derive(Debug, Clone)]
pub enum HirItemKind {
    /// Function or method declaration.
    Fn(HirFn),
    /// `const NAME: T = EXPR;`.
    Const(HirConst),
    /// `static [mut] NAME: T = EXPR;`.
    Static(HirStatic),
    /// Aggregate declaration whose body is its field/variant list.
    Adt(HirAdt),
    /// `impl` block. Items inside are flattened into `Fn` items after
    /// lowering.
    Impl(HirImpl),
    /// `trait` declaration with its methods.
    Trait(HirTrait),
}

/// Lowered function declaration.
#[derive(Debug, Clone)]
pub struct HirFn {
    /// Function name.
    pub name: Ident,
    /// Parameter types paired with their binding patterns.
    pub params: Vec<HirParam>,
    /// Declared return type.
    pub ret: Option<Ty>,
    /// Function body, if present (trait method signatures have no
    /// body).
    pub body: Option<HirBody>,
    /// `true` when the declaration is `unsafe`.
    pub is_unsafe: bool,
    /// `true` when the first parameter is a `self` receiver.
    pub has_self: bool,
}

/// Lowered parameter.
#[derive(Debug, Clone)]
pub struct HirParam {
    /// Binding pattern.
    pub pattern: HirPat,
    /// Resolved type of the parameter.
    pub ty: Ty,
}

/// Lowered constant item.
#[derive(Debug, Clone)]
pub struct HirConst {
    /// Item name.
    pub name: Ident,
    /// Resolved type of the constant.
    pub ty: Ty,
    /// Initializer expression.
    pub value: HirExpr,
}

/// Lowered static item.
#[derive(Debug, Clone)]
pub struct HirStatic {
    /// Item name.
    pub name: Ident,
    /// Resolved type of the static.
    pub ty: Ty,
    /// Mutability as declared.
    pub mutable: bool,
    /// Initializer expression.
    pub value: HirExpr,
}

/// Lowered struct/enum declaration.
#[derive(Debug, Clone)]
pub struct HirAdt {
    /// Declared name.
    pub name: Ident,
    /// Kind of aggregate.
    pub kind: HirAdtKind,
    /// Resolved self type for convenience.
    pub self_ty: Ty,
}

/// Kind of aggregate at the HIR level.
#[derive(Debug, Clone)]
pub enum HirAdtKind {
    /// `struct` with named, positional, or unit body. The embedded
    /// list carries the field names in declaration order — empty for
    /// a unit struct or tuple struct with positional-only fields.
    Struct(Vec<Ident>),
    /// `enum` with the given variants. Each variant carries its
    /// name plus an optional ordered field list — `None` for unit
    /// (`Line`) and tuple variants (`Circle(f64)`); `Some(names)`
    /// for struct-payload variants (`Rect { w, h }`). The MIR
    /// lowerer reads the field names so `__struct("Rect", ...)`
    /// calls can reorder operands into declaration order even
    /// when the matching enum variant rather than a real struct
    /// is the target.
    Enum(Vec<HirEnumVariant>),
}

/// A single enum variant's HIR representation.
#[derive(Debug, Clone)]
pub struct HirEnumVariant {
    /// Variant name (e.g. `Rect` in `enum Shape { Rect { w, h } }`).
    pub name: Ident,
    /// Ordered field names for struct-payload variants. `None`
    /// for unit and tuple-struct variants.
    pub struct_fields: Option<Vec<Ident>>,
    /// Ordered field types matching `struct_fields`, parallel
    /// vector. Filled by [`crate::lower`] from the AST so the MIR
    /// lowerer can emit typed loads for `Shape::Rect { w, h }`
    /// match arms (without this the loaded i64 was printed verbatim
    /// for f64 fields). Length matches `struct_fields`'s when
    /// present.
    pub struct_field_tys: Option<Vec<crate::tree::Ty>>,
}

/// Lowered `impl` block.
#[derive(Debug, Clone)]
pub struct HirImpl {
    /// Self type the impl attaches to.
    pub self_ty: Ty,
    /// Syntactic name of the self type (last path segment of the
    /// impl header's type expression), if the impl targets a nominal
    /// type. `impl Counter { ... }` yields `Some("Counter")`;
    /// reference and tuple self types yield `None`. Used by the
    /// tree-walker to key method lookups by type name so two impls
    /// with the same method name on different types do not collide.
    pub self_name: Option<Ident>,
    /// Trait being implemented, if any, represented by its name.
    pub trait_name: Option<Ident>,
    /// Method items in source order.
    pub methods: Vec<HirFn>,
}

/// Lowered `trait` declaration.
#[derive(Debug, Clone)]
pub struct HirTrait {
    /// Trait name.
    pub name: Ident,
    /// Method items in declaration order.
    pub methods: Vec<HirFn>,
}

/// Body of a function/closure/const initializer.
#[derive(Debug, Clone)]
pub struct HirBody {
    /// Root block evaluated by the body.
    pub block: HirBlock,
}

/// Block expression: statements followed by an optional tail.
#[derive(Debug, Clone)]
pub struct HirBlock {
    /// Stable id for this block.
    pub id: HirId,
    /// Source range covering the block.
    pub span: Span,
    /// Statements executed in order.
    pub stmts: Vec<HirStmt>,
    /// Optional tail expression whose value is the block's result.
    pub tail: Option<Box<HirExpr>>,
    /// Type of the block (unit when no tail).
    pub ty: Ty,
}

/// Single statement inside a block.
#[derive(Debug, Clone)]
pub struct HirStmt {
    /// Stable id.
    pub id: HirId,
    /// Source range.
    pub span: Span,
    /// Statement kind.
    pub kind: HirStmtKind,
}

/// Statement kinds at the HIR level.
#[derive(Debug, Clone)]
pub enum HirStmtKind {
    /// `let pat = expr`.
    Let {
        /// Binding pattern.
        pattern: HirPat,
        /// Declared type of the binding.
        ty: Ty,
        /// Optional initializer.
        init: Option<HirExpr>,
    },
    /// Expression used as a statement (with or without trailing `;`).
    Expr {
        /// Expression evaluated for effect.
        expr: HirExpr,
        /// `true` when the expression was followed by `;`.
        has_semi: bool,
    },
    /// `defer { ... }`.
    Defer(HirExpr),
    /// `go expr`.
    Go(HirExpr),
    /// A nested item declaration.
    Item(Box<HirItem>),
}

/// Expression node.
#[derive(Debug, Clone)]
pub struct HirExpr {
    /// Stable id.
    pub id: HirId,
    /// Source range.
    pub span: Span,
    /// Resolved type for this expression.
    pub ty: Ty,
    /// Variant.
    pub kind: HirExprKind,
}

/// One arm of a `select { … }` expression after HIR lowering.
#[derive(Debug, Clone)]
pub struct HirSelectArm {
    /// Operation kind — recv on a channel, send on a channel, or the
    /// default fallback arm.
    pub op: HirSelectOp,
    /// Body evaluated when this arm is chosen.
    pub body: HirExpr,
}

/// Operation performed by a `select` arm.
#[derive(Debug, Clone)]
pub enum HirSelectOp {
    /// `pat = chan.recv()` — receive from `channel`, binding the
    /// received value to `pattern`.
    Recv {
        /// Pattern that receives the value.
        pattern: HirPat,
        /// Channel expression.
        channel: HirExpr,
    },
    /// `chan.send(value)` — send `value` on `channel`.
    Send {
        /// Channel expression.
        channel: HirExpr,
        /// Value being sent.
        value: HirExpr,
    },
    /// `default`.
    Default,
}

/// HIR expression kinds.
#[derive(Debug, Clone)]
pub enum HirExprKind {
    /// Primitive literal preserved in source form.
    Literal(HirLiteral),
    /// Named path reference resolved to a concrete target.
    Path {
        /// Path segments.
        segments: Vec<Ident>,
        /// Resolved definition id, when the resolver produced one.
        def: Option<DefId>,
    },
    /// Direct function call.
    Call {
        /// Callee expression.
        callee: Box<HirExpr>,
        /// Call arguments.
        args: Vec<HirExpr>,
    },
    /// Method call.
    MethodCall {
        /// Receiver expression.
        receiver: Box<HirExpr>,
        /// Method name.
        name: Ident,
        /// Call arguments.
        args: Vec<HirExpr>,
    },
    /// Field access `receiver.name`.
    Field {
        /// Receiver expression.
        receiver: Box<HirExpr>,
        /// Field name.
        name: Ident,
    },
    /// Tuple index `receiver.0`.
    TupleIndex {
        /// Receiver expression.
        receiver: Box<HirExpr>,
        /// Tuple index.
        index: u32,
    },
    /// Indexing `base[index]`.
    Index {
        /// Base expression.
        base: Box<HirExpr>,
        /// Index expression.
        index: Box<HirExpr>,
    },
    /// Unary operator.
    Unary {
        /// Operator.
        op: HirUnaryOp,
        /// Operand.
        operand: Box<HirExpr>,
    },
    /// Binary operator.
    Binary {
        /// Operator.
        op: HirBinaryOp,
        /// Left operand.
        lhs: Box<HirExpr>,
        /// Right operand.
        rhs: Box<HirExpr>,
    },
    /// Assignment.
    Assign {
        /// Place being assigned to.
        place: Box<HirExpr>,
        /// Value being stored.
        value: Box<HirExpr>,
    },
    /// `if` / `else` chain.
    If {
        /// Condition expression.
        condition: Box<HirExpr>,
        /// Then branch.
        then_branch: Box<HirExpr>,
        /// Optional else branch.
        else_branch: Option<Box<HirExpr>>,
    },
    /// `match` expression.
    Match {
        /// Scrutinee expression.
        scrutinee: Box<HirExpr>,
        /// Arms in source order.
        arms: Vec<HirMatchArm>,
    },
    /// `loop { body }`.
    Loop {
        /// Body expression.
        body: Box<HirExpr>,
    },
    /// `while cond { body }`.
    While {
        /// Condition.
        condition: Box<HirExpr>,
        /// Body.
        body: Box<HirExpr>,
    },
    /// Block expression.
    Block(HirBlock),
    /// Closure expression.
    Closure {
        /// Parameters.
        params: Vec<HirParam>,
        /// Optional return type.
        ret: Option<Ty>,
        /// Body expression.
        body: Box<HirExpr>,
    },
    /// Post-lifting reference to a closure whose body has been moved
    /// to a synthetic top-level function. `captures` holds the
    /// expressions that produce each captured value in declaration
    /// order; the MIR lowerer stores them on the heap and tracks
    /// which local holds the resulting env pointer so subsequent
    /// direct calls can be dispatched to `name` natively.
    LiftedClosure {
        /// Synthetic top-level function name (`__closure_N`).
        name: Ident,
        /// Captured-value expressions in the same order the lifted
        /// function's `gos_load`s expect them.
        captures: Vec<HirExpr>,
    },
    /// `select { … }` expression. Preserves the channel/default arm
    /// structure so the evaluator can poll each channel's readiness
    /// at runtime and pick the first ready arm, falling back to the
    /// `default` arm when none are ready.
    Select {
        /// Arms in source order.
        arms: Vec<HirSelectArm>,
    },
    /// `return expr?`.
    Return(Option<Box<HirExpr>>),
    /// `break [value]`.
    Break(Option<Box<HirExpr>>),
    /// `continue`.
    Continue,
    /// Tuple literal.
    Tuple(Vec<HirExpr>),
    /// Array literal (explicit or repeat form).
    Array(HirArrayExpr),
    /// `go expr`.
    Go(Box<HirExpr>),
    /// Cast `expr as T`.
    Cast {
        /// Value being cast.
        value: Box<HirExpr>,
        /// Target type after lowering.
        ty: Ty,
    },
    /// Range expression `a..b` / `a..=b`.
    Range {
        /// Lower bound.
        start: Option<Box<HirExpr>>,
        /// Upper bound.
        end: Option<Box<HirExpr>>,
        /// `true` when the upper bound is inclusive.
        inclusive: bool,
    },
    /// Unresolved placeholder for forms the lowerer does not yet
    /// rewrite (e.g. macro invocations, select expressions).
    Placeholder,
}

/// Literal values at the HIR level.
#[derive(Debug, Clone)]
pub enum HirLiteral {
    /// Integer literal preserved verbatim.
    Int(String),
    /// Float literal preserved verbatim.
    Float(String),
    /// String literal with lexer-decoded contents.
    String(String),
    /// Char literal.
    Char(char),
    /// Byte literal.
    Byte(u8),
    /// Byte-string literal.
    ByteString(Vec<u8>),
    /// Boolean literal.
    Bool(bool),
    /// Unit literal `()`.
    Unit,
}

/// Unary operators at the HIR level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirUnaryOp {
    /// `-x`.
    Neg,
    /// `!x`.
    Not,
    /// `&x`.
    RefShared,
    /// `&mut x`.
    RefMut,
    /// `*x` (raw deref inside unsafe).
    Deref,
}

/// Binary operators at the HIR level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HirBinaryOp {
    /// `+`.
    Add,
    /// `-`.
    Sub,
    /// `*`.
    Mul,
    /// `/`.
    Div,
    /// `%`.
    Rem,
    /// `&`.
    BitAnd,
    /// `|`.
    BitOr,
    /// `^`.
    BitXor,
    /// `<<`.
    Shl,
    /// `>>`.
    Shr,
    /// `==`.
    Eq,
    /// `!=`.
    Ne,
    /// `<`.
    Lt,
    /// `<=`.
    Le,
    /// `>`.
    Gt,
    /// `>=`.
    Ge,
    /// `&&`.
    And,
    /// `||`.
    Or,
}

/// One arm of a `match` expression.
#[derive(Debug, Clone)]
pub struct HirMatchArm {
    /// Pattern matched by this arm.
    pub pattern: HirPat,
    /// Optional `if`-guard.
    pub guard: Option<HirExpr>,
    /// Right-hand side expression.
    pub body: HirExpr,
}

/// Array expression forms at the HIR level.
#[derive(Debug, Clone)]
pub enum HirArrayExpr {
    /// Explicit element list.
    List(Vec<HirExpr>),
    /// Repeat form `[value; count]`.
    Repeat {
        /// Value to repeat.
        value: Box<HirExpr>,
        /// Count expression.
        count: Box<HirExpr>,
    },
}

/// HIR pattern form. Deliberately simpler than the AST pattern so
/// downstream passes only need a handful of variants.
#[derive(Debug, Clone)]
pub struct HirPat {
    /// Stable id.
    pub id: HirId,
    /// Source range.
    pub span: Span,
    /// Type of the matched value.
    pub ty: Ty,
    /// Pattern variant.
    pub kind: HirPatKind,
}

/// HIR pattern kinds.
#[derive(Debug, Clone)]
pub enum HirPatKind {
    /// `_` matching anything.
    Wildcard,
    /// Identifier binding (`mut? name`).
    Binding {
        /// Binding name.
        name: Ident,
        /// `true` when declared mutable.
        mutable: bool,
    },
    /// Literal pattern.
    Literal(HirLiteral),
    /// Tuple pattern.
    Tuple(Vec<HirPat>),
    /// Enum variant or tuple-struct pattern.
    Variant {
        /// Variant name (last path segment).
        name: Ident,
        /// Sub-patterns, in declaration order.
        fields: Vec<HirPat>,
    },
    /// Struct pattern `Path { f: p, .. }`.
    Struct {
        /// Path naming the struct.
        name: Ident,
        /// Field patterns.
        fields: Vec<HirFieldPat>,
        /// `true` when `..` was written to ignore remaining fields.
        rest: bool,
    },
    /// `&pat` or `&mut pat`.
    Ref {
        /// Inner pattern.
        inner: Box<HirPat>,
        /// `true` when the reference was declared `&mut`.
        mutable: bool,
    },
    /// Or-pattern `a | b | c`.
    Or(Vec<HirPat>),
    /// `..` rest pattern.
    Rest,
    /// Range pattern `lo..hi` (exclusive) or `lo..=hi` (inclusive).
    /// Carries both literal bounds plus the inclusivity flag so
    /// MIR can lower it into a `(scrut >= lo) && (scrut < hi)` /
    /// `(scrut >= lo) && (scrut <= hi)` predicate.
    Range {
        /// Lower bound literal (always present in source).
        lo: HirLiteral,
        /// Upper bound literal (always present in source).
        hi: HirLiteral,
        /// `true` for `..=` (inclusive of `hi`); `false` for `..`.
        inclusive: bool,
    },
}

/// A single field pattern inside a struct pattern.
#[derive(Debug, Clone)]
pub struct HirFieldPat {
    /// Field name.
    pub name: Ident,
    /// Sub-pattern, or shorthand binding if absent.
    pub pattern: Option<HirPat>,
}
