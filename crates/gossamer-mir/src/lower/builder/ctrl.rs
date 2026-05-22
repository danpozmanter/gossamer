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
    pub(crate) fn lower_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let cond_local = self.lower_expr(condition)?;
        // 0.7.0 flag::Cell auto-deref for `if flags.verbose { … }`.
        let cond_local = self.auto_deref_cell(cond_local, span);
        let then_block = self.new_block(span);
        let else_block = self.new_block(span);
        let join_block = self.new_block(span);
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, else_block)],
            default: then_block,
        });

        let result_local = self.fresh(ty);

        self.set_current(then_block);
        if let Some(then_value) = self.lower_expr(then_branch) {
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(then_value))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(else_block);
        if let Some(else_branch) = else_branch {
            if let Some(else_value) = self.lower_expr(else_branch) {
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(else_value))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        } else {
            let unit_local = self.lower_unit(span);
            self.emit_assign(
                Place::local(result_local),
                Rvalue::Use(Operand::Copy(Place::local(unit_local))),
                span,
            );
            self.terminate(Terminator::Goto { target: join_block });
        }

        self.set_current(join_block);
        Some(result_local)
    }

    pub(crate) fn lower_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Route guarded arms and any non-flat pattern shape
        // (tuple / or-pattern / nested variant binding) through
        // the if-chain lowering. The original SwitchInt path
        // below stays the fast path for flat int / bool /
        // single-variant matches whose discriminant fits one
        // word.
        let needs_chain = arms.iter().any(|arm| {
            arm.guard.is_some()
                || matches!(
                    arm.pattern.kind,
                    HirPatKind::Tuple(_)
                        | HirPatKind::Or(_)
                        | HirPatKind::Struct { .. }
                        | HirPatKind::Range { .. }
                        | HirPatKind::Ref { .. }
                        | HirPatKind::At { .. }
                        | HirPatKind::Literal(
                            HirLiteral::String(_) | HirLiteral::Char(_) | HirLiteral::Float(_)
                        )
                )
                || matches!(
                    &arm.pattern.kind,
                    HirPatKind::Variant { name, .. }
                        if matches!(name.name.as_str(), "Ok" | "Err" | "Some" | "None")
                            || self.enums.lookup(std::slice::from_ref(name)).is_some()
                )
        });
        if needs_chain {
            return self.lower_match_with_guards(scrutinee, arms, ty, span);
        }
        let mut switch_arms: Vec<(i128, BlockId)> = Vec::new();
        let mut default_block: Option<BlockId> = None;
        // Per-arm binding: the variant pattern's inner Binding (e.g.
        // `Ok(v)` → `v`) is registered against the scrutinee local
        // when we enter the arm block, so the body can reference it.
        // The scrutinee's static type carries through (e.g. for
        // `Ok(v)` on a `Result<json::Value, _>` the binding gets
        // typed as `json::Value`).
        // Per-arm captured payload binding: (binding name, mutable
        // flag, variant constructor name). The variant name lets
        // the arm-body fixup routine re-pin the scrutinee local's
        // type to the right `substs` slot — `substs[0]` for
        // `Ok`/`Some`, `substs[1]` for `Err` — so subsequent reads
        // of the bound name see the payload type, not the wrapper.
        let mut arm_bindings: Vec<Option<(Ident, bool, Option<String>)>> =
            Vec::with_capacity(arms.len());
        let mut arm_bodies: Vec<(BlockId, &HirExpr)> = Vec::with_capacity(arms.len());
        for arm in arms {
            let arm_block = self.new_block(span);
            arm_bodies.push((arm_block, &arm.body));
            arm_bindings.push(None);
            match &arm.pattern.kind {
                HirPatKind::Literal(HirLiteral::Int(text)) => {
                    let v = parse_int(text).unwrap_or_else(|| {
                        unreachable!("match arm: lexer-validated int literal `{text}` failed parse")
                    });
                    switch_arms.push((v, arm_block));
                }
                HirPatKind::Literal(HirLiteral::Bool(b)) => {
                    switch_arms.push((i128::from(*b), arm_block));
                }
                HirPatKind::Wildcard | HirPatKind::Binding { .. } => {
                    // Multiple wildcard arms are accepted; only the
                    // first is reachable. Subsequent wildcard bodies
                    // are emitted into dead blocks the SwitchInt
                    // never targets.
                    if default_block.is_none() {
                        default_block = Some(arm_block);
                    }
                }
                // Variant patterns (`Ok(x)`, `Err(e)`, `Some(v)`, …)
                // don't yet have runtime discriminants, but we can
                // still produce a well-formed CFG by always taking
                // the first variant arm as a "happy path" default.
                // Bind any inner pattern to the scrutinee local so
                // `let x = foo()?` compiles. Wrong for genuine error
                // cases, but enough for programs whose control flow
                // stays on the Ok/Some path.
                HirPatKind::Variant { name, fields } => {
                    // User-defined enum: dispatch by the variant's
                    // declaration index recorded in `EnumIndex`.
                    // Fall back to `Result`/`Option`'s historical
                    // happy-path encoding (`Ok` / `Some` = 0,
                    // `Err` / `None` = 1) for the stdlib variants
                    // that don't have a Gossamer enum behind them.
                    let pos: i128 =
                        if let Some((_, idx)) = self.enums.lookup(std::slice::from_ref(name)) {
                            idx as i128
                        } else if matches!(name.name.as_str(), "Err" | "None" | "Some" | "Ok") {
                            match name.name.as_str() {
                                "Some" | "Ok" => 0,
                                _ => 1,
                            }
                        } else {
                            switch_arms.len() as i128
                        };
                    switch_arms.push((pos, arm_block));
                    // For `Ok(v)` / `Some(v)` patterns the
                    // payload is structurally identical to the
                    // scrutinee in compiled mode (Result/Option
                    // are flat single-slot values today), so
                    // bind the inner name to the scrutinee local
                    // when entering the arm. Captures only the
                    // first single-Binding inner field — wider
                    // patterns continue through the placeholder.
                    if let Some(first) = fields.first() {
                        if let HirPatKind::Binding {
                            name: bname,
                            mutable,
                        } = &first.kind
                        {
                            *arm_bindings.last_mut().expect("arm tracked") =
                                Some((bname.clone(), *mutable, Some(name.name.clone())));
                        }
                    }
                }
                // Tuple / struct / range / or-pattern shapes that
                // the no-guards SwitchInt path doesn't decode are
                // treated as wildcard arms here: the first one
                // becomes the default, later ones are dead.
                _ => {
                    if default_block.is_none() {
                        default_block = Some(arm_block);
                    }
                }
            }
        }
        let scrutinee_local = self.lower_expr(scrutinee)?;
        let join_block = self.new_block(span);
        let result_local = self.fresh(ty);
        // Save the post-scrutinee block before allocating the
        // default arm; the unreachable_block creation below sets
        // current to that block and then terminates it (leaving
        // current = None), which would otherwise swallow our
        // SwitchInt / Goto terminator below.
        let dispatch_block = self.current;
        let default = default_block.unwrap_or_else(|| {
            let unreachable_block = self.new_block(span);
            self.set_current(unreachable_block);
            self.terminate(Terminator::Unreachable);
            unreachable_block
        });
        if let Some(block) = dispatch_block {
            self.set_current(block);
        }
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(scrutinee_local)),
            arms: switch_arms,
            default,
        });
        for ((arm_block, body), binding) in arm_bodies.into_iter().zip(arm_bindings) {
            self.set_current(arm_block);
            // When the arm pattern was `Ok(v)` / `Some(v)` /
            // `Variant(v)`, register `v` against the scrutinee
            // local so the arm body's references resolve. If the
            // scrutinee is a flat `*mut GosJson` (i.e. its static
            // type is `Result<json::Value, _>` / `Option<json::Value>`
            // / `json::Value`), promote the scrutinee local to
            // `json::Value` so chained `j.field` accesses route
            // through the json runtime helpers.
            if let Some((bname, _mutable, variant_name)) = binding {
                let scrut_ty = self.locals[scrutinee_local.0 as usize].ty;
                if let Some(name) = variant_name.as_deref() {
                    // Generalised happy-path payload pin: for
                    // `Ok(x)` / `Some(x)` the payload is the
                    // wrapper's first generic arg; for `Err(e)`
                    // the second. Without this, downstream code
                    // sees `x` typed as the whole `Result<T, E>`
                    // and routes value-formatting / coercion
                    // through the wrong path.
                    let slot = match name {
                        "Ok" | "Some" => Some(0),
                        "Err" => Some(1),
                        _ => None,
                    };
                    if let Some(idx) = slot {
                        if let Some(payload_ty) = self.adt_generic_at(scrut_ty, idx) {
                            self.locals[scrutinee_local.0 as usize].ty = payload_ty;
                        }
                    }
                }
                self.bind_local(&bname.name, scrutinee_local);
            }
            if let Some(value_local) = self.lower_expr(body) {
                // Pin the match-result local's type to the arm's
                // value type when the HIR type is opaque (Var /
                // Error). Lets chained patterns like `let v =
                // match r { Ok(j) => j, .. }; v.field` flow the
                // concrete `json::Value` (or struct) shape into
                // the surrounding `let`'s field-access lowering.
                use gossamer_types::TyKind;
                let arm_value_ty = self.locals[value_local.0 as usize].ty;
                let result_kind = self.tcx.kind_of(self.locals[result_local.0 as usize].ty);
                let arm_kind = self.tcx.kind_of(arm_value_ty);
                let result_is_loose =
                    matches!(result_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                let arm_is_concrete =
                    !matches!(arm_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                if result_is_loose && arm_is_concrete {
                    self.locals[result_local.0 as usize].ty = arm_value_ty;
                }
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(value_local))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join_block });
            }
        }
        self.set_current(join_block);
        Some(result_local)
    }

    pub(crate) fn lower_match_with_guards(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[HirMatchArm],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let scrutinee_local = self.lower_expr(scrutinee)?;
        let bool_ty = self.tcx.bool_ty();
        let result_local = self.fresh(ty);
        let join = self.new_block(span);

        for arm in arms {
            let arm_block = self.new_block(span);
            let next_block = self.new_block(span);

            // Push a binding scope so guard + body see the
            // pattern-bound names. Bindings introduced by the
            // pattern (single-name, tuple-element, variant-payload)
            // are recorded against MIR locals here too.
            self.push_scope();
            // Defer payload extraction for Result/Option arms into
            // arm_block so the payload pointer is only dereferenced
            // on the matching branch. Without this, `gos_rt_result_payload`
            // runs unconditionally in the header block and crashes
            // when the scrutinee is None/Err on the next iteration.
            self.payload_defer_block = Some(arm_block);
            // 0.8.0: no always-matches fallback. When
            // `lower_pattern_predicate` doesn't decode the
            // pattern shape (tuple/struct/range/or-patterns that
            // need richer destructuring than the SwitchInt path
            // covers), MIR refuses to compile rather than silently
            // miscompiling subsequent arms as unreachable. The
            // legacy `GOSSAMER_STRICT_LOWER=1` opt-in is now the
            // only behaviour.
            let pat_match_local =
                match self.lower_pattern_predicate(scrutinee_local, &arm.pattern, span) {
                    Some(local) => local,
                    None => {
                        let kind = pattern_kind_label(&arm.pattern);
                        panic!(
                            "MIR lower: match-guard arm has unsupported pattern shape \
                         ({kind}); add explicit destructuring for this pattern shape \
                         before relying on it in compiled code"
                        );
                    }
                };
            // Clear the defer hint in case the pattern didn't consume it
            // (e.g. wildcard arms have no payload to extract).
            self.payload_defer_block = None;

            // Combine the pattern predicate with the guard (if any).
            let predicate = if let Some(guard_expr) = &arm.guard {
                let guard_local = self.lower_expr(guard_expr)?;
                let combined = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(combined),
                    Rvalue::BinaryOp {
                        // Bool is i1/i8 at runtime; bitwise and is
                        // equivalent to logical AND for the
                        // 0/1 truth values produced above.
                        op: BinOp::BitAnd,
                        lhs: Operand::Copy(Place::local(pat_match_local)),
                        rhs: Operand::Copy(Place::local(guard_local)),
                    },
                    span,
                );
                combined
            } else {
                pat_match_local
            };

            self.terminate(Terminator::SwitchInt {
                discriminant: Operand::Copy(Place::local(predicate)),
                arms: vec![(0, next_block)],
                default: arm_block,
            });

            self.set_current(arm_block);
            if let Some(value_local) = self.lower_expr(&arm.body) {
                use gossamer_types::TyKind;
                let arm_value_ty = self.locals[value_local.0 as usize].ty;
                let result_kind = self.tcx.kind_of(self.locals[result_local.0 as usize].ty);
                let arm_kind = self.tcx.kind_of(arm_value_ty);
                let result_is_loose =
                    matches!(result_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                let arm_is_concrete =
                    !matches!(arm_kind, TyKind::Var(_) | TyKind::Error | TyKind::Never);
                if result_is_loose && arm_is_concrete {
                    self.locals[result_local.0 as usize].ty = arm_value_ty;
                }
                if let Some(struct_name) = self.local_struct.get(&value_local).cloned() {
                    self.local_struct.insert(result_local, struct_name);
                }
                if let Some(rk) = self.local_runtime_kind.get(&value_local).copied() {
                    self.local_runtime_kind.insert(result_local, rk);
                }
                if let Some(en) = self.local_elem_struct.get(&value_local).cloned() {
                    self.local_elem_struct.insert(result_local, en);
                }
                self.emit_assign(
                    Place::local(result_local),
                    Rvalue::Use(Operand::Copy(Place::local(value_local))),
                    span,
                );
                self.terminate(Terminator::Goto { target: join });
            } else if self.current.is_some() {
                // Arm body ended normally (no value to bind, but
                // control still falls through — typical of a loop
                // tail). Connect to join so the codegen doesn't
                // leave a dangling block.
                self.terminate(Terminator::Goto { target: join });
            }
            self.pop_scope();

            self.set_current(next_block);
        }
        // Ran past every arm without matching. Match is supposed
        // to be exhaustive; jump to join with the result-local
        // left at its default zero value (the `fresh` above didn't
        // initialise it). This branch is reachable only when the
        // exhaustiveness checker missed something.
        self.terminate(Terminator::Goto { target: join });
        self.set_current(join);
        Some(result_local)
    }

    pub(crate) fn lower_pattern_predicate(
        &mut self,
        scrutinee: Local,
        pattern: &HirPat,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        let bool_ty = self.tcx.bool_ty();
        match &pattern.kind {
            HirPatKind::Wildcard => {
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Binding { name, .. } => {
                self.bind_local(&name.name, scrutinee);
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Literal(HirLiteral::Int(text)) => {
                let v = parse_int(text)?;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let lit_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(v))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Bool(b)) => {
                let lit_local = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(*b))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::String(text)) => {
                // String-literal match arm. Compare via
                // `gos_rt_str_eq` which the runtime exposes; emit
                // a fresh string operand for the literal.
                let str_ty = self.tcx.string_ty();
                let lit_local = self.fresh(str_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Str(text.clone()))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                let next = self.new_block(span);
                self.terminate(Terminator::Call {
                    callee: Operand::Const(ConstValue::Str("gos_rt_str_eq".to_string())),
                    args: vec![
                        Operand::Copy(Place::local(scrutinee)),
                        Operand::Copy(Place::local(lit_local)),
                    ],
                    destination: Place::local(cmp),
                    target: Some(next),
                });
                self.set_current(next);
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Char(c)) => {
                let char_ty = self.tcx.char_ty();
                let lit_local = self.fresh(char_ty);
                self.emit_assign(
                    Place::local(lit_local),
                    Rvalue::Use(Operand::Const(ConstValue::Char(*c))),
                    span,
                );
                let cmp = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(cmp),
                    Rvalue::BinaryOp {
                        op: BinOp::Eq,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lit_local)),
                    },
                    span,
                );
                Some(cmp)
            }
            HirPatKind::Literal(HirLiteral::Float(_) | HirLiteral::Unit) => {
                // Float / Unit literal arms are pathological;
                // float equality match is rarely correct. Fall
                // back to "always matches" so the program still
                // compiles. Programs depending on these arms
                // should use a guard with `==` instead.
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Ref { inner, .. } => {
                // `&pat` patterns: peel through the reference
                // and match the inner pattern. The compiled tier
                // doesn't materialise references separately; the
                // scrutinee local already holds the pointer.
                self.lower_pattern_predicate(scrutinee, inner, span)
            }
            HirPatKind::Rest => {
                // Rest pattern in a non-tuple context — match
                // anything; binds nothing.
                let l = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(l),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                Some(l)
            }
            HirPatKind::Tuple(sub_pats) => {
                // Conjunction across tuple-element predicates. Each
                // sub-pattern is matched against the corresponding
                // tuple field via a fresh local that holds the
                // projected element value.
                use gossamer_types::TyKind;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let elem_tys: Vec<Ty> = match self.tcx.kind_of(scrut_ty) {
                    TyKind::Tuple(elems) => elems.clone(),
                    _ => return None,
                };
                let mut acc = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                    span,
                );
                for (idx, sub_pat) in sub_pats.iter().enumerate() {
                    // Prefer the tuple's recorded element type, but when
                    // inference left it unresolved (`let pair = (10,
                    // "hi")` keeps the binding's tuple type loose) fall
                    // back to the sub-pattern's own type. Without this
                    // the element local defaults to a pointer shape and
                    // the `println!` arg dispatcher routes an `i64`
                    // through `gos_rt_concat_str`, strlen'ing the
                    // integer value and segfaulting.
                    let from_tuple = elem_tys.get(idx).copied();
                    let unresolved = |t: Ty| {
                        matches!(
                            self.tcx.kind_of(t),
                            TyKind::Var(_) | TyKind::Error | TyKind::Never
                        )
                    };
                    let elem_ty = match from_tuple {
                        Some(t) if !unresolved(t) => t,
                        _ if !unresolved(sub_pat.ty) => sub_pat.ty,
                        Some(t) => t,
                        None => return None,
                    };
                    let elem_local = self.fresh(elem_ty);
                    let elem_place = Place {
                        local: scrutinee,
                        projection: vec![crate::ir::Projection::Field(idx as u32)],
                    };
                    self.emit_assign(
                        Place::local(elem_local),
                        Rvalue::Use(Operand::Copy(elem_place)),
                        span,
                    );
                    let sub_pred = self.lower_pattern_predicate(elem_local, sub_pat, span)?;
                    let combined = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(combined),
                        Rvalue::BinaryOp {
                            op: BinOp::BitAnd,
                            lhs: Operand::Copy(Place::local(acc)),
                            rhs: Operand::Copy(Place::local(sub_pred)),
                        },
                        span,
                    );
                    acc = combined;
                }
                Some(acc)
            }
            HirPatKind::Or(branches) => {
                // Disjunction across branch predicates. Each branch
                // contributes its own match check; their bool
                // results are bitwise-ORed together.
                let mut acc = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(acc),
                    Rvalue::Use(Operand::Const(ConstValue::Bool(false))),
                    span,
                );
                for branch in branches {
                    let pred = self.lower_pattern_predicate(scrutinee, branch, span)?;
                    let combined = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(combined),
                        Rvalue::BinaryOp {
                            op: BinOp::BitOr,
                            lhs: Operand::Copy(Place::local(acc)),
                            rhs: Operand::Copy(Place::local(pred)),
                        },
                        span,
                    );
                    acc = combined;
                }
                Some(acc)
            }
            HirPatKind::Range { lo, hi, inclusive } => {
                // `lo..hi` and `lo..=hi` arms reduce to
                // `(scrut >= lo) && (scrut <op> hi)` where the
                // upper comparison is `<` for exclusive and `<=`
                // for inclusive. Only integer literal bounds are
                // accepted today; float / char ranges fall
                // through to the unsupported placeholder.
                let HirLiteral::Int(lo_text) = lo else {
                    return None;
                };
                let HirLiteral::Int(hi_text) = hi else {
                    return None;
                };
                let lo_v = parse_int(lo_text)?;
                let hi_v = parse_int(hi_text)?;
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let lo_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(lo_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(lo_v))),
                    span,
                );
                let hi_local = self.fresh(scrut_ty);
                self.emit_assign(
                    Place::local(hi_local),
                    Rvalue::Use(Operand::Const(ConstValue::Int(hi_v))),
                    span,
                );
                let ge = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(ge),
                    Rvalue::BinaryOp {
                        op: BinOp::Ge,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(lo_local)),
                    },
                    span,
                );
                let upper_op = if *inclusive { BinOp::Le } else { BinOp::Lt };
                let upper = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(upper),
                    Rvalue::BinaryOp {
                        op: upper_op,
                        lhs: Operand::Copy(Place::local(scrutinee)),
                        rhs: Operand::Copy(Place::local(hi_local)),
                    },
                    span,
                );
                let combined = self.fresh(bool_ty);
                self.emit_assign(
                    Place::local(combined),
                    Rvalue::BinaryOp {
                        op: BinOp::BitAnd,
                        lhs: Operand::Copy(Place::local(ge)),
                        rhs: Operand::Copy(Place::local(upper)),
                    },
                    span,
                );
                Some(combined)
            }
            HirPatKind::Struct { name, fields, .. } => {
                // Struct pattern matching a value of a known
                // struct type — OR a struct-payload enum variant
                // (`Shape::Rect { w, h }` lowers as
                // `HirPatKind::Struct { name: "Rect", ... }`).
                // For an enum-variant struct, the predicate
                // routes to the variant's discriminant index;
                // for a real struct it's always true (shape
                // verified by the type-checker). Each named-field
                // sub-pattern reads through a
                // `Projection::Field(idx)` of the scrutinee.
                let order = self
                    .structs
                    .get(&name.name)
                    .cloned()
                    .or_else(|| self.enums.variant_fields.get(&name.name).cloned());
                let variant_idx = self
                    .enums
                    .lookup(std::slice::from_ref(name))
                    .map(|(_, i)| i);
                // Whether ANY sibling variant of this enum carries
                // a payload — controls whether the runtime layout
                // is `[disc, p0, ...]` (heap aggregate) or just an
                // i64 disc value.
                let any_variant_has_payload =
                    self.enums.has_any_payload(std::slice::from_ref(name));
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let scrut_is_real_struct = self
                    .struct_name_of(self.locals[scrutinee.0 as usize].ty)
                    .is_some_and(|sn| self.structs.contains_key(&sn));
                let scrut_is_payload_enum =
                    variant_idx.is_some() && (any_variant_has_payload || !fields.is_empty());
                // Predicate seed: for an enum variant, compare the
                // scrutinee's discriminant to the variant index;
                // for a free struct, every value of the scrutinee
                // type matches. For payload-bearing enums the
                // scrutinee is a *ptr* to `[disc, p0, p1, ...]`,
                // so we must load disc from offset 0 first —
                // comparing the bare scrutinee (a heap address) to
                // a small variant index always returns false and
                // the arm body never runs (the cli_args /
                // control_flow / data_structures bug).
                let acc = self.fresh(bool_ty);
                if let Some(idx) = variant_idx {
                    let scrut_for_cmp = if scrut_is_payload_enum {
                        let zero_off = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(zero_off),
                            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                            span,
                        );
                        let disc_load = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(disc_load),
                            Rvalue::CallIntrinsic {
                                name: "gos_load",
                                args: vec![
                                    Operand::Copy(Place::local(scrutinee)),
                                    Operand::Copy(Place::local(zero_off)),
                                ],
                            },
                            span,
                        );
                        disc_load
                    } else {
                        scrutinee
                    };
                    let cmp_ty = if scrut_is_payload_enum {
                        i64_ty
                    } else {
                        self.locals[scrutinee.0 as usize].ty
                    };
                    let lit_local = self.fresh(cmp_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                        span,
                    );
                    self.emit_assign(
                        Place::local(acc),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(scrut_for_cmp)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                } else {
                    self.emit_assign(
                        Place::local(acc),
                        Rvalue::Use(Operand::Const(ConstValue::Bool(true))),
                        span,
                    );
                }
                if let Some(order) = order {
                    let declared_tys = self.enums.variant_field_tys.get(&name.name).cloned();
                    for f in fields {
                        let pos = order.iter().position(|n| n == &f.name.name);
                        let Some(pos) = pos else { continue };
                        let field_idx = u32::try_from(pos).ok()?;
                        // The field's HIR-recorded type lives on
                        // the sub-pattern (or, for shorthand, on
                        // the field-pattern itself); fall back to
                        // the enum decl's recorded field type for
                        // shorthand bindings whose `f.pattern` is
                        // `None`. Without the decl-typed fallback
                        // an `f64` field decoded as the I64 load
                        // result, and printing/arithmetic saw the
                        // bit pattern of the float as an integer.
                        let field_ty = match &f.pattern {
                            Some(sub) => sub.ty,
                            None => declared_tys
                                .as_ref()
                                .and_then(|tys| tys.get(pos).copied())
                                .unwrap_or(i64_ty),
                        };
                        let elem = self.fresh(field_ty);
                        if scrut_is_real_struct {
                            // Free struct: real field projection.
                            let field_place = Place {
                                local: scrutinee,
                                projection: vec![crate::ir::Projection::Field(field_idx)],
                            };
                            self.emit_assign(
                                Place::local(elem),
                                Rvalue::Use(Operand::Copy(field_place)),
                                span,
                            );
                        } else if scrut_is_payload_enum {
                            // Enum-variant struct payload: the
                            // scrutinee is a pointer to a heap
                            // aggregate `[disc, p0, p1, …]`.
                            // Load this field from offset
                            // (field_idx + 1) * 8.
                            let off_local = self.fresh(i64_ty);
                            self.emit_assign(
                                Place::local(off_local),
                                Rvalue::Use(Operand::Const(ConstValue::Int(
                                    (i128::from(field_idx) + 1) * 8,
                                ))),
                                span,
                            );
                            self.emit_assign(
                                Place::local(elem),
                                Rvalue::CallIntrinsic {
                                    name: "gos_load",
                                    args: vec![
                                        Operand::Copy(Place::local(scrutinee)),
                                        Operand::Copy(Place::local(off_local)),
                                    ],
                                },
                                span,
                            );
                        } else {
                            // Bare-disc enum variant or unknown
                            // shape: bind to zero so the body
                            // compiles. The exhaustiveness
                            // checker is supposed to gate this.
                            self.emit_assign(
                                Place::local(elem),
                                Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                                span,
                            );
                        }
                        if let Some(sub) = &f.pattern {
                            let sub_pred = self.lower_pattern_predicate(elem, sub, span)?;
                            // AND into the accumulator. Today we
                            // can't easily re-write `acc`, so we
                            // emit a fresh combined local each
                            // iteration; the optimiser collapses
                            // the chain.
                            let combined = self.fresh(bool_ty);
                            self.emit_assign(
                                Place::local(combined),
                                Rvalue::BinaryOp {
                                    op: BinOp::BitAnd,
                                    lhs: Operand::Copy(Place::local(acc)),
                                    rhs: Operand::Copy(Place::local(sub_pred)),
                                },
                                span,
                            );
                            // Rebind acc to the combined value
                            // via another assign.
                            self.emit_assign(
                                Place::local(acc),
                                Rvalue::Use(Operand::Copy(Place::local(combined))),
                                span,
                            );
                        } else {
                            // Shorthand `{ x, y }` binds the field
                            // name directly to the field local.
                            self.bind_local(&f.name.name, elem);
                        }
                    }
                }
                Some(acc)
            }
            HirPatKind::Variant { name, fields } => {
                // Two encodings hide behind variant patterns in
                // compiled mode:
                //
                //   1. **User-defined enums** (registered in the
                //      `EnumIndex`): the scrutinee holds the
                //      variant's declaration index; predicate is
                //      `scrutinee == idx`.
                //   2. **`Option<T>` / `Result<T, E>` stdlib
                //      variants**: the scrutinee carries the
                //      wrapped value directly (happy-path
                //      encoding — `unwrap` is identity). The
                //      compiled tier can't actually distinguish
                //      `Ok(_)` from `Err(_)` at runtime, so the
                //      `Ok` / `Some` arm becomes the unconditional
                //      always-true predicate and `Err` / `None`
                //      becomes always-false. This compiles `?`
                //      down to "take the success path"; programs
                //      that depend on real error dispatch keep
                //      working under `gos run`.
                if let Some((_, idx)) = self.enums.lookup(std::slice::from_ref(name)) {
                    let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                    let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                    // For payload-bearing variants the scrutinee is a
                    // ptr to `[disc, p0, p1, ...]`; for no-payload
                    // enums the scrutinee IS the variant index. The
                    // ctor at lower_user_enum_ctor only allocates
                    // when args.len() > 0, so we must distinguish at
                    // match time: if the variant has fields OR any
                    // sibling variant has fields, treat scrutinee as
                    // ptr and load disc from offset 0. Otherwise the
                    // scrutinee is the i64 index directly.
                    let any_variant_has_payload =
                        self.enums.has_any_payload(std::slice::from_ref(name));
                    // Inline-able enum: the scrutinee is the 2-word by-value
                    // `i128` [disc, payload]; the discriminant is its low word.
                    let mut peeled = scrut_ty;
                    while let gossamer_types::TyKind::Ref { inner, .. } = self.tcx.kind_of(peeled) {
                        peeled = *inner;
                    }
                    let scrut_is_inline = self.tcx.is_inline_enum_ty(peeled);
                    let scrut_for_cmp = if scrut_is_inline {
                        let disc_load = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(disc_load),
                            Rvalue::CallIntrinsic {
                                name: "gos_rt_result_disc",
                                args: vec![Operand::Copy(Place::local(scrutinee))],
                            },
                            span,
                        );
                        disc_load
                    } else if any_variant_has_payload || !fields.is_empty() {
                        let zero_off = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(zero_off),
                            Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                            span,
                        );
                        let disc_load = self.fresh(i64_ty);
                        self.emit_assign(
                            Place::local(disc_load),
                            Rvalue::CallIntrinsic {
                                name: "gos_load",
                                args: vec![
                                    Operand::Copy(Place::local(scrutinee)),
                                    Operand::Copy(Place::local(zero_off)),
                                ],
                            },
                            span,
                        );
                        disc_load
                    } else {
                        scrutinee
                    };
                    let lit_local = self.fresh(scrut_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(idx as i128))),
                        span,
                    );
                    let cmp = self.fresh(bool_ty);
                    self.emit_assign(
                        Place::local(cmp),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(scrut_for_cmp)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                    // Bind payload fields and check nested patterns by
                    // loading from offsets (i+1)*8 of the scrutinee pointer.
                    let any_payload = self.enums.has_any_payload(std::slice::from_ref(name));
                    let declared_tys = self.enums.variant_field_tys.get(&name.name).cloned();
                    let mut acc = cmp;
                    for (i, field) in fields.iter().enumerate() {
                        if let HirPatKind::Binding { name: bname, .. } = &field.kind {
                            if scrut_is_inline {
                                // 2-word by-value enum: the single field is the
                                // payload high word.
                                let binding_ty = declared_tys
                                    .as_ref()
                                    .and_then(|tys| tys.get(i).copied())
                                    .filter(|&ty| {
                                        !matches!(
                                            self.tcx.kind_of(ty),
                                            gossamer_types::TyKind::Var(_)
                                                | gossamer_types::TyKind::Error
                                        )
                                    })
                                    .unwrap_or(i64_ty);
                                let is_f64 = matches!(
                                    self.tcx.kind_of(binding_ty),
                                    gossamer_types::TyKind::Float(_)
                                );
                                let getter = if is_f64 {
                                    "gos_rt_result_payload_f64"
                                } else {
                                    "gos_rt_result_payload"
                                };
                                let payload_local = self.fresh(binding_ty);
                                self.emit_assign(
                                    Place::local(payload_local),
                                    Rvalue::CallIntrinsic {
                                        name: getter,
                                        args: vec![Operand::Copy(Place::local(scrutinee))],
                                    },
                                    span,
                                );
                                self.bind_local(&bname.name, payload_local);
                            } else if any_payload || !fields.is_empty() {
                                let off_local = self.fresh(i64_ty);
                                self.emit_assign(
                                    Place::local(off_local),
                                    Rvalue::Use(Operand::Const(ConstValue::Int(
                                        ((i + 1) * 8) as i128,
                                    ))),
                                    span,
                                );
                                // Use the declared variant field type (e.g. f64) so
                                // that define_var_to_with can bitcast the I64 result
                                // of gos_load to the correct type.
                                let binding_ty = declared_tys
                                    .as_ref()
                                    .and_then(|tys| tys.get(i).copied())
                                    .filter(|&ty| {
                                        !matches!(
                                            self.tcx.kind_of(ty),
                                            gossamer_types::TyKind::Var(_)
                                                | gossamer_types::TyKind::Error
                                        )
                                    })
                                    .unwrap_or(i64_ty);
                                let payload_local = self.fresh(binding_ty);
                                self.emit_assign(
                                    Place::local(payload_local),
                                    Rvalue::CallIntrinsic {
                                        name: "gos_load",
                                        args: vec![
                                            Operand::Copy(Place::local(scrutinee)),
                                            Operand::Copy(Place::local(off_local)),
                                        ],
                                    },
                                    span,
                                );
                                self.bind_local(&bname.name, payload_local);
                            } else {
                                self.bind_local(&bname.name, scrutinee);
                            }
                        } else if !matches!(field.kind, HirPatKind::Wildcard) {
                            // Nested constructor pattern (e.g. Color::Red inside
                            // Shape::Circle): load the field as i64 and recurse.
                            if scrut_is_inline {
                                let field_local = self.fresh(i64_ty);
                                self.emit_assign(
                                    Place::local(field_local),
                                    Rvalue::CallIntrinsic {
                                        name: "gos_rt_result_payload",
                                        args: vec![Operand::Copy(Place::local(scrutinee))],
                                    },
                                    span,
                                );
                                if let Some(sub_pred) =
                                    self.lower_pattern_predicate(field_local, field, span)
                                {
                                    let combined = self.fresh(bool_ty);
                                    self.emit_assign(
                                        Place::local(combined),
                                        Rvalue::BinaryOp {
                                            op: BinOp::BitAnd,
                                            lhs: Operand::Copy(Place::local(acc)),
                                            rhs: Operand::Copy(Place::local(sub_pred)),
                                        },
                                        span,
                                    );
                                    acc = combined;
                                }
                            } else if any_payload || !fields.is_empty() {
                                let off_local = self.fresh(i64_ty);
                                self.emit_assign(
                                    Place::local(off_local),
                                    Rvalue::Use(Operand::Const(ConstValue::Int(
                                        ((i + 1) * 8) as i128,
                                    ))),
                                    span,
                                );
                                let field_local = self.fresh(i64_ty);
                                self.emit_assign(
                                    Place::local(field_local),
                                    Rvalue::CallIntrinsic {
                                        name: "gos_load",
                                        args: vec![
                                            Operand::Copy(Place::local(scrutinee)),
                                            Operand::Copy(Place::local(off_local)),
                                        ],
                                    },
                                    span,
                                );
                                if let Some(sub_pred) =
                                    self.lower_pattern_predicate(field_local, field, span)
                                {
                                    let combined = self.fresh(bool_ty);
                                    self.emit_assign(
                                        Place::local(combined),
                                        Rvalue::BinaryOp {
                                            op: BinOp::BitAnd,
                                            lhs: Operand::Copy(Place::local(acc)),
                                            rhs: Operand::Copy(Place::local(sub_pred)),
                                        },
                                        span,
                                    );
                                    acc = combined;
                                }
                            }
                        }
                    }
                    return Some(acc);
                }
                // Result/Option dispatch picks one of two paths
                // based on the scrutinee's static type:
                //
                //   * Concrete `Result<T, E>` / `Option<T>` (Adt
                //     with our sentinel DefId): the scrutinee is a
                //     `*mut GosResult` carrying a real disc bit.
                //     Compare `gos_rt_result_disc(scrut)` to the
                //     expected arm value.
                //
                //   * Unresolved (`Var` / `Error` / `Never`) or any
                //     other shape: fall back to the happy-path
                //     encoding so legacy producers (`.send()`,
                //     `.map_err()`-chains whose return type the
                //     typer left as `Var`) keep working. `Ok` /
                //     `Some` arms are unconditionally true; `Err`
                //     / `None` arms unconditionally false.
                let expected_disc: i64 = match name.name.as_str() {
                    "Ok" | "Some" => 0,
                    _ => 1,
                };
                let scrut_ty = self.locals[scrutinee.0 as usize].ty;
                let scrut_kind = self.tcx.kind_of(scrut_ty);
                let real_disc = matches!(
                    scrut_kind,
                    TyKind::Adt { .. } if self.is_result_or_option_adt(scrut_ty)
                );
                let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                let const_pred = self.fresh(bool_ty);
                if real_disc {
                    let disc_local = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(disc_local),
                        Rvalue::CallIntrinsic {
                            name: "gos_rt_result_disc",
                            args: vec![Operand::Copy(Place::local(scrutinee))],
                        },
                        span,
                    );
                    let lit_local = self.fresh(i64_ty);
                    self.emit_assign(
                        Place::local(lit_local),
                        Rvalue::Use(Operand::Const(ConstValue::Int(i128::from(expected_disc)))),
                        span,
                    );
                    self.emit_assign(
                        Place::local(const_pred),
                        Rvalue::BinaryOp {
                            op: BinOp::Eq,
                            lhs: Operand::Copy(Place::local(disc_local)),
                            rhs: Operand::Copy(Place::local(lit_local)),
                        },
                        span,
                    );
                } else {
                    let happy_path = expected_disc == 0;
                    self.emit_assign(
                        Place::local(const_pred),
                        Rvalue::Use(Operand::Const(ConstValue::Bool(happy_path))),
                        span,
                    );
                }
                if let Some(first) = fields.first() {
                    // Accept both `Some(x)` (Binding) and `Some((a, b))` (Tuple payload).
                    let is_binding = matches!(first.kind, HirPatKind::Binding { .. });
                    let is_tuple_payload = matches!(first.kind, HirPatKind::Tuple(_));
                    if is_binding || is_tuple_payload {
                        // Bind the payload. With real discriminant
                        // encoding, allocate a fresh local from
                        // `gos_rt_result_payload`. With happy-path
                        // encoding, bind directly to the scrutinee
                        // (legacy: the scrutinee value IS the
                        // payload). Pin the binding's MIR type
                        // from the scrutinee's substs slot.
                        let payload_slot = match name.name.as_str() {
                            "Ok" | "Some" => Some(0),
                            "Err" => Some(1),
                            _ => None,
                        };
                        let payload_ty = payload_slot
                            .and_then(|idx| self.adt_generic_at(scrut_ty, idx))
                            .unwrap_or(i64_ty);
                        // Redirect payload + binding emissions to arm_block when
                        // the match arm loop set `payload_defer_block`. Unconditional
                        // payload extraction in the pre-branch header dereferences a
                        // null pointer when the scrutinee is None/Err on re-entry.
                        let saved_current = self.current;
                        if let Some(defer) = self.payload_defer_block.take() {
                            self.current = Some(defer);
                        }
                        let payload_local = if real_disc {
                            let p = self.fresh(payload_ty);
                            // For Ok(f64) / Some(f64) payloads the
                            // i64 bit-pattern packing must be unpacked
                            // via `bitcast`, not `sitofp`. Route those
                            // through the dedicated `_f64` shim so the
                            // LLVM tier reads the f64 value back as
                            // its source type instead of treating the
                            // bit pattern as an integer.
                            let payload_extractor = if matches!(
                                self.tcx.kind_of(payload_ty),
                                gossamer_types::TyKind::Float(_)
                            ) {
                                "gos_rt_result_payload_f64"
                            } else {
                                "gos_rt_result_payload"
                            };
                            self.emit_assign(
                                Place::local(p),
                                Rvalue::CallIntrinsic {
                                    name: payload_extractor,
                                    args: vec![Operand::Copy(Place::local(scrutinee))],
                                },
                                span,
                            );
                            p
                        } else {
                            // Happy-path: alias the scrutinee.
                            // Repin the scrutinee's MIR type when
                            // the substs gave a concrete payload
                            // type AND the scrutinee's current
                            // type is less specific (unresolved
                            // OR a generic i64 fallback that the
                            // dispatch left behind for a
                            // `Vec<Option<String>>`-style index).
                            // Without this `let row[i] : i64;
                            // match row[i] { Some(k: String) ... }`
                            // would print `k` through the integer
                            // formatter — the regex captures_all
                            // case where strings rendered as raw
                            // pointer ints. The original guard
                            // also kept tagged structs
                            // (`http::Response`/json::Value) safe
                            // by refusing to overwrite — preserve
                            // that by only repinning when the
                            // payload is a *non-Int* concrete
                            // type (String / Bool / Char / Float)
                            // OR the scrutinee is genuinely
                            // unresolved.
                            let payload_kind = self.tcx.kind_of(payload_ty).clone();
                            let scrut_kind_now = self
                                .tcx
                                .kind_of(self.locals[scrutinee.0 as usize].ty)
                                .clone();
                            let scrut_unresolved = matches!(
                                scrut_kind_now,
                                TyKind::Var(_) | TyKind::Error | TyKind::Never,
                            );
                            let payload_concrete = !matches!(
                                payload_kind,
                                TyKind::Var(_) | TyKind::Error | TyKind::Never,
                            );
                            let payload_overrides_int =
                                matches!(
                                    payload_kind,
                                    TyKind::String | TyKind::Bool | TyKind::Char | TyKind::Float(_)
                                ) && matches!(scrut_kind_now, TyKind::Int(_));
                            if payload_concrete && (scrut_unresolved || payload_overrides_int) {
                                self.locals[scrutinee.0 as usize].ty = payload_ty;
                            }
                            scrutinee
                        };
                        if is_binding {
                            let HirPatKind::Binding { name: bname, .. } = &first.kind else {
                                unreachable!()
                            };
                            self.bind_local(&bname.name, payload_local);
                            // Tag the payload local so
                            // `binding.field` / `binding.method` calls
                            // route through the right struct / runtime
                            // dispatch path. The struct/runtime-kind
                            // info is inherited from the wrapper's
                            // generic args (`Result<Opts, _>` →
                            // payload of Ok arm gets Opts).
                            if let Some(sname) = self.struct_name_of(first.ty) {
                                self.local_struct.insert(payload_local, sname);
                            }
                            let scrut_outer_ty = self.locals[scrutinee.0 as usize].ty;
                            let inner_ty = if name.name == "Err" {
                                self.second_generic_of(scrut_outer_ty)
                            } else {
                                self.first_generic_of(scrut_outer_ty)
                            };
                            if let Some(inner) = inner_ty {
                                if let Some(sname) = self.struct_name_of(inner) {
                                    let runtime_kind: Option<&'static str> = match sname.as_str() {
                                        "Error" => Some("errors::Error"),
                                        "Response" => Some("http::Response"),
                                        "Request" => Some("http::Request"),
                                        "Client" => Some("http::Client"),
                                        "Scanner" => Some("bufio::Scanner"),
                                        "Pattern" => Some("regex::Pattern"),
                                        _ => None,
                                    };
                                    self.local_struct.insert(payload_local, sname);
                                    if let Some(rk) = runtime_kind {
                                        self.local_runtime_kind.insert(payload_local, rk);
                                    }
                                }
                            }
                            if let Some(rk) = self.local_runtime_kind.get(&scrutinee).copied() {
                                self.local_runtime_kind.entry(payload_local).or_insert(rk);
                            }
                            if let Some(elem) = self.local_elem_struct.get(&scrutinee).cloned() {
                                self.local_elem_struct.entry(payload_local).or_insert(elem);
                            }
                        } else if let HirPatKind::Tuple(sub_pats) = &first.kind {
                            // `Some((a, b))` — unpack the tuple payload
                            // into individual field bindings.
                            self.bind_tuple_pattern(payload_local, sub_pats, span);
                        }
                        // Restore the header block so the caller sees
                        // `const_pred` in the right block context.
                        self.current = saved_current;
                    } else if !matches!(first.kind, HirPatKind::Wildcard) {
                        // Concrete payload pattern: literal, range, nested
                        // variant, or-pattern, etc. (e.g. `Ok(1)`, `Ok(1..=5)`).
                        // `gos_rt_result_payload` null-checks internally, so
                        // calling it unconditionally in the pre-branch header
                        // is safe regardless of discriminant. The disc predicate
                        // (`const_pred`) will be false when the variant doesn't
                        // match, making the combined AND false too.
                        let payload_slot = match name.name.as_str() {
                            "Ok" | "Some" => Some(0usize),
                            "Err" => Some(1usize),
                            _ => None,
                        };
                        let payload_ty = payload_slot
                            .and_then(|idx| self.adt_generic_at(scrut_ty, idx))
                            .unwrap_or(i64_ty);
                        let payload_local = if real_disc {
                            let p = self.fresh(payload_ty);
                            // For Ok(f64) / Some(f64) payloads the
                            // i64 bit-pattern packing must be unpacked
                            // via `bitcast`, not `sitofp`. Route those
                            // through the dedicated `_f64` shim so the
                            // LLVM tier reads the f64 value back as
                            // its source type instead of treating the
                            // bit pattern as an integer.
                            let payload_extractor = if matches!(
                                self.tcx.kind_of(payload_ty),
                                gossamer_types::TyKind::Float(_)
                            ) {
                                "gos_rt_result_payload_f64"
                            } else {
                                "gos_rt_result_payload"
                            };
                            self.emit_assign(
                                Place::local(p),
                                Rvalue::CallIntrinsic {
                                    name: payload_extractor,
                                    args: vec![Operand::Copy(Place::local(scrutinee))],
                                },
                                span,
                            );
                            p
                        } else {
                            scrutinee
                        };
                        if let Some(sub_pred) =
                            self.lower_pattern_predicate(payload_local, first, span)
                        {
                            let combined = self.fresh(bool_ty);
                            self.emit_assign(
                                Place::local(combined),
                                Rvalue::BinaryOp {
                                    op: BinOp::BitAnd,
                                    lhs: Operand::Copy(Place::local(const_pred)),
                                    rhs: Operand::Copy(Place::local(sub_pred)),
                                },
                                span,
                            );
                            return Some(combined);
                        }
                    }
                }
                Some(const_pred)
            }
            HirPatKind::Literal(_) => None,
            HirPatKind::At { name, sub, .. } => {
                // `x @ subpat`: bind `x` to the scrutinee, then run
                // the subpattern's filter. Without recursing into
                // `sub` the constraint is dropped — every input
                // matches the arm and the user's intent (e.g.
                // `x @ 1..=3`) is silently widened to `x => …`.
                self.bind_local(&name.name, scrutinee);
                self.lower_pattern_predicate(scrutinee, sub, span)
            }
        }
    }

    pub(crate) fn lower_cast(
        &mut self,
        value: &HirExpr,
        target: Ty,
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        let value_local = self.lower_expr(value)?;
        let dest = self.fresh(ty);
        self.emit_assign(
            Place::local(dest),
            Rvalue::Cast {
                operand: Operand::Copy(Place::local(value_local)),
                target,
            },
            span,
        );
        Some(dest)
    }

    pub(crate) fn bind_tuple_pattern(
        &mut self,
        tuple_local: Local,
        sub_patterns: &[HirPat],
        span: Span,
    ) {
        use gossamer_types::TyKind;
        let rest_pos = sub_patterns
            .iter()
            .position(|p| matches!(p.kind, HirPatKind::Rest));
        let n_after = rest_pos.map_or(0, |r| sub_patterns.len() - r - 1);
        // Determine the tuple's total field count from its type for
        // correct rest-pattern tail indexing.
        let total_len = if rest_pos.is_some() {
            let tuple_ty = self.locals[tuple_local.0 as usize].ty;
            if let TyKind::Tuple(elems) = self.tcx.kind_of(tuple_ty).clone() {
                elems.len()
            } else {
                sub_patterns.len()
            }
        } else {
            sub_patterns.len()
        };
        for (i, sub) in sub_patterns.iter().enumerate() {
            if matches!(sub.kind, HirPatKind::Rest | HirPatKind::Wildcard) {
                continue;
            }
            let HirPatKind::Binding { name, mutable } = &sub.kind else {
                continue;
            };
            let field_idx = if let Some(rest_idx) = rest_pos {
                if i < rest_idx {
                    i
                } else {
                    total_len - n_after + (i - rest_idx - 1)
                }
            } else {
                i
            };
            // Use the sub-pattern's type when concrete; derive from the
            // tuple's field type otherwise. Without this fallback, bindings
            // from `Some((n, m))` get `Var(_)` types and the print dispatcher
            // calls `gos_rt_concat_str` instead of `gos_rt_concat_i64`.
            let pat_ty_unresolved = matches!(
                self.tcx.kind_of(sub.ty).clone(),
                TyKind::Var(_) | TyKind::Error | TyKind::Never
            );
            let elem_ty = if pat_ty_unresolved {
                let tuple_ty = self.locals[tuple_local.0 as usize].ty;
                if let TyKind::Tuple(elems) = self.tcx.kind_of(tuple_ty).clone() {
                    elems.get(field_idx).copied().unwrap_or(sub.ty)
                } else {
                    sub.ty
                }
            } else {
                sub.ty
            };
            let element_local =
                self.push_local(elem_ty, Some(Ident::new(name.name.as_str())), *mutable);
            self.bind_local(name.name.as_str(), element_local);
            let projection = vec![crate::ir::Projection::Field(
                u32::try_from(field_idx).expect("tuple projection overflow"),
            )];
            let place = Place {
                local: tuple_local,
                projection,
            };
            self.emit_assign(
                Place::local(element_local),
                Rvalue::Use(Operand::Copy(place)),
                span,
            );
        }
    }

    pub(crate) fn lower_while(&mut self, condition: &HirExpr, body: &HirExpr, span: Span) {
        let header = self.new_block(span);
        let body_block = self.new_block(span);
        let exit = self.new_block(span);
        self.terminate(Terminator::Goto { target: header });

        self.set_current(header);
        let Some(cond_local) = self.lower_expr(condition) else {
            return;
        };
        self.terminate(Terminator::SwitchInt {
            discriminant: Operand::Copy(Place::local(cond_local)),
            arms: vec![(0, exit)],
            default: body_block,
        });

        self.set_current(body_block);
        // Auto-region: if the body's allocations provably die at the
        // iteration boundary, wrap it in an arena region so the whole
        // iteration's heap is bulk-freed at the back-edge instead of a
        // per-node refcount teardown. Eligibility is decided on the HIR
        // before lowering so `region_depth` flags the body's locals.
        let regioned = self.loop_body_region_eligible(body);
        if regioned {
            self.emit_region_call("gos_rt_region_push", span);
            self.region_depth += 1;
        }
        // `break` jumps to `exit`; `continue` jumps back to the
        // condition test (`header`).
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
            result: None,
            break_used: false,
        });
        let _ = self.lower_expr(body);
        self.loop_stack.pop();
        if regioned {
            self.region_depth = self.region_depth.saturating_sub(1);
            // Eligibility guarantees no early exit, so `current` is the
            // body's fall-through; pop the region before the back-edge.
            if self.current.is_some() {
                self.emit_region_call("gos_rt_region_pop", span);
            }
        }
        self.terminate(Terminator::Goto { target: header });

        self.set_current(exit);
    }

    /// Emits a zero-argument unit-returning call to a runtime region helper
    /// (`gos_rt_region_push` / `gos_rt_region_pop`) and continues lowering
    /// in a fresh block.
    pub(crate) fn emit_region_call(&mut self, sym: &str, span: Span) {
        let unit = self.tcx.unit();
        let dest = self.fresh(unit);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(sym.to_string())),
            args: vec![],
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
    }

    /// Conservative escape check: is this loop body safe to auto-region?
    pub(crate) fn loop_body_region_eligible(&self, body: &HirExpr) -> bool {
        crate::lower::helpers::LoopEligibility::new(&*self.tcx, self.region_unsafe).check(body)
    }

    pub(crate) fn lower_loop(&mut self, body: &HirExpr, ty: Ty, span: Span) -> Option<Local> {
        if let Some(for_loop) = detect_for_loop(body) {
            if let Some(result) = self.try_lower_for_loop(&for_loop, span) {
                return Some(result);
            }
        }
        let header = self.new_block(span);
        let exit = self.new_block(span);
        // Pre-allocate a result local so `break <expr>` has somewhere
        // to write its payload. `loop` is an expression in Gossamer
        // (`let x = loop { ... break v }`), and the typechecker pins
        // `ty` to the unified break-payload type.
        let result_local = self.fresh(ty);
        self.terminate(Terminator::Goto { target: header });
        self.set_current(header);
        self.loop_stack.push(LoopContext {
            continue_to: header,
            break_to: exit,
            result: Some(result_local),
            break_used: false,
        });
        let _ = self.lower_expr(body);
        let ctx = self.loop_stack.pop().expect("loop stack underflow");
        self.terminate(Terminator::Goto { target: header });
        self.set_current(exit);
        if ctx.break_used {
            Some(result_local)
        } else {
            None
        }
    }

    pub(crate) fn try_lower_for_loop(
        &mut self,
        for_loop: &ForLoopShape<'_>,
        span: Span,
    ) -> Option<Local> {
        use gossamer_types::TyKind;
        // The local-type recovery probes below key off a bare
        // `Path` iter expression. A `for c in &xs` loop lowers the
        // iterand as `Unary { RefShared, Path("xs") }`, so peel a
        // leading `&` / `&mut` wrapper first — otherwise the probe
        // misses the binding, the element type defaults to i64, and a
        // `[String]` iterates as raw heap pointers (atlas_db's schema
        // corruption: `orders.<ptr>` instead of `orders.id`).
        let probe_expr = match &for_loop.iter_expr.kind {
            HirExprKind::Unary {
                op: gossamer_hir::HirUnaryOp::RefShared | gossamer_hir::HirUnaryOp::RefMut,
                operand,
                ..
            } => operand.as_ref(),
            _ => for_loop.iter_expr,
        };
        // If the iter expression is a user-defined struct (Adt),
        // the fast-paths below would all misfire and the default
        // fallback would treat it as a runtime Vec (reading `len`
        // off the wrong offset and silently running zero
        // iterations). Bail out so the generic `loop` lowering
        // drives the HIR `match iter.next() { Some => ..., None
        // => break }` desugar against the user's `impl` method.
        let mut iter_ty_probe = for_loop.iter_expr.ty;
        for _ in 0..8 {
            match self.tcx.kind_of(iter_ty_probe) {
                TyKind::Ref { inner, .. } => iter_ty_probe = *inner,
                TyKind::Adt { .. } => return None,
                _ => break,
            }
        }
        // Same probe through the local table — the HIR type for a
        // `Path("c")` iter expression often arrives as `Var(_)`
        // even though the local was pinned to the struct on its
        // `let` statement; the local's MIR-side type is the
        // authoritative source.
        if let HirExprKind::Path { segments, .. } = &probe_expr.kind {
            if let Some(first) = segments.first() {
                if let Some(local) = self.lookup_local(&first.name) {
                    let mut local_ty = self.locals[local.0 as usize].ty;
                    for _ in 0..8 {
                        match self.tcx.kind_of(local_ty) {
                            TyKind::Ref { inner, .. } => local_ty = *inner,
                            TyKind::Adt { .. } => return None,
                            _ => break,
                        }
                    }
                }
            }
        }
        // `for (k, v) in m.iter()` on a HashMap. Snapshot the keys
        // into a fresh `GosVec`, iterate it, and inside each iteration
        // synthesise `v = m.get_or(k, default)` so the tuple pattern
        // bindings see real values.
        if let Some(local) = self.try_lower_for_hashmap_iter(for_loop, span) {
            return Some(local);
        }
        // `for (idx, x) in v.iter().enumerate()` / `for (idx, x) in
        // v.enumerate()`. Strip `.enumerate()` (and a wrapping
        // `.iter()` if present), then drive the standard array /
        // vec loop while binding `idx` to the per-iteration counter
        // and `x` to the element. Without this, the for-loop falls
        // through to the array/vec dispatch with a 2-tuple loop_pat
        // and the body never runs (no fields on a scalar element).
        if let Some(inner) = enumerate_inner_expr(for_loop.iter_expr) {
            if let HirPatKind::Tuple(sub_pats) = &for_loop.loop_pat.kind {
                if sub_pats.len() == 2 {
                    return self.lower_for_enumerate(
                        inner,
                        &sub_pats[0],
                        &sub_pats[1],
                        for_loop.body,
                        span,
                    );
                }
            }
        }
        // `for entry in v.iter()` / `for entry in v` where v is a
        // `json::Value` array — synthesise the loop with
        // `gos_rt_json_len` + `gos_rt_json_at`.
        let iter_target = match &for_loop.iter_expr.kind {
            HirExprKind::MethodCall { receiver, name, .. } if name.name == "iter" => {
                Some(receiver.as_ref())
            }
            _ => None,
        };
        let json_iter = iter_target.filter(|recv| {
            let recv_ty = self
                .receiver_local_from_path(recv)
                .map_or(recv.ty, |local| self.locals[local.0 as usize].ty);
            self.is_json_value_ty(recv_ty)
        });
        if let Some(recv) = json_iter {
            return self.lower_for_json(recv, for_loop.loop_pat, for_loop.body, span);
        }
        if self.is_json_value_ty(for_loop.iter_expr.ty) {
            return self.lower_for_json(for_loop.iter_expr, for_loop.loop_pat, for_loop.body, span);
        }
        // `for byte in s.as_bytes()` / `for byte in s.as_bytes().iter()`
        // — `s` is a String. The Vec fallback below would call
        // `gos_rt_vec_len`/`gos_rt_vec_get_ptr` on a `*const c_char`
        // and segfault on the read of the (non-existent) Vec
        // header. Detect the shape and emit a strlen-bound counter
        // loop reading bytes via `gos_rt_str_byte_at`.
        if let Some(string_expr) = self.detect_string_bytes_iter(for_loop.iter_expr) {
            return self.lower_for_string_bytes(
                string_expr,
                for_loop.loop_pat,
                for_loop.body,
                span,
            );
        }
        match &for_loop.iter_expr.kind {
            HirExprKind::Range {
                start: Some(start),
                end: Some(end),
                inclusive,
            } => self.lower_for_range(
                start,
                end,
                *inclusive,
                for_loop.loop_pat,
                for_loop.body,
                span,
            ),
            HirExprKind::Array(arr) => {
                let len = match arr {
                    gossamer_hir::HirArrayExpr::List(elems) => elems.len() as i64,
                    gossamer_hir::HirArrayExpr::Repeat { count, .. } => {
                        literal_u64(count).and_then(|c| i64::try_from(c).ok())?
                    }
                };
                self.lower_for_array(
                    for_loop.iter_expr,
                    for_loop.loop_pat,
                    for_loop.body,
                    len,
                    span,
                )
            }
            _ => {
                // Fallback chain:
                //   1. fixed-size `[T; N]` (`&[T; N]`) → array iter.
                //   2. runtime `Vec<T>` (or peeled-through-`&`)
                //      → `gos_rt_vec_*` dynamic-length iter.
                //   3. give up (the for-loop fallback re-emits
                //      the original `loop {}` shape).
                let mut cur = for_loop.iter_expr.ty;
                // Also peel `.iter()` method calls — `for x in v.iter()`
                // and `for x in &v` both end up wanting Vec iteration.
                let iter_recv = match &for_loop.iter_expr.kind {
                    HirExprKind::MethodCall { receiver, name, .. } if name.name == "iter" => {
                        Some(receiver.as_ref())
                    }
                    _ => None,
                };
                if let Some(recv) = iter_recv {
                    let recv_ty = self
                        .receiver_local_from_path(recv)
                        .map_or(recv.ty, |local| self.locals[local.0 as usize].ty);
                    // Also try the receiver's HIR-expression kind:
                    // `[..].iter()` has receiver = Array(...) whose
                    // ty may be unresolved on the MIR side; the AST
                    // shape gives us the literal length directly.
                    if let HirExprKind::Array(arr) = &recv.kind {
                        let len = match arr {
                            gossamer_hir::HirArrayExpr::List(elems) => Some(elems.len() as i64),
                            gossamer_hir::HirArrayExpr::Repeat { count, .. } => {
                                literal_u64(count).and_then(|c| i64::try_from(c).ok())
                            }
                        };
                        if let Some(len) = len {
                            return self.lower_for_array(
                                recv,
                                for_loop.loop_pat,
                                for_loop.body,
                                len,
                                span,
                            );
                        }
                    }
                    let mut peeled = recv_ty;
                    let mut found_elem: Option<Ty> = None;
                    let mut found_len: Option<i64> = None;
                    loop {
                        match self.tcx.kind_of(peeled) {
                            TyKind::Vec(elem) | TyKind::Slice(elem) => {
                                found_elem = Some(*elem);
                                break;
                            }
                            TyKind::Array { len, elem } => {
                                if let Ok(l) = i64::try_from(*len) {
                                    found_len = Some(l);
                                    found_elem = Some(*elem);
                                }
                                break;
                            }
                            TyKind::Ref { inner, .. } => peeled = *inner,
                            _ => break,
                        }
                    }
                    if let Some(len) = found_len {
                        return self.lower_for_array(
                            recv,
                            for_loop.loop_pat,
                            for_loop.body,
                            len,
                            span,
                        );
                    }
                    // For `.iter()` on a receiver whose MIR type
                    // didn't resolve to a Vec/Slice/Array (often
                    // because the receiver is a field projection
                    // through a Var-typed parent), default to
                    // runtime-Vec iteration. The receiver value
                    // is whatever `gos_rt_arr_iter` returns on it
                    // (identity for slices/vecs); the loop reads
                    // each element via `gos_rt_vec_get_ptr` +
                    // `gos_load`. Element type defaults to i64.
                    let elem_ty =
                        found_elem.unwrap_or_else(|| self.tcx.int_ty(gossamer_types::IntTy::I64));
                    return self.lower_for_vec(
                        recv,
                        elem_ty,
                        for_loop.loop_pat,
                        for_loop.body,
                        span,
                    );
                }
                // If the iter expression is a Path bound to a
                // local, prefer the local's MIR-pinned type to
                // the HIR expression type — the typechecker
                // often leaves stdlib-call results as Var, but
                // the MIR side may have pinned them to a
                // concrete `Vec<T>` via a runtime-helper return
                // type pin.
                if let HirExprKind::Path { segments, .. } = &probe_expr.kind {
                    if let Some(first) = segments.first() {
                        if let Some(local) = self.lookup_local(&first.name) {
                            cur = self.locals[local.0 as usize].ty;
                        }
                    }
                }
                // Check whether the HIR-expression type or the
                // chained method-call return type pins the iter
                // expression to a `Vec<T>`. The MIR-side dispatch
                // for `s.split(...)`, `.lines()`, `.iter()`, etc.
                // already pins their return to `Vec<String>` via
                // `pinned_ret`, so by the time we get here we can
                // look at the call target name + walk the
                // dispatch table to reach the Vec(elem) shape.
                let mut for_vec_elem: Option<Ty> = None;
                if let TyKind::Vec(elem) = self.tcx.kind_of(cur) {
                    for_vec_elem = Some(*elem);
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::MethodCall { name, .. } = &for_loop.iter_expr.kind {
                        if matches!(name.name.as_str(), "split" | "lines") {
                            for_vec_elem = Some(self.tcx.string_ty());
                        }
                    }
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::MethodCall { name, receiver, .. } = &for_loop.iter_expr.kind
                    {
                        if matches!(name.name.as_str(), "keys" | "values") {
                            let recv_ty = self
                                .receiver_local_from_path(receiver)
                                .map_or(receiver.ty, |l| self.locals[l.0 as usize].ty);
                            if matches!(self.tcx.kind_of(recv_ty), TyKind::HashMap { .. })
                                || matches!(self.tcx.kind_of(recv_ty), TyKind::Ref { .. })
                                    && self.hash_map_key_kind(recv_ty).is_some()
                            {
                                let elem = if name.name.as_str() == "keys" {
                                    match self.hash_map_key_kind(recv_ty) {
                                        Some(MapKeyKind::String) => self.tcx.string_ty(),
                                        _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                                    }
                                } else {
                                    match self.hash_map_value_kind(recv_ty) {
                                        Some(MapValueKind::String) => self.tcx.string_ty(),
                                        _ => self.tcx.int_ty(gossamer_types::IntTy::I64),
                                    }
                                };
                                for_vec_elem = Some(elem);
                            }
                        }
                    }
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::Call { callee, args } = &for_loop.iter_expr.kind {
                        if let HirExprKind::Path { segments, .. } = &callee.kind {
                            let names: Vec<&str> =
                                segments.iter().map(|s| s.name.as_str()).collect();
                            if let Some((_, _, item)) =
                                self.resolve_external_binding(&names, args.len())
                            {
                                if let gossamer_resolve::BindingType::Vec(inner) = &item.ret {
                                    for_vec_elem = Some(self.binding_type_to_mir(inner));
                                }
                            }
                        }
                    }
                }
                if for_vec_elem.is_none() {
                    if let HirExprKind::Call { callee, .. } = &for_loop.iter_expr.kind {
                        if let HirExprKind::Path { segments, .. } = &callee.kind {
                            let joined = segments
                                .iter()
                                .map(|s| s.name.as_str())
                                .collect::<Vec<_>>()
                                .join("::");
                            // `regex::find_all` returns `Vec<(i64,
                            // i64, String)>` per the public API.
                            // The runtime stores 24-byte tuples, so
                            // pin the element type to a 3-tuple here
                            // — without this the loop body treats
                            // each element as a single i64/String,
                            // crashing on `hit.2` past the end of
                            // a wrongly-strided buffer.
                            let i64_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                            let str_ty = self.tcx.string_ty();
                            let tup_ty = self.tcx.intern(gossamer_types::TyKind::Tuple(vec![
                                i64_ty, i64_ty, str_ty,
                            ]));
                            // `captures_all` returns
                            // `Vec<Vec<Option<String>>>`; iterating it
                            // gives a `Vec<Option<String>>` per match.
                            // The element must be `Option<String>` (not
                            // a bare `String`) so `row[i]` indexing types
                            // as the tagged-union and `match row[i] {
                            // Some(k) => …, None => … }` reads the real
                            // discriminant rather than the happy-path
                            // encoding.
                            let opt_str_ty = self.option_string_ty();
                            let opt_str_vec_ty =
                                self.tcx.intern(gossamer_types::TyKind::Vec(opt_str_ty));
                            for_vec_elem = match joined.as_str() {
                                "regex::find_all" | "std::regex::find_all" => Some(tup_ty),
                                "regex::split" | "std::regex::split" => Some(str_ty),
                                "regex::captures_all" | "std::regex::captures_all" => {
                                    Some(opt_str_vec_ty)
                                }
                                _ => None,
                            };
                        }
                    }
                }
                if let Some(elem) = for_vec_elem {
                    return self.lower_for_vec(
                        for_loop.iter_expr,
                        elem,
                        for_loop.loop_pat,
                        for_loop.body,
                        span,
                    );
                }
                let len_opt = loop {
                    match self.tcx.kind_of(cur) {
                        TyKind::Array { len, .. } => {
                            break i64::try_from(*len).ok();
                        }
                        TyKind::Vec(elem) | TyKind::Slice(elem) => {
                            let elem = *elem;
                            return self.lower_for_vec(
                                for_loop.iter_expr,
                                elem,
                                for_loop.loop_pat,
                                for_loop.body,
                                span,
                            );
                        }
                        TyKind::Ref { inner, .. } => cur = *inner,
                        _ => break None,
                    }
                };
                if let Some(len) = len_opt {
                    return self.lower_for_array(
                        for_loop.iter_expr,
                        for_loop.loop_pat,
                        for_loop.body,
                        len,
                        span,
                    );
                }
                // Default fallback: treat as a runtime Vec.
                // Element type defaults to i64, which is the
                // pointer width — every slot in a GosVec is
                // 8 bytes regardless of element shape, so
                // method calls on the binding still dispatch.
                let elem_ty = self.tcx.int_ty(gossamer_types::IntTy::I64);
                self.lower_for_vec(
                    for_loop.iter_expr,
                    elem_ty,
                    for_loop.loop_pat,
                    for_loop.body,
                    span,
                )
            }
        }
    }
}
