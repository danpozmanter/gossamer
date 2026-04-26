//! Mid-level IR (MIR) data types.
//! MIR is the **single source of truth** for all language semantics.
//! The interpreter executes MIR directly; the compiler lowers MIR to
//! machine code. No semantic logic lives outside this IR — if a
//! behaviour is not expressible as a [`StatementKind`], [`Terminator`],
//! [`Rvalue`], or [`ConstValue`], it does not exist at this layer.
//! Mirrors rustc's MIR in spirit: a per-function control-flow graph of
//! [`BasicBlock`]s, each ending in a [`Terminator`]. Local variables
//! live in a flat `Vec` indexed by [`Local`]. The IR is SSA-lite:
//! locals may be assigned multiple times, but the lowerer gives every
//! temporary a fresh local so most intermediates do obey single
//! assignment in practice.

#![forbid(unsafe_code)]

use gossamer_ast::Ident;
use gossamer_lex::Span;
use gossamer_resolve::DefId;
use gossamer_types::Ty;

/// Local variable index within a [`Body`]. `Local(0)` is the return
/// slot; subsequent indices are parameters followed by temporaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Local(pub u32);

impl Local {
    /// Index `0` — reserved for the function's return value.
    pub const RETURN: Self = Self(0);

    /// Raw numeric index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Basic-block identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct BlockId(pub u32);

impl BlockId {
    /// Entry block assigned at body construction time.
    pub const ENTRY: Self = Self(0);

    /// Raw numeric index.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Per-function CFG plus locals table.
#[derive(Debug, Clone)]
pub struct Body {
    /// Source-level function name, useful in diagnostics.
    pub name: String,
    /// [`DefId`] assigned to this function by the resolver. Needed
    /// by the native backend to link `Operand::FnRef(def)` sites to
    /// their definitions without going through the function name.
    /// `None` for functions without a resolver-assigned id (e.g.
    /// synthesised closures before resolver integration lands).
    pub def: Option<DefId>,
    /// Number of parameters; parameters live at locals `1..=arity`.
    pub arity: u32,
    /// Type of each local, indexed by [`Local`].
    pub locals: Vec<LocalDecl>,
    /// CFG blocks indexed by [`BlockId`].
    pub blocks: Vec<BasicBlock>,
    /// Source span of the source-level function declaration.
    pub span: Span,
}

impl Body {
    /// Borrows a block by id.
    ///
    /// # Panics
    ///
    /// Panics if `id` is out of range.
    #[must_use]
    pub fn block(&self, id: BlockId) -> &BasicBlock {
        &self.blocks[id.0 as usize]
    }

    /// Mutably borrows a block by id.
    ///
    /// # Panics
    ///
    /// Panics if `id` is out of range.
    pub fn block_mut(&mut self, id: BlockId) -> &mut BasicBlock {
        &mut self.blocks[id.0 as usize]
    }

