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
    pub(crate) fn lower_expr(&mut self, expr: &HirExpr) -> Option<Local> {
        match &expr.kind {
            HirExprKind::Literal(lit) => Some(self.lower_literal(lit, expr.ty, expr.span)),
            HirExprKind::Path { segments, def } => {
                self.lower_path(segments, *def, expr.ty, expr.span)
            }
            HirExprKind::Unary { op, operand } => {
                self.lower_unary(*op, operand, expr.ty, expr.span)
            }
            HirExprKind::Binary { op, lhs, rhs } => {
                self.lower_binary(*op, lhs, rhs, expr.ty, expr.span)
            }
            HirExprKind::Assign { place, value } => {
                self.lower_assign(place, value, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Call { callee, args } => self.lower_call(callee, args, expr.ty, expr.span),
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.lower_if(
                condition,
                then_branch,
                else_branch.as_deref(),
                expr.ty,
                expr.span,
            ),
            HirExprKind::While { condition, body } => {
                self.lower_while(condition, body, expr.span);
                Some(self.lower_unit(expr.span))
            }
            HirExprKind::Loop { body } => self.lower_loop(body, expr.ty, expr.span),
            HirExprKind::Block(block) => self.lower_block(block),
            HirExprKind::Return(value) => {
                if let Some(value) = value {
                    if let Some(mut local) = self.lower_expr(value) {
                        // When the function's declared return type
                        // is a callable shape (`fn(...) -> ...` /
                        // `Fn(...) -> ...`) and the returned value
                        // is a bare fn item, wrap it in the env+
                        // code blob so the caller's slot uniformly
                        // carries an env_ptr.
                        use gossamer_types::TyKind;
                        let ret_ty = self.locals[Local::RETURN.0 as usize].ty;
                        let value_ty = self.locals[local.0 as usize].ty;
                        let dest_callable = matches!(
                            self.tcx.kind_of(ret_ty),
                            TyKind::FnPtr(_) | TyKind::FnTrait(_)
                        );
                        let src_is_fn_def =
                            matches!(self.tcx.kind_of(value_ty), TyKind::FnDef { .. });
                        let src_names_fn = self.local_fn_name.contains_key(&local);
                        if dest_callable && (src_is_fn_def || src_names_fn) {
                            local = self.coerce_to_fn_trait_if_needed(local, ret_ty, expr.span);
                        }
                        // Array-literal → Vec coercion. `fn cols()
                        // -> [String] { return ["a", "b"] }` lowers
                        // the literal as a flat `Array<String; 2>`
                        // because `lower_array_list` only takes the
                        // Vec path when `expr.ty` is already
                        // `Vec(_)`/`Slice(_)`. Without an explicit
                        // coercion at the return site, the caller
                        // reads the stack-Array bytes as a GosVec
                        // header and sees len=0 / segfaults. Detect
                        // the (Array, Vec) shape mismatch and route
                        // the value through `coerce_array_to_vec`
                        // (which calls `gos_rt_vec_from_arr`).
                        if let TyKind::Array { elem, len } = self.tcx.kind_of(value_ty).clone() {
                            let target_elem = match self.tcx.kind_of(ret_ty) {
                                TyKind::Vec(e) | TyKind::Slice(e) => Some(*e),
                                _ => None,
                            };
                            if target_elem == Some(elem) {
                                local = self.coerce_array_to_vec(local, elem, len, expr.span);
                            }
                        }
                        self.emit_assign(
                            Place::local(Local::RETURN),
                            Rvalue::Use(Operand::Copy(Place::local(local))),
                            expr.span,
                        );
                    }
                }
                self.terminate(Terminator::Return);
                None
            }
            HirExprKind::Break(payload) => {
                // Jump to the innermost loop's break target. Outside
                // a loop the resolver/typechecker is supposed to
                // reject this; if it slips through, fall back to
                // `Unreachable` rather than emit a dangling jump.
                let (break_to, result_local) = if let Some(ctx) = self.loop_stack.last_mut() {
                    ctx.break_used = true;
                    (ctx.break_to, ctx.result)
                } else {
                    self.terminate(Terminator::Unreachable);
                    return None;
                };
                if let (Some(value), Some(result)) = (payload, result_local) {
                    if let Some(value_local) = self.lower_expr(value) {
                        self.emit_assign(
                            Place::local(result),
                            Rvalue::Use(Operand::Copy(Place::local(value_local))),
                            expr.span,
                        );
                    }
                }
                self.terminate(Terminator::Goto { target: break_to });
                None
            }
            HirExprKind::Continue => {
                if let Some(ctx) = self.loop_stack.last().copied() {
                    self.terminate(Terminator::Goto {
                        target: ctx.continue_to,
                    });
                } else {
                    self.terminate(Terminator::Unreachable);
                }
                None
            }
            HirExprKind::Tuple(elems) => self.lower_tuple(elems, expr.ty, expr.span),
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                self.lower_array_list(elems, expr.ty, expr.span)
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                self.lower_array_repeat(value, count, expr.ty, expr.span)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                self.lower_tuple_index(receiver, *index, expr.ty, expr.span)
            }
            HirExprKind::Index { base, index } => {
                self.lower_index_access(base, index, expr.ty, expr.span)
            }
            HirExprKind::Match { scrutinee, arms } => {
                self.lower_match(scrutinee, arms, expr.ty, expr.span)
            }
            HirExprKind::Cast { value, ty: target } => {
                self.lower_cast(value, *target, expr.ty, expr.span)
            }
            HirExprKind::Field { receiver, name } => {
                self.lower_field_access(receiver, name, expr.ty, expr.span)
            }
            HirExprKind::LiftedClosure { name, captures } => {
                self.lower_lifted_closure(name, captures, expr.ty, expr.span)
            }
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
            } => self.lower_method_call(receiver, name, args, expr.ty, expr.span),
            HirExprKind::Go(inner) => {
                let go_span = expr.span;
                // Real spawn for `go f(args)` where f is a named
                // function with 0-2 scalar args: emit a call to
                // `gos_rt_go_spawn_call_N(fn_addr, args…)`. The
                // runtime helper transmutes fn_addr back to
                // `extern "C" fn(...) -> i64` and runs it on a
                // fresh OS thread.
                //
                // Anything more complex (closure captures, >2
                // args, method calls) falls back to synchronous
                // execution so the program still runs — sound
                // for single-threaded workloads.
                if let HirExprKind::Call { callee, args } = &inner.kind {
                    if let HirExprKind::Path { def: Some(def), .. } = &callee.kind {
                        if args.len() <= 6 {
                            let sym: &'static str = match args.len() {
                                0 => "gos_rt_go_spawn_call_0",
                                1 => "gos_rt_go_spawn_call_1",
                                2 => "gos_rt_go_spawn_call_2",
                                3 => "gos_rt_go_spawn_call_3",
                                4 => "gos_rt_go_spawn_call_4",
                                5 => "gos_rt_go_spawn_call_5",
                                _ => "gos_rt_go_spawn_call_6",
                            };
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let fn_addr_local = self.fresh(i64_ty);
                            let substs = self.substs_of(callee.ty);
                            self.emit_assign(
                                Place::local(fn_addr_local),
                                Rvalue::Use(Operand::FnRef { def: *def, substs }),
                                go_span,
                            );
                            let mut operands = Vec::with_capacity(args.len() + 1);
                            operands.push(Operand::Copy(Place::local(fn_addr_local)));
                            for arg in args {
                                let mut a = self.lower_expr(arg)?;
                                let lt = self.locals[a.0 as usize].ty;
                                if let gossamer_types::TyKind::Array { elem, len } =
                                    self.tcx.kind_of(lt).clone()
                                {
                                    a = self.coerce_array_to_vec(a, elem, len, go_span);
                                }
                                operands.push(Operand::Copy(Place::local(a)));
                            }
                            let unit_ty = self.tcx.unit();
                            let dest = self.fresh(unit_ty);
                            let next = self.new_block(go_span);
                            self.terminate(Terminator::Call {
                                callee: Operand::Const(ConstValue::Str(sym.to_string())),
                                args: operands,
                                destination: Place::local(dest),
                                target: Some(next),
                            });
                            self.set_current(next);
                            return Some(dest);
                        }
                    }
                }
                // Fallback: synchronous.
                let _ = self.lower_expr(inner);
                Some(self.lower_unit(go_span))
            }
            HirExprKind::Select { arms } => {
                // Sequential stub: run each arm's side-effects and
                // then the first arm's body. The real runtime will
                // pick the first ready channel, but under the
                // single-task stub we just pretend arm 0 fired.
                use gossamer_hir::HirSelectOp;
                let mut result: Option<Local> = None;
                for (i, arm) in arms.iter().enumerate() {
                    match &arm.op {
                        HirSelectOp::Recv { channel, .. } | HirSelectOp::Send { channel, .. } => {
                            let _ = self.lower_expr(channel);
                        }
                        HirSelectOp::Default => {}
                    }
                    if i == 0 {
                        result = self.lower_expr(&arm.body);
                    }
                }
                result.or_else(|| Some(self.lower_unit(expr.span)))
            }
            HirExprKind::Range {
                start,
                end,
                inclusive,
            } => {
                // Standalone Range value. The compiled tier
                // represents a Range as a 2-i64 tuple
                // `(lo, hi)`. Open-ended bounds default to 0
                // for `lo` and `i64::MAX` for `hi`. Used by
                // slice expressions like `arr[1..]` which the
                // surrounding Index lowering picks the bounds
                // out of.
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let lo_local = if let Some(s) = start {
                    self.lower_expr(s)?
                } else {
                    let l = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(l),
                        Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                        expr.span,
                    );
                    l
                };
                let hi_local = if let Some(e) = end {
                    self.lower_expr(e)?
                } else {
                    let l = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(l),
                        Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(i64::MAX)))),
                        expr.span,
                    );
                    l
                };
                // Bump `hi` for inclusive ranges so the half-
                // open `[lo, hi)` interpretation downstream
                // doesn't drop the last element.
                let hi_local = if *inclusive {
                    let one = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(one),
                        Rvalue::Use(Operand::Const(ConstValue::Int(1))),
                        expr.span,
                    );
                    let bumped = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(bumped),
                        Rvalue::BinaryOp {
                            op: BinOp::Add,
                            lhs: Operand::Copy(Place::local(hi_local)),
                            rhs: Operand::Copy(Place::local(one)),
                        },
                        expr.span,
                    );
                    bumped
                } else {
                    hi_local
                };
                let dest = self.fresh(expr.ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Aggregate {
                        kind: crate::ir::AggregateKind::Tuple,
                        operands: vec![
                            Operand::Copy(Place::local(lo_local)),
                            Operand::Copy(Place::local(hi_local)),
                        ],
                    },
                    expr.span,
                );
                Some(dest)
            }
            // The native build pipeline runs `gossamer_hir::lift_closures`
            // upstream, so by the time we lower a Closure here we are
            // either in the VM's pre-JIT pass (which never executes the
            // resulting MIR — execution stays on the tree-walker) or
            // an unreachable path. Emit a zero-shaped placeholder so
            // pre-pass lowering succeeds without claiming to lower the
            // closure semantically. Same shape for the resolver's
            // `Placeholder` sentinel: parse / resolve diagnostics halt
            // the build before a real run, so any survivor reaches MIR
            // only on the VM's no-execute pre-pass.
            HirExprKind::Closure { .. } | HirExprKind::Placeholder => {
                let dest = self.fresh(expr.ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    expr.span,
                );
                Some(dest)
            }
        }
    }

    pub(crate) fn lower_literal(&mut self, lit: &HirLiteral, ty: Ty, span: Span) -> Local {
        // Pin the literal's MIR type to the concrete kind the
        // literal implies, not the HIR expression's `ty` which may
        // still be an unresolved inference variable. Downstream
        // passes (string-concat detection, cranelift type
        // inference) rely on this being grounded.
        use gossamer_types::{FloatTy as Ft, IntTy as It, TyKind};
        let concrete = match lit {
            HirLiteral::String(_) => Some(self.tcx.string_ty()),
            HirLiteral::Bool(_) => Some(self.tcx.bool_ty()),
            HirLiteral::Char(_) => Some(self.tcx.char_ty()),
            HirLiteral::Unit => Some(self.tcx.unit()),
            _ => None,
        };
        let local_ty = match concrete {
            Some(concrete_ty) => concrete_ty,
            None => match self.tcx.kind_of(ty) {
                TyKind::Int(_) | TyKind::Float(_) => ty,
                _ => match lit {
                    HirLiteral::Int(_) => self.tcx.int_ty(It::I64),
                    HirLiteral::Float(_) => self.tcx.float_ty(Ft::F64),
                    _ => ty,
                },
            },
        };
        let local = self.fresh(local_ty);
        let value = literal_to_const(lit);
        self.emit_assign(
            Place::local(local),
            Rvalue::Use(Operand::Const(value)),
            span,
        );
        local
    }

    pub(crate) fn lower_path(
        &mut self,
        segments: &[Ident],
        def: Option<gossamer_resolve::DefId>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        if let Some(first) = segments.first() {
            if let Some(local) = self.lookup_local(&first.name) {
                return Some(local);
            }
        }
        // `None` as a value (no payload) lowers to a heap-allocated
        // `gos_rt_result_new(1, 0)` so the match disc check can
        // distinguish it from `Some(_)`.
        if let Some(last) = segments.last() {
            if last.name.as_str() == "None" && segments.len() == 1 {
                return self.lower_result_no_payload(1, ty, span);
            }
        }
        // Enum variant constructor (no-payload form): `Color::Green`
        // / `List::Nil`. When the enum has any payload-bearing
        // sibling, allocate a one-word `[disc]` heap aggregate so
        // match dispatch can uniformly load disc from offset 0.
        // Otherwise emit the variant index directly as i64.
        if let Some((enum_name, idx)) = self.enums.lookup(segments) {
            if self.enums.has_any_payload(segments) {
                return self.lower_user_enum_ctor(
                    &enum_name,
                    u32::try_from(idx).unwrap_or(0),
                    &[],
                    ty,
                    span,
                );
            }
            let int_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let local = self.push_local(int_ty, None, false);
            self.emit_assign(
                Place::local(local),
                Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                span,
            );
            return Some(local);
        }
        // `json::Value::Null` (path expression, no parens) is a unit
        // variant constructor for the stdlib `json::Value` enum.
        // The user-enum path above doesn't catch it because
        // `json::Value` isn't declared in the program — it lives in
        // `gossamer-std`. Without this arm the path falls through to
        // the FnRef fallback below, producing a function-pointer
        // value that downstream code interpreted as a `*mut GosJson`
        // and segfaulted on dereference (askq's
        // `json::parse(...).unwrap_or(json::Value::Null)`). Route
        // through the runtime constructor instead so the resulting
        // local holds a real, dereferenceable GosJson handle.
        let names: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
        let strip_std: &[&str] = if names.first() == Some(&"std") {
            &names[1..]
        } else {
            &names[..]
        };
        let json_unit = matches!(
            (
                strip_std.first(),
                strip_std.get(1),
                strip_std.get(2),
                strip_std.last()
            ),
            (Some(&"json"), Some(&"Value"), Some(&"Null"), _)
                | (Some(&"encoding"), _, _, Some(&"Null"))
        ) && strip_std.last() == Some(&"Null")
            && (strip_std.len() == 3 || strip_std.len() == 4);
        if json_unit {
            let json_ty = self.tcx.json_value_ty();
            let dest = self.fresh(json_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_json_value_null".to_string())),
                args: Vec::new(),
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // When the typechecker leaves a path-expr's type as `Var(_)`
        // — common for paths that resolve to `const` / `static`
        // items because the const-value pass runs after typeck —
        // pin the local's MIR type from the folded `ConstValue`'s
        // shape. Without this, the local stays `Var` and downstream
        // dispatch (operand_print_kind, format-helper selection)
        // falls through to a default that treats the value as a
        // c-string pointer; passing an integer there segfaults the
        // first strlen.
        let pinned_ty = if matches!(self.tcx.kind_of(ty), gossamer_types::TyKind::Var(_))
            && let Some(def) = def
            && let Some(value) = self.consts.get(&def)
        {
            match value {
                ConstValue::Int(_) => self.tcx.int_ty(gossamer_types::IntTy::I64),
                ConstValue::Float(_) => self.tcx.float_ty(gossamer_types::FloatTy::F64),
                ConstValue::Bool(_) => self.tcx.bool_ty(),
                ConstValue::Char(_) => self.tcx.char_ty(),
                ConstValue::Str(_) => self.tcx.string_ty(),
                ConstValue::Unit => ty,
            }
        } else {
            ty
        };
        let local = self.fresh(pinned_ty);
        let joined_name = segments
            .iter()
            .map(|s| s.name.as_str())
            .collect::<Vec<_>>()
            .join("::");
        let operand = if let Some(def) = def {
            // A path that resolves to a top-level `const` item
            // inlines the literal value here. Without this, the
            // FnRef fallback below would treat the const like a
            // function pointer and the codegen would emit zero
            // (or a string-tag pointer) at every use site.
            if let Some(value) = self.consts.get(&def) {
                Operand::Const(value.clone())
            } else {
                // Path resolves to a named item. Record the joined
                // name when the result is function-shaped so a later
                // call site (closure env, router handler, etc.) can
                // round-trip back to the symbol via `gos_fn_addr`.
                if matches!(
                    self.tcx.kind_of(pinned_ty),
                    gossamer_types::TyKind::FnDef { .. }
                        | gossamer_types::TyKind::FnPtr(_)
                        | gossamer_types::TyKind::FnTrait(_)
                        | gossamer_types::TyKind::Closure { .. }
                ) {
                    self.local_fn_name.insert(local, joined_name.clone());
                }
                Operand::FnRef {
                    def,
                    substs: self.substs_of(pinned_ty),
                }
            }
        } else {
            // Record that `local` holds a function-name constant
            // so a later `let` binding + call can still dispatch
            // directly to the named function without treating
            // the local as a closure env pointer.
            self.local_fn_name.insert(local, joined_name.clone());
            Operand::Const(ConstValue::Str(joined_name))
        };
        self.emit_assign(Place::local(local), Rvalue::Use(operand), span);
        Some(local)
    }

    pub(crate) fn lower_unary(
        &mut self,
        op: HirUnaryOp,
        operand: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let inner = self.lower_expr(operand)?;
        let mir_op = match op {
            HirUnaryOp::Neg => UnOp::Neg,
            HirUnaryOp::Not => UnOp::Not,
            HirUnaryOp::RefShared | HirUnaryOp::RefMut => {
                // For aggregate-typed operands (Vec, String, HashMap,
                // struct, …) and opaque-handle Adts (Regex, SqlDb,
                // Channel — all stored as i64 locals carrying a
                // ptr-shaped value) the existing `inner` local already
                // holds the canonical pointer the callee expects, so
                // `&x` is a no-op.
                //
                // For `&mut`-on-named-place-of-scalar (i.e.
                // `&mut state` where `state: i64`), the callee
                // genuinely wants a pointer that lets it write back
                // — without this, deref-assign through the borrowed
                // ref lands on the value-as-ptr and segfaults. Emit
                // `Rvalue::Ref` so the backend pulls a real slot
                // address.
                //
                // We restrict the Rvalue::Ref path to `&mut` on
                // genuine place expressions (path / field / index /
                // nested deref) for SCALAR operands. Shared `&` on a
                // literal or temporary keeps the historical
                // value-passthrough so existing dispatch sites (e.g.
                // `map.get(&k)` lowering to `gos_rt_map_get_i64(m,
                // k_value)`) continue to work.
                let scalar = matches!(
                    self.tcx.kind_of(operand.ty),
                    gossamer_types::TyKind::Int(_)
                        | gossamer_types::TyKind::Float(_)
                        | gossamer_types::TyKind::Bool
                        | gossamer_types::TyKind::Char
                );
                let is_place_expr = matches!(
                    operand.kind,
                    HirExprKind::Path { .. }
                        | HirExprKind::Field { .. }
                        | HirExprKind::TupleIndex { .. }
                        | HirExprKind::Index { .. }
                        | HirExprKind::Unary {
                            op: HirUnaryOp::Deref,
                            ..
                        }
                );
                if !(scalar && matches!(op, HirUnaryOp::RefMut) && is_place_expr) {
                    return Some(inner);
                }
                let place = self
                    .lower_place_expr(operand)
                    .unwrap_or(Place::local(inner));
                let dest = self.fresh(ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::Ref {
                        mutable: true,
                        place,
                    },
                    span,
                );
                return Some(dest);
            }
            HirUnaryOp::Deref => {
                let cell_kind = self.local_runtime_kind.get(&inner).copied();
                let (helper, dest_ty): (Option<&'static str>, Ty) = match cell_kind {
                    Some("flag::Cell::String") => {
                        (Some("gos_rt_flag_cell_load_str"), self.tcx.string_ty())
                    }
                    Some("flag::Cell::Int" | "flag::Cell::Uint" | "flag::Cell::Duration") => (
                        Some("gos_rt_flag_cell_load_i64"),
                        self.tcx.int_ty(gossamer_types::IntTy::I64),
                    ),
                    Some("flag::Cell::Bool") => {
                        (Some("gos_rt_flag_cell_load_bool"), self.tcx.bool_ty())
                    }
                    Some("flag::Cell::Float") => (
                        Some("gos_rt_flag_cell_load_f64"),
                        self.tcx.float_ty(gossamer_types::FloatTy::F64),
                    ),
                    Some("flag::Cell::StringList") => {
                        let s = self.tcx.string_ty();
                        (
                            Some("gos_rt_flag_cell_load_vec"),
                            self.tcx.intern(gossamer_types::TyKind::Vec(s)),
                        )
                    }
                    _ => (None, ty),
                };
                if let Some(helper_name) = helper {
                    let dest = self.fresh(dest_ty);
                    self.emit_assign(
                        Place::local(dest),
                        Rvalue::CallIntrinsic {
                            name: helper_name,
                            args: vec![Operand::Copy(Place::local(inner))],
                        },
                        span,
                    );
                    return Some(dest);
                }
                // Real reference deref: when the inner local has
                // type `&T` (taken via `&x` or yielded by an
                // iterator like `vec.iter()`), `*p` must load from
                // the address rather than yield the pointer.
                // Without this load, `for x in v.iter() { *x }`
                // prints the iterator's slot pointer, not the
                // element. Apply only to scalar inner types
                // (i64/f64/bool/char) — aggregate refs are passed
                // by pointer in this codegen and have their own
                // projection paths.
                let inner_ty = self.locals[inner.0 as usize].ty;
                if let gossamer_types::TyKind::Ref { inner: pointee, .. } =
                    self.tcx.kind_of(inner_ty)
                {
                    let pointee = *pointee;
                    if matches!(
                        self.tcx.kind_of(pointee),
                        gossamer_types::TyKind::Int(_)
                            | gossamer_types::TyKind::Float(_)
                            | gossamer_types::TyKind::Bool
                            | gossamer_types::TyKind::Char
                    ) {
                        let zero_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                        let zero = self.fresh(zero_ty);
                        self.emit_assign(
                            Place::local(zero),
                            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                            span,
                        );
                        let dest = self.fresh(pointee);
                        self.emit_assign(
                            Place::local(dest),
                            Rvalue::CallIntrinsic {
                                name: "gos_load",
                                args: vec![
                                    Operand::Copy(Place::local(inner)),
                                    Operand::Copy(Place::local(zero)),
                                ],
                            },
                            span,
                        );
                        return Some(dest);
                    }
                }
                return Some(inner);
            }
        };
        // When the HIR type is unresolved (`Var(_)`) the unary
        // result inherits the operand's type — `!bool` is `bool`,
        // `-i64` is `i64`, etc. Without this fallback the
        // destination local is `Var`/ptr-shaped, and downstream
        // print kinds route the i1 result through `print_str`
        // (treating the bit as a string pointer) rather than
        // `print_bool`, segfaulting in `strlen` on `0x1`.
        let ty = if matches!(self.tcx.kind_of(ty), TyKind::Var(_) | TyKind::Error) {
            let inner_ty = self.locals[inner.0 as usize].ty;
            if matches!(
                self.tcx.kind_of(inner_ty),
                TyKind::Bool | TyKind::Int(_) | TyKind::Float(_) | TyKind::Char
            ) {
                inner_ty
            } else {
                ty
            }
        } else {
            ty
        };
        let local = self.fresh(ty);
        self.emit_assign(
            Place::local(local),
            Rvalue::UnaryOp {
                op: mir_op,
                operand: Operand::Copy(Place::local(inner)),
            },
            span,
        );
        Some(local)
    }

    pub(crate) fn lower_binary(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // Short-circuit `&&` / `||`. The pre-0.10.0 lowering called
        // `lower_expr` on BOTH sides up front, so any side-effecting
        // or out-of-bounds RHS fired even when the LHS already
        // determined the result. `while j > 0 && arr[j - 1] < si`
        // panicked with "index is -1" once j reached 0 because
        // `arr[j - 1]` evaluated unconditionally. Build a small
        // branch lattice (eval LHS, branch on it, eval RHS only on
        // the path that needs it, merge into a single result).
        if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
            let bool_ty = self.tcx.bool_ty();
            let result = self.fresh(bool_ty);
            let lhs_local = self.lower_expr(lhs)?;
            let rhs_block = self.new_block(span);
            let short_block = self.new_block(span);
            let join_block = self.new_block(span);
            // For `&&`: LHS true → eval RHS; LHS false → short-circuit false.
            // For `||`: LHS true → short-circuit true; LHS false → eval RHS.
            let (true_target, false_target) = match op {
                HirBinaryOp::And => (rhs_block, short_block),
                HirBinaryOp::Or => (short_block, rhs_block),
                _ => unreachable!(),
            };
            self.terminate(Terminator::SwitchInt {
                discriminant: Operand::Copy(Place::local(lhs_local)),
                arms: vec![(0, false_target)],
                default: true_target,
            });
            // Short-circuit arm: write the LHS-determined constant.
            self.set_current(short_block);
            let short_value = i64::from(!matches!(op, HirBinaryOp::And));
            self.emit_assign(
                Place::local(result),
                Rvalue::Use(Operand::Const(ConstValue::Int(short_value as i128))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
            // RHS arm: evaluate the right operand and copy into result.
            self.set_current(rhs_block);
            let rhs_local = self.lower_expr(rhs)?;
            self.emit_assign(
                Place::local(result),
                Rvalue::Use(Operand::Copy(Place::local(rhs_local))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
            self.set_current(join_block);
            return Some(result);
        }
        let lhs_local = self.lower_expr(lhs)?;
        let rhs_local = self.lower_expr(rhs)?;
        // 0.7.0 flag::Cell auto-deref at the binary-op boundary —
        // `flags.output == "text"` works without `*`. Matches the
        // VM tier's `values_equal` / `compare` auto-unwrap shape.
        let lhs_local = self.auto_deref_cell(lhs_local, span);
        let rhs_local = self.auto_deref_cell(rhs_local, span);
        // Detect string concatenation (`s1 + s2` where at least
        // one side is a `String`) and route it through the native
        // runtime's `gos_rt_str_concat` helper rather than the
        // integer `+`. HIR types may still carry unresolved
        // inference variables here, so we inspect the lowered
        // MIR locals' concrete types too.
        let is_string_ty = |this: &Self, t: Ty| -> bool {
            let mut cur = t;
            loop {
                match this.tcx.kind_of(cur) {
                    TyKind::String => return true,
                    TyKind::Ref { inner, .. } => cur = *inner,
                    _ => return false,
                }
            }
        };
        if matches!(op, HirBinaryOp::Add) {
            if is_string_ty(self, ty)
                || is_string_ty(self, lhs.ty)
                || is_string_ty(self, rhs.ty)
                || is_string_ty(self, self.locals[lhs_local.0 as usize].ty)
                || is_string_ty(self, self.locals[rhs_local.0 as usize].ty)
            {
                let dest_ty = self.tcx.string_ty();
                let dest = self.fresh(dest_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_concat".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(lhs_local)),
                        Operand::Copy(Place::local(rhs_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return Some(dest);
            }
        }
        // String equality / inequality. Pointer-compare miscompares
        // because two byte-equal strings often have distinct
        // backing buffers (cell-loaded values, format!-built
        // strings, etc.). Route through the runtime's byte-level
        // helper.
        if matches!(op, HirBinaryOp::Eq | HirBinaryOp::Ne)
            && (is_string_ty(self, lhs.ty)
                || is_string_ty(self, rhs.ty)
                || is_string_ty(self, self.locals[lhs_local.0 as usize].ty)
                || is_string_ty(self, self.locals[rhs_local.0 as usize].ty))
        {
            let bool_ty = self.tcx.bool_ty();
            let cmp = self.fresh(bool_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_str_eq".to_string())),
                args: vec![
                    Operand::Copy(Place::local(lhs_local)),
                    Operand::Copy(Place::local(rhs_local)),
                ],
                destination: Place::local(cmp),
                target: Some(next),
            });
            self.set_current(next);
            if matches!(op, HirBinaryOp::Ne) {
                let dest = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(dest),
                    Rvalue::UnaryOp {
                        op: UnOp::Not,
                        operand: Operand::Copy(Place::local(cmp)),
                    },
                    span,
                );
                return Some(dest);
            }
            return Some(cmp);
        }
        // When the HIR type is still an inference variable, ground the
        // result type from the operands so the LLVM backend uses the
        // correct alloca type (double vs ptr). Without this, f64
        // arithmetic in impl methods produces ptr-typed intermediate
        // locals whose integer bit-pattern arithmetic diverges from
        // the correct float computation.
        let ty = if matches!(self.tcx.kind_of(ty), TyKind::Var(_) | TyKind::Error) {
            let lhs_ty = self.locals[lhs_local.0 as usize].ty;
            let rhs_ty = self.locals[rhs_local.0 as usize].ty;
            let resolve_float_or_int =
                |this: &Self, t: gossamer_types::Ty| -> Option<gossamer_types::Ty> {
                    match this.tcx.kind_of(t) {
                        TyKind::Float(_) | TyKind::Int(_) | TyKind::Bool => Some(t),
                        _ => None,
                    }
                };
            // For comparison ops, result is bool regardless of operand types.
            if matches!(
                op,
                HirBinaryOp::Eq
                    | HirBinaryOp::Ne
                    | HirBinaryOp::Lt
                    | HirBinaryOp::Le
                    | HirBinaryOp::Gt
                    | HirBinaryOp::Ge
            ) {
                self.tcx.bool_ty()
            } else if let Some(concrete) = resolve_float_or_int(self, lhs_ty) {
                concrete
            } else if let Some(concrete) = resolve_float_or_int(self, rhs_ty) {
                concrete
            } else {
                ty
            }
        } else {
            ty
        };
        let local = self.fresh(ty);
        let bin_op = lower_binop(op);
        self.emit_assign(
            Place::local(local),
            Rvalue::BinaryOp {
                op: bin_op,
                lhs: Operand::Copy(Place::local(lhs_local)),
                rhs: Operand::Copy(Place::local(rhs_local)),
            },
            span,
        );
        Some(local)
    }

    pub(crate) fn lower_assign(&mut self, place: &HirExpr, value: &HirExpr, span: Span) {
        // `xs[idx] = v` on a `Vec<T>` / `Slice<T>` receiver routes
        // through the runtime helper `gos_rt_vec_set_i64`. The
        // generic projection path computes a flat-array address
        // `base + idx * stride` that's correct for `[T; N]` but
        // wrong for Vec headers (the data lives at `header.ptr`,
        // not directly in the slot). Without this short-circuit
        // `tc_names[idx] = s` in askq's chat round was a no-op
        // and the LLM's tool name came back empty. Pointer-sized
        // element types (i64 / String / json::Value / Adt
        // pointers) all use the same i64-sized helper since the
        // values are pointer-shaped on the wire.
        if let HirExprKind::Index { base, index } = &place.kind {
            use gossamer_types::TyKind;
            let base_local_for_kind = self
                .receiver_local_from_path(base)
                .map_or(base.ty, |l| self.locals[l.0 as usize].ty);
            let mut peeled = base_local_for_kind;
            while let TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                peeled = *inner;
            }
            let is_vec_or_slice =
                matches!(self.tcx.kind_of(peeled), TyKind::Vec(_) | TyKind::Slice(_));
            if is_vec_or_slice {
                let Some(value_local) = self.lower_expr(value) else {
                    return;
                };
                let Some(base_local) = self.lower_expr(base) else {
                    return;
                };
                let Some(idx_local) = self.lower_expr(index) else {
                    return;
                };
                let unit_ty = self.tcx.unit();
                let dest = self.fresh(unit_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_vec_set_i64".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(base_local)),
                        Operand::Copy(Place::local(idx_local)),
                        Operand::Copy(Place::local(value_local)),
                    ],
                    destination: Place::local(dest),
                    target: Some(next),
                });
                self.set_current(next);
                return;
            }
        }
        let Some(mut value_local) = self.lower_expr(value) else {
            return;
        };
        let Some(mir_place) = self.lower_place_expr(place) else {
            return;
        };
        // Same callable-coercion as `let` and `return`: when the
        // lvalue's static type is callable and the rvalue is a
        // bare fn item, wrap the fn into the env+code blob so the
        // slot ends up env-shaped.
        {
            use gossamer_types::TyKind;
            let dest_callable = matches!(
                self.tcx.kind_of(place.ty),
                TyKind::FnPtr(_) | TyKind::FnTrait(_)
            );
            let value_ty = self.locals[value_local.0 as usize].ty;
            let src_is_fn_def = matches!(self.tcx.kind_of(value_ty), TyKind::FnDef { .. });
            let src_names_fn = self.local_fn_name.contains_key(&value_local);
            if dest_callable && (src_is_fn_def || src_names_fn) {
                value_local = self.coerce_to_fn_trait_if_needed(value_local, place.ty, span);
            }
        }
        self.emit_assign(
            mir_place,
            Rvalue::Use(Operand::Copy(Place::local(value_local))),
            span,
        );
    }

    pub(crate) fn lower_place_expr(&mut self, expr: &HirExpr) -> Option<Place> {
        match &expr.kind {
            HirExprKind::Path { segments, .. } => {
                let first = segments.first()?;
                let local = self.lookup_local(&first.name)?;
                Some(Place::local(local))
            }
            HirExprKind::Field { receiver, name } => {
                let mut base = self.lower_place_expr(receiver)?;
                // Resolve which struct's field ordering to use.
                // Prefer the receiver's static type — for nested
                // projections (`o.inner.x`) the receiver expression
                // is `o.inner` whose type is `Inner`, while
                // `local_struct[base.local]` would point at `o`'s
                // root struct `Outer` and miss the field. The
                // local-struct fallback is kept for cases where
                // type information is partial (inference variables
                // leaking through).
                let struct_name = self
                    .struct_name_from_expr(receiver)
                    .or_else(|| self.local_struct.get(&base.local).cloned())?;
                let order = self.structs.get(&struct_name)?;
                let idx = u32::try_from(order.iter().position(|f| f == &name.name)?).ok()?;
                base.projection.push(crate::ir::Projection::Field(idx));
                Some(base)
            }
            HirExprKind::TupleIndex { receiver, index } => {
                let mut base = self.lower_place_expr(receiver)?;
                base.projection.push(crate::ir::Projection::Field(*index));
                Some(base)
            }
            HirExprKind::Index { base, index } => {
                // For a Vec / Slice base whose elements are multi-slot
                // aggregates (`bodies[i].x` over a `Vec<Body>`), a flat
                // `Projection::Index` would treat the local's value as
                // an inline element buffer — but the local holds a
                // `*mut GosVec` *header*, so the index strides off the
                // header fields instead of the data buffer (every
                // access past element 0 reads garbage). Route through
                // `lower_index_access`, which emits
                // `gos_rt_vec_get_ptr` to materialise the real element
                // address; the returned local carries the element's
                // struct tag so the appended `Field` projection
                // resolves correctly.
                // Prefer the base local's MIR-resolved type over the
                // HIR expression type: a `let mut bodies: [Body; N]`
                // binding is promoted to `Vec<Body>` on the MIR side
                // (its local type is rewritten), but the HIR `base.ty`
                // still reads `[Body; N]`. Using only the HIR type
                // would miss the promotion and fall through to the
                // flat-projection path that strides off the GosVec
                // header.
                let base_ty_effective = match &base.kind {
                    HirExprKind::Path { segments, .. } => segments
                        .first()
                        .and_then(|seg| self.lookup_local(&seg.name))
                        .map_or(base.ty, |l| self.locals[l.0 as usize].ty),
                    _ => base.ty,
                };
                let base_kind = {
                    use gossamer_types::TyKind;
                    let raw = self.tcx.kind_of(base_ty_effective).clone();
                    match raw {
                        TyKind::Ref { inner, .. } => self.tcx.kind_of(inner).clone(),
                        other => other,
                    }
                };
                if let gossamer_types::TyKind::Vec(elem) | gossamer_types::TyKind::Slice(elem) =
                    base_kind
                {
                    let elem_multislot = matches!(
                        self.tcx.kind_of(elem),
                        gossamer_types::TyKind::Tuple(_) | gossamer_types::TyKind::Adt { .. }
                    ) && self.type_slot_bytes(elem) > 8;
                    if elem_multislot {
                        // Materialise the element address with
                        // `gos_rt_vec_get_ptr` and bind it to a
                        // `&elem`-typed local. A reference-typed local
                        // makes the backend auto-deref it (load the
                        // stored pointer) before walking the appended
                        // `Field` projection, so both reads *and*
                        // writes land on the element inside the Vec's
                        // data buffer rather than a stack copy. The
                        // element's struct tag is propagated so the
                        // `Field` arm can resolve field indices.
                        use gossamer_types::{Mutbl, TyKind};
                        let base_place = self.lower_place_expr(base)?;
                        let index_local = self.lower_expr(index)?;
                        let ref_ty = self.tcx.intern(TyKind::Ref {
                            mutability: Mutbl::Mut,
                            inner: elem,
                        });
                        let ptr_local = self.fresh(ref_ty);
                        let next = self.new_block(expr.span);
                        self.terminate(Terminator::Call {
                            callee: Operand::Const(ConstValue::Str(
                                "gos_rt_vec_get_ptr".to_string(),
                            )),
                            args: vec![
                                Operand::Copy(Place::local(base_place.local)),
                                Operand::Copy(Place::local(index_local)),
                            ],
                            destination: Place::local(ptr_local),
                            target: Some(next),
                        });
                        self.set_current(next);
                        if let Some(elem_struct) =
                            self.local_elem_struct.get(&base_place.local).cloned()
                        {
                            self.local_struct.insert(ptr_local, elem_struct);
                        } else if let Some(name) = self.struct_name_of(elem) {
                            self.local_struct.insert(ptr_local, name);
                        }
                        return Some(Place::local(ptr_local));
                    }
                }
                let mut base_place = self.lower_place_expr(base)?;
                let index_local = self.lower_expr(index)?;
                base_place
                    .projection
                    .push(crate::ir::Projection::Index(index_local));
                Some(base_place)
            }
            // `*operand = ...` — deref-assign through a `&mut T` /
            // `*mut T`. The Place is the base local with a `Deref`
            // projection appended; the lowerer's `Place::Deref`
            // arm in cranelift/LLVM stores through the pointer.
            HirExprKind::Unary {
                op: gossamer_hir::HirUnaryOp::Deref,
                operand,
            } => {
                let mut base = self.lower_place_expr(operand)?;
                base.projection.push(crate::ir::Projection::Deref);
                Some(base)
            }
            _ => None,
        }
    }

    pub(crate) fn lower_tuple(&mut self, elems: &[HirExpr], ty: Ty, span: Span) -> Option<Local> {
        use gossamer_types::TyKind;
        // Declared element types (when `ty` resolved to a concrete
        // Tuple) let us coerce a flat `[T; N]` array-literal element
        // into a heap `GosVec` when the tuple slot is declared `[T]`
        // (e.g. the `(String, [u8])` pairs passed to
        // `archive::tar::write`). Without this the inline array
        // widens the tuple and the consumer reads element[0] as the
        // Vec pointer.
        let elem_tys: Option<Vec<Ty>> = match self.tcx.kind_of(ty) {
            TyKind::Tuple(tys) => Some(tys.clone()),
            _ => None,
        };
        let mut operands = Vec::with_capacity(elems.len());
        for (i, elem) in elems.iter().enumerate() {
            let mut local = self.lower_expr(elem)?;
            if let Some(field_ty) = elem_tys.as_ref().and_then(|t| t.get(i)).copied() {
                let val_ty = self.locals[local.0 as usize].ty;
                if let TyKind::Array { elem: e, len } = self.tcx.kind_of(val_ty).clone()
                    && matches!(
                        self.tcx.kind_of(field_ty),
                        TyKind::Vec(_) | TyKind::Slice(_)
                    )
                {
                    local = self.coerce_array_to_vec(local, e, len, span);
                }
            }
            operands.push(Operand::Copy(Place::local(local)));
        }
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Aggregate {
                kind: crate::ir::AggregateKind::Tuple,
                operands,
            },
            span,
        );
        Some(dest)
    }

    pub(crate) fn lower_tuple_index(
        &mut self,
        receiver: &HirExpr,
        index: u32,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let receiver_local = self.lower_expr(receiver)?;
        // A tuple element read out of a Vec (`v[i].0`) inherits its
        // field types from the element type the binding was pinned
        // to; when that came from an unannotated `let mut xs = []`
        // the int-literal fields can remain `Var`, which lowers to a
        // `ptr` load (and the value is then mis-dispatched as a
        // string). Pin the receiver's tuple field types to i64 so the
        // field load reads an integer slot.
        let recv_ty = self.locals[receiver_local.0 as usize].ty;
        let resolved_recv_ty = self.resolve_var_tuple_fields(recv_ty);
        if resolved_recv_ty != recv_ty {
            self.locals[receiver_local.0 as usize].ty = resolved_recv_ty;
        }
        // Pin the destination to the resolved field type when the
        // expression's own type is still unresolved.
        let dest_ty = if matches!(
            self.tcx.kind_of(ty),
            gossamer_types::TyKind::Var(_) | gossamer_types::TyKind::Error
        ) {
            match self.tcx.kind_of(resolved_recv_ty) {
                gossamer_types::TyKind::Tuple(fields) => {
                    fields.get(index as usize).copied().unwrap_or(ty)
                }
                _ => ty,
            }
        } else {
            ty
        };
        let dest = self.fresh(dest_ty);
        let place = Place {
            local: receiver_local,
            projection: vec![crate::ir::Projection::Field(index)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        Some(dest)
    }

    pub(crate) fn lower_index_access(
        &mut self,
        base: &HirExpr,
        index: &HirExpr,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // Slice expression `arr[lo..hi]`: the index is a Range
        // value rather than a single integer. Route through a
        // runtime slice helper. Returns a `*mut GosVec` so the
        // surrounding code can iterate or `to_vec()` on it.
        if let HirExprKind::Range {
            start,
            end,
            inclusive,
        } = &index.kind
        {
            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
            let base_local = self.lower_expr(base)?;
            let lo_local = if let Some(s) = start {
                self.lower_expr(s)?
            } else {
                let l = self.fresh(i64_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                    span,
                );
                l
            };
            let hi_local = if let Some(e) = end {
                self.lower_expr(e)?
            } else {
                // `arr[lo..]` — substitute `arr.len()` as the
                // upper bound by calling `gos_rt_len` on the
                // base. Works for both arrays and Vecs since
                // `gos_rt_len` reads the leading length word.
                let l = self.fresh(i64_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_len".to_string())),
                    args: vec![Operand::Copy(Place::local(base_local))],
                    destination: Place::local(l),
                    target: Some(next),
                });
                self.set_current(next);
                l
            };
            let hi_local = if *inclusive {
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
                        lhs: Operand::Copy(Place::local(hi_local)),
                        rhs: Operand::Copy(Place::local(one)),
                    },
                    span,
                );
                bumped
            } else {
                hi_local
            };
            let dest_ty = ty;
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_vec_slice".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(lo_local)),
                    Operand::Copy(Place::local(hi_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        // Walk through references so `&String` indexing behaves
        // the same as indexing a bare `String`. Prefer the MIR
        // local's pinned type over the HIR type when the base is
        // a simple Path — the type checker may have left the HIR
        // type as an unresolved inference variable for receivers
        // produced by runtime helpers (e.g. `read_to_string`),
        // and the indexing path needs the concrete `String` to
        // route to `gos_rt_str_byte_at` instead of falling
        // through to the array-projection helper.
        let mut base_kind = self
            .receiver_local_from_path(base)
            .map_or(base.ty, |local| self.locals[local.0 as usize].ty);
        while let TyKind::Ref { inner, .. } = self.tcx.kind_of(base_kind) {
            base_kind = *inner;
        }
        let base_is_string = matches!(self.tcx.kind_of(base_kind), TyKind::String);
        if base_is_string {
            let base_local = self.lower_expr(base)?;
            let index_local = self.lower_expr(index)?;
            // `gos_rt_str_byte_at` returns a zero-extended byte —
            // pin the MIR destination to `i64` so downstream
            // print/format dispatch routes to the integer helper
            // instead of mis-treating the byte as a string ptr.
            let dest_ty = match self.tcx.kind_of(ty) {
                TyKind::Int(_) => ty,
                _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
            };
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str("gos_rt_str_byte_at".to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(index_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            return Some(dest);
        }
        let base_local = self.lower_expr(base)?;
        let index_local = self.lower_expr(index)?;
        // For Vec / Slice receivers (whose runtime layout is a
        // `*mut GosVec` header, not a flat element buffer) route
        // index reads through `gos_rt_vec_get_i64`. A naked
        // `Projection::Index` would treat the local's first 8
        // bytes as element 0 — which is the GosVec `len` field,
        // not the data buffer.
        let actual_base_kind = self
            .tcx
            .kind_of(self.locals[base_local.0 as usize].ty)
            .clone();
        let actual_base_kind = match actual_base_kind {
            TyKind::Ref { inner, .. } => self.tcx.kind_of(inner).clone(),
            other => other,
        };
        if let TyKind::Vec(elem) | TyKind::Slice(elem) = actual_base_kind {
            // Pin the destination's MIR type to the element type
            // recorded on the base. The HIR-side `ty` is sometimes
            // an inference variable that lost contact with the
            // concrete element kind by the time we get here (e.g.
            // for the result of `flag::Set::parse`'s `Vec<String>`
            // routed through a `?` Try), and printing/formatting
            // dispatch then routes the i64 ptr through the integer
            // helper instead of the string helper.
            //
            // `Vec<Option<T>>` / `Vec<Result<T, _>>` store each
            // element as the wrapped tagged-union handle (a
            // `*mut [disc, payload]` cell), exactly like any other
            // enum — `xs.push(None)` and `xs.push(Some(v))` must be
            // distinguishable at read time. Read the element back as
            // the wrapper so `match v[i] { Some(k) => …, None => … }`
            // sees the real discriminant; the `Some(k)` arm still
            // binds `k: T`. (An earlier "peel to the unwrapped T"
            // shortcut assumed a happy-path encoding the compiled
            // push side never used, so `xs[i]` handed back the raw
            // handle pointer as if it were the payload.)
            let elem_unwrapped = elem;
            let elem_kind_now = self.tcx.kind_of(elem_unwrapped);
            let dest_ty = match elem_kind_now {
                TyKind::String | TyKind::Bool | TyKind::Char | TyKind::Float(_) => elem_unwrapped,
                TyKind::Int(_) => elem_unwrapped,
                // Aggregate elements (struct, tuple, fixed array) —
                // keep the pinned element type so subsequent
                // `Field(idx)` / `Index(k)` projections find the
                // right slot layout. A `Vec<[i64; 2]>` element is a
                // multi-slot inline array; preserving the `Array`
                // type lets the chained `v[i][j]` read each slot.
                TyKind::Adt { .. } | TyKind::Tuple(_) | TyKind::Array { .. } => elem_unwrapped,
                // `Vec<json::Value>` indexing must produce a
                // `JsonValue`-typed local so subsequent
                // `json::get(&v[i], ...)` / `v[i].clone()` dispatch
                // through the json runtime helpers. Without this
                // pin, the dest fell through to the i64 default
                // and `tcs[k].clone()` (askq) lost the json tag
                // — every nested field probe missed.
                TyKind::JsonValue => elem_unwrapped,
                // `Vec<Vec<T>>` indexing: preserve the inner Vec type
                // so that subsequent indexing on the result routes
                // through `gos_rt_vec_get_i64` rather than direct
                // pointer arithmetic on the GosVec header.
                // Without this, `caps[0]` on a `Vec<Vec<String>>`
                // returns a local typed `i64`, and `row[1]` then reads
                // the GosVec `cap` field instead of element 1.
                TyKind::Vec(_) | TyKind::Slice(_) => elem_unwrapped,
                _ => match self.tcx.kind_of(ty) {
                    TyKind::Int(_) | TyKind::String | TyKind::Bool | TyKind::Float(_) => ty,
                    _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                },
            };
            // Multi-slot element types (tuples, named structs with
            // multiple fields) need `gos_rt_vec_get_ptr` — the
            // single-slot `gos_rt_vec_get_i64` only reads the first
            // 8 bytes of the slot, so a `(String, String)` element
            // would hand back just the first String and the
            // subsequent `.0` / `.1` projection would dereference
            // that c-string ptr as if it were a tuple aggregate.
            // A by-value `Result`/`Option` element is a 16-byte `i128` read
            // through the dedicated helper (not the 8-byte `get_i64`, which
            // would drop the payload, nor the aggregate-address `get_ptr`).
            let elem_is_result_option = matches!(
                self.tcx.kind_of(elem_unwrapped),
                TyKind::Adt { def, .. } if def.local == u32::MAX || def.local == u32::MAX - 1
            );
            let elem_is_multislot = matches!(
                self.tcx.kind_of(elem_unwrapped),
                TyKind::Tuple(_) | TyKind::Adt { .. } | TyKind::Array { .. }
            ) && self.type_slot_bytes(elem_unwrapped) > 8;
            let (helper, dest_ty) = if elem_is_result_option {
                ("gos_rt_vec_get_i128", elem_unwrapped)
            } else if elem_is_multislot {
                ("gos_rt_vec_get_ptr", dest_ty)
            } else {
                ("gos_rt_vec_get_i64", dest_ty)
            };
            let dest = self.fresh(dest_ty);
            let next = self.new_block(span);
            self.terminate(Terminator::Call {
                callee: Operand::Const(ConstValue::Str(helper.to_string())),
                args: vec![
                    Operand::Copy(Place::local(base_local)),
                    Operand::Copy(Place::local(index_local)),
                ],
                destination: Place::local(dest),
                target: Some(next),
            });
            self.set_current(next);
            // Propagate the base's element-struct tag onto the
            // result so subsequent `entries[i].<field>` access
            // (or `let entry = entries[i]; entry.<field>`)
            // resolves through `lookup_place_expr`'s struct-name
            // path instead of falling through to JsonValue.
            if let Some(elem_struct) = self.local_elem_struct.get(&base_local).cloned() {
                self.local_struct.insert(dest, elem_struct);
            } else if let Some(name) = self.struct_name_of(elem) {
                self.local_struct.insert(dest, name);
            }
            return Some(dest);
        }
        // Fixed-array index (`a[j]` where `a: [T; N]`). When the
        // array came out of a multi-slot Vec element (`v[i][j]`) its
        // element type can still be an inference variable, which the
        // codegen would load as a `ptr` and then mis-dispatch (e.g.
        // concat as a string). Resolve a `Var` element to i64 on the
        // base local and pin the destination to match.
        let base_ty = self.locals[base_local.0 as usize].ty;
        let resolved_base_ty = self.resolve_var_tuple_fields(base_ty);
        if resolved_base_ty != base_ty {
            self.locals[base_local.0 as usize].ty = resolved_base_ty;
        }
        let dest_ty = if matches!(self.tcx.kind_of(ty), TyKind::Var(_) | TyKind::Error) {
            match self.tcx.kind_of(resolved_base_ty) {
                TyKind::Array { elem, .. } | TyKind::Slice(elem) | TyKind::Vec(elem) => *elem,
                _ => ty,
            }
        } else {
            ty
        };
        let dest = self.fresh(dest_ty);
        let place = Place {
            local: base_local,
            projection: vec![crate::ir::Projection::Index(index_local)],
        };
        self.emit_assign(Place::local(dest), Rvalue::Use(Operand::Copy(place)), span);
        if let Some(elem_struct) = self.local_elem_struct.get(&base_local).cloned() {
            self.local_struct.insert(dest, elem_struct);
        }
        Some(dest)
    }
}
