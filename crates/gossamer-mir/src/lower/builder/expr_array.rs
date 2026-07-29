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
    pub(crate) fn lower_let_array_as_vec(
        &mut self,
        local: Local,
        elems: &[HirExpr],
        span: Span,
    ) -> bool {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let unit_ty = self.tcx.unit();
        // Size the vec slot by its declared element type. An empty
        // `let xs: [(String, i64)] = []` would otherwise pass 8 to
        // `Vec::new`, which makes the byte-erased `gos_rt_vec_push`
        // copy only the first 8 bytes of each tuple - every i64 in
        // a `(String, i64)` element is then lost on push and reread
        // as the next entry's String pointer on iteration. Extract
        // the inner element type from the binding's `Vec(_)` /
        // `Slice(_)` kind and route through `elem_bytes_of` (which
        // returns the correct stride for each element type, including
        // 1 for bool and larger counts for multi-slot aggregates).
        let binding_ty = self.locals[local.0 as usize].ty;
        let elem_bytes_val: i128 = match self.tcx.kind_of(binding_ty) {
            gossamer_types::TyKind::Vec(elem) | gossamer_types::TyKind::Slice(elem) => {
                i128::from(self.elem_bytes_of(*elem).max(1))
            }
            _ => 8,
        };
        let elem_bytes = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes),
            Rvalue::Use(Operand::Const(ConstValue::Int(elem_bytes_val))),
            span,
        );
        // `Vec::new` is the codegen-side intrinsic name that
        // routes to `gos_rt_vec_new(8)`; using it avoids pulling
        // in the lower-level helper directly and keeps the call
        // dispatch path identical to user-written `Vec::new()`.
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
            args: vec![Operand::Copy(Place::local(elem_bytes))],
            destination: Place::local(local),
            target: Some(next),
        });
        self.set_current(next);
        // Whether an inner array element should be promoted to a heap
        // `GosVec` (nested dynamic `Vec<Vec<T>>` / `[[T]]`) or stored
        // inline (`Vec<[T; N]>`). A binding whose element type is a
        // fixed-size `Array` keeps its elements inline so the literal
        // and the later `push([..])` agree on a 16-byte inline slot;
        // coercing them to pointers there would desync the stride and
        // corrupt every read.
        let binding_elem_is_fixed_array = matches!(
            self.tcx.kind_of(binding_ty),
            gossamer_types::TyKind::Vec(e) | gossamer_types::TyKind::Slice(e)
                if matches!(self.tcx.kind_of(*e), gossamer_types::TyKind::Array { .. })
        );
        for elem in elems {
            let Some(mut elem_local) = self.lower_expr(elem) else {
                return false;
            };
            // If an element is itself a flat Array{T,N} (e.g. the inner
            // arrays in `[[i64]]`), coerce it to a heap GosVec so the
            // outer Vec stores *mut GosVec pointers, not flat aggregates
            // - unless the binding's element type is a fixed-size array,
            // in which case the inner array stays inline.
            if !binding_elem_is_fixed_array {
                let lt = self.locals[elem_local.0 as usize].ty;
                if let gossamer_types::TyKind::Array {
                    elem: inner_elem,
                    len: inner_len,
                } = self.tcx.kind_of(lt).clone()
                {
                    elem_local = self.coerce_array_to_vec(elem_local, inner_elem, inner_len, span);
                }
            }
            let push_dest = self.fresh(unit_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                args: vec![
                    Operand::Copy(Place::local(local)),
                    Operand::Copy(Place::local(elem_local)),
                ],
                destination: Place::local(push_dest),
                target: Some(next),
            });
            self.set_current(next);
        }
        true
    }

    pub(crate) fn lower_array_list(
        &mut self,
        elems: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // When the context expects a Vec/Slice (dynamic array), build a
        // proper GosVec via gos_rt_vec_push rather than a flat aggregate
        // that codegen can't distinguish from a fixed-size array.  This
        // fixes [[i64]] (Vec-of-Vec) shapes where flat aggregates were
        // stored as elements and then mis-read as GosVec structs.
        if matches!(self.tcx.kind_of(ty), TyKind::Vec(_) | TyKind::Slice(_)) {
            let local = self.fresh(ty);
            if self.lower_let_array_as_vec(local, elems, span) {
                return Some(local);
            }
        }
        let mut operands = Vec::with_capacity(elems.len());
        let mut elem_struct: Option<String> = None;
        let mut elem_ty: Option<Ty> = None;
        for elem in elems {
            let local = self.lower_expr(elem)?;
            if elem_struct.is_none() {
                if let Some(name) = self.local_struct.get(&local).cloned() {
                    elem_struct = Some(name);
                }
            }
            // The HIR-side `expr.ty` for an array literal is often a
            // leaked inference variable (e.g. `Var(...)`), which sizes
            // every aggregate alloca to a single i64 slot - and a
            // 3-element array literal then writes 24 B into 8 B of
            // stack and clobbers adjacent locals (the recurring
            // "for x in [1, 2, 3] { ... if !flag ... }" miscompile).
            // Fall back to the lowered element local's MIR type when
            // the HIR type can't tell us the stride.
            if elem_ty.is_none() {
                let lt = self.locals[local.0 as usize].ty;
                if !matches!(self.tcx.kind_of(lt), TyKind::Var(_) | TyKind::Error) {
                    elem_ty = Some(lt);
                }
            }
            operands.push(Operand::Copy(Place::local(local)));
        }
        // Pin the destination to a real `Array { elem, len }` so the
        // codegen sees the full slot count. When the typeck left the
        // outer Array's elem as `Var`/`Error`, the print/format
        // dispatch can't classify the elem and falls back to the
        // `<value>` placeholder - refresh elem from the lowered
        // element local's concrete MIR type when available.
        let dest_ty = match self.tcx.kind_of(ty) {
            TyKind::Array { elem, .. } => {
                let elem_unresolved =
                    matches!(self.tcx.kind_of(*elem), TyKind::Var(_) | TyKind::Error);
                match (elem_unresolved, elem_ty) {
                    (true, Some(et)) => self.tcx.intern(TyKind::Array {
                        elem: et,
                        len: gossamer_types::ArrayLen::Concrete(elems.len()),
                    }),
                    _ => ty,
                }
            }
            _ => match elem_ty {
                Some(et) => self.tcx.intern(TyKind::Array {
                    elem: et,
                    len: gossamer_types::ArrayLen::Concrete(elems.len()),
                }),
                None => ty,
            },
        };
        let dest = self.fresh(dest_ty);
        if let Some(name) = elem_struct {
            self.local_elem_struct.insert(dest, name);
        }
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Array,
                operands,
            },
            span,
        );
        Some(dest)
    }

    pub(crate) fn lower_array_repeat(
        &mut self,
        value: &HirExpr,
        count: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        self.lower_array_repeat_into(value, count, ty, span, None)
    }

    pub(crate) fn lower_array_repeat_into(
        &mut self,
        value: &HirExpr,
        count: &HirExpr,
        ty: Ty,
        span: Span,
        destination: Option<Local>,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // A `[value; N]` literal whose context wants a growable Vec/Slice
        // builds a heap GosVec of N copies, byte-correct for the element
        // type - not a fixed inline array. Mirrors `lower_array_list`'s
        // Vec promotion and covers both a literal and a runtime count.
        let wants_vec = matches!(self.tcx.kind_of(ty), TyKind::Vec(_) | TyKind::Slice(_));
        if !wants_vec {
            if let Some(count_u64) = literal_u64(count) {
                let value_local = self.lower_expr(value)?;
                let dest = self.fresh(ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Repeat {
                        value: Operand::Copy(Place::local(value_local)),
                        count: count_u64,
                    },
                    span,
                );
                return Some(dest);
            }
        }
        // Build a heap `GosVec` of `count` copies of `value`. Used when
        // the target type is a growable Vec/Slice, and as the fallback
        // for a runtime (non-literal) count where an inline fixed array
        // is impossible.
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let value_local = self.lower_expr(value)?;
        let count_local = self.lower_expr(count)?;
        // Size each slot by the element's own byte width so a Vec of
        // multi-slot elements (tuples, String, fixed arrays) copies the
        // whole element on every push rather than truncating to 8 bytes.
        let elem_src_ty = match self.tcx.kind_of(ty) {
            TyKind::Vec(e) | TyKind::Slice(e) => *e,
            _ => self.locals[value_local.0 as usize].ty,
        };
        let elem_bytes_val = i128::from(self.elem_bytes_of(elem_src_ty).max(1));
        let elem_bytes_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(elem_bytes_val))),
            span,
        );
        let vec_local = destination.unwrap_or_else(|| self.fresh(ty));
        let primitive_repeat = elem_bytes_val <= 8
            && matches!(
                self.tcx.kind_of(elem_src_ty),
                TyKind::Int(_) | TyKind::Bool | TyKind::Char
            );
        if primitive_repeat {
            let after_repeat = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_repeat_primitive".to_string())),
                args: vec![
                    Operand::Copy(Place::local(elem_bytes_local)),
                    Operand::Copy(Place::local(count_local)),
                    Operand::Copy(Place::local(value_local)),
                ],
                destination: Place::local(vec_local),
                target: Some(after_repeat),
            });
            self.set_current(after_repeat);
            return Some(vec_local);
        }
        let after_new = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_with_capacity".to_string())),
            args: vec![
                Operand::Copy(Place::local(elem_bytes_local)),
                Operand::Copy(Place::local(count_local)),
            ],
            destination: Place::local(vec_local),
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
                rhs: Operand::Copy(Place::local(count_local)),
            },
            span,
        );
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cmp)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        let after_push = self.new_block(span);
        let unit_ty = self.tcx.unit();
        let push_dest = self.fresh(unit_ty);
        // Byte-erased push: `gos_rt_vec_push` copies the vec's per-slot
        // byte width (set at `with_capacity`), so it handles any element
        // type, unlike the i64-only `gos_rt_vec_push_i64`.
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
            args: vec![
                Operand::Copy(Place::local(vec_local)),
                Operand::Copy(Place::local(value_local)),
            ],
            destination: Place::local(push_dest),
            target: Some(after_push),
        });
        self.set_current(after_push);
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
        Some(vec_local)
    }
}