    /// Returns the type of `local`.
    ///
    /// # Panics
    ///
    /// Panics if `local` is out of range.
    #[must_use]
    pub fn local_ty(&self, local: Local) -> Ty {
        self.locals[local.0 as usize].ty
    }
}

/// Metadata attached to every [`Local`].
#[derive(Debug, Clone)]
pub struct LocalDecl {
    /// Type assigned to the local.
    pub ty: Ty,
    /// Optional source-level identifier that introduced this local.
    pub debug_name: Option<Ident>,
    /// `true` when the local is declared mutable at the source level.
    pub mutable: bool,
}

/// A basic block: a straight-line sequence of statements terminated by
/// a single [`Terminator`].
#[derive(Debug, Clone)]
pub struct BasicBlock {
    /// Stable id (matches this block's position in [`Body::blocks`]).
    pub id: BlockId,
    /// Straight-line body.
    pub stmts: Vec<Statement>,
    /// Control-flow terminator.
    pub terminator: Terminator,
    /// Source span covering the original construct.
    pub span: Span,
}

/// One statement inside a [`BasicBlock`].
#[derive(Debug, Clone)]
pub struct Statement {
    /// Statement kind.
    pub kind: StatementKind,
    /// Source span.
    pub span: Span,
}

/// Non-terminator statement kinds.
#[derive(Debug, Clone)]
pub enum StatementKind {
    /// `place = rvalue`. Copies (or moves) the value produced by
    /// `rvalue` into `place`. For aggregates the copy is a shallow
    /// bitwise copy of the flat layout; heap objects reachable through
    /// the value are handled by the GC write barrier.
    Assign {
        /// Destination place.
        place: Place,
        /// Right-hand value.
        rvalue: Rvalue,
    },
    /// Marks `local` as live. Emitted at block entry for temporaries.
    StorageLive(Local),
    /// Marks `local` as dead. Emitted when a temporary goes out of
    /// scope so the GC doesn't spuriously trace it.
    StorageDead(Local),
    /// Sets the active discriminant of an enum place to `variant`.
    SetDiscriminant {
        /// Place whose tag is being written.
        place: Place,
        /// Variant index within the enum's declaration order.
        variant: u32,
    },
    /// GC write barrier recording a pointer store for the concurrent
    /// mark phase. `place` is the mutated object; `value` is the
    /// reference being stored. The lowerer must emit this for **every**
    /// field or index assignment that may store a heap pointer so that
    /// both the interpreter and the native backend share the same
    /// collector invariants.
    GcWriteBarrier {
        /// Destination being mutated.
        place: Place,
        /// Reference that was just written.
        value: Operand,
    },
    /// No-op preserved for alignment with rustc-style MIR dumps.
    Nop,
}

/// Control-flow terminator closing a block.
#[derive(Debug, Clone)]
pub enum Terminator {
    /// Unconditional jump to `target`.
    Goto {
        /// Successor block.
        target: BlockId,
    },
    /// Multi-way branch on an integer discriminant. Evaluates
    /// `discriminant` to an integer and jumps to the block whose arm
    /// value equals it (integer equality). If no arm matches,
    /// control falls through to `default`. Used for `if`, `match`
    /// on integers/bools, and loop headers.
    SwitchInt {
        /// Scrutinee operand.
        discriminant: Operand,
        /// Match arms: each pair is `(value, target)`.
        arms: Vec<(i128, BlockId)>,
        /// Default arm taken when no explicit value matches.
        default: BlockId,
    },
    /// `return place_0` from the enclosing function.
    Return,
    /// Function call. Control transfers to `target` on normal return.
    Call {
        /// Callee operand (usually a constant function reference).
        callee: Operand,
        /// Call arguments in source order.
        args: Vec<Operand>,
        /// Destination place receiving the returned value.
        destination: Place,
        /// Continuation block. `None` encodes a diverging call.
        target: Option<BlockId>,
    },
    /// Runtime assertion (bounds / overflow). On failure jumps to a
    /// dedicated panic block.
    Assert {
        /// Assertion to evaluate.
        cond: Operand,
        /// `true` when the assertion fires when `cond` is truthy; the
        /// normal "assert cond is true" form uses `false`.
        expected: bool,
        /// Runtime message selector.
        msg: AssertMessage,
        /// Success continuation.
        target: BlockId,
    },
    /// Compiler knows this block is never reached at runtime.
    Unreachable,
    /// Unconditional panic: terminates the program with `message`.
    Panic {
        /// Human-readable reason.
        message: String,
    },
    /// Drops the value stored at `place` (invokes its `drop_fn` if
    /// any) and jumps to `target`.
    Drop {
        /// Place to drop.
        place: Place,
        /// Continuation after the drop completes.
        target: BlockId,
    },
}

/// Assertion message category — used by the runtime to produce
/// human-readable panic text without interpolating strings in emitted
/// code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AssertMessage {
    /// `index < len` failed for an indexing operation.
    BoundsCheck,
    /// Arithmetic overflow in debug mode.
    Overflow,
    /// Integer divide/modulo by zero.
    DivideByZero,
}

/// An lvalue — a place the IR can read from or write to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Place {
    /// Local the place is rooted in.
    pub local: Local,
    /// Projection chain applied to `local` from outermost to innermost.
    pub projection: Vec<Projection>,
}

impl Place {
    /// Returns a bare local with no projection.
    #[must_use]
    pub const fn local(local: Local) -> Self {
        Self {
            local,
            projection: Vec::new(),
        }
    }

