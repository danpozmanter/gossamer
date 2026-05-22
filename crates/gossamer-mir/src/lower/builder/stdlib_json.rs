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
    pub(crate) fn emit_json_get(
        &mut self,
        receiver_local: Local,
        field: &str,
        span: Span,
    ) -> Local {
        let json_ty = self.tcx.json_value_ty();
        let dest = self.fresh(json_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_get".to_string())),
            args: vec![
                Operand::Copy(Place::local(receiver_local)),
                Operand::Const(ConstValue::Str(field.to_string())),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    pub(crate) fn lower_json_free_call(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        span: Span,
    ) -> Option<Local> {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return None;
        };
        if segments.len() < 2 {
            return None;
        }
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let last = *names.last()?;
        let module_chain = &names[..names.len() - 1];
        let module_ok = matches!(
            module_chain,
            ["json"] | ["encoding", "json"] | ["std", "encoding", "json"]
        );
        let value_ctor_ok = matches!(
            module_chain,
            ["json", "Value"]
                | ["encoding", "json", "Value"]
                | ["std", "encoding", "json", "Value"]
        );
        if !module_ok && !value_ctor_ok {
            return None;
        }
        if value_ctor_ok {
            // `json::Value::object([(k, v), …])`: when the arg is
            // an array literal of pairs, build a flat
            // `[k0, v0, k1, v1, …]` arena buffer and call the
            // variadic-style helper. The previous path passed the
            // array's stack-slot address as a `*mut GosVec`, and
            // the helper read garbage out of the missing header.
            if (last == "object" || last == "Object")
                && let Some(first_arg) = args.first()
                && let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(pairs)) = &first_arg.kind
            {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let unit_ty = self.tcx.unit();
                let total_slots = (pairs.len() * 2) as i128;
                // gos_alloc N*16 bytes for the flat pairs buffer.
                let bytes_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(bytes_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(total_slots * 8))),
                    span,
                );
                let buf_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(buf_local),
                    Rvalue::CallIntrinsic {
                        name: "gos_alloc",
                        args: vec![Operand::Copy(Place::local(bytes_local))],
                    },
                    span,
                );
                // For each pair `(k, v)` (a HirExprKind::Tuple),
                // lower k and v separately and gos_store at the
                // right offsets.
                for (i, pair) in pairs.iter().enumerate() {
                    let HirExprKind::Tuple(fields) = &pair.kind else {
                        return None;
                    };
                    if fields.len() != 2 {
                        return None;
                    }
                    let key_local = self.lower_expr(&fields[0])?;
                    let val_local = self.lower_expr(&fields[1])?;
                    let key_off = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(key_off),
                        Rvalue::Use(Operand::Const(ConstValue::Int((i * 2 * 8) as i128))),
                        span,
                    );
                    let val_off = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(val_off),
                        Rvalue::Use(Operand::Const(ConstValue::Int((i * 2 * 8 + 8) as i128))),
                        span,
                    );
                    let store_k = self.fresh(unit_ty);
                    self.emit_assign(
                        Place::local(store_k),
                        Rvalue::CallIntrinsic {
                            name: "gos_store",
                            args: vec![
                                Operand::Copy(Place::local(buf_local)),
                                Operand::Copy(Place::local(key_off)),
                                Operand::Copy(Place::local(key_local)),
                            ],
                        },
                        span,
                    );
                    let store_v = self.fresh(unit_ty);
                    self.emit_assign(
                        Place::local(store_v),
                        Rvalue::CallIntrinsic {
                            name: "gos_store",
                            args: vec![
                                Operand::Copy(Place::local(buf_local)),
                                Operand::Copy(Place::local(val_off)),
                                Operand::Copy(Place::local(val_local)),
                            ],
                        },
                        span,
                    );
                }
                let count_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(count_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(pairs.len() as i128))),
                    span,
                );
                let ret_ty = self.tcx.json_value_ty();
                let dest = self.fresh(ret_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_json_value_object_n".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(count_local)),
                        Operand::Copy(Place::local(buf_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
            // Zero-arg `json::Value::object()` — route to the _n variant
            // with n=0 so the compiled tier always passes explicit args.
            // The generic path below would emit a call with no arguments,
            // leaving the GosVec register uninitialized; on Windows the
            // garbage value is non-null and misaligned, causing a fault.
            if (last == "object" || last == "Object") && args.is_empty() {
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let zero = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(zero),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    span,
                );
                let ret_ty = self.tcx.json_value_ty();
                let dest = self.fresh(ret_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_json_value_object_n".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(zero)),
                        Operand::Copy(Place::local(zero)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
            let rt_name = match last {
                "String" => "gos_rt_json_value_string",
                "Int" => "gos_rt_json_value_int",
                "Bool" => "gos_rt_json_value_bool",
                "Null" => "gos_rt_json_value_null",
                "Array" => "gos_rt_json_value_array",
                "object" | "Object" => "gos_rt_json_value_object",
                _ => return None,
            };
            let mut arg_locals = Vec::with_capacity(args.len());
            for arg in args {
                arg_locals.push(self.lower_expr(arg)?);
            }
            let ret_ty = self.tcx.json_value_ty();
            let dest = self.fresh(ret_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
                args: arg_locals
                    .into_iter()
                    .map(|l| Operand::Copy(Place::local(l)))
                    .collect(),
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // Struct-aware render: json::render(user_struct) → serialize each field.
        //
        // The HIR type of `&val` is often left as a generic Var (the
        // parameter T in `render<T>(val: &T)`). To find the concrete
        // struct type, peel `&` syntactically and use the inner
        // expression's type, which the typechecker always resolves.
        if (last == "render" || last == "encode") && !args.is_empty() {
            let inner_arg = {
                let mut e = &args[0];
                while let gossamer_hir::HirExprKind::Unary {
                    op: gossamer_hir::HirUnaryOp::RefShared | gossamer_hir::HirUnaryOp::RefMut,
                    operand,
                    ..
                } = &e.kind
                {
                    e = operand;
                }
                e
            };
            let mut peeled = inner_arg.ty;
            while let gossamer_types::TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                peeled = *inner;
            }
            if let gossamer_types::TyKind::Adt { def, .. } = self.tcx.kind_of(peeled).clone() {
                if let Some(result) = self.lower_json_render_adt(args, def, span) {
                    return Some(result);
                }
            }
            // Scalar / array / json::Value args. `gos_rt_json_render`
            // dereferences its argument as a `*GosJson`, so a bare i64
            // (`encode(42)`) would be read as a wild pointer (SIGSEGV)
            // and a typed scalar `*GosVec` (`encode([1,2,3])`) would be
            // misread as a Value. Box the argument into a `*GosJson`
            // first.
            if let Some(result) = self.lower_json_render_value(args, peeled, span) {
                return Some(result);
            }
        }
        let (rt_name, ret_ty) = match last {
            "parse" | "decode" => ("gos_rt_json_parse", self.result_json_value_error_adt_ty()),
            "render" | "encode" => ("gos_rt_json_render", self.tcx.string_ty()),
            // `json::set(obj, key, value) → json::Value` — append or
            // replace a named field on an object-shaped Value.
            "set" => ("gos_rt_json_set", self.tcx.json_value_ty()),
            // User-level `json::get` returns `Option<json::Value>`.
            // The bare `gos_rt_json_get` is still used by the field-
            // access lowering for `root.a.b.c` (raw chain pointer).
            "get" => ("gos_rt_json_get_opt", self.option_json_value_adt_ty()),
            "at" => ("gos_rt_json_at", self.tcx.json_value_ty()),
            // `json::as_i64` / `as_f64` / `as_str` return `Option<T>`
            // (Some only when the JSON node is the matching type),
            // matching the VM. The auto-derived `from_json` matches on
            // the `Some`/`None` to validate field types; a bare-value
            // return made every non-matching field silently coerce.
            "as_i64" => ("gos_rt_json_as_i64_opt", self.option_i64_adt_ty()),
            "as_f64" => ("gos_rt_json_as_f64_opt", self.option_f64_adt_ty()),
            "as_str" => ("gos_rt_json_as_str_opt", self.option_string_adt_ty()),
            "as_bool" => ("gos_rt_json_as_bool", self.tcx.bool_ty()),
            "as_array" => ("gos_rt_json_as_array_opt", self.option_json_array_adt_ty()),
            "keys" => ("gos_rt_json_keys_opt", self.option_string_vec_adt_ty()),
            "len" => (
                "gos_rt_json_len",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            "is_null" => ("gos_rt_json_is_null", self.tcx.bool_ty()),
            _ => return None,
        };
        let mut arg_locals = Vec::with_capacity(args.len());
        for arg in args {
            arg_locals.push(self.lower_expr(arg)?);
        }
        let dest = self.fresh(ret_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(rt_name.to_string())),
            args: arg_locals
                .into_iter()
                .map(|l| Operand::Copy(Place::local(l)))
                .collect(),
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn lower_json_render_adt(
        &mut self,
        args: &[HirExpr],
        def: gossamer_resolve::DefId,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let struct_name = self.struct_defs.get(&def)?.clone();
        let field_names = self.structs.get(&struct_name)?.clone();
        let field_tys: Vec<_> = self.tcx.struct_field_tys(def)?.to_vec();
        if field_names.len() != field_tys.len() || field_names.is_empty() {
            return None;
        }
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let unit_ty = self.tcx.unit();
        let string_ty = self.tcx.string_ty();
        let json_val_ty = self.tcx.json_value_ty();
        let vec_of_i64_ty = self.tcx.intern(TyKind::Vec(i64_ty));

        let struct_local = self.lower_expr(&args[0])?;

        // Allocate the KV pairs vec (8-byte element slots — cstr/GosJson ptrs).
        let pairs_vec = self.fresh(vec_of_i64_ty);
        let elem_size = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(elem_size),
            Rvalue::Use(Operand::Const(ConstValue::Int(8))),
            span,
        );
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("Vec::new".to_string())),
            args: vec![Operand::Copy(Place::local(elem_size))],
            destination: Place::local(pairs_vec),
            target: Some(next),
        });
        self.set_current(next);

        for (i, (name, &fty)) in field_names.iter().zip(field_tys.iter()).enumerate() {
            // Read the struct field by index projection.
            let field_local = self.fresh(fty);
            self.emit_assign(
                Place::local(field_local),
                Rvalue::Use(Operand::Copy(Place {
                    local: struct_local,
                    projection: vec![crate::ir::Projection::Field(i as u32)],
                })),
                span,
            );

            // Push field name as a cstr (String-typed local).
            let name_local = self.fresh(string_ty);
            self.emit_assign(
                Place::local(name_local),
                Rvalue::Use(Operand::Const(ConstValue::Str(name.clone()))),
                span,
            );
            let push_dest = self.fresh(unit_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                args: vec![
                    Operand::Copy(Place::local(pairs_vec)),
                    Operand::Copy(Place::local(name_local)),
                ],
                destination: Place::local(push_dest),
                target: Some(next),
            });
            self.set_current(next);

            // Convert field to *mut GosJson via the appropriate constructor.
            let mut flat_fty = fty;
            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat_fty) {
                flat_fty = *inner;
            }
            let json_helper: &'static str = match self.tcx.kind_of(flat_fty) {
                TyKind::Int(_) => "gos_rt_json_value_int",
                TyKind::Float(_) => "gos_rt_json_value_float",
                TyKind::Bool => "gos_rt_json_value_bool",
                TyKind::String => "gos_rt_json_value_string",
                TyKind::JsonValue => "gos_rt_json_identity",
                _ => "gos_rt_json_value_null",
            };
            let json_local = self.fresh(json_val_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(json_helper.to_string())),
                args: vec![Operand::Copy(Place::local(field_local))],
                destination: Place::local(json_local),
                target: Some(next),
            });
            self.set_current(next);

            // Push the *mut GosJson ptr into the pairs vec.
            let push_dest = self.fresh(unit_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_push".to_string())),
                args: vec![
                    Operand::Copy(Place::local(pairs_vec)),
                    Operand::Copy(Place::local(json_local)),
                ],
                destination: Place::local(push_dest),
                target: Some(next),
            });
            self.set_current(next);
        }

        // Build the json::Value object from the KV pairs vec.
        let json_obj = self.fresh(json_val_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_value_object".to_string())),
            args: vec![Operand::Copy(Place::local(pairs_vec))],
            destination: Place::local(json_obj),
            target: Some(next),
        });
        self.set_current(next);

        // Free the pairs vec immediately after use — it was only borrowed by
        // gos_rt_json_value_object, so we own it and must release it here.
        // Doing this inline (rather than relying on insert_drops_at_returns)
        // keeps the free inside the JSON arm only: the drop-at-return pass
        // operates on all return paths unconditionally, so a pairs_vec drop
        // at the Return block would also fire along the text-mode arm where
        // pairs_vec was never initialised, producing gos_rt_vec_free(garbage).
        let free_dest = self.fresh(unit_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_vec_free".to_string())),
            args: vec![Operand::Copy(Place::local(pairs_vec))],
            destination: Place::local(free_dest),
            target: Some(next),
        });
        self.set_current(next);
        // Re-assign pairs_vec to a sentinel so insert_drops_at_returns sees a
        // re-assignment and disqualifies it from emitting a second free at Return.
        self.emit_assign(
            Place::local(pairs_vec),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );

        // Render the json::Value to a compact JSON string.
        let result = self.fresh(string_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_render".to_string())),
            args: vec![Operand::Copy(Place::local(json_obj))],
            destination: Place::local(result),
            target: Some(next),
        });
        self.set_current(next);
        Some(result)
    }

    /// Boxes a scalar / array / json::Value `render`/`encode` argument
    /// into a `*GosJson` and renders it. Returns `None` for shapes this
    /// helper does not handle (e.g. nested aggregates), letting the
    /// caller fall through.
    pub(crate) fn lower_json_render_value(
        &mut self,
        args: &[HirExpr],
        value_ty: gossamer_types::Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let string_ty = self.tcx.string_ty();
        let json_val_ty = self.tcx.json_value_ty();
        let arg_local = self.lower_expr(&args[0])?;

        // Produce a `*GosJson` local from the argument.
        let json_local = match self.tcx.kind_of(value_ty).clone() {
            TyKind::JsonValue => arg_local,
            TyKind::Int(_) | TyKind::Bool | TyKind::Float(_) | TyKind::String => {
                let helper = match self.tcx.kind_of(value_ty) {
                    TyKind::Int(_) => "gos_rt_json_value_int",
                    TyKind::Bool => "gos_rt_json_value_bool",
                    TyKind::Float(_) => "gos_rt_json_value_float",
                    _ => "gos_rt_json_value_string",
                };
                let dest = self.fresh(json_val_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(helper.to_string())),
                    args: vec![Operand::Copy(Place::local(arg_local))],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                dest
            }
            TyKind::Vec(elem) | TyKind::Slice(elem) | TyKind::Array { elem, .. } => {
                let mut flat = elem;
                while let TyKind::Ref { inner, .. } = self.tcx.kind_of(flat) {
                    flat = *inner;
                }
                let kind: i64 = match self.tcx.kind_of(flat) {
                    TyKind::Float(_) => 1,
                    TyKind::String => 2,
                    TyKind::Bool => 3,
                    // Int, or an unresolved `Var` left by the typer on
                    // an integer array literal (`encode([1, 2, 3])`):
                    // default to the i64 slot reading. Numeric literals
                    // default to i64 in Gossamer, so this matches.
                    TyKind::Int(_) | TyKind::Var(_) | TyKind::Error => 0,
                    // Other non-scalar elements (struct, nested Vec):
                    // leave to the fallthrough rather than mis-encoding.
                    _ => return None,
                };
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let arg_local = {
                    let lt = self.locals[arg_local.0 as usize].ty;
                    if let TyKind::Array { elem, len } = self.tcx.kind_of(lt).clone() {
                        self.coerce_array_to_vec(arg_local, elem, len, span)
                    } else {
                        arg_local
                    }
                };
                let kind_local = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(kind_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(kind)))),
                    span,
                );
                let dest = self.fresh(json_val_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str(
                        "gos_rt_json_array_from_scalar_vec".to_string(),
                    )),
                    args: vec![
                        Operand::Copy(Place::local(arg_local)),
                        Operand::Copy(Place::local(kind_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                dest
            }
            _ => return None,
        };

        let result = self.fresh(string_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_json_render".to_string())),
            args: vec![Operand::Copy(Place::local(json_local))],
            destination: Place::local(result),
            target: Some(next),
        });
        self.set_current(next);
        Some(result)
    }

    pub(crate) fn maybe_coerce_json_value(
        &mut self,
        value: Local,
        target_ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let mut cur = target_ty;
        let kind = loop {
            match self.tcx.kind_of(cur) {
                TyKind::Ref { inner, .. } => cur = *inner,
                other => break other.clone(),
            }
        };
        let (helper, ret_ty) = match kind {
            TyKind::Int(_) => (
                "gos_rt_json_as_i64",
                self.tcx.int_ty(gossamer_types::IntTy::I64),
            ),
            TyKind::Float(_) => (
                "gos_rt_json_as_f64",
                self.tcx.float_ty(gossamer_types::FloatTy::F64),
            ),
            TyKind::Bool => ("gos_rt_json_as_bool", self.tcx.bool_ty()),
            TyKind::String => ("gos_rt_json_as_str", self.tcx.string_ty()),
            _ => return None,
        };
        Some(self.emit_single_arg_call(helper, value, ret_ty, span))
    }
}
