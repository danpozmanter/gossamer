//! Core type representation shared across the type-checker, trait
//! solver, and later IR passes.
//! The [`Ty`] handle is a cheap `Copy` wrapper around an index into the
//! [`crate::TyCtxt`] interner. Structural type data lives in [`TyKind`].
//! Two semantically identical types always intern to the same [`Ty`],
//! so pointer-equality can stand in for structural equality.

#![forbid(unsafe_code)]

use gossamer_resolve::DefId;

use crate::subst::Substs;
use crate::traits::TraitRef;

/// Interner handle for a type. Cheap to copy; meaningful only when
/// paired with the [`crate::TyCtxt`] that produced it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Ty(pub(crate) u32);

impl Ty {
    /// Raw numeric index into the interner, useful for stable sort
    /// orders and debug output.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Width tag for signed and unsigned integer types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum IntTy {
    /// Signed 8-bit integer.
    I8,
    /// Signed 16-bit integer.
    I16,
    /// Signed 32-bit integer.
    I32,
    /// Signed 64-bit integer.
    I64,
    /// Signed 128-bit integer.
    I128,
    /// Signed pointer-sized integer.
    Isize,
    /// Unsigned 8-bit integer.
    U8,
    /// Unsigned 16-bit integer.
    U16,
    /// Unsigned 32-bit integer.
    U32,
    /// Unsigned 64-bit integer.
    U64,
    /// Unsigned 128-bit integer.
    U128,
    /// Unsigned pointer-sized integer.
    Usize,
}

impl IntTy {
    /// Returns the source-level name of this integer type (e.g. `i32`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::I8 => "i8",
            Self::I16 => "i16",
            Self::I32 => "i32",
            Self::I64 => "i64",
            Self::I128 => "i128",
            Self::Isize => "isize",
            Self::U8 => "u8",
            Self::U16 => "u16",
            Self::U32 => "u32",
            Self::U64 => "u64",
            Self::U128 => "u128",
            Self::Usize => "usize",
        }
    }

    /// Returns `true` when this integer type is signed.
    #[must_use]
    pub const fn is_signed(self) -> bool {
        matches!(
            self,
            Self::I8 | Self::I16 | Self::I32 | Self::I64 | Self::I128 | Self::Isize
        )
    }
}

/// Width tag for floating-point types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FloatTy {
    /// 32-bit IEEE-754 binary32.
    F32,
    /// 64-bit IEEE-754 binary64.
    F64,
}

impl FloatTy {
    /// Returns the source-level name of this float type (e.g. `f64`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
            Self::F64 => "f64",
        }
    }
}

/// Reference mutability marker used by [`TyKind::Ref`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mutbl {
    /// `&T` — shared GC reference.
    Not,
    /// `&mut T` — exclusive GC reference.
    Mut,
}

impl Mutbl {
    /// Returns the keyword form used when printing reference types.
    #[must_use]
    pub const fn prefix(self) -> &'static str {
        match self {
            Self::Not => "&",
            Self::Mut => "&mut ",
        }
    }
}

/// Type-inference variable identifier produced by
/// [`crate::InferCtxt::fresh_var`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TyVid(pub u32);

impl TyVid {
    /// Returns the raw numeric index of this variable.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Zero-based index of a bound generic parameter within its defining
/// item's generics list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ParamIdx(pub u32);

impl ParamIdx {
    /// Returns the raw numeric index of this parameter.
    #[must_use]
    pub const fn as_u32(self) -> u32 {
        self.0
    }
}

/// Signature of a bare function pointer or `fn`-typed item.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FnSig {
    /// Parameter types in source order.
    pub inputs: Vec<Ty>,
    /// Return type (use the interned unit type for `()`).
    pub output: Ty,
}

/// Closure-trait kind attached to a [`TyKind::Closure`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ClosureKind {
    /// Non-mutating closure (`Fn`).
    Fn,
    /// Mutating closure (`FnMut`).
    FnMut,
    /// Owning closure (`FnOnce`).
    FnOnce,
}

impl ClosureKind {
    /// Returns the source-level trait spelling of this closure kind.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fn => "Fn",
            Self::FnMut => "FnMut",
            Self::FnOnce => "FnOnce",
        }
    }
}

