#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
#![forbid(unsafe_code)]
use super::*;
use std::collections::HashMap;
use std::fmt::Write as _;

use crate::BuildError;
use anyhow::Result;
use gossamer_abi as abi;
use gossamer_mir::{
    BasicBlock, BinOp, Body, ConstValue, Local, Operand, Place, Projection, Rvalue, Statement,
    StatementKind, Terminator, UnOp,
};
use gossamer_types::{FloatTy, IntTy, Ty, TyCtxt, TyKind};

impl<'a> Lowerer<'a> {
    /// If `place`'s final projection is an `Index` whose
    /// preceding type resolves to `String` / `&String`, emit
    /// a runtime call to `gos_rt_str_byte_at(ptr, idx) ->
    /// i64` and return the byte value. Returns `None` for
    /// non-string indexing so the caller falls through to the
    /// generic aggregate walk.
    pub(crate) fn try_string_byte_read(&mut self, place: &Place) -> Option<String> {
        if place.projection.is_empty() {
            return None;
        }
        // Walk every projection except the last, resolving
        // the type after each step — the final one must be
        // `Index` on a `String`.
        let (prefix, last) = place.projection.split_at(place.projection.len() - 1);
        let Projection::Index(idx_local) = &last[0] else {
            return None;
        };
        // Compute the type the last-step operates on by
        // walking `prefix`.
        let mut ty = self.body.local_ty(place.local);
        for proj in prefix {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(i) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => self.unwrap_ref(ty),
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        ty = self.unwrap_ref(ty);
        if !matches!(self.tcx.kind(ty), Some(TyKind::String)) {
            return None;
        }
        // Resolve the pointer to the string. With no prefix
        // projections, that's the local's loaded value; with
        // prefix projections it's the projected address, then
        // a load of `ptr`.
        let str_ptr = if prefix.is_empty() {
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = load ptr, ptr {slot}",
                slot = local_slot(place.local),
            )
            .unwrap();
            tmp
        } else {
            let prefix_place = Place {
                local: place.local,
                projection: prefix.to_vec(),
            };
            let addr = self.lower_place_address(&prefix_place);
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {addr}").unwrap();
            tmp
        };
        // Load the i64 index value.
        let idx_tmp = self.fresh();
        writeln!(
            self.out,
            "  {idx_tmp} = load i64, ptr {slot}",
            slot = local_slot(*idx_local)
        )
        .unwrap();
        declare_rt(&mut self.runtime_refs, "gos_rt_str_byte_at");
        let out = self.fresh();
        writeln!(
            self.out,
            "  {out} = call i64 @gos_rt_str_byte_at(ptr {str_ptr}, i64 {idx_tmp})"
        )
        .unwrap();
        Some(out)
    }

