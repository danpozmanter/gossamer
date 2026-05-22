//! Type interner.
//! Types in Gossamer are content-addressed through the [`TyCtxt`]
//! interner. Interning returns a stable [`Ty`] handle whose equality
//! is pointer-equality on the backing table. All compiler passes share
//! a single context so that type comparisons are O(1).

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::ty::{FloatTy, IntTy, Ty, TyKind};

/// Interner that maps [`TyKind`]s to stable [`Ty`] handles.
#[derive(Debug, Default, Clone)]
pub struct TyCtxt {
    kinds: Vec<TyKind>,
    index: HashMap<TyKind, Ty>,
    primitives: Primitives,
    struct_fields: HashMap<gossamer_resolve::DefId, Vec<Ty>>,
    /// Human-readable names for ADT / alias / fn `DefId`s, indexed by
    /// the local component. Populated by the type checker for user
    /// structs and by sentinel registrations for `Result`/`Option`.
    def_names: HashMap<gossamer_resolve::DefId, String>,
    /// Reference-counting type-meta blobs, keyed by the codegen symbol
    /// name (`gos_rc_meta_<id>`). Populated by MIR lowering when it
    /// emits a `gos_rc_alloc` for an RC-managed ADT; consumed by both
    /// codegen backends to emit one module-global constant per blob and
    /// reference its address at the allocation site. The blob is the
    /// flat `[i64]` child-layout format the runtime's `gos_rt_rc_release`
    /// walks (see `gossamer-runtime` `c_abi::rc`).
    rc_metas: HashMap<String, Vec<i64>>,
    /// Interned self-types of every heap-allocated (RC-managed) user
    /// enum. Populated by MIR lowering from the enum index; consulted by
    /// the drop pass to recognise locals holding RC pointers that need a
    /// `gos_rt_rc_release` at end of life. Membership must stay
    /// conservative — see `rc_enum_tys` in the MIR enum index.
    rc_managed_tys: std::collections::HashSet<Ty>,
    /// `DefId.local` of user enums whose every variant has at most one
    /// field that fits in a single 8-byte slot. Such enums use the 2-word
    /// by-value `i128` [disc, payload] representation (no heap node);
    /// `render_ty` / `cl_type_of` map them to `i128` and `is_rc_managed`
    /// reports them as values (their payload, if a managed pointer, is
    /// released per-discriminant on drop).
    inline_enum_defs: std::collections::HashSet<u32>,
}

/// Cached handles for the primitive types that every program uses. The
/// table is populated lazily on the first call to the corresponding
/// accessor.
#[derive(Debug, Default, Clone)]
struct Primitives {
    unit: Option<Ty>,
    never: Option<Ty>,
    bool_: Option<Ty>,
    char_: Option<Ty>,
    string_: Option<Ty>,
    error: Option<Ty>,
    json_value: Option<Ty>,
    dyn_error: Option<Ty>,
}

impl TyCtxt {
    /// Returns a fresh interner with no entries.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Interns `kind` and returns its stable handle. Calling this with
    /// two structurally-equal `TyKind`s returns the same [`Ty`].
    pub fn intern(&mut self, kind: TyKind) -> Ty {
        if let Some(ty) = self.index.get(&kind) {
            return *ty;
        }
        let ty = Ty(u32::try_from(self.kinds.len()).expect("ty interner overflow"));
        self.kinds.push(kind.clone());
        self.index.insert(kind, ty);
        ty
    }

    /// Looks up the [`TyKind`] backing a handle. Returns [`None`] if
    /// `ty` was not produced by this interner.
    #[must_use]
    pub fn kind(&self, ty: Ty) -> Option<&TyKind> {
        self.kinds.get(ty.0 as usize)
    }

    /// Borrows `kind(ty)`, panicking if the handle is not owned by this
    /// interner. Used in contexts where the caller knows the handle is
    /// valid (e.g. after a prior `intern`).
    ///
    /// # Panics
    ///
    /// Panics when `ty` was not produced by this interner.
    #[must_use]
    pub fn kind_of(&self, ty: Ty) -> &TyKind {
        self.kind(ty).expect("ty handle not owned by this interner")
    }

