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
    pub(crate) fn new(body: &'a Body, tcx: &'a TyCtxt) -> Self {
        Self {
            body,
            tcx,
            out: String::new(),
            next_ssa: 0,
            runtime_refs: std::collections::BTreeSet::new(),
            fn_name_by_def: std::collections::HashMap::new(),
            param_tys_by_name: std::collections::HashMap::new(),
            strings: std::rc::Rc::new(std::cell::RefCell::new(StringPool::default())),
            current_block: None,
            preempt_seq: 0,
            capture_summary: gossamer_mir::CaptureSummary::default(),
            cabi_handlers: std::collections::BTreeMap::new(),
        }
    }

    /// Main entry point - emits the function's IR text in its
    /// entirety. The module-level global declarations (panic
    /// message constants, etc.) accumulated during lowering
    /// remain in `self.runtime_refs` and are read by the
    /// caller to prepend to the module.
    pub(crate) fn lower(&mut self) -> Result<String, BuildError> {
        // Reject i128 / u128 use up-front. The runtime ABI is i64
        // throughout; codegen would silently truncate or emit an
        // invalid `sext i128 to i64`. Surface a clear diagnostic
        // mentioning the offending type and the compiled tier.
        for (i, _) in self.body.locals.iter().enumerate() {
            let ty = self
                .body
                .local_ty(gossamer_mir::Local(u32::try_from(i).unwrap_or(0)));
            if matches!(
                self.tcx.kind(ty),
                Some(gossamer_types::TyKind::Int(
                    gossamer_types::IntTy::I128 | gossamer_types::IntTy::U128
                ))
            ) {
                return Err(BuildError::Unsupported(
                    "i128 / u128 is not supported by the compiled tier; \
                     use i64 / u64 or split the value into two 64-bit halves",
                ));
            }
        }
        // Closure bodies (`__closure_N`) used to fall back to
        // Cranelift because their MIR return slot was typed
        // `Unit`. The 2026-05-07 lift fix now propagates the
        // body's typeck-inferred return type into the lifted
        // fn signature, so the LLVM lowerer can emit them
        // through the same pipeline as user functions and the
        // generated `define`s become visible to the IR-shape
        // gates.
        self.emit_prelude();
        // Entry block opens with `alloca`s for every local.
        self.emit_allocas();
        // Copy function parameters into their local slots so
        // the rest of the body uniformly reads through
        // `local_slot`. MIR reserves `_1..=_arity` as
        // parameter locals.
        self.emit_param_stores();
        // No per-call call-stack instrumentation: panic traces and
        // SIGQUIT dumps for the compiled tier come from unwinding the
        // real machine stack on demand (see `gos_rt_panic` /
        // `sigquit::render_to`). A push/pop pair on every function
        // entry blocks leaf-function inlining and serialises on a
        // global lock, which is unacceptable in hot numeric loops.
        // Unconditional jump into the MIR entry block.
        writeln!(self.out, "  br label %bb0").unwrap();
        for block in &self.body.blocks {
            self.lower_block(block)?;
        }
        writeln!(self.out, "}}").unwrap();
        Ok(std::mem::take(&mut self.out))
    }

    /// Drains the module-level globals this body introduced
    /// (string constants for panic messages, etc.). Called by
    /// the emitter once the body text is in the module.
    pub(crate) fn take_module_globals(&mut self) -> Vec<String> {
        std::mem::take(&mut self.runtime_refs).into_iter().collect()
    }

    pub(crate) fn emit_prelude(&mut self) {
        let ret_ty = render_ty(self.tcx, self.body.local_ty(Local::RETURN));
        let mut params = String::new();
        for i in 0..self.body.arity {
            if i > 0 {
                params.push_str(", ");
            }
            let local = Local(i + 1);
            let p_ty = render_ty(self.tcx, self.body.local_ty(local));
            let _ = write!(params, "{p_ty} %p{i}");
        }
        writeln!(
            self.out,
            "define {ret_ty} @\"{name}\"({params}) {{",
            name = escape_ident(&mangle_fn_name(&self.body.name)),
            ret_ty = ret_ty,
            params = params,
        )
        .unwrap();
        writeln!(self.out, "entry:").unwrap();
    }

    pub(crate) fn emit_allocas(&mut self) {
        // Back-edges charge an estimate of their loop work against this
        // budget. Tiny scalar loops poll less often, while call-heavy loops
        // consume several units per iteration and retain prompt preemption.
        writeln!(self.out, "  %gos_preempt_counter = alloca i32").unwrap();
        writeln!(self.out, "  store i32 16384, ptr %gos_preempt_counter").unwrap();
        for (i, decl) in self.body.locals.iter().enumerate() {
            if is_unit(self.tcx, decl.ty) {
                // Zero-sized: skip. Reads return the singleton
                // `()` directly via emit-time folding.
                continue;
            }
            let slot = local_slot(Local(i as u32));
            if is_aggregate(self.tcx, decl.ty) {
                if let Some(bytes) = self.heap_spilled_local_bytes(Local(i as u32)) {
                    declare_rt(&mut self.runtime_refs, "gos_rt_aggr_alloc");
                    writeln!(
                        self.out,
                        "  {slot} = call ptr @gos_rt_aggr_alloc(i64 {bytes})"
                    )
                    .unwrap();
                    continue;
                }
                // Aggregates use Cranelift's flat layout:
                // 8-byte i64-sized slots, one per scalar
                // field, struct-of-struct flattened in
                // declaration order. `alloca [N x i64]`
                // gets us the same footprint and honours
                // 8-byte alignment the runtime expects.
                let slots = slot_count(self.tcx, decl.ty).unwrap_or(1).max(1);
                writeln!(self.out, "  {slot} = alloca [{slots} x i64]").unwrap();
            } else {
                let ty = render_ty(self.tcx, decl.ty);
                if ty == "i128" {
                    // Explicit ABI alignment: without it opt and llc can
                    // disagree about the slot's alignment (the module
                    // carries no datalayout string) and opt's memcpy
                    // expansion then emits 16-byte *aligned* vector ops
                    // against a frame slot llc placed at 8-mod-16 - a
                    // SIGSEGV at the first `vmovaps`.
                    writeln!(self.out, "  {slot} = alloca i128, align 16").unwrap();
                } else {
                    writeln!(self.out, "  {slot} = alloca {ty}").unwrap();
                }
            }
        }
    }

    pub(crate) fn heap_spilled_local_bytes(&self, local: Local) -> Option<u64> {
        let ty = self.body.local_ty(local);
        if !is_aggregate(self.tcx, ty) {
            return None;
        }
        let bytes = aggregate_storage_bytes(self.tcx, ty)?;
        (bytes > STACK_AGGREGATE_SPILL_BYTES).then_some(bytes)
    }

    pub(crate) fn emit_heap_spill_frees(&mut self) {
        for (i, _) in self.body.locals.iter().enumerate() {
            let local = Local(i as u32);
            let Some(bytes) = self.heap_spilled_local_bytes(local) else {
                continue;
            };
            declare_rt(&mut self.runtime_refs, "gos_rt_aggr_free");
            writeln!(
                self.out,
                "  call void @\"gos_rt_aggr_free\"(ptr {slot}, i64 {bytes})",
                slot = local_slot(local)
            )
            .unwrap();
        }
    }

    pub(crate) fn emit_param_stores(&mut self) {
        // A lifted closure (`__closure_N`) is invoked through a shape thunk that
        // forwards each argument BY VALUE (the runtime iter/sort helpers and the
        // `Fn` fat-pointer call site pass the element word directly). A directly
        // called function instead receives an inline aggregate BY POINTER (the
        // caller hands over the address of its flat-slot storage). The two
        // conventions only diverge for a heap-pointer aggregate (`slot_count`
        // = `None`: a recursive/heap enum, opaque blob handle) whose sole word
        // is the handle pointer: a closure gets that pointer as a value (store
        // it, exactly like a scalar), a direct callee gets its address (copy
        // the word out). Multi-slot aggregates (`slot_count = Some`) are always
        // by-pointer, so both memcpy.
        let is_closure = self.body.name.starts_with("__closure");
        for i in 0..self.body.arity {
            let local = Local(i + 1);
            let local_ty = self.body.local_ty(local);
            if is_unit(self.tcx, local_ty) {
                continue;
            }
            let slot = local_slot(local);
            let by_pointer = is_aggregate(self.tcx, local_ty)
                && (slot_count(self.tcx, local_ty).is_some() || !is_closure);
            if by_pointer {
                let bytes = u64::from(slot_count(self.tcx, local_ty).unwrap_or(1).max(1)) * 8;
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {slot}, ptr %p{i}, i64 {bytes}, i1 false)"
                )
                .unwrap();
            } else {
                let ty = render_ty(self.tcx, local_ty);
                writeln!(self.out, "  store {ty} %p{i}, ptr {slot}").unwrap();
            }
        }
    }
}