    /// Resolves the leaf type of a projection chain: the type
    /// the final `load`/`store` should use. Walks the MIR
    /// projections the same way the runtime does — an `Index`
    /// on an array yields the element type, a `Field` on a
    /// struct yields the field's type, etc. Auto-peels `&T` /
    /// `&mut T` reference layers at each step so the same
    /// code path handles `fn energy(b: &[Body; 5])`-style
    /// reference parameters whose MIR may or may not carry
    /// an explicit `Deref` projection.
    pub(crate) fn place_leaf_ty(&self, place: &Place) -> Ty {
        let mut ty = self.body.local_ty(place.local);
        for proj in &place.projection {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(idx) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*idx as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*idx as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => match self.tcx.kind(ty) {
                    Some(TyKind::Ref { inner, .. }) => *inner,
                    _ => ty,
                },
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        ty
    }

    /// True when the local's slot holds a pointer to the actual
    /// data (rather than the data itself). Reference types,
    /// runtime-managed shapes (`Vec`, `Slice`, `String`,
    /// `HashMap`, channels, dyn objects), and function pointers
    /// all live as `ptr` in the slot. Stack-allocated
    /// aggregates (Array / Tuple / Adt declared inline) hold
    /// their data in-place. Anything classified as opaque
    /// `ptr` by the type renderer that *isn't* a stack
    /// aggregate is treated as a pointer-bearing slot — this
    /// catches inference variables that left the typeck pipeline
    /// unresolved (a runtime call like `os::args()` whose return
    /// type is materialised at MIR time but never gets a concrete
    /// `Vec` resolution).
    pub(crate) fn is_pointer_local_ty(tcx: &TyCtxt, ty: Ty) -> bool {
        if matches!(
            tcx.kind(ty),
            Some(
                TyKind::Ref { .. }
                    | TyKind::Vec(_)
                    | TyKind::Slice(_)
                    | TyKind::String
                    | TyKind::HashMap { .. }
                    | TyKind::Sender(_)
                    | TyKind::Receiver(_)
                    | TyKind::JoinHandle(_)
                    | TyKind::Dyn(_)
                    | TyKind::FnPtr(_)
                    | TyKind::FnDef { .. }
            )
        ) {
            return true;
        }
        // For unresolved inference variables / opaque shapes,
        // the alloca was built as `ptr` (see `emit_allocas`).
        // Treat those as pointer-bearing too.
        !is_aggregate(tcx, ty) && render_ty(tcx, ty) == "ptr" && !is_unit(tcx, ty)
    }

    /// Peels any `&T` / `&mut T` layers off `ty` so subsequent
    /// type-dependent work (struct-field offset lookup, array
    /// stride calculation) sees the underlying aggregate.
    pub(crate) fn unwrap_ref(&self, mut ty: Ty) -> Ty {
        loop {
            match self.tcx.kind(ty) {
                Some(TyKind::Ref { inner, .. }) => ty = *inner,
                _ => return ty,
            }
        }
    }

    /// When `arg` is a `Copy` of a stack-aggregate local whose
    /// slot contents will outlive the current frame as data —
    /// but whose stack address won't — emit a heap copy and
    /// return the heap pointer (as i64). Returns `None` for
    /// non-aggregate operands; callers fall through to the
    /// normal arg-coercion path.
    ///
    /// Used by the `gos_rt_result_new` arg-emission path so
    /// `Ok(Bag { ... })` doesn't return a pointer to a struct
    /// that lives only on the producer's stack.
    pub(crate) fn maybe_heap_copy_aggregate(&mut self, arg: &Operand) -> Option<String> {
        self.maybe_heap_copy_aggregate_with(arg, /* leak */ false)
    }

    /// Same shape as [`Self::maybe_heap_copy_aggregate`] but routes the
    /// heap allocation through `gos_rt_aggr_alloc_leak` instead of
    /// the GC-tracked `gos_rt_aggr_alloc`. Used when the surviving
    /// handle escapes the GC's reachability graph — HashMap inserts
    /// store the pointer as a bare i64 in MapStorage, which the
    /// tracing collector cannot walk through, so the GC-tracked
    /// allocation would be reclaimed mid-program. The leak variant
    /// keeps the bytes live until process exit at the cost of not
    /// reclaiming HashMap entries when their map drops.
    pub(crate) fn maybe_heap_copy_aggregate_leak(&mut self, arg: &Operand) -> Option<String> {
        self.maybe_heap_copy_aggregate_with(arg, /* leak */ true)
    }

    /// Lowers the guarded copy-blob walk intrinsics emitted by the MIR
    /// aggregate drop pass. The first argument is a bare local whose
    /// SLOT ADDRESS is passed (the runtime walks the aggregate's flat
    /// words in place); the optional second argument names the
    /// module-global guarded meta blob.
    pub(crate) fn lower_guarded_walk_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
    ) -> Result<(), BuildError> {
        let Some(Operand::Copy(p)) = args.first() else {
            return Ok(());
        };
        // Bare local: the alloca is the slot address. Projected place
        // (an option field being overwritten in place): resolve the
        // field address — the walk reads/writes the slot words there.
        // `map_set_blob_values` takes the map POINTER VALUE.
        if name == "gos_rt_map_set_blob_values" {
            if !p.projection.is_empty() {
                return Ok(());
            }
            let v = self.fresh();
            writeln!(
                self.out,
                "  {v} = load ptr, ptr {slot}",
                slot = local_slot(p.local)
            )
            .unwrap();
            declare_rt(&mut self.runtime_refs, name);
            writeln!(self.out, "  call void @gos_rt_map_set_blob_values(ptr {v})").unwrap();
            return Ok(());
        }
        // `vec_set_elem_meta` takes the vec POINTER VALUE; the walk
        // intrinsics take the aggregate's slot address.
        if name == "gos_rt_vec_set_elem_meta" {
            if !p.projection.is_empty() {
                return Ok(());
            }
            let v = self.fresh();
            writeln!(
                self.out,
                "  {v} = load ptr, ptr {slot}",
                slot = local_slot(p.local)
            )
            .unwrap();
            let meta = match args.get(1) {
                Some(Operand::Const(ConstValue::Str(sym))) if !sym.is_empty() => {
                    format!("@\"{sym}\"")
                }
                _ => "null".to_string(),
            };
            declare_rt(&mut self.runtime_refs, name);
            writeln!(
                self.out,
                "  call void @gos_rt_vec_set_elem_meta(ptr {v}, ptr {meta})"
            )
            .unwrap();
            return Ok(());
        }
        let base = if p.projection.is_empty() {
            local_slot(p.local)
        } else {
            self.lower_place_address(p)
        };
        declare_rt(&mut self.runtime_refs, name);
        if name == "gos_rt_option_slot_retain" || name == "gos_rt_option_slot_release" {
            writeln!(self.out, "  call void @{name}(ptr {base})").unwrap();
            return Ok(());
        }
        let meta = match args.get(1) {
            Some(Operand::Const(ConstValue::Str(sym))) if !sym.is_empty() => {
                format!("@\"{sym}\"")
            }
            _ => "null".to_string(),
        };
        writeln!(self.out, "  call void @{name}(ptr {base}, ptr {meta})").unwrap();
        Ok(())
    }

