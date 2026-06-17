#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Classifies an expression's natural result kind from its
    /// HIR `Ty`. Unknown / aggregate / polymorphic types stay in
    /// `Value`; `f64` → `F64`, all integer types → `I64`. When
    /// the type is unknown (an unresolved / error type) we default to
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

    /// Bare type name to dispatch a struct `==` / `!=` through its
    /// derived `<Type>::eq` method, seeing through `&` / `&mut`. Returns
    /// `Some` only for a *struct* whose layout the compiler knows (an
    /// entry in `layouts`); enums return `None` so they compare
    /// structurally via the native `Op::Eq` (only `Value::Struct` routes
    /// through `Type::eq`). Mirrors the MIR builder's `adt_dispatch_name`
    /// for the struct case.
    pub(crate) fn struct_eq_dispatch_name(&self, ty: gossamer_types::Ty) -> Option<String> {
        let ty = self.unwrap_ref(ty);
        let TyKind::Adt { def, .. } = self.tcx.kind(ty)? else {
            return None;
        };
        if !self.layouts.contains_key(def) {
            return None;
        }
        let rendered = gossamer_types::printer::render_ty(self.tcx, ty);
        let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
        // The synthesized `eq` registers under the struct's bare source
        // name, so drop any generic-argument suffix (`Wrap<i64>` → `Wrap`).
        let bare = bare.split('<').next().unwrap_or(bare);
        if bare.is_empty() || bare.starts_with("adt#") {
            return None;
        }
        Some(bare.to_string())
    }

    /// `true` when `ty` resolves (through `&` / `&mut` layers) to a
    /// nominal `Adt` (struct or enum). Used to confirm a `&mut self`
    /// receiver is an aggregate before marking it for the write-back
    /// cell protocol.
    pub(crate) fn is_adt_ref(&self, ty: gossamer_types::Ty) -> bool {
        matches!(self.tcx.kind(self.unwrap_ref(ty)), Some(TyKind::Adt { .. }))
    }

    /// Bare nominal name of an `Adt` value's type (`Counter`,
    /// `Stack` for a `Stack<i64>`), seeing through `&` / `&mut`. Used to
    /// reconstruct the `Type::method` key a `&mut self` call dispatches
    /// to, so the call site can consult [`FnBuilder::method_muts`].
    /// Returns `None` for non-`Adt` receivers (primitives, collections)
    /// and synthesized anonymous types.
    pub(crate) fn adt_type_name(&self, ty: gossamer_types::Ty) -> Option<String> {
        let ty = self.unwrap_ref(ty);
        let TyKind::Adt { .. } = self.tcx.kind(ty)? else {
            return None;
        };
        let rendered = gossamer_types::printer::render_ty(self.tcx, ty);
        let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
        let bare = bare.split('<').next().unwrap_or(bare);
        if bare.is_empty() || bare.starts_with("adt#") {
            return None;
        }
        Some(bare.to_string())
    }

    /// `true` when `ty` (through `&` / `&mut` layers) is an array, vec,
    /// or slice - a collection the for-loop fast path can drive by index
    /// via `len()` + `IndexGet`. User `impl Iterator` types (`Adt`) are
    /// excluded so their stateful `next()` keeps its own desugar.
    pub(crate) fn is_indexable_collection_ty(&self, ty: gossamer_types::Ty) -> bool {
        let ty = self.unwrap_ref(ty);
        matches!(
            self.tcx.kind(ty),
            Some(TyKind::Array { .. } | TyKind::Vec(_) | TyKind::Slice(_))
        )
    }

    /// Collects the names a pattern binds to an array / vec / slice
    /// value, deriving each binding's type from the resolved `scrut_ty`
    /// (the type of the value the pattern matches). The for-loop fast path
    /// uses the resulting `collection_locals` tags to drive `for x in
    /// <binding>` by index even when the binding's own HIR type stayed an
    /// inference var.
    pub(crate) fn collect_collection_binding_names(
        &self,
        pat: &HirPat,
        scrut_ty: Option<gossamer_types::Ty>,
        out: &mut Vec<String>,
    ) {
        match &pat.kind {
            HirPatKind::Binding { name, .. } => {
                if scrut_ty.is_some_and(|t| self.is_indexable_collection_ty(t)) {
                    out.push(name.name.clone());
                }
            }
            HirPatKind::At { name, sub, .. } => {
                if scrut_ty.is_some_and(|t| self.is_indexable_collection_ty(t)) {
                    out.push(name.name.clone());
                }
                self.collect_collection_binding_names(sub, scrut_ty, out);
            }
            HirPatKind::Variant { name, fields } => {
                for (i, fp) in fields.iter().enumerate() {
                    let fty = self.variant_payload_ty(scrut_ty, name.name.as_str(), i);
                    self.collect_collection_binding_names(fp, fty, out);
                }
            }
            HirPatKind::Struct { fields, .. } => {
                for f in fields {
                    let fty = self.struct_pat_field_ty(scrut_ty, f.name.name.as_str());
                    match &f.pattern {
                        Some(p) => self.collect_collection_binding_names(p, fty, out),
                        None => {
                            if fty.is_some_and(|t| self.is_indexable_collection_ty(t)) {
                                out.push(f.name.name.clone());
                            }
                        }
                    }
                }
            }
            HirPatKind::Tuple(parts) => {
                for (i, p) in parts.iter().enumerate() {
                    let ety = self.tuple_elem_ty(scrut_ty, i);
                    self.collect_collection_binding_names(p, ety, out);
                }
            }
            HirPatKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                let ety = scrut_ty.and_then(|t| self.array_elem_ty(t));
                for p in prefix {
                    self.collect_collection_binding_names(p, ety, out);
                }
                if let Some(rest) = rest {
                    // The `..rest` sub-slice has the same indexable
                    // collection type as the scrutinee.
                    self.collect_collection_binding_names(rest, scrut_ty, out);
                }
                for p in suffix {
                    self.collect_collection_binding_names(p, ety, out);
                }
            }
            HirPatKind::Ref { inner, .. } => {
                let inner_ty = scrut_ty.map(|t| self.unwrap_ref(t));
                self.collect_collection_binding_names(inner, inner_ty, out);
            }
            HirPatKind::Or(alts) => {
                for alt in alts {
                    self.collect_collection_binding_names(alt, scrut_ty, out);
                }
            }
            HirPatKind::Wildcard
            | HirPatKind::Rest
            | HirPatKind::Literal(_)
            | HirPatKind::Range { .. } => {}
        }
    }

    /// Payload type of a matched `Option` / `Result` variant field. Only
    /// these two generic enums are resolved here (their substitution maps
    /// directly to the variant payload); other enums return `None`.
    fn variant_payload_ty(
        &self,
        scrut_ty: Option<gossamer_types::Ty>,
        variant: &str,
        idx: usize,
    ) -> Option<gossamer_types::Ty> {
        let ty = self.unwrap_ref(scrut_ty?);
        let TyKind::Adt { def, substs } = self.tcx.kind(ty)? else {
            return None;
        };
        let types = substs.types();
        // `Option<T>` (sentinel def `u32::MAX - 1`): `Some(T)` → `types[0]`.
        if def.local == u32::MAX - 1 {
            return types.get(idx).copied();
        }
        // `Result<T, E>` (sentinel def `u32::MAX`): `Ok` → `T`, `Err` → `E`.
        if def.local == u32::MAX {
            return match variant {
                "Ok" => types.first().copied(),
                "Err" => types.get(1).copied(),
                _ => None,
            };
        }
        None
    }

    /// Element type at `idx` of a tuple-typed value, if known.
    fn tuple_elem_ty(
        &self,
        scrut_ty: Option<gossamer_types::Ty>,
        idx: usize,
    ) -> Option<gossamer_types::Ty> {
        let ty = self.unwrap_ref(scrut_ty?);
        let TyKind::Tuple(elems) = self.tcx.kind(ty)? else {
            return None;
        };
        elems.get(idx).copied()
    }

    /// Declared type of struct field `field_name` on a struct-typed value,
    /// if the struct's layout is known (non-generic resolution).
    fn struct_pat_field_ty(
        &self,
        scrut_ty: Option<gossamer_types::Ty>,
        field_name: &str,
    ) -> Option<gossamer_types::Ty> {
        let ty = self.unwrap_ref(scrut_ty?);
        let TyKind::Adt { def, .. } = self.tcx.kind(ty)? else {
            return None;
        };
        let names = self.layouts.get(def)?;
        let idx = names.iter().position(|f| f == field_name)?;
        self.tcx.struct_field_tys(*def)?.get(idx).copied()
    }

    /// `true` when the `for`-loop receiver `expr` is an indexable
    /// collection - either by its resolved type or, for a pattern-bound
    /// local whose inferred type stayed an unresolved var, by the
    /// `collection_locals` tag recorded at bind time.
    pub(crate) fn receiver_is_collection(&self, expr: &HirExpr) -> bool {
        if self.is_indexable_collection_ty(expr.ty) {
            return true;
        }
        if let HirExprKind::Path { segments, .. } = &expr.kind {
            if segments.len() == 1 {
                if let Some(tr) = self.lookup_local(&segments[0].name) {
                    return self.collection_locals.contains(&tr.reg);
                }
            }
        }
        false
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
