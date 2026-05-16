#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Classifies an expression's natural result kind from its
    /// HIR `Ty`. Unknown / aggregate / polymorphic types stay in
    /// `Value`; `f64` → `F64`, all integer types → `I64`. When
    /// the type is unknown (walker / error type) we default to
    /// `Value` so the generic path handles it.
    pub(crate) fn expr_kind(&self, expr: &HirExpr) -> RegKind {
        match self.tcx.kind(expr.ty) {
            Some(TyKind::Float(FloatTy::F64)) => RegKind::F64,
            Some(TyKind::Int(_)) => RegKind::I64,
            _ => RegKind::Value,
        }
    }

    /// If `ty` is a named struct whose layout is known, returns
    /// the offset of `field_name` within its declaration-order
    /// field list. Used to fold field reads/writes to the
    /// `ByOffset` op variants that skip the runtime name scan.
    pub(crate) fn resolve_struct_field_offset(
        &self,
        ty: gossamer_types::Ty,
        field_name: &str,
    ) -> Option<u16> {
        let ty = self.unwrap_ref(ty);
        let kind = self.tcx.kind(ty)?;
        let def = match kind {
            TyKind::Adt { def, .. } => *def,
            _ => return None,
        };
        let fields = self.layouts.get(&def)?;
        let idx = fields.iter().position(|f| f == field_name)?;
        u16::try_from(idx).ok()
    }

    /// Peels any `&T` / `&mut T` reference layers off a `Ty`,
    /// so type-directed optimisations work through reference
    /// binders (`fn energy(b: &[Body; 5])`).
    pub(crate) fn unwrap_ref(&self, mut ty: gossamer_types::Ty) -> gossamer_types::Ty {
        loop {
            match self.tcx.kind(ty) {
                Some(TyKind::Ref { inner, .. }) => ty = *inner,
                _ => return ty,
            }
        }
    }

    /// Returns the element type of an array / vec / slice,
    /// peeling reference layers first.
    pub(crate) fn array_elem_ty(&self, ty: gossamer_types::Ty) -> Option<gossamer_types::Ty> {
        let ty = self.unwrap_ref(ty);
        match self.tcx.kind(ty) {
            Some(TyKind::Array { elem, .. } | TyKind::Vec(elem) | TyKind::Slice(elem)) => {
                Some(*elem)
            }
            _ => None,
        }
    }

    /// Returns `true` when `ty` resolves to `HashMap<i64, i64>`,
    /// the typed shape that rides through `Value::IntMap`. The
    /// resolver may already have erased one or both of the
    /// generic args when the inference variable couldn't be
    /// pinned; callers fall back to the boxed `Value::Map` in
    /// that case rather than risk a typed op crashing on a
    /// non-`i64` payload.
    pub(crate) fn is_int_map_ty(&self, ty: gossamer_types::Ty) -> bool {
        let ty = self.unwrap_ref(ty);
        let Some(TyKind::HashMap { key, value }) = self.tcx.kind(ty) else {
            return false;
        };
        let key_is_i64 = matches!(
            self.tcx.kind(*key),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        );
        let value_is_i64 = matches!(
            self.tcx.kind(*value),
            Some(TyKind::Int(IntTy::I64 | IntTy::Isize | IntTy::Usize))
        );
        key_is_i64 && value_is_i64
    }
}