    fn maybe_heap_copy_aggregate_with(&mut self, arg: &Operand, leak: bool) -> Option<String> {
        let Operand::Copy(place) = arg else {
            return None;
        };
        if !place.projection.is_empty() {
            return None;
        }
        let local_ty = self.body.local_ty(place.local);
        if !is_aggregate(self.tcx, local_ty) {
            return None;
        }
        // Sentinel Adts (Result/Option, u32::MAX / u32::MAX-1)
        // are themselves heap-allocated pointers — the slot
        // holds the pointer directly. No copy needed.
        if let Some(TyKind::Adt { def, .. }) = self.tcx.kind(local_ty)
            && (def.local == u32::MAX || def.local == u32::MAX - 1)
        {
            return None;
        }
        let slots = slot_count(self.tcx, local_ty)?;
        if slots == 0 {
            return None;
        }
        let bytes = u64::from(slots) * 8;
        // Aggregate types with a registered copy-blob meta become
        // reference-counted copies (`gos_rt_rc_alloc_copy`): the blob
        // retains the guarded children it now shares, registers in the
        // copy-blob provenance set, and is reclaimed deterministically
        // when its owning slot's guarded walk releases it. Map inserts
        // (the `leak` variant) keep the unmanaged allocator: map
        // storage never releases values, and an RC header under a
        // pointer the map hands back out would be mis-freed.
        if !leak && let Some(sym) = self.tcx.aggr_copy_meta(local_ty) {
            let meta = if sym.is_empty() {
                "null".to_string()
            } else {
                format!("@\"{sym}\"")
            };
            declare_rt(&mut self.runtime_refs, "gos_rt_rc_alloc_copy");
            let src = local_slot(place.local);
            let heap = self.fresh();
            writeln!(
                self.out,
                "  {heap} = call ptr @gos_rt_rc_alloc_copy(i64 {bytes}, ptr {meta}, ptr {src})"
            )
            .unwrap();
            let heap_i64 = self.fresh();
            writeln!(self.out, "  {heap_i64} = ptrtoint ptr {heap} to i64").unwrap();
            return Some(heap_i64);
        }
        let helper = if leak {
            "gos_rt_aggr_alloc_leak"
        } else {
            "gos_rt_aggr_alloc"
        };
        declare_rt(&mut self.runtime_refs, helper);
        let heap = self.fresh();
        writeln!(self.out, "  {heap} = call ptr @{helper}(i64 {bytes})").unwrap();
        if !leak {
            self.emit_gc_root_push(&heap);
        }
        let src = local_slot(place.local);
        writeln!(
            self.out,
            "  call void @llvm.memcpy.p0.p0.i64(ptr {heap}, ptr {src}, i64 {bytes}, i1 false)"
        )
        .unwrap();
        let heap_i64 = self.fresh();
        writeln!(self.out, "  {heap_i64} = ptrtoint ptr {heap} to i64").unwrap();
        Some(heap_i64)
    }

