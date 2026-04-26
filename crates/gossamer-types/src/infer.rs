//! Hindley-Milner unification over the [`crate::TyCtxt`] interner.
//! The [`InferCtxt`] hands out fresh [`TyVid`]s and maintains a
//! union-find mapping from variables to either another variable (a
//! parent link) or to a concrete resolved [`Ty`]. The `unify` method
//! implements standard structural unification with an occurs check,
//! walking through the interner to look at `TyKind` payloads.

#![forbid(unsafe_code)]

use thiserror::Error;

use crate::context::TyCtxt;
use crate::subst::{GenericArg, Substs};
use crate::traits::TraitRef;
use crate::ty::{FnSig, IntTy, Ty, TyKind, TyVid};

/// One slot in the union-find table maintained by [`InferCtxt`].
#[derive(Debug, Clone, Copy)]
enum VarSlot {
    /// Unresolved variable whose parent is another variable id (root
    /// when `parent == self`).
    Parent(u32),
    /// Variable bound to a concrete type handle.
    Resolved(Ty),
}

/// Inference context: owns fresh-var allocation and the union-find
/// substitution table.
///
/// Variables come in two flavours:
/// * **Plain** — produced by [`InferCtxt::fresh_var`]; unifies with
///   any type.
/// * **Integer-constrained** — produced by
///   [`InferCtxt::fresh_int_var`]; only unifies with concrete integer
///   types. Used by the typechecker to give unsuffixed integer
///   literals (`42`, `0`, `0x2a`) Go-style "untyped constant"
///   semantics: the literal takes the integer type required by its
///   use site, falls back to `i64` when no constraint resolves it
///   ([`InferCtxt::default_unresolved_int_vars`]), and is rejected
///   when forced into a non-integer position.
#[derive(Debug, Clone)]
pub struct InferCtxt {
    slots: Vec<VarSlot>,
    /// Per-variable flag. `true` at index `i` means the var with id
    /// `i` was minted as integer-constrained. The flag is meaningful
    /// only on root variables (consumers should call [`Self::root_of`]
    /// before reading), and unions propagate the constraint through
    /// the union-find merge in [`Self::bind_var`].
    integer_constrained: Vec<bool>,
}

impl InferCtxt {
    /// Creates an empty inference context.
    #[must_use]
    pub fn new() -> Self {
        Self {
            slots: Vec::new(),
            integer_constrained: Vec::new(),
        }
    }

    /// Allocates a fresh unresolved inference variable that unifies
    /// with any type.
    pub fn fresh_var(&mut self, tcx: &mut TyCtxt) -> Ty {
        self.alloc_var(tcx, false)
    }

    /// Allocates a fresh inference variable constrained to integer
    /// types. See the [`InferCtxt`] doc comment for the model.
    pub fn fresh_int_var(&mut self, tcx: &mut TyCtxt) -> Ty {
        self.alloc_var(tcx, true)
    }

    fn alloc_var(&mut self, tcx: &mut TyCtxt, integer_constrained: bool) -> Ty {
        let idx = u32::try_from(self.slots.len()).expect("too many inference vars");
        self.slots.push(VarSlot::Parent(idx));
        self.integer_constrained.push(integer_constrained);
        tcx.intern(TyKind::Var(TyVid(idx)))
    }

    /// Defaults every integer-constrained variable that is still
    /// unresolved to `i64`. Called once at the end of typechecking
    /// so unsuffixed literals with no use-site constraint pick a
    /// concrete type instead of leaking through to lowering as a
    /// raw `Var`.
    pub fn default_unresolved_int_vars(&mut self, tcx: &mut TyCtxt) {
        let i64_ty = tcx.int_ty(IntTy::I64);
        let count = self.slots.len();
        for idx in 0..count {
            if !self.integer_constrained.get(idx).copied().unwrap_or(false) {
                continue;
            }
            let root = self.root_of(TyVid(idx as u32));
            if matches!(self.slots[root as usize], VarSlot::Parent(_))
                && self
                    .integer_constrained
                    .get(root as usize)
                    .copied()
                    .unwrap_or(false)
            {
                self.slots[root as usize] = VarSlot::Resolved(i64_ty);
            }
        }
    }

