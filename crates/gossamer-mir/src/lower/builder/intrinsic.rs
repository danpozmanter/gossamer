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
    pub(crate) fn try_lower_sort_by(
        &mut self,
        receiver: &HirExpr,
        closure_arg: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy, TyKind};
        let recv_place = self.lower_place_expr(receiver)?;
        let recv_ty = self.locals[recv_place.local.0 as usize].ty;
        let recv_kind = match self.tcx.kind_of(recv_ty) {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(*inner).clone(),
            other => other.clone(),
        };
        let elem_ty_concrete = match &recv_kind {
            TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => *elem,
            _ => return None,
        };
        let elem_kind = self.tcx.kind_of(elem_ty_concrete).clone();
        // Single-slot scalar elements (i64, String pointers, bools)
        // sort through the by-value i64 helpers; multi-slot
        // aggregates (Tuple / Adt) sort through the byte-stride
        // helpers that hand the comparator pointers to each
        // element. Arrays-of-T as elements aren't sortable -
        // their content fan-out makes the comparator ABI
        // ambiguous; bail out.
        // `fs::DirInfo` and the other opaque heap-blob / handle stdlib
        // structs (`def.local` in the `u32::MAX - 16 ..= u32::MAX - 2`
        // sentinel range) are single pointer-valued slots, so the
        // comparator receives the value directly like any scalar - the
        // aggregate helper would hand it a pointer to the slot (a pointer
        // to the pointer) and the comparison would read the wrong bytes.
        let elem_is_opaque_handle = matches!(
            elem_kind,
            TyKind::Adt { def, .. } if (u32::MAX - 16..=u32::MAX - 2).contains(&def.local)
        );
        let elem_is_scalar = matches!(
            elem_kind,
            TyKind::Int(IntTy::I64) | TyKind::String | TyKind::Bool
        ) || elem_is_opaque_handle;
        let elem_is_aggregate =
            !elem_is_opaque_handle && matches!(elem_kind, TyKind::Tuple(_) | TyKind::Adt { .. });
        if !elem_is_scalar && !elem_is_aggregate {
            return None;
        }
        let raw_closure_local = self.lower_expr(closure_arg)?;
        // For scalar elements the closure receives values
        // directly. For aggregate elements the cranelift ABI
        // already passes aggregates as pointers (see
        // `cl_type_of`), so declaring the comparator inputs as
        // the concrete element type produces the right shape:
        // the runtime hands two element pointers, the closure
        // body's field-access projections walk off those
        // pointers correctly, no auto-deref needed.
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let elem_ty = if elem_is_scalar || elem_is_aggregate {
            elem_ty_concrete
        } else {
            i64_ty
        };
        let cmp_sig = gossamer_types::FnSig {
            inputs: vec![elem_ty, elem_ty],
            output: i64_ty,
        };
        let cmp_trait_ty = self.tcx.intern(TyKind::FnTrait(cmp_sig));
        let closure_local =
            self.coerce_to_fn_trait_if_needed(raw_closure_local, cmp_trait_ty, span);
        let unit_ty = self.tcx.unit();
        let vec_helper = if elem_is_aggregate {
            "gos_rt_vec_sort_by_aggr"
        } else {
            "gos_rt_vec_sort_by_i64"
        };
        let arr_helper = if elem_is_aggregate {
            "gos_rt_arr_sort_by_aggr"
        } else {
            "gos_rt_arr_sort_by_i64"
        };
        match &recv_kind {
            TyKind::Vec(_) | TyKind::Slice(_) => {
                let dest = self.fresh(unit_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(vec_helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(recv_place.local)),
                        Operand::Copy(Place::local(closure_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(self.lower_unit(span))
            }
            TyKind::Array { len, .. } => {
                let len_local = self.fresh(i64_ty);
                let len_i128 = i128::try_from(len.to_usize()).unwrap_or(0);
                self.emit_assign(
                    Place::local(len_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(len_i128))),
                    span,
                );
                let mut args = vec![
                    Operand::Copy(Place::local(recv_place.local)),
                    Operand::Copy(Place::local(len_local)),
                ];
                if elem_is_aggregate {
                    // Stride helper needs the element width in
                    // bytes so it can advance the cursor between
                    // elements. The bytes value uses the same
                    // `type_slot_bytes` rule as Vec layouts.
                    let bytes_local = self.fresh(i64_ty);
                    let elem_bytes = i128::from(self.type_slot_bytes(elem_ty_concrete).max(8));
                    self.emit_assign(
                        Place::local(bytes_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(elem_bytes))),
                        span,
                    );
                    args.push(Operand::Copy(Place::local(bytes_local)));
                }
                args.push(Operand::Copy(Place::local(closure_local)));
                let dest = self.fresh(unit_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(arr_helper.to_string())),
                    args,
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(self.lower_unit(span))
            }
            _ => None,
        }
    }

    pub(crate) fn try_lower_array_swap(
        &mut self,
        receiver: &HirExpr,
        i_expr: &HirExpr,
        j_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Build a Place that names the receiver as a place
        // expression. Bail out if the receiver isn't an
        // assignable l-value (a path, field, or index chain).
        let recv_place = self.lower_place_expr(receiver)?;
        let i_local = self.lower_expr(i_expr)?;
        let j_local = self.lower_expr(j_expr)?;
        let recv_kind = self
            .tcx
            .kind_of(self.locals[recv_place.local.0 as usize].ty);
        let inner_kind = match recv_kind {
            gossamer_types::TyKind::Ref { inner, .. } => self.tcx.kind_of(*inner).clone(),
            other => other.clone(),
        };
        let is_vec_or_slice = matches!(
            inner_kind,
            gossamer_types::TyKind::Vec(_) | gossamer_types::TyKind::Slice(_)
        );
        let elem_ty = match &inner_kind {
            gossamer_types::TyKind::Array { elem, .. } => *elem,
            gossamer_types::TyKind::Slice(elem) => *elem,
            gossamer_types::TyKind::Vec(elem) => *elem,
            _ => return None,
        };
        if is_vec_or_slice && recv_place.projection.is_empty() {
            // Vec/Slice swap goes through the runtime helpers so
            // the GosVec header isn't mis-treated as a flat
            // element buffer. The previous inline 4-op store
            // wrote into the GosVec header (offset 0 = len) and
            // bubble_sort silently no-op'd or corrupted state.
            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let elem_at_i = self.fresh(i64_ty);
            let next1 = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_i64".to_string())),
                args: vec![
                    Operand::Copy(Place::local(recv_place.local)),
                    Operand::Copy(Place::local(i_local)),
                ],
                destination: Place::local(elem_at_i),
                target: Some(next1),
            });
            self.set_current(next1);
            let elem_at_j = self.fresh(i64_ty);
            let next2 = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_i64".to_string())),
                args: vec![
                    Operand::Copy(Place::local(recv_place.local)),
                    Operand::Copy(Place::local(j_local)),
                ],
                destination: Place::local(elem_at_j),
                target: Some(next2),
            });
            self.set_current(next2);
            let unit_ty = self.tcx.unit();
            let set1 = self.fresh(unit_ty);
            let next3 = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_set_i64".to_string())),
                args: vec![
                    Operand::Copy(Place::local(recv_place.local)),
                    Operand::Copy(Place::local(i_local)),
                    Operand::Copy(Place::local(elem_at_j)),
                ],
                destination: Place::local(set1),
                target: Some(next3),
            });
            self.set_current(next3);
            let set2 = self.fresh(unit_ty);
            let next4 = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_set_i64".to_string())),
                args: vec![
                    Operand::Copy(Place::local(recv_place.local)),
                    Operand::Copy(Place::local(j_local)),
                    Operand::Copy(Place::local(elem_at_i)),
                ],
                destination: Place::local(set2),
                target: Some(next4),
            });
            self.set_current(next4);
            let unit_local = self.lower_unit(span);
            return Some(unit_local);
        }
        let mut at_i = recv_place.clone();
        at_i.projection.push(crate::ir::Projection::Index(i_local));
        let mut at_j = recv_place.clone();
        at_j.projection.push(crate::ir::Projection::Index(j_local));
        let temp_i = self.fresh(elem_ty);
        let temp_j = self.fresh(elem_ty);
        self.emit_assign(
            Place::local(temp_i),
            Rvalue::Use(Operand::Copy(at_i.clone())),
            span,
        );
        self.emit_assign(
            Place::local(temp_j),
            Rvalue::Use(Operand::Copy(at_j.clone())),
            span,
        );
        self.emit_assign(at_i, Rvalue::Use(Operand::Copy(Place::local(temp_j))), span);
        self.emit_assign(at_j, Rvalue::Use(Operand::Copy(Place::local(temp_i))), span);
        let unit_local = self.lower_unit(span);
        Some(unit_local)
    }

    /// `m.insert/get/contains` on a `HashMap` keyed by a flat struct / tuple,
    /// routed to the content-hashing `skey` runtime so two equal-but-distinct
    /// allocations key the same slot (matching the VM). Returns `None` for any
    /// other key shape, leaving the normal pointer-keyed path to run.
    pub(crate) fn try_lower_struct_key_map_op(
        &mut self,
        receiver: &HirExpr,
        op: &str,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let recv_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
        let (key_ty, val_ty) = self.hash_map_kv_tys(recv_ty)?;
        // Only aggregate keys (struct / tuple) content-hash; bare scalar and
        // `String` keys keep their dedicated `_i64` / `_str` fast paths.
        if !self.is_aggregate_key(key_ty) {
            return None;
        }
        let descriptor = self.key_descriptor(key_ty)?;
        let recv_local = self.lower_expr(receiver)?;
        let key_local = self.lower_expr(args.first()?)?;
        let desc_op = Operand::Const(ConstValue::Str(descriptor));
        let (name, dest_ty, val_arg) = match op {
            "insert" if args.len() == 2 => {
                let val_local = self.lower_expr(&args[1])?;
                (
                    "gos_rt_map_insert_skey",
                    self.tcx.unit(),
                    Some(Operand::Copy(Place::local(val_local))),
                )
            }
            "get" if args.len() == 1 => (
                "gos_rt_map_get_skey_opt",
                self.option_payload_adt_ty(val_ty),
                None,
            ),
            "pop" if args.len() == 1 => (
                "gos_rt_map_pop_skey",
                self.option_payload_adt_ty(val_ty),
                None,
            ),
            "contains_key" | "contains" if args.len() == 1 => {
                ("gos_rt_map_contains_skey", self.tcx.bool_ty(), None)
            }
            _ => return None,
        };
        let mut call_args = vec![
            Operand::Copy(Place::local(recv_local)),
            Operand::Copy(Place::local(key_local)),
            desc_op,
        ];
        call_args.extend(val_arg);
        let dest = self.fresh(dest_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name.to_string())),
            args: call_args,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn try_lower_map_inc(
        &mut self,
        outer_recv: &HirExpr,
        outer_key: &HirExpr,
        value_expr: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Binary {
            op: HirBinaryOp::Add,
            lhs,
            rhs,
        } = &value_expr.kind
        else {
            return None;
        };
        let (get_call, by_expr) = if let HirExprKind::MethodCall { name, .. } = &lhs.kind {
            if name.name.as_str() == "get_or" {
                (lhs.as_ref(), rhs.as_ref())
            } else {
                return None;
            }
        } else if let HirExprKind::MethodCall { name, .. } = &rhs.kind {
            if name.name.as_str() == "get_or" {
                (rhs.as_ref(), lhs.as_ref())
            } else {
                return None;
            }
        } else {
            return None;
        };
        let HirExprKind::MethodCall {
            receiver: inner_recv,
            args: get_args,
            ..
        } = &get_call.kind
        else {
            return None;
        };
        if get_args.len() != 2 {
            return None;
        }
        if !exprs_match(outer_recv, inner_recv) || !exprs_match(outer_key, &get_args[0]) {
            return None;
        }
        // Peephole only handles `HashMap<i64, i64>`. The
        // `gos_rt_map_inc_i64` helper takes the key as an i64;
        // forwarding a `*const c_char` here corrupts the lookup.
        // For non-i64 receivers fall through to the general
        // get_or + insert path so the key is hashed correctly.
        let outer_recv_ty = self
            .receiver_local_from_path(outer_recv)
            .map_or(outer_recv.ty, |l| self.locals[l.0 as usize].ty);
        let key_kind = self.hash_map_key_kind(outer_recv_ty);
        let value_kind = self.hash_map_value_kind(outer_recv_ty);
        if !matches!(
            (key_kind, value_kind),
            (Some(MapKeyKind::I64), Some(MapValueKind::I64))
        ) {
            return None;
        }
        let recv_local = self.lower_expr(outer_recv)?;
        let key_local = self.lower_expr(outer_key)?;
        let by_local = self.lower_expr(by_expr)?;
        let dest = self.fresh(ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_map_inc_i64".to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(key_local)),
                Operand::Copy(Place::local(by_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn try_lower_result_map_with_eager_recv(
        &mut self,
        receiver: &HirExpr,
        method: &Ident,
        closure_arg: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let recv_local = self.lower_expr(receiver)?;
        let recv_ty = self.locals[recv_local.0 as usize].ty;
        if !matches!(self.tcx.kind_of(recv_ty), TyKind::Adt { .. })
            || !self.is_result_or_option_adt(recv_ty)
        {
            return None;
        }
        let closure_local = self.lower_expr(closure_arg)?;
        // Wrap a bare `__closure_N` fn-name local into a 16-byte
        // env blob `[fn_addr, _]` so the helper's first-word load
        // resolves to the lifted body. Mirrors the wrap that the
        // generic call dispatch performs.
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let env_local = if let Some(fn_name) = self.local_fn_name.get(&closure_local).cloned() {
            let size_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(size_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(16))),
                span,
            );
            let env = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(env),
                Rvalue::CallIntrinsic {
                    name: "gos_alloc",
                    args: vec![Operand::Copy(Place::local(size_local))],
                },
                span,
            );
            let fn_addr = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(fn_addr),
                Rvalue::CallIntrinsic {
                    name: "gos_fn_addr",
                    args: vec![Operand::Const(ConstValue::Str(fn_name))],
                },
                span,
            );
            let zero_off = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(zero_off),
                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                span,
            );
            let store_dest = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(store_dest),
                Rvalue::CallIntrinsic {
                    name: "gos_store",
                    args: vec![
                        Operand::Copy(Place::local(env)),
                        Operand::Copy(Place::local(zero_off)),
                        Operand::Copy(Place::local(fn_addr)),
                    ],
                },
                span,
            );
            env
        } else {
            closure_local
        };
        let helper = match method.name.as_str() {
            "map_err" => "gos_rt_result_map_err",
            "map" => "gos_rt_result_map",
            _ => return None,
        };
        let dest = self.fresh(recv_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(helper.to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(env_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn try_lower_iter_call(
        &mut self,
        joined: &str,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy, TyKind};
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let unit_ty = self.tcx.unit();

        match (joined, args.len()) {
            // Non-closure constructors / accessors.
            ("iter::count", 1) => {
                let v = self.lower_iter_vec_arg(&args[0])?;
                let dest = self.fresh(i64_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_count".to_string())),
                    args: vec![Operand::Copy(Place::local(v))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::sum", 1) => {
                // Element-type dispatch: f64 vec → sum_f64, otherwise sum_i64.
                let v = self.lower_iter_vec_arg(&args[0])?;
                let elem_is_f64 =
                    matches!(self.iter_element_kind(args[0].ty), Some(TyKind::Float(_)));
                let helper = if elem_is_f64 {
                    "gos_rt_iter_sum_f64"
                } else {
                    "gos_rt_iter_sum_i64"
                };
                let dest_ty = if elem_is_f64 {
                    self.tcx.float_ty(gossamer_types::FloatTy::F64)
                } else {
                    i64_ty
                };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![Operand::Copy(Place::local(v))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::product", 1) => {
                let v = self.lower_iter_vec_arg(&args[0])?;
                let elem_is_f64 =
                    matches!(self.iter_element_kind(args[0].ty), Some(TyKind::Float(_)));
                let helper = if elem_is_f64 {
                    "gos_rt_iter_product_f64"
                } else {
                    "gos_rt_iter_product_i64"
                };
                let dest_ty = if elem_is_f64 {
                    self.tcx.float_ty(gossamer_types::FloatTy::F64)
                } else {
                    i64_ty
                };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![Operand::Copy(Place::local(v))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // Bare prelude `min(xs)` / `max(xs)` over a `Vec`/array return
            // `Option<T>`, exactly like `iter::min`/`iter::max`. The two-arg
            // scalar forms (`min(a, b)`) are handled separately; only the
            // single Vec/array argument reaches here.
            ("iter::min" | "min" | "math::min", 1) => {
                self.lower_iter_simple_vec_i64_opt("gos_rt_iter_min_i64", args, span)
            }
            ("iter::max" | "max" | "math::max", 1) => {
                self.lower_iter_simple_vec_i64_opt("gos_rt_iter_max_i64", args, span)
            }
            ("iter::range", 2) => {
                let a = self.lower_expr(&args[0])?;
                let b = self.lower_expr(&args[1])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest = self.fresh(vec_i64);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_range".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(a)),
                        Operand::Copy(Place::local(b)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::range_inclusive", 2) => {
                let a = self.lower_expr(&args[0])?;
                let b = self.lower_expr(&args[1])?;
                let dest = self.fresh(ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_iter_range_inclusive".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(a)),
                        Operand::Copy(Place::local(b)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::repeat", 2) => {
                let v = self.lower_expr(&args[0])?;
                let n = self.lower_expr(&args[1])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest = self.fresh(vec_i64);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_repeat_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(v)),
                        Operand::Copy(Place::local(n)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::take", 2) => {
                let n = self.lower_expr(&args[0])?;
                let v = self.lower_iter_vec_arg(&args[1])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest = self.fresh(vec_i64);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_take_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(n)),
                        Operand::Copy(Place::local(v)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::skip", 2) => {
                let n = self.lower_expr(&args[0])?;
                let v = self.lower_iter_vec_arg(&args[1])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest = self.fresh(vec_i64);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_skip_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(n)),
                        Operand::Copy(Place::local(v)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::rev", 1) => {
                self.lower_iter_simple_vec_in_vec_out("gos_rt_iter_reversed_i64", args, ty, span)
            }
            ("iter::chain", 2) => {
                let a = self.lower_iter_vec_arg(&args[0])?;
                let b = self.lower_iter_vec_arg(&args[1])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest = self.fresh(vec_i64);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_chain_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(a)),
                        Operand::Copy(Place::local(b)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::dedup", 1) => {
                self.lower_iter_simple_vec_in_vec_out("gos_rt_iter_dedup_i64", args, ty, span)
            }
            ("iter::flatten", 1) => {
                let vec_local = self.lower_iter_vec_arg(&args[0])?;
                let dest_ty = self.tcx.intern(TyKind::Vec(i64_ty));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_flatten_i64",
                    vec![Operand::Copy(Place::local(vec_local))],
                    dest_ty,
                    span,
                ))
            }
            ("iter::enumerate", 1) => {
                let vec_local = self.lower_iter_vec_arg(&args[0])?;
                let pair = self.tcx.intern(TyKind::Tuple(vec![i64_ty, i64_ty]));
                let dest_ty = self.tcx.intern(TyKind::Vec(pair));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_enumerate_i64",
                    vec![Operand::Copy(Place::local(vec_local))],
                    dest_ty,
                    span,
                ))
            }
            ("iter::zip", 2) => {
                let a = self.lower_iter_vec_arg(&args[0])?;
                let b = self.lower_iter_vec_arg(&args[1])?;
                let pair = self.tcx.intern(TyKind::Tuple(vec![i64_ty, i64_ty]));
                let dest_ty = self.tcx.intern(TyKind::Vec(pair));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_zip_i64",
                    vec![
                        Operand::Copy(Place::local(a)),
                        Operand::Copy(Place::local(b)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::pairwise", 1) => {
                let vec_local = self.lower_iter_vec_arg(&args[0])?;
                let pair = self.tcx.intern(TyKind::Tuple(vec![i64_ty, i64_ty]));
                let dest_ty = self.tcx.intern(TyKind::Vec(pair));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_pairwise_i64",
                    vec![Operand::Copy(Place::local(vec_local))],
                    dest_ty,
                    span,
                ))
            }
            ("iter::windows", 2) => {
                let n = self.lower_expr(&args[0])?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let inner = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest_ty = self.tcx.intern(TyKind::Vec(inner));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_windowed_i64",
                    vec![
                        Operand::Copy(Place::local(n)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::chunks", 2) => {
                let n = self.lower_expr(&args[0])?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let inner = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest_ty = self.tcx.intern(TyKind::Vec(inner));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_chunk_by_size_i64",
                    vec![
                        Operand::Copy(Place::local(n)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::unzip", 1) => {
                let vec_local = self.lower_iter_vec_arg(&args[0])?;
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                let dest_ty = self.tcx.intern(TyKind::Tuple(vec![vec_i64, vec_i64]));
                Some(self.emit_combinator_call(
                    "gos_rt_iter_unzip_i64",
                    vec![Operand::Copy(Place::local(vec_local))],
                    dest_ty,
                    span,
                ))
            }
            // Closure-taking helpers. Args are `(f, ..., xs)`; coerce
            // the closure to its callback FnTrait shape so the
            // unified callable infra ships an env pointer with the
            // body address at env[0].
            ("iter::for_each", 2) => {
                let f64_ty = self.tcx.float_ty(gossamer_types::FloatTy::F64);
                let elem_is_f64 =
                    matches!(self.iter_element_kind(args[1].ty), Some(TyKind::Float(_)));
                let in_ty = if elem_is_f64 { f64_ty } else { i64_ty };
                let helper = if elem_is_f64 {
                    "gos_rt_iter_for_each_f64"
                } else {
                    "gos_rt_iter_for_each_i64"
                };
                let closure_local = self.lower_iter_closure(&args[0], &[in_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest = self.fresh(unit_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(self.lower_unit(span))
            }
            ("iter::map", 2) => {
                // Route an f64-element map through the float-ABI shim + closure
                // so the element rides an SSE register; a hardcoded i64 sig
                // would hand the closure integer-register bits it reads as a
                // garbage double. The output shape stays the closure's own
                // return type so a `[f64] -> [i64]` map (or the reverse) is
                // typed correctly.
                let f64_ty = self.tcx.float_ty(gossamer_types::FloatTy::F64);
                let elem_is_f64 =
                    matches!(self.iter_element_kind(args[1].ty), Some(TyKind::Float(_)));
                let out_ty = self
                    .iter_element_kind(ty)
                    .map_or(i64_ty, |k| self.tcx.intern(k));
                let out_is_f64 = matches!(self.tcx.kind_of(out_ty), TyKind::Float(_));
                let (in_ty, helper) = match (elem_is_f64, out_is_f64) {
                    (true, true) => (f64_ty, "gos_rt_iter_map_f64"),
                    (true, false) => (f64_ty, "gos_rt_iter_map_f64_word"),
                    (false, true) => (i64_ty, "gos_rt_iter_map_word_f64"),
                    (false, false) => (i64_ty, "gos_rt_iter_map_i64"),
                };
                let closure_local = self.lower_iter_closure(&args[0], &[in_ty], out_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest = self.fresh(ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::filter", 2) => {
                let bool_ty = self.tcx.bool_ty();
                let f64_ty = self.tcx.float_ty(gossamer_types::FloatTy::F64);
                let elem_is_f64 =
                    matches!(self.iter_element_kind(args[1].ty), Some(TyKind::Float(_)));
                let in_ty = if elem_is_f64 { f64_ty } else { i64_ty };
                let helper = if elem_is_f64 {
                    "gos_rt_iter_filter_f64"
                } else {
                    "gos_rt_iter_filter_i64"
                };
                let closure_local = self.lower_iter_closure(&args[0], &[in_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest = self.fresh(ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::fold", 3) => {
                let init_local = self.lower_expr(&args[0])?;
                let closure_local =
                    self.lower_iter_closure(&args[1], &[i64_ty, i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[2])?;
                let dest = self.fresh(i64_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_fold_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(init_local)),
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::sum_by", 2) => {
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest = self.fresh(i64_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_sum_by_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::any", 2) => {
                let bool_ty = self.tcx.bool_ty();
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                // Bool-typed destination so `{}` renders true/false
                // like the VM; the shim returns i64 0/1.
                let dest = self.fresh(bool_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_any_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::all", 2) => {
                let bool_ty = self.tcx.bool_ty();
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                // Bool-typed destination so `{}` renders true/false
                // like the VM; the shim returns i64 0/1.
                let dest = self.fresh(bool_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_all_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            ("iter::find", 2) => {
                // Build an Option<i64> from a (flag, value) pair so
                // pattern matching on the result keeps working.
                let bool_ty = self.tcx.bool_ty();
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let value = self.fresh(i64_ty);
                let after_value = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_iter_find_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(value),
                    target: Some(after_value),
                });
                self.set_current(after_value);
                let flag = self.fresh(i64_ty);
                let after_flag = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_iter_find_i64_flag".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    destination: Place::local(flag),
                    target: Some(after_flag),
                });
                self.set_current(after_flag);
                // Convert flag (0/1) → disc (0 for Some, 1 for None).
                let disc = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(disc),
                    Rvalue::BinaryOp {
                        op: crate::BinOp::Sub,
                        lhs: Operand::Const(ConstValue::Int(1)),
                        rhs: Operand::Copy(Place::local(flag)),
                    },
                    span,
                );
                let rty = self.result_repr_ty(ty);
                let dest = self.fresh(rty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::CallIntrinsic {
                        name: "gos_rt_result_new",
                        args: vec![
                            Operand::Copy(Place::local(disc)),
                            Operand::Copy(Place::local(value)),
                        ],
                    },
                    span,
                );
                Some(dest)
            }
            _ => None,
        }
    }

    /// Destination type for `option::unwrap_or` / `result::unwrap_or`:
    /// the call expression's HIR type when concrete, else the payload
    /// type (`substs[0]`) recovered from the scrutinee's HIR or MIR
    /// type, else i64. The call-expression type is often still an
    /// inference Var here, and the dest must keep a heap payload's
    /// real type so the drop / fmt machinery sees e.g. a String
    /// rather than a raw i64.
    fn unwrap_default_dest_ty(
        &mut self,
        expr_ty: Ty,
        scrutinee_hir_ty: Ty,
        scrutinee_local: Local,
    ) -> Ty {
        use gossamer_types::TyKind;
        let concrete = |t: Ty| {
            !matches!(
                self.tcx.kind_of(t),
                TyKind::Var(_) | TyKind::Error | TyKind::Never
            )
        };
        if concrete(expr_ty) {
            return expr_ty;
        }
        let sources = [scrutinee_hir_ty, self.locals[scrutinee_local.0 as usize].ty];
        for src in sources {
            if let TyKind::Adt { substs, .. } = self.tcx.kind_of(src) {
                if let Some(payload) = substs.types().first().copied() {
                    if concrete(payload) {
                        return payload;
                    }
                }
            }
        }
        self.tcx.int_ty(gossamer_types::IntTy::I64)
    }

    pub(crate) fn try_lower_option_call(
        &mut self,
        joined: &str,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::IntTy;
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        match (joined, args.len()) {
            ("option::is_some", 1) => {
                self.lower_combinator_pred_call("gos_rt_option_is_some", args, span)
            }
            ("option::is_none", 1) => {
                self.lower_combinator_pred_call("gos_rt_option_is_none", args, span)
            }
            ("option::unwrap_or", 2) => {
                let fallback = self.lower_expr(&args[0])?;
                let opt = self.lower_expr(&args[1])?;
                let dest_ty = self.unwrap_default_dest_ty(ty, args[1].ty, opt);
                let helper =
                    if matches!(self.tcx.kind_of(dest_ty), gossamer_types::TyKind::Float(_)) {
                        "gos_rt_option_default_f64"
                    } else {
                        "gos_rt_option_default_i64"
                    };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(fallback)),
                        Operand::Copy(Place::local(opt)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // `option::map(f, opt) -> Option<U>`. Closure-arg first
            // (Gossamer's data-last `|>` syntactic-sugar passes the
            // pipe value as the *trailing* arg), opt second. Builds
            // a fresh Option packed in `*mut GosResult` with disc=0
            // for Some(mapped) and disc=1 for None passthrough.
            ("option::map", 2) => {
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let opt_local = self.lower_expr(&args[1])?;
                let opt_ty = {
                    let substs = gossamer_types::Substs::from_types([i64_ty]);
                    self.tcx.intern(gossamer_types::TyKind::Adt {
                        def: gossamer_resolve::DefId::local(u32::MAX - 1),
                        substs,
                    })
                };
                let dest = self.fresh(opt_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_option_map_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(opt_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // `result::unwrap_or(v, res) -> T`. Data-last pipe: the
            // fallback value is arg 0, the Result arg 1. Returns the
            // `Ok` payload, or the fallback when the Result is `Err`.
            ("result::unwrap_or", 2) => {
                let fallback = self.lower_expr(&args[0])?;
                let res_local = self.lower_expr(&args[1])?;
                let dest_ty = self.unwrap_default_dest_ty(ty, args[1].ty, res_local);
                let helper =
                    if matches!(self.tcx.kind_of(dest_ty), gossamer_types::TyKind::Float(_)) {
                        "gos_rt_result_default_f64"
                    } else {
                        "gos_rt_result_default"
                    };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![
                        Operand::Copy(Place::local(fallback)),
                        Operand::Copy(Place::local(res_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // `result::unwrap_or_else(f, res) -> T`. Data-last pipe: the
            // closure is arg 0, the Result arg 1. Returns the `Ok`
            // value, or the closure applied to the `Err` payload.
            ("result::unwrap_or_else", 2) => {
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let res_local = self.lower_expr(&args[1])?;
                let dest_ty = if matches!(
                    self.tcx.kind_of(ty),
                    gossamer_types::TyKind::Var(_)
                        | gossamer_types::TyKind::Error
                        | gossamer_types::TyKind::Never
                ) {
                    i64_ty
                } else {
                    ty
                };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_result_default_with".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(res_local)),
                        Operand::Copy(Place::local(closure_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // `result::map_err(f, res) -> Result<T, F>`. Data-last
            // pipe: closure first, Result second. Routes through the
            // same env-first shim the method form uses; Ok passes
            // through unchanged.
            ("result::map_err", 2) => {
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let res_local = self.lower_expr(&args[1])?;
                let dest_ty = if matches!(
                    self.tcx.kind_of(ty),
                    gossamer_types::TyKind::Var(_)
                        | gossamer_types::TyKind::Error
                        | gossamer_types::TyKind::Never
                ) {
                    let err_ty = self.tcx.dyn_error_ty();
                    let substs = gossamer_types::Substs::from_types([i64_ty, err_ty]);
                    self.tcx.intern(gossamer_types::TyKind::Adt {
                        def: gossamer_resolve::DefId::local(u32::MAX),
                        substs,
                    })
                } else {
                    ty
                };
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_result_map_err".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(res_local)),
                        Operand::Copy(Place::local(closure_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            // `result::map(f, res) -> Result<U, E>`. Same shape as
            // `option::map`; Err passes through unchanged.
            ("result::map", 2) => {
                let closure_local = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let res_local = self.lower_expr(&args[1])?;
                let err_ty = self.tcx.dyn_error_ty();
                let substs = gossamer_types::Substs::from_types([i64_ty, err_ty]);
                let res_ty = self.tcx.intern(gossamer_types::TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                });
                let dest = self.fresh(res_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_result_map_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(closure_local)),
                        Operand::Copy(Place::local(res_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                Some(dest)
            }
            _ => None,
        }
    }

    /// Method-form chain combinators on a Result/Option receiver:
    /// `x.and_then(f)` / `or_else(f)` / `filter(p)` / `ok_or_else(f)`.
    /// Mirrors the data-last free forms below: the closure crosses the
    /// C-ABI as the env-blob `lower_iter_closure` builds.
    pub(crate) fn lower_variant_chain_method(
        &mut self,
        receiver: &HirExpr,
        method: &Ident,
        closure_arg: &HirExpr,
        receiver_ty: Ty,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy, TyKind};
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let is_option = self.is_option_adt(receiver_ty);
        let helper = match (method.name.as_str(), is_option) {
            ("and_then", true) => "gos_rt_option_and_then",
            ("and_then", false) => "gos_rt_result_and_then",
            ("or_else", true) => "gos_rt_option_or_else",
            ("or_else", false) => "gos_rt_result_or_else",
            ("filter", true) => "gos_rt_option_filter",
            ("ok_or_else", _) => "gos_rt_result_ok_or_else",
            _ => return None,
        };
        // Closure parameter shape: `and_then`/`filter` and Result's
        // `or_else` receive the payload word; Option's `or_else` and
        // `ok_or_else` are nullary thunks.
        let inputs: &[Ty] = match (method.name.as_str(), is_option) {
            ("or_else", true) | ("ok_or_else", _) => &[],
            _ => &[i64_ty],
        };
        let closure_out = match method.name.as_str() {
            "filter" => self.tcx.bool_ty(),
            "ok_or_else" => i64_ty,
            _ if is_option => {
                let payload = self.enum_payload_ty(receiver_ty, 0).unwrap_or(i64_ty);
                self.option_payload_adt_ty(payload)
            }
            _ => self.result_i64_error_adt_ty(),
        };
        let recv = self.lower_expr(receiver)?;
        let closure = self.lower_iter_closure(closure_arg, inputs, closure_out, span)?;
        let checked = if matches!(
            self.tcx.kind_of(ty),
            TyKind::Var(_) | TyKind::Error | TyKind::Never
        ) {
            receiver_ty
        } else {
            ty
        };
        let dest_ty = self.result_repr_ty(checked);
        Some(self.emit_combinator_call(
            helper,
            vec![
                Operand::Copy(Place::local(recv)),
                Operand::Copy(Place::local(closure)),
            ],
            dest_ty,
            span,
        ))
    }

    /// Lowers the closure-taking std combinators wired natively in the
    /// Task-22 pass: `result::and_then`/`or_else`/`ok`/`err`/`is_ok`/
    /// `is_err`, the remaining `option::*` family, and the newer
    /// closure-taking `iter::*` entries. Free data-last call shapes
    /// only; returns `None` for any other name so the generic call
    /// path keeps running.
    pub(crate) fn try_lower_combinator_call(
        &mut self,
        joined: &str,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy, TyKind};
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let bool_ty = self.tcx.bool_ty();
        match (joined, args.len()) {
            ("result::and_then" | "result::or_else", 2) => {
                let out_ty = self.result_i64_error_adt_ty();
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], out_ty, span)?;
                let res = self.lower_expr(&args[1])?;
                let dest_ty = self.result_repr_ty(
                    if matches!(
                        self.tcx.kind_of(ty),
                        TyKind::Var(_) | TyKind::Error | TyKind::Never
                    ) {
                        args[1].ty
                    } else {
                        ty
                    },
                );
                let helper = if joined == "result::and_then" {
                    "gos_rt_result_and_then"
                } else {
                    "gos_rt_result_or_else"
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(res)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("result::ok" | "result::err", 1) => {
                let res = self.lower_expr(&args[0])?;
                let slot = usize::from(joined == "result::err");
                let payload = self.enum_payload_ty(args[0].ty, slot).unwrap_or(i64_ty);
                let dest_ty = self.option_payload_adt_ty(payload);
                let helper = if joined == "result::ok" {
                    "gos_rt_result_to_opt_ok"
                } else {
                    "gos_rt_result_to_opt_err"
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![Operand::Copy(Place::local(res))],
                    dest_ty,
                    span,
                ))
            }
            ("result::is_ok", 1) => {
                self.lower_combinator_pred_call("gos_rt_result_is_ok", args, span)
            }
            ("result::is_err", 1) => {
                self.lower_combinator_pred_call("gos_rt_result_is_err", args, span)
            }
            ("option::and_then" | "option::or_else", 2) => {
                let payload = self.enum_payload_ty(args[1].ty, 0).unwrap_or(i64_ty);
                let out_ty = self.option_payload_adt_ty(payload);
                let inputs: &[Ty] = if joined == "option::and_then" {
                    &[i64_ty]
                } else {
                    &[]
                };
                let closure = self.lower_iter_closure(&args[0], inputs, out_ty, span)?;
                let opt = self.lower_expr(&args[1])?;
                let helper = if joined == "option::and_then" {
                    "gos_rt_option_and_then"
                } else {
                    "gos_rt_option_or_else"
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(opt)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    out_ty,
                    span,
                ))
            }
            ("option::filter", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let opt = self.lower_expr(&args[1])?;
                let payload = self.enum_payload_ty(args[1].ty, 0).unwrap_or(i64_ty);
                let dest_ty = self.option_payload_adt_ty(payload);
                Some(self.emit_combinator_call(
                    "gos_rt_option_filter",
                    vec![
                        Operand::Copy(Place::local(opt)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("option::or", 2) => {
                let alt = self.lower_expr(&args[0])?;
                let opt = self.lower_expr(&args[1])?;
                let payload = self.enum_payload_ty(args[1].ty, 0).unwrap_or(i64_ty);
                let dest_ty = self.option_payload_adt_ty(payload);
                Some(self.emit_combinator_call(
                    "gos_rt_option_or",
                    vec![
                        Operand::Copy(Place::local(alt)),
                        Operand::Copy(Place::local(opt)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("sync::Once::call" | "Once::call", 2) => {
                // `Once::call(o, || ...)` - handle first, nullary closure
                // second. The closure crosses the C-ABI through the same
                // env-thunk convention as `option::unwrap_or_else`; the run
                // body's value is ignored (the i64 result is the ran flag).
                let handle = self.lower_expr(&args[0])?;
                let closure = self.lower_iter_closure(&args[1], &[], i64_ty, span)?;
                Some(self.emit_combinator_call(
                    "gos_rt_once_call",
                    vec![
                        Operand::Copy(Place::local(handle)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    i64_ty,
                    span,
                ))
            }
            ("middleware::bearer_ok" | "http::middleware::bearer_ok", 2) => {
                // `bearer_ok(req, verify)` - request first, a
                // String-taking verify closure second. Mirrors the
                // VM-native `native_bearer_ok`; the closure runs on the
                // extracted Bearer token and its bool result is returned
                // (false, without calling verify, when no Bearer header
                // is present).
                let string_ty = self.tcx.string_ty();
                let req = self.lower_expr(&args[0])?;
                let verify = self.lower_iter_closure(&args[1], &[string_ty], bool_ty, span)?;
                Some(self.emit_combinator_call(
                    "gos_rt_http_bearer_ok",
                    vec![
                        Operand::Copy(Place::local(req)),
                        Operand::Copy(Place::local(verify)),
                    ],
                    bool_ty,
                    span,
                ))
            }
            ("sync::RwLock::with_read" | "RwLock::with_read", 2) => {
                // `RwLock::with_read(lock, |v| ...)` - handle first, an
                // i64-taking closure second. Mirrors the VM-native
                // `native_rwlock_with_read`; the callback runs under a
                // read lock and its result is returned unchanged.
                let handle = self.lower_expr(&args[0])?;
                let closure = self.lower_iter_closure(&args[1], &[i64_ty], i64_ty, span)?;
                Some(self.emit_combinator_call(
                    "gos_rt_rwlock_with_read",
                    vec![
                        Operand::Copy(Place::local(handle)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    i64_ty,
                    span,
                ))
            }
            ("sync::RwLock::with_write" | "RwLock::with_write", 2) => {
                // `RwLock::with_write(lock, |v| ...)` - the callback runs
                // under a write lock and its result becomes the new
                // guarded value, which is also returned.
                let handle = self.lower_expr(&args[0])?;
                let closure = self.lower_iter_closure(&args[1], &[i64_ty], i64_ty, span)?;
                Some(self.emit_combinator_call(
                    "gos_rt_rwlock_with_write",
                    vec![
                        Operand::Copy(Place::local(handle)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    i64_ty,
                    span,
                ))
            }
            ("option::unwrap_or_else", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[], i64_ty, span)?;
                let opt = self.lower_expr(&args[1])?;
                let dest_ty = self.unwrap_default_dest_ty(ty, args[1].ty, opt);
                Some(self.emit_combinator_call(
                    "gos_rt_option_default_with",
                    vec![
                        Operand::Copy(Place::local(opt)),
                        Operand::Copy(Place::local(closure)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("option::zip", 2) => {
                let first = self.lower_expr(&args[0])?;
                let second = self.lower_expr(&args[1])?;
                let a = self.enum_payload_ty(args[0].ty, 0).unwrap_or(i64_ty);
                let b = self.enum_payload_ty(args[1].ty, 0).unwrap_or(i64_ty);
                let pair = self.tcx.intern(TyKind::Tuple(vec![a, b]));
                let dest_ty = self.option_payload_adt_ty(pair);
                Some(self.emit_combinator_call(
                    "gos_rt_option_zip",
                    vec![
                        Operand::Copy(Place::local(first)),
                        Operand::Copy(Place::local(second)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("option::flatten", 1) => {
                let opt = self.lower_expr(&args[0])?;
                let inner = self.enum_payload_ty(args[0].ty, 0).unwrap_or(i64_ty);
                let dest_ty = self.result_repr_ty(inner);
                Some(self.emit_combinator_call(
                    "gos_rt_option_flatten",
                    vec![Operand::Copy(Place::local(opt))],
                    dest_ty,
                    span,
                ))
            }
            ("option::iter", 1) => {
                let opt = self.lower_expr(&args[0])?;
                let payload = self.enum_payload_ty(args[0].ty, 0).unwrap_or(i64_ty);
                let dest_ty = self.tcx.intern(TyKind::Vec(payload));
                Some(self.emit_combinator_call(
                    "gos_rt_option_iter",
                    vec![Operand::Copy(Place::local(opt))],
                    dest_ty,
                    span,
                ))
            }
            ("iter::filter_map" | "iter::find_map", 2) => {
                let opt_i64 = self.option_payload_adt_ty(i64_ty);
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], opt_i64, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let (helper, dest_ty) = if joined == "iter::filter_map" {
                    let dest = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                        ty
                    } else {
                        self.tcx.intern(TyKind::Vec(i64_ty))
                    };
                    ("gos_rt_iter_filter_map_i64", dest)
                } else {
                    ("gos_rt_iter_find_map_i64", opt_i64)
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::flat_map", 2) => {
                let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                // A callback returning a fixed-size array hands back a
                // raw slot buffer with no GosVec header; route those
                // through the arr-variant shim with the static length.
                let arr_len = match self.callable_output_of(&args[0]) {
                    Some(out) => match self.tcx.kind_of(out) {
                        TyKind::Array { len, .. } => Some(*len),
                        _ => None,
                    },
                    None => None,
                };
                let cb_out = match arr_len {
                    Some(_) => self
                        .callable_output_of(&args[0])
                        .expect("arr_len derived from callable output"),
                    None => vec_i64,
                };
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], cb_out, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest_ty = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                    ty
                } else {
                    vec_i64
                };
                let mut call_args = vec![
                    Operand::Copy(Place::local(closure)),
                    Operand::Copy(Place::local(vec_local)),
                ];
                let helper = if let Some(len) = arr_len {
                    let len_local = self.fresh(i64_ty);
                    let len_i128 = i128::try_from(len.to_usize()).unwrap_or(0);
                    self.emit_assign(
                        Place::local(len_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(len_i128))),
                        span,
                    );
                    call_args.push(Operand::Copy(Place::local(len_local)));
                    "gos_rt_iter_flat_map_arr_i64"
                } else {
                    "gos_rt_iter_flat_map_i64"
                };
                Some(self.emit_combinator_call(helper, call_args, dest_ty, span))
            }
            ("iter::reduce", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty, i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest_ty = self.option_payload_adt_ty(i64_ty);
                Some(self.emit_combinator_call(
                    "gos_rt_iter_reduce_i64",
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::scan", 3) => {
                let init = self.lower_expr(&args[0])?;
                let closure = self.lower_iter_closure(&args[1], &[i64_ty, i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[2])?;
                let dest_ty = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                    ty
                } else {
                    self.tcx.intern(TyKind::Vec(i64_ty))
                };
                Some(self.emit_combinator_call(
                    "gos_rt_iter_scan_i64",
                    vec![
                        Operand::Copy(Place::local(init)),
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::product_by", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                Some(self.emit_combinator_call(
                    "gos_rt_iter_product_by_i64",
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    i64_ty,
                    span,
                ))
            }
            ("iter::position", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest_ty = self.option_payload_adt_ty(i64_ty);
                Some(self.emit_combinator_call(
                    "gos_rt_iter_position_i64",
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::take_while" | "iter::skip_while", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest_ty = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                    ty
                } else {
                    self.tcx.intern(TyKind::Vec(i64_ty))
                };
                let helper = if joined == "iter::take_while" {
                    "gos_rt_iter_take_while_i64"
                } else {
                    "gos_rt_iter_skip_while_i64"
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::partition", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], bool_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let dest_ty = if matches!(self.tcx.kind_of(ty), TyKind::Tuple(_)) {
                    ty
                } else {
                    let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                    self.tcx.intern(TyKind::Tuple(vec![vec_i64, vec_i64]))
                };
                Some(self.emit_combinator_call(
                    "gos_rt_iter_partition_i64",
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::sort_by" | "iter::min_by" | "iter::max_by", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty, i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let (helper, dest_ty) = match joined {
                    "iter::sort_by" => {
                        let dest = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                            ty
                        } else {
                            self.tcx.intern(TyKind::Vec(i64_ty))
                        };
                        ("gos_rt_iter_sorted_by_i64", dest)
                    }
                    "iter::min_by" => {
                        ("gos_rt_iter_min_by_i64", self.option_payload_adt_ty(i64_ty))
                    }
                    _ => ("gos_rt_iter_max_by_i64", self.option_payload_adt_ty(i64_ty)),
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::sort_by_key" | "iter::min_by_key" | "iter::max_by_key", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let (helper, dest_ty) = match joined {
                    "iter::sort_by_key" => {
                        let dest = if matches!(self.tcx.kind_of(ty), TyKind::Vec(_)) {
                            ty
                        } else {
                            self.tcx.intern(TyKind::Vec(i64_ty))
                        };
                        ("gos_rt_iter_sorted_by_key_i64", dest)
                    }
                    "iter::min_by_key" => (
                        "gos_rt_iter_min_by_key_i64",
                        self.option_payload_adt_ty(i64_ty),
                    ),
                    _ => (
                        "gos_rt_iter_max_by_key_i64",
                        self.option_payload_adt_ty(i64_ty),
                    ),
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            ("iter::chunk_by" | "iter::count_by", 2) => {
                let closure = self.lower_iter_closure(&args[0], &[i64_ty], i64_ty, span)?;
                let vec_local = self.lower_iter_vec_arg(&args[1])?;
                let (helper, dest_ty) = if joined == "iter::chunk_by" {
                    let dest = if matches!(self.tcx.kind_of(ty), TyKind::HashMap { .. }) {
                        ty
                    } else {
                        let vec_i64 = self.tcx.intern(TyKind::Vec(i64_ty));
                        self.tcx.intern(TyKind::HashMap {
                            key: i64_ty,
                            value: vec_i64,
                        })
                    };
                    ("gos_rt_iter_group_by_i64", dest)
                } else {
                    let dest = if matches!(self.tcx.kind_of(ty), TyKind::HashMap { .. }) {
                        ty
                    } else {
                        self.tcx.intern(TyKind::HashMap {
                            key: i64_ty,
                            value: i64_ty,
                        })
                    };
                    ("gos_rt_iter_count_by_i64", dest)
                };
                Some(self.emit_combinator_call(
                    helper,
                    vec![
                        Operand::Copy(Place::local(closure)),
                        Operand::Copy(Place::local(vec_local)),
                    ],
                    dest_ty,
                    span,
                ))
            }
            _ => None,
        }
    }

    /// Declared output type of a callable-shaped argument expression
    /// (`FnPtr` / `FnTrait` sig, or a lifted closure's registered
    /// return type), or `None` when the shape is unknown.
    fn callable_output_of(&self, arg: &HirExpr) -> Option<Ty> {
        use gossamer_types::TyKind;
        match self.tcx.kind_of(arg.ty) {
            TyKind::FnPtr(sig) | TyKind::FnTrait(sig) => Some(sig.output),
            TyKind::FnDef { def, .. } => self.fn_returns.get(def).copied(),
            _ => None,
        }
    }

    /// `substs[idx]` of a Result/Option-shaped `ty`, ref-transparent;
    /// `None` when the type is not a resolved enum Adt or the payload
    /// slot is still an inference Var.
    fn enum_payload_ty(&self, ty: Ty, idx: usize) -> Option<Ty> {
        use gossamer_types::TyKind;
        let mut resolved = ty;
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(resolved) {
            resolved = *inner;
        }
        match self.tcx.kind_of(resolved) {
            TyKind::Adt { def, substs } if def.local == u32::MAX || def.local == u32::MAX - 1 => {
                let payload = substs.types().get(idx).copied()?;
                if matches!(
                    self.tcx.kind_of(payload),
                    TyKind::Var(_) | TyKind::Error | TyKind::Never
                ) {
                    None
                } else {
                    Some(payload)
                }
            }
            _ => None,
        }
    }

    /// Emits a call to `helper` and returns the destination local.
    fn emit_combinator_call(
        &mut self,
        helper: &str,
        args: Vec<Operand>,
        dest_ty: Ty,
        span: Span,
    ) -> Local {
        let dest = self.fresh(dest_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(helper.to_string())),
            args,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    /// Lowers a 1-arg Result/Option predicate (`is_ok` / `is_some` /
    /// ...) with a bool-typed destination so `{}` prints true/false on
    /// every tier.
    fn lower_combinator_pred_call(
        &mut self,
        helper: &'static str,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let v = self.lower_expr(&args[0])?;
        let bool_ty = self.tcx.bool_ty();
        Some(self.emit_combinator_call(helper, vec![Operand::Copy(Place::local(v))], bool_ty, span))
    }

    pub(crate) fn lower_iter_simple_vec_i64(
        &mut self,
        helper: &str,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let v = self.lower_iter_vec_arg(&args[0])?;
        let dest = self.fresh(i64_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(helper.to_string())),
            args: vec![Operand::Copy(Place::local(v))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    /// Like [`Self::lower_iter_simple_vec_i64`] but pins the dest as a
    /// boxed `Option<i64>` (the 16-byte Result/Option ABI), for
    /// terminals such as `iter::min` / `iter::max` whose Gossamer type
    /// is `Option<i64>`. The matching shim returns an i128-packed
    /// Option (None = 1, Some(m) = `gos_rt_result_new(0, m)`).
    pub(crate) fn lower_iter_simple_vec_i64_opt(
        &mut self,
        helper: &str,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let opt_ty = self.option_payload_adt_ty(i64_ty);
        let v = self.lower_iter_vec_arg(&args[0])?;
        let dest = self.fresh(opt_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(helper.to_string())),
            args: vec![Operand::Copy(Place::local(v))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn lower_iter_simple_vec_in_vec_out(
        &mut self,
        helper: &str,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::{IntTy, TyKind};
        let v = self.lower_iter_vec_arg(&args[0])?;
        // Pin the dest to `Vec<elem>` (never the call's raw Array/Var
        // type): the shim returns a heap `*mut GosVec`, so an
        // unannotated `iter::rev(xs)[i]` would otherwise take the
        // stack-array index path on a heap pointer and SIGSEGV.
        let i64_ty = self.tcx.int_ty(IntTy::I64);
        let elem = match self.tcx.kind_of(ty) {
            TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => *elem,
            _ => i64_ty,
        };
        let vec_ty = self.tcx.intern(TyKind::Vec(elem));
        let dest = self.fresh(vec_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(helper.to_string())),
            args: vec![Operand::Copy(Place::local(v))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn lower_iter_closure(
        &mut self,
        closure_arg: &HirExpr,
        inputs: &[Ty],
        output: Ty,
        span: Span,
    ) -> Option<Local> {
        let raw = self.lower_expr(closure_arg)?;
        let cb_sig = gossamer_types::FnSig {
            inputs: inputs.to_vec(),
            output,
        };
        let cb_trait_ty = self.tcx.intern(gossamer_types::TyKind::FnTrait(cb_sig));
        Some(self.coerce_to_fn_trait_if_needed(raw, cb_trait_ty, span))
    }

    pub(crate) fn lower_iter_vec_arg(&mut self, arg: &HirExpr) -> Option<Local> {
        use gossamer_types::TyKind;
        let raw = self.lower_expr(arg)?;
        let raw_ty = self.locals[raw.0 as usize].ty;
        if let TyKind::Array { elem, len } = self.tcx.kind_of(raw_ty).clone() {
            return Some(self.coerce_array_to_vec(raw, elem, len, arg.span));
        }
        Some(raw)
    }

    pub(crate) fn try_lower_for_hashmap_iter(
        &mut self,
        for_loop: &ForLoopShape<'_>,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let HirExprKind::MethodCall { receiver, name, .. } = &for_loop.iter_expr.kind else {
            return None;
        };
        if name.name != "iter" {
            return None;
        }
        let mut recv_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
        // Peel `&` / `&mut` so `for (k, v) in m.iter()` over a `&HashMap`
        // parameter is recognised as a map receiver; otherwise it falls through
        // to the generic for-vec path, which reads the map handle as a Vec. The
        // downstream key / value kind helpers already peel, and the receiver
        // handle matches what `m.len()` / `m.get_or()` pass through a borrow.
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(recv_ty) {
            recv_ty = *inner;
        }
        let recv_runtime_kind = self
            .receiver_local_from_path(receiver)
            .and_then(|l| self.local_runtime_kind.get(&l).copied());
        let is_hashmap = matches!(self.tcx.kind_of(recv_ty), TyKind::HashMap { .. });
        let is_btmap = recv_runtime_kind == Some("collections::BTreeMap");
        if !is_hashmap && !is_btmap {
            return None;
        }
        let HirPatKind::Tuple(elems) = &for_loop.loop_pat.kind else {
            return None;
        };
        if elems.len() != 2 {
            return None;
        }
        // Accept a `Binding` (bind the name) or `_` (read the slot but
        // bind no name) in either tuple position. The key is read either
        // way - the value lookup needs it - so a `_` key just suppresses
        // its user-visible binding. A literal or nested sub-pattern is
        // left to the generic for-vec path.
        let key_binding = match &elems[0].kind {
            HirPatKind::Binding { name, mutable } => Some((name.clone(), *mutable)),
            HirPatKind::Wildcard => None,
            _ => return None,
        };
        let val_binding = match &elems[1].kind {
            HirPatKind::Binding { name, mutable } => Some((name.clone(), *mutable)),
            HirPatKind::Wildcard => None,
            _ => return None,
        };
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let str_ty = self.tcx.string_ty();
        // A struct / tuple key hashes to opaque bytes the runtime cannot turn
        // back into the user's value, so `keys()` is empty and cannot drive the
        // loop. When the key is unused (`_`) and the values are scalar, iterate
        // the live values directly - matching the VM, which yields each entry's
        // value. (A used key needs key materialisation, and String / struct
        // values need owned-pointer recovery; both fall through to the generic
        // path for now.)
        if !is_btmap
            && key_binding.is_none()
            && matches!(self.hash_map_key_kind(recv_ty), Some(MapKeyKind::Other))
            && matches!(self.hash_map_value_kind(recv_ty), Some(MapValueKind::I64))
        {
            return self.lower_for_struct_keyed_values(receiver, for_loop.loop_pat, for_loop, span);
        }
        let (key_ty, val_ty, keys_helper, get_or_helper) = if is_btmap {
            // BTreeMap is hard-coded to `<String, i64>` in the
            // runtime today (see `GosBtMap`). Mirror that shape
            // here so the iter loop reads keys as strings and
            // values as i64. The runtime helper
            // `gos_rt_btmap_keys` returns a fresh `Vec<*c_char>`
            // in sorted key order.
            (str_ty, i64_ty, "gos_rt_btmap_keys", "gos_rt_btmap_get_or")
        } else {
            let key_kind = self.hash_map_key_kind(recv_ty);
            let value_kind = self.hash_map_value_kind(recv_ty);
            let key_ty = match key_kind {
                Some(MapKeyKind::String) => str_ty,
                _ => i64_ty,
            };
            let val_ty = match value_kind {
                Some(MapValueKind::String) => str_ty,
                // A struct value is stored as a boxed pointer; bind `v`
                // as a reference (a single box-pointer word) so field
                // access derefs the box. Typing the binding as the
                // by-value struct makes the drop pass treat the blob
                // pointer as an inline struct and release its RC fields
                // — a use-after-free (and on Windows a misaligned-RC
                // crash) once the map's own share is later released.
                // Non-struct aggregates keep the value type. Mirrors the
                // `for v in m.values()` binding in
                // [`try_lower_for_hashmap_iter`]'s sibling in ctrl.rs.
                Some(MapValueKind::Other) => {
                    let value_struct = self
                        .hash_map_kv_tys(recv_ty)
                        .map(|(_, v)| v)
                        .filter(|v| self.struct_name_of(*v).is_some());
                    match value_struct {
                        Some(v) => self.tcx.intern(TyKind::Ref {
                            mutability: gossamer_types::Mutbl::Not,
                            inner: v,
                        }),
                        None => self.hash_map_kv_tys(recv_ty).map_or(i64_ty, |(_, v)| v),
                    }
                }
                _ => i64_ty,
            };
            let keys_helper = match key_kind {
                Some(MapKeyKind::String) => "gos_rt_map_keys_str",
                _ => "gos_rt_map_keys_i64",
            };
            let get_or_helper = match (key_kind, value_kind) {
                (Some(MapKeyKind::String), Some(MapValueKind::String)) => {
                    "gos_rt_map_get_or_str_str"
                }
                (Some(MapKeyKind::String), _) => "gos_rt_map_get_or_str_i64",
                (_, Some(MapValueKind::String)) => "gos_rt_map_get_or_i64_str",
                _ => "gos_rt_map_get_or_i64",
            };
            (key_ty, val_ty, keys_helper, get_or_helper)
        };

        let recv_local = self.lower_expr(receiver)?;
        let keys_vec_ty = self.tcx.intern(TyKind::Vec(key_ty));
        let keys_vec = self.fresh(keys_vec_ty);
        let after_keys = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(keys_helper.to_string())),
            args: vec![Operand::Copy(Place::local(recv_local))],
            destination: Place::local(keys_vec),
            target: Some(after_keys),
        });
        self.set_current(after_keys);

        let len_local = self.fresh(i64_ty);
        let after_len = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
            args: vec![Operand::Copy(Place::local(keys_vec))],
            destination: Place::local(len_local),
            target: Some(after_len),
        });
        self.set_current(after_len);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        // ptr = gos_rt_vec_get_ptr(keys, counter); k = *ptr
        let ptr_local = self.fresh(i64_ty);
        let after_ptr = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_ptr".to_string())),
            args: vec![
                Operand::Copy(Place::local(keys_vec)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(ptr_local),
            target: Some(after_ptr),
        });
        self.set_current(after_ptr);
        let key_local = self.push_local(
            key_ty,
            key_binding.as_ref().map(|(n, _)| n.clone()),
            key_binding.as_ref().is_some_and(|(_, m)| *m),
        );
        if let Some((name, _)) = &key_binding {
            self.bind_local(&name.name, key_local);
        }
        let after_load = self.new_block(span);
        let zero_off = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_off),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_load".to_string())),
            args: vec![
                Operand::Copy(Place::local(ptr_local)),
                Operand::Copy(Place::local(zero_off)),
            ],
            destination: Place::local(key_local),
            target: Some(after_load),
        });
        self.set_current(after_load);

        // v = m.get_or(k, default). Default-by-value-type: 0 for
        // i64-valued maps, an empty string for string-valued maps.
        let default_local = if val_ty == str_ty {
            let l = self.fresh(str_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Str(String::new()))),
                span,
            );
            l
        } else {
            let l = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                span,
            );
            l
        };
        let val_local = self.push_local(
            val_ty,
            val_binding.as_ref().map(|(n, _)| n.clone()),
            val_binding.as_ref().is_some_and(|(_, m)| *m),
        );
        if let Some((name, _)) = &val_binding {
            self.bind_local(&name.name, val_local);
        }
        let after_val = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(get_or_helper.to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(key_local)),
                Operand::Copy(Place::local(default_local)),
            ],
            destination: Place::local(val_local),
            target: Some(after_val),
        });
        self.set_current(after_val);

        // Auto-region the body, exactly as the `for x in vec` path does: the
        // key/value bindings are read from the snapshot above (outside the
        // region), and only the body's per-iteration allocations are
        // bulk-freed at the iteration boundary. Eligibility rejects any
        // escape, so this can only speed the loop up, never change a result.
        let regioned = self.begin_loop_region(for_loop.body, span);
        let _ = self.lower_expr(for_loop.body);
        self.pop_scope();
        self.end_loop_region(regioned, span);
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        let bumped = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(bumped),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(bumped))),
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        let unit_ty = self.tcx.unit();
        let unit = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(unit),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        Some(unit)
    }

    /// Lowers `for (_, v) in m.iter()` over a struct / tuple-keyed map by
    /// driving the loop from the values snapshot. The key bytes a struct-keyed
    /// map stores are opaque (no `keys()` round-trip), so a key-driven loop
    /// would never iterate; reading the values directly yields each entry's
    /// value exactly as the VM's `m.iter()` does. The value binding's runtime
    /// kind selects the values helper and per-element getter.
    fn lower_for_struct_keyed_values(
        &mut self,
        receiver: &HirExpr,
        loop_pat: &HirPat,
        for_loop: &ForLoopShape<'_>,
        span: Span,
    ) -> Option<Local> {
        let HirPatKind::Tuple(elems) = &loop_pat.kind else {
            return None;
        };
        let val_binding = match &elems[1].kind {
            HirPatKind::Binding { name, mutable } => Some((name.clone(), *mutable)),
            HirPatKind::Wildcard => None,
            _ => return None,
        };
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let str_ty = self.tcx.string_ty();
        let recv_ty = self
            .receiver_local_from_path(receiver)
            .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
        let (val_ty, values_helper, getter) = match self.hash_map_value_kind(recv_ty) {
            Some(MapValueKind::String) => (str_ty, "gos_rt_map_values_str", "gos_rt_vec_get_ptr"),
            Some(MapValueKind::Other) => {
                // Struct / aggregate values are boxed pointers in the snapshot.
                let v = self.hash_map_kv_tys(recv_ty).map_or(i64_ty, |(_, v)| v);
                (v, "gos_rt_map_values_vec", "gos_rt_vec_get_ptr")
            }
            _ => (
                i64_ty,
                "gos_rt_map_values_i64",
                "gos_rt_vec_get_i64_unchecked",
            ),
        };

        let recv_local = self.lower_expr(receiver)?;
        let vals_vec_ty = self.tcx.intern(gossamer_types::TyKind::Vec(val_ty));
        let vals_vec = self.fresh(vals_vec_ty);
        let after_vals = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(values_helper.to_string())),
            args: vec![Operand::Copy(Place::local(recv_local))],
            destination: Place::local(vals_vec),
            target: Some(after_vals),
        });
        self.set_current(after_vals);

        let len_local = self.fresh(i64_ty);
        let after_len = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
            args: vec![Operand::Copy(Place::local(vals_vec))],
            destination: Place::local(len_local),
            target: Some(after_len),
        });
        self.set_current(after_len);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        self.push_scope();
        let val_local = self.push_local(
            val_ty,
            val_binding.as_ref().map(|(n, _)| n.clone()),
            val_binding.as_ref().is_some_and(|(_, m)| *m),
        );
        if let Some((name, _)) = &val_binding {
            self.bind_local(&name.name, val_local);
        }
        let after_get = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(getter.to_string())),
            args: vec![
                Operand::Copy(Place::local(vals_vec)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(val_local),
            target: Some(after_get),
        });
        self.set_current(after_get);

        // Auto-region the body: the value binding is read from the values
        // snapshot above (outside the region), so only the body's
        // per-iteration allocations are arena-freed at the boundary.
        let regioned = self.begin_loop_region(for_loop.body, span);
        let _ = self.lower_expr(for_loop.body);
        self.pop_scope();
        self.end_loop_region(regioned, span);
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        let bumped = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(bumped),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(bumped))),
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        let unit_ty = self.tcx.unit();
        let unit = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(unit),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        Some(unit)
    }

    /// Materialise a HashMap `m.iter()` bound directly to a
    /// `Vec<(K, V)>` into a real heap vector of `(K, V)` tuples.
    /// Mirrors the `for (k, v) in m.iter()` lowering: snapshot the
    /// keys via `gos_rt_map_keys_*`, then `get_or` each value and
    /// push the `(k, v)` tuple. The for-loop form is handled earlier
    /// by `try_lower_for_hashmap_iter`; this covers the direct-bind
    /// form (`let entries = m.iter()`) that otherwise dispatched the
    /// map receiver through `gos_rt_arr_iter` and segfaulted on the
    /// compiled tiers.
    pub(crate) fn materialize_hashmap_entries(
        &mut self,
        receiver: &HirExpr,
        recv_ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let str_ty = self.tcx.string_ty();
        let key_kind = self.hash_map_key_kind(recv_ty);
        let value_kind = self.hash_map_value_kind(recv_ty);
        let key_ty = match key_kind {
            Some(MapKeyKind::String) => str_ty,
            _ => i64_ty,
        };
        let val_ty = match value_kind {
            Some(MapValueKind::String) => str_ty,
            // A struct value is stored as a boxed pointer; bind the
            // tuple slot as a reference so field access derefs the box.
            // A by-value struct binding makes the drop pass release the
            // blob pointer's RC fields as if they were inline — a
            // use-after-free. Mirrors the `for (k, v) in m.iter()` and
            // `for v in m.values()` bindings.
            Some(MapValueKind::Other) => {
                let value_struct = self
                    .hash_map_kv_tys(recv_ty)
                    .map(|(_, v)| v)
                    .filter(|v| self.struct_name_of(*v).is_some());
                match value_struct {
                    Some(v) => self.tcx.intern(TyKind::Ref {
                        mutability: gossamer_types::Mutbl::Not,
                        inner: v,
                    }),
                    None => self.hash_map_kv_tys(recv_ty).map_or(i64_ty, |(_, v)| v),
                }
            }
            _ => i64_ty,
        };
        let keys_helper = match key_kind {
            Some(MapKeyKind::String) => "gos_rt_map_keys_str",
            _ => "gos_rt_map_keys_i64",
        };
        let get_or_helper = match (key_kind, value_kind) {
            (Some(MapKeyKind::String), Some(MapValueKind::String)) => "gos_rt_map_get_or_str_str",
            (Some(MapKeyKind::String), _) => "gos_rt_map_get_or_str_i64",
            (_, Some(MapValueKind::String)) => "gos_rt_map_get_or_i64_str",
            _ => "gos_rt_map_get_or_i64",
        };

        let unit_ty = self.tcx.unit();
        let tuple_ty = self.tcx.intern(TyKind::Tuple(vec![key_ty, val_ty]));
        let result_vec_ty = self.tcx.intern(TyKind::Vec(tuple_ty));

        let recv_local = self.lower_expr(receiver)?;

        // keys = m.keys() - a fresh real Vec<K> snapshot.
        let keys_vec_ty = self.tcx.intern(TyKind::Vec(key_ty));
        let keys_vec = self.fresh(keys_vec_ty);
        let after_keys = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(keys_helper.to_string())),
            args: vec![Operand::Copy(Place::local(recv_local))],
            destination: Place::local(keys_vec),
            target: Some(after_keys),
        });
        self.set_current(after_keys);

        // len = keys.len()
        let len_local = self.fresh(i64_ty);
        let after_len = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_len".to_string())),
            args: vec![Operand::Copy(Place::local(keys_vec))],
            destination: Place::local(len_local),
            target: Some(after_len),
        });
        self.set_current(after_len);

        // result = Vec::new(elem_bytes_of((K, V)))
        let elem_bytes_val = i128::from(self.elem_bytes_of(tuple_ty).max(8));
        let elem_bytes = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes),
            Rvalue::Use(Operand::Const(ConstValue::Int(elem_bytes_val))),
            span,
        );
        let result_vec = self.fresh(result_vec_ty);
        let after_new = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
            args: vec![Operand::Copy(Place::local(elem_bytes))],
            destination: Place::local(result_vec),
            target: Some(after_new),
        });
        self.set_current(after_new);

        let counter = self.push_local(i64_ty, None, true);
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let step_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let bool_ty = self.tcx.bool_ty();
        let cmp = self.fresh(bool_ty);
        self.emit_assign(
            Place::local(cmp),
            Rvalue::BinaryOp {
                op: BinOp::Lt,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(len_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        // key = keys[counter]
        let ptr_local = self.fresh(i64_ty);
        let after_ptr = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_get_ptr".to_string())),
            args: vec![
                Operand::Copy(Place::local(keys_vec)),
                Operand::Copy(Place::local(counter)),
            ],
            destination: Place::local(ptr_local),
            target: Some(after_ptr),
        });
        self.set_current(after_ptr);
        let key_local = self.fresh(key_ty);
        let zero_off = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_off),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let after_load = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_load".to_string())),
            args: vec![
                Operand::Copy(Place::local(ptr_local)),
                Operand::Copy(Place::local(zero_off)),
            ],
            destination: Place::local(key_local),
            target: Some(after_load),
        });
        self.set_current(after_load);

        // val = m.get_or(key, default)
        let default_local = if val_ty == str_ty {
            let l = self.fresh(str_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Str(String::new()))),
                span,
            );
            l
        } else {
            let l = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(l),
                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                span,
            );
            l
        };
        let val_local = self.fresh(val_ty);
        let after_val = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(get_or_helper.to_string())),
            args: vec![
                Operand::Copy(Place::local(recv_local)),
                Operand::Copy(Place::local(key_local)),
                Operand::Copy(Place::local(default_local)),
            ],
            destination: Place::local(val_local),
            target: Some(after_val),
        });
        self.set_current(after_val);

        // tuple = (key, val); result.push(tuple)
        let tuple_local = self.fresh(tuple_ty);
        self.emit_assign(
            Place::local(tuple_local),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Tuple,
                operands: vec![
                    Operand::Copy(Place::local(key_local)),
                    Operand::Copy(Place::local(val_local)),
                ],
            },
            span,
        );
        let push_dest = self.fresh(unit_ty);
        let after_push = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
            args: vec![
                Operand::Copy(Place::local(result_vec)),
                Operand::Copy(Place::local(tuple_local)),
            ],
            destination: Place::local(push_dest),
            target: Some(after_push),
        });
        self.set_current(after_push);
        self.terminate(Terminator::Goto { target: step_block });

        self.set_current(step_block);
        let one = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(one),
            Rvalue::Use(Operand::Const(ConstValue::Int(1))),
            span,
        );
        let bumped = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(bumped),
            Rvalue::BinaryOp {
                op: BinOp::Add,
                lhs: Operand::Copy(Place::local(counter)),
                rhs: Operand::Copy(Place::local(one)),
            },
            span,
        );
        self.emit_assign(
            Place::local(counter),
            Rvalue::Use(Operand::Copy(Place::local(bumped))),
            span,
        );
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
        Some(result_vec)
    }
}