    pub(crate) fn concat_print_kind(&self, op: &Operand) -> ConcatKind {
        match op {
            Operand::Const(ConstValue::Str(_)) => ConcatKind::StrPtr,
            Operand::Const(ConstValue::Int(_)) => ConcatKind::Int,
            Operand::Const(ConstValue::Float(_)) => ConcatKind::Float,
            Operand::Const(ConstValue::Bool(_)) => ConcatKind::Bool,
            Operand::Const(ConstValue::Char(_)) => ConcatKind::Char,
            Operand::Const(ConstValue::Unit) => ConcatKind::Int,
            Operand::Copy(p) => {
                let ty = self.unwrap_ref(self.place_leaf_ty(p));
                match self.tcx.kind(ty) {
                    Some(TyKind::Bool) => ConcatKind::Bool,
                    Some(TyKind::Char) => ConcatKind::Char,
                    Some(TyKind::Float(_)) => ConcatKind::Float,
                    Some(TyKind::String | TyKind::Ref { .. }) => ConcatKind::StrPtr,
                    Some(TyKind::Int(int_ty)) => {
                        if int_ty_is_unsigned_llvm(*int_ty) {
                            ConcatKind::Uint
                        } else {
                            ConcatKind::Int
                        }
                    }
                    Some(TyKind::Unit | TyKind::Never) => ConcatKind::Int,
                    // Unresolved inference variable: the dominant
                    // producer that flows into println is
                    // `__concat`, which returns a String pointer
                    // at runtime. Default to StrPtr so
                    // `println!("a={n}")` doesn't reprint the
                    // empty-string pointer as a giant integer.
                    Some(TyKind::Var(_)) => ConcatKind::StrPtr,
                    // Aggregate / collection / variant types
                    // we can route through runtime format helpers.
                    Some(TyKind::JsonValue) => ConcatKind::JsonValue,
                    Some(TyKind::DynError) => ConcatKind::ErrorMessage,
                    Some(TyKind::Array { elem, len }) => {
                        let n = i64::try_from(*len).unwrap_or(0);
                        match self.tcx.kind(*elem) {
                            Some(TyKind::Int(_)) => ConcatKind::ArrI64(n),
                            Some(TyKind::Float(_)) => ConcatKind::ArrF64(n),
                            Some(TyKind::Bool) => ConcatKind::ArrBool(n),
                            Some(TyKind::String) => ConcatKind::ArrString(n),
                            _ => ConcatKind::Unsupported,
                        }
                    }
                    Some(TyKind::Slice(elem) | TyKind::Vec(elem)) => match self.tcx.kind(*elem) {
                        Some(TyKind::Int(_)) => ConcatKind::VecI64,
                        Some(TyKind::Float(_)) => ConcatKind::VecF64,
                        Some(TyKind::Bool) => ConcatKind::VecBool,
                        Some(TyKind::String) => ConcatKind::VecString,
                        Some(TyKind::Vec(inner) | TyKind::Slice(inner)) => {
                            match self.tcx.kind(*inner) {
                                Some(TyKind::Int(_)) => ConcatKind::VecVecI64,
                                Some(TyKind::String) => ConcatKind::VecVecString,
                                _ => ConcatKind::Unsupported,
                            }
                        }
                        _ => ConcatKind::Unsupported,
                    },
                    // Aggregate / collection / variant types we
                    // can't yet route. Refuse loudly so the
                    // backend triggers fallback.
                    Some(
                        kind @ (TyKind::Tuple(_)
                        | TyKind::HashMap { .. }
                        | TyKind::Sender(_)
                        | TyKind::Receiver(_)
                        | TyKind::JoinHandle(_)
                        | TyKind::Adt { .. }
                        | TyKind::Closure { .. }
                        | TyKind::FnDef { .. }
                        | TyKind::FnPtr(_)
                        | TyKind::FnTrait(_)
                        | TyKind::Dyn(_)),
                    ) => {
                        if std::env::var("GOS_LLVM_TRACE").is_ok() {
                            eprintln!("llvm backend: concat_print_kind unsupported: {kind:?}");
                        }
                        ConcatKind::Unsupported
                    }
                    Some(TyKind::Param { .. } | TyKind::Alias { .. } | TyKind::Error) | None => {
                        ConcatKind::Int
                    }
                }
            }
            Operand::FnRef { .. } => ConcatKind::Int,
        }
    }