/// Structural payload of an interned type.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum TyKind {
    /// `bool`.
    Bool,
    /// `char`.
    Char,
    /// `String` — GC-backed UTF-8 string.
    String,
    /// Signed or unsigned integer types `i8`..`usize`.
    Int(IntTy),
    /// Floating-point types `f32` / `f64`.
    Float(FloatTy),
    /// `()` — the unit type.
    Unit,
    /// `!` — the never type.
    Never,
    /// Tuple type `(T1, ..., Tn)` with two or more elements.
    Tuple(Vec<Ty>),
    /// Fixed-size array `[T; N]`.
    Array {
        /// Element type.
        elem: Ty,
        /// Element count.
        len: usize,
    },
    /// Unsized slice `[T]`, always seen through a reference at runtime.
    Slice(Ty),
    /// `Vec<T>` — built-in growable sequence.
    Vec(Ty),
    /// `HashMap<K, V>` — built-in hash map.
    HashMap {
        /// Key type.
        key: Ty,
        /// Value type.
        value: Ty,
    },
    /// `Sender<T>` — channel send endpoint.
    Sender(Ty),
    /// `Receiver<T>` — channel receive endpoint.
    Receiver(Ty),
    /// `json::Value` — opaque dynamic JSON node. Carries no
    /// generic parameters; the runtime backs every node with a
    /// boxed `serde_json::Value`. Field access on a `JsonValue`
    /// receiver is rewritten by MIR lowering into a runtime
    /// `gos_rt_json_get(receiver, "field")` call.
    JsonValue,
    /// GC reference type `&T` or `&mut T`.
    Ref {
        /// Mutability of the reference.
        mutability: Mutbl,
        /// Pointee type.
        inner: Ty,
    },
    /// Reference to a specific function definition.
    FnDef {
        /// `DefId` of the function.
        def: DefId,
        /// Generic substitutions instantiating the function.
        substs: Substs,
    },
    /// Anonymous function-pointer type `fn(...) -> ...`.
    FnPtr(FnSig),
    /// Callable trait type `Fn(args) -> ret` — accepts both bare
    /// `fn` items and capturing closures via implicit coercion.
    /// Lowered as a `(env_ptr, code_ptr)` fat pointer (two
    /// consecutive `i64` slots) so the env that a capturing
    /// closure needs has a place to live, and so a bare item can
    /// still satisfy the type by setting `env` to null. Mirrors
    /// Rust's `dyn Fn(args) -> ret` shape; a single trait covers
    /// the common case (no `FnMut` / `FnOnce` split for v1.0.0
    /// since every captured value is GC-managed).
    FnTrait(FnSig),
    /// Anonymous closure type, tied to the expression that introduced it.
    Closure {
        /// `DefId` of the closure (normally the enclosing expression's
        /// synthetic item id).
        def: DefId,
        /// Captured-type substitutions.
        substs: Substs,
        /// Closure-trait kind.
        kind: ClosureKind,
    },
    /// Named ADT (struct or enum) instantiation.
    Adt {
        /// `DefId` of the ADT.
        def: DefId,
        /// Generic substitutions.
        substs: Substs,
    },
    /// Type alias reference `type Alias<G> = Target`.
    Alias {
        /// `DefId` of the alias.
        def: DefId,
        /// Generic substitutions.
        substs: Substs,
    },
    /// Dynamic trait object `dyn Trait<Args>`.
    Dyn(TraitRef),
    /// Unresolved inference variable introduced during unification.
    Var(TyVid),
    /// Bound type parameter referring to the `ParamIdx`-th generic of
    /// its defining item.
    Param {
        /// Position in the parent generics list.
        idx: ParamIdx,
        /// Source-level name for diagnostics (`T`, `U`, ...).
        name: &'static str,
    },
    /// A type that could not be resolved; diagnostics have already been
    /// produced.
    Error,
}

impl TyKind {
    /// Returns `true` for the primitive numeric, boolean, character,
    /// unit, or never kinds.
    #[must_use]
    pub const fn is_primitive(&self) -> bool {
        matches!(
            self,
            Self::Bool
                | Self::Char
                | Self::Int(_)
                | Self::Float(_)
                | Self::Unit
                | Self::Never
                | Self::String
        )
    }
}