    /// Returns the current representative of a type handle. Inference
    /// variables are walked transitively to a non-variable type (or to
    /// the root variable if still unresolved).
    #[must_use]
    pub fn resolve(&self, tcx: &TyCtxt, ty: Ty) -> Ty {
        let mut current = ty;
        loop {
            let Some(TyKind::Var(vid)) = tcx.kind(current) else {
                return current;
            };
            let Some(next) = self.walk_var(tcx, *vid) else {
                return current;
            };
            if next == current {
                return current;
            }
            current = next;
        }
    }

    fn walk_var(&self, tcx: &TyCtxt, vid: TyVid) -> Option<Ty> {
        let mut idx = vid.0;
        loop {
            match self.slots.get(idx as usize)? {
                VarSlot::Parent(parent) if *parent == idx => {
                    return Some(lookup_var(tcx, idx));
                }
                VarSlot::Parent(parent) => idx = *parent,
                VarSlot::Resolved(resolved) => return Some(*resolved),
            }
        }
    }

    fn root_of(&mut self, vid: TyVid) -> u32 {
        let mut idx = vid.0;
        while let Some(VarSlot::Parent(parent)) = self.slots.get(idx as usize).copied() {
            if parent == idx {
                return idx;
            }
            idx = parent;
        }
        idx
    }

    fn bind(&mut self, vid: TyVid, ty: Ty) {
        let root = self.root_of(vid);
        self.slots[root as usize] = VarSlot::Resolved(ty);
    }

    /// Unifies two types, mutating the substitution table on success.
    pub fn unify(&mut self, tcx: &mut TyCtxt, lhs: Ty, rhs: Ty) -> Result<(), UnifyError> {
        let lhs = self.resolve(tcx, lhs);
        let rhs = self.resolve(tcx, rhs);
        if lhs == rhs {
            return Ok(());
        }
        let lhs_kind = tcx.kind_of(lhs).clone();
        let rhs_kind = tcx.kind_of(rhs).clone();
        self.unify_kinds(tcx, lhs, rhs, &lhs_kind, &rhs_kind)
    }

    fn unify_kinds(
        &mut self,
        tcx: &mut TyCtxt,
        lhs: Ty,
        rhs: Ty,
        lhs_kind: &TyKind,
        rhs_kind: &TyKind,
    ) -> Result<(), UnifyError> {
        match (lhs_kind, rhs_kind) {
            (TyKind::Var(vid), _) => self.bind_var(tcx, *vid, rhs),
            (_, TyKind::Var(vid)) => self.bind_var(tcx, *vid, lhs),
            (TyKind::Error | TyKind::Never, _) | (_, TyKind::Error | TyKind::Never) => Ok(()),
            _ if lhs_kind == rhs_kind => Ok(()),
            _ => self.unify_structural(tcx, lhs_kind, rhs_kind),
        }
    }

    fn bind_var(&mut self, tcx: &mut TyCtxt, vid: TyVid, ty: Ty) -> Result<(), UnifyError> {
        if occurs(self, tcx, vid, ty) {
            return Err(UnifyError::Occurs { var: vid });
        }
        let root = self.root_of(vid);
        let needs_int = self
            .integer_constrained
            .get(root as usize)
            .copied()
            .unwrap_or(false);
        if needs_int {
            let resolved = self.resolve(tcx, ty);
            match tcx.kind(resolved).cloned() {
                Some(TyKind::Int(_)) => {}
                Some(TyKind::Var(other_vid)) => {
                    // Propagate the integer constraint to the target
                    // var's root so the merged equivalence class
                    // remains integer-only.
                    let other_root = self.root_of(other_vid);
                    if other_root as usize >= self.integer_constrained.len() {
                        self.integer_constrained
                            .resize((other_root as usize) + 1, false);
                    }
                    self.integer_constrained[other_root as usize] = true;
                }
                _ => return Err(UnifyError::IntegerConstraint),
            }
        }
        self.bind(vid, ty);
        Ok(())
    }

