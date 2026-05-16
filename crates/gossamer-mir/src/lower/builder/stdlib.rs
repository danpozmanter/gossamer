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
    pub(crate) fn emit_single_arg_call(
        &mut self,
        name: &'static str,
        receiver: Local,
        ret_ty: Ty,
        span: Span,
    ) -> Local {
        let dest = self.fresh(ret_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(name.to_string())),
            args: vec![Operand::Copy(Place::local(receiver))],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        dest
    }

    pub(crate) fn coerce_to_fn_trait_if_needed(
        &mut self,
        source_local: Local,
        expected: Ty,
        span: Span,
    ) -> Local {
        use gossamer_types::TyKind;
        let expected_kind = self.tcx.kind_of(expected).clone();
        let sig_opt = match &expected_kind {
            TyKind::FnTrait(sig) | TyKind::FnPtr(sig) => Some(sig.clone()),
            _ => return source_local,
        };
        let Some(sig) = sig_opt else {
            return source_local;
        };
        let source_ty = self.locals[source_local.0 as usize].ty;
        let source_kind = self.tcx.kind_of(source_ty);
        // Wrap when the source is a genuine fn item (`FnDef`)
        // OR a local that the MIR builder marked as holding a
        // function-name string constant (the lift-closures pass
        // produces these for non-capturing closures: the local
        // ends up Copy(Const(Str("__closure_N"))) — a rodata
        // pointer, NOT a callable address). FnPtr-typed locals
        // are already env_ptr-shaped after this round of fixes,
        // so re-wrapping them would double-indirect; FnTrait and
        // Closure values are env-shaped by construction.
        let names_a_fn = self.local_fn_name.contains_key(&source_local);
        let needs_wrap = matches!(source_kind, TyKind::FnDef { .. }) || names_a_fn;
        if !needs_wrap {
            return source_local;
        }
        let env_ty = expected;
        // Allocate the env blob: 16 bytes (thunk ptr + real fn ptr).
        let size_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(size_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(16))),
            span,
        );
        let env_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(env_local),
            Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(Place::local(size_local))],
            },
            span,
        );
        // Resolve the per-shape thunk name. Encodes input + return
        // types so the backend can synthesize a thunk with the
        // correct calling convention regardless of arg / ret types.
        let thunk_name = mangle_callable_shape(self.tcx, &sig);
        let tramp_addr_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(tramp_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(thunk_name))],
            },
            span,
        );
        let zero_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(zero_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let sink_a = self.fresh(env_ty);
        self.emit_assign(
            Place::local(sink_a),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(zero_local)),
                    Operand::Copy(Place::local(tramp_addr_local)),
                ],
            },
            span,
        );
        let eight_local = self.fresh(env_ty);
        self.emit_assign(
            Place::local(eight_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(8))),
            span,
        );
        // When the source local was bound to a fn name via
        // `let c = some_fn_name` (e.g. a lifted non-capturing
        // closure), its slot holds the address of the *string*
        // (the way the MIR encodes a `def: None` path), not the
        // function. Resolve to the real fn address via
        // `gos_fn_addr` so the trampoline forwards to the actual
        // code. Direct fn references (FnDef/FnPtr-typed locals)
        // already hold the right value.
        let real_fn_operand = if let Some(name) = self.local_fn_name.get(&source_local).cloned() {
            let addr_local = self.fresh(env_ty);
            self.emit_assign(
                Place::local(addr_local),
                Rvalue::CallIntrinsic {
                    name: "gos_fn_addr",
                    args: vec![Operand::Const(ConstValue::Str(name))],
                },
                span,
            );
            Operand::Copy(Place::local(addr_local))
        } else {
            Operand::Copy(Place::local(source_local))
        };
        let sink_b = self.fresh(env_ty);
        self.emit_assign(
            Place::local(sink_b),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(env_local)),
                    Operand::Copy(Place::local(eight_local)),
                    real_fn_operand,
                ],
            },
            span,
        );
        env_local
    }

    pub(crate) fn lower_http_serve(
        &mut self,
        addr_expr: &HirExpr,
        handler_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let addr_local = self.lower_expr(addr_expr)?;
        let handler_local = self.lower_expr(handler_expr)?;
        // If the handler is a stateful runtime type (Router /
        // FileServer / Proxy), its `serve` method lives in
        // gossamer-runtime, not in user code. Pick the matching
        // gos_rt_* runtime symbol; otherwise fall back to the
        // user-defined `{T}::serve` lookup.
        let handler_runtime_kind = self.local_runtime_kind.get(&handler_local).copied();
        let serve_fn_name = match handler_runtime_kind {
            Some("http::Router") => "gos_rt_router_serve".to_string(),
            Some("http::FileServer") => "gos_rt_file_server_serve".to_string(),
            Some("http::Proxy") => "gos_rt_proxy_forward".to_string(),
            _ => {
                let handler_ty = self.locals[handler_local.0 as usize].ty;
                let handler_struct = self.struct_name_of(handler_ty)?;
                format!("{handler_struct}::serve")
            }
        };
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let fn_addr_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(serve_fn_name))],
            },
            span,
        );
        let unit_ty = self.tcx.unit();
        let dest = self.fresh(unit_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_http_serve".to_string())),
            args: vec![
                Operand::Copy(Place::local(addr_local)),
                Operand::Copy(Place::local(handler_local)),
                Operand::Copy(Place::local(fn_addr_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn lower_http2_bind_and_run_h2c(
        &mut self,
        addr_expr: &HirExpr,
        handler_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let addr_local = self.lower_expr(addr_expr)?;
        let handler_local = self.lower_expr(handler_expr)?;
        let handler_ty = self.locals[handler_local.0 as usize].ty;
        let handler_struct = self.struct_name_of(handler_ty)?;
        let serve_fn_name = format!("{handler_struct}::serve");
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let fn_addr_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(fn_addr_local),
            Rvalue::CallIntrinsic {
                name: "gos_fn_addr",
                args: vec![Operand::Const(ConstValue::Str(serve_fn_name))],
            },
            span,
        );
        let unit_ty = self.tcx.unit();
        let dest = self.fresh(unit_ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_http2_bind_and_run_h2c".to_string())),
            args: vec![
                Operand::Copy(Place::local(addr_local)),
                Operand::Copy(Place::local(handler_local)),
                Operand::Copy(Place::local(fn_addr_local)),
            ],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }

    pub(crate) fn lower_flag_define(
        &mut self,
        name_expr: &HirExpr,
        specs_expr: &HirExpr,
        _ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Only handle the inline-array literal form. Dynamic specs
        // would need real runtime support — fall through to the
        // generic call dispatch (which produces a stub) so the rest
        // of the program still compiles.
        let HirExprKind::Array(gossamer_hir::HirArrayExpr::List(specs)) = &specs_expr.kind else {
            return None;
        };

        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);

        // Step 1: `flag::Set::new(name)` -> set_local.
        let name_local = self.lower_expr(name_expr)?;
        let set_local = self.fresh(i64_ty);
        let after_new = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_flag_set_new".to_string())),
            args: vec![Operand::Copy(Place::local(name_local))],
            destination: Place::local(set_local),
            target: Some(after_new),
        });
        self.set_current(after_new);
        self.local_runtime_kind.insert(set_local, "flag::Set");

        // Step 2: walk each spec, register it on the set, and stash
        // the resulting cell local + kind for the aggregate that
        // becomes the return value.
        let mut layout: Vec<(String, &'static str)> = Vec::with_capacity(specs.len());
        let mut cell_locals: Vec<Local> = Vec::with_capacity(specs.len());
        for spec in specs {
            let HirExprKind::Call {
                callee: spec_callee,
                args: spec_args,
            } = &spec.kind
            else {
                return None;
            };
            let HirExprKind::Path {
                segments: spec_segs,
                ..
            } = &spec_callee.kind
            else {
                return None;
            };
            let spec_path: String = spec_segs
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::");
            let (rt_helper, cell_kind, default_ty): (&'static str, &'static str, Ty) =
                match spec_path.as_str() {
                    "flag::int" => ("gos_rt_flag_set_int", "flag::Cell::Int", i64_ty),
                    "flag::string" => (
                        "gos_rt_flag_set_string",
                        "flag::Cell::String",
                        self.tcx.string_ty(),
                    ),
                    "flag::bool" => (
                        "gos_rt_flag_set_bool",
                        "flag::Cell::Bool",
                        self.tcx.bool_ty(),
                    ),
                    _ => return None,
                };
            // spec args: (long, default, help, short)
            if spec_args.len() < 3 {
                return None;
            }
            let long_local = self.lower_expr(&spec_args[0])?;
            let default_local = self.lower_expr(&spec_args[1])?;
            let help_local = self.lower_expr(&spec_args[2])?;
            let short_expr = spec_args.get(3);

            // Recover the long-name string for the layout table. Only
            // string-literal long names are supported; dynamic names
            // would have to be looked up at runtime, defeating the
            // purpose of a static field map.
            let long_name = match &spec_args[0].kind {
                HirExprKind::Literal(gossamer_hir::HirLiteral::String(s)) => s.clone(),
                _ => return None,
            };

            // Coerce the default operand's type so cranelift's
            // signature for the runtime helper picks the right ABI
            // slot (string ptr / i64 / i8). The lowered local already
            // has the literal's type from `lower_expr`; the helper
            // still wants its own pinned ABI.
            let _ = default_ty;

            let cell_local = self.fresh(i64_ty);
            let after_reg = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(rt_helper.to_string())),
                args: vec![
                    Operand::Copy(Place::local(set_local)),
                    Operand::Copy(Place::local(long_local)),
                    Operand::Copy(Place::local(default_local)),
                    Operand::Copy(Place::local(help_local)),
                ],
                destination: Place::local(cell_local),
                target: Some(after_reg),
            });
            self.set_current(after_reg);
            self.local_runtime_kind.insert(cell_local, cell_kind);

            // Optional short alias: `flag::int("count", ..., 'c')`.
            if let Some(short_arg) = short_expr {
                if let HirExprKind::Literal(gossamer_hir::HirLiteral::Char(ch)) = &short_arg.kind {
                    let short_local = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(short_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(*ch as u32)))),
                        span,
                    );
                    let unit_ty = self.tcx.unit();
                    let unit_dest = self.fresh(unit_ty);
                    let after_short = self.new_block(span);
                    self.terminate(Terminator::Call {
                        callee: Operand::Const(ConstValue::Str(
                            "gos_rt_flag_set_short".to_string(),
                        )),
                        args: vec![
                            Operand::Copy(Place::local(set_local)),
                            Operand::Copy(Place::local(short_local)),
                        ],
                        destination: Place::local(unit_dest),
                        target: Some(after_short),
                    });
                    self.set_current(after_short);
                }
            }

            layout.push((long_name, cell_kind));
            cell_locals.push(cell_local);
        }

        // Step 3: `set.parse(os::args())`. `gos_rt_os_args` returns
        // a pointer the parser recognises as the `argv + 1`
        // sentinel.
        let args_local = self.fresh(i64_ty);
        let after_args = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_os_args".to_string())),
            args: vec![],
            destination: Place::local(args_local),
            target: Some(after_args),
        });
        self.set_current(after_args);
        let parse_dest = self.fresh(i64_ty);
        let after_parse = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str("gos_rt_flag_set_parse".to_string())),
            args: vec![
                Operand::Copy(Place::local(set_local)),
                Operand::Copy(Place::local(args_local)),
            ],
            destination: Place::local(parse_dest),
            target: Some(after_parse),
        });
        self.set_current(after_parse);

        // Step 4: build an aggregate of cell pointers as the result.
        // The aggregate's local picks up the layout table so field
        // access dispatches positionally. Each cell is i64-shaped
        // (a pointer) for codegen purposes.
        let agg_ty = self.tcx.intern(gossamer_types::TyKind::Tuple(
            (0..cell_locals.len()).map(|_| i64_ty).collect(),
        ));
        let dest = self.fresh(agg_ty);
        let operands: Vec<Operand> = cell_locals
            .iter()
            .map(|l| Operand::Copy(Place::local(*l)))
            .collect();
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::AggregateKind::Tuple,
                operands,
            },
            span,
        );
        self.local_runtime_kind.insert(dest, "flag::DefineResult");
        self.local_define_layout.insert(dest, layout);
        Some(dest)
    }

    pub(crate) fn lower_result_no_payload(
        &mut self,
        disc: i64,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let disc_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(disc_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(disc)))),
            span,
        );
        let zero_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::CallIntrinsic {
                name: "gos_rt_result_new",
                args: vec![
                    Operand::Copy(Place::local(disc_local)),
                    Operand::Copy(Place::local(zero_local)),
                ],
            },
            span,
        );
        Some(dest)
    }

    pub(crate) fn lower_user_enum_ctor(
        &mut self,
        variant_idx: u32,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let n_args = args.len();
        let bytes = ((n_args + 1) * 8) as i128;
        let size_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(size_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(bytes))),
            span,
        );
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(Place::local(size_local))],
            },
            span,
        );
        // Write disc at offset 0
        let disc_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(disc_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(variant_idx)))),
            span,
        );
        let zero_off = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(zero_off),
            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
            span,
        );
        let unit_ty = self.tcx.unit();
        let store_dest = self.fresh(unit_ty);
        self.emit_assign(
            Place::local(store_dest),
            Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(Place::local(dest)),
                    Operand::Copy(Place::local(zero_off)),
                    Operand::Copy(Place::local(disc_local)),
                ],
            },
            span,
        );
        // Write each payload at offset (i+1)*8
        for (i, arg) in args.iter().enumerate() {
            let payload_local = self.lower_expr(arg)?;
            let off_local = self.fresh(i64_ty);
            self.emit_assign(
                Place::local(off_local),
                Rvalue::Use(Operand::Const(ConstValue::Int(((i + 1) * 8) as i128))),
                span,
            );
            let payload_dest = self.fresh(unit_ty);
            self.emit_assign(
                Place::local(payload_dest),
                Rvalue::CallIntrinsic {
                    name: "gos_store",
                    args: vec![
                        Operand::Copy(Place::local(dest)),
                        Operand::Copy(Place::local(off_local)),
                        Operand::Copy(Place::local(payload_local)),
                    ],
                },
                span,
            );
        }
        Some(dest)
    }

    pub(crate) fn lower_result_ctor(
        &mut self,
        disc: i64,
        payload_expr: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if std::env::var("GOS_MIR_TRACE").is_ok() {
            eprintln!("[lower_result_ctor] disc={disc}");
        }
        let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
        let disc_local = self.fresh(i64_ty);
        self.emit_assign(
            Place::local(disc_local),
            Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(disc)))),
            span,
        );
        let payload_local = self.lower_expr(payload_expr)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::CallIntrinsic {
                name: "gos_rt_result_new",
                args: vec![
                    Operand::Copy(Place::local(disc_local)),
                    Operand::Copy(Place::local(payload_local)),
                ],
            },
            span,
        );
        Some(dest)
    }
}