    /// Sign- or zero-extends a value to `i64` for the Int print
    /// path. Looks at the operand's source type so a `u8` byte
    /// extends differently than an `i32`.
    pub(crate) fn widen_to_i64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i64" {
            return v.to_string();
        }
        // LLVM 18 rejects sext/zext from ptr; pointer-typed locals stored via
        // inttoptr (e.g. loop iteration over array elements) must use ptrtoint.
        if src_llvm == "ptr" {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = ptrtoint ptr {v} to i64").unwrap();
            return tmp;
        }
        let signed = match op {
            Operand::Copy(p) => {
                let ty = self.unwrap_ref(self.place_leaf_ty(p));
                matches!(self.tcx.kind(ty), Some(TyKind::Int(i)) if int_signed(*i))
                    || !matches!(self.tcx.kind(ty), Some(TyKind::Int(_)))
            }
            _ => true,
        };
        let tmp = self.fresh();
        let instr = if signed { "sext" } else { "zext" };
        writeln!(self.out, "  {tmp} = {instr} {src_llvm} {v} to i64").unwrap();
        tmp
    }

    /// Zero-extend an unsigned operand to `i64`. Distinct from
    /// `widen_to_i64` (which sign-extends) so values >= 2^63 don't
    /// flip sign on the way to the runtime printer. Used by the
    /// `Uint` arms of `concat_print_kind`.
    pub(crate) fn widen_to_u64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i64" {
            return v.to_string();
        }
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = zext {src_llvm} {v} to i64").unwrap();
        tmp
    }

    pub(crate) fn widen_to_f64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "double" {
            return v.to_string();
        }
        if src_llvm == "float" {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = fpext float {v} to double").unwrap();
            return tmp;
        }
        v.to_string()
    }

    /// Like [`Self::widen_to_f64`] but also converts integer operands via
    /// `sitofp`. Used by `__fmt_prec`, which accepts a numeric value
    /// regardless of MIR type and renders it as a float.
    pub(crate) fn coerce_to_f64(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        match src_llvm.as_str() {
            "double" => v.to_string(),
            "float" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = fpext float {v} to double").unwrap();
                tmp
            }
            "i1" | "i8" | "i16" | "i32" | "i64" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = sitofp {src_llvm} {v} to double").unwrap();
                tmp
            }
            _ => v.to_string(),
        }
    }

    pub(crate) fn widen_bool_to_i32(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i32" {
            return v.to_string();
        }
        let tmp = self.fresh();
        if src_llvm == "i1" || src_llvm == "i8" || src_llvm == "i16" {
            writeln!(self.out, "  {tmp} = zext {src_llvm} {v} to i32").unwrap();
        } else if src_llvm == "i64" {
            writeln!(self.out, "  {tmp} = trunc i64 {v} to i32").unwrap();
        } else {
            return v.to_string();
        }
        tmp
    }

    pub(crate) fn widen_char_to_i32(&mut self, op: &Operand, v: &str) -> String {
        let src_llvm = self.operand_llvm_ty(op);
        if src_llvm == "i32" {
            return v.to_string();
        }
        let tmp = self.fresh();
        if src_llvm == "i64" {
            writeln!(self.out, "  {tmp} = trunc i64 {v} to i32").unwrap();
        } else if src_llvm == "i8" || src_llvm == "i16" {
            writeln!(self.out, "  {tmp} = zext {src_llvm} {v} to i32").unwrap();
        } else {
            return v.to_string();
        }
        tmp
    }

    /// Inserts the LLVM cast that brings `value` (of type
    /// `from_ty`) over to `to_ty`, returning the new SSA name.
    /// No-op when the types already match. Handles the common
    /// scalar-to-pointer / pointer-to-scalar / int-width and
    /// float-width permutations the variant-stub path needs.
    pub(crate) fn coerce_llvm_value(&mut self, value: &str, from_ty: &str, to_ty: &str) -> String {
        if from_ty == to_ty {
            return value.to_string();
        }
        let tmp = self.fresh();
        let op = match (from_ty, to_ty) {
            ("ptr", _) if to_ty.starts_with('i') => "ptrtoint",
            (_, "ptr") if from_ty.starts_with('i') => {
                // `inttoptr` requires the source to be at least
                // pointer-width. Narrower integers (i1 for bool,
                // i32 for char) must be zext'd to i64 first or
                // LLVM rejects with "pointer cast from non-integral".
                let from_w: u32 = from_ty[1..].parse().unwrap_or(64);
                if from_w < 64 {
                    let widened = self.fresh();
                    writeln!(self.out, "  {widened} = zext {from_ty} {value} to i64").unwrap();
                    writeln!(self.out, "  {tmp} = inttoptr i64 {widened} to ptr").unwrap();
                    return tmp;
                }
                "inttoptr"
            }
            ("ptr", "double") => {
                // Through i64 — LLVM has no direct ptr→double.
                let mid = self.fresh();
                writeln!(self.out, "  {mid} = ptrtoint ptr {value} to i64").unwrap();
                writeln!(self.out, "  {tmp} = bitcast i64 {mid} to double").unwrap();
                return tmp;
            }
            ("double", "ptr") => {
                let mid = self.fresh();
                writeln!(self.out, "  {mid} = bitcast double {value} to i64").unwrap();
                writeln!(self.out, "  {tmp} = inttoptr i64 {mid} to ptr").unwrap();
                return tmp;
            }
            _ if from_ty.starts_with('i') && to_ty.starts_with('i') => {
                let from_w: u32 = from_ty[1..].parse().unwrap_or(64);
                let to_w: u32 = to_ty[1..].parse().unwrap_or(64);
                if to_w > from_w {
                    "zext"
                } else if to_w < from_w {
                    "trunc"
                } else {
                    return value.to_string();
                }
            }
            // Integer ↔ floating-point conversions use `sitofp`
            // / `fptosi` (signed) — `bitcast` reinterprets bits
            // and produces a denormal float for small integers,
            // which is both wrong semantically and rejected by
            // `opt`'s verifier when the integer literal is too
            // small to be a valid double bit-pattern.
            _ if from_ty.starts_with('i') && (to_ty == "double" || to_ty == "float") => "sitofp",
            _ if (from_ty == "double" || from_ty == "float") && to_ty.starts_with('i') => "fptosi",
            _ => "bitcast",
        };
        writeln!(self.out, "  {tmp} = {op} {from_ty} {value} to {to_ty}").unwrap();
        tmp
    }

    pub(crate) fn fresh(&mut self) -> String {
        let n = self.next_ssa;
        self.next_ssa += 1;
        format!("%t{n}")
    }

    /// Looks up the rendered LLVM type for an operand,
    /// walking any projection chain so `p.x + p.y` sees the
    /// field type rather than the struct-ptr one. String-byte
    /// reads (`s[i]`) classify as `i64`.
    /// Returns the LLVM type the rvalue's lowering produces, or
    /// the empty string when the rvalue's emitter writes
    /// directly into the destination slot (aggregates, repeats,
    /// raw heap intrinsics, runtime calls). The assign-store
    /// path uses this to decide whether a coercion is needed
    /// between the produced value and the destination slot's
    /// leaf type.
    pub(crate) fn rvalue_llvm_ty(&self, rvalue: &Rvalue) -> String {
        use gossamer_mir::BinOp;
        match rvalue {
            Rvalue::Use(op) => self.operand_llvm_ty(op),
            Rvalue::BinaryOp { op, lhs, .. } => {
                // Comparison ops produce `i1`; everything else
                // follows the lhs operand type. Returning the
                // wrong shape here makes the assign-store path
                // emit a spurious coercion that itself produces
                // invalid IR.
                match op {
                    BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => {
                        "i1".to_string()
                    }
                    _ => self.operand_llvm_ty(lhs),
                }
            }
            Rvalue::UnaryOp { operand, .. } => self.operand_llvm_ty(operand),
            Rvalue::Cast { target, .. } => render_ty(self.tcx, *target),
            Rvalue::Ref { .. } | Rvalue::Len(_) => "ptr".to_string(),
            // Aggregate / Repeat write into the destination
            // directly; no coercion at the assign-store site.
            Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } => String::new(),
            // CallIntrinsic ABIs vary; coercion is handled
            // per-intrinsic inside lower_raw_intrinsic.
            Rvalue::CallIntrinsic { .. } => String::new(),
        }
    }

    pub(crate) fn operand_llvm_ty(&self, op: &Operand) -> String {
        match op {
            Operand::Copy(p) => {
                if self.place_is_string_byte(p) {
                    return "i64".to_string();
                }
                render_ty(self.tcx, self.place_leaf_ty(p))
            }
            Operand::Const(c) => const_llvm_ty(c).to_string(),
            Operand::FnRef { .. } => "ptr".to_string(),
        }
    }

    /// Returns the [`gossamer_types::Ty`] behind an operand,
    /// used where the caller needs to do kind-aware dispatch
    /// (arithmetic vs comparison, integer signedness).
    /// Respects projection chains. For string-byte reads we
    /// scan the body's locals for an existing `i64`-kind
    /// handle so downstream numeric classifiers see an int.
    /// For constants we scan the body's locals for an
    /// existing handle of the same kind, so a float constant
    /// in a float context classifies as `f64`.
    pub(crate) fn operand_ty(&self, op: &Operand) -> Ty {
        match op {
            Operand::Copy(p) => {
                if self.place_is_string_byte(p) {
                    if let Some(ty) = self.borrow_i64_ty() {
                        return ty;
                    }
                }
                self.place_leaf_ty(p)
            }
            Operand::Const(value) => match value {
                ConstValue::Float(_) => self
                    .borrow_kind_ty(|k| matches!(k, TyKind::Float(gossamer_types::FloatTy::F64)))
                    .unwrap_or_else(|| self.body.local_ty(Local::RETURN)),
                ConstValue::Int(_) | ConstValue::Char(_) | ConstValue::Bool(_) => self
                    .borrow_i64_ty()
                    .unwrap_or_else(|| self.body.local_ty(Local::RETURN)),
                _ => self.body.local_ty(Local::RETURN),
            },
            Operand::FnRef { .. } => self.body.local_ty(Local::RETURN),
        }
    }

    pub(crate) fn borrow_kind_ty(&self, want: impl Fn(&TyKind) -> bool) -> Option<Ty> {
        for decl in &self.body.locals {
            if let Some(k) = self.tcx.kind(decl.ty) {
                if want(k) {
                    return Some(decl.ty);
                }
            }
        }
        None
    }

    /// Returns `true` when the final projection step of
    /// `place` indexes into a `String` / `&String`. Used to
    /// reclassify the operand type as `i64` without needing
    /// to mint a fresh `Ty` handle.
    pub(crate) fn place_is_string_byte(&self, place: &Place) -> bool {
        if place.projection.is_empty() {
            return false;
        }
        let (prefix, last) = place.projection.split_at(place.projection.len() - 1);
        if !matches!(last[0], Projection::Index(_)) {
            return false;
        }
        let mut ty = self.body.local_ty(place.local);
        for proj in prefix {
            ty = self.unwrap_ref(ty);
            ty = match proj {
                Projection::Field(i) => match self.tcx.kind(ty) {
                    Some(TyKind::Adt { def, .. }) => self
                        .tcx
                        .struct_field_tys(*def)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    _ => ty,
                },
                Projection::Index(_) => match self.tcx.kind(ty) {
                    Some(TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        *elem
                    }
                    _ => ty,
                },
                Projection::Deref => self.unwrap_ref(ty),
                Projection::Downcast(_) | Projection::Discriminant => ty,
            };
        }
        matches!(self.tcx.kind(self.unwrap_ref(ty)), Some(TyKind::String))
    }

    /// Scans the body's locals for an existing `i64`-kinded
    /// [`Ty`] handle. Used to answer `operand_ty` queries for
    /// string-byte reads without needing to mint a fresh
    /// interner entry.
    pub(crate) fn borrow_i64_ty(&self) -> Option<Ty> {
        for decl in &self.body.locals {
            if matches!(
                self.tcx.kind(decl.ty),
                Some(TyKind::Int(gossamer_types::IntTy::I64))
            ) {
                return Some(decl.ty);
            }
        }
        None
    }
}