    fn unify_structural(
        &mut self,
        tcx: &mut TyCtxt,
        lhs_kind: &TyKind,
        rhs_kind: &TyKind,
    ) -> Result<(), UnifyError> {
        match (lhs_kind, rhs_kind) {
            (TyKind::Tuple(a), TyKind::Tuple(b)) => self.unify_seq(tcx, a, b),
            (TyKind::Array { elem: ae, len: al }, TyKind::Array { elem: be, len: bl })
                if al == bl =>
            {
                self.unify(tcx, *ae, *be)
            }
            (TyKind::Slice(a), TyKind::Slice(b))
            | (TyKind::Vec(a), TyKind::Vec(b))
            | (TyKind::Sender(a), TyKind::Sender(b))
            | (TyKind::Receiver(a), TyKind::Receiver(b)) => self.unify(tcx, *a, *b),
            (TyKind::HashMap { key: ak, value: av }, TyKind::HashMap { key: bk, value: bv }) => {
                self.unify(tcx, *ak, *bk)?;
                self.unify(tcx, *av, *bv)
            }
            (
                TyKind::Ref {
                    mutability: am,
                    inner: ai,
                },
                TyKind::Ref {
                    mutability: bm,
                    inner: bi,
                },
            ) if am == bm => self.unify(tcx, *ai, *bi),
            (TyKind::FnPtr(a), TyKind::FnPtr(b)) => self.unify_fn_sig(tcx, a, b),
            (
                TyKind::Adt {
                    def: ad,
                    substs: asu,
                },
                TyKind::Adt {
                    def: bd,
                    substs: bsu,
                },
            )
            | (
                TyKind::Alias {
                    def: ad,
                    substs: asu,
                },
                TyKind::Alias {
                    def: bd,
                    substs: bsu,
                },
            )
            | (
                TyKind::FnDef {
                    def: ad,
                    substs: asu,
                },
                TyKind::FnDef {
                    def: bd,
                    substs: bsu,
                },
            ) if ad == bd => self.unify_substs(tcx, asu, bsu),
            (TyKind::Dyn(a), TyKind::Dyn(b)) => self.unify_trait_ref(tcx, a, b),
            _ => Err(UnifyError::Mismatch),
        }
    }

    fn unify_seq(&mut self, tcx: &mut TyCtxt, a: &[Ty], b: &[Ty]) -> Result<(), UnifyError> {
        if a.len() != b.len() {
            return Err(UnifyError::Mismatch);
        }
        for (x, y) in a.iter().zip(b) {
            self.unify(tcx, *x, *y)?;
        }
        Ok(())
    }

    fn unify_fn_sig(&mut self, tcx: &mut TyCtxt, a: &FnSig, b: &FnSig) -> Result<(), UnifyError> {
        self.unify_seq(tcx, &a.inputs, &b.inputs)?;
        self.unify(tcx, a.output, b.output)
    }

    fn unify_substs(&mut self, tcx: &mut TyCtxt, a: &Substs, b: &Substs) -> Result<(), UnifyError> {
        let a_args = a.as_slice();
        let b_args = b.as_slice();
        if a_args.len() != b_args.len() {
            return Err(UnifyError::Mismatch);
        }
        for (x, y) in a_args.iter().zip(b_args) {
            self.unify_arg(tcx, x, y)?;
        }
        Ok(())
    }

    fn unify_arg(
        &mut self,
        tcx: &mut TyCtxt,
        a: &GenericArg,
        b: &GenericArg,
    ) -> Result<(), UnifyError> {
        match (a, b) {
            (GenericArg::Type(x), GenericArg::Type(y)) => self.unify(tcx, *x, *y),
            (GenericArg::Const(x), GenericArg::Const(y)) if x == y => Ok(()),
            _ => Err(UnifyError::Mismatch),
        }
    }

