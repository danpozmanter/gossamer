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
    /// Populates an aggregate stack slot (the destination
    /// `place`'s flat layout) with each operand in order.
    /// Each operand occupies one i64-wide slot for scalar
    /// fields; nested aggregates add their own slot count.
    pub(crate) fn emit_aggregate_store(
        &mut self,
        place: &Place,
        operands: &[Operand],
    ) -> Result<(), BuildError> {
        if !place.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "Aggregate assignment through projection",
            ));
        }
        let base = local_slot(place.local);
        let mut slot_idx = 0u32;
        for operand in operands {
            let op_ty = self.operand_ty(operand);
            let mut op_slots = slot_count(self.tcx, op_ty).unwrap_or(1);
            // `operand_ty` falls back to the return-slot type for
            // `Const(Str)` and FnRef operands when their own type
            // isn't directly representable. When the function
            // returns a multi-slot aggregate (struct return), that
            // fallback inflates `op_slots` to the return type's
            // slot count, sending a 1-slot string-literal operand
            // through the multi-slot memcpy branch which then
            // bails because the source is a Const, not a place.
            // Every Const* and FnRef value is exactly 1 slot in the
            // flat ABI (the underlying constant produces a single
            // i64/double/ptr). Force `op_slots = 1` to match.
            if matches!(operand, Operand::Const(_) | Operand::FnRef { .. }) {
                op_slots = 1;
            }
            if op_slots == 0 {
                // A genuine zero-slot operand (Unit). Nothing to
                // store; skip to the next operand without
                // advancing `slot_idx`.
                continue;
            }
            // A struct/tuple field whose own value is a single-slot
            // aggregate (e.g. a nested `struct Inner { tag: i64 }`) must be
            // copied by value into the parent's slot - `lower_operand` of an
            // aggregate place yields its *address*, so the scalar-store branch
            // would embed a pointer and the parent would read it back inline as
            // garbage. Route 1-slot aggregate copies through the memcpy branch.
            let op_is_aggregate_copy =
                matches!(operand, Operand::Copy(_)) && is_aggregate(self.tcx, op_ty);
            if op_slots == 1 && !op_is_aggregate_copy {
                let v = self.lower_operand(operand)?;
                let op_llvm = self.operand_llvm_ty(operand);
                let dst = self.fresh();
                writeln!(
                    self.out,
                    "  {dst} = getelementptr i64, ptr {base}, i64 {slot_idx}"
                )
                .unwrap();
                writeln!(self.out, "  store {op_llvm} {v}, ptr {dst}").unwrap();
            } else {
                // Nested aggregate. The operand may be either a
                // bare-local copy (`Operand::Copy(p)` with empty
                // projection - the original supported shape) or
                // a projected place (`base.inner`, `tuple.0`,
                // etc.). For both, the source is an in-memory
                // place whose address we compute through
                // `lower_place_address`, then memcpy `op_slots *
                // 8` bytes into the destination slot. The
                // memcpy expands at `llc` time to the platform's
                // best sequence.
                let src_place = match operand {
                    Operand::Copy(p) => p,
                    Operand::Const(_) | Operand::FnRef { .. } => {
                        // Genuinely multi-slot const / FnRef are
                        // not surfaced by the current MIR: every
                        // multi-slot constructor is materialised
                        // into a temp local first, and FnRef is
                        // always 1-slot. The Unit-typed
                        // mis-classification is handled by the
                        // `op_slots == 0` recovery above so this
                        // branch is unreachable on well-formed
                        // input; we surface it as a hard error
                        // rather than a silent miscompile.
                        return Err(BuildError::Unsupported(
                            "nested aggregate operand must be a Copy(place); \
                             multi-slot constants and FnRef values are not \
                             materialised through the aggregate-store path",
                        ));
                    }
                };
                let src = self.lower_place_address(src_place);
                let dst = self.fresh();
                writeln!(
                    self.out,
                    "  {dst} = getelementptr i64, ptr {base}, i64 {slot_idx}"
                )
                .unwrap();
                let bytes = u64::from(op_slots) * 8;
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)"
                )
                .unwrap();
            }
            slot_idx += op_slots;
        }
        Ok(())
    }

    /// `[value; count]` - fills `count` slots with the same
    /// scalar `value`. Small counts are unrolled (`llc -O3`
    /// later SLP-vectorises); larger counts drop into a
    /// tight loop to keep module text small.
    pub(crate) fn emit_repeat_store(
        &mut self,
        place: &Place,
        value: &Operand,
        count: u64,
    ) -> Result<(), BuildError> {
        if !place.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "Repeat assignment through projection",
            ));
        }
        let v = self.lower_operand(value)?;
        let v_llvm = self.operand_llvm_ty(value);
        let base = local_slot(place.local);
        if count <= 16 {
            for i in 0..count {
                let dst = self.fresh();
                writeln!(self.out, "  {dst} = getelementptr i64, ptr {base}, i64 {i}").unwrap();
                writeln!(self.out, "  store {v_llvm} {v}, ptr {dst}").unwrap();
            }
        } else {
            let head = self.fresh_label("repeat_head");
            let body = self.fresh_label("repeat_body");
            let done = self.fresh_label("repeat_done");
            let counter = self.fresh();
            writeln!(self.out, "  {counter} = alloca i64").unwrap();
            writeln!(self.out, "  store i64 0, ptr {counter}").unwrap();
            writeln!(self.out, "  br label %{head}").unwrap();
            writeln!(self.out, "{head}:").unwrap();
            let cur = self.fresh();
            writeln!(self.out, "  {cur} = load i64, ptr {counter}").unwrap();
            let cond = self.fresh();
            writeln!(self.out, "  {cond} = icmp ult i64 {cur}, {count}").unwrap();
            writeln!(self.out, "  br i1 {cond}, label %{body}, label %{done}").unwrap();
            writeln!(self.out, "{body}:").unwrap();
            let dst = self.fresh();
            writeln!(
                self.out,
                "  {dst} = getelementptr i64, ptr {base}, i64 {cur}"
            )
            .unwrap();
            writeln!(self.out, "  store {v_llvm} {v}, ptr {dst}").unwrap();
            let next = self.fresh();
            writeln!(self.out, "  {next} = add i64 {cur}, 1").unwrap();
            writeln!(self.out, "  store i64 {next}, ptr {counter}").unwrap();
            writeln!(self.out, "  br label %{head}").unwrap();
            writeln!(self.out, "{done}:").unwrap();
        }
        Ok(())
    }

    /// Renders an aggregate / variant `value` to a `*c_char`
    /// suitable for `gos_rt_print_str` / `gos_rt_concat_str`.
    /// Each kind is routed through a runtime helper that walks
    /// the value's layout and produces a Display string. The Arr*
    /// kinds carry the static length so the helper knows the
    /// flat buffer's bounds.
    pub(crate) fn emit_aggregate_format(&mut self, kind: ConcatKind, value: &str) -> String {
        let dest = self.fresh();
        match kind {
            ConcatKind::VecI64 => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_i64");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_i64(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::VecF64 => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_f64");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_f64(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::VecBool => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_bool");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_bool(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::VecString => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_string");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_string(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::VecVecI64 => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_vec_i64");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_vec_i64(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::VecVecString => {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_format_vec_string");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_vec_format_vec_string(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::ArrI64(n) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_arr_format_i64");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_arr_format_i64(ptr {value}, i64 {n})"
                )
                .unwrap();
            }
            ConcatKind::ArrF64(n) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_arr_format_f64");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_arr_format_f64(ptr {value}, i64 {n})"
                )
                .unwrap();
            }
            ConcatKind::ArrBool(n) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_arr_format_bool");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_arr_format_bool(ptr {value}, i64 {n})"
                )
                .unwrap();
            }
            ConcatKind::ArrString(n) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_arr_format_string");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_arr_format_string(ptr {value}, i64 {n})"
                )
                .unwrap();
            }
            ConcatKind::JsonValue => {
                declare_rt(&mut self.runtime_refs, "gos_rt_json_display");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_json_display(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::ErrorMessage => {
                // Display renders the colon-joined cause chain;
                // `.message()` keeps `gos_rt_error_message`.
                declare_rt(&mut self.runtime_refs, "gos_rt_error_display");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_error_display(ptr {value})"
                )
                .unwrap();
            }
            ConcatKind::Map => {
                declare_rt(&mut self.runtime_refs, "gos_rt_map_format");
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_map_format(ptr {value})"
                )
                .unwrap();
            }
            // The operand is the by-value `i128` enum (disc + payload), not a
            // buffer pointer. It crosses the `extern "C"` boundary through
            // `fat_i128_call_arg`, which on Win64 spills it to a 16-byte slot
            // and passes `ptr` (matching the runtime's `__int128` ABI) and on
            // SysV passes the bare `i128`.
            ConcatKind::Option(payload_kind) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_debug_option");
                let opt_arg = self.fat_i128_call_arg(value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_debug_option({opt_arg}, i64 {payload_kind})"
                )
                .unwrap();
            }
            ConcatKind::Result(ok_kind, err_kind) => {
                declare_rt(&mut self.runtime_refs, "gos_rt_debug_result");
                let res_arg = self.fat_i128_call_arg(value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_debug_result({res_arg}, i64 {ok_kind}, i64 {err_kind})"
                )
                .unwrap();
            }
            _ => unreachable!("emit_aggregate_format called with non-aggregate kind"),
        }
        dest
    }

    /// Routes an aggregate-print operand to its runtime formatter.
    /// `Tuple` needs the operand to recompute its per-element tags;
    /// every other aggregate kind is fully described by `kind` +
    /// `value` and delegates to [`Self::emit_aggregate_format`].
    pub(crate) fn emit_concat_aggregate(
        &mut self,
        arg: &Operand,
        kind: ConcatKind,
        value: &str,
    ) -> Result<String, BuildError> {
        match kind {
            ConcatKind::Tuple => self.emit_tuple_format(arg, value),
            _ => Ok(self.emit_aggregate_format(kind, value)),
        }
    }

    /// Emits the `gos_rt_tuple_format(buf, n, tags)` call for a tuple
    /// operand. `value` is the address of the tuple's flat `[N x i64]`
    /// slot buffer; the tag array is interned as a module constant of
    /// raw bytes (one per element) and its body pointer passed as
    /// `tags`.
    fn emit_tuple_format(&mut self, arg: &Operand, value: &str) -> Result<String, BuildError> {
        let Operand::Copy(p) = arg else {
            return Err(BuildError::Unsupported(
                "tuple format expects a place operand",
            ));
        };
        let leaf = self.unwrap_ref(self.place_leaf_ty(p));
        let Some(TyKind::Tuple(elems)) = self.tcx.kind(leaf) else {
            return Err(BuildError::Unsupported(
                "tuple format on a non-tuple operand",
            ));
        };
        let elems: Vec<Ty> = elems.clone();
        let mut tags: Vec<u8> = Vec::with_capacity(elems.len());
        for e in &elems {
            match self.tuple_elem_tag(*e) {
                Some(t) => tags.push(t),
                None => {
                    return Err(BuildError::Unsupported(
                        "tuple element type is not formattable on the compiled tier",
                    ));
                }
            }
        }
        let n = elems.len();
        // The tag bytes are all < 0x80, so each maps to a single UTF-8
        // byte: the interned constant's body is exactly the tag array.
        let tag_str: String = tags.iter().map(|&b| b as char).collect();
        let (tags_global, _) = self.strings.borrow_mut().intern(&tag_str);
        declare_rt(&mut self.runtime_refs, "gos_rt_tuple_format");
        let dest = self.fresh();
        writeln!(
            self.out,
            "  {dest} = call ptr @gos_rt_tuple_format(ptr {value}, i64 {n}, ptr {tags_global})"
        )
        .unwrap();
        Ok(dest)
    }
}