// Win64 carries a runtime helper's 2-word `i128` (Fat) return in a 16-byte
// vector register (`<16 x i8>`), matching the rustc-compiled runtime; llc
// returns a bare `i128` GP-register pair, so a `gos_rt_*` call must be wired as
// `<16 x i8>` + bitcast to agree with the runtime. This boundary exists ONLY
// for `gos_rt_*` symbols, whose return type comes from the ABI registry. A bare
// `i128` returned by a user function is a gossamer->gossamer call (the callee is
// `define i128`/`ret i128` in the same module) and must stay a GP-register-pair
// `i128` on both sides — applying the vector ABI to it asymmetrically miscompiles
// every `Result`/`Option`/inline-enum a user function returns on Windows. Both
// call emitters (`emit_named_call`, `lower_runtime_call_intrinsic`) MUST gate on
// the registry return type via this one decision so they cannot drift apart.
pub(crate) fn needs_win64_fat_ret(is_windows: bool, registry_ret: Option<&str>) -> bool {
    is_windows && registry_ret == Some("i128")
}

#[cfg(test)]
mod tests {
    use super::needs_win64_fat_ret;

    #[test]
    fn user_fn_i128_return_is_not_marshalled_on_windows() {
        // A user function (no ABI-registry entry) returning a Fat `i128`
        // aggregate must NOT get the `<16 x i8>` wire ABI, on any platform.
        assert!(!needs_win64_fat_ret(true, None));
        assert!(!needs_win64_fat_ret(false, None));
    }

    #[test]
    fn runtime_i128_return_is_marshalled_only_on_windows() {
        assert!(needs_win64_fat_ret(true, Some("i128")));
        assert!(!needs_win64_fat_ret(false, Some("i128")));
    }

    #[test]
    fn non_i128_runtime_return_is_never_marshalled() {
        assert!(!needs_win64_fat_ret(true, Some("ptr")));
        assert!(!needs_win64_fat_ret(true, Some("i64")));
        assert!(!needs_win64_fat_ret(true, Some("void")));
    }
}
