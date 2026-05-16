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
    /// Inline fast path for `gos_rt_stream_write_byte(stream, b)`.
    ///
    /// The stdout case dominates the fasta benchmark (50M+
    /// calls). Going through an FFI call for every byte spends
    /// hundreds of millions of nanoseconds in PLT + stack-frame
    /// setup alone. Inlining the buffer-append (load len,
    /// bounds check, store byte, increment len) cuts those
    /// hot-loop calls down to ~5 instructions each.
    ///
    /// Shape:
    /// ```llvm
    ///   %fd = load i32, ptr %stream
    ///   %is_stdout = icmp eq i32 %fd, 1
    ///   br i1 %is_stdout, label %fast_check, label %slow
    /// fast_check:
    ///   %len = load i64, ptr @GOS_RT_STDOUT_LEN
    ///   %full = icmp uge i64 %len, 8192
    ///   br i1 %full, label %slow, label %append
    /// append:
    ///   %dst = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 %len
    ///   %byte = trunc i64 %b to i8
    ///   store i8 %byte, ptr %dst
    ///   %newlen = add i64 %len, 1
    ///   store i64 %newlen, ptr @GOS_RT_STDOUT_LEN
    ///   br label %end
    /// slow:
    ///   call void @gos_rt_stream_write_byte_slow(ptr %stream, i64 %b)
    ///   br label %end
    /// end:
    /// ```
    pub(crate) fn lower_stream_write_byte_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        for sym in [
            "gos_rt_stdout_acquire",
            "gos_rt_stdout_release",
            "gos_rt_stream_write_byte",
        ] {
            declare_rt(&mut self.runtime_refs, sym);
        }
        let stream_v = self.lower_operand(&args[0])?;
        let byte_v = self.lower_operand(&args[1])?;
        // Suffix to keep block labels unique within a function.
        let suffix = self.next_ssa;
        self.next_ssa += 1;
        let fast_check = format!("wb_check_{suffix}");
        let append = format!("wb_append_{suffix}");
        let slow = format!("wb_slow_{suffix}");
        let end = format!("wb_end_{suffix}");

        // Read fd and route stdout (fd==1) to the fast path.
        // `!invariant.load` tells LLVM the fd field of a stream
        // never changes after construction (every stream the
        // runtime exposes is a static `&STREAM_*`), so the load
        // can be hoisted out of containing loops. Without the
        // hint LLVM keeps a per-iteration `cmpl $1, (%stream)`
        // which is the hot path of fasta's inner loop.
        let fd = self.fresh();
        writeln!(
            self.out,
            "  {fd} = load i32, ptr {stream_v}, !invariant.load !0"
        )
        .unwrap();
        let is_stdout = self.fresh();
        writeln!(self.out, "  {is_stdout} = icmp eq i32 {fd}, 1").unwrap();
        writeln!(
            self.out,
            "  br i1 {is_stdout}, label %{fast_check}, label %{slow}"
        )
        .unwrap();

        // fast_check: bounds-check the buffer. Take the
        // process-global stdout lock first so this thread's
        // load+store on `@GOS_RT_STDOUT_LEN` cannot tear against
        // a concurrent goroutine on another worker thread.
        // `gos_rt_stdout_acquire` / `_release` wrap a
        // `parking_lot::RawMutex`; uncontended cost is ~10 ns.
        writeln!(self.out, "{fast_check}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_acquire()").unwrap();
        let len = self.fresh();
        writeln!(self.out, "  {len} = load i64, ptr @GOS_RT_STDOUT_LEN").unwrap();
        let full = self.fresh();
        writeln!(self.out, "  {full} = icmp uge i64 {len}, 8192").unwrap();
        // On overflow we still hold the lock — release before
        // routing to the slow call path so the slow path can
        // re-acquire through the safe Rust guard.
        let full_release = format!("wb_full_rel_{suffix}");
        writeln!(
            self.out,
            "  br i1 {full}, label %{full_release}, label %{append}"
        )
        .unwrap();
        writeln!(self.out, "{full_release}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{slow}").unwrap();

        // append: store the byte at bytes[len], bump len, release.
        writeln!(self.out, "{append}:").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 {len}"
        )
        .unwrap();
        let byte_8 = self.fresh();
        writeln!(self.out, "  {byte_8} = trunc i64 {byte_v} to i8").unwrap();
        writeln!(self.out, "  store i8 {byte_8}, ptr {dst}").unwrap();
        let newlen = self.fresh();
        writeln!(self.out, "  {newlen} = add i64 {len}, 1").unwrap();
        writeln!(self.out, "  store i64 {newlen}, ptr @GOS_RT_STDOUT_LEN").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // slow: full-call path. The runtime helper acquires the
        // lock itself through the safe `StdoutGuard`.
        writeln!(self.out, "{slow}:").unwrap();
        writeln!(
            self.out,
            "  call void @gos_rt_stream_write_byte(ptr {stream_v}, i64 {byte_v})"
        )
        .unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // Merge.
        writeln!(self.out, "{end}:").unwrap();
        // Destination is `()`; nothing to store.
        let _ = destination;
        match target {
            Some(t) => writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap(),
            None => writeln!(self.out, "  unreachable").unwrap(),
        }
        Ok(())
    }

    /// Inline fast path for `gos_rt_heap_i64_set(v, idx,
    /// val)`. The `GosI64Vec` is laid out as
    /// `{ i64 len; ptr data }` (8-byte aligned); we load
    /// `data` from offset 8, index it by `idx`, store `val`.
    /// Skips bounds checks — user code is expected to keep
    /// `idx` in range.
    pub(crate) fn lower_heap_i64_set_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(&args[0])?;
        let idx = self.lower_operand(&args[1])?;
        let val = self.lower_operand(&args[2])?;
        let data_ptr_addr = self.fresh();
        writeln!(
            self.out,
            "  {data_ptr_addr} = getelementptr i8, ptr {v}, i64 8"
        )
        .unwrap();
        let data = self.fresh();
        writeln!(self.out, "  {data} = load ptr, ptr {data_ptr_addr}").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i64, ptr {data}, i64 {idx}"
        )
        .unwrap();
        writeln!(self.out, "  store i64 {val}, ptr {dst}").unwrap();
        let _ = destination;
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_heap_i64_get(v, idx) ->
    /// i64`. Mirror of `lower_heap_i64_set_inline`.
    pub(crate) fn lower_heap_i64_get_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(&args[0])?;
        let idx = self.lower_operand(&args[1])?;
        let data_ptr_addr = self.fresh();
        writeln!(
            self.out,
            "  {data_ptr_addr} = getelementptr i8, ptr {v}, i64 8"
        )
        .unwrap();
        let data = self.fresh();
        writeln!(self.out, "  {data} = load ptr, ptr {data_ptr_addr}").unwrap();
        let src = self.fresh();
        writeln!(
            self.out,
            "  {src} = getelementptr i64, ptr {data}, i64 {idx}"
        )
        .unwrap();
        let val = self.fresh();
        writeln!(self.out, "  {val} = load i64, ptr {src}").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {val}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_vec_len(v) -> i64`. The
    /// `GosVec` heap struct stores `len: i64` at offset 0, so the
    /// runtime helper degenerates to one load. Inlining skips the
    /// FFI call entirely; LLVM then hoists the load when `v` is
    /// loop-invariant.
    pub(crate) fn lower_vec_len_inline(
        &mut self,
        arg: &Operand,
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let v = self.lower_operand(arg)?;
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = load i64, ptr {v}").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {tmp}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_str_len(s) -> i64`. Strings
    /// are null-terminated, so the length is `strlen(s)` —
    /// LLVM has a builtin `@strlen` that constant-folds against
    /// rodata literals. Folding is critical because user code
    /// like `let alu_len = alu.len()` becomes a compile-time
    /// constant, which collapses every `idx % alu_len` modulus
    /// in the hot loop from a real `idiv` (~20-40 cycles) to a
    /// multiply-by-magic.
    pub(crate) fn lower_str_len_inline(
        &mut self,
        arg: &Operand,
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let s_v = self.lower_operand(arg)?;
        // `strlen` returns `size_t` (assumed 64-bit on the
        // targets we care about). Declare it once at the
        // module level via the runtime-refs set.
        self.runtime_refs
            .insert("declare i64 @strlen(ptr)".to_string());
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = call i64 @strlen(ptr {s_v})").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {tmp}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for
    /// `gos_rt_stream_write_byte_array(stream, arr, len)`.
    ///
    /// Pack the low byte of every i64 slot in `arr[..len]`
    /// directly into the stdout buffer. The stream-fd check is
    /// hoisted (via `!invariant.load !0`); when we know the fd
    /// is 1 we drop into a tight pack loop that LLVM unrolls
    /// when `len` is compile-time-known. For the fasta_block /
    /// fasta_mt programs `len` is `line_len + 1` ≤ 61 and the
    /// buffer is rarely full, so the slow path almost never
    /// fires.
    ///
    /// Layout summary:
    /// ```llvm
    ///   %fd = load i32, ptr %stream, !invariant.load !0
    ///   %is_stdout = icmp eq i32 %fd, 1
    ///   br i1 %is_stdout, label %fast_check, label %slow_call
    /// fast_check:
    ///   %len = load i64, ptr @GOS_RT_STDOUT_LEN
    ///   %sum = add i64 %len, %wlen
    ///   %fits = icmp ule i64 %sum, 8192
    ///   br i1 %fits, label %pack, label %slow_call
    /// pack:
    ///   %i = phi i64 [0, %fast_check], [%inext, %pack_body]
    ///   %done = icmp uge i64 %i, %wlen
    ///   br i1 %done, label %store_len, label %pack_body
    /// pack_body:
    ///   %src = getelementptr i64, ptr %arr, i64 %i
    ///   %v = load i64, ptr %src
    ///   %byte = trunc i64 %v to i8
    ///   %dst = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 %newlen
    ///   store i8 %byte, ptr %dst
    ///   ; loop
    /// store_len:
    ///   store i64 %sum, ptr @GOS_RT_STDOUT_LEN
    ///   br label %end
    /// slow_call:
    ///   call void @gos_rt_stream_write_byte_array(...)
    ///   br label %end
    /// end:
    /// ```
    pub(crate) fn lower_stream_write_byte_array_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        for sym in [
            "gos_rt_stdout_acquire",
            "gos_rt_stdout_release",
            "gos_rt_stream_write_byte_array",
        ] {
            declare_rt(&mut self.runtime_refs, sym);
        }
        let stream_v = self.lower_operand(&args[0])?;
        let arr_v = self.lower_operand(&args[1])?;
        let len_v = self.lower_operand(&args[2])?;
        let suffix = self.next_ssa;
        self.next_ssa += 1;
        let fast_check = format!("wba_check_{suffix}");
        let pack_header = format!("wba_pack_{suffix}");
        let pack_body = format!("wba_body_{suffix}");
        let store_len_lbl = format!("wba_store_{suffix}");
        let slow = format!("wba_slow_{suffix}");
        let end = format!("wba_end_{suffix}");

        // fd check
        let fd = self.fresh();
        writeln!(
            self.out,
            "  {fd} = load i32, ptr {stream_v}, !invariant.load !0"
        )
        .unwrap();
        let is_stdout = self.fresh();
        writeln!(self.out, "  {is_stdout} = icmp eq i32 {fd}, 1").unwrap();
        writeln!(
            self.out,
            "  br i1 {is_stdout}, label %{fast_check}, label %{slow}"
        )
        .unwrap();

        // Capacity check. Acquire the stdout lock before the
        // `LEN` load so the read + `LEN` store on the inline
        // path are atomic with respect to other goroutines.
        // The lock is released along every exit (store_len,
        // and the slow-call branch).
        writeln!(self.out, "{fast_check}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_acquire()").unwrap();
        let cur_len = self.fresh();
        writeln!(self.out, "  {cur_len} = load i64, ptr @GOS_RT_STDOUT_LEN").unwrap();
        let new_len = self.fresh();
        writeln!(self.out, "  {new_len} = add i64 {cur_len}, {len_v}").unwrap();
        let fits = self.fresh();
        writeln!(self.out, "  {fits} = icmp ule i64 {new_len}, 8192").unwrap();
        let fits_release = format!("wba_nofit_rel_{suffix}");
        writeln!(
            self.out,
            "  br i1 {fits}, label %{pack_header}, label %{fits_release}"
        )
        .unwrap();
        writeln!(self.out, "{fits_release}:").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{slow}").unwrap();

        // Pack loop header (PHI for the loop counter).
        writeln!(self.out, "{pack_header}:").unwrap();
        let i_phi = self.fresh();
        writeln!(
            self.out,
            "  {i_phi} = phi i64 [ 0, %{fast_check} ], [ %t_inext_{suffix}, %{pack_body} ]",
        )
        .unwrap();
        let done = self.fresh();
        writeln!(self.out, "  {done} = icmp uge i64 {i_phi}, {len_v}").unwrap();
        writeln!(
            self.out,
            "  br i1 {done}, label %{store_len_lbl}, label %{pack_body}"
        )
        .unwrap();

        // Pack body — read arr[i], pack into buf[cur_len + i].
        writeln!(self.out, "{pack_body}:").unwrap();
        let src = self.fresh();
        writeln!(
            self.out,
            "  {src} = getelementptr i64, ptr {arr_v}, i64 {i_phi}"
        )
        .unwrap();
        let raw = self.fresh();
        writeln!(self.out, "  {raw} = load i64, ptr {src}").unwrap();
        let byte = self.fresh();
        writeln!(self.out, "  {byte} = trunc i64 {raw} to i8").unwrap();
        let dst_off = self.fresh();
        writeln!(self.out, "  {dst_off} = add i64 {cur_len}, {i_phi}").unwrap();
        let dst = self.fresh();
        writeln!(
            self.out,
            "  {dst} = getelementptr i8, ptr @GOS_RT_STDOUT_BYTES, i64 {dst_off}"
        )
        .unwrap();
        writeln!(self.out, "  store i8 {byte}, ptr {dst}").unwrap();
        // increment counter — must use the exact name we
        // forward-referenced in the PHI above.
        writeln!(self.out, "  %t_inext_{suffix} = add i64 {i_phi}, 1").unwrap();
        writeln!(self.out, "  br label %{pack_header}").unwrap();

        // Store the new length once we've packed the whole block,
        // then release the stdout lock acquired in fast_check.
        writeln!(self.out, "{store_len_lbl}:").unwrap();
        writeln!(self.out, "  store i64 {new_len}, ptr @GOS_RT_STDOUT_LEN").unwrap();
        writeln!(self.out, "  call void @gos_rt_stdout_release()").unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // Slow path: fall back to the runtime helper.
        writeln!(self.out, "{slow}:").unwrap();
        writeln!(
            self.out,
            "  call void @gos_rt_stream_write_byte_array(ptr {stream_v}, ptr {arr_v}, i64 {len_v})"
        )
        .unwrap();
        writeln!(self.out, "  br label %{end}").unwrap();

        // End — destination is `()`; nothing to store.
        writeln!(self.out, "{end}:").unwrap();
        let _ = destination;
        match target {
            Some(t) => writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap(),
            None => writeln!(self.out, "  unreachable").unwrap(),
        }
        Ok(())
    }

    /// Inline `v.push(x)` for arbitrary element widths.
    /// `gos_rt_vec_push(*mut GosVec, *const u8)` reads the
    /// element through the second pointer; the i64 / ptr value
    /// needs to land on the stack first so we can hand the
    /// helper an `&value` instead of `value`. Mirrors the
    /// Cranelift backend's `lower_intrinsic_call` stack-slot
    /// dance for the same symbol.
    pub(crate) fn lower_vec_push_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let vec_v = self.lower_operand(&args[0])?;
        let vec_ty = self.operand_llvm_ty(&args[0]);
        let vec_ptr = if vec_ty == "ptr" {
            vec_v
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = inttoptr {vec_ty} {vec_v} to ptr").unwrap();
            tmp
        };
        // Aggregate-element push (`xs.push((a, b))` where the
        // element type is a tuple/struct/array): the runtime
        // `gos_rt_vec_push(vec, ptr)` memcpys `vec.elem_bytes`
        // bytes from `ptr`. The scalar path below spills a
        // pointer-sized value into an `alloca i64` and would
        // copy `elem_bytes` from a too-small slot, clobbering
        // the vec's storage. Pass the operand's slot address
        // directly so the memcpy reads the full aggregate.
        if let Operand::Copy(p) = &args[1]
            && is_aggregate(self.tcx, self.place_leaf_ty(p))
        {
            let val_addr = if p.projection.is_empty() {
                local_slot(p.local)
            } else {
                self.lower_place_address(p)
            };
            declare_rt(&mut self.runtime_refs, "gos_rt_vec_push");
            writeln!(
                self.out,
                "  call void @gos_rt_vec_push(ptr {vec_ptr}, ptr {val_addr})"
            )
            .unwrap();
            if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
                let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
                let dslot = local_slot(destination.local);
                let zero = match dest_ty.as_str() {
                    "ptr" => "null".to_string(),
                    "double" | "float" => "0.0".to_string(),
                    _ => "0".to_string(),
                };
                writeln!(self.out, "  store {dest_ty} {zero}, ptr {dslot}").unwrap();
            }
            emit_terminator_branch(&mut self.out, target);
            return Ok(());
        }
        let val_v = self.lower_operand(&args[1])?;
        let val_ty = self.operand_llvm_ty(&args[1]);
        // 8-byte slot — every scalar / GC pointer fits in one
        // word, matching the runtime's `elem_bytes=8` default.
        let slot = self.fresh();
        writeln!(self.out, "  {slot} = alloca i64").unwrap();
        let val_i64 = match val_ty.as_str() {
            "i64" => val_v,
            "i32" | "i16" | "i8" | "i1" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = sext {val_ty} {val_v} to i64").unwrap();
                tmp
            }
            "double" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = bitcast double {val_v} to i64").unwrap();
                tmp
            }
            "float" => {
                let mid = self.fresh();
                writeln!(self.out, "  {mid} = fpext float {val_v} to double").unwrap();
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = bitcast double {mid} to i64").unwrap();
                tmp
            }
            "ptr" => {
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = ptrtoint ptr {val_v} to i64").unwrap();
                tmp
            }
            _ => val_v,
        };
        writeln!(self.out, "  store i64 {val_i64}, ptr {slot}").unwrap();
        declare_rt(&mut self.runtime_refs, "gos_rt_vec_push");
        writeln!(
            self.out,
            "  call void @gos_rt_vec_push(ptr {vec_ptr}, ptr {slot})"
        )
        .unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let dest_ty = render_ty(self.tcx, self.body.local_ty(destination.local));
            let dslot = local_slot(destination.local);
            let zero = match dest_ty.as_str() {
                "ptr" => "null".to_string(),
                "double" | "float" => "0.0".to_string(),
                _ => "0".to_string(),
            };
            writeln!(self.out, "  store {dest_ty} {zero}, ptr {dslot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }

    /// Inline fast path for `gos_rt_str_byte_at(s, i) -> i64`.
    ///
    /// The bytecode is `*((s as *const u8) + i)` zero-extended
    /// to i64. We skip the runtime's null check since the
    /// caller already validated that the string handle is
    /// non-null at construction; null pointers will segfault
    /// rather than silently returning 0, but that matches
    /// every other byte-load path in the language.
    pub(crate) fn lower_str_byte_at_inline(
        &mut self,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let s_v = self.lower_operand(&args[0])?;
        let i_v = self.lower_operand(&args[1])?;
        let addr = self.fresh();
        writeln!(
            self.out,
            "  {addr} = getelementptr i8, ptr {s_v}, i64 {i_v}"
        )
        .unwrap();
        let byte = self.fresh();
        writeln!(self.out, "  {byte} = load i8, ptr {addr}").unwrap();
        let ext = self.fresh();
        writeln!(self.out, "  {ext} = zext i8 {byte} to i64").unwrap();
        if !is_unit(self.tcx, self.body.local_ty(destination.local)) {
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store i64 {ext}, ptr {slot}").unwrap();
        }
        emit_terminator_branch(&mut self.out, target);
        Ok(())
    }
}