    /// `true` when this place is a bare local with no projection.
    #[must_use]
    pub fn is_simple(&self) -> bool {
        self.projection.is_empty()
    }
}

/// One step in a place projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Projection {
    /// `*place` — dereference.
    Deref,
    /// `place.field` with the field's numeric index.
    Field(u32),
    /// `place[index]` — runtime array indexing.
    Index(Local),
    /// `place as variant` — access an enum's payload through an
    /// already-discriminated variant.
    Downcast(u32),
    /// The discriminant word of an enum place (read-only projection).
    Discriminant,
}

/// Operand form used by rvalues and terminators.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Operand {
    /// Copy/move the value stored at `place`.
    Copy(Place),
    /// Compile-time constant.
    Const(ConstValue),
    /// Reference to a named function plus the generic arguments it
    /// was instantiated with at this call site. Non-empty `substs`
    /// signal that the monomorphiser should produce a specialised
    /// copy of the callee body with a mangled name derived from the
    /// argument list.
    FnRef {
        /// `DefId` of the referenced function.
        def: DefId,
        /// Generic instantiation. Empty for monomorphic callees.
        substs: gossamer_types::Substs,
    },
}

/// Constant values surfaced in the IR.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConstValue {
    /// `()`.
    Unit,
    /// `bool`.
    Bool(bool),
    /// Signed 128-bit; narrower widths sit inside until codegen
    /// truncates.
    Int(i128),
    /// IEEE-754 binary64 as its bit pattern (so `PartialEq` holds).
    Float(u64),
    /// Unicode scalar value.
    Char(char),
    /// UTF-8 string constant.
    Str(String),
}

/// Right-hand side of an [`StatementKind::Assign`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rvalue {
    /// Plain operand read.
    Use(Operand),
    /// Binary operator applied to two operands.
    BinaryOp {
        /// Operator.
        op: BinOp,
        /// Left operand.
        lhs: Operand,
        /// Right operand.
        rhs: Operand,
    },
    /// Unary operator.
    UnaryOp {
        /// Operator.
        op: UnOp,
        /// Operand.
        operand: Operand,
    },
    /// `expr as T`. Converts the operand to the target type. Same-
    /// width integer casts are identity; narrowing, widening, and
    /// float conversions are representation changes that codegen must
    /// materialise.
    Cast {
        /// Operand being converted.
        operand: Operand,
        /// Target type after the cast.
        target: Ty,
    },
    /// Aggregate constructor. Builds a tuple, array, struct, or
    /// enum payload in a flat memory layout. Elements appear in
    /// declaration order; the codegen backend and the interpreter
    /// must agree on the same field offsets and discriminant word
    /// placement (see [`Projection::Field`] and
    /// [`StatementKind::SetDiscriminant`]).
    Aggregate {
        /// Aggregate kind.
        kind: AggregateKind,
        /// Element operands in declaration order.
        operands: Vec<Operand>,
    },
    /// `len(place)` — length of an array/vec/slice.
    Len(Place),
    /// `[value; count]` repeat constructor.
    Repeat {
        /// Repeated value.
        value: Operand,
        /// Compile-time count.
        count: u64,
    },
    /// `&place` or `&mut place`.
    Ref {
        /// `true` for `&mut`.
        mutable: bool,
        /// Referent place.
        place: Place,
    },
    /// Direct intrinsic call. Arguments are inline operands.
    CallIntrinsic {
        /// Intrinsic name.
        name: &'static str,
        /// Arguments.
        args: Vec<Operand>,
    },
}

/// Aggregate constructors surfaced by the lowerer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AggregateKind {
    /// Tuple with the given element types.
    Tuple,
    /// Struct-shaped aggregate.
    Adt {
        /// `DefId` of the struct/enum.
        def: DefId,
        /// Variant index for enums; `0` for structs.
        variant: u32,
    },
    /// Array literal with explicit elements.
    Array,
}

/// Binary operators supported at the MIR level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BinOp {
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
}

/// Unary operators supported at the MIR level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UnOp {
    /// `-x`.
    Neg,
    /// `!x`.
    Not,
}