    fn unify_trait_ref(
        &mut self,
        tcx: &mut TyCtxt,
        a: &TraitRef,
        b: &TraitRef,
    ) -> Result<(), UnifyError> {
        if a.def != b.def {
            return Err(UnifyError::Mismatch);
        }
        self.unify_substs(tcx, &a.substs, &b.substs)
    }
}

impl Default for InferCtxt {
    fn default() -> Self {
        Self::new()
    }
}

fn lookup_var(tcx: &TyCtxt, idx: u32) -> Ty {
    let kind = TyKind::Var(TyVid(idx));
    for (handle_idx, stored) in
        (0_u32..).zip((0..tcx.len()).filter_map(|i| tcx.kind(Ty(u32::try_from(i).unwrap()))))
    {
        if *stored == kind {
            return Ty(handle_idx);
        }
    }
    panic!("var {idx} not interned");
}

fn occurs(infer: &InferCtxt, tcx: &TyCtxt, vid: TyVid, ty: Ty) -> bool {
    let resolved = infer.resolve(tcx, ty);
    match tcx.kind(resolved) {
        Some(TyKind::Var(other)) => other.0 == vid.0,
        Some(kind) => occurs_in_kind(infer, tcx, vid, kind),
        None => false,
    }
}

fn occurs_in_kind(infer: &InferCtxt, tcx: &TyCtxt, vid: TyVid, kind: &TyKind) -> bool {
    match kind {
        TyKind::Tuple(parts) => parts.iter().any(|t| occurs(infer, tcx, vid, *t)),
        TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => {
            occurs(infer, tcx, vid, *elem)
        }
        TyKind::HashMap { key, value } => {
            occurs(infer, tcx, vid, *key) || occurs(infer, tcx, vid, *value)
        }
        TyKind::Sender(pointee)
        | TyKind::Receiver(pointee)
        | TyKind::Ref { inner: pointee, .. } => occurs(infer, tcx, vid, *pointee),
        TyKind::FnPtr(sig) => {
            sig.inputs.iter().any(|t| occurs(infer, tcx, vid, *t))
                || occurs(infer, tcx, vid, sig.output)
        }
        TyKind::FnDef { substs, .. }
        | TyKind::Adt { substs, .. }
        | TyKind::Alias { substs, .. }
        | TyKind::Closure { substs, .. } => occurs_in_substs(infer, tcx, vid, substs),
        TyKind::Dyn(trait_ref) => occurs_in_substs(infer, tcx, vid, &trait_ref.substs),
        TyKind::Bool
        | TyKind::Char
        | TyKind::String
        | TyKind::Int(_)
        | TyKind::Float(_)
        | TyKind::Unit
        | TyKind::Never
        | TyKind::Var(_)
        | TyKind::Param { .. }
        | TyKind::Error => false,
    }
}

fn occurs_in_substs(infer: &InferCtxt, tcx: &TyCtxt, vid: TyVid, substs: &Substs) -> bool {
    substs.as_slice().iter().any(|arg| match arg {
        GenericArg::Type(ty) => occurs(infer, tcx, vid, *ty),
        GenericArg::Const(_) => false,
    })
}

/// Unification failure reported by [`InferCtxt::unify`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum UnifyError {
    /// Types have incompatible shapes.
    #[error("type mismatch")]
    Mismatch,
    /// Binding the variable would produce an infinite type.
    #[error("occurs check: variable {var:?} appears in the type it's being unified with")]
    Occurs {
        /// The variable that would be self-referential.
        var: TyVid,
    },
    /// An integer-constrained inference variable (introduced by an
    /// unsuffixed integer literal) was forced into a non-integer
    /// position.
    #[error("integer literal cannot satisfy non-integer type constraint")]
    IntegerConstraint,
}
