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
    pub(crate) fn lower_block(
        &mut self,
        block: &gossamer_mir::BasicBlock,
    ) -> Result<(), BuildError> {
        writeln!(self.out, "bb{}:", block.id.as_u32()).unwrap();
        // No loop-back-edge safepoint. A runtime call on every
        // iteration is opaque to `opt -O3` and blocks vectorisation
        // of tight numeric loops — the difference between sub-1-second
        // and 50-second runs on spectral-norm and n-body. Allocation-
        // driven safepoint dispatch (`gos_rt_aggr_alloc` updates the
        // byte-pressure counter; the next function-prologue safepoint
        // collects when the threshold trips) is sufficient for any
        // loop that allocates; pure-arithmetic loops have nothing to
        // collect.
        let _ = block.id;
        let cleanup = gossamer_mir::plan_cleanup_with_summary(self.body, &self.capture_summary);
        for entry in cleanup.at_block_entry(block.id) {
            self.emit_cleanup_call(entry);
        }
        for stmt in &block.stmts {
            self.lower_stmt(stmt)?;
        }
        for entry in cleanup.at_block_exit(block.id) {
            self.emit_cleanup_call(entry);
        }
        self.current_block = Some(block.id.as_u32());
        self.lower_terminator(&block.terminator)?;
        self.current_block = None;
        Ok(())
    }

    pub(crate) fn lower_stmt(&mut self, stmt: &Statement) -> Result<(), BuildError> {
        match &stmt.kind {
            StatementKind::Assign { place, rvalue } => {
                self.lower_assign(place, rvalue)?;
            }
            StatementKind::StorageLive(local) => {
                // Hint to LLVM's register allocator that the
                // alloca's storage becomes live. Treat unit /
                // zero-sized locals as no-ops since they have no
                // alloca.
                if !is_unit(self.tcx, self.body.local_ty(*local)) {
                    let slot = local_slot(*local);
                    let bytes =
                        u64::from(slot_count(self.tcx, self.body.local_ty(*local)).unwrap_or(1))
                            * 8;
                    writeln!(
                        self.out,
                        "  call void @llvm.lifetime.start.p0(i64 {bytes}, ptr {slot})"
                    )
                    .unwrap();
                }
            }
            StatementKind::StorageDead(local) => {
                if !is_unit(self.tcx, self.body.local_ty(*local)) {
                    let slot = local_slot(*local);
                    let bytes =
                        u64::from(slot_count(self.tcx, self.body.local_ty(*local)).unwrap_or(1))
                            * 8;
                    writeln!(
                        self.out,
                        "  call void @llvm.lifetime.end.p0(i64 {bytes}, ptr {slot})"
                    )
                    .unwrap();
                }
            }
            StatementKind::Nop => {}
            StatementKind::SetDiscriminant { place, variant } => {
                // Stores the variant index at offset 0 of the
                // enum's backing place. Matches the Cranelift
                // convention: tag at slot 0, payload at +8.
                let addr = if place.projection.is_empty() {
                    local_slot(place.local)
                } else {
                    self.lower_place_address(place)
                };
                writeln!(
                    self.out,
                    "  store i64 {variant}, ptr {addr}",
                    variant = *variant,
                )
                .unwrap();
            }
            StatementKind::GcWriteBarrier { value, .. } => {
                // Explicit GcWriteBarrier statements are emitted
                // by the MIR layer for stores that the
                // codegen-side lower_assign post-store sniffer
                // wouldn't catch (runtime-helper internal writes
                // exposed through MIR, aggregate-field writes
                // that the per-field shape detection misses).
                //
                // The runtime's `gos_rt_write_barrier(u32)` takes
                // a GcRef index — only i64-encoded values
                // (the flat ABI's GcRef shape) reach this path.
                // Pointer-typed runtime handles are tracked by
                // the GC's allocation registry without a barrier.
                let value_ty = self.operand_llvm_ty(value);
                if value_ty == "i64" {
                    let Ok(lowered) = self.lower_operand(value) else {
                        return Ok(());
                    };
                    self.emit_write_barrier(&lowered);
                }
            }
        }
        Ok(())
    }

    pub(crate) fn lower_assign(
        &mut self,
        place: &Place,
        rvalue: &Rvalue,
    ) -> Result<(), BuildError> {
        let dest_ty_mir = self.body.local_ty(place.local);
        if is_unit(self.tcx, dest_ty_mir) {
            // Even when the destination's MIR type is unit, the
            // rvalue may be a side-effecting intrinsic (gos_store
            // sinks, etc.). Funnel those into the raw-intrinsic
            // path so the IR records the side effect.
            if let Rvalue::CallIntrinsic { name, args } = rvalue
                && matches!(
                    *name,
                    "gos_load" | "gos_store" | "gos_alloc" | "gos_fn_addr"
                )
            {
                return self.lower_raw_intrinsic(name, args, place, None);
            }
            // Drop-style intrinsic calls (`gos_rt_map_free`,
            // `gos_rt_vec_free`, etc.) emitted by the MIR cleanup
            // pass come through with a unit-typed destination
            // because their result is `()`. Without this branch
            // the call would be dropped on the floor and the
            // container would leak until process exit. Route any
            // `gos_rt_*` intrinsic at the runtime-call path so
            // the IR records the side effect.
            if let Rvalue::CallIntrinsic { name, args } = rvalue
                && name.starts_with("gos_rt_")
            {
                self.lower_runtime_call_intrinsic(name, args, place.local)?;
                return Ok(());
            }
            return Ok(());
        }
        // Aggregate constructions (`Aggregate`, `Repeat`) are
        // routed straight at the destination slot — they
        // populate the stack aggregate in-place rather than
        // producing a scalar value to store.
        match rvalue {
            Rvalue::Aggregate { operands, .. } => {
                return self.emit_aggregate_store(place, operands);
            }
            Rvalue::Repeat { value, count } => {
                return self.emit_repeat_store(place, value, *count);
            }
            // Rvalue-position raw heap intrinsics (the
            // `coerce_to_fn_trait_if_needed` MIR pass uses
            // these for the FnTrait env blob, and lifted
            // closures use them for env materialisation).
            // Reuse the same inline handler the terminator path
            // hits via `lower_call`.
            Rvalue::CallIntrinsic { name, args }
                if matches!(
                    *name,
                    "gos_load" | "gos_store" | "gos_alloc" | "gos_fn_addr"
                ) =>
            {
                return self.lower_raw_intrinsic(name, args, place, None);
            }
            _ => {}
        }
        // Whole-aggregate copy: when the destination is an
        // aggregate local and the rvalue is a plain `Use(Copy)`
        // of another aggregate value (a bare local OR a
        // projected aggregate field — `let p = pts[i]`), memcpy
        // the flat storage rather than trying to load/store it
        // as a single scalar.
        if place.projection.is_empty() && is_aggregate(self.tcx, dest_ty_mir) {
            if let Rvalue::Use(Operand::Copy(src_place)) = rvalue {
                let src_leaf_ty = self.place_leaf_ty(src_place);
                if is_aggregate(self.tcx, src_leaf_ty) {
                    let bytes =
                        u64::from(slot_count(self.tcx, dest_ty_mir).unwrap_or(1).max(1)) * 8;
                    let src_addr = if src_place.projection.is_empty() {
                        local_slot(src_place.local)
                    } else {
                        self.lower_place_address(src_place)
                    };
                    writeln!(
                        self.out,
                        "  call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)",
                        dst = local_slot(place.local),
                        src = src_addr,
                    )
                    .unwrap();
                    return Ok(());
                }
            }
        }
        let leaf_ty = self.place_leaf_ty(place);
        let leaf_llvm = render_ty(self.tcx, leaf_ty);
        let value = self.lower_rvalue(rvalue, place.local)?;
        // The rvalue's LLVM type may differ from the destination
        // slot's leaf type for several shapes:
        //
        //   * `Use(FnRef)` returns a `ptr` literal; when the
        //     destination is an `i64` slot (goroutine-spawn path
        //     stores fn addresses as i64), coerce ptr → i64.
        //   * `Use(Const(Int(n)))` returns the integer literal
        //     `n` as an `i64`; when the destination is a float
        //     slot (closure capturing a float, struct field of
        //     type `f64`, etc.), the bare `store double 16` IR
        //     is rejected by `opt`/`llc`. Coerce i64 → double.
        //   * `Use(Const(Float(...)))` returns a `0xH…` literal
        //     typed `double`; when the destination is integer-
        //     shaped (rare; format-precision args route this
        //     way), coerce double → i64.
        //
        // The pre-0.5.0 silent Cranelift fallback masked these
        // mismatches because Cranelift accepted them. Strict
        // LLVM verification surfaces them; coerce here.
        let rvalue_llvm = self.rvalue_llvm_ty(rvalue);
        // When the rvalue is void (e.g. `Use(Copy(_tmp))` where the
        // source local was assigned the result of a void-returning
        // runtime call), `lower_place_read` returns an empty string.
        // Coercing `bitcast void <empty> to ptr` is invalid IR.
        // Synthesise a null sentinel matching the destination's
        // leaf type so the slot has a well-defined bit pattern when
        // the return path or any later use reads it.
        let (rvalue_llvm, value) = if rvalue_llvm == "void" || value.is_empty() {
            let sentinel = match leaf_llvm.as_str() {
                "ptr" => "null".to_string(),
                "double" | "float" => "0.0".to_string(),
                _ => "0".to_string(),
            };
            (leaf_llvm.clone(), sentinel)
        } else {
            (rvalue_llvm, value)
        };
        let value = if rvalue_llvm != leaf_llvm && !rvalue_llvm.is_empty() && leaf_llvm != "void" {
            self.coerce_llvm_value(&value, &rvalue_llvm, &leaf_llvm)
        } else {
            value
        };
        let addr = if place.projection.is_empty() {
            local_slot(place.local)
        } else {
            self.lower_place_address(place)
        };
        // When a runtime call returns a heap pointer to an
        // aggregate (e.g. `gos_rt_result_payload` returning a
        // heap-allocated Bag / ExecOutput / tuple), the destination
        // is an inline `[N x i64]` alloca. A bare `store ptr` only
        // writes the blob address into slot 0; subsequent field
        // reads then load the blob pointer instead of the actual
        // field value. Memcpy the full struct instead. This applies
        // to every aggregate slot count, including N==1: a 1-slot
        // `Bag { items: Vec<String> }` value-semantically holds a
        // Vec ptr at offset 0, NOT the Bag's address itself.
        if place.projection.is_empty()
            && leaf_llvm == "ptr"
            && is_aggregate(self.tcx, dest_ty_mir)
            && slot_count(self.tcx, dest_ty_mir).is_some_and(|n| n >= 1)
        {
            let bytes = u64::from(slot_count(self.tcx, dest_ty_mir).unwrap_or(1).max(1)) * 8;
            writeln!(
                self.out,
                "  call void @llvm.memcpy.p0.p0.i64(ptr {addr}, ptr {value}, i64 {bytes}, i1 false)"
            )
            .unwrap();
        } else {
            writeln!(self.out, "  store {leaf_llvm} {value}, ptr {addr}").unwrap();
        }
        // Write barrier: only when the value is an i64-encoded GC ref, not a
        // raw machine pointer. All pointer-bearing MIR types (Vec, String,
        // HashMap, Ref, …) render as `ptr` in LLVM IR — the runtime's
        // gos_rt_write_barrier(u32) takes a GcRef index, not a machine
        // address, so emitting `trunc i64 ptr to i32` is both invalid IR and
        // semantically wrong. Skip the barrier for ptr-typed values; the GC
        // tracks those through its allocation registry rather than write
        // barriers.
        if !place.projection.is_empty()
            && leaf_llvm != "ptr"
            && Self::is_pointer_local_ty(self.tcx, leaf_ty)
        {
            self.emit_write_barrier(&value);
        }
        Ok(())
    }

    pub(crate) fn lower_terminator(&mut self, term: &Terminator) -> Result<(), BuildError> {
        match term {
            Terminator::Return => {
                // Pop the call-stack frame pushed in the function
                // prologue so panic dumps walk the right stack.
                self.emit_stack_pop();
                // Emit cleanup calls for owning heap-typed locals before
                // the actual `ret`. Mirrors the Cranelift Return path —
                // see `gossamer_mir::plan_cleanup` for the analysis.
                let cleanup =
                    gossamer_mir::plan_cleanup_with_summary(self.body, &self.capture_summary);
                for entry in cleanup.at_return() {
                    self.emit_cleanup_call(entry);
                }
                // Raw-pointer tracing-GC shadow-stack restore. Pops
                // every root pushed inside this body. For the
                // aggregate-return path the heap copy below is
                // pushed AFTER the restore so the entry persists
                // into the caller's frame (matching Cranelift).
                self.emit_gc_root_restore();
                let ret_ty = self.body.local_ty(Local::RETURN);
                let ret_llvm = render_ty(self.tcx, ret_ty);
                if is_unit(self.tcx, ret_ty) {
                    writeln!(self.out, "  ret void").unwrap();
                } else if is_aggregate(self.tcx, ret_ty) {
                    // Aggregate return: the callee's `%l0` is a stack
                    // alloca whose storage dies when the frame pops.
                    // Heap-allocate so the returned pointer outlives
                    // the call, copy the inline data over, and return
                    // the heap pointer. Both LLVM and Cranelift
                    // callers can dereference the result safely.
                    let bytes = u64::from(slot_count(self.tcx, ret_ty).unwrap_or(1).max(1)) * 8;
                    declare_rt(&mut self.runtime_refs, "gos_rt_gc_alloc");
                    let heap = self.fresh();
                    writeln!(
                        self.out,
                        "  {heap} = call ptr @gos_rt_gc_alloc(i64 {bytes})"
                    )
                    .unwrap();
                    writeln!(
                        self.out,
                        "  call void @llvm.memcpy.p0.p0.i64(ptr {heap}, ptr {slot}, i64 {bytes}, i1 false)",
                        slot = local_slot(Local::RETURN)
                    )
                    .unwrap();
                    // Push after the restore so the entry persists
                    // into the caller's frame.
                    self.emit_gc_root_push(&heap);
                    writeln!(self.out, "  ret ptr {heap}").unwrap();
                } else {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load {ret_llvm}, ptr {slot}",
                        slot = local_slot(Local::RETURN)
                    )
                    .unwrap();
                    writeln!(self.out, "  ret {ret_llvm} {tmp}").unwrap();
                }
                Ok(())
            }
            Terminator::Goto { target } => {
                if self.current_block.is_some_and(|src| target.as_u32() <= src) {
                    self.emit_preempt_check();
                }
                writeln!(self.out, "  br label %bb{}", target.as_u32()).unwrap();
                Ok(())
            }
            Terminator::SwitchInt {
                discriminant,
                arms,
                default,
            } => {
                let src = self.current_block.unwrap_or(u32::MAX);
                let has_back_edge =
                    arms.iter().any(|(_, t)| t.as_u32() <= src) || default.as_u32() <= src;
                if has_back_edge {
                    self.emit_preempt_check();
                }
                let v = self.lower_operand(discriminant)?;
                let ty = render_ty(self.tcx, self.operand_ty(discriminant));
                writeln!(
                    self.out,
                    "  switch {ty} {v}, label %bb{default} [",
                    default = default.as_u32()
                )
                .unwrap();
                for (cst, target) in arms {
                    writeln!(self.out, "    {ty} {cst}, label %bb{}", target.as_u32()).unwrap();
                }
                writeln!(self.out, "  ]").unwrap();
                Ok(())
            }
            Terminator::Unreachable => {
                writeln!(self.out, "  unreachable").unwrap();
                Ok(())
            }
            Terminator::Panic { message } => {
                self.lower_panic(message);
                Ok(())
            }
            Terminator::Drop { target, .. } => {
                // Gossamer runtime manages drops through the GC
                // hooks; the MIR `Drop` terminator is a
                // sequencing point that the LLVM backend can
                // treat as a plain `Goto` without calling any
                // destructor (no-op drop).
                writeln!(self.out, "  br label %bb{}", target.as_u32()).unwrap();
                Ok(())
            }
            Terminator::Assert {
                cond,
                expected,
                target,
                msg,
            } => self.lower_assert(cond, *expected, *target, msg),
            Terminator::Call {
                callee,
                args,
                destination,
                target,
            } => self.lower_call(callee, args, destination, target.as_ref()),
        }
    }
}
