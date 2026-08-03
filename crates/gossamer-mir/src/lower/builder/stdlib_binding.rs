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
    pub(crate) fn lower_external_binding_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.is_empty() {
            return None;
        }
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let (module_path, item_name, item) = self.resolve_external_binding(&names, args.len())?;

        let mangled_module = module_path.replace("::", "__");
        let mangled = format!("gos_binding_{mangled_module}__{item_name}");

        let ret_ty = self.binding_type_to_mir(&item.ret);
        let mut arg_locals: Vec<Local> = Vec::with_capacity(args.len());
        for (idx, arg) in args.iter().enumerate() {
            let raw = self.lower_expr(arg)?;
            let param_ty = item.params.get(idx);
            let coerced = self.coerce_arg_for_binding(raw, param_ty, span);
            arg_locals.push(coerced);
        }
        let dest = self.fresh(ret_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(mangled)),
            args: arg_locals
                .into_iter()
                .map(|l| Operand::Copy(Place::local(l)))
                .collect(),
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        // Result / Option returns arrive as a binding-ABI
        // `*mut GosVariant`; convert to the runtime's packed i128
        // result (string payloads become runtime strings) and type
        // the converted local as the real Result / Option Adt so
        // match / `?` / fmt downstream see the standard shape.
        let converted = self.convert_binding_variant_return(dest, &item.ret, span);
        Some(converted.unwrap_or(dest))
    }

    /// Emits the `gos_rt_binding_variant_to_result` conversion for a
    /// binding call whose declared return is `Result` / `Option`.
    /// Returns `None` for every other return shape.
    fn convert_binding_variant_return(
        &mut self,
        raw: Local,
        ret: &gossamer_resolve::BindingType,
        span: Span,
    ) -> Option<Local> {
        use gossamer_resolve::BindingType as B;
        use gossamer_types::TyKind;
        let adt_ty = match ret {
            B::Result(ok, err) => {
                let ok_ty = self.binding_type_to_mir(ok);
                let err_ty = self.binding_type_to_mir(err);
                let substs = gossamer_types::Substs::from_types([ok_ty, err_ty]);
                self.tcx.intern(TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX),
                    substs,
                })
            }
            B::Option(inner) => {
                let inner_ty = self.binding_type_to_mir(inner);
                let substs = gossamer_types::Substs::from_types([inner_ty]);
                self.tcx.intern(TyKind::Adt {
                    def: gossamer_resolve::DefId::local(u32::MAX - 1),
                    substs,
                })
            }
            _ => return None,
        };
        let dest = self.fresh(adt_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(
                "gos_rt_binding_variant_to_result".to_string(),
            )),
            args: vec![Operand::Copy(Place::local(raw))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn coerce_arg_for_binding(
        &mut self,
        raw: Local,
        param_ty: Option<&gossamer_resolve::BindingType>,
        span: Span,
    ) -> Local {
        use gossamer_resolve::BindingType as B;
        use gossamer_types::TyKind;
        let Some(B::Vec(_)) = param_ty else {
            return raw;
        };
        let raw_ty = self.locals[raw.0 as usize].ty;
        let TyKind::Array { elem, len } = self.tcx.kind_of(raw_ty) else {
            return raw;
        };
        let elem_ty = *elem;
        let len_val = len.to_usize();
        let elem_bytes = self.elem_bytes_of(elem_ty);
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
        let vec_local = self.fresh(i64_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_from_arr".to_string())),
            args: vec![
                Operand::Copy(Place::local(elem_bytes_local)),
                Operand::Copy(Place::local(raw)),
                Operand::Copy(Place::local(len_local)),
            ],
            destination: Place::local(vec_local),
            target: Some(next),
        });
        self.set_current(next);
        vec_local
    }

    pub(crate) fn coerce_array_to_vec(
        &mut self,
        raw: Local,
        elem_ty: Ty,
        len: gossamer_types::ArrayLen,
        span: Span,
    ) -> Local {
        use gossamer_types::TyKind;
        let len = len.to_usize();
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);

        // Nested array: `Array{Array{T,N},M}` → `Vec<Vec<T>>`.
        // Each inner flat array must become a heap GosVec pointer so that
        // `gos_rt_vec_get_i64` on the outer Vec returns a valid *mut GosVec.
        if let TyKind::Array {
            elem: inner_elem,
            len: inner_len,
        } = self.tcx.kind_of(elem_ty).clone()
        {
            let inner_len = inner_len.to_usize();
            let inner_elem_bytes = self.elem_bytes_of(inner_elem);
            let inner_elem_bytes_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(inner_elem_bytes_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(
                    inner_elem_bytes,
                )))),
                span,
            );
            let inner_len_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(inner_len_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(inner_len as i128))),
                span,
            );
            let outer_len_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(outer_len_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(len as i128))),
                span,
            );
            let inner_vec_ty = self.tcx.intern(TyKind::Vec(inner_elem));
            let outer_vec_ty = self.tcx.intern(TyKind::Vec(inner_vec_ty));
            let dest = self.fresh(outer_vec_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_nested_arr_to_vec".to_string())),
                args: vec![
                    Operand::Copy(Place::local(inner_elem_bytes_local)),
                    Operand::Copy(Place::local(inner_len_local)),
                    Operand::Copy(Place::local(raw)),
                    Operand::Copy(Place::local(outer_len_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return dest;
        }

        let elem_bytes = self.elem_bytes_of(elem_ty);
        let elem_bytes_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(elem_bytes)))),
            span,
        );
        let len_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(len_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(len as i128))),
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
        if let Some(name) = self.local_elem_struct.get(&raw).cloned() {
            self.local_elem_struct.insert(dest, name);
        }
        dest
    }

    /// Converts only the outer fixed array to a Vec. Nested fixed-array
    /// elements retain their inline `[T; N]` layout.
    pub(crate) fn fixed_array_to_vec(
        &mut self,
        raw: Local,
        elem_ty: Ty,
        len: gossamer_types::ArrayLen,
        span: Span,
    ) -> Local {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let elem_bytes_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(
                self.elem_bytes_of(elem_ty),
            )))),
            span,
        );
        let len_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(len_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(len.to_usize() as i128))),
            span,
        );
        let vec_ty = self.tcx.intern(gossamer_types::TyKind::Vec(elem_ty));
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
        dest
    }

    /// Coerces a borrowed array (`&[T; N]` reaching a `&[T]` / `&Vec<T>`
    /// parameter) into a borrowing GosVec view. Identical buffer to
    /// `coerce_array_to_vec`, but built via `gos_rt_vec_borrow_arr` so the
    /// drop pass leaves it non-owning: a borrow must not free the element
    /// children the source array still owns. Nested-array borrows fall back
    /// to the owning conversion (exotic; the inner vecs need real headers).
    pub(crate) fn coerce_borrow_array_to_vec(
        &mut self,
        raw: Local,
        elem_ty: Ty,
        len: gossamer_types::ArrayLen,
        span: Span,
    ) -> Local {
        use gossamer_types::TyKind;
        if matches!(self.tcx.kind_of(elem_ty), TyKind::Array { .. }) {
            return self.coerce_array_to_vec(raw, elem_ty, len, span);
        }
        let len = len.to_usize();
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let elem_bytes = self.elem_bytes_of(elem_ty);
        let elem_bytes_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_bytes_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(elem_bytes)))),
            span,
        );
        let len_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(len_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(len as i128))),
            span,
        );
        let vec_ty = self.tcx.intern(TyKind::Vec(elem_ty));
        let dest = self.fresh(vec_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_borrow_arr".to_string())),
            args: vec![
                Operand::Copy(Place::local(elem_bytes_local)),
                Operand::Copy(Place::local(raw)),
                Operand::Copy(Place::local(len_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    pub(crate) fn resolve_external_binding(
        &self,
        names: &[&str],
        argc: usize,
    ) -> Option<(String, String, gossamer_resolve::ExternalItem)> {
        if names.len() >= 2 {
            let qualified = names.join("::");
            if let Some(item) = gossamer_resolve::lookup_external_item(&qualified) {
                let (module_path, item_name) = qualified.rsplit_once("::")?;
                return Some((module_path.to_string(), item_name.to_string(), item));
            }
            // Module-prefixed lookup: try the leaf segment against
            // every module whose path ends in the leading segment.
            // E.g. `echo::shout` matches the `echo` module.
            let leading = names[0];
            let leaf = *names.last()?;
            for m in gossamer_resolve::all_external_modules() {
                let path_segs: Vec<&str> = m.path.split("::").collect();
                if path_segs.last().copied() == Some(leading)
                    && let Some(item) = m.items.iter().find(|i| i.name == leaf)
                {
                    return Some((m.path.clone(), item.name.clone(), item.clone()));
                }
            }
            return None;
        }
        // Bare-leaf lookup: walk every module's items looking for
        // the unique candidate matching arity.
        let leaf = names[0];
        let mut matches: Vec<(String, gossamer_resolve::ExternalItem)> = Vec::new();
        for m in gossamer_resolve::all_external_modules() {
            for item in &m.items {
                if item.name == leaf && item.params.len() == argc {
                    matches.push((m.path.clone(), item.clone()));
                }
            }
        }
        if matches.len() == 1 {
            let (module_path, item) = matches.pop()?;
            return Some((module_path, item.name.clone(), item));
        }
        None
    }
}