    /// Number of interned types, useful for tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.kinds.len()
    }

    /// Returns `true` when no types have been interned.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.kinds.is_empty()
    }

    /// Interns `()`.
    pub fn unit(&mut self) -> Ty {
        if let Some(ty) = self.primitives.unit {
            return ty;
        }
        let ty = self.intern(TyKind::Unit);
        self.primitives.unit = Some(ty);
        ty
    }

    /// Interns `!`.
    pub fn never(&mut self) -> Ty {
        if let Some(ty) = self.primitives.never {
            return ty;
        }
        let ty = self.intern(TyKind::Never);
        self.primitives.never = Some(ty);
        ty
    }

    /// Interns `bool`.
    pub fn bool_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.bool_ {
            return ty;
        }
        let ty = self.intern(TyKind::Bool);
        self.primitives.bool_ = Some(ty);
        ty
    }

    /// Interns `char`.
    pub fn char_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.char_ {
            return ty;
        }
        let ty = self.intern(TyKind::Char);
        self.primitives.char_ = Some(ty);
        ty
    }

    /// Interns `String`.
    pub fn string_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.string_ {
            return ty;
        }
        let ty = self.intern(TyKind::String);
        self.primitives.string_ = Some(ty);
        ty
    }

    /// Interns the poisoned [`TyKind::Error`] sentinel.
    pub fn error_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.error {
            return ty;
        }
        let ty = self.intern(TyKind::Error);
        self.primitives.error = Some(ty);
        ty
    }

    /// Interns the opaque dynamic JSON value type. Cached because
    /// MIR lowering and type checking both reach for it on every
    /// `json::Value`-typed expression.
    pub fn json_value_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.json_value {
            return ty;
        }
        let ty = self.intern(TyKind::JsonValue);
        self.primitives.json_value = Some(ty);
        ty
    }

    /// Interns the opaque dynamic `errors::Error` type.
    pub fn dyn_error_ty(&mut self) -> Ty {
        if let Some(ty) = self.primitives.dyn_error {
            return ty;
        }
        let ty = self.intern(TyKind::DynError);
        self.primitives.dyn_error = Some(ty);
        ty
    }

    /// Interns an integer primitive.
    pub fn int_ty(&mut self, int: IntTy) -> Ty {
        self.intern(TyKind::Int(int))
    }

    /// Interns a floating-point primitive.
    pub fn float_ty(&mut self, float: FloatTy) -> Ty {
        self.intern(TyKind::Float(float))
    }

    /// Records the field types of a named struct in source order.
    /// Called by the typechecker once per struct declaration.
    pub fn register_struct_fields(&mut self, def: gossamer_resolve::DefId, fields: Vec<Ty>) {
        self.struct_fields.insert(def, fields);
    }

    /// Returns the registered field types of the struct identified
    /// by `def`, or `None` when no registration has been made.
    #[must_use]
    pub fn struct_field_tys(&self, def: gossamer_resolve::DefId) -> Option<&[Ty]> {
        self.struct_fields.get(&def).map(Vec::as_slice)
    }

    /// Records a human-readable name for the given `DefId`.
    /// Overwrites any previous registration.
    pub fn register_def_name(&mut self, def: gossamer_resolve::DefId, name: impl Into<String>) {
        self.def_names.insert(def, name.into());
    }

    /// Returns the registered name for `def`, or `None`.
    #[must_use]
    pub fn def_name(&self, def: gossamer_resolve::DefId) -> Option<&str> {
        self.def_names.get(&def).map(String::as_str)
    }

    /// Records a reference-counting type-meta blob under `symbol`,
    /// keeping the first registration if one already exists (the blob is
    /// a pure function of the type, so duplicates are identical).
    pub fn register_rc_meta(&mut self, symbol: impl Into<String>, blob: Vec<i64>) {
        self.rc_metas.entry(symbol.into()).or_insert(blob);
    }

    /// Returns the RC type-meta blob registered under `symbol`, if any.
    #[must_use]
    pub fn rc_meta(&self, symbol: &str) -> Option<&[i64]> {
        self.rc_metas.get(symbol).map(Vec::as_slice)
    }

    /// Iterates every registered `(symbol, blob)` pair so a codegen
    /// backend can emit one module-global constant per RC type-meta.
    pub fn rc_metas(&self) -> impl Iterator<Item = (&str, &[i64])> {
        self.rc_metas
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    /// Records that `ty` is a heap-allocated (RC-managed) user enum.
    pub fn register_rc_managed_ty(&mut self, ty: Ty) {
        self.rc_managed_tys.insert(ty);
    }

    /// Registers a user enum (by `DefId.local`) as inline-able — its values
    /// are the 2-word by-value `i128` representation.
    pub fn register_inline_enum_def(&mut self, def_local: u32) {
        self.inline_enum_defs.insert(def_local);
    }

    /// True when `ty` is an inline-able user enum (2-word by-value).
    #[must_use]
    pub fn is_inline_enum_ty(&self, ty: Ty) -> bool {
        matches!(
            self.kind(ty),
            Some(TyKind::Adt { def, .. }) if self.inline_enum_defs.contains(&def.local)
        )
    }

    /// Returns true when `ty` is a heap-allocated user enum whose values
    /// are reference-counted (`gos_rc_alloc`) and must be released with
    /// `gos_rt_rc_release` at end of life.
    #[must_use]
    pub fn is_rc_managed(&self, ty: Ty) -> bool {
        // `Result<T,E>` (sentinel `u32::MAX`) and `Option<T>` (`u32::MAX - 1`)
        // are by-value 2-word (`i128`) values, NOT RC pointers. Their payload
        // is RC-managed through the binding it is extracted into; the
        // Result/Option local itself must never be RC-released (that would
        // treat the packed `i128` as a pointer and corrupt the heap).
        if matches!(
            self.kind(ty),
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX || def.local == u32::MAX - 1
        ) {
            return false;
        }
        // Inline-able user enums are by-value (their payload's RC, if any, is
        // released per-discriminant on drop), never RC-managed as a whole.
        if self.is_inline_enum_ty(ty) {
            return false;
        }
        if matches!(self.kind(ty), Some(TyKind::String)) {
            return true;
        }
        if self.rc_managed_tys.contains(&ty) {
            return true;
        }
        // `Weak<T>` (sentinel def `u32::MAX - 6`) is a weak-counted
        // pointer into an RC allocation, regardless of its payload
        // substitution. It must be processed by the drop pass — which
        // branches on `is_weak_ty` to emit the weak retain/release
        // helpers instead of the strong ones — so report it here even
        // though no per-instantiation registration exists.
        self.is_weak_ty(ty)
    }

    /// Returns true when `ty` is a `Weak<T>` reference (the sentinel
    /// `DefId` `u32::MAX - 6`). Used by the drop pass to route the
    /// retain/release of a weak binding through
    /// `gos_rt_rc_weak_retain` / `gos_rt_rc_weak_release`.
    #[must_use]
    pub fn is_weak_ty(&self, ty: Ty) -> bool {
        matches!(
            self.kind(ty),
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX - 6
        )
    }
}
