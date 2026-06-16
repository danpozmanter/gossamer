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
    pub(crate) fn lower_rvalue(
        &mut self,
        rvalue: &Rvalue,
        dest_local: Local,
    ) -> Result<String, BuildError> {
        match rvalue {
            Rvalue::Use(op) => self.lower_operand(op),
            Rvalue::UnaryOp { op, operand } => self.lower_unary(*op, operand, dest_local),
            Rvalue::BinaryOp { op, lhs, rhs } => self.lower_binary(*op, lhs, rhs, dest_local),
            Rvalue::Cast { operand, target } => self.lower_cast(operand, *target, dest_local),
            Rvalue::CallIntrinsic { name, args } => {
                self.lower_call_intrinsic(name, args, dest_local)
            }
            Rvalue::Ref { place, .. } => {
                // `&place` — we return the address of the
                // projection walk (or the bare stack slot when
                // there's no projection). In Gossamer's
                // runtime shape references are just raw
                // pointers, so the store at the caller simply
                // takes the address value as `ptr`.
                if place.projection.is_empty() {
                    Ok(local_slot(place.local))
                } else {
                    Ok(self.lower_place_address(place))
                }
            }
            Rvalue::Len(place) => {
                // `Rvalue::Len` reports the length of a
                // runtime-managed sequence. Stack-allocated
                // arrays have static lengths the compiler
                // folds from the type; heap-backed
                // `Vec`/`Slice`/`String` values go through
                // `gos_rt_len`.
                let ty = self.place_leaf_ty(place);
                if let Some(TyKind::Array { len, .. }) = self.tcx.kind(ty) {
                    return Ok(format!("{len}"));
                }
                // A bare local of Vec/Slice type with a non-String
                // element reads its length straight from the leading
                // i64 of the GosVec header (NULL -> 0), which removes
                // a per-iteration FFI call from every
                // `while i < xs.len()` loop. `Vec<String>` stays on
                // `gos_rt_len`: `env::args()` hands out a sentinel
                // pointer whose length lives in `ARGS_LEN`, not at
                // `*p`. Projected places keep the call as well.
                if place.projection.is_empty() {
                    let mut peeled = ty;
                    while let Some(TyKind::Ref { inner, .. }) = self.tcx.kind(peeled) {
                        peeled = *inner;
                    }
                    let elem = match self.tcx.kind(peeled) {
                        Some(TyKind::Vec(e) | TyKind::Slice(e)) => Some(*e),
                        _ => None,
                    };
                    let inlineable =
                        elem.is_some_and(|e| !matches!(self.tcx.kind(e), Some(TyKind::String)));
                    if inlineable {
                        let ptr = self.fresh();
                        writeln!(
                            self.out,
                            "  {ptr} = load ptr, ptr {slot}",
                            slot = local_slot(place.local),
                        )
                        .unwrap();
                        let s = self.next_ssa;
                        self.next_ssa += 1;
                        let (lz, ll, lc) = (
                            format!("len_z_{s}"),
                            format!("len_l_{s}"),
                            format!("len_c_{s}"),
                        );
                        let isnull = self.fresh();
                        writeln!(self.out, "  {isnull} = icmp eq ptr {ptr}, null").unwrap();
                        writeln!(self.out, "  br i1 {isnull}, label %{lz}, label %{ll}").unwrap();
                        writeln!(self.out, "{ll}:").unwrap();
                        let n = self.fresh();
                        writeln!(self.out, "  {n} = load i64, ptr {ptr}").unwrap();
                        writeln!(self.out, "  br label %{lc}").unwrap();
                        writeln!(self.out, "{lz}:").unwrap();
                        writeln!(self.out, "  br label %{lc}").unwrap();
                        writeln!(self.out, "{lc}:").unwrap();
                        let res = self.fresh();
                        writeln!(self.out, "  {res} = phi i64 [ {n}, %{ll} ], [ 0, %{lz} ]")
                            .unwrap();
                        return Ok(res);
                    }
                }
                // For heap-backed shapes the operand is the
                // opaque pointer; call the runtime.
                declare_rt(&mut self.runtime_refs, "gos_rt_len");
                let ptr = if place.projection.is_empty() {
                    let tmp = self.fresh();
                    writeln!(
                        self.out,
                        "  {tmp} = load ptr, ptr {slot}",
                        slot = local_slot(place.local),
                    )
                    .unwrap();
                    tmp
                } else {
                    self.lower_place_address(place)
                };
                let tmp = self.fresh();
                writeln!(self.out, "  {tmp} = call i64 @gos_rt_len(ptr {ptr})").unwrap();
                Ok(tmp)
            }
            Rvalue::Aggregate { .. } | Rvalue::Repeat { .. } => {
                // These rvalues are only legal on the right-hand
                // side of an `Assign` statement, and
                // `lower_assign` routes them directly to the
                // dedicated in-place aggregate store. Reaching
                // them here means the MIR used them as an
                // operand, which the MVP doesn't cover.
                Err(BuildError::Unsupported(
                    "Aggregate / Repeat as non-assignment rvalue",
                ))
            }
            Rvalue::StaticLoad(sref) => {
                let llvm_ty = render_ty(self.tcx, sref.ty);
                self.register_static_global(sref, &llvm_ty);
                let tmp = self.fresh();
                writeln!(
                    self.out,
                    "  {tmp} = load {llvm_ty}, ptr @{sym}",
                    sym = sref.symbol,
                )
                .unwrap();
                Ok(tmp)
            }
        }
    }

    /// Registers the backing module global for a `static mut`. The
    /// definition uses `linkonce_odr` linkage so the same global emitted
    /// from every object that references the static coalesces to one
    /// shared cell at link time. `runtime_refs` is a `BTreeSet`, so the
    /// duplicate definitions a single module emits dedup to one line.
    pub(crate) fn register_static_global(&mut self, sref: &gossamer_mir::StaticRef, llvm_ty: &str) {
        let init = render_const(&sref.init);
        self.runtime_refs.insert(format!(
            "@{sym} = linkonce_odr global {llvm_ty} {init}",
            sym = sref.symbol,
        ));
    }

    /// MIR's `CallIntrinsic` is used for stdlib math and
    /// conversion calls the lowerer wants inline (no separate
    /// Call terminator). The MVP covers the single-argument
    /// f64 functions the nbody-shape programs call through
    /// `std::math::sqrt` etc. — each maps to an LLVM intrinsic
    /// (`llvm.sqrt.f64`, `llvm.sin.f64`, …) which `llc -O3`
    /// lowers to the matching SSE/AVX instruction.
    /// Generic `gos_rt_*` runtime-call intrinsic in `Rvalue`
    /// position. Mirrors the Cranelift backend's behaviour:
    /// emit a typed `call` against the named runtime symbol,
    /// returning the result as the destination local's value.
    pub(crate) fn lower_runtime_call_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let dest_ty = render_ty(self.tcx, self.body.local_ty(dest_local));
        // Pull canonical parameter types from the runtime registry
        // when the symbol is registered. This sidesteps two related
        // miscompiles: (a) a Unit / `void` operand becoming a `void`
        // call-site argument (LLVM rejects this), and (b) two
        // distinct calls for the same symbol producing two divergent
        // `declare` lines (LLVM rejects redefinition).
        let registry_param_llvm: Vec<String> = gossamer_abi::lookup(name)
            .map(|e| {
                e.sig
                    .params
                    .iter()
                    .map(|p| p.llvm_ir().to_string())
                    .collect()
            })
            .unwrap_or_default();
        // `gos_rt_chan_send` / `gos_rt_chan_try_send` expect their
        // second argument to be `*const u8` — a pointer to a
        // memory slot holding the value bytes (the runtime
        // memcpys `chan.elem_bytes` from there). A naive
        // `inttoptr i64 N to ptr` produces a wild pointer that
        // segfaults inside `push_back`. Stack-spill the value
        // and pass the slot address, matching the Cranelift
        // backend.
        let chan_send_spill = matches!(name, "gos_rt_chan_send" | "gos_rt_chan_try_send");
        // `gos_rt_result_new(disc, payload)` stores `payload` as an
        // i64. For aggregate payloads (struct literals, tuples,
        // arrays built on this function's stack), the operand's
        // value in the flat-slot ABI is its stack alloca address —
        // which becomes dangling the moment the caller's frame
        // pops. Heap-copy the aggregate before passing so the
        // pointer outlives the function return.
        let result_new_heap_copy = matches!(name, "gos_rt_result_new");
        // HashMap insert with struct value — same rationale as
        // `gos_rt_result_new`: the value lives on the inserting
        // frame's stack and goes dangling once that frame returns.
        let map_insert_heap_copy = matches!(
            name,
            "gos_rt_map_insert_i64_i64" | "gos_rt_map_insert_str_i64"
        );
        let mut arg_text = String::new();
        for (i, arg) in args.iter().enumerate() {
            if i > 0 {
                arg_text.push_str(", ");
            }
            let a_ty = self.operand_llvm_ty(arg);
            let a_v = self.lower_operand(arg)?;
            if result_new_heap_copy
                && i == 1
                && let Some(heap_v) = self
                    .maybe_heap_copy_value_enum(arg)
                    .or_else(|| self.maybe_heap_copy_aggregate(arg))
            {
                let _ = write!(arg_text, "i64 {heap_v}");
                continue;
            }
            if map_insert_heap_copy
                && i == 2
                && let Some(heap_v) = self.maybe_heap_copy_aggregate(arg)
            {
                let _ = write!(arg_text, "i64 {heap_v}");
                continue;
            }
            if chan_send_spill && i == 1 {
                // Spill the value into a fresh 8-byte stack slot
                // (the channel element width is at most one
                // word in the current runtime ABI) and pass the
                // slot address. The `a_ty` could be ptr / i64 /
                // double / i32 / i1; widen scalars to i64 so the
                // slot's 8 bytes are fully initialised.
                let slot = self.fresh();
                writeln!(self.out, "  {slot} = alloca i64").unwrap();
                let stored_ty;
                let stored_val;
                if a_ty == "ptr" {
                    stored_ty = "ptr".to_string();
                    stored_val = a_v.clone();
                } else if a_ty == "double" || a_ty == "float" {
                    stored_ty = "double".to_string();
                    stored_val = a_v.clone();
                } else if a_ty.starts_with('i') && a_ty != "i64" {
                    let widened = self.fresh();
                    writeln!(self.out, "  {widened} = zext {a_ty} {a_v} to i64").unwrap();
                    stored_ty = "i64".to_string();
                    stored_val = widened;
                } else {
                    stored_ty = "i64".to_string();
                    stored_val = a_v.clone();
                }
                writeln!(self.out, "  store {stored_ty} {stored_val}, ptr {slot}").unwrap();
                let _ = write!(arg_text, "ptr {slot}");
                continue;
            }
            if let Some(want_ty) = registry_param_llvm.get(i) {
                // Win64: a 2-word `i128` (by-value Result/Option) crosses
                // the `extern "C"` boundary by pointer, not in a register
                // pair — that is how rustc lowers an `i128` parameter on
                // `x86_64-pc-windows`. Spill the value into a 16-byte-aligned
                // slot and pass its address, matching the runtime's ABI. On
                // SysV (Linux/macOS) `i128` is register-passed identically by
                // llc and rustc, so this branch is skipped there.
                if crate::emit::target_is_windows() && want_ty == "i128" {
                    let v = if a_ty == "i128" {
                        a_v.clone()
                    } else {
                        self.coerce_llvm_value(&a_v, &a_ty, "i128")
                    };
                    let fat = self.fat_i128_call_arg(&v);
                    let _ = write!(arg_text, "{fat}");
                    continue;
                }
                if a_ty == "void" || a_ty.is_empty() {
                    let zero = match want_ty.as_str() {
                        "ptr" => "null".to_string(),
                        "double" => "0.0".to_string(),
                        _ => "0".to_string(),
                    };
                    let _ = write!(arg_text, "{want_ty} {zero}");
                    continue;
                }
                if &a_ty != want_ty {
                    let coerced = self.coerce_llvm_value(&a_v, &a_ty, want_ty);
                    let _ = write!(arg_text, "{want_ty} {coerced}");
                    continue;
                }
            }
            let _ = write!(arg_text, "{a_ty} {a_v}");
        }
        // Build a `declare` stub matching the call's actual
        // shape so the module header carries a single coherent
        // declaration. The runtime fn's signature is whatever
        // we just emitted at the call site — record it here.
        // Prefer the registry signature when available so two
        // different call sites for the same symbol always agree.
        let mut decl_args = String::new();
        if !registry_param_llvm.is_empty() {
            // Win64: declare i128 (Fat) params as `ptr` so the declaration
            // matches the by-pointer call emitted above (and rustc's ABI).
            decl_args = registry_param_llvm
                .iter()
                .map(|t| {
                    if crate::emit::target_is_windows() && t == "i128" {
                        "ptr".to_string()
                    } else {
                        t.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
        } else {
            for (i, arg) in args.iter().enumerate() {
                if i > 0 {
                    decl_args.push_str(", ");
                }
                let t = self.operand_llvm_ty(arg);
                let t = if t == "void" || t.is_empty() {
                    "i64".to_string()
                } else {
                    t
                };
                let _ = write!(decl_args, "{t}");
            }
        }
        let registry_ret_llvm: Option<String> =
            gossamer_abi::lookup(name).map(|e| e.sig.ret.llvm_ir().to_string());
        // Win64: an `i128` (Fat) return crosses the boundary in a 16-byte
        // vector register, which rustc models as `<16 x i8>`; llc returns a
        // bare `i128` in a GP register pair instead, so the two disagree. We
        // declare + call the runtime symbol as `<16 x i8>` to match rustc,
        // then `bitcast` the result back to the `i128` the rest of the body
        // expects. Skipped on SysV, where bare `i128` already agrees.
        let win_fat_ret = super::misc::needs_win64_fat_ret(
            crate::emit::target_is_windows(),
            registry_ret_llvm.as_deref(),
        );
        // The logical return type the surrounding code consumes (always the
        // registry/MIR type); `decl_ret` below is the *wire* type used for the
        // declaration and call instruction.
        let logical_ret = if let Some(r) = &registry_ret_llvm {
            r.clone()
        } else if dest_ty == "void" || is_unit(self.tcx, self.body.local_ty(dest_local)) {
            "void".to_string()
        } else {
            dest_ty.clone()
        };
        let decl_ret = if win_fat_ret {
            "<16 x i8>".to_string()
        } else {
            logical_ret.clone()
        };
        // Always declare using call-site types so the declaration matches
        // the call instruction LLVM sees. Registry types (via declare_rt)
        // may differ — e.g. gos_rt_result_payload is I64 in the registry
        // but called as ptr in compiled MIR because the payload is a heap
        // pointer reinterpreted as i64 in the C ABI. On x86-64 both share
        // the rax register so the call is correct; the declaration must
        // agree with the call site or opt miscompiles with the wrong type.
        //
        // De-duplicate by function name: if any prior call site already
        // produced a declaration for this symbol, keep that one and
        // skip this insertion. Multiple distinct signatures for the
        // same name make `opt` reject the IR with `invalid
        // redefinition of function`, which then drops the whole module
        // into the Cranelift fallback (Result/Option constructors with
        // mixed `i64` / `ptr` / `void` payload args are the canonical
        // trigger).
        let needle = format!("@{name}(");
        if !self.runtime_refs.iter().any(|d| d.contains(&needle)) {
            self.runtime_refs
                .insert(format!("declare {decl_ret} @{name}({decl_args})"));
        }
        if decl_ret == "void" {
            writeln!(self.out, "  call void @{name}({arg_text})").unwrap();
            // Rvalue-position void call: synthesise a sentinel value
            // matching the destination slot's type. Normally the dest
            // is unit-typed (a no-op store), but the drop pass may assign
            // a free call to a local whose type is the function's return
            // type (e.g. ptr). LLVM 18 rejects `store ptr 0`; use `null`.
            let sentinel = match dest_ty.as_str() {
                "ptr" => "null",
                "double" | "float" => "0.0",
                _ => "0",
            };
            Ok(sentinel.to_string())
        } else {
            let tmp = self.fresh();
            writeln!(self.out, "  {tmp} = call {decl_ret} @{name}({arg_text})").unwrap();
            // Win64 Fat return: unwrap the `<16 x i8>` wire value back to the
            // `i128` the rest of the body manipulates.
            let tmp = if win_fat_ret {
                let unwrapped = self.fresh();
                writeln!(self.out, "  {unwrapped} = bitcast <16 x i8> {tmp} to i128").unwrap();
                unwrapped
            } else {
                tmp
            };
            // The declaration is canonical, but the destination
            // slot may expect a different but ABI-compatible
            // shape (e.g. `gos_rt_result_payload` returns `i64`
            // per registry, but the call site stores it into a
            // ptr-typed slot when the payload was a heap pointer
            // reinterpreted as an integer). Coerce so the
            // surrounding store / use is well-typed. Compare against the
            // logical return type, not the `<16 x i8>` wire type.
            if logical_ret != dest_ty && dest_ty != "void" && !dest_ty.is_empty() {
                let coerced = self.coerce_llvm_value(&tmp, &logical_ret, &dest_ty);
                Ok(coerced)
            } else {
                Ok(tmp)
            }
        }
    }

    pub(crate) fn lower_call_intrinsic(
        &mut self,
        name: &str,
        args: &[Operand],
        dest_local: Local,
    ) -> Result<String, BuildError> {
        let (llvm_intrinsic, expected_arity) = match name {
            "f64.sqrt" | "sqrt" => ("llvm.sqrt.f64", 1),
            "f64.sin" | "sin" => ("llvm.sin.f64", 1),
            "f64.cos" | "cos" => ("llvm.cos.f64", 1),
            "f64.abs" | "abs" => ("llvm.fabs.f64", 1),
            "f64.floor" | "floor" => ("llvm.floor.f64", 1),
            "f64.ceil" | "ceil" => ("llvm.ceil.f64", 1),
            "f64.exp" | "exp" => ("llvm.exp.f64", 1),
            "f64.ln" | "ln" | "f64.log" | "log" => ("llvm.log.f64", 1),
            // Inline `buf.set_byte(i, x)` as a branchless bounds-guarded store
            // instead of a runtime call. `GosU8Vec` is `{ i64 len, ptr data }`;
            // an out-of-range / null access redirects to a scratch byte, which
            // reproduces `gos_rt_heap_u8_set`'s no-op-on-OOB semantics without
            // the per-byte call overhead (fasta's hot inner loop).
            "gos_rt_heap_u8_set" if args.len() == 3 => {
                let v = self.lower_operand(&args[0])?;
                let idx = self.lower_operand(&args[1])?;
                let val = self.lower_operand(&args[2])?;
                self.runtime_refs.insert(
                    "@gos_u8_set_scratch = internal global [16 x i8] zeroinitializer".to_string(),
                );
                self.runtime_refs.insert(
                    "@gos_u8_set_hdr = internal global { i64, ptr } { i64 0, ptr @gos_u8_set_scratch }"
                        .to_string(),
                );
                let vnn = self.fresh();
                let vbase = self.fresh();
                let len = self.fresh();
                let dptr = self.fresh();
                let data = self.fresh();
                let ge0 = self.fresh();
                let lt = self.fresh();
                let inb = self.fresh();
                let elem = self.fresh();
                let target = self.fresh();
                let valb = self.fresh();
                writeln!(self.out, "  {vnn} = icmp ne ptr {v}, null").unwrap();
                writeln!(
                    self.out,
                    "  {vbase} = select i1 {vnn}, ptr {v}, ptr @gos_u8_set_hdr"
                )
                .unwrap();
                writeln!(self.out, "  {len} = load i64, ptr {vbase}").unwrap();
                writeln!(
                    self.out,
                    "  {dptr} = getelementptr inbounds i8, ptr {vbase}, i64 8"
                )
                .unwrap();
                writeln!(self.out, "  {data} = load ptr, ptr {dptr}").unwrap();
                writeln!(self.out, "  {ge0} = icmp sge i64 {idx}, 0").unwrap();
                writeln!(self.out, "  {lt} = icmp slt i64 {idx}, {len}").unwrap();
                writeln!(self.out, "  {inb} = and i1 {ge0}, {lt}").unwrap();
                writeln!(
                    self.out,
                    "  {elem} = getelementptr inbounds i8, ptr {data}, i64 {idx}"
                )
                .unwrap();
                writeln!(
                    self.out,
                    "  {target} = select i1 {inb}, ptr {elem}, ptr @gos_u8_set_scratch"
                )
                .unwrap();
                writeln!(self.out, "  {valb} = trunc i64 {val} to i8").unwrap();
                writeln!(self.out, "  store i8 {valb}, ptr {target}").unwrap();
                let dest_ty = render_ty(self.tcx, self.body.local_ty(dest_local));
                return Ok(match dest_ty.as_str() {
                    "ptr" => "null",
                    "double" | "float" => "0.0",
                    _ => "0",
                }
                .to_string());
            }
            other if other.starts_with("gos_rt_") => {
                // Generic runtime-call intrinsic: emit a regular
                // call against the named runtime symbol. Mirrors
                // how the Cranelift backend's `lower_intrinsic_call`
                // falls through to a named-call dispatch when the
                // intrinsic isn't a recognised inline shape.
                return self.lower_runtime_call_intrinsic(other, args, dest_local);
            }
            _ => {
                return Err(BuildError::Unsupported("unknown CallIntrinsic name"));
            }
        };
        if args.len() != expected_arity {
            return Err(BuildError::Unsupported("CallIntrinsic arity mismatch"));
        }
        // Ensure a `declare` for this intrinsic lands in the
        // module header.
        self.runtime_refs
            .insert(format!("declare double @{llvm_intrinsic}(double)"));
        let arg_v = self.lower_operand(&args[0])?;
        let dest_llvm = render_ty(self.tcx, self.body.local_ty(dest_local));
        let tmp = self.fresh();
        writeln!(
            self.out,
            "  {tmp} = call {dest_llvm} @{llvm_intrinsic}(double {arg_v})"
        )
        .unwrap();
        Ok(tmp)
    }
}
