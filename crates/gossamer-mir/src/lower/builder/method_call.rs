#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::needless_range_loop)]
#![allow(clippy::wildcard_imports)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::option_if_let_else)]
#![allow(clippy::match_same_arms)]
#![allow(clippy::if_not_else)]
#![allow(clippy::single_match_else)]
#![allow(clippy::needless_pass_by_value)]
#![allow(clippy::manual_let_else)]
#![allow(clippy::redundant_else)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::map_unwrap_or)]
#![allow(clippy::struct_excessive_bools)]
#![allow(clippy::module_name_repetitions)]
#![allow(clippy::unnecessary_wraps)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::single_match)]
#![allow(clippy::useless_conversion)]

use std::collections::HashMap;

use gossamer_ast::Ident;
use gossamer_hir::{
    HirAdtKind, HirBinaryOp, HirBlock, HirExpr, HirExprKind, HirFn, HirItem, HirItemKind,
    HirLiteral, HirMatchArm, HirPat, HirPatKind, HirProgram, HirStmt, HirStmtKind, HirUnaryOp,
};
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{
    BasicBlock, BinOp, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue,
    Statement, StatementKind, Terminator, UnOp,
};

use super::*;

use super::Builder;

impl<'a> Builder<'a> {
    pub(crate) fn lower_method_call(
        &mut self,
        receiver: &HirExpr,
        method: &Ident,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // `.clone()` on a `json::Value` receiver. The generic
        // identity-copy arm walks `match self.tcx.kind_of(ty)` and
        // falls through to `_ =>` for `JsonValue`, then the MIR
        // receiver-kind probe defaulted to `receiver_ty` (a `Var`
        // for chained accesses like `tcs[k].clone()`). The cloned
        // local then lost its `JsonValue` tag and downstream
        // `json::get(&clone_local, ...)` missed the json runtime
        // helper, returning the empty string. Short-circuit clone
        // on a JsonValue receiver to a direct copy with the
        // receiver's MIR type preserved.
        // `.clone()` on a `json::Value` receiver short-circuits to a
        // direct copy with the receiver's MIR type preserved (the
        // generic identity-copy arm later falls through to a Var dest
        // for `tcs[k].clone()` shapes and downstream json helpers stop
        // dispatching). Only lower the receiver when we know we'll
        // consume it here — falling through after the lower would
        // leave behind the receiver's lowered Call as dead but live
        // MIR, and any heap-container result (e.g. `gos_rt_vec_get_i64`
        // producing a `Vec<T>`-typed dest) would be marked twice for
        // `gos_rt_vec_free`, producing a double free at scope end.
        if method.name.as_str() == "clone" && args.is_empty() && self.is_json_value_ty(receiver.ty)
        {
            let recv_local = self.lower_expr(receiver)?;
            let recv_mir_ty = self.locals[recv_local.0 as usize].ty;
            let dest = self.fresh(recv_mir_ty);
            if let Some(rk) = self.local_runtime_kind.get(&recv_local).copied() {
                self.local_runtime_kind.insert(dest, rk);
            }
            self.emit_assign(
                Place::local(dest),
                Rvalue::Use(Operand::Copy(Place::local(recv_local))),
                span,
            );
            return Some(dest);
        }
        // `result.map_err(closure)` / `result.map(closure)` when the
        // HIR receiver type is unresolved but its lowered MIR type
        // turns out to be a Result Adt. Without this short-circuit
        // the generic dispatch sees the unresolved kind, falls
        // through to the identity-copy arm, and silently drops the
        // mapping (errors.gos `text.parse().map_err(|_| …)?`
        // reproducer).
        if matches!(method.name.as_str(), "map_err" | "map") && args.len() == 1 {
            let receiver_ty_for_kind = self
                .receiver_local_from_path(receiver)
                .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
            if matches!(self.tcx.kind_of(receiver_ty_for_kind), TyKind::Var(_)) {
                if let Some(local) =
                    self.try_lower_result_map_with_eager_recv(receiver, method, &args[0], ty, span)
                {
                    return Some(local);
                }
            }
        }
        // `[].to_vec()` — the empty-array literal carries no
        // element type, so the generic `gos_rt_vec_clone` arm
        // produces a `GosVec { elem_bytes: 0, … }`. Subsequent
        // `.push(t)` allocates `0 * cap` bytes for the data
        // buffer; `xs[0]` then reads through a bogus offset
        // and segfaults. Detect the empty-array shape and pin
        // the dest's `elem_bytes` from the call's HIR return
        // type (`Vec<T>`) by emitting a direct
        // `gos_rt_vec_new(elem_bytes_for_T)` instead.
        if method.name.as_str() == "to_vec"
            && args.is_empty()
            && let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) = &receiver.kind
            && elems.is_empty()
        {
            let mut peeled = ty;
            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                peeled = *inner;
            }
            let elem_ty_opt = match self.tcx.kind_of(peeled) {
                TyKind::Vec(elem) | TyKind::Slice(elem) => Some(*elem),
                TyKind::Array { elem, .. } => Some(*elem),
                _ => None,
            };
            if let Some(elem_ty) = elem_ty_opt {
                let elem_bytes = self.elem_bytes_of(elem_ty);
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let elem_bytes_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(elem_bytes_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(elem_bytes)))),
                    span,
                );
                let vec_ty = self.tcx.intern(TyKind::Vec(elem_ty));
                let dest = self.fresh(vec_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_vec_new".to_string())),
                    args: vec![Operand::Copy(Place::local(elem_bytes_local))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
        }
        // `[a, b, c].to_vec()` on a non-empty literal-array
        // receiver. The default `to_vec` arm lowers to
        // `gos_rt_vec_clone(receiver)`, but `gos_rt_vec_clone`
        // expects a real `*const GosVec` header (len/cap/
        // elem_bytes/ptr). The lowered receiver is a stack
        // `[T; N]` aggregate whose first 24 bytes are the raw
        // payload — `gos_rt_vec_clone` then reads `elems[0]` as
        // `len`, `elems[1]` as `cap`, etc. and either segfaults
        // or panics with a bogus `memory allocation of <huge>
        // bytes failed` when the runtime tries to copy that
        // many bytes. Detect the literal-array shape, lower the
        // elements normally, and route through the existing
        // `gos_rt_vec_from_arr(elem_bytes, &arr, len)` shim that
        // builds a real `GosVec` header around the stack
        // payload. Mirrors `coerce_arg_for_binding`'s `[T; N] →
        // Vec<T>` fix for binding calls.
        if method.name.as_str() == "to_vec"
            && args.is_empty()
            && let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) = &receiver.kind
            && !elems.is_empty()
        {
            let raw = self.lower_expr(receiver)?;
            let raw_ty = self.locals[raw.0 as usize].ty;
            let TyKind::Array { elem: elem_ty, len } = self.tcx.kind_of(raw_ty) else {
                return None;
            };
            let elem_bytes = self.elem_bytes_of(*elem_ty);
            let len_val = *len;
            let elem_ty = *elem_ty;
            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let elem_bytes_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(elem_bytes_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(elem_bytes)))),
                span,
            );
            let len_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(len_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(len_val as i128))),
                span,
            );
            let vec_ty = self.tcx.intern(TyKind::Vec(elem_ty));
            let dest = self.fresh(vec_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_from_arr".to_string())),
                args: vec![
                    Operand::Copy(Place::local(elem_bytes_local)),
                    Operand::Copy(Place::local(raw)),
                    Operand::Copy(Place::local(len_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // `arr.swap(i, j)` super-instruction. The generic Call
        // fallback at the end of this function would lower this as
        // `Call(Const(Str("swap")), …)` which the cranelift backend
        // can't resolve — JIT- and AOT-compiled bodies silently
        // produced a typed-zero stub, leaving the receiver
        // unmutated. Inlining as four index ops (read i, read j,
        // write j-into-i, write i-into-j) keeps the semantics
        // intact across every backend.
        if method.name.as_str() == "swap" && args.len() == 2 {
            if let Some(swap_local) =
                self.try_lower_array_swap(receiver, &args[0], &args[1], ty, span)
            {
                return Some(swap_local);
            }
        }
        // `xs.sort_by(closure)` for `[i64; N]` / `[i64]` / `Vec<i64>`.
        // Routes through one of two runtime helpers depending on
        // the receiver shape: fixed buffers go through
        // `gos_rt_arr_sort_by_i64(ptr, len, env)`; Vec receivers
        // through `gos_rt_vec_sort_by_i64(vec, env)`. Both load the
        // closure body address from `env[0]` and forward the
        // `(env, *const T, *const T) -> i64` callback.
        if method.name.as_str() == "sort_by" && args.len() == 1 {
            if let Some(local) = self.try_lower_sort_by(receiver, &args[0], ty, span) {
                return Some(local);
            }
        }
        // Fused-increment peephole: `m.insert(k, m.get_or(k, 0)
        // + by)` (or `… + 1`) on an i64-keyed map collapses into
        // a single `gos_rt_map_inc_i64(m, k, by)` call. Halves
        // the lock + hash work on every counter-style loop.
        if method.name.as_str() == "insert" && args.len() == 2 {
            if let Some(local) = self.try_lower_map_inc(receiver, &args[0], &args[1], ty, span) {
                return Some(local);
            }
        }
        // `m.inc(key)` / `m.inc(key, by)` for `HashMap<String, i64>`.
        // The interpreter ships a dedicated counter idiom; the
        // compiled tier needs a matching dispatch or values stay
        // at zero. Default `by` to 1 when only the key is given.
        if method.name.as_str() == "inc" && (args.len() == 1 || args.len() == 2) {
            let recv_ty_local = self
                .receiver_local_from_path(receiver)
                .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
            let val_kind = self.hash_map_value_kind(recv_ty_local);
            let key_kind = self.hash_map_key_kind(recv_ty_local);
            if matches!(val_kind, Some(MapValueKind::I64)) {
                let (fn_name, key_kind_ok) = match key_kind {
                    Some(MapKeyKind::String) => ("gos_rt_map_inc_str_i64", true),
                    Some(MapKeyKind::I64) => ("gos_rt_map_inc_i64", true),
                    _ => ("", false),
                };
                if key_kind_ok {
                    let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                    let recv_local = self.lower_expr(receiver)?;
                    let key_local = self.lower_expr(&args[0])?;
                    let by_local = if args.len() == 2 {
                        self.lower_expr(&args[1])?
                    } else {
                        let l = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(l),
                            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                            span,
                        );
                        l
                    };
                    let dest = self.fresh(i64_ty);
                    let next = self.new_block(span);
                    self.terminate(Terminator::Call {
                        callee: Operand::Const(ConstValue::Str(fn_name.to_string())),
                        args: vec![
                            Operand::Copy(Place::local(recv_local)),
                            Operand::Copy(Place::local(key_local)),
                            Operand::Copy(Place::local(by_local)),
                        ],
                        destination: Place::local(dest),
                        target: Some(next),
                    });
                    self.set_current(next);
                    return Some(dest);
                }
            }
        }
        // `b.push_str(s)` on an owned `String` receiver. The runtime
        // models gos `String` as `*const c_char` (immutable
        // nul-terminated bytes), so true in-place mutation isn't
        // representable. Rewrite as `b = __concat(b, s)`: build a new
        // string into the runtime's concat buffer and update the
        // receiver local in place. Without this rewrite the call
        // landed on a typed-zero stub and the receiver kept its
        // original empty bytes (the `release_owned_string_push_str`
        // gauge entry).
        if method.name.as_str() == "push_str"
            && args.len() == 1
            && let Some(recv_local) = self.receiver_local_from_path(receiver)
        {
            let recv_ty = self.locals[recv_local.0 as usize].ty;
            let mut peeled = recv_ty;
            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                peeled = *inner;
            }
            if matches!(self.tcx.kind_of(peeled), TyKind::String) {
                let arg_local = self.lower_expr(&args[0])?;
                let concat_dest = self.fresh(recv_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("__concat".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(recv_local)),
                        Operand::Copy(Place::local(arg_local)),
                    ],
                    destination: Place::local(concat_dest),
                    target: Some(next),
                });
                self.set_current(next);
                self.emit_assign(
                    Place::local(recv_local),
                    Rvalue::Use(Operand::Copy(Place::local(concat_dest))),
                    span,
                );
                return Some(self.lower_unit(span));
            }
        }

        // Prefer the MIR local's pinned type over the HIR receiver
        // type when the receiver is a Path bound to a local — the
        // type checker may have left the HIR type as an inference
        // variable, but we pin runtime-helper return types
        // (`gos_rt_stream_read_to_string` → `String`, etc.) on the
        // MIR side at line ~2026. Without this lookup `s.len()`
        // for `let s = stdin.read_to_string()` falls through the
        // `len` dispatch's default arm to `gos_rt_len` — which
        // misinterprets the C-string pointer as a length-prefixed
        // buffer and returns the first 8 data bytes.
        let receiver_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |local| self.locals[local.0 as usize].ty);
        let receiver_kind = self.tcx.kind_of(receiver_ty).clone();
        // Unwrap a leading `&T` so `s.len()` on a `&String`
        // parameter lowers the same as on an owned `String`.
        let mut receiver_kind_flat = match &receiver_kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(*inner).clone(),
            other => other.clone(),
        };
        // `(*flags.<long>).method(...)` — the HIR receiver type is
        // an unresolved inference variable, but the underlying cell
        // kind is known statically from `local_define_layout`.
        // Promote the receiver kind so method dispatch (`to_string`,
        // `len`, …) picks the right runtime helper.
        if matches!(receiver_kind_flat, TyKind::Var(_)) {
            if let Some(kind) = self.peek_define_deref_kind(receiver) {
                receiver_kind_flat = kind;
            }
        }
        // `<chain>.method().to_string()` — when the chain ends in
        // a call whose return shape is pinned (`len`, `parse`,
        // `to_string`, integer-yielding helpers), surface that
        // shape so downstream `.to_string()` dispatches through
        // the i64/f64 runtime formatter instead of the
        // identity-copy arm. Expression-only walk; emits no MIR.
        if matches!(receiver_kind_flat, TyKind::Var(_)) {
            if let Some(kind) = self.peek_method_chain_kind(receiver) {
                receiver_kind_flat = kind;
            }
        }
        // `args[i].method()` — when typeck resolves the Index
        // expression to its base collection (Vec / Slice / Array)
        // instead of the element type (a multi-module typeck
        // regression: single-file builds correctly type
        // `args[i]` as `String`, but with `mod util;` the HIR
        // node retains the base `Vec<String>`), prefer the
        // element kind taken from the base local's MIR type.
        // Without this, `.len()` on `args[i]` lands on the
        // collection arm `gos_rt_arr_len`, which then crashes
        // inside `mov (%rdi),%rax` reading a Vec header out of a
        // `*const c_char` string pointer.
        let needs_index_fixup = matches!(
            receiver_kind_flat,
            TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } | TyKind::Var(_)
        );
        if needs_index_fixup {
            if let HirExprKind::Index { base, .. } = &receiver.kind {
                if let Some(base_ty) = self.peek_collection_type(base) {
                    let elem_ty = match self.tcx.kind_of(base_ty) {
                        TyKind::Vec(elem) | TyKind::Slice(elem) => Some(*elem),
                        TyKind::Array { elem, .. } => Some(*elem),
                        _ => None,
                    };
                    if let Some(elem_ty) = elem_ty {
                        let mut elem_kind = self.tcx.kind_of(elem_ty).clone();
                        while let TyKind::Ref { inner, .. } = elem_kind {
                            elem_kind = self.tcx.kind_of(inner).clone();
                        }
                        if !matches!(elem_kind, TyKind::Var(_)) {
                            receiver_kind_flat = elem_kind;
                        }
                    }
                }
            }
        }

        // Detect `recv.headers.<insert|get>(name[, value])` where
        // `recv` is an `http::Response`/`http::Request`. Fold the
        // chain into a single `gos_rt_http_*_set_header` /
        // `_get_header` call so the intermediate headers handle
        // never has to be represented.
        if let HirExprKind::Field {
            receiver: inner,
            name: field_name,
        } = &receiver.kind
        {
            if field_name.name.as_str() == "headers" {
                let inner_local_for_kind = self.receiver_local_from_path(inner);
                let inner_kind = inner_local_for_kind
                    .and_then(|l| self.local_runtime_kind.get(&l).copied())
                    .or_else(|| {
                        let inner_ty =
                            inner_local_for_kind.map_or(inner.ty, |l| self.locals[l.0 as usize].ty);
                        match self.tcx.kind_of(inner_ty) {
                            TyKind::Ref { inner: i, .. } => self.struct_name_of(*i),
                            _ => self.struct_name_of(inner_ty),
                        }
                        .and_then(|s| match s.as_str() {
                            "Response" => Some("http::Response"),
                            "Request" => Some("http::Request"),
                            _ => None,
                        })
                    });
                if matches!(inner_kind, Some("http::Response" | "http::Request")) {
                    let helper = match (inner_kind, method.name.as_str()) {
                        (Some("http::Response"), "insert") => {
                            Some(("gos_rt_http_response_set_header", self.tcx.unit(), 2usize))
                        }
                        (Some("http::Response"), "get") => Some((
                            "gos_rt_http_response_get_header",
                            self.tcx.string_ty(),
                            1usize,
                        )),
                        (Some("http::Request"), "insert") => {
                            Some(("gos_rt_http_request_set_header", self.tcx.unit(), 2usize))
                        }
                        (Some("http::Request"), "get") => Some((
                            "gos_rt_http_request_get_header",
                            self.tcx.string_ty(),
                            1usize,
                        )),
                        _ => None,
                    };
                    if let Some((rt, ret_ty, want_args)) = helper {
                        if args.len() == want_args {
                            let inner_local = self.lower_expr(inner)?;
                            let mut ops = Vec::with_capacity(args.len() + 1);
                            ops.push(Operand::Copy(Place::local(inner_local)));
                            for a in args {
                                let al = self.lower_expr(a)?;
                                ops.push(Operand::Copy(Place::local(al)));
                            }
                            let dest = self.fresh(ret_ty);
                            let next = self.new_block(span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(rt.to_string())),
                                args: ops,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            return Some(dest);
                        }
                    }
                }
            }
        }

        // Stdlib dispatch table. First by method name alone —
        // covers receivers whose HIR type is still an unresolved
        // inference variable (common post-checker). The runtime
        // helpers accept any receiver shape and return a safe
        // default (0, empty, null) for inputs the native runtime
        // doesn't yet represent.
        //
        // When the callee name is empty the method is identity
        // (currently `.to_string()` / `.clone()` on any scalar or
        // string-shaped receiver — the GC already aliases the
        // buffer).
        let mut runtime_symbol: Option<&'static str> = match method.name.as_str() {
            // `.to_string()` routes to the runtime numeric
            // formatter for integer / float receivers. String
            // receivers fall through to the identity copy.
            // `to_string()` (no args) — scalar-to-string for
            // integer / float receivers; identity copy for the
            // others.
            //
            // `to_string(len)` (1 arg) — the canonical "freeze the
            // build buffer" step at the end of a `U8Vec`-backed
            // incremental construction loop. Mirrors F#'s
            // `StringBuilder.ToString()` and Rust's
            // `String::from_utf8(vec).unwrap()`. Routes to a
            // runtime helper that copies the first `len` bytes
            // into a fresh immutable `String`.
            "to_string" => {
                if args.len() == 1 {
                    Some("gos_rt_heap_u8_to_string")
                } else {
                    match &receiver_kind_flat {
                        TyKind::Int(_) => Some("gos_rt_i64_to_str"),
                        TyKind::Float(_) => Some("gos_rt_f64_to_str"),
                        _ => Some(""),
                    }
                }
            }
            "clone" => match &receiver_kind_flat {
                TyKind::Vec(_) | TyKind::Slice(_) => Some("gos_rt_vec_clone"),
                _ => Some(""),
            },
            // Option / Result methods. Result/Option now live as
            // `*mut GosResult { disc, payload }` heap aggregates
            // (see `gos_rt_result_new`), so `.unwrap()` /
            // `.unwrap_or()` / `.ok()` / `.err()` route through
            // runtime helpers that read the disc and return the
            // payload (or default) as a raw 64-bit slot. The
            // older identity-copy path was a leftover from the
            // pre-discriminator layout and silently returned the
            // aggregate pointer for callers expecting an i64 —
            // see e.g. fasta's `args[0].parse().unwrap_or(1000)`,
            // which yielded an arena address instead of 10. Fall
            // back to identity for non-Result receivers (e.g.
            // stdlib helpers that still return raw inner values
            // tagged with a Result-shaped HIR type).
            "unwrap" | "expect" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_unwrap")
                } else {
                    Some("")
                }
            }
            "unwrap_or" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_unwrap_or")
                } else {
                    Some("")
                }
            }
            "ok" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_ok")
                } else {
                    Some("")
                }
            }
            "err" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_err")
                } else {
                    Some("")
                }
            }
            // `result.ok_or(new_err)` on a Result receiver replaces
            // the Err with `new_err`; passes Ok through. Mirrors
            // Option's `.ok_or` shape so callers can write
            // `s.parse().ok_or("not a number".to_string())?` and
            // get a domain-meaningful message rather than the raw
            // ParseError. The HIR `receiver_ty` is often a Var for
            // chained calls (`parse().ok_or(...)`); detect the
            // result-returning shape via the chained method's name.
            "ok_or" => {
                let hir_is_result = matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty);
                let chain_returns_result = matches!(
                    &receiver.kind,
                    HirExprKind::MethodCall { name, .. } if matches!(
                        name.name.as_str(),
                        "parse" | "parse_i64" | "parse_f64"
                    )
                );
                if hir_is_result || chain_returns_result {
                    Some("gos_rt_result_ok_or")
                } else {
                    Some("")
                }
            }
            "len" => match &receiver_kind_flat {
                TyKind::String => Some("gos_rt_str_len"),
                TyKind::HashMap { .. } => Some("gos_rt_map_len"),
                TyKind::JsonValue => Some("gos_rt_json_len"),
                TyKind::Vec(_) | TyKind::Array { .. } | TyKind::Slice(_) => Some("gos_rt_len"),
                // The MIR type didn't resolve. Inspect the HIR
                // expression's static type as a fallback — common
                // shape is `let s = fs::read_to_string(...)?; s.len()`
                // where the typechecker leaves `s` as `Var(...)` but
                // the HIR `Path(s)` node still carries `String`.
                // Without this fallback the dispatch lands on the
                // generic `gos_rt_len`, which reads a Vec header
                // out of a `*const c_char` and returns garbage.
                _ => {
                    let mut peeled = receiver.ty;
                    while let TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                        peeled = *inner;
                    }
                    match self.tcx.kind_of(peeled) {
                        TyKind::String => Some("gos_rt_str_len"),
                        TyKind::HashMap { .. } => Some("gos_rt_map_len"),
                        TyKind::JsonValue => Some("gos_rt_json_len"),
                        _ => Some("gos_rt_len"),
                    }
                }
            },
            "trim" => Some("gos_rt_str_trim"),
            "contains" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_contains")
            }
            "starts_with" => Some("gos_rt_str_starts_with"),
            "ends_with" => Some("gos_rt_str_ends_with"),
            "find" if matches!(&receiver_kind_flat, TyKind::String) => Some("gos_rt_str_find_opt"),
            "rfind" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_rfind_opt")
            }
            "replace" => Some("gos_rt_str_replace"),
            "split" => Some("gos_rt_str_split"),
            // 0.7.0 string surface — split_once / rsplit_once return
            // `Option<(String, String)>` packed as a `*mut GosResult`
            // pair payload (see `gos_rt_str_split_once`).
            "split_once" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_split_once")
            }
            "rsplit_once" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_rsplit_once")
            }
            "count" if matches!(&receiver_kind_flat, TyKind::String) => Some("gos_rt_str_count"),
            "strip_chars" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_strip_chars")
            }
            "lstrip_chars" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_lstrip_chars")
            }
            "rstrip_chars" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_rstrip_chars")
            }
            "zfill" if matches!(&receiver_kind_flat, TyKind::String) => Some("gos_rt_str_zfill"),
            "center" if matches!(&receiver_kind_flat, TyKind::String) => Some("gos_rt_str_center"),
            "slice" if matches!(&receiver_kind_flat, TyKind::String) => Some("gos_rt_str_slice"),
            // 0.7.0 Vec method surface. `xs.slice(a, b)?` returns a
            // Result<Vec<T>, errors::Error>; `xs.first()` / `xs.last()`
            // return Option<T>; `xs.reversed()` returns a fresh Vec;
            // `xs.contains` / `xs.index_of` / `xs.count_of` need
            // element-type dispatch (String vs i64).
            "slice"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                Some("gos_rt_vec_slice_result")
            }
            "first"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                Some("gos_rt_vec_first")
            }
            "last"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                Some("gos_rt_vec_last")
            }
            "reversed"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                Some("gos_rt_vec_reversed")
            }
            "contains"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                let elem = vec_element_kind(self.tcx, receiver_ty);
                Some(if elem == VecElemKind::Str {
                    "gos_rt_vec_contains_str"
                } else {
                    "gos_rt_vec_contains_i64"
                })
            }
            "index_of"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                let elem = vec_element_kind(self.tcx, receiver_ty);
                Some(if elem == VecElemKind::Str {
                    "gos_rt_vec_index_of_str"
                } else {
                    "gos_rt_vec_index_of_i64"
                })
            }
            "count_of"
                if matches!(
                    &receiver_kind_flat,
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. }
                ) =>
            {
                let elem = vec_element_kind(self.tcx, receiver_ty);
                Some(if elem == VecElemKind::Str {
                    "gos_rt_vec_count_of_str"
                } else {
                    "gos_rt_vec_count_of_i64"
                })
            }
            // 0.7.0 HashMap method surface — keys / values yield
            // Vec<K> / Vec<V>; pop returns Option<V>.
            "keys" if matches!(&receiver_kind_flat, TyKind::HashMap { .. }) => {
                Some("gos_rt_map_keys_vec")
            }
            "values" if matches!(&receiver_kind_flat, TyKind::HashMap { .. }) => {
                Some("gos_rt_map_values_vec")
            }
            "pop" if matches!(&receiver_kind_flat, TyKind::HashMap { .. }) => {
                let key = hashmap_key_kind(self.tcx, receiver_ty);
                Some(if key == VecElemKind::Str {
                    "gos_rt_map_pop_str"
                } else {
                    "gos_rt_map_pop_i64"
                })
            }
            "lines" => Some("gos_rt_str_lines"),
            "repeat" => Some("gos_rt_str_repeat"),
            "byte_at" => Some("gos_rt_str_byte_at"),
            // `s.substring(start, end)` for String receivers.
            // Without this dispatch the call falls through to a
            // bare-name free-fn lookup; user code that defines a
            // `pub fn substring(s: &String, a: i64, b: i64)`
            // wrapper (askq's `util::substring`) then resolves
            // its own `s.substring(a, b)` body to itself and
            // stack-overflows in compiled mode.
            "substring" if matches!(&receiver_kind_flat, TyKind::String) => {
                Some("gos_rt_str_substring")
            }
            // `is_empty` collapses to `len(self) == 0`. Route to
            // a small helper that delegates to the right `len`
            // backend for the receiver kind.
            "is_empty" => match &receiver_kind_flat {
                TyKind::String => Some("gos_rt_str_is_empty"),
                _ => Some("gos_rt_len_is_zero"),
            },
            // errors::Error methods. `is` here routes
            // unconditionally to the runtime helper because no
            // other type in the stdlib defines a `.is(...)`
            // method today; if a user struct defines one, the
            // user-impl dispatch below wins (it runs after this
            // table).
            "message" => Some("gos_rt_error_message"),
            "cause" => Some("gos_rt_error_cause"),
            "is" => Some("gos_rt_error_is"),
            // bufio::Scanner methods.
            "scan" => Some("gos_rt_bufio_scanner_scan"),
            "text" => Some("gos_rt_bufio_scanner_text"),
            // `ResponseStream::next_line() -> Option<String>`. The
            // receiver is the 3-slot blob `[__handle, status,
            // content_type]` returned by `gos_rt_http_stream`; the
            // helper reads the leading i64 (the handle) and pops
            // one line from the registered Vec.
            "next_line" => Some("gos_rt_http_stream_next_line"),
            // http::Response getters.
            "status" => Some("gos_rt_http_response_status"),
            "body" => Some("gos_rt_http_response_body"),
            // http builder. The kind-dispatch above already routes
            // tagged `http::Request` receivers for `.header(k, v)`
            // builder calls; this name-only arm catches untagged
            // ones — `.send` falls below to the channel default
            // because channel sends are far more common in user
            // code than untagged-http requests.
            "header" => Some("gos_rt_http_request_header"),
            "send" => Some("gos_rt_chan_send"),
            // string parsing — `text.parse()` for an i64 binding
            // routes to gos_rt_parse_i64 with a discarded ok flag.
            // Pin return to i64 for the common case; users with
            // f64 / float must annotate explicitly today.
            "parse" => Some("gos_rt_parse_i64_result"),
            // Result/Option chained helpers map to identity on
            // the happy path. The user passes in a closure; we
            // discard it (the compiled tier doesn't run the
            // error-mapping closure today).
            // map_err / map only dispatch when the receiver's
            // MIR-pinned type is a real Result Adt. Stdlib helpers
            // like `fs::write` return a bool today; routing
            // `.map_err(...)` on a bool through the result-helper
            // would feed an i8 to a `*mut GosResult` parameter and
            // trip the cranelift verifier.
            "map_err" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_map_err")
                } else {
                    Some("")
                }
            }
            "map" => {
                if matches!(&receiver_kind_flat, TyKind::Adt { .. })
                    && self.is_result_or_option_adt(receiver_ty)
                {
                    Some("gos_rt_result_map")
                } else {
                    Some("")
                }
            }
            "to_lowercase" | "to_lower" => Some("gos_rt_str_to_lower"),
            "to_uppercase" | "to_upper" => Some("gos_rt_str_to_upper"),
            "push" => Some("gos_rt_vec_push"),
            "pop" => Some("gos_rt_vec_pop"),
            "sort" => Some("gos_rt_vec_sort_i64"),
            "iter" => Some("gos_rt_arr_iter"),
            "to_vec" => match &receiver_kind_flat {
                // Vec/Slice/Array `.to_vec()` must produce an
                // independent copy — bubble_sort's `out.swap(...)`
                // was mutating the caller's slice through the
                // aliased pointer. Other types fall through to
                // the identity copy.
                TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } => {
                    Some("gos_rt_vec_clone")
                }
                _ => Some(""),
            },
            "as_bytes" => match &receiver_kind_flat {
                // String-receiver `.as_bytes()` materialises a real
                // length-prefixed arena buffer. The previous identity
                // lowering returned the raw c_char ptr; passing that
                // to a callee with `&[u8]` parameter and calling
                // `.len()` on it inside the callee read the first
                // 8 string bytes as a length, crashing on dereference.
                TyKind::String => Some("gos_rt_str_as_bytes"),
                _ => Some(""),
            },
            "as_str" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_as_str"),
                _ => Some(""),
            },
            // JSON value query/cast methods. The runtime helpers
            // accept a `*mut GosJson` (passed as a flat pointer)
            // and return either a fresh `*mut GosJson` (for
            // chained queries) or a primitive scalar.
            "as_i64" => Some("gos_rt_json_as_i64"),
            "as_f64" => Some("gos_rt_json_as_f64"),
            "as_bool" => Some("gos_rt_json_as_bool"),
            "is_null" => Some("gos_rt_json_is_null"),
            "at" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_at"),
                _ => None,
            },
            "recv" => Some("gos_rt_chan_recv_option"),
            // `rx.recv_ctx(&ctx)` — same shape as `recv`, but
            // takes a Context handle as the second arg. The
            // runtime helper polls cancellation on both the
            // goroutine park path and the OS-thread condvar
            // path, returning None when the context fires.
            "recv_ctx" => Some("gos_rt_chan_recv_ctx_option"),
            "try_send" => Some("gos_rt_chan_try_send"),
            "try_recv" => Some("gos_rt_chan_try_recv_option"),
            "close" => Some("gos_rt_chan_close"),
            // Stream methods (on `io::stdout()` / `io::stderr()`
            // / `io::stdin()` handles). Mirrors Rust's `Write` /
            // `BufRead` trait surface.
            "write_byte" => Some("gos_rt_stream_write_byte"),
            "write_byte_array" | "write_bytes" => Some("gos_rt_stream_write_byte_array"),
            "write" | "write_str" => Some("gos_rt_stream_write_str"),
            "flush" => Some("gos_rt_stream_flush"),
            "read_line" => Some("gos_rt_stream_read_line"),
            "read_to_string" => Some("gos_rt_stream_read_to_string"),
            // HashMap method dispatch — gated on the receiver
            // actually being a `HashMap`, not just on having a
            // matching method name. Without the gate, a user
            // struct with an `impl Foo { fn get(...) }` would
            // route through the map helper at codegen time and
            // either segfault on the wrong ABI or read garbage.
            // `get` extends the gate to `JsonValue` because the
            // json runtime also exposes a single-arg `get(key)`.
            "insert" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::I64) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_insert_str_i64"),
                        _ => Some("gos_rt_map_insert_i64_i64"),
                    },
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_insert_str_str"),
                        _ => Some("gos_rt_map_insert_i64_str"),
                    },
                    _ => Some("gos_rt_map_insert_i64_i64"),
                },
                _ => None,
            },
            "get" => match &receiver_kind_flat {
                TyKind::JsonValue => Some("gos_rt_json_get"),
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_str_str"),
                        _ => Some("gos_rt_map_get_i64_str"),
                    },
                    _ => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_str_i64"),
                        _ => Some("gos_rt_map_get_i64"),
                    },
                },
                _ => None,
            },
            "get_or" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_or_str_str"),
                        _ => Some("gos_rt_map_get_or_i64_str"),
                    },
                    _ => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_get_or_str_i64"),
                        _ => Some("gos_rt_map_get_or_i64"),
                    },
                },
                _ => None,
            },
            "or_insert" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::I64) => match self.hash_map_key_kind(receiver_ty) {
                        Some(MapKeyKind::String) => Some("gos_rt_map_or_insert_str_i64"),
                        _ => Some("gos_rt_map_or_insert_i64_i64"),
                    },
                    _ => None,
                },
                _ => None,
            },
            "remove" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_remove_str"),
                    _ => Some("gos_rt_map_remove_i64"),
                },
                _ => None,
            },
            "contains_key" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_contains_key_str"),
                    _ => Some("gos_rt_map_contains_key_i64"),
                },
                _ => None,
            },
            "clear" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => Some("gos_rt_map_clear"),
                _ => None,
            },
            // `m.inc_at(seq, start, len, by)` — zero-copy slice
            // hash for `HashMap<String, i64>`. Single hash lookup
            // per call, no per-iteration scratch allocation —
            // mirrors `*m.entry(&seq[i..i+k]).or_insert(0) += by`.
            "inc_at" => match self.hash_map_value_kind(receiver_ty) {
                Some(MapValueKind::I64) => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_inc_at_str_i64"),
                    _ => None,
                },
                _ => None,
            },
            // HashMap iteration. Each helper snapshots the
            // requested column into a fresh `GosVec` so the
            // for-loop lowerer can drive iteration with the
            // regular `gos_rt_vec_*` helpers. String-keyed /
            // string-valued shapes go through `*_str`; everything
            // else through `*_i64`.
            "keys" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_key_kind(receiver_ty) {
                    Some(MapKeyKind::String) => Some("gos_rt_map_keys_str"),
                    _ => Some("gos_rt_map_keys_i64"),
                },
                _ => None,
            },
            "values" => match &receiver_kind_flat {
                TyKind::HashMap { .. } => match self.hash_map_value_kind(receiver_ty) {
                    Some(MapValueKind::String) => Some("gos_rt_map_values_str"),
                    _ => Some("gos_rt_map_values_i64"),
                },
                _ => None,
            },
            // Mutex<T> / WaitGroup / Atomic / heap-Vec
            // primitives. Each method dispatches by name —
            // the runtime function takes the receiver
            // pointer as its first arg, matching the rest of
            // the table.
            "lock" => Some("gos_rt_mutex_lock"),
            "unlock" => Some("gos_rt_mutex_unlock"),
            "add" => Some("gos_rt_wg_add"),
            "done" => Some("gos_rt_wg_done"),
            "wait" => Some("gos_rt_wg_wait"),
            "load" => Some("gos_rt_atomic_i64_load"),
            "store" => Some("gos_rt_atomic_i64_store"),
            "fetch_add" => Some("gos_rt_atomic_i64_fetch_add"),
            "set_at" => Some("gos_rt_heap_i64_set"),
            "get_at" => Some("gos_rt_heap_i64_get"),
            "vec_len" => Some("gos_rt_heap_i64_len"),
            "write_range_to_stdout" => Some("gos_rt_heap_i64_write_bytes_to_stdout"),
            "write_lines_to_stdout" => Some("gos_rt_heap_i64_write_lines_to_stdout"),
            // U8Vec methods. Distinct names from the I64Vec
            // family because MIR's method dispatch is by name
            // alone — sharing `set_at` between i64 and u8
            // receivers would silently write through the
            // i64-stride helper to a u8 buffer, corrupting
            // adjacent bytes.
            "set_byte" => Some("gos_rt_heap_u8_set"),
            "get_byte" => Some("gos_rt_heap_u8_get"),
            "byte_len" => Some("gos_rt_heap_u8_len"),
            "write_byte_range_to_stdout" => Some("gos_rt_heap_u8_write_bytes_to_stdout"),
            "write_byte_lines_to_stdout" => Some("gos_rt_heap_u8_write_lines_to_stdout"),
            _ => None,
        };
        let _ = receiver_kind;

        // Char→String coercion for `s.split(c)` and `s.contains(c)`-
        // style calls where the user passes a `char` literal but
        // the underlying runtime helper expects a c-string ptr.
        // Lower the char arg through `gos_rt_char_to_str` before
        // it reaches the runtime call.
        let needs_char_to_str = matches!(
            method.name.as_str(),
            "split" | "contains" | "starts_with" | "ends_with" | "find" | "replace"
        );
        let _ = needs_char_to_str;

        // Receiver-shape-aware dispatch. Reads the kind tag from
        // a path-bound receiver, or inspects a chained method
        // call to recover its result kind for the
        // `a.b().c()`-style shapes.
        let receiver_runtime_kind = self
            .receiver_local_from_path(receiver)
            .and_then(|l| self.local_runtime_kind.get(&l).copied())
            .or_else(|| self.expr_runtime_kind(receiver));
        let kind_dispatch: Option<&'static str> =
            match (receiver_runtime_kind, method.name.as_str()) {
                (Some("flag::Set"), "string") => Some("gos_rt_flag_set_string"),
                (Some("flag::Set"), "int") => Some("gos_rt_flag_set_int"),
                (Some("flag::Set"), "uint") => Some("gos_rt_flag_set_uint"),
                (Some("flag::Set"), "float") => Some("gos_rt_flag_set_float"),
                (Some("flag::Set"), "bool") => Some("gos_rt_flag_set_bool"),
                (Some("flag::Set"), "duration") => Some("gos_rt_flag_set_duration"),
                (Some("flag::Set"), "string_list") => Some("gos_rt_flag_set_string_list"),
                (Some("flag::Set"), "short") => Some("gos_rt_flag_set_short"),
                (Some("flag::Set"), "usage") => Some("gos_rt_flag_set_usage"),
                (Some("flag::Set"), "parse") => Some("gos_rt_flag_set_parse"),
                // 0.4.0 stateful HTTP types — method-call dispatch.
                (Some("http::Router"), "add") => Some("gos_rt_router_add"),
                (Some("http::Router"), "get") => Some("gos_rt_router_get"),
                (Some("http::Router"), "post") => Some("gos_rt_router_post"),
                (Some("http::Router"), "put") => Some("gos_rt_router_put"),
                (Some("http::Router"), "delete") => Some("gos_rt_router_delete"),
                (Some("http::Router"), "patch") => Some("gos_rt_router_patch"),
                (Some("http::Router"), "head") => Some("gos_rt_router_head"),
                (Some("http::Router"), "options") => Some("gos_rt_router_options"),
                (Some("http::Router"), "serve") => Some("gos_rt_router_serve"),
                (Some("http::FileServer"), "serve") => Some("gos_rt_file_server_serve"),
                (Some("http::NativeClient"), "get") => Some("gos_rt_native_client_get"),
                (Some("http::Proxy"), "forward") => Some("gos_rt_proxy_forward"),
                (Some("http::Client"), "get") => Some("gos_rt_http_client_get"),
                (Some("http::Client"), "post") => Some("gos_rt_http_client_post"),
                (Some("http::Request"), "header") => Some("gos_rt_http_request_header"),
                (Some("http::Request"), "body") => Some("gos_rt_http_request_body"),
                (Some("http::Request"), "send") => Some("gos_rt_http_request_send"),
                (Some("http::Request"), "path") => Some("gos_rt_http_request_path"),
                (Some("http::Request"), "method") => Some("gos_rt_http_request_method"),
                (Some("http::Response"), "status") => Some("gos_rt_http_response_status"),
                (Some("http::Response"), "body") => Some("gos_rt_http_response_body"),
                (Some("bufio::Scanner"), "scan") => Some("gos_rt_bufio_scanner_scan"),
                (Some("bufio::Scanner"), "text") => Some("gos_rt_bufio_scanner_text"),
                (Some("errors::Error"), "message") => Some("gos_rt_error_message"),
                (Some("errors::Error"), "cause") => Some("gos_rt_error_cause"),
                (Some("errors::Error"), "is") => Some("gos_rt_error_is"),
                (Some("regex::Pattern"), "is_match") => Some("gos_rt_regex_is_match"),
                (Some("regex::Pattern"), "find") => Some("gos_rt_regex_find"),
                (Some("regex::Pattern"), "find_all") => Some("gos_rt_regex_find_all"),
                (Some("regex::Pattern"), "replace") => Some("gos_rt_regex_replace"),
                (Some("regex::Pattern"), "replace_all") => Some("gos_rt_regex_replace_all"),
                (Some("regex::Pattern"), "split") => Some("gos_rt_regex_split"),
                (Some("collections::HashSet"), "insert") => Some("gos_rt_set_insert"),
                (Some("collections::HashSet"), "contains") => Some("gos_rt_set_contains"),
                (Some("collections::HashSet"), "remove") => Some("gos_rt_set_remove"),
                (Some("collections::HashSet"), "len") => Some("gos_rt_set_len"),
                (Some("collections::BTreeMap"), "insert") => Some("gos_rt_btmap_insert"),
                (Some("collections::BTreeMap"), "get_or") => Some("gos_rt_btmap_get_or"),
                (Some("collections::BTreeMap"), "len") => Some("gos_rt_btmap_len"),
                (Some("sync::Map"), "set" | "insert") => Some("gos_rt_sync_map_set"),
                (Some("sync::Map"), "get") => Some("gos_rt_sync_map_get"),
                (Some("sync::Map"), "delete" | "remove") => Some("gos_rt_sync_map_delete"),
                (Some("sync::Map"), "len") => Some("gos_rt_sync_map_len"),
                (Some("sync::Map"), "contains" | "contains_key") => {
                    Some("gos_rt_sync_map_contains")
                }
                (Some("sync::Map"), "keys") => Some("gos_rt_sync_map_keys"),
                _ => None,
            };
        if let Some(rt) = kind_dispatch {
            // Lower the receiver + args, emit a Call to the
            // runtime helper, return the dest local. Pin a
            // sensible return type for the destination.
            let receiver_local = self.lower_expr(receiver)?;
            let mut arg_operands = Vec::with_capacity(args.len() + 1);
            arg_operands.push(Operand::Copy(Place::local(receiver_local)));
            // Router HTTP-verb methods take (router, pattern,
            // env, fn_addr) — synthesize the handler's env+fn_addr
            // from the trailing user argument (must be a struct
            // whose impl Handler { fn serve(...) }).
            let router_handler_method = matches!(
                rt,
                "gos_rt_router_get"
                    | "gos_rt_router_post"
                    | "gos_rt_router_put"
                    | "gos_rt_router_delete"
                    | "gos_rt_router_patch"
                    | "gos_rt_router_head"
                    | "gos_rt_router_options"
                    | "gos_rt_router_add"
            );
            let mut rt = rt;
            if router_handler_method && !args.is_empty() {
                let handler_idx = args.len() - 1;
                for arg in &args[..handler_idx] {
                    let a = self.lower_expr(arg)?;
                    let a = self.auto_deref_cell(a, span);
                    arg_operands.push(Operand::Copy(Place::local(a)));
                }
                let handler_local = self.lower_expr(&args[handler_idx])?;
                match self.emit_router_handler_abi(handler_local, span) {
                    RouterHandlerAbi::Bare(fn_addr) => {
                        arg_operands.push(fn_addr);
                        if let Some(bare_rt) = Self::router_bare_variant(rt) {
                            rt = bare_rt;
                        }
                    }
                    RouterHandlerAbi::WithEnv { env, fn_addr } => {
                        arg_operands.push(env);
                        arg_operands.push(fn_addr);
                    }
                }
            } else {
                for arg in args {
                    let a = self.lower_expr(arg)?;
                    let a = self.auto_deref_cell(a, span);
                    arg_operands.push(Operand::Copy(Place::local(a)));
                }
            }
            let pinned: Ty = match rt {
                "gos_rt_error_message"
                | "gos_rt_bufio_scanner_text"
                | "gos_rt_http_response_body"
                | "gos_rt_http_request_path"
                | "gos_rt_http_request_method"
                | "gos_rt_regex_find"
                | "gos_rt_regex_replace"
                | "gos_rt_regex_replace_all"
                | "gos_rt_flag_set_usage" => self.tcx.string_ty(),
                "gos_rt_error_is"
                | "gos_rt_regex_is_match"
                | "gos_rt_bufio_scanner_scan"
                | "gos_rt_set_insert"
                | "gos_rt_set_contains"
                | "gos_rt_set_remove" => self.tcx.bool_ty(),
                "gos_rt_http_response_status"
                | "gos_rt_set_len"
                | "gos_rt_btmap_len"
                | "gos_rt_btmap_get_or" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_btmap_insert" | "gos_rt_flag_set_short" => self.tcx.unit(),
                "gos_rt_router_add"
                | "gos_rt_router_get"
                | "gos_rt_router_post"
                | "gos_rt_router_put"
                | "gos_rt_router_delete"
                | "gos_rt_router_patch"
                | "gos_rt_router_head"
                | "gos_rt_router_options"
                | "gos_rt_router_add_fn"
                | "gos_rt_router_get_fn"
                | "gos_rt_router_post_fn"
                | "gos_rt_router_put_fn"
                | "gos_rt_router_delete_fn"
                | "gos_rt_router_patch_fn"
                | "gos_rt_router_head_fn"
                | "gos_rt_router_options_fn" => self.tcx.unit(),
                "gos_rt_regex_find_all" | "gos_rt_regex_split" | "gos_rt_flag_set_parse" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_error_cause" => self.option_adt_ty(),
                "gos_rt_sync_map_get" => self.option_string_adt_ty(),
                "gos_rt_sync_map_keys" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_sync_map_len" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_sync_map_contains" => self.tcx.bool_ty(),
                "gos_rt_sync_map_set" | "gos_rt_sync_map_delete" => self.tcx.unit(),
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(pinned);
            // Tag chained dest locals so further method calls
            // dispatch correctly: get/post return Request, send
            // returns Response, header/body return Request again.
            let dest_kind: Option<&'static str> = match rt {
                "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
                "gos_rt_http_request_header" | "gos_rt_http_request_body" => Some("http::Request"),
                "gos_rt_http_request_send" => Some("http::Response"),
                "gos_rt_flag_set_string" => Some("flag::Cell::String"),
                "gos_rt_flag_set_int" => Some("flag::Cell::Int"),
                "gos_rt_flag_set_uint" => Some("flag::Cell::Uint"),
                "gos_rt_flag_set_float" => Some("flag::Cell::Float"),
                "gos_rt_flag_set_bool" => Some("flag::Cell::Bool"),
                "gos_rt_flag_set_duration" => Some("flag::Cell::Duration"),
                "gos_rt_flag_set_string_list" => Some("flag::Cell::StringList"),
                _ => None,
            };
            if let Some(k) = dest_kind {
                self.local_runtime_kind.insert(dest, k);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(rt.to_string())),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // `is_some` / `is_ok` / `is_none` / `is_err`. When the
        // receiver is a `*mut GosResult` Result/Option Adt,
        // dispatch through the runtime helper that reads `disc`.
        // For non-Result receivers (legacy intrinsics that still
        // return raw inner values tagged Result-shaped) fall back
        // to the constant-true/false synthesis so the previous
        // lowering shape is preserved — those call sites assume
        // the happy path is always taken.
        if let name @ ("is_some" | "is_ok" | "is_none" | "is_err") = method.name.as_str() {
            let receiver_local = self.lower_expr(receiver)?;
            let lowered_ty = self.locals[receiver_local.0 as usize].ty;
            let lowered_is_result = matches!(self.tcx.kind_of(lowered_ty), TyKind::Adt { .. })
                && self.is_result_or_option_adt(lowered_ty);
            let recv_is_result = matches!(&receiver_kind_flat, TyKind::Adt { .. })
                && self.is_result_or_option_adt(receiver_ty);
            let bool_ty = self.tcx.bool_ty();
            if lowered_is_result || recv_is_result {
                let helper = match name {
                    "is_some" | "is_ok" => "gos_rt_result_is_ok",
                    _ => "gos_rt_result_is_err",
                };
                let dest = self.fresh(bool_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![Operand::Copy(Place::local(receiver_local))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
            // Legacy: receiver is the inner value with a
            // null/zero sentinel for the missing case.
            let constant = matches!(name, "is_some" | "is_ok");
            let dest = self.fresh(bool_ty);
            self.emit_assign(
                Place::local(dest),
                Rvalue::Use(Operand::Const(ConstValue::Bool(constant))),
                span,
            );
            return Some(dest);
        }

        let receiver_local = self.lower_expr(receiver)?;
        let lowered_kind_dispatch: Option<&'static str> = match (
            self.local_runtime_kind.get(&receiver_local).copied(),
            method.name.as_str(),
        ) {
            (Some("flag::Set"), "string") => Some("gos_rt_flag_set_string"),
            (Some("flag::Set"), "int") => Some("gos_rt_flag_set_int"),
            (Some("flag::Set"), "uint") => Some("gos_rt_flag_set_uint"),
            (Some("flag::Set"), "float") => Some("gos_rt_flag_set_float"),
            (Some("flag::Set"), "bool") => Some("gos_rt_flag_set_bool"),
            (Some("flag::Set"), "duration") => Some("gos_rt_flag_set_duration"),
            (Some("flag::Set"), "string_list") => Some("gos_rt_flag_set_string_list"),
            (Some("flag::Set"), "short") => Some("gos_rt_flag_set_short"),
            (Some("flag::Set"), "usage") => Some("gos_rt_flag_set_usage"),
            (Some("flag::Set"), "parse") => Some("gos_rt_flag_set_parse"),
            // 0.4.0 stateful HTTP types — method-call dispatch.
            (Some("http::Router"), "add") => Some("gos_rt_router_add"),
            (Some("http::Router"), "get") => Some("gos_rt_router_get"),
            (Some("http::Router"), "post") => Some("gos_rt_router_post"),
            (Some("http::Router"), "put") => Some("gos_rt_router_put"),
            (Some("http::Router"), "delete") => Some("gos_rt_router_delete"),
            (Some("http::Router"), "patch") => Some("gos_rt_router_patch"),
            (Some("http::Router"), "head") => Some("gos_rt_router_head"),
            (Some("http::Router"), "options") => Some("gos_rt_router_options"),
            (Some("http::Router"), "serve") => Some("gos_rt_router_serve"),
            (Some("http::FileServer"), "serve") => Some("gos_rt_file_server_serve"),
            (Some("http::NativeClient"), "get") => Some("gos_rt_native_client_get"),
            (Some("http::Proxy"), "forward") => Some("gos_rt_proxy_forward"),
            (Some("http::Client"), "get") => Some("gos_rt_http_client_get"),
            (Some("http::Client"), "post") => Some("gos_rt_http_client_post"),
            (Some("http::Request"), "header") => Some("gos_rt_http_request_header"),
            (Some("http::Request"), "body") => Some("gos_rt_http_request_body"),
            (Some("http::Request"), "send") => Some("gos_rt_http_request_send"),
            (Some("http::Request"), "path") => Some("gos_rt_http_request_path"),
            (Some("http::Request"), "method") => Some("gos_rt_http_request_method"),
            (Some("http::Response"), "status") => Some("gos_rt_http_response_status"),
            (Some("http::Response"), "body") => Some("gos_rt_http_response_body"),
            (Some("bufio::Scanner"), "scan") => Some("gos_rt_bufio_scanner_scan"),
            (Some("bufio::Scanner"), "text") => Some("gos_rt_bufio_scanner_text"),
            (Some("errors::Error"), "message") => Some("gos_rt_error_message"),
            (Some("errors::Error"), "cause") => Some("gos_rt_error_cause"),
            (Some("errors::Error"), "is") => Some("gos_rt_error_is"),
            (Some("regex::Pattern"), "is_match") => Some("gos_rt_regex_is_match"),
            (Some("regex::Pattern"), "find") => Some("gos_rt_regex_find"),
            (Some("regex::Pattern"), "find_all") => Some("gos_rt_regex_find_all"),
            (Some("regex::Pattern"), "replace") => Some("gos_rt_regex_replace"),
            (Some("regex::Pattern"), "replace_all") => Some("gos_rt_regex_replace_all"),
            (Some("regex::Pattern"), "split") => Some("gos_rt_regex_split"),
            (Some("collections::HashSet"), "insert") => Some("gos_rt_set_insert"),
            (Some("collections::HashSet"), "contains") => Some("gos_rt_set_contains"),
            (Some("collections::HashSet"), "remove") => Some("gos_rt_set_remove"),
            (Some("collections::HashSet"), "len") => Some("gos_rt_set_len"),
            (Some("collections::BTreeMap"), "insert") => Some("gos_rt_btmap_insert"),
            (Some("collections::BTreeMap"), "get_or") => Some("gos_rt_btmap_get_or"),
            (Some("collections::BTreeMap"), "len") => Some("gos_rt_btmap_len"),
            (Some("sync::Map"), "set" | "insert") => Some("gos_rt_sync_map_set"),
            (Some("sync::Map"), "get") => Some("gos_rt_sync_map_get"),
            (Some("sync::Map"), "delete" | "remove") => Some("gos_rt_sync_map_delete"),
            (Some("sync::Map"), "len") => Some("gos_rt_sync_map_len"),
            (Some("sync::Map"), "contains" | "contains_key") => Some("gos_rt_sync_map_contains"),
            (Some("sync::Map"), "keys") => Some("gos_rt_sync_map_keys"),
            (Some("vec::Iter"), "next") => Some("gos_rt_arr_iter_next"),
            _ => None,
        };
        if let Some(rt) = lowered_kind_dispatch {
            let mut arg_operands = Vec::with_capacity(args.len() + 1);
            arg_operands.push(Operand::Copy(Place::local(receiver_local)));
            // Router HTTP-verb methods take (router, pattern,
            // env, fn_addr) — synthesize the handler's env+fn_addr
            // from the last user argument (must be a struct whose
            // impl Handler { fn serve(...) }).
            let router_handler_method = matches!(
                rt,
                "gos_rt_router_get"
                    | "gos_rt_router_post"
                    | "gos_rt_router_put"
                    | "gos_rt_router_delete"
                    | "gos_rt_router_patch"
                    | "gos_rt_router_head"
                    | "gos_rt_router_options"
                    | "gos_rt_router_add"
            );
            let mut rt = rt;
            if router_handler_method && !args.is_empty() {
                let handler_idx = args.len() - 1;
                // Lower non-handler args (method-name for add,
                // pattern for verb methods).
                for arg in &args[..handler_idx] {
                    let a = self.lower_expr(arg)?;
                    let a = self.auto_deref_cell(a, span);
                    arg_operands.push(Operand::Copy(Place::local(a)));
                }
                let handler_local = self.lower_expr(&args[handler_idx])?;
                match self.emit_router_handler_abi(handler_local, span) {
                    RouterHandlerAbi::Bare(fn_addr) => {
                        arg_operands.push(fn_addr);
                        if let Some(bare_rt) = Self::router_bare_variant(rt) {
                            rt = bare_rt;
                        }
                    }
                    RouterHandlerAbi::WithEnv { env, fn_addr } => {
                        arg_operands.push(env);
                        arg_operands.push(fn_addr);
                    }
                }
            } else {
                for arg in args {
                    let a = self.lower_expr(arg)?;
                    let a = self.auto_deref_cell(a, span);
                    arg_operands.push(Operand::Copy(Place::local(a)));
                }
            }
            let pinned: Ty = match rt {
                "gos_rt_error_message"
                | "gos_rt_bufio_scanner_text"
                | "gos_rt_http_response_body"
                | "gos_rt_http_request_path"
                | "gos_rt_http_request_method"
                | "gos_rt_regex_find"
                | "gos_rt_regex_replace"
                | "gos_rt_regex_replace_all"
                | "gos_rt_flag_set_usage" => self.tcx.string_ty(),
                "gos_rt_error_is"
                | "gos_rt_regex_is_match"
                | "gos_rt_bufio_scanner_scan"
                | "gos_rt_set_insert"
                | "gos_rt_set_contains"
                | "gos_rt_set_remove" => self.tcx.bool_ty(),
                "gos_rt_http_response_status"
                | "gos_rt_set_len"
                | "gos_rt_btmap_len"
                | "gos_rt_btmap_get_or" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_btmap_insert" | "gos_rt_flag_set_short" => self.tcx.unit(),
                "gos_rt_router_add"
                | "gos_rt_router_get"
                | "gos_rt_router_post"
                | "gos_rt_router_put"
                | "gos_rt_router_delete"
                | "gos_rt_router_patch"
                | "gos_rt_router_head"
                | "gos_rt_router_options"
                | "gos_rt_router_add_fn"
                | "gos_rt_router_get_fn"
                | "gos_rt_router_post_fn"
                | "gos_rt_router_put_fn"
                | "gos_rt_router_delete_fn"
                | "gos_rt_router_patch_fn"
                | "gos_rt_router_head_fn"
                | "gos_rt_router_options_fn" => self.tcx.unit(),
                "gos_rt_regex_find_all" | "gos_rt_regex_split" | "gos_rt_flag_set_parse" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_error_cause" => self.option_adt_ty(),
                "gos_rt_arr_iter_next" => {
                    // Recover element type from the iterator local's MIR
                    // type (pinned to the original Vec<T> by `gos_rt_arr_iter`
                    // dispatch) so `Some(s)` binds `s` with the right type.
                    let mut iter_ty = self.locals[receiver_local.0 as usize].ty;
                    while let TyKind::Ref { inner, .. } = self.tcx.kind_of(iter_ty) {
                        iter_ty = *inner;
                    }
                    let elem_opt = match self.tcx.kind_of(iter_ty) {
                        TyKind::Vec(e) | TyKind::Slice(e) => Some(*e),
                        TyKind::Array { elem, .. } => Some(*elem),
                        _ => None,
                    };
                    if let Some(elem) = elem_opt {
                        let substs = gossamer_types::Substs::from_types([elem]);
                        self.tcx.intern(gossamer_types::TyKind::Adt {
                            def: gossamer_resolve::DefId::local(u32::MAX - 1),
                            substs,
                        })
                    } else if matches!(self.tcx.kind_of(ty), TyKind::Adt { .. }) {
                        ty
                    } else {
                        self.option_adt_ty()
                    }
                }
                "gos_rt_sync_map_get" => self.option_string_adt_ty(),
                "gos_rt_sync_map_keys" => {
                    let s = self.tcx.string_ty();
                    self.tcx.intern(gossamer_types::TyKind::Vec(s))
                }
                "gos_rt_sync_map_len" => self.tcx.int_ty(gossamer_types::IntTy::I64),
                "gos_rt_sync_map_contains" => self.tcx.bool_ty(),
                "gos_rt_sync_map_set" | "gos_rt_sync_map_delete" => self.tcx.unit(),
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(pinned);
            let dest_kind: Option<&'static str> = match rt {
                "gos_rt_http_client_get" | "gos_rt_http_client_post" => Some("http::Request"),
                "gos_rt_http_request_header" | "gos_rt_http_request_body" => Some("http::Request"),
                "gos_rt_http_request_send" => Some("http::Response"),
                "gos_rt_flag_set_string" => Some("flag::Cell::String"),
                "gos_rt_flag_set_int" => Some("flag::Cell::Int"),
                "gos_rt_flag_set_uint" => Some("flag::Cell::Uint"),
                "gos_rt_flag_set_float" => Some("flag::Cell::Float"),
                "gos_rt_flag_set_bool" => Some("flag::Cell::Bool"),
                "gos_rt_flag_set_duration" => Some("flag::Cell::Duration"),
                "gos_rt_flag_set_string_list" => Some("flag::Cell::StringList"),
                _ => None,
            };
            if let Some(k) = dest_kind {
                self.local_runtime_kind.insert(dest, k);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(rt.to_string())),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        let mut arg_operands = Vec::with_capacity(args.len() + 1);
        arg_operands.push(Operand::Copy(Place::local(receiver_local)));
        for arg in args {
            let a = self.lower_expr(arg)?;
            // 0.7.0 flag::Cell auto-deref at the call boundary —
            // mirrors the bytecode VM's auto-unwrap shape so
            // `get_comic(flags.number)` works without `*`.
            let a = self.auto_deref_cell(a, span);
            arg_operands.push(Operand::Copy(Place::local(a)));
        }

        // `gos_rt_result_map_err` / `gos_rt_result_map` expect a
        // closure handle whose first 8 bytes hold the lifted
        // function's address. The HIR lift pass turns
        // non-capturing closures into a bare-name path
        // (`__closure_N`) which lowers to a string-literal pointer
        // — passing that to the helper segfaults the moment it
        // transmutes the first 8 ASCII bytes into a function
        // pointer. Wrap the arg as a 16-byte heap blob
        // `[fn_addr, _]` so the helper's first-word load resolves
        // to the actual lifted function.
        // Dispatch closure args by capture shape. Two distinct
        // ABIs are in play and the runtime helpers separate them
        // explicitly:
        //
        //   - **Capturing closures** lift to `extern "C" fn(env,
        //     payload) -> ret`. The MIR-side LiftedClosure node
        //     produces a heap-allocated env blob whose first slot
        //     is the lifted function's address; the rest are the
        //     captured values. Dispatched through
        //     `gos_rt_result_map` / `_map_err` (env-first ABI).
        //
        //   - **Non-capturing closures** lift to `extern "C" fn
        //     (payload) -> ret` — no env. The HIR lift pass turns
        //     them into a bare `Path` that lowers to a fn-name
        //     constant; `local_fn_name` is then set on the local.
        //     Dispatched through `gos_rt_result_map_bare` /
        //     `_map_err_bare` (no-env ABI), passing the function
        //     address directly.
        //
        // Pre-fix the same `gos_rt_result_map` was used for both,
        // with the call site wrapping the bare fn-pointer in a
        // 16-byte `[fn_addr, _]` blob and praying the C ABI's
        // unused-arg semantics would let the closure's first
        // param shadow the env pointer. On x86_64 it didn't —
        // RDI/RSI assignment matched the helper's perspective
        // (env_ptr, payload), so the closure's `v` param shadowed
        // RDI = env_ptr while the actual payload sat unread in
        // RSI. The closure body then transformed env_ptr instead
        // of payload, which corrupted the resulting Result and
        // produced the askq round-2 strlen-on-bad-pointer crash.
        // See `~/dev/contexts/lang/fix_architecture_ownership.md`
        // root-cause #3.
        if matches!(
            runtime_symbol,
            Some("gos_rt_result_map_err" | "gos_rt_result_map")
        ) && arg_operands.len() == 2
        {
            let closure_local = match &arg_operands[1] {
                Operand::Copy(p) if p.projection.is_empty() => Some(p.local),
                _ => None,
            };
            if let Some(local) = closure_local
                && let Some(fn_name) = self.local_fn_name.get(&local).cloned()
            {
                // Non-capturing path: pass the lifted fn addr as a
                // raw i64 and dispatch through the `_bare` helper
                // that calls it as `f(payload)` — single arg, no
                // env. Switch the dispatched symbol to the bare
                // variant.
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let fn_addr_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(fn_addr_local),
                    Rvalue::CallIntrinsic {
                        name: "gos_fn_addr",
                        args: vec![Operand::Const(ConstValue::Str(fn_name))],
                    },
                    span,
                );
                arg_operands[1] = Operand::Copy(Place::local(fn_addr_local));
                runtime_symbol = match runtime_symbol {
                    Some("gos_rt_result_map") => Some("gos_rt_result_map_bare"),
                    Some("gos_rt_result_map_err") => Some("gos_rt_result_map_err_bare"),
                    other => other,
                };
            }
            // Capturing-closure path: the LiftedClosure lowering
            // already produced an `env_ptr` whose first 8 bytes
            // hold the lifted fn addr. The original
            // `gos_rt_result_map(_err)` env-first dispatch is
            // correct for this shape; nothing to rewrite.
        }

        // Re-check the dispatch for Result/Option methods now that
        // the receiver has been lowered. The HIR-side `receiver_ty`
        // is often a `Var` for chained method calls (e.g.
        // `s.parse().unwrap_or(...)`), so the table at the top
        // selected `Some("")` (identity) without seeing that the
        // pinned local type is in fact a Result/Option Adt. Without
        // this fix-up `.unwrap_or(default)` returns the aggregate
        // pointer instead of the inner payload.
        let lowered_recv_ty = self.locals[receiver_local.0 as usize].ty;
        let lowered_is_result = matches!(self.tcx.kind_of(lowered_recv_ty), TyKind::Adt { .. })
            && self.is_result_or_option_adt(lowered_recv_ty);
        // Inverse of the lowered_is_result fix-up above: if the HIR
        // typechecker thought the receiver was a Result/Option Adt
        // (because the call site chained `.unwrap_or(...)` /
        // `.unwrap()` / `.ok()` / `.err()`) but the lowered MIR type
        // is a real scalar — `json::as_i64(v).unwrap_or(0)` is the
        // canonical case, where `gos_rt_json_as_i64` returns a raw
        // `i64` — fall back to identity. The runtime helpers picked
        // by the original dispatch (`gos_rt_result_unwrap_or` etc.)
        // would treat the i64 as a `*mut GosResult` pointer, read
        // garbage as the `disc`, and return the receiver itself
        // bit-cast as the inner value. The askq tool-call accumulator
        // hit exactly this: every `idx` it computed for a tool_call's
        // `index` field was a multi-trillion garbage number, and the
        // ensuing `while (tc_ids.len() as i64) <= idx` push loop
        // grew the vec to 100+ empty slots before the `[idx] = s`
        // write hit a stale pointer.
        let lowered_kind = self.tcx.kind_of(lowered_recv_ty);
        let lowered_is_scalar = matches!(
            lowered_kind,
            TyKind::Bool | TyKind::Char | TyKind::Int(_) | TyKind::Float(_) | TyKind::String
        );
        // Inverse fix-up: when the lowered receiver is a real scalar
        // (the typechecker thought `json::as_str(v)` returned
        // `Option<&str>` but the runtime helper hands back a raw
        // `*c_char`), force the dispatch to identity so the
        // `gos_rt_result_*` helpers don't dereference the scalar
        // value as a `*mut GosResult`. The askq tool-call name
        // corruption (`json_escape ← strlen_evex`) was the canonical
        // case — see
        // ~/dev/contexts/lang/fix_architecture_ownership.md.
        let mut runtime_symbol = if lowered_is_scalar
            && matches!(
                method.name.as_str(),
                "unwrap" | "unwrap_or" | "ok" | "err" | "expect" | "map" | "map_err"
            ) {
            Some("")
        } else {
            runtime_symbol
        };
        // The `to_string` empty-symbol promotion is only valid for
        // the `.to_string()` method. Without this gate, an inverse
        // fix-up that forces `unwrap_or` on a scalar back to
        // identity (`Some("")`) would accidentally promote to
        // `gos_rt_i64_to_str`, turning `as_i64(v).unwrap_or(0)`
        // into a string render of the i64.
        if matches!(runtime_symbol, Some("")) && method.name.as_str() == "to_string" {
            runtime_symbol = match self.tcx.kind_of(lowered_recv_ty) {
                TyKind::Int(_) => Some("gos_rt_i64_to_str"),
                TyKind::Float(_) => Some("gos_rt_f64_to_str"),
                _ => runtime_symbol,
            };
        }
        if lowered_is_result {
            match method.name.as_str() {
                "unwrap" | "expect" => runtime_symbol = Some("gos_rt_result_unwrap"),
                "unwrap_or" => runtime_symbol = Some("gos_rt_result_unwrap_or"),
                "ok" => runtime_symbol = Some("gos_rt_result_ok"),
                "err" => runtime_symbol = Some("gos_rt_result_err"),
                _ => {}
            }
        }
        // `.clone()` on a Vec/Slice receiver: dispatch to
        // `gos_rt_vec_clone` so the result is a fresh independent
        // `GosVec` allocation rather than a bitwise pointer alias.
        // Without this, `caps[0].clone()` (where `caps[0]` returns an
        // inner `*mut GosVec` pinned to a fresh local) leaves two
        // locals holding the same pointer; the auto-drop pass then
        // emits `gos_rt_vec_free` for each, producing a double free.
        // The top-of-method dispatch table (`runtime_symbol = match
        // method.name.as_str() { … }`) keys on the HIR receiver kind,
        // which is still a `Var` for chained `Index<i>.clone()` shapes
        // — `lowered_recv_ty` is the resolved MIR-side type.
        if method.name.as_str() == "clone"
            && matches!(
                self.tcx.kind_of(lowered_recv_ty),
                TyKind::Vec(_) | TyKind::Slice(_)
            )
        {
            runtime_symbol = Some("gos_rt_vec_clone");
        }

        if let Some(sym) = runtime_symbol {
            return self.dispatch_via_runtime_symbol(
                sym,
                receiver,
                method,
                args,
                ty,
                span,
                receiver_local,
                arg_operands,
            );
        }
        // User-defined `impl` method dispatch: when the receiver's
        // static type names a known struct, look up the mangled
        // method name (`Struct::method`) and emit a direct call
        // with the receiver as the first argument. Mirrors the
        // tree-walker's qualified-method lookup so user code can
        // build natively without rewriting every method as a free
        // function.
        let struct_name = self.struct_name_of(receiver_ty).or_else(|| {
            self.local_struct
                .get(&receiver_local)
                .cloned()
                .or_else(|| self.struct_name_from_expr(receiver))
        });
        if let Some(sname) = struct_name {
            let mangled = format!("{}::{}", sname, method.name);
            // Pin a sensible destination type if HIR left it
            // unresolved. Trait-dispatched method calls
            // (`circle.name()` where `name` is declared on the
            // `Shape` trait) often arrive with the destination ty
            // still an inference variable; use the impl's known
            // return type when available so the codegen sees the
            // real `String` / `f64` / etc. instead of falling
            // back to `i64` and printing the pointer bits.
            let dest_ty = match self.tcx.kind_of(ty) {
                gossamer_types::TyKind::Error | gossamer_types::TyKind::Var(_) => self
                    .impl_methods
                    .get(&mangled)
                    .copied()
                    .flatten()
                    .unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64)),
                _ => ty,
            };
            let dest = self.fresh(dest_ty);
            if let Some(out_struct) = self.struct_name_of(dest_ty) {
                self.local_struct.insert(dest, out_struct);
            }
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(mangled)),
                args: arg_operands,
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }

        // No stdlib helper, no struct-impl match. Emit a generic
        // by-name Call: cranelift's `Const(Str(name))` callee path
        // resolves the symbol via `callees_by_name` (lifted
        // closures, free fns) or falls back to a typed-zero stub
        // for genuinely unknown names. Either branch produces a
        // well-formed CFG, so the build never refuses to lower a
        // method shape we haven't taught the dispatch table about.
        let dest_ty = match self.tcx.kind_of(ty) {
            TyKind::Error | TyKind::Var(_) => self.tcx.int_ty(gossamer_types::IntTy::I64),
            _ => ty,
        };
        let dest = self.fresh(dest_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(method.name.clone())),
            args: arg_operands,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }
}
