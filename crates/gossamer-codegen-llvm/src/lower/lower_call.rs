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
    /// Indirect call lowering for `f(args…)` where `f` is a
    /// local variable holding either a plain function pointer
    /// or a closure-environment record. The callee classifier
    /// follows what the Cranelift backend does in its
    /// `call_indirect` arm:
    ///   1. `FnDef` / `FnPtr`-typed local → value is the fn
    ///      address; call directly with the plain arg list.
    ///   2. Closure env (other reference / opaque ptr local) →
    ///      load fn pointer from `env[0]`, then call with `env`
    ///      as the implicit first arg followed by the user
    ///      args.
    pub(crate) fn lower_indirect_call(
        &mut self,
        place: &Place,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported(
                "indirect call destination cannot have projections",
            ));
        }
        let callee_ty = self.body.local_ty(place.local);
        // Mirrors the Cranelift narrowing: only `FnDef`-typed
        // locals (the result of `Operand::FnRef`) hold a raw
        // function address. `FnPtr` / `FnTrait` locals carry an
        // env pointer post the MIR's let / return / assign
        // coercion, so they share the closure dispatch shape.
        let is_plain_fn = matches!(self.tcx.kind(callee_ty), Some(TyKind::FnDef { .. }));
        // Read the local's value: for a function pointer the
        // load yields the callable address; for a closure env
        // it yields the env pointer.
        let callee_llvm = render_ty(self.tcx, callee_ty);
        let raw_value = self.lower_place_read(place);
        // When a closure was stored through a Vec<i64> or similar
        // integer-typed container, the loaded value is an i64 holding
        // a pointer bit-pattern. Convert to ptr so it can be used as
        // a memory address for vtable + env dispatch.
        let env_value = if callee_llvm == "i64" {
            let p = self.fresh();
            writeln!(self.out, "  {p} = inttoptr i64 {raw_value} to ptr").unwrap();
            p
        } else {
            raw_value
        };
        let fn_ptr = if is_plain_fn {
            env_value.clone()
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {env_value}").unwrap();
            tmp
        };
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_llvm = render_ty(self.tcx, dest_ty_mir);
        let mut arg_text = String::new();
        if !is_plain_fn {
            // Closure: env is the first arg.
            arg_text.push_str("ptr ");
            arg_text.push_str(&env_value);
        }
        for arg in args {
            if !arg_text.is_empty() {
                arg_text.push_str(", ");
            }
            let a_ty = self.operand_llvm_ty(arg);
            let a_v = self.lower_operand(arg)?;
            let _ = write!(arg_text, "{a_ty} {a_v}");
        }
        if dest_llvm == "void" || is_unit(self.tcx, dest_ty_mir) {
            writeln!(self.out, "  call void {fn_ptr}({arg_text})").unwrap();
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = call {dest_llvm} {fn_ptr}({arg_text})").unwrap();
            let slot = local_slot(destination.local);
            writeln!(self.out, "  store {dest_llvm} {tmp}, ptr {slot}").unwrap();
        }
        match target {
            Some(t) => {
                writeln!(self.out, "  br label %bb{}", t.as_u32()).unwrap();
            }
            None => {
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        Ok(())
    }

    /// Lowers a single operand to a `ptr` SSA holding the
    /// argument's stringification. Strings pass through; numeric
    /// types route through their `gos_rt_*_to_str` helper.
    pub(crate) fn lower_arg_to_str_ptr(&mut self, arg: &Operand) -> Result<String, BuildError> {
        for sym in [
            "gos_rt_i64_to_str",
            "gos_rt_u64_to_str",
            "gos_rt_f64_to_str",
            "gos_rt_bool_to_str",
            "gos_rt_char_to_str",
        ] {
            declare_rt(&mut self.runtime_refs, sym);
        }
        let kind = self.concat_print_kind(arg);
        if matches!(kind, ConcatKind::Unsupported) {
            return Err(BuildError::Unsupported(
                "stringify of aggregate or variant types",
            ));
        }
        let value = self.lower_operand(arg)?;
        let dest = self.fresh();
        match kind {
            ConcatKind::StrPtr => Ok(value),
            ConcatKind::Int => {
                let widened = self.widen_to_i64(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_i64_to_str(i64 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Uint => {
                let widened = self.widen_to_u64(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_u64_to_str(i64 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Float => {
                let widened = self.widen_to_f64(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_f64_to_str(double {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Bool => {
                let widened = self.widen_bool_to_i32(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_bool_to_str(i32 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            ConcatKind::Char => {
                let widened = self.widen_char_to_i32(arg, &value);
                writeln!(
                    self.out,
                    "  {dest} = call ptr @gos_rt_char_to_str(i32 {widened})"
                )
                .unwrap();
                Ok(dest)
            }
            kind @ (ConcatKind::VecI64
            | ConcatKind::VecF64
            | ConcatKind::VecBool
            | ConcatKind::VecString
            | ConcatKind::VecVecI64
            | ConcatKind::VecVecString
            | ConcatKind::ArrI64(_)
            | ConcatKind::ArrF64(_)
            | ConcatKind::ArrBool(_)
            | ConcatKind::ArrString(_)
            | ConcatKind::JsonValue
            | ConcatKind::ErrorMessage) => Ok(self.emit_aggregate_format(kind, &value)),
            ConcatKind::Unsupported => unreachable!("checked above"),
        }
    }

    /// Direct-call lowering for `Operand::FnRef` and simple
    /// prelude-name calls (the MIR lowerer leaves prelude targets
    /// as `ConstValue::Str("println")` etc.). Indirect closure
    /// calls go through [`Self::lower_indirect_call`] via the
    /// `Operand::Copy` branch.
    pub(crate) fn lower_call(
        &mut self,
        callee: &Operand,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        if !destination.projection.is_empty() {
            return Err(BuildError::Unsupported("call with projected destination"));
        }
        let target_name: Option<String> = match callee {
            Operand::FnRef { def, .. } => {
                // Resolve through the per-module `DefId.local` →
                // name map populated by the emitter. Unknown
                // `def.local` means the referenced function isn't
                // in this MIR module - typically a stdlib helper
                // the frontend was expected to monomorphise but
                // didn't. 0.8.0: this is a hard error (no
                // Cranelift fallback) so the missing
                // monomorphisation surfaces at compile time.
                self.fn_name_by_def.get(&def.local).cloned()
            }
            Operand::Const(ConstValue::Str(name)) => Some(name.clone()),
            Operand::Copy(place) => {
                // `Copy(local)` callee: indirect call through a
                // function pointer. Two shapes:
                //   1. `FnDef`/`FnPtr` typed local - the value
                //      *is* the callable address.
                //   2. Closure env pointer - first heap word is
                //      the function pointer; env doubles as the
                //      implicit first argument.
                // Mirror Cranelift's `call_indirect` handling.
                self.lower_indirect_call(place, args, destination, target)?;
                return Ok(());
            }
            Operand::Const(_) => None,
        };
        let Some(name) = target_name else {
            // 0.8.0: this used to be a silent route-to-Cranelift
            // fallback. Now it's a hard error: report what was
            // unresolved so the frontend / MIR side has something
            // actionable to chase.
            let kind_label = match callee {
                Operand::FnRef { def, .. } => format!("FnRef def.local={:?}", def.local),
                Operand::Const(c) => format!("Const({c:?})"),
                Operand::Copy(_) => "Copy".to_string(),
            };
            return Err(BuildError::Unsupported(Box::leak(
                format!(
                    "indirect / closure call not lowered: callee shape {kind_label} \
                    has no resolvable name in fn_name_by_def - frontend monomorphisation \
                    bug or missing stdlib registration"
                )
                .into_boxed_str(),
            )));
        };
        // `__concat` is the parser's lowering of `println!`-style
        // formatted output: it takes a heterogeneous arg list,
        // prints each piece directly to stdout, and produces an
        // empty-string pointer for the surrounding `println` call
        // to consume. Mirror the Cranelift backend's per-arg
        // dispatch (one runtime print call per operand keyed off
        // the operand's MIR kind).
        if name == "__concat" {
            self.lower_concat_call(args, destination, target)?;
            return Ok(());
        }
        // `__fmt_prec(value, prec)` - emitted by macro expansion for
        // `{:.N}` specs. Routes through `gos_rt_f64_prec_to_str` so
        // the result is a heap String that the surrounding `__concat`
        // pipeline consumes like any other string operand.
        if name == "__fmt_prec" {
            self.lower_fmt_prec_call(args, destination, target)?;
            return Ok(());
        }
        // `println` / `print` / `eprintln` / `eprint` route to
        // the runtime's `gos_rt_print_str` for each arg, plus a
        // trailing `gos_rt_println()` for the `*ln` variants.
        // This mirrors what the Cranelift backend does in
        // `lower_intrinsic_call` - the runtime's println is
        // arity-0, so an inline `gos_rt_print_str(arg)` then
        // `gos_rt_println()` reproduces the user-level
        // `println(s)` semantics.
        if matches!(name.as_str(), "println" | "print" | "eprintln" | "eprint") {
            self.lower_print_call(&name, args, destination, target)?;
            return Ok(());
        }
        // `panic(args...)` builds a single concatenated message
        // (space-joined to mirror the interpreter's
        // `render_args`) and routes it through `gos_rt_panic`,
        // which is `noreturn` and emits the GX0005 prefix +
        // aborts. Fall back to an empty-string pointer when no
        // args were given.
        if name == "panic" {
            // `panic(args...)` builds a single space-joined
            // message via `gos_rt_str_concat` over the
            // per-arg `to_str` helpers, then calls
            // `gos_rt_panic` (noreturn). Empty arg list panics
            // with an empty message - `gos_rt_panic` then
            // emits its default "panic" string.
            declare_rt(&mut self.runtime_refs, "gos_rt_panic");
            let msg = self.emit_args_to_concat_string(args, " ")?;
            writeln!(self.out, "  call void @gos_rt_panic(ptr {msg})").unwrap();
            writeln!(self.out, "  unreachable").unwrap();
            return Ok(());
        }
        // Recognise `math::*` calls and emit a direct
        // LLVM intrinsic invocation instead of routing
        // through an undefined `@"math::sqrt"` symbol. These
        // lower to the host's SSE/AVX instruction via `llc`.
        if let Some(intrinsic_name) = math_intrinsic(&name)
            && args.len() == 1
        {
            self.lower_math_intrinsic(intrinsic_name, &args[0], destination, target)?;
            return Ok(());
        }
        // Hot path inlining for byte-at-a-time stdout writes.
        // The runtime exposes the stdout buffer as a pair of
        // global symbols (`@GOS_RT_STDOUT_BYTES` / `@GOS_RT_STDOUT_LEN`);
        // we emit the buffer-append fast path directly so that
        // fasta-style inner loops (50M+ calls) don't pay one
        // FFI call per byte. Only the slow path (full buffer)
        // falls through to `gos_rt_stream_write_byte`.
        if name == "gos_rt_stream_write_byte" && args.len() == 2 {
            self.lower_stream_write_byte_inline(args, destination, target)?;
            return Ok(());
        }
        // Bulk byte-array write for stdout: pack `len` low-bytes
        // of an `[i64; N]` array into the global stdout buffer
        // inline. The fasta_block / fasta_mt programs call
        // `out.write_byte_array(&line, line_len + 1)` once per
        // 60-char line; without inlining each line pays one
        // FFI call. With inlining the loop bound is usually
        // a compile-time-known small integer, so LLVM unrolls
        // and the per-byte pack drops to one `mov` + `inc`.
        if name == "gos_rt_stream_write_byte_array" && args.len() == 3 {
            self.lower_stream_write_byte_array_inline(args, destination, target)?;
            return Ok(());
        }
        // Hot path inlining for `s[i]` byte reads. Strings are
        // null-terminated `*const u8` in the runtime; the
        // out-of-bounds case (which would need `strlen`) costs
        // O(strlen) per access for fasta-style `alu[idx %
        // alu_len]` loops, so we inline the simple `addr+i`
        // load assuming the user's modulus keeps `i` in range.
        if name == "gos_rt_str_byte_at" && args.len() == 2 {
            self.lower_str_byte_at_inline(args, destination, target)?;
            return Ok(());
        }
        // Hot path inlining for `s.len()` on strings. Lowers
        // to `i64 @strlen(ptr s)` (a libc call LLVM
        // constant-folds for compile-time-known string
        // constants). With the constant in hand, modulus
        // operations like `idx % alu_len` reduce to
        // multiply-by-magic instead of `idiv`, which dominates
        // the fasta inner loop.
        if name == "gos_rt_str_len" && args.len() == 1 {
            self.lower_str_len_inline(&args[0], destination, target)?;
            return Ok(());
        }
        // `vec.len()` reads the `len: i64` field at offset 0 of the
        // `GosVec` heap struct. One FFI call per loop iteration
        // would otherwise dominate any `for i in 0..xs.len() { … }`
        // shape that doesn't pre-cache the length.
        if name == "gos_rt_vec_len" && args.len() == 1 {
            self.lower_vec_len_inline(&args[0], destination, target)?;
            return Ok(());
        }
        // `.len()` on a Vec/Slice that routed through the generic
        // `gos_rt_len` dispatcher: same null-guarded header load, but
        // only when the static element type pins the receiver as a
        // real word-stride GosVec - never `Vec<String>`, whose
        // `env::args()` sentinel pointer keeps its length in
        // `ARGS_LEN` rather than at `*p`.
        if name == "gos_rt_len" && args.len() == 1 && self.vec_operand_has_word_elem(&args[0]) {
            self.lower_vec_len_inline(&args[0], destination, target)?;
            return Ok(());
        }
        // `s.split(c)` where `c` is a char: the runtime takes `sep: *const c_char`
        // (a C string pointer), but MIR emits the char code as `i32`. Convert
        // via `gos_rt_char_to_str` first, mirroring the Cranelift backend.
        if name == "gos_rt_str_split" && args.len() == 2 {
            if matches!(self.concat_print_kind(&args[1]), ConcatKind::Char) {
                declare_rt(&mut self.runtime_refs, "gos_rt_char_to_str");
                declare_rt(&mut self.runtime_refs, "gos_rt_str_split");
                let s = self.lower_operand(&args[0])?;
                let c_raw = self.lower_operand(&args[1])?;
                let c_widened = self.widen_char_to_i32(&args[1], &c_raw);
                let sep_ptr = self.fresh();
                let tmp = self.fresh();
                let dst = local_slot(destination.local);
                writeln!(
                    self.out,
                    "  {sep_ptr} = call ptr @gos_rt_char_to_str(i32 {c_widened})"
                )
                .unwrap();
                writeln!(
                    self.out,
                    "  {tmp} = call ptr @gos_rt_str_split(ptr {s}, ptr {sep_ptr})"
                )
                .unwrap();
                writeln!(self.out, "  store ptr {tmp}, ptr {dst}").unwrap();
                emit_terminator_branch(&mut self.out, target);
                return Ok(());
            }
        }
        // Heap-Vec inline fast paths. The runtime returns a
        // `*mut GosI64Vec { len: i64, data: *mut i64 }`; user
        // code accesses elements via `vec.set_at(i, v)` and
        // `vec.get_at(i)`. Without inlining each access pays
        // one FFI call (~5-20 ns), which dominates the
        // multi-threaded fasta hot loop. The inline shape
        // skips the runtime's bounds check (caller is
        // expected to keep `i` in range, same convention as
        // `str_byte_at`).
        if name == "gos_rt_heap_i64_set" && args.len() == 3 {
            self.lower_heap_i64_set_inline(args, destination, target)?;
            return Ok(());
        }
        if name == "gos_rt_heap_i64_get" && args.len() == 2 {
            self.lower_heap_i64_get_inline(args, destination, target)?;
            return Ok(());
        }
        // Raw heap intrinsics that the cranelift tier handles
        // inline. Lower them to a direct LLVM `getelementptr +
        // load/store` here so the LLVM tier doesn't bail and
        // route the body to cranelift just for these calls.
        if matches!(
            name.as_str(),
            "gos_load"
                | "gos_store"
                | "gos_alloc"
                | "gos_rc_alloc"
                | "gos_fn_addr"
                | "gos_enum_disc"
                | "gos_enum_set_disc"
        ) {
            self.lower_raw_intrinsic(&name, args, destination, target)?;
            return Ok(());
        }
        // `v.push(x)` on a Vec - the runtime helper takes
        // `(*mut GosVec, *const u8)` and `memcpy`s the value
        // through the second pointer. The MIR routes every
        // element type to the same generic `gos_rt_vec_push`
        // symbol, so we have to spill the i64 / ptr value into
        // an alloca and pass the alloca's address. Without this
        // the LLVM tier passed the i64 value as a pointer and
        // the helper segfaulted dereferencing it (`SEGV_MAPERR`
        // at `si_addr=value`). Mirrors the cranelift-side
        // stack-slot dance in `lower_intrinsic_call`.
        if name == "gos_rt_vec_push" && args.len() == 2 {
            self.lower_vec_push_inline(args, destination, target)?;
            return Ok(());
        }
        // Inline primitive Vec index get/set/get_ptr (lenient bounds: null /
        // out-of-range → 0 / no-op / null, matching the runtime). Removes a
        // per-element FFI call from hot index loops (BFS, scans) and lets
        // LLVM hoist the loop-invariant len/ptr loads.
        // Only inline when the element is a primitive int/bool - exactly the
        // hot index-loop case (queue/visited/scans). A heap-pointer Adt
        // element (e.g. `Vec<DirInfo>`, where `&entries[i]` has
        // reference-through-handle semantics the generic call-result path
        // handles) keeps the runtime call.
        if name == "gos_rt_vec_get_i64"
            && args.len() == 2
            && (is_primitive_int_llvm(&render_ty(self.tcx, self.body.local_ty(destination.local)))
                || self.vec_operand_elem_is_vec(&args[0]))
        {
            self.lower_vec_get_i64_inline(args, destination, target)?;
            return Ok(());
        }
        // The MIR emits this only for the counted-loop element read of a
        // primitive int/bool/char Vec, where the index is proven in
        // `[0, len)` and the receiver non-null. Inline it without the null /
        // bounds guard so the inner loop is a straight load.
        if name == "gos_rt_vec_get_i64_unchecked" && args.len() == 2 {
            self.lower_vec_get_i64_unchecked_inline(args, destination, target)?;
            return Ok(());
        }
        if name == "gos_rt_vec_set_i64"
            && args.len() == 3
            && is_primitive_int_llvm(&self.operand_llvm_ty(&args[2]))
        {
            self.lower_vec_set_i64_inline(args, destination, target)?;
            return Ok(());
        }
        // NOTE: gos_rt_vec_get_ptr is intentionally NOT inlined - its result
        // handling is dest-type-dependent (a multi-slot aggregate dest
        // memcpys from the returned address rather than storing it), which
        // the generic call-result path handles correctly.
        // Variant constructor stubs: `Ok(v)`, `Some(v)`, `Err(e)`
        // pass the wrapped value through unchanged (the compiled
        // tier flattens Option/Result, so `unwrap` is identity).
        // `None` and other no-payload variants resolve to a zero
        // value. Mirrors the Cranelift backend's variant-stub
        // branch so escaped Result/Option values don't end up
        // calling a non-existent `@"Ok"` symbol at link time.
        let is_variant_stub = matches!(name.as_str(), "Ok" | "Some" | "Err" | "None")
            || (name.chars().next().is_some_and(|c| c.is_ascii_uppercase())
                && !name.contains("::"));
        if is_variant_stub {
            self.emit_variant_stub(&name, args, destination, target)?;
            return Ok(());
        }
        // `channel()` / `sync::channel()` - the std prelude shape
        // for unbuffered channels. MIR emits a 0-arg `Call("channel",
        // dest=tuple_local)`; we lower to `gos_rt_chan_new(8, 0)` and
        // mirror the cranelift backend's "write chan_ptr to both
        // tuple slots" trick so `pair.0` and `pair.1` (Sender +
        // Receiver) project to the same handle.
        if matches!(
            name.as_str(),
            "channel"
                | "channel::new"
                | "sync::channel"
                | "sync::Channel::new"
                | "gos_rt_chan_new"
                | "Channel::new"
        ) {
            declare_rt(&mut self.runtime_refs, "gos_rt_chan_new");
            let cap_arg = if let Some(a) = args.first() {
                Some(self.lower_operand(a)?)
            } else {
                None
            };
            let cap = cap_arg.unwrap_or_else(|| "0".to_string());
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = call ptr @gos_rt_chan_new(i32 8, i64 {cap})"
            )
            .unwrap();
            // Materialise a fresh 16-byte tuple buffer so the
            // `(Sender, Receiver)` projections both observe the
            // same channel handle. The destination MIR local may
            // be a single-ptr alloca (typeck doesn't always
            // preserve the tuple shape through the channel-call
            // path), so writing slot 1 into the destination
            // directly would overflow the alloca and clobber
            // adjacent stack memory. Mirrors the Cranelift
            // backend's `create_sized_stack_slot(16, 3)` shape.
            let pair_buf = self.fresh();
            writeln!(self.out, "  {pair_buf} = alloca [2 x i64]").unwrap();
            let slot0 = self.fresh();
            writeln!(
                self.out,
                "  {slot0} = getelementptr i64, ptr {pair_buf}, i64 0"
            )
            .unwrap();
            writeln!(self.out, "  store ptr {tmp}, ptr {slot0}").unwrap();
            let slot1 = self.fresh();
            writeln!(
                self.out,
                "  {slot1} = getelementptr i64, ptr {pair_buf}, i64 1"
            )
            .unwrap();
            writeln!(self.out, "  store ptr {tmp}, ptr {slot1}").unwrap();
            // Store the buffer address into the destination
            // local so downstream `.0` / `.1` projections lower
            // as loads from `pair_buf + N*8`.
            let dest_slot = local_slot(destination.local);
            writeln!(self.out, "  store ptr {pair_buf}, ptr {dest_slot}").unwrap();
            emit_terminator_branch(&mut self.out, target);
            return Ok(());
        }
        // `Vec::new(elem_bytes)` - the runtime helper signature is
        // `gos_rt_vec_new(elem_bytes: u32)`. MIR passes the element
        // width as `i64`, so we truncate to `i32` before the call.
        if matches!(name.as_str(), "Vec::new" | "gos_rt_vec_new") {
            let kind = llvm_vec_elem_kind_from_local(self.body, self.tcx, destination.local);
            let dst = local_slot(destination.local);
            let eb_i64 = if let Some(a) = args.first() {
                self.lower_operand(a)?
            } else {
                "8".to_string()
            };
            let eb_i32 = self.fresh();
            writeln!(self.out, "  {eb_i32} = trunc i64 {eb_i64} to i32").unwrap();
            let tmp = self.fresh();
            if kind == vec_elem_kind_llvm::PRIMITIVE {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_new");
                writeln!(self.out, "  {tmp} = call ptr @gos_rt_vec_new(i32 {eb_i32})").unwrap();
            } else {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_new_typed");
                writeln!(
                    self.out,
                    "  {tmp} = call ptr @gos_rt_vec_new_typed(i32 {eb_i32}, i8 {kind})"
                )
                .unwrap();
            }
            writeln!(self.out, "  store ptr {tmp}, ptr {dst}").unwrap();
            emit_terminator_branch(&mut self.out, target);
            return Ok(());
        }
        // `Vec::with_capacity(elem_bytes, cap)` mirrors `Vec::new` but
        // pre-allocates buffer space.
        if matches!(
            name.as_str(),
            "Vec::with_capacity" | "gos_rt_vec_with_capacity"
        ) {
            let kind = llvm_vec_elem_kind_from_local(self.body, self.tcx, destination.local);
            let dst = local_slot(destination.local);
            let eb_i64 = if let Some(a) = args.first() {
                self.lower_operand(a)?
            } else {
                "8".to_string()
            };
            let cap_i64 = if let Some(a) = args.get(1) {
                self.lower_operand(a)?
            } else {
                "0".to_string()
            };
            let eb_i32 = self.fresh();
            writeln!(self.out, "  {eb_i32} = trunc i64 {eb_i64} to i32").unwrap();
            let tmp = self.fresh();
            if kind == vec_elem_kind_llvm::PRIMITIVE {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_with_capacity");
                writeln!(
                    self.out,
                    "  {tmp} = call ptr @gos_rt_vec_with_capacity(i32 {eb_i32}, i64 {cap_i64})"
                )
                .unwrap();
            } else {
                declare_rt(&mut self.runtime_refs, "gos_rt_vec_with_capacity_typed");
                writeln!(
                    self.out,
                    "  {tmp} = call ptr @gos_rt_vec_with_capacity_typed(i32 {eb_i32}, i64 {cap_i64}, i8 {kind})"
                )
                .unwrap();
            }
            writeln!(self.out, "  store ptr {tmp}, ptr {dst}").unwrap();
            emit_terminator_branch(&mut self.out, target);
            return Ok(());
        }
        // HashMap / collection constructors: MIR emits a 0-arg call but the
        // runtime function takes (key_bytes: i32, val_bytes: i32). Mirror the
        // Cranelift backend's hardcoded 8/8 (all GC-managed values are
        // pointer-sized, so 8 bytes covers every key/value type).
        if matches!(
            name.as_str(),
            "HashMap::new" | "collections::HashMap::new" | "gos_rt_map_new"
        ) {
            declare_rt(&mut self.runtime_refs, "gos_rt_map_new");
            let dst = local_slot(destination.local);
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = call ptr @gos_rt_map_new(i32 8, i32 8)").unwrap();
            writeln!(self.out, "  store ptr {tmp}, ptr {dst}").unwrap();
            if let Some(tgt) = target {
                writeln!(self.out, "  br label %bb{}", tgt.as_u32()).unwrap();
            }
            return Ok(());
        }
        if matches!(
            name.as_str(),
            "HashMap::with_capacity"
                | "collections::HashMap::with_capacity"
                | "gos_rt_map_new_with_capacity"
        ) {
            declare_rt(&mut self.runtime_refs, "gos_rt_map_new_with_capacity");
            let dst = local_slot(destination.local);
            let cap = if let Some(a) = args.first() {
                self.lower_operand(a)?
            } else {
                "0".to_string()
            };
            let tmp = self.fresh();
            writeln!(
                self.out,
                "  {tmp} = call ptr @gos_rt_map_new_with_capacity(i32 8, i32 8, i64 {cap})"
            )
            .unwrap();
            writeln!(self.out, "  store ptr {tmp}, ptr {dst}").unwrap();
            if let Some(tgt) = target {
                writeln!(self.out, "  br label %bb{}", tgt.as_u32()).unwrap();
            }
            return Ok(());
        }
        let symbol = map_prelude_symbol(&name);
        self.emit_named_call(symbol, args, destination, target)
    }

    /// Reads the heap pointer value addressed by an operand
    /// passed as the `ptr` argument of `gos_load`/`gos_store`.
    /// Differs from the generic `lower_operand` because
    /// aggregate-typed locals (Adt / Tuple / Array) are stored
    /// as a heap pointer in their slot - `lower_place_read`
    /// returns the slot's *address* for those, but the raw
    /// heap intrinsics need the *contents* (the heap pointer
    /// the slot stores). Without this distinction the
    /// `getelementptr` walks from the local's stack alloca
    /// instead of the heap blob, corrupting the slot.
    pub(crate) fn lower_raw_ptr_arg(&mut self, op: &Operand) -> Result<String, BuildError> {
        if let Operand::Copy(place) = op
            && place.projection.is_empty()
            && is_aggregate(self.tcx, self.body.local_ty(place.local))
        {
            let slot = local_slot(place.local);
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {slot}").unwrap();
            return Ok(tmp);
        }
        let raw = self.lower_operand(op)?;
        let raw_ty = self.operand_llvm_ty(op);
        if raw_ty == "ptr" {
            return Ok(raw);
        }
        let tmp = self.fresh();
        writeln!(self.out, "  {tmp} = inttoptr {raw_ty} {raw} to ptr").unwrap();
        Ok(tmp)
    }

    /// Lowers an argument operand for a regular fn call.
    ///
    /// Three storage shapes co-exist:
    ///
    /// 1. Callee expects `Ref<T>`, caller has an enum-Adt local
    ///    (slot_count = None): the slot holds a GC heap pointer.
    ///    Load it and pass it directly so the callee GEPs into the
    ///    heap data without an extra indirection.
    ///
    /// 2. Callee expects an aggregate parameter (Array, Tuple, or
    ///    any Adt): pass the alloca ADDRESS. `emit_param_stores`
    ///    in the callee does `memcpy(callee_slot, arg_ptr, bytes)`.
    ///    For enum-Adt locals the alloca holds the heap pointer (8
    ///    bytes), so the memcpy correctly copies the pointer; for
    ///    inline structs it copies the actual field data. Without
    ///    this bypass, `lower_place_read`'s enum-Adt load would
    ///    return the heap pointer value and the callee would
    ///    memcpy from the Cons-node data instead of the pointer.
    ///
    /// 3. All other args: `lower_operand` → `lower_place_read`.
    pub(crate) fn lower_call_arg(
        &mut self,
        op: &Operand,
        expected: Option<Ty>,
    ) -> Result<(String, String), BuildError> {
        if let Some(want) = expected
            && let Operand::Copy(place) = op
            && place.projection.is_empty()
        {
            let local_ty = self.body.local_ty(place.local);
            if matches!(self.tcx.kind(want), Some(TyKind::Ref { .. })) {
                // Case 1: &T param, enum-Adt slot → load heap ptr.
                if matches!(self.tcx.kind(local_ty), Some(TyKind::Adt { .. }))
                    && slot_count(self.tcx, local_ty).is_none()
                {
                    let slot = local_slot(place.local);
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = load ptr, ptr {slot}").unwrap();
                    return Ok((tmp, "ptr".to_string()));
                }
            } else if is_aggregate(self.tcx, want) && is_aggregate(self.tcx, local_ty) {
                // Case 2: aggregate param → pass alloca address so
                // the callee's memcpy copies the right bytes.
                return Ok((local_slot(place.local), "ptr".to_string()));
            } else if is_aggregate(self.tcx, want)
                && slot_count(self.tcx, want).is_none_or(|n| n == 1)
                && matches!(
                    self.tcx.kind(local_ty),
                    Some(TyKind::Var(_) | TyKind::Error | TyKind::Int(_)) | None
                )
            {
                // Case 2b: the callee declares a one-slot aggregate
                // param (a tagged-pointer / bare-discriminant enum),
                // but this call site's arg local is a one-word scalar
                // - either inference left it untyped (method-call arg
                // temporaries) or a unit-only enum lowered its value
                // to a bare `i64` discriminant. The callee memcpys 8
                // bytes from the arg address, so pass the slot address;
                // passing the loaded VALUE instead makes the callee
                // dereference the bits (read garbage or fault).
                // Bounded to one slot (a tagged heap enum's
                // `slot_count` is `None`, its storage one word; an
                // `i64` slot is exactly one word) so the callee can
                // never overread.
                return Ok((local_slot(place.local), "ptr".to_string()));
            }
        }
        let v = self.lower_operand(op)?;
        let ty = self.operand_llvm_ty(op);
        Ok((v, ty))
    }

    /// Reads the value being stored by a raw heap intrinsic
    /// (`gos_store`'s third arg). Aggregate-typed locals hold
    /// the heap pointer in their slot - when one flows in as
    /// the value, return the *contents* (load ptr from slot)
    /// instead of the slot address. Returns `(value, llvm_ty)`.
    pub(crate) fn lower_raw_value_arg(
        &mut self,
        op: &Operand,
    ) -> Result<(String, String), BuildError> {
        if let Operand::Copy(place) = op
            && place.projection.is_empty()
            && is_aggregate(self.tcx, self.body.local_ty(place.local))
        {
            let slot = local_slot(place.local);
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = load ptr, ptr {slot}").unwrap();
            return Ok((tmp, "ptr".to_string()));
        }
        let v = self.lower_operand(op)?;
        let ty = self.operand_llvm_ty(op);
        Ok((v, ty))
    }

    /// Lowers the raw-pointer intrinsics (`gos_load`,
    /// `gos_store`, `gos_alloc`, `gos_fn_addr`) directly to
    /// LLVM IR so the LLVM tier doesn't have to fall back to
    /// cranelift for closure envs / vec iteration / fn-pointer
    /// trampolines. Mirrors the cranelift-side handlers in
    /// `lower_intrinsic_outcome`.
    pub(crate) fn lower_raw_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        destination: &Place,
        target: Option<&gossamer_mir::BlockId>,
    ) -> Result<(), BuildError> {
        let dest_ty_mir = self.body.local_ty(destination.local);
        let dest_ty = render_ty(self.tcx, dest_ty_mir);
        match name {
            "gos_enum_load" => {
                // gos_enum_load(ptr, off) -> i64 at (ptr & !7) + off.
                // Enum payload read: the mask strips a tagged repr's disc
                // bits and is a no-op for aligned header-repr pointers.
                if args.len() < 2 {
                    return Err(BuildError::Unsupported("gos_enum_load arity"));
                }
                let pv = self.lower_operand(&args[0])?;
                let p_ty = self.operand_llvm_ty(&args[0]);
                let p64 = self.coerce_llvm_value(&pv, &p_ty, "i64");
                let off_v = self.lower_operand(&args[1])?;
                let off_ty = self.operand_llvm_ty(&args[1]);
                let off64 = self.coerce_llvm_value(&off_v, &off_ty, "i64");
                let m = self.fresh();
                writeln!(self.out, "  {m} = and i64 {p64}, -8").unwrap();
                // Unit variants of tagged enums are TAGGED NULLS (base
                // zero, no object): a payload load on one yields 0
                // instead of dereferencing address zero. Perfectly
                // predicted on payload-bearing values.
                let is_null = self.fresh();
                writeln!(self.out, "  {is_null} = icmp eq i64 {m}, 0").unwrap();
                let entry_l = self.fresh_label("enum_load_entry");
                let load_l = self.fresh_label("enum_load");
                let done_l = self.fresh_label("enum_load_done");
                writeln!(self.out, "  br label %{entry_l}").unwrap();
                writeln!(self.out, "{entry_l}:").unwrap();
                writeln!(
                    self.out,
                    "  br i1 {is_null}, label %{done_l}, label %{load_l}"
                )
                .unwrap();
                writeln!(self.out, "{load_l}:").unwrap();
                let mp = self.fresh();
                writeln!(self.out, "  {mp} = inttoptr i64 {m} to ptr").unwrap();
                let addr = self.fresh();
                writeln!(
                    self.out,
                    "  {addr} = getelementptr i8, ptr {mp}, i64 {off64}"
                )
                .unwrap();
                let lv = self.fresh();
                writeln!(self.out, "  {lv} = load i64, ptr {addr}").unwrap();
                writeln!(self.out, "  br label %{done_l}").unwrap();
                writeln!(self.out, "{done_l}:").unwrap();
                let v = self.fresh();
                writeln!(
                    self.out,
                    "  {v} = phi i64 [ 0, %{entry_l} ], [ {lv}, %{load_l} ]"
                )
                .unwrap();
                // Bit-preserving recovery, exactly as `gos_load`: float
                // payloads were stored as raw bits and must bitcast back.
                let coerced = if dest_ty == "double" || dest_ty == "float" {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = bitcast i64 {v} to {dest_ty}").unwrap();
                    tmp
                } else {
                    self.coerce_llvm_value(&v, "i64", &dest_ty)
                };
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_enum_tag" => {
                // gos_enum_tag(ptr, disc) -> ptr | (disc << 1). Tagged-repr
                // enums (<= 4 variants) carry the discriminant in pointer
                // bits 1-2; bit 0 stays 0 (odd pointers are string bodies).
                if args.len() < 2 {
                    return Err(BuildError::Unsupported("gos_enum_tag arity"));
                }
                let pv = self.lower_operand(&args[0])?;
                let p_ty = self.operand_llvm_ty(&args[0]);
                let p64 = self.coerce_llvm_value(&pv, &p_ty, "i64");
                let d = self.lower_operand(&args[1])?;
                let d_ty = self.operand_llvm_ty(&args[1]);
                let d64 = self.coerce_llvm_value(&d, &d_ty, "i64");
                let sh = self.fresh();
                writeln!(self.out, "  {sh} = shl i64 {d64}, 1").unwrap();
                let or = self.fresh();
                writeln!(self.out, "  {or} = or i64 {p64}, {sh}").unwrap();
                let coerced = self.coerce_llvm_value(&or, "i64", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_enum_disc_tag" => {
                // gos_enum_disc_tag(ptr) -> (ptr >> 1) & 3.
                if args.is_empty() {
                    return Err(BuildError::Unsupported("gos_enum_disc_tag arity"));
                }
                let pv = self.lower_operand(&args[0])?;
                let p_ty = self.operand_llvm_ty(&args[0]);
                let p64 = self.coerce_llvm_value(&pv, &p_ty, "i64");
                let sh = self.fresh();
                writeln!(self.out, "  {sh} = lshr i64 {p64}, 1").unwrap();
                let m = self.fresh();
                writeln!(self.out, "  {m} = and i64 {sh}, 3").unwrap();
                let coerced = self.coerce_llvm_value(&m, "i64", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_enum_untag" => {
                // gos_enum_untag(ptr) -> ptr & !7 (payload base).
                if args.is_empty() {
                    return Err(BuildError::Unsupported("gos_enum_untag arity"));
                }
                let pv = self.lower_operand(&args[0])?;
                let p_ty = self.operand_llvm_ty(&args[0]);
                let p64 = self.coerce_llvm_value(&pv, &p_ty, "i64");
                let m = self.fresh();
                writeln!(self.out, "  {m} = and i64 {p64}, -8").unwrap();
                let coerced = self.coerce_llvm_value(&m, "i64", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_enum_disc" => {
                // gos_enum_disc(payload_ptr) -> i64. The discriminant is
                // the byte at payload-3 (inside the RC header: strong u32,
                // weak u8, disc u8, meta_id u16).
                if args.is_empty() {
                    return Err(BuildError::Unsupported("gos_enum_disc arity"));
                }
                let p = self.lower_raw_ptr_arg(&args[0])?;
                let addr = self.fresh();
                writeln!(self.out, "  {addr} = getelementptr i8, ptr {p}, i64 -3").unwrap();
                let b = self.fresh();
                writeln!(self.out, "  {b} = load i8, ptr {addr}").unwrap();
                let v = self.fresh();
                writeln!(self.out, "  {v} = zext i8 {b} to i64").unwrap();
                let coerced = self.coerce_llvm_value(&v, "i64", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_enum_set_disc" => {
                // gos_enum_set_disc(payload_ptr, disc_i64): store the low
                // byte of the discriminant at payload-3.
                if args.len() < 2 {
                    return Err(BuildError::Unsupported("gos_enum_set_disc arity"));
                }
                let p = self.lower_raw_ptr_arg(&args[0])?;
                let d = self.lower_operand(&args[1])?;
                let d_ty = self.operand_llvm_ty(&args[1]);
                let d64 = self.coerce_llvm_value(&d, &d_ty, "i64");
                let b = self.fresh();
                writeln!(self.out, "  {b} = trunc i64 {d64} to i8").unwrap();
                let addr = self.fresh();
                writeln!(self.out, "  {addr} = getelementptr i8, ptr {p}, i64 -3").unwrap();
                writeln!(self.out, "  store i8 {b}, ptr {addr}").unwrap();
            }
            "gos_load" => {
                // gos_load(ptr_i64, offset_i64) -> i64
                if args.len() < 2 {
                    return Err(BuildError::Unsupported("gos_load arity"));
                }
                let p = self.lower_raw_ptr_arg(&args[0])?;
                let off_v = self.lower_operand(&args[1])?;
                let off_ty = self.operand_llvm_ty(&args[1]);
                // gep i8, p, off → addr
                // `sext` is integer-only; use the type-aware
                // coercion so a `double` offset (closure capture
                // routed through gos_load) converts via `fptosi`
                // rather than the malformed `sext double to i64`
                // shape that `opt`'s verifier rejects.
                let off64 = self.coerce_llvm_value(&off_v, &off_ty, "i64");
                let addr = self.fresh();
                writeln!(
                    self.out,
                    "  {addr} = getelementptr i8, ptr {p}, i64 {off64}"
                )
                .unwrap();
                if crate::emit::want_race_instrumentation() {
                    declare_rt(&mut self.runtime_refs, "gos_rt_race_access");
                    let addr_int = self.fresh();
                    writeln!(self.out, "  {addr_int} = ptrtoint ptr {addr} to i64").unwrap();
                    writeln!(
                        self.out,
                        "  call void @gos_rt_race_access(i64 {addr_int}, i32 0)"
                    )
                    .unwrap();
                }
                let loaded = self.fresh();
                writeln!(self.out, "  {loaded} = load i64, ptr {addr}").unwrap();
                // Heap-load value recovery is bit-preserving:
                // a `double` capture stored via `gos_store` (which
                // bitcasts the float bits into the i64 slot)
                // must be read back via `bitcast i64 to double`,
                // not `sitofp` (which would interpret the bits
                // as an integer value). Mirrors the gos_store
                // path above.
                let coerced = if dest_ty == "double" || dest_ty == "float" {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = bitcast i64 {loaded} to {dest_ty}").unwrap();
                    tmp
                } else {
                    self.coerce_llvm_value(&loaded, "i64", &dest_ty)
                };
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_store" => {
                // gos_store(ptr, offset, value) - writes 8 bytes.
                if args.len() < 3 {
                    return Err(BuildError::Unsupported("gos_store arity"));
                }
                let p = self.lower_raw_ptr_arg(&args[0])?;
                let off_v = self.lower_operand(&args[1])?;
                let off_ty = self.operand_llvm_ty(&args[1]);
                // The value being stored may itself be an
                // aggregate-typed Copy whose slot holds the
                // heap pointer (the recursive-enum case where
                // a `Box<List>` rest field lives in another
                // local's slot). Use the same heap-pointer
                // resolution as the ptr arg so we store the
                // heap pointer rather than the slot address.
                let (val_v, val_ty) = self.lower_raw_value_arg(&args[2])?;
                // `sext` is integer-only; use the type-aware
                // coercion so a `double` offset (closure capture
                // routed through gos_load) converts via `fptosi`
                // rather than the malformed `sext double to i64`
                // shape that `opt`'s verifier rejects.
                let off64 = self.coerce_llvm_value(&off_v, &off_ty, "i64");
                // Heap-store value-coercion is bit-preserving:
                // a `double` capture stored into an `i64` heap
                // slot must keep its IEEE-754 bit pattern intact
                // (so the matching `gos_load` can read it back
                // via `bitcast i64 to double`). `coerce_llvm_value`
                // uses `fptosi` for value-semantic conversions -
                // wrong here: `fptosi(0.5)` is `0`, losing the
                // capture. Emit `bitcast` explicitly for the
                // float-to-i64 store path.
                let val64 = if val_ty == "double" || val_ty == "float" {
                    let tmp = self.fresh();
                    writeln!(self.out, "  {tmp} = bitcast {val_ty} {val_v} to i64").unwrap();
                    tmp
                } else {
                    self.coerce_llvm_value(&val_v, &val_ty, "i64")
                };
                let addr = self.fresh();
                writeln!(
                    self.out,
                    "  {addr} = getelementptr i8, ptr {p}, i64 {off64}"
                )
                .unwrap();
                if crate::emit::want_race_instrumentation() {
                    declare_rt(&mut self.runtime_refs, "gos_rt_race_access");
                    let addr_int = self.fresh();
                    writeln!(self.out, "  {addr_int} = ptrtoint ptr {addr} to i64").unwrap();
                    writeln!(
                        self.out,
                        "  call void @gos_rt_race_access(i64 {addr_int}, i32 1)"
                    )
                    .unwrap();
                }
                writeln!(self.out, "  store i64 {val64}, ptr {addr}").unwrap();
                if dest_ty != "void" && !is_unit(self.tcx, dest_ty_mir) {
                    let slot = local_slot(destination.local);
                    // Sink stores: pick a zero-shaped literal that
                    // matches the destination's LLVM type so `opt`
                    // doesn't reject `store ptr 0` / `store double 0`.
                    let zero = match dest_ty.as_str() {
                        "ptr" => "null".to_string(),
                        "double" | "float" => "0.0".to_string(),
                        _ => "0".to_string(),
                    };
                    writeln!(self.out, "  store {dest_ty} {zero}, ptr {slot}").unwrap();
                }
            }
            "gos_alloc" => {
                // gos_alloc(size_i64) -> ptr (via libc malloc).
                let size_v = if args.is_empty() {
                    "0".to_string()
                } else {
                    let v = self.lower_operand(&args[0])?;
                    let t = self.operand_llvm_ty(&args[0]);
                    if t == "i64" {
                        v
                    } else {
                        let tmp = self.fresh();
                        writeln!(self.out, "  {tmp} = sext {t} {v} to i64").unwrap();
                        tmp
                    }
                };
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = call ptr @malloc(i64 {size_v})").unwrap();
                let coerced = self.coerce_llvm_value(&tmp, "ptr", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_rc_alloc" | "gos_rc_alloc_tagged" => {
                // gos_rc_alloc(size_i64, meta_symbol) -> ptr to a
                // reference-counted heap object (strong count 1). The
                // second arg is a const-string naming the module-global
                // child-layout meta blob; an empty name means a leaf
                // (null meta). The blob globals are emitted alongside
                // the string pool (see emit.rs).
                let size_v = if args.is_empty() {
                    "0".to_string()
                } else {
                    let v = self.lower_operand(&args[0])?;
                    let t = self.operand_llvm_ty(&args[0]);
                    if t == "i64" {
                        v
                    } else {
                        let tmp = self.fresh();
                        writeln!(self.out, "  {tmp} = sext {t} {v} to i64").unwrap();
                        tmp
                    }
                };
                let meta_ptr = match args.get(1) {
                    Some(Operand::Const(ConstValue::Str(sym))) if !sym.is_empty() => {
                        format!("@\"{sym}\"")
                    }
                    _ => "null".to_string(),
                };
                let rt_name = if name == "gos_rc_alloc_tagged" {
                    "gos_rt_rc_alloc_tagged"
                } else {
                    "gos_rt_rc_alloc"
                };
                declare_rt(&mut self.runtime_refs, rt_name);
                let tmp = self.fresh();
                writeln!(
                    self.out,
                    "  {tmp} = call ptr @{rt_name}(i64 {size_v}, ptr {meta_ptr})"
                )
                .unwrap();
                let coerced = self.coerce_llvm_value(&tmp, "ptr", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            "gos_fn_addr" => {
                // gos_fn_addr("name") -> ptr to that function.
                let Some(Operand::Const(ConstValue::Str(fname))) = args.first() else {
                    return Err(BuildError::Unsupported("gos_fn_addr arg"));
                };
                // LLVM IR pointer-to-function constants are written
                // as the function symbol itself; declare-only is OK
                // because the cranelift companion (or another LLVM
                // body) provides the definition. When `fname` is a
                // runtime symbol (`gos_rt_*`), ensure the matching
                // `declare` lands in the module - otherwise opt
                // rejects `bitcast ptr @gos_rt_<name>` with "use of
                // undefined value".
                if fname.starts_with("gos_rt_") && gossamer_abi::lookup(fname).is_some() {
                    declare_rt(&mut self.runtime_refs, fname);
                }
                // Win64: a handler invoked by the rustc-compiled runtime
                // through `extern "C" fn(..) -> i128` must return the
                // 2-word `i128` in a vector register (xmm0), but `@fname`
                // (a gossamer `ret i128`) returns it in the GP-register
                // pair. Take the address of the synthesized `<16 x i8>`
                // C-ABI return thunk (`name$cabi`) instead so the runtime
                // reads the discriminant/payload from the register it
                // expects. `cabi_handlers` is empty off Windows, so this
                // is a no-op there and for non-handler fn-addresses.
                let sym = if self.cabi_handlers.contains_key(fname.as_str()) {
                    format!("{fname}$cabi")
                } else {
                    fname.clone()
                };
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = bitcast ptr @\"{sym}\" to ptr").unwrap();
                let coerced = self.coerce_llvm_value(&tmp, "ptr", &dest_ty);
                let slot = local_slot(destination.local);
                writeln!(self.out, "  store {dest_ty} {coerced}, ptr {slot}").unwrap();
            }
            _ => {
                return Err(BuildError::Unsupported("unrecognised raw intrinsic"));
            }
        }
        // The Terminator::Call caller passes a target and expects
        // the branch to fall through to the next block; the
        // Rvalue-in-Assign caller passes `None` (it sits inside a
        // basic block before the terminator) and skips the branch.
        if target.is_some() {
            emit_terminator_branch(&mut self.out, target);
        }
        Ok(())
    }
}

/// True for LLVM integer/bool scalar types - the safe element kinds for the
/// inline Vec get/set fast path (the loaded i64 maps cleanly to these).
fn is_primitive_int_llvm(ty: &str) -> bool {
    matches!(ty, "i64" | "i32" | "i16" | "i8" | "i1")
}
