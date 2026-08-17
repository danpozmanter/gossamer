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
        // the type after each step - the final one must be
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
                    Some(TyKind::Adt { def, substs }) => self
                        .tcx
                        .adt_field_tys(*def, substs)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    Some(TyKind::Array { elem, .. }) => *elem,
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
    /// projections the same way the runtime does - an `Index`
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
                    Some(TyKind::Adt { def, substs }) => self
                        .tcx
                        .adt_field_tys(*def, substs)
                        .and_then(|tys| tys.get(*idx as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*idx as usize).copied().unwrap_or(ty),
                    Some(TyKind::Array { elem, .. }) => *elem,
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
    /// aggregate is treated as a pointer-bearing slot - this
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
    /// slot contents will outlive the current frame as data -
    /// but whose stack address won't - emit a heap copy and
    /// return the heap pointer (as i64). Returns `None` for
    /// non-aggregate operands; callers fall through to the
    /// normal arg-coercion path.
    ///
    /// Used by the `gos_rt_result_new` arg-emission path so
    /// `Ok(Bag { ... })` doesn't return a pointer to a struct
    /// that lives only on the producer's stack.
    pub(crate) fn maybe_heap_copy_aggregate(&mut self, arg: &Operand) -> Option<String> {
        self.maybe_heap_copy_aggregate_with(arg, /* leak */ false, /* map_owned */ false)
    }

    /// Heap-copies a 2-word by-value enum payload (sentinel
    /// `Option` / `Result` or inline user enum) passed to
    /// `gos_rt_result_new`, returning the heap address as an i64
    /// SSA value. The payload word then carries a pointer that the
    /// symmetric `gos_rt_result_payload_i128` extractor
    /// dereferences. Without this, the i128 operand would truncate
    /// to its low (discriminant) word at the i64 call boundary.
    pub(crate) fn maybe_heap_copy_value_enum(&mut self, arg: &Operand) -> Option<String> {
        let Operand::Copy(place) = arg else {
            return None;
        };
        if !place.projection.is_empty() {
            return None;
        }
        let local_ty = self.body.local_ty(place.local);
        let is_value_enum = matches!(
            self.tcx.kind(local_ty),
            Some(TyKind::Adt { def, .. }) if def.local == u32::MAX || def.local == u32::MAX - 1
        ) || self.tcx.is_inline_enum_ty(local_ty);
        if !is_value_enum {
            return None;
        }
        declare_rt(&mut self.runtime_refs, "gos_rt_aggr_alloc");
        // `noalias`: a fresh allocation, so the memcpy below cannot be writing
        // through any other live pointer.
        let heap = self.fresh();
        writeln!(
            self.out,
            "  {heap} = call noalias ptr @gos_rt_aggr_alloc(i64 16)"
        )
        .unwrap();
        let src = local_slot(place.local);
        writeln!(
            self.out,
            "  call void @llvm.memcpy.p0.p0.i64(ptr {heap}, ptr {src}, i64 16, i1 false)"
        )
        .unwrap();
        let heap_i64 = self.fresh();
        writeln!(self.out, "  {heap_i64} = ptrtoint ptr {heap} to i64").unwrap();
        Some(heap_i64)
    }

    /// Same shape as [`Self::maybe_heap_copy_aggregate`] but routes the
    /// heap allocation through `gos_rt_aggr_alloc_leak` instead of
    /// the GC-tracked `gos_rt_aggr_alloc`. This is reserved for legacy
    /// escape paths that do not report their stored pointers to the GC.
    /// HashMap inserts use [`Self::maybe_heap_copy_aggregate_for_map`], whose
    /// reference-counted structural copy is reclaimed with the map entry.
    pub(crate) fn maybe_heap_copy_aggregate_leak(&mut self, arg: &Operand) -> Option<String> {
        self.maybe_heap_copy_aggregate_with(arg, /* leak */ true, /* map_owned */ false)
    }

    /// Heap-copies an aggregate that becomes a `HashMap` entry. Structural
    /// metadata is preferred over the guarded copy-blob metadata because the
    /// map owns direct `String` / `Vec` fields as well as the outer blob.
    pub(crate) fn maybe_heap_copy_aggregate_for_map(&mut self, arg: &Operand) -> Option<String> {
        self.maybe_heap_copy_aggregate_with(arg, /* leak */ false, /* map_owned */ true)
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
        // field address - the walk reads/writes the slot words there.
        // Map ownership markers take the map POINTER VALUE.
        if matches!(
            name,
            "gos_rt_map_set_blob_values" | "gos_rt_map_set_vec_values"
        ) {
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
            writeln!(self.out, "  call void @{name}(ptr {v})").unwrap();
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
        // `vec_set_slot_children` likewise takes the vec POINTER VALUE plus
        // the static slot-children layout blob.
        if name == "gos_rt_vec_set_slot_children" {
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
                "  call void @gos_rt_vec_set_slot_children(ptr {v}, ptr {meta})"
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

    fn maybe_heap_copy_aggregate_with(
        &mut self,
        arg: &Operand,
        leak: bool,
        map_owned: bool,
    ) -> Option<String> {
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
        // are themselves heap-allocated pointers - the slot
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
        // retains its children, registers in the copy-blob provenance set,
        // and is reclaimed deterministically by its owning slot or map entry.
        // The explicit `leak` variant remains only for legacy escape paths
        // whose storage does not participate in deterministic teardown.
        let copy_meta = if map_owned {
            let structural = format!("gos_rc_meta_boxaggr_{}", local_ty.as_u32());
            self.tcx
                .rc_meta(&structural)
                .map(|_| structural)
                .or_else(|| self.tcx.aggr_copy_meta(local_ty).map(str::to_owned))
        } else {
            self.tcx.aggr_copy_meta(local_ty).map(str::to_owned)
        };
        if !leak && let Some(sym) = copy_meta {
            let meta = if sym.is_empty() {
                "null".to_string()
            } else {
                format!("@\"{sym}\"")
            };
            declare_rt(&mut self.runtime_refs, "gos_rt_rc_alloc_copy");
            let src = local_slot(place.local);
            // `noalias`: the RC block is freshly allocated, so nothing else
            // live at this point addresses it.
            let heap = self.fresh();
            writeln!(
                self.out,
                "  {heap} = call noalias ptr @gos_rt_rc_alloc_copy(i64 {bytes}, ptr {meta}, ptr {src})"
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
        // `noalias`: both helpers return a fresh allocation, so the memcpy
        // below cannot be writing through any other live pointer.
        let heap = self.fresh();
        writeln!(
            self.out,
            "  {heap} = call noalias ptr @{helper}(i64 {bytes})"
        )
        .unwrap();
        let src = local_slot(place.local);
        writeln!(
            self.out,
            "  call void @llvm.memcpy.p0.p0.i64(ptr {heap}, ptr {src}, i64 {bytes}, i1 false)"
        )
        .unwrap();
        // The leak allocator raw-copies the aggregate's words, so any
        // guarded (RC) child it now shares - a `String` / `Vec` field of a
        // map-stored struct - must be retained, or it is freed when the
        // inserting scope's original aggregate is released and the
        // map-held copy dangles. The bytes leak with the map entry (the
        // documented "map storage never releases values" contract), so
        // there is no symmetric release.
        if leak
            && let Some(sym) = self.tcx.aggr_copy_meta(local_ty)
            && !sym.is_empty()
        {
            declare_rt(&mut self.runtime_refs, "gos_rt_aggr_retain_children");
            writeln!(
                self.out,
                "  call void @gos_rt_aggr_retain_children(ptr {heap}, ptr @\"{sym}\")"
            )
            .unwrap();
        }
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
                let ty = self.tcx.peel_nominal(ty);
                // A container renders through its own runtime shim whether
                // the local carries the container's type or the bare i64
                // handle the constructor returned.
                if let Some(TyKind::Adt { def, .. }) = self.tcx.kind(ty)
                    && let Some(sym) = container_format_symbol(def.local)
                {
                    return ConcatKind::HandleFormat(sym);
                }
                match self.tcx.kind(ty) {
                    Some(TyKind::Bool) => ConcatKind::Bool,
                    Some(TyKind::Char) => ConcatKind::Char,
                    Some(TyKind::Float(_)) => ConcatKind::Float,
                    Some(TyKind::String | TyKind::Ref { .. }) => ConcatKind::StrPtr,
                    Some(TyKind::Int(int_ty)) => {
                        if p.projection.is_empty()
                            && let Some(kind) = self.set_handle_print_kind(p.local, 0)
                        {
                            return kind;
                        }
                        // A `u64` / `usize` / `u128` is the only unsigned
                        // family whose value can exceed `i64::MAX`, so it
                        // selects the unsigned printer: a value above 2^63
                        // renders as a large positive decimal rather than a
                        // negative i64. The narrower unsigned types
                        // (`u8`/`u16`/`u32`) keep the signed printer - their
                        // float-cast / wrapping results (`-1.5 as u8 == -1`)
                        // print signed, matching the VM, which only boxes a
                        // `u64`/`usize` format argument as `Value::Uint`.
                        if int_ty_is_unsigned_llvm(*int_ty) && int_width(*int_ty) >= 64 {
                            ConcatKind::Uint
                        } else {
                            ConcatKind::Int
                        }
                    }
                    // `time::Duration` / `time::Instant` are transparent
                    // `i64`s; print them as the integer they carry.
                    Some(TyKind::Unit | TyKind::Never | TyKind::Duration | TyKind::Instant) => {
                        ConcatKind::Int
                    }
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
                        let n = i64::try_from(len.to_usize()).unwrap_or(0);
                        let elem = *elem;
                        match self.tcx.kind(elem) {
                            Some(TyKind::Int(gossamer_types::IntTy::U8)) => ConcatKind::ArrU8(n),
                            Some(TyKind::Int(_)) => ConcatKind::ArrI64(n),
                            Some(TyKind::Float(_)) => ConcatKind::ArrF64(n),
                            Some(TyKind::Bool) => ConcatKind::ArrBool(n),
                            Some(TyKind::Char) => ConcatKind::ArrChar(n),
                            Some(TyKind::String) => ConcatKind::ArrString(n),
                            // Nested fixed array: rows are inline (N * M
                            // contiguous slots), so both static lengths
                            // route to the nested formatter.
                            Some(TyKind::Array {
                                elem: inner_elem,
                                len: inner_len,
                            }) => {
                                let m = i64::try_from(inner_len.to_usize()).unwrap_or(0);
                                match self.tcx.kind(*inner_elem) {
                                    Some(TyKind::Int(_)) => ConcatKind::ArrArrI64(n, m),
                                    Some(TyKind::Float(_)) => ConcatKind::ArrArrF64(n, m),
                                    Some(TyKind::Bool) => ConcatKind::ArrArrBool(n, m),
                                    _ => ConcatKind::Unsupported,
                                }
                            }
                            // Array rows are inline, so element `i` starts at
                            // `base + i * slots * 8`.
                            _ => match self.adt_debug_fmt_symbol(elem) {
                                Some(sym) => {
                                    let slots = i64::from(
                                        crate::ty::slot_count(self.tcx, elem).unwrap_or(1).max(1),
                                    );
                                    ConcatKind::ArrAdt(
                                        n,
                                        slots * 8,
                                        sym,
                                        self.adt_fmt_takes_slot_address(elem),
                                    )
                                }
                                None => ConcatKind::Unsupported,
                            },
                        }
                    }
                    Some(TyKind::Slice(elem) | TyKind::Vec(elem)) => {
                        let elem = *elem;
                        match self.tcx.kind(elem) {
                            Some(TyKind::Int(IntTy::U64 | IntTy::Usize)) => ConcatKind::VecUint,
                            Some(TyKind::Int(_)) => ConcatKind::VecI64,
                            Some(TyKind::Float(_)) => ConcatKind::VecF64,
                            Some(TyKind::Bool) => ConcatKind::VecBool,
                            Some(TyKind::Char) => ConcatKind::VecChar,
                            Some(TyKind::String) => ConcatKind::VecString,
                            Some(TyKind::Vec(inner) | TyKind::Slice(inner)) => {
                                match self.tcx.kind(*inner) {
                                    Some(TyKind::Int(_)) => ConcatKind::VecVecI64,
                                    Some(TyKind::Float(_)) => ConcatKind::VecVecF64,
                                    Some(TyKind::String) => ConcatKind::VecVecString,
                                    _ => self
                                        .value_descriptor(elem)
                                        .map_or(ConcatKind::Unsupported, ConcatKind::VecDesc),
                                }
                            }
                            Some(TyKind::HashMap { .. }) => ConcatKind::VecMap,
                            Some(TyKind::Tuple(nested)) => {
                                let arity = nested.len();
                                match self.tuple_elem_tags(elem) {
                                    // The element's own tags start with the
                                    // nested marker; the renderer wants the
                                    // per-field tags directly.
                                    Some(tags) if arity > 0 && tags.len() > 2 => {
                                        ConcatKind::VecTuple(tags[2..].to_vec(), arity)
                                    }
                                    _ => ConcatKind::Unsupported,
                                }
                            }
                            _ => match self.adt_debug_fmt_symbol(elem) {
                                Some(sym) => {
                                    ConcatKind::VecAdt(sym, self.adt_fmt_takes_slot_address(elem))
                                }
                                None => self
                                    .value_descriptor(elem)
                                    .map_or(ConcatKind::Unsupported, ConcatKind::VecDesc),
                            },
                        }
                    }
                    // Tuple of scalar elements: route through
                    // `gos_rt_tuple_format`. Mixed/aggregate element
                    // types (or narrow ints whose flat-slot bytes
                    // aren't a full i64) require richer display planning.
                    Some(TyKind::Tuple(elems)) => {
                        if !elems.is_empty()
                            && elems.iter().all(|e| self.tuple_elem_tags(*e).is_some())
                        {
                            ConcatKind::Tuple
                        } else {
                            ConcatKind::Unsupported
                        }
                    }
                    // Scalar-keyed, scalar/string-valued HashMap:
                    // route through `gos_rt_map_format`.
                    Some(TyKind::HashMap { key, value, .. }) => {
                        let unsigned_kv = matches!(
                            self.tcx.kind(self.unwrap_ref(*key)),
                            Some(TyKind::Int(IntTy::U64 | IntTy::Usize))
                        ) || matches!(
                            self.tcx.kind(self.unwrap_ref(*value)),
                            Some(TyKind::Int(IntTy::U64 | IntTy::Usize))
                        );
                        if !self.map_kv_supported(*key) {
                            ConcatKind::Unsupported
                        } else if unsigned_kv && self.map_kv_supported(*value) {
                            // The plain map formatter reads every integer slot
                            // as signed; the tags carry each side's declared
                            // width instead.
                            let unsigned_tag = |ty| {
                                u8::from(matches!(
                                    self.tcx.kind(self.unwrap_ref(ty)),
                                    Some(TyKind::Int(IntTy::U64 | IntTy::Usize))
                                ))
                            };
                            ConcatKind::MapTagged(unsigned_tag(*key), unsigned_tag(*value))
                        } else if self.map_kv_supported(*value) {
                            ConcatKind::Map
                        } else {
                            // A container value is stored as a handle word,
                            // so its tag tells the renderer to read it as one;
                            // an aggregate value is a slot buffer the derived
                            // `fmt` or the tuple tags render.
                            match self.tuple_elem_tag(*value) {
                                Some(tag) => ConcatKind::MapTagged(0, tag),
                                None => self.map_aggregate_value_kind(*key, *value),
                            }
                        }
                    }
                    Some(TyKind::Adt { def, substs }) if matches!(def.local, n if n == u32::MAX - 7 || n == u32::MAX - 18) =>
                    {
                        let is_btree = def.local == u32::MAX - 18;
                        let elem = substs.types().first().copied();
                        let scalar =
                            elem.and_then(|elem| match self.tcx.kind(self.unwrap_ref(elem)) {
                                Some(TyKind::Int(IntTy::U64 | IntTy::Usize)) => {
                                    Some(ConcatKind::SetUint(is_btree))
                                }
                                Some(TyKind::Int(i)) if int_width(*i) == 64 => {
                                    Some(ConcatKind::SetI64(is_btree))
                                }
                                Some(TyKind::String) => Some(ConcatKind::SetString(is_btree)),
                                _ => None,
                            });
                        match scalar {
                            Some(kind) => kind,
                            // An aggregate element is stored as its slot
                            // bytes, which its descriptor renders.
                            None => elem
                                .and_then(|elem| self.value_descriptor(elem))
                                .map_or(ConcatKind::Unsupported, |desc| {
                                    ConcatKind::SetDesc(desc, is_btree)
                                }),
                        }
                    }
                    // `{:?}` of an `Option<T>` / `Result<T, E>` with scalar /
                    // String payloads: render through the runtime debug helper.
                    // User structs / enums with a `#[derive(Debug)]` fmt are
                    // routed to that fmt before reaching here, so this only
                    // catches the built-in by-value enums. Aggregate / nested
                    // payloads fall through to Unsupported.
                    Some(TyKind::Adt { def, substs })
                        if (def.local == u32::MAX || def.local == u32::MAX - 1) =>
                    {
                        let tys = substs.types();
                        if def.local == u32::MAX - 1 {
                            match tys.first().and_then(|t| self.debug_payload_plan(*t)) {
                                Some(k) => ConcatKind::Option(k),
                                None => ConcatKind::Unsupported,
                            }
                        } else {
                            match (
                                tys.first().and_then(|t| self.debug_payload_plan(*t)),
                                tys.get(1).and_then(|t| self.debug_payload_plan(*t)),
                            ) {
                                (Some(ok), Some(err)) => ConcatKind::Result(ok, err),
                                _ => ConcatKind::Unsupported,
                            }
                        }
                    }
                    // Aggregate / collection / variant types this planner
                    // cannot route. Refusing here surfaces the gap as a
                    // hard build error instead of a silently wrong render.
                    Some(
                        kind @ (TyKind::Sender(_)
                        | TyKind::Receiver(_)
                        | TyKind::JoinHandle(_)
                        | TyKind::Iterator(_)
                        | TyKind::Range(_)
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
                    Some(TyKind::Nominal { .. }) => {
                        unreachable!("nominal aliases are peeled above")
                    }
                }
            }
            Operand::FnRef { .. } => ConcatKind::Int,
        }
    }

    /// The element kind of a set built from a sequence: the source vec's
    /// element type names the declared width the set's own i64 handle drops.
    fn set_from_vec_kind(&self, args: &[Operand], is_btree: bool) -> ConcatKind {
        let unsigned = args.first().is_some_and(|arg| match arg {
            Operand::Copy(place) => matches!(
                self.tcx.kind(self.unwrap_ref(self.place_leaf_ty(place))),
                Some(TyKind::Vec(elem) | TyKind::Slice(elem))
                    if matches!(
                        self.tcx.kind(self.unwrap_ref(*elem)),
                        Some(TyKind::Int(IntTy::U64 | IntTy::Usize))
                    )
            ),
            _ => false,
        });
        if unsigned {
            ConcatKind::SetUint(is_btree)
        } else {
            ConcatKind::SetI64(is_btree)
        }
    }

    fn set_handle_print_kind(&self, local: Local, depth: u8) -> Option<ConcatKind> {
        if depth > 8 {
            return None;
        }
        let mut saw_set_ctor = false;
        let mut is_btree = false;
        let mut elem_kind = None;
        for block in &self.body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind
                    && place.local == local
                    && place.projection.is_empty()
                    && let Rvalue::Use(Operand::Copy(src)) = rvalue
                    && src.projection.is_empty()
                    && let Some(kind) = self.set_handle_print_kind(src.local, depth + 1)
                {
                    return Some(kind);
                }
            }
            if let Terminator::Call {
                callee,
                args,
                destination,
                ..
            } = &block.terminator
                && let Operand::Const(ConstValue::Str(name)) = callee
            {
                if destination.local == local && destination.projection.is_empty() {
                    if let Some(sym) = container_ctor_format_symbol(name.as_str()) {
                        return Some(ConcatKind::HandleFormat(sym));
                    }
                    // A set that came from another set - the defensive
                    // `gos_rt_set_clone` a `let` binds through (neither type
                    // carries its own refcount), or a set-algebra result -
                    // holds the elements of the set it was built from, so the
                    // element kind lives on that handle rather than on this
                    // call's own operands.
                    if matches!(
                        name.as_str(),
                        "gos_rt_set_clone"
                            | "gos_rt_set_clear"
                            | "gos_rt_set_union"
                            | "gos_rt_set_intersection"
                            | "gos_rt_set_intersection_skey"
                            | "gos_rt_set_difference"
                            | "gos_rt_set_symmetric_difference"
                    ) && let Some(Operand::Copy(src)) = args.first()
                        && src.projection.is_empty()
                        && let Some(kind) = self.set_handle_print_kind(src.local, depth + 1)
                    {
                        return Some(kind);
                    }
                    match name.as_str() {
                        "gos_rt_btree_set_new" => {
                            saw_set_ctor = true;
                            is_btree = true;
                        }
                        "gos_rt_set_new" => saw_set_ctor = true,
                        // `Set::from(values)` over a runtime sequence: the
                        // constructor names the element kind directly, so the
                        // handle still renders under `{:?}`.
                        "gos_rt_btree_set_from_vec_i64" => {
                            saw_set_ctor = true;
                            is_btree = true;
                            elem_kind = Some(self.set_from_vec_kind(args, true));
                        }
                        "gos_rt_btree_set_from_vec_str" => {
                            saw_set_ctor = true;
                            is_btree = true;
                            elem_kind = Some(ConcatKind::SetString(true));
                        }
                        "gos_rt_set_from_vec_i64" => {
                            saw_set_ctor = true;
                            elem_kind = Some(self.set_from_vec_kind(args, false));
                        }
                        "gos_rt_set_from_vec_str" => {
                            saw_set_ctor = true;
                            elem_kind = Some(ConcatKind::SetString(false));
                        }
                        _ => {}
                    }
                }
                if args.first().is_some_and(|arg| {
                    matches!(
                        arg,
                        Operand::Copy(place)
                            if place.local == local && place.projection.is_empty()
                    )
                }) {
                    match name.as_str() {
                        "gos_rt_set_insert_i64" => {
                            // The inserted value names the element's declared
                            // width, which the set handle's own i64 type does
                            // not carry.
                            let unsigned = args.get(1).is_some_and(|arg| match arg {
                                Operand::Copy(place) => matches!(
                                    self.tcx.kind(self.unwrap_ref(self.place_leaf_ty(place))),
                                    Some(TyKind::Int(IntTy::U64 | IntTy::Usize))
                                ),
                                _ => false,
                            });
                            elem_kind = Some(if unsigned {
                                ConcatKind::SetUint(is_btree)
                            } else {
                                ConcatKind::SetI64(is_btree)
                            });
                        }
                        "gos_rt_set_insert" => elem_kind = Some(ConcatKind::SetString(is_btree)),
                        // An aggregate element is stored as its slot bytes;
                        // the inserted operand names the type whose descriptor
                        // renders them.
                        "gos_rt_set_insert_skey" => {
                            if let Some(Operand::Copy(place)) = args.get(1)
                                && let Some(desc) = self.value_descriptor(self.place_leaf_ty(place))
                            {
                                elem_kind = Some(ConcatKind::SetDesc(desc, is_btree));
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if elem_kind.is_some() || saw_set_ctor {
            elem_kind.or(Some(ConcatKind::SetString(is_btree)))
        } else {
            None
        }
    }

    /// Symbol of `ty`'s derived `Debug` formatter (`Type::fmt`), or `None`
    /// when `ty` is not a user ADT or the unit defines no such formatter.
    /// Built-in generic ADTs (`Option`, `Result`, the set types) never carry
    /// one, so they fall out through the same lookup.
    pub(crate) fn adt_debug_fmt_symbol(&self, ty: Ty) -> Option<String> {
        let ty = self.unwrap_ref(ty);
        if !matches!(self.tcx.kind(ty), Some(TyKind::Adt { .. })) {
            return None;
        }
        // Impl methods register under the type's bare source name, so a
        // generic instantiation drops its argument suffix (`Wrap<f64>` ->
        // `Wrap`); `adt#N` is the placeholder for an unnamed Adt.
        let rendered = gossamer_types::printer::render_ty(self.tcx, ty);
        let bare = rendered.rsplit("::").next().unwrap_or(&rendered);
        let bare = bare.split('<').next().unwrap_or(bare);
        if bare.starts_with("adt#") {
            return None;
        }
        let sym = format!("{bare}::fmt");
        self.param_tys_by_name.contains_key(&sym).then_some(sym)
    }

    /// True when `ty`'s derived `fmt` receives the address of the value's
    /// slot buffer. A struct is stored as flat slots and read through that
    /// address; an enum's value is a single word - an inline tag or an RC
    /// node pointer - whose `fmt` decodes that word directly.
    pub(crate) fn adt_fmt_takes_slot_address(&self, ty: Ty) -> bool {
        match self.tcx.kind(self.unwrap_ref(ty)) {
            Some(TyKind::Adt { def, .. }) => self.tcx.enum_variant_tys(*def).is_none(),
            _ => false,
        }
    }

    /// Rendering plan for an `Option` / `Result` payload: a fixed runtime tag
    /// for scalars, Strings, and collections, or the payload ADT's derived
    /// `fmt`. `None` for a payload no formatter reaches.
    pub(crate) fn debug_payload_plan(&self, ty: Ty) -> Option<DebugPayload> {
        if let Some(tag) = self.debug_payload_kind(ty) {
            return Some(DebugPayload::Tag(tag));
        }
        if let Some(sym) = self.adt_debug_fmt_symbol(ty) {
            return Some(DebugPayload::Fmt(sym));
        }
        // A tuple payload carries its own self-describing tag stream.
        if let Some(TyKind::Tuple(_)) = self.tcx.kind(self.unwrap_ref(ty)) {
            return self.tuple_elem_tags(ty).map(DebugPayload::Tuple);
        }
        self.value_descriptor(ty).map(DebugPayload::Desc)
    }

    /// Maps an `Option` / `Result` payload type to the `gos_rt_debug_*`
    /// formatter kind (0=i64, 1=u64, 2=f64, 3=bool, 4=char, 5=String), or
    /// `None` for an aggregate / nested payload the scalar helper can't render.
    pub(crate) fn debug_payload_kind(&self, ty: Ty) -> Option<u8> {
        let ty = self.unwrap_ref(ty);
        match self.tcx.kind(ty) {
            // A `u64` / `usize` payload reads as unsigned, so a value at or
            // above `i64::MAX` renders as its own decimal rather than the
            // negative the same bits spell. The VM boxes the payload as its
            // `Uint` value for the same reason.
            Some(TyKind::Int(IntTy::U64 | IntTy::Usize)) => Some(1),
            Some(TyKind::Int(_)) => Some(0),
            Some(TyKind::Float(_)) => Some(2),
            Some(TyKind::Bool) => Some(3),
            Some(TyKind::Char) => Some(4),
            Some(TyKind::String) => Some(5),
            // A parsed document renders through its own display helper.
            Some(TyKind::JsonValue) => Some(8),
            // `errors::Error` is the error arm of nearly every fallible
            // signature, so a `Result` carrying one renders like the rest.
            Some(TyKind::DynError) => Some(10),
            // `Result<(), E>` is the shape of every fallible routine that
            // reports only success or failure; its Ok arm renders `()`.
            Some(TyKind::Unit) => Some(i64::from(gossamer_abi::DEBUG_PAYLOAD_UNIT) as u8),
            // A `Vec` payload is rendered by the same formatter a bare
            // `{:?}` of that vec uses; the slot carries its pointer.
            Some(TyKind::Vec(elem) | TyKind::Slice(elem)) => {
                match self.tcx.kind(self.unwrap_ref(*elem)) {
                    // An unsigned element has no scalar vec formatter of its
                    // own here; the descriptor path renders it, element tag
                    // and all.
                    Some(TyKind::Int(IntTy::U64 | IntTy::Usize)) => None,
                    Some(TyKind::Int(_)) => Some(6),
                    Some(TyKind::String) => Some(7),
                    _ => None,
                }
            }
            _ => None,
        }
    }

    /// Per-element tag for `gos_rt_tuple_format`, or `None` when the
    /// element type can't be rendered from a raw 8-byte tuple slot.
    /// Integers are restricted to 64-bit width and floats to `f64`
    /// because a narrower scalar stores fewer than 8 bytes into its
    /// slot, so reading the slot back as an i64 / f64 bit pattern
    /// would pick up adjacent bytes. `bool` (low bit) and `char` (low
    /// 32 bits) are read with a mask, so both are safe.
    /// Tags describing `elem` in a `gos_rt_tuple_format` stream: one byte
    /// for a scalar, or the `8, count, <nested tags…>` form for a nested
    /// tuple whose slots are flattened into the parent's buffer.
    /// Rendering plan for a map value the tag encoding cannot name: a struct
    /// or enum through its derived `fmt`, a tuple through its element tags.
    fn map_aggregate_value_kind(&self, key: Ty, value: Ty) -> ConcatKind {
        if let Some(sym) = self.adt_debug_fmt_symbol(value) {
            return ConcatKind::MapAdt(sym);
        }
        if let Some(TyKind::Tuple(nested)) = self.tcx.kind(self.unwrap_ref(value)) {
            let arity = nested.len();
            if let Some(tags) = self.tuple_elem_tags(value)
                && arity > 0
                && tags.len() > 2
            {
                return ConcatKind::MapTuple(tags[2..].to_vec(), arity);
            }
        }
        match (self.value_descriptor(key), self.value_descriptor(value)) {
            (Some(mut stream), Some(val)) => {
                let val_at = stream.len();
                stream.extend(val);
                ConcatKind::MapDesc(stream, val_at)
            }
            _ => ConcatKind::Unsupported,
        }
    }

    /// Recursive rendering descriptor for `ty`: the scalar tags a tuple stream
    /// already uses, plus container tags whose element descriptors follow. A
    /// shape with no descriptor - anything needing a derived `fmt` - is `None`.
    pub(crate) fn value_descriptor(&self, ty: Ty) -> Option<Vec<u8>> {
        let ty = self.unwrap_ref(ty);
        match self.tcx.kind(ty) {
            Some(TyKind::Tuple(elems)) => {
                let elems: Vec<Ty> = elems.clone();
                if elems.is_empty() || elems.len() > usize::from(u8::MAX) {
                    return None;
                }
                let mut out = vec![gossamer_abi::TUPLE_TAG_NESTED, elems.len() as u8];
                for e in &elems {
                    out.extend(self.value_descriptor(*e)?);
                }
                Some(out)
            }
            Some(TyKind::Vec(elem) | TyKind::Slice(elem)) => {
                let elem = *elem;
                let mut out = vec![gossamer_abi::DESC_VEC];
                out.extend(self.value_descriptor(elem)?);
                Some(out)
            }
            Some(TyKind::HashMap { key, value, .. }) => {
                let (key, value) = (*key, *value);
                let mut out = vec![gossamer_abi::DESC_MAP];
                out.extend(self.value_descriptor(key)?);
                out.extend(self.value_descriptor(value)?);
                Some(out)
            }
            Some(TyKind::Adt { def, substs }) if matches!(def.local, n if n == u32::MAX - 7 || n == u32::MAX - 18) =>
            {
                let ordered = u8::from(def.local == u32::MAX - 18);
                let elem = self.unwrap_ref(*substs.types().first()?);
                match self.tcx.kind(elem) {
                    Some(TyKind::Int(_) | TyKind::Bool | TyKind::Char) => {
                        Some(vec![gossamer_abi::DESC_SET_I64, ordered])
                    }
                    Some(TyKind::String) => Some(vec![gossamer_abi::DESC_SET_STR, ordered]),
                    _ => None,
                }
            }
            // A nested `Result` / `Option` is carried as a pointer to its
            // two words, so the renderer needs the arms' own descriptors
            // to read whichever one the discriminant selects.
            Some(TyKind::Adt { def, substs })
                if def.local == u32::MAX || def.local == u32::MAX - 1 =>
            {
                let is_option = def.local == u32::MAX - 1;
                let tys = substs.types();
                let mut out = vec![if is_option {
                    gossamer_abi::DESC_OPTION
                } else {
                    gossamer_abi::DESC_RESULT
                }];
                out.extend(self.value_descriptor(*tys.first()?)?);
                if !is_option {
                    out.extend(self.value_descriptor(*tys.get(1)?)?);
                }
                Some(out)
            }
            // `errors::Error` is the Err arm of nearly every fallible
            // signature, so a descriptor walk has to be able to name it.
            Some(TyKind::DynError) => Some(vec![gossamer_abi::DESC_ERROR]),
            _ => self.tuple_elem_tag(ty).map(|tag| vec![tag]),
        }
    }

    pub(crate) fn tuple_elem_tags(&self, elem: Ty) -> Option<Vec<u8>> {
        if let Some(TyKind::Tuple(nested)) = self.tcx.kind(self.unwrap_ref(elem)) {
            let nested: Vec<Ty> = nested.clone();
            if nested.is_empty() || nested.len() > usize::from(u8::MAX) {
                return None;
            }
            let mut out = vec![gossamer_abi::TUPLE_TAG_NESTED, nested.len() as u8];
            for e in &nested {
                out.extend(self.tuple_elem_tags(*e)?);
            }
            return Some(out);
        }
        self.tuple_elem_tag(elem).map(|tag| vec![tag])
    }

    pub(crate) fn tuple_elem_tag(&self, elem: Ty) -> Option<u8> {
        match self.tcx.kind(self.unwrap_ref(elem)) {
            // A `u64` / `usize` slot spans the whole unsigned range, so it
            // reads as unsigned wherever a tag stream names it.
            Some(TyKind::Int(IntTy::U64 | IntTy::Usize)) => Some(1),
            Some(TyKind::Int(i)) if int_width(*i) == 64 => Some(0),
            Some(TyKind::Duration | TyKind::Instant) => Some(0),
            Some(TyKind::Float(FloatTy::F64)) => Some(2),
            Some(TyKind::Bool) => Some(3),
            Some(TyKind::Char) => Some(4),
            Some(TyKind::String) => Some(5),
            Some(TyKind::Vec(inner) | TyKind::Slice(inner))
                if matches!(self.tcx.kind(self.unwrap_ref(*inner)), Some(TyKind::Int(_))) =>
            {
                Some(6)
            }
            Some(TyKind::HashMap { key, value, .. })
                if self.map_kv_supported(*key) && self.map_kv_supported(*value) =>
            {
                Some(7)
            }
            _ => None,
        }
    }

    /// True when a `HashMap` key/value type is one `gos_rt_map_format`
    /// renders from its live storage: an integer (rendered as a signed
    /// decimal, matching the VM) or a `String` (rendered bare).
    pub(crate) fn map_kv_supported(&self, ty: Ty) -> bool {
        matches!(
            self.tcx.kind(self.unwrap_ref(ty)),
            Some(TyKind::Int(_) | TyKind::String)
        )
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

    /// Masks an i64 SSA value to the declared width of `target`
    /// and extends it back to i64 by the target's signedness.
    /// This is the single point where a narrow integer type's
    /// width becomes observable under the i64 runtime model:
    /// `as u8` zero-extends the low byte, `as i8` sign-extends it.
    pub(crate) fn mask_to_int_width(&mut self, value: &str, target: IntTy) -> String {
        let width = int_width(target);
        let narrow = self.fresh();
        writeln!(self.out, "  {narrow} = trunc i64 {value} to i{width}").unwrap();
        let ext = if int_signed(target) { "sext" } else { "zext" };
        let widened = self.fresh();
        writeln!(self.out, "  {widened} = {ext} i{width} {narrow} to i64").unwrap();
        widened
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
        // Nothing converts to `void`: it is legal only as a function
        // result, and a unit destination stores nothing at all. Hand the
        // value back untouched so the caller's store - which skips a
        // unit slot - has something well-formed to name.
        if to_ty == "void" {
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
                // Through i64 - LLVM has no direct ptr→double.
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
            // Floating-point width changes are value-preserving
            // conversions, not bit reinterpretations: `fptrunc`
            // narrows double → float (f32), `fpext` widens the
            // other way. A `bitcast` between them is invalid IR.
            ("double", "float") => "fptrunc",
            ("float", "double") => "fpext",
            // Integer ↔ floating-point conversions use `sitofp`
            // / `fptosi` (signed) - `bitcast` reinterprets bits
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
            // A static load produces the static's declared scalar type.
            Rvalue::StaticLoad(sref) => render_ty(self.tcx, sref.ty),
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
    /// `true` when `op` is a `String` / `&String` value: a copy of a
    /// string-typed place, or an inlined string literal (`Const(Str)`,
    /// the shape copy-propagation leaves a single-use string binding).
    /// Mirrors Cranelift's `operand_is_string` so the string-comparison
    /// route fires identically on both compiled tiers.
    pub(crate) fn operand_is_string(&self, op: &Operand) -> bool {
        match op {
            Operand::Const(ConstValue::Str(_)) => true,
            Operand::Copy(p) => {
                let ty = self.place_leaf_ty(p);
                match self.tcx.kind(ty) {
                    Some(TyKind::String) => true,
                    Some(TyKind::Ref { inner, .. }) => {
                        matches!(self.tcx.kind(*inner), Some(TyKind::String))
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }

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
                    Some(TyKind::Adt { def, substs }) => self
                        .tcx
                        .adt_field_tys(*def, substs)
                        .and_then(|tys| tys.get(*i as usize).copied())
                        .unwrap_or(ty),
                    Some(TyKind::Tuple(elems)) => elems.get(*i as usize).copied().unwrap_or(ty),
                    Some(TyKind::Array { elem, .. }) => *elem,
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
        // Any ≤64-bit integer local works: they all render as
        // `i64` and classify as `NumericKind::Int`, which is all
        // callers use the borrowed Ty for.
        for decl in &self.body.locals {
            if let Some(TyKind::Int(i)) = self.tcx.kind(decl.ty)
                && int_width(*i) <= 64
            {
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
// `i128` on both sides - applying the vector ABI to it asymmetrically miscompiles
// every `Result`/`Option`/inline-enum a user function returns on Windows. Both
// call emitters (`emit_named_call`, `lower_runtime_call_intrinsic`) MUST gate on
// the registry return type via this one decision so they cannot drift apart.
pub(crate) fn needs_win64_fat_ret(is_windows: bool, registry_ret: Option<&str>) -> bool {
    is_windows && registry_ret == Some("i128")
}

/// Runtime format shim for a container handle's sentinel `DefId`:
/// `Deque` (`u32::MAX - 19`), `MaxHeap` (`- 28`), `MinHeap` (`- 30`),
/// `Queue` (`- 31`), `Stack` (`- 32`). These containers hold their
/// elements in the runtime, so one shim per container renders the text
/// every tier prints.
pub(crate) fn container_format_symbol(def_local: u32) -> Option<&'static str> {
    Some(match u32::MAX - def_local {
        19 => "gos_rt_deque_format",
        28 => "gos_rt_bheap_max_format",
        30 => "gos_rt_bheap_min_format",
        31 => "gos_rt_queue_format",
        32 => "gos_rt_stack_format",
        _ => return None,
    })
}

/// Runtime format shim for a container handle a local was constructed
/// by, for the locals the MIR types as a bare `i64` handle rather than
/// the container's sentinel ADT.
pub(crate) fn container_ctor_format_symbol(ctor: &str) -> Option<&'static str> {
    Some(match ctor {
        "gos_rt_deque_new" | "gos_rt_deque_from_vec_i64" => "gos_rt_deque_format",
        "gos_rt_queue_new" | "gos_rt_queue_from_vec_i64" => "gos_rt_queue_format",
        "gos_rt_stack_new" | "gos_rt_stack_from_vec_i64" => "gos_rt_stack_format",
        "gos_rt_bheap_max_new_i64" | "gos_rt_bheap_max_from_vec_i64" => "gos_rt_bheap_max_format",
        "gos_rt_bheap_min_new_i64" | "gos_rt_bheap_min_from_vec_i64" => "gos_rt_bheap_min_format",
        _ => return None,
    })
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
