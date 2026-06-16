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
    pub(crate) fn lower_operand(&mut self, op: &Operand) -> Result<String, BuildError> {
        match op {
            Operand::Copy(place) => Ok(self.lower_place_read(place)),
            Operand::Const(ConstValue::Str(text)) => {
                let (name, _len) = self.strings.borrow_mut().intern(text);
                Ok(name)
            }
            Operand::Const(value) => Ok(render_const(value)),
            Operand::FnRef { def, .. } => {
                // Emit the function symbol as a `ptr` value - that
                // matches `operand_llvm_ty(FnRef) = "ptr"` and the
                // `FnDef → ptr` rendering of the destination slot.
                // The MIR lowerer also stuffs fn addresses into
                // `i64` slots for the goroutine-spawn path; the
                // assignment-store branch in `lower_assign` coerces
                // ptr → i64 (`ptrtoint`) when the destination's
                // leaf type is integer-shaped, so both shapes get a
                // well-typed `store`.
                if let Some(name) = self.fn_name_by_def.get(&def.local).cloned() {
                    // A `[rust-bindings]` import has a stub MIR body
                    // but no emitted definition; its ABI also needs
                    // the arg/return conversions only the direct-call
                    // path performs. Reject it here with an
                    // actionable message instead of emitting a
                    // reference to an undefined symbol that dies
                    // inside `opt`.
                    if gossamer_resolve::lookup_external_item(&name).is_some() {
                        return Err(BuildError::Unsupported(
                            "a [rust-bindings] function cannot be passed as a value yet - \
                             wrap it in a closure: `|x| f(x)`",
                        ));
                    }
                    return Ok(format!("@\"{name}\""));
                }
                Err(BuildError::Unsupported("FnRef operand not yet lowered"))
            }
        }
    }

    /// Reads a place, walking its `projection` chain. For a
    /// plain local (no projection) this is a single `load`
    /// against the stack slot. For `a[i].field` chains we
    /// compute the byte-offset address via `getelementptr i64`
    /// steps and then `load` the leaf scalar at its native
    /// type - matching the flat-slot layout the Cranelift
    /// backend emits. String byte indexing (`s[i]` where `s`
    /// is `String` or `&String`) is short-circuited through
    /// `gos_rt_str_byte_at`.
    pub(crate) fn lower_place_read(&mut self, place: &Place) -> String {
        if let Some(value) = self.try_string_byte_read(place) {
            return value;
        }
        let leaf_ty = self.place_leaf_ty(place);
        let leaf_llvm = render_ty(self.tcx, leaf_ty);
        if leaf_llvm == "void" {
            return String::new();
        }
        if place.projection.is_empty() {
            // Multi-slot aggregates (`[Body; 5]`, structs, tuples)
            // are stored as a flat `[N x i64]` slab - the "value"
            // downstream code expects is the slot address itself.
            //
            // Exception: enum (Adt) types whose field layout is unknown
            // to the type context (`slot_count = None`) are heap-pointer
            // aggregates. Their `[1 x i64]` slot holds a heap pointer,
            // not inline field data. For these, do a `load ptr` to
            // recover the heap pointer rather than returning the stack
            // slot address. Example: `List { Cons(i64, Box<List>), Nil }`.
            let local_ty = self.body.local_ty(place.local);
            if is_aggregate(self.tcx, local_ty) {
                let sc = slot_count(self.tcx, local_ty);
                // Enum / unknown-layout Adt: slot_count returns None.
                // Load the pointer value stored in the slot.
                if sc.is_none() {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load {leaf_llvm}, ptr {slot}",
                        slot = local_slot(place.local)
                    )
                    .unwrap();
                    return tmp;
                }
                // Multi-slot or single-field struct: the address IS the value.
                return local_slot(place.local);
            }
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = load {leaf_llvm}, ptr {slot}",
                slot = local_slot(place.local)
            )
            .unwrap();
            return tmp;
        }
        let addr = self.lower_place_address(place);
        // When the projected leaf is itself a multi-slot aggregate
        // (struct/tuple/array embedded inline), return its address
        // rather than collapsing the sub-aggregate to its first
        // word. Mirrors the cranelift behaviour and keeps any
        // downstream `Field`/`Index` projections walking memory.
        if is_aggregate(self.tcx, leaf_ty) && slot_count(self.tcx, leaf_ty).unwrap_or(1) > 1 {
            return addr;
        }
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = load {leaf_llvm}, ptr {addr}").unwrap();
        tmp
    }

    /// Computes the pointer address for a projected place.
    /// Walks `Field` / `Index` / `Deref` steps as byte-offset
    /// `getelementptr` instructions against the root local's
    /// stack slot (or a dereferenced pointer).
    pub(crate) fn lower_place_address(&mut self, place: &Place) -> String {
        let mut current = local_slot(place.local);
        let mut current_ty = self.body.local_ty(place.local);
        // If the root local is a reference (`&[Body; 5]`) or a
        // runtime-managed pointer (Vec, Slice, String,
        // HashMap, …), the local's *slot* holds a pointer
        // to the actual storage; load it once so subsequent
        // projections walk the referent rather than the
        // alloca itself. Stack-allocated aggregates ([Body;5]
        // declared inline) hold the data directly in their slot
        // so we leave `current` pointing at the alloca.
        //
        // Skip this auto-deref when the first projection is itself
        // an explicit `Deref` - the projection performs the same
        // pointer-load, and applying both produces a double-deref
        // that lands on garbage (the case `*s = expr` where
        // `s: &mut i64`).
        let skip_auto_deref = matches!(place.projection.first(), Some(Projection::Deref));
        if !skip_auto_deref && Self::is_pointer_local_ty(self.tcx, current_ty) {
            let next = self.fresh();
            writeln!(self.out, "  {next} = load ptr, ptr {current}").unwrap();
            current = next;
            current_ty = self.unwrap_ref(current_ty);
        }
        let mut stride_slots: u32 = elem_slots(self.tcx, current_ty);
        for proj in &place.projection {
            match proj {
                Projection::Field(idx) => {
                    // Sum prior fields' slot widths so a nested
                    // struct field (`outer.inner.x`) lands past
                    // the embedded inner's full layout instead of
                    // overlapping it. Falls back to `idx` when the
                    // type is opaque (sentinel Adt, references) -
                    // in those cases each field is exactly one
                    // slot and `idx == slot_offset`.
                    let slot_offset = field_slot_offset(self.tcx, current_ty, *idx);
                    let next = self.fresh();
                    writeln!(
                        self.out,
                        "  {next} = getelementptr i64, ptr {current}, i64 {slot_offset}"
                    )
                    .unwrap();
                    current = next;
                    // Advance current_ty so the next projection's
                    // stride and field-offset computation reflects
                    // the projected field, not the parent.
                    current_ty = match self.tcx.kind(current_ty) {
                        Some(TyKind::Adt { def, .. }) => self
                            .tcx
                            .struct_field_tys(*def)
                            .and_then(|tys| tys.get(*idx as usize).copied())
                            .unwrap_or(current_ty),
                        Some(TyKind::Tuple(elems)) => {
                            elems.get(*idx as usize).copied().unwrap_or(current_ty)
                        }
                        _ => current_ty,
                    };
                    stride_slots = elem_slots(self.tcx, current_ty);
                }
                Projection::Index(index_local) => {
                    // Load the index value, widen to i64, then
                    // multiply by the per-element slot count
                    // and add to the base pointer.
                    let idx_slot = local_slot(*index_local);
                    let idx_raw = self.fresh();
                    writeln!(self.out, "  {idx_raw} = load i64, ptr {idx_slot}").unwrap();
                    // Audit C6: bounds-check the dynamic index
                    // against the statically-known fixed-array
                    // length. Skipped for non-Array shapes - Vec /
                    // Slice indexing routes through the runtime
                    // intrinsics which validate independently.
                    self.emit_array_bounds_check(current_ty, &idx_raw);
                    let next = self.fresh();
                    if stride_slots == 1 {
                        writeln!(
                            self.out,
                            "  {next} = getelementptr i64, ptr {current}, i64 {idx_raw}"
                        )
                        .unwrap();
                    } else {
                        let scaled = self.fresh();
                        writeln!(self.out, "  {scaled} = mul i64 {idx_raw}, {stride_slots}")
                            .unwrap();
                        writeln!(
                            self.out,
                            "  {next} = getelementptr i64, ptr {current}, i64 {scaled}"
                        )
                        .unwrap();
                    }
                    current = next;
                    // Advance current_ty to the element type so
                    // subsequent projections (multi-dim array, nested
                    // field, …) see the right shape. Without this,
                    // `arr[i][j]` over `[[T; N]; M]` would use the
                    // outer-array's stride/bounds for the inner index
                    // and panic / corrupt data - the underlying
                    // 3D-array bug from iron_knight's `make_zobrist`.
                    current_ty = match self.tcx.kind(current_ty) {
                        Some(
                            TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem),
                        ) => *elem,
                        _ => current_ty,
                    };
                    stride_slots = elem_slots(self.tcx, current_ty);
                }
                Projection::Deref => {
                    let next = self.fresh();
                    writeln!(self.out, "  {next} = load ptr, ptr {current}").unwrap();
                    current = next;
                    stride_slots = 1;
                }
                Projection::Discriminant => {
                    // Discriminant at offset 0; no pointer
                    // change, but later Field offsets walk past
                    // the tag word.
                    stride_slots = 1;
                }
                Projection::Downcast(_) => {
                    // Skip the 8-byte tag word to land on the
                    // payload.
                    let next = self.fresh();
                    writeln!(
                        self.out,
                        "  {next} = getelementptr i8, ptr {current}, i64 8"
                    )
                    .unwrap();
                    current = next;
                    stride_slots = 1;
                }
            }
        }
        current
    }
}
