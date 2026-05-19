#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Typed counterpart to [`Self::compile_expr`]. Returns
    /// whatever kind the expression naturally produces,
    /// skipping the `BoxF64` / `BoxI64` round-trip when the
    /// result feeds into another typed consumer. Callers that
    /// need a `Value` register invoke [`Self::compile_expr`],
    /// which wraps this method and coerces via `as_value`.
    pub(crate) fn compile_expr_ex(&mut self, expr: &HirExpr) -> RuntimeResult<TypedReg> {
        match &expr.kind {
            // Numeric literals land in their typed reg file so
            // adjacent typed ops can consume them directly.
            HirExprKind::Literal(lit) => self.compile_literal_ex(lit, expr.ty),
            // Single-segment paths resolve to locals; the local
            // already carries its `TypedReg`, so we return it
            // as-is without boxing.
            HirExprKind::Path { segments, .. } if segments.len() == 1 => {
                if let Some(tr) = self.lookup_local(&segments[0].name) {
                    return Ok(tr);
                }
                let reg = self.compile_path(segments)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Binary { op, lhs, rhs } => self.compile_binary_ex(*op, lhs, rhs),
            HirExprKind::Unary { op, operand } => self.compile_unary_ex(*op, operand),
            HirExprKind::Call { callee, args } => {
                if let Some(tr) = self.try_intrinsic_call(callee, args)? {
                    return Ok(tr);
                }
                let reg = self.compile_call_ex(callee, args, expr.ty)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Field { receiver, name } => self.compile_field_ex(receiver, name, expr.ty),
            // Typed numeric cast. We classify the result and the
            // source by the existing `expr_kind` helper. The four
            // tractable combinations land directly in the right
            // typed register file:
            //
            //   i64 → f64              →  IntToFloatF64
            //   f64 → i64              →  FloatToIntI64
            //   i64 → narrow int       →  TruncCastI64 (wrapping semantics)
            //   i64 → i64/u64/isize    →  identity (same bit width)
            //   f64 → f64              →  identity
            //
            // Anything else (refs, custom From impls, trait dyn
            // casts) defers via the catch-all in `compile_expr`.
            HirExprKind::Cast {
                value,
                ty: target_ty,
            } => {
                let dst_kind = self.expr_kind(expr);
                let src_kind = self.expr_kind(value);
                match (dst_kind, src_kind) {
                    (RegKind::F64, RegKind::I64) => {
                        let src_tr = self.compile_expr_ex(value)?;
                        let src_i = self.as_i64(src_tr);
                        let dst_f = self.alloc_float();
                        self.emit(Op::IntToFloatF64 { dst_f, src_i });
                        Ok(TypedReg {
                            reg: dst_f,
                            kind: RegKind::F64,
                        })
                    }
                    (RegKind::I64, RegKind::F64) => {
                        let src_tr = self.compile_expr_ex(value)?;
                        let src_f = self.as_f64(src_tr);
                        let dst_i = self.alloc_int();
                        self.emit(Op::FloatToIntI64 { dst_i, src_f });
                        Ok(TypedReg {
                            reg: dst_i,
                            kind: RegKind::I64,
                        })
                    }
                    (RegKind::F64, RegKind::F64) => self.compile_expr_ex(value),
                    (RegKind::I64, RegKind::I64) => {
                        // Narrowing casts (e.g. `x as i32`, `x as u8`) must
                        // truncate + sign/zero-extend, not pass through the
                        // source value unchanged.
                        let target_kind = self.tcx.kind(*target_ty);
                        // u64/usize: produce Value::Uint for unsigned display.
                        if matches!(
                            target_kind,
                            Some(TyKind::Int(
                                gossamer_types::IntTy::U64 | gossamer_types::IntTy::Usize
                            ))
                        ) {
                            let src_tr = self.compile_expr_ex(value)?;
                            let src_i = self.as_i64(src_tr);
                            let dst_v = self.alloc_reg();
                            self.emit(Op::I64ToUint { dst_v, src_i });
                            return Ok(TypedReg {
                                reg: dst_v,
                                kind: RegKind::Value,
                            });
                        }
                        let (shift, signed) = match target_kind {
                            Some(TyKind::Int(gossamer_types::IntTy::I8)) => (56u8, true),
                            Some(TyKind::Int(gossamer_types::IntTy::I16)) => (48u8, true),
                            Some(TyKind::Int(gossamer_types::IntTy::I32)) => (32u8, true),
                            Some(TyKind::Int(gossamer_types::IntTy::U8)) => (56u8, false),
                            Some(TyKind::Int(gossamer_types::IntTy::U16)) => (48u8, false),
                            Some(TyKind::Int(gossamer_types::IntTy::U32)) => (32u8, false),
                            // i64/isize: same bit width — identity.
                            _ => {
                                return self.compile_expr_ex(value);
                            }
                        };
                        let src_tr = self.compile_expr_ex(value)?;
                        let src_i = self.as_i64(src_tr);
                        let dst_i = self.alloc_int();
                        self.emit(Op::TruncCastI64 {
                            dst_i,
                            src_i,
                            shift,
                            signed,
                        });
                        Ok(TypedReg {
                            reg: dst_i,
                            kind: RegKind::I64,
                        })
                    }
                    _ => {
                        let reg = self.compile_deferred(expr)?;
                        Ok(TypedReg {
                            reg,
                            kind: RegKind::Value,
                        })
                    }
                }
            }
            // Typed flat-i64 indexed read fast path. When the base
            // resolves to a local register marked as a
            // `Value::IntArray` (built via `try_build_int_array`)
            // and the parent expects an i64, we emit
            // `Op::IntArrayGetI64` which feeds the typed `i64`
            // register file directly — no `Value::Int` box/unbox.
            HirExprKind::Index { base, index }
                if matches!(self.tcx.kind(expr.ty), Some(TyKind::Int(_))) =>
            {
                let base_reg = self.compile_expr(base)?;
                if self.flat_int_locals.contains(&base_reg) {
                    let idx_tr = self.compile_expr_ex(index)?;
                    let idx_i = self.as_i64(idx_tr);
                    let dst_i = self.alloc_int();
                    self.emit(Op::IntArrayGetI64 {
                        dst_i,
                        base: base_reg,
                        index_i: idx_i,
                    });
                    return Ok(TypedReg {
                        reg: dst_i,
                        kind: RegKind::I64,
                    });
                }
                // Slow path: generic IndexGet → boxed Value reg.
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            // Typed flat-f64 indexed read fast path. Same shape as
            // the flat-i64 path above but for `Value::FloatVec` —
            // the inner-loop scratch arrays in nbody-style code
            // ride this branch.
            HirExprKind::Index { base, index }
                if matches!(self.tcx.kind(expr.ty), Some(TyKind::Float(FloatTy::F64))) =>
            {
                let base_reg = self.compile_expr(base)?;
                if self.flat_float_locals.contains(&base_reg) {
                    let idx_tr = self.compile_expr_ex(index)?;
                    let idx_i = self.as_i64(idx_tr);
                    let dst_f = self.alloc_float();
                    self.emit(Op::FloatVecGetF64 {
                        dst_f,
                        base: base_reg,
                        index_i: idx_i,
                    });
                    return Ok(TypedReg {
                        reg: dst_f,
                        kind: RegKind::F64,
                    });
                }
                // Slow path: generic IndexGet → boxed Value reg.
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                if let Some(tr) = self.try_build_float_array(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_int_array(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_float_vec(expr.ty, elems.as_slice())? {
                    return Ok(tr);
                }
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                if let Some(tr) = self.try_build_float_vec_repeat(expr.ty, value, count)? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_build_int_array_repeat(expr.ty, value, count)? {
                    return Ok(tr);
                }
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            // Everything else goes through the generic path,
            // which always yields a `Value` register.
            _ => {
                let reg = self.compile_expr(expr)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    pub(crate) fn compile_expr(&mut self, expr: &HirExpr) -> RuntimeResult<Reg> {
        match &expr.kind {
            HirExprKind::Literal(lit) => self.compile_literal(lit),
            HirExprKind::Path { segments, .. } => self.compile_path(segments),
            HirExprKind::Unary { op, operand } => self.compile_unary(*op, operand),
            HirExprKind::Binary { op, lhs, rhs } => self.compile_binary(*op, lhs, rhs),
            HirExprKind::Assign { place, value } => self.compile_assign(place, value),
            // Route through `_ex` so intrinsic-style calls
            // (`math::sqrt(x)`, etc.) get lowered to dedicated
            // opcodes when the arg kind is concrete f64, even
            // inside functions whose bodies are compiled via
            // the regular path (e.g. `fn fsqrt(x) { math::sqrt(x) }`).
            HirExprKind::Call { callee, args } => {
                let tr = {
                    let intr = self.try_intrinsic_call(callee, args)?;
                    if let Some(tr) = intr {
                        tr
                    } else {
                        let reg = self.compile_call_ex(callee, args, expr.ty)?;
                        TypedReg {
                            reg,
                            kind: RegKind::Value,
                        }
                    }
                };
                Ok(self.as_value(tr))
            }
            HirExprKind::If {
                condition,
                then_branch,
                else_branch,
            } => self.compile_if(condition, then_branch, else_branch.as_deref()),
            HirExprKind::While { condition, body } => self.compile_while(condition, body),
            // `Loop { body }` — native-compile only when the body
            // stays inside VM-handleable expression shapes.
            // Anything with an embedded Match (typically the
            // for-loop desugaring `loop { match iter.next() { ...
            // None => break } }`) defers the whole loop so the
            // walker can handle Break propagation correctly.
            HirExprKind::Loop { body } => {
                // Try the typed-i64 for-range fast path *before* the
                // generic-unsupported check. A `for i in 0..n { ... }`
                // desugar contains a `Match` (Some/None on `next()`),
                // which the unsupported check would otherwise route
                // to `compile_deferred` — sending every iteration of
                // every range loop through the tree-walker. The
                // fast-path emit handles the desugar shape directly,
                // so the walker fallback isn't needed.
                if let Some(reg) = self.try_compile_for_loop_range(body)? {
                    return Ok(reg);
                }
                if let Some(reg) = self.try_compile_for_loop_vec_iter(body)? {
                    return Ok(reg);
                }
                if body_contains_unsupported(body) {
                    self.compile_deferred(expr)
                } else {
                    self.compile_loop(body)
                }
            }
            HirExprKind::Block(block) => {
                let result = self.compile_block(block)?;
                Ok(match result {
                    BlockResult::Unit | BlockResult::Diverges => self.load_unit(),
                    BlockResult::ValueIn(reg) => reg,
                })
            }
            HirExprKind::Return(value) => self.compile_return(value.as_deref()),
            HirExprKind::Break(value) => self.compile_break(value.as_deref()),
            // Native `continue` — emit a forward jump that the
            // enclosing loop emitter patches once it knows the
            // address of its per-iteration step op. Routing through
            // a patch list (rather than jumping straight to
            // `loop_start`) lets the for-range / for-vec-iter fast
            // paths advance their typed counter on `continue`; a
            // direct jump-to-header bypasses the counter
            // increment that lives at the bottom of the body and
            // produces a livelock.
            HirExprKind::Continue => {
                if self.loop_stack.last().is_none() {
                    return Err(RuntimeError::Unsupported("continue outside of loop"));
                }
                let patch = self.emit(Op::Jump { target: 0 });
                self.loop_stack
                    .last_mut()
                    .expect("loop ctx")
                    .continue_patches
                    .push(patch);
                Ok(self.load_unit())
            }
            // Native method dispatch — avoids the `EvalDeferred`
            // env-rebuild cost that dominated tight loops
            // (fasta's inner `out.write_byte(…)` etc.).
            HirExprKind::MethodCall {
                receiver,
                name,
                args,
                ..
            } => self.compile_method_call(receiver, name, args),
            // Native indexed read.
            HirExprKind::Index { base, index } => {
                let base_reg = self.compile_expr(base)?;
                let idx_reg = self.compile_expr(index)?;
                let dst = self.alloc_reg();
                self.emit(Op::IndexGet {
                    dst,
                    base: base_reg,
                    index: idx_reg,
                });
                Ok(dst)
            }
            // Native struct-field read.
            HirExprKind::Field { receiver, name } => {
                let recv_reg = self.compile_expr(receiver)?;
                let name_idx = self.const_idx(
                    ConstKey::String(name.name.clone()),
                    Value::String(SmolStr::from(name.name.clone())),
                );
                let dst = self.alloc_reg();
                let cache_idx = self.alloc_field_cache_idx();
                self.emit(Op::FieldGet {
                    dst,
                    receiver: recv_reg,
                    name_idx,
                    cache_idx,
                });
                Ok(dst)
            }
            // Native tuple / positional-field read.
            HirExprKind::TupleIndex { receiver, index } => {
                let recv_reg = self.compile_expr(receiver)?;
                let dst = self.alloc_reg();
                self.emit(Op::TupleIndex {
                    dst,
                    receiver: recv_reg,
                    index: *index,
                });
                Ok(dst)
            }
            // Cast — delegate to the typed compile path so the
            // typed-numeric arms fire, then box back into a
            // Value reg for whoever asked for one.
            HirExprKind::Cast { .. } => {
                let tr = self.compile_expr_ex(expr)?;
                Ok(self.as_value(tr))
            }
            // Native tuple literal — `(a, b, c)` lands in
            // `count` consecutive value registers, then
            // `Op::BuildTuple` packs them. No walker re-entry.
            HirExprKind::Tuple(elems) => {
                let n = elems.len();
                if n == 0 {
                    // Empty tuple is unit-shaped; just emit
                    // `Value::Tuple(Arc::new(vec![]))` via
                    // BuildTuple with count 0 to keep semantics
                    // honest.
                    let dst = self.alloc_reg();
                    self.emit(Op::BuildTuple {
                        dst,
                        first: 0,
                        count: 0,
                    });
                    return Ok(dst);
                }
                // Allocate a contiguous block of value registers
                // up front, then compile each elem into its
                // pre-assigned slot via Move. Doing it this way
                // (rather than naively `compile_expr` per elem
                // and hoping they land contiguously) keeps the
                // BuildTuple op's first-reg invariant.
                let first = self.alloc_reg();
                for _ in 1..n {
                    let _ = self.alloc_reg();
                }
                for (i, elem) in elems.iter().enumerate() {
                    let r = self.compile_expr(elem)?;
                    let slot = first + i as u16;
                    if r != slot {
                        self.emit(Op::Move { dst: slot, src: r });
                    }
                }
                let dst = self.alloc_reg();
                let count = u16::try_from(n).map_err(|_| {
                    RuntimeError::Unsupported("tuple literal exceeds 65535 elements")
                })?;
                self.emit(Op::BuildTuple { dst, first, count });
                Ok(dst)
            }
            // Anything the VM's native lowering doesn't handle
            // yet — match, closures, `go expr`, `continue`,
            // and the rest — falls through to `Op::EvalDeferred`.
            // The VM hands the expression + captured local
            // environment to a bundled tree-walker which
            // returns a Value. Result: the VM never fails at
            // compile time; it just does slower work for these
            // nodes until a native opcode is wired.
            _ => self.compile_deferred(expr),
        }
    }

    /// Captures the current locally-visible bindings (name → reg)
    /// and emits an `Op::EvalDeferred` that hands `expr` plus
    /// those values off to the bundled tree-walker. The reg
    /// list is stored in `deferred_env_regs` so the VM can both
    /// pass the values in and sync mutations back out.
    pub(crate) fn compile_deferred(&mut self, expr: &HirExpr) -> RuntimeResult<Reg> {
        // GOS_VM_FALLBACK=1 surfaces every walker-fallback emit point
        // so users hunting an interp perf cliff can see which HIR
        // shapes are routing iterations through the slow path.
        // Stop-gap until every `EvalDeferred` site has a native
        // opcode (closures with captures, complex method receivers,
        // and a few other tails noted in the H2 audit).
        if std::env::var("GOS_VM_FALLBACK").is_ok() {
            eprintln!(
                "vm: deferred-walker fallback for HirExprKind::{:?}",
                std::mem::discriminant(&expr.kind),
            );
        }
        // Snapshot the visible locals (inner scopes shadow
        // outer ones — overwrite slot for already-seen names).
        let mut entries: Vec<(String, TypedReg)> = Vec::new();
        for scope in &self.scopes {
            for (name, tr) in &scope.locals {
                if let Some(i) = entries.iter().position(|(n, _)| n == name) {
                    entries[i].1 = *tr;
                } else {
                    entries.push((name.clone(), *tr));
                }
            }
        }
        // Typed locals must cross the walker boundary as boxed
        // `Value`s. Box before the call, remember the (typed,
        // value_reg) pair, and unbox back after the walker runs
        // so mutations inside the deferred block propagate.
        let mut names: Vec<String> = Vec::with_capacity(entries.len());
        let mut regs: Vec<Reg> = Vec::with_capacity(entries.len());
        let mut writebacks: Vec<(TypedReg, Reg)> = Vec::new();
        for (name, tr) in entries {
            let value_reg = match tr.kind {
                RegKind::Value => tr.reg,
                RegKind::F64 => {
                    let dst = self.alloc_reg();
                    self.emit(Op::BoxF64 {
                        dst_v: dst,
                        src_f: tr.reg,
                    });
                    writebacks.push((tr, dst));
                    dst
                }
                RegKind::I64 => {
                    let dst = self.alloc_reg();
                    self.emit(Op::BoxI64 {
                        dst_v: dst,
                        src_i: tr.reg,
                    });
                    writebacks.push((tr, dst));
                    dst
                }
            };
            names.push(name);
            regs.push(value_reg);
        }
        let expr_idx =
            u32::try_from(self.deferred_exprs.len()).expect("deferred expression index overflow");
        self.deferred_exprs.push(expr.clone());
        self.deferred_envs.push(names);
        self.deferred_env_regs.push(regs);
        let dst = self.alloc_reg();
        self.emit(Op::EvalDeferred { dst, expr_idx });
        for (tr, vr) in writebacks {
            match tr.kind {
                RegKind::F64 => {
                    self.emit(Op::UnboxF64 {
                        dst_f: tr.reg,
                        src_v: vr,
                    });
                }
                RegKind::I64 => {
                    self.emit(Op::UnboxI64 {
                        dst_i: tr.reg,
                        src_v: vr,
                    });
                }
                RegKind::Value => {}
            }
        }
        Ok(dst)
    }

    pub(crate) fn compile_literal(&mut self, lit: &HirLiteral) -> RuntimeResult<Reg> {
        let (key, value) = literal_const(lit);
        let idx = self.const_idx(key, value);
        let dst = self.alloc_reg();
        self.emit(Op::LoadConst { dst, idx });
        Ok(dst)
    }

    /// Typed binary-op compile. Emits `AddF64` / `LtI64` /
    /// etc. when both operands share a concrete numeric kind;
    /// otherwise falls back to the generic `binary_op` path
    /// (which operates on `Value` regs).
    pub(crate) fn compile_binary_ex(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<TypedReg> {
        if matches!(op, HirBinaryOp::And | HirBinaryOp::Or) {
            let reg = self.compile_short_circuit(op, lhs, rhs)?;
            return Ok(TypedReg {
                reg,
                kind: RegKind::Value,
            });
        }
        let lk = self.expr_kind(lhs);
        let rk = self.expr_kind(rhs);
        // Both operands f64 — emit a typed f64 op. For `+-*/`
        // the result is also f64; for comparisons it's a
        // `Bool` Value.
        if lk == RegKind::F64 && rk == RegKind::F64 {
            // Peephole fuse `a * b + c` / `c + a * b` /
            // `c - a * b` into `MulAddF64` / `MulSubF64`
            // before touching operand evaluation. Halves the
            // op count on any vector-math-style expression
            // tree (`x + dt * vx`, `vx - dx * mag`, ...).
            if let Some(tr) = self.try_compile_fma(op, lhs, rhs)? {
                return Ok(tr);
            }
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_f = self.as_f64(lhs_tr);
            let rhs_f = self.as_f64(rhs_tr);
            return self.emit_binary_f64(op, lhs_f, rhs_f);
        }
        if lk == RegKind::I64 && rk == RegKind::I64 {
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_i = self.as_i64(lhs_tr);
            let rhs_i = self.as_i64(rhs_tr);
            return self.emit_binary_i64(op, lhs_i, rhs_i);
        }
        // Fallback: generic path on Value regs.
        let lhs_reg = self.compile_expr(lhs)?;
        let rhs_reg = self.compile_expr(rhs)?;
        let dst = self.alloc_reg();
        let instr = self
            .binary_op(op, dst, lhs_reg, rhs_reg)
            .ok_or(RuntimeError::Unsupported("binary op kind"))?;
        self.emit(instr);
        Ok(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        })
    }

    pub(crate) fn compile_unary_ex(
        &mut self,
        op: HirUnaryOp,
        operand: &HirExpr,
    ) -> RuntimeResult<TypedReg> {
        let kind = self.expr_kind(operand);
        match (op, kind) {
            (HirUnaryOp::Neg, RegKind::F64) => {
                let tr = self.compile_expr_ex(operand)?;
                let src_f = self.as_f64(tr);
                let dst = self.alloc_float();
                self.emit(Op::NegF64 { dst_f: dst, src_f });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            (HirUnaryOp::Neg, RegKind::I64) => {
                let tr = self.compile_expr_ex(operand)?;
                let src_i = self.as_i64(tr);
                let dst = self.alloc_int();
                self.emit(Op::NegI64 { dst_i: dst, src_i });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                })
            }
            _ => {
                let reg = self.compile_unary(op, operand)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    pub(crate) fn compile_literal_ex(
        &mut self,
        lit: &HirLiteral,
        _ty: Ty,
    ) -> RuntimeResult<TypedReg> {
        match lit {
            HirLiteral::Float(text) => {
                let value = strip_float_suffix(text).parse::<f64>().unwrap_or(0.0);
                let idx = self.f64_const_idx(value);
                let dst = self.alloc_float();
                self.emit(Op::LoadConstF64 { dst_f: dst, idx });
                Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                })
            }
            HirLiteral::Int(text) => {
                if let Some(n) = parse_int(text) {
                    let idx = self.i64_const_idx(n);
                    let dst = self.alloc_int();
                    self.emit(Op::LoadConstI64 { dst_i: dst, idx });
                    return Ok(TypedReg {
                        reg: dst,
                        kind: RegKind::I64,
                    });
                }
                let reg = self.compile_literal(lit)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
            _ => {
                let reg = self.compile_literal(lit)?;
                Ok(TypedReg {
                    reg,
                    kind: RegKind::Value,
                })
            }
        }
    }

    /// Phase-2 field-read fast path. When the field's own
    /// type is `f64`, emit `IndexedFieldGetF64` /
    /// `FieldGetF64` so the scalar skips a `Value::Float`
    /// wrap and lands directly in the float register file —
    /// critical for nbody's inner loop, where every
    /// `bodies[i].x` read feeds straight into f64 math.
    pub(crate) fn compile_field_ex(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        field_ty: Ty,
    ) -> RuntimeResult<TypedReg> {
        let field_is_f64 = matches!(self.tcx.kind(field_ty), Some(TyKind::Float(FloatTy::F64)));
        // Try to resolve the receiver's struct field layout
        // for a compile-time offset. When present, emit an
        // offset-based op so the runtime skips the field-name
        // scan entirely.
        let elem_ty = match &receiver.kind {
            HirExprKind::Index { base, .. } => self.array_elem_ty(base.ty),
            _ => Some(self.unwrap_ref(receiver.ty)),
        };
        let offset = elem_ty.and_then(|t| self.resolve_struct_field_offset(t, name.name.as_str()));
        let name_idx = self.const_idx(
            ConstKey::String(name.name.clone()),
            Value::String(SmolStr::from(name.name.clone())),
        );
        // Fused `base[i].field` — avoids cloning the inner
        // struct `Arc`.
        if let HirExprKind::Index { base, index } = &receiver.kind {
            let base_reg = self.compile_expr(base)?;
            let idx_reg = self.compile_expr(index)?;
            if field_is_f64 {
                if let Some(offset) = offset {
                    // Known-flat local: emit the dedicated
                    // FloatArray-only read that skips the
                    // discriminant check.
                    if let Some(&stride) = self.flat_locals.get(&base_reg) {
                        let dst = self.alloc_float();
                        self.emit(Op::FlatGetF64 {
                            dst_f: dst,
                            base: base_reg,
                            index: idx_reg,
                            stride,
                            offset,
                        });
                        return Ok(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        });
                    }
                    let dst = self.alloc_float();
                    self.emit(Op::IndexedFieldGetF64ByOffset {
                        dst_f: dst,
                        base: base_reg,
                        index: idx_reg,
                        offset,
                    });
                    return Ok(TypedReg {
                        reg: dst,
                        kind: RegKind::F64,
                    });
                }
                let dst = self.alloc_float();
                self.emit(Op::IndexedFieldGetF64 {
                    dst_f: dst,
                    base: base_reg,
                    index: idx_reg,
                    name_idx,
                });
                return Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                });
            }
            let dst = self.alloc_reg();
            self.emit(Op::IndexedFieldGet {
                dst,
                base: base_reg,
                index: idx_reg,
                name_idx,
            });
            return Ok(TypedReg {
                reg: dst,
                kind: RegKind::Value,
            });
        }
        // Plain `value.field` — the receiver itself is a
        // single value, so we already avoid the indexed
        // clone. The remaining win is unboxing the scalar
        // into a float reg.
        let recv_reg = self.compile_expr(receiver)?;
        if field_is_f64 {
            if let Some(offset) = offset {
                let dst = self.alloc_float();
                self.emit(Op::FieldGetF64ByOffset {
                    dst_f: dst,
                    receiver: recv_reg,
                    offset,
                });
                return Ok(TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                });
            }
            let dst = self.alloc_float();
            self.emit(Op::FieldGetF64 {
                dst_f: dst,
                receiver: recv_reg,
                name_idx,
            });
            return Ok(TypedReg {
                reg: dst,
                kind: RegKind::F64,
            });
        }
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_field_cache_idx();
        self.emit(Op::FieldGet {
            dst,
            receiver: recv_reg,
            name_idx,
            cache_idx,
        });
        Ok(TypedReg {
            reg: dst,
            kind: RegKind::Value,
        })
    }

    pub(crate) fn compile_short_circuit(
        &mut self,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<Reg> {
        let result = self.alloc_reg();
        let lhs_reg = self.compile_expr(lhs)?;
        self.emit(Op::Move {
            dst: result,
            src: lhs_reg,
        });
        let branch_idx = match op {
            HirBinaryOp::And => self.emit(Op::BranchIfNot {
                cond: result,
                target: 0,
            }),
            HirBinaryOp::Or => self.emit(Op::BranchIf {
                cond: result,
                target: 0,
            }),
            _ => unreachable!(),
        };
        let rhs_reg = self.compile_expr(rhs)?;
        self.emit(Op::Move {
            dst: result,
            src: rhs_reg,
        });
        let after = self.cur_idx();
        self.patch_jump(branch_idx, after);
        Ok(result)
    }

    pub(crate) fn compile_method_call(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
    ) -> RuntimeResult<Reg> {
        // Super-instruction fast path for the canonical
        // `m.insert(k, m.get_or(k, 0) + by)` counter-bump.
        // Detected here (before compiling args) so the inner
        // `get_or` call is never lowered.
        if name.name == "insert" && args.len() == 2 {
            if let Some((key_expr, by_expr)) = match_map_inc_pattern(receiver, &args[0], &args[1]) {
                if matches!(self.tcx.kind(receiver.ty), Some(TyKind::HashMap { .. })) {
                    // Typed `HashMap<i64, i64>` route: use
                    // `Op::IntMapInc` so the key + delta stay in
                    // the i64 register file the whole time.
                    if self.is_int_map_ty(receiver.ty) {
                        let map_reg = self.compile_expr(receiver)?;
                        let key_tr = self.compile_expr_ex(key_expr)?;
                        let key_i = self.as_i64(key_tr);
                        let by_tr = self.compile_expr_ex(by_expr)?;
                        let by_i = self.as_i64(by_tr);
                        let dst_i = self.alloc_int();
                        self.emit(Op::IntMapInc {
                            dst_i,
                            map_reg,
                            key_i,
                            by_i,
                        });
                        // Caller wants a `Value` register; box the
                        // post-increment value back so the existing
                        // statement-context code keeps working.
                        let dst = self.alloc_reg();
                        self.emit(Op::BoxI64 {
                            dst_v: dst,
                            src_i: dst_i,
                        });
                        return Ok(dst);
                    }
                    let map_reg = self.compile_expr(receiver)?;
                    let key_reg = self.compile_expr(key_expr)?;
                    let by_reg = self.compile_expr(by_expr)?;
                    let dst = self.alloc_reg();
                    self.emit(Op::MapInc {
                        dst,
                        map_reg,
                        key_reg,
                        by_reg,
                    });
                    return Ok(dst);
                }
            }
        }
        // `m.inc_at(seq, start, len, by)` super-instruction for a
        // string-keyed integer-valued `HashMap`. Inlines the
        // slice-hash + entry-increment so a sliding-window
        // counter update doesn't pay the generic builtin-call
        // dispatch on each iteration.
        if name.name == "inc_at"
            && args.len() == 4
            && matches!(self.tcx.kind(receiver.ty), Some(TyKind::HashMap { .. }))
        {
            let map_reg = self.compile_expr(receiver)?;
            let seq_reg = self.compile_expr(&args[0])?;
            let start_reg = self.compile_expr(&args[1])?;
            let len_reg = self.compile_expr(&args[2])?;
            let by_reg = self.compile_expr(&args[3])?;
            let dst = self.alloc_reg();
            let wide_idx = u16::try_from(self.wide_ops.len()).expect("wide_ops index overflow");
            self.wide_ops.push(crate::bytecode::WideOp::MapIncAt {
                dst,
                map_reg,
                seq_reg,
                start_reg,
                len_reg,
                by_reg,
            });
            self.emit(Op::Wide { idx: wide_idx });
            return Ok(dst);
        }
        // `arr.swap(i, j)` super-instruction. The generic
        // `MethodCall` dispatch routes through the `swap` builtin
        // which returns a fresh aggregate, then the writeback `Op::Move`
        // copies it back into the receiver's slot. That works under
        // pure bytecode but the cranelift JIT lowers MIR for the
        // function body and has no intrinsic for the writeback —
        // the JIT silently drops the mutation, leaving callers
        // looping on stale data. Inlining the swap as
        // `t = recv[i]; recv[i] = recv[j]; recv[j] = t` keeps the
        // semantics identical between bytecode and JIT and turns a
        // value-clone-per-swap into two index reads + two index
        // writes (in place).
        if name.name == "swap" && args.len() == 2 {
            let recv_reg = self.compile_expr(receiver)?;
            // Typed-storage fast paths: when the receiver is a
            // tracked `flat_int_locals` / `flat_float_locals` we
            // emit the fused `IntArraySwap` / `FloatVecSwap` op
            // (one dispatch + one `Vec::swap` in place) instead of
            // the four-op generic IndexGet/IndexSet dance. fannkuch
            // hits this on `[i64; 16]`.
            if self.flat_int_locals.contains(&recv_reg) {
                let i_tr = self.compile_expr_ex(&args[0])?;
                let i_i = self.as_i64(i_tr);
                let j_tr = self.compile_expr_ex(&args[1])?;
                let j_i = self.as_i64(j_tr);
                self.emit(Op::IntArraySwap {
                    base: recv_reg,
                    i_i,
                    j_i,
                });
                let dst = self.alloc_reg();
                let unit_idx = self.const_idx(ConstKey::Unit, Value::Unit);
                self.emit(Op::LoadConst { dst, idx: unit_idx });
                return Ok(dst);
            }
            if self.flat_float_locals.contains(&recv_reg) {
                let i_tr = self.compile_expr_ex(&args[0])?;
                let i_i = self.as_i64(i_tr);
                let j_tr = self.compile_expr_ex(&args[1])?;
                let j_i = self.as_i64(j_tr);
                self.emit(Op::FloatVecSwap {
                    base: recv_reg,
                    i_i,
                    j_i,
                });
                let dst = self.alloc_reg();
                let unit_idx = self.const_idx(ConstKey::Unit, Value::Unit);
                self.emit(Op::LoadConst { dst, idx: unit_idx });
                return Ok(dst);
            }
            let i_reg = self.compile_expr(&args[0])?;
            let j_reg = self.compile_expr(&args[1])?;
            let temp_i = self.alloc_reg();
            self.emit(Op::IndexGet {
                dst: temp_i,
                base: recv_reg,
                index: i_reg,
            });
            let temp_j = self.alloc_reg();
            self.emit(Op::IndexGet {
                dst: temp_j,
                base: recv_reg,
                index: j_reg,
            });
            self.emit(Op::IndexSet {
                base: recv_reg,
                index: i_reg,
                value: temp_j,
            });
            self.emit(Op::IndexSet {
                base: recv_reg,
                index: j_reg,
                value: temp_i,
            });
            let dst = self.alloc_reg();
            let unit_idx = self.const_idx(ConstKey::Unit, Value::Unit);
            self.emit(Op::LoadConst { dst, idx: unit_idx });
            return Ok(dst);
        }
        // Typed-IntMap method dispatch fast paths. Skip the
        // generic builtin-IC route for the handful of HashMap
        // methods that hot counter loops drive.
        if self.is_int_map_ty(receiver.ty) {
            if let Some(reg) = self.try_compile_int_map_method(receiver, &name.name, args)? {
                return Ok(reg);
            }
        }
        let receiver_reg = self.compile_expr(receiver)?;
        // Super-instruction fast path for `<stream>.write_byte(<b>)`.
        // The runtime handler in `vm.rs::Op::StreamWriteByte`
        // verifies the receiver is a Stream and the byte is an
        // integer; if not, it falls through to a normal MethodCall
        // dispatch. Skipping the args-buf + IC + builtin-extract
        // chain saves the dominant per-character overhead in
        // fasta's hot output loop. Mirrors CPython 3.11's
        // `CALL_NO_KW_BUILTIN_O` specialisation.
        if name.name == "write_byte" && args.len() == 1 {
            // Use the typed compile path so a typed-i64 result (from
            // e.g. `Op::IntArrayGetI64`) can flow through an
            // explicit `BoxI64` rather than being re-fetched as a
            // boxed `Value::Int`. The handler still expects a
            // `Value` register, but `BoxI64` is a single op.
            let byte_tr = self.compile_expr_ex(&args[0])?;
            let byte_reg = self.as_value(byte_tr);
            let dst = self.alloc_reg();
            self.emit(Op::StreamWriteByte {
                dst,
                stream_reg: receiver_reg,
                byte_reg,
            });
            return Ok(dst);
        }
        // Mirror super-instruction for `<u8vec>.set_byte(<idx>, <byte>)`.
        // fasta's per-byte buffer fill drives this op millions of
        // times per phase; the inline handler skips the
        // MethodCall + IC + `&[Value]` round-trip.
        if name.name == "set_byte" && args.len() == 2 {
            let idx_tr = self.compile_expr_ex(&args[0])?;
            let idx_reg = self.as_value(idx_tr);
            let byte_tr = self.compile_expr_ex(&args[1])?;
            let byte_reg = self.as_value(byte_tr);
            let dst = self.alloc_reg();
            self.emit(Op::U8VecSetByte {
                dst,
                u8vec_reg: receiver_reg,
                idx_reg,
                byte_reg,
            });
            return Ok(dst);
        }
        // Mirror super-instruction for `<u8vec>.get_byte(<idx>) -> i64`.
        // The handler writes into a typed `i64` register, so a
        // downstream `Op::Add` etc. picks the result up without an
        // intermediate `Value::Int` round-trip. Caller still
        // expects a `Value` register, so we box back through
        // `Op::BoxI64` — the register allocator and downstream
        // typed-arith specialisation usually elide that pair.
        if name.name == "get_byte" && args.len() == 1 {
            let idx_tr = self.compile_expr_ex(&args[0])?;
            let idx_reg = self.as_value(idx_tr);
            let dst_i = self.alloc_int();
            self.emit(Op::U8VecGetByte {
                dst_i,
                u8vec_reg: receiver_reg,
                idx_reg,
            });
            let dst = self.alloc_reg();
            self.emit(Op::BoxI64 {
                dst_v: dst,
                src_i: dst_i,
            });
            return Ok(dst);
        }
        let arg_regs: Vec<Reg> = args
            .iter()
            .map(|a| self.compile_expr(a))
            .collect::<RuntimeResult<Vec<_>>>()?;
        let args_start = self.next_reg;
        for (i, r) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i).expect("argc overflow"))
                .expect("reg overflow");
            self.ensure_reg_slot(slot);
            self.emit(Op::Move { dst: slot, src: *r });
        }
        let argc = u16::try_from(args.len()).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: args.len(),
        })?;
        let name_idx = self.global_idx(&name.name);
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        self.emit(Op::MethodCall {
            dst,
            receiver: receiver_reg,
            name_idx,
            args: args_start,
            argc,
            cache_idx,
        });
        // Mutating-method writeback. The interp builtins for
        // `push` / `insert` / etc. return the *new* aggregate
        // rather than mutating in place, so the VM has to thread
        // the result back into the receiver's storage. The tree-
        // walker handles this via `maybe_writeback`; the VM has
        // no equivalent dispatcher, so we splice the move here
        // when the receiver is a bindable local. Field / Index
        // receivers fall through with no writeback today.
        if Self::is_mutating_method_name(name.name.as_str()) {
            if let HirExprKind::Path { segments, .. } = &receiver.kind {
                if segments.len() == 1 {
                    if let Some(target) = self.lookup_local(&segments[0].name) {
                        if target.kind == RegKind::Value && target.reg == receiver_reg {
                            self.emit(Op::Move {
                                dst: target.reg,
                                src: dst,
                            });
                        }
                    }
                }
            }
        }
        Ok(dst)
    }

    /// Extended call compiler that takes the call's **result** type.
    /// Used by callers that have it on hand (for example
    /// `HirExprKind::Call`'s `expr.ty`) so the typed
    /// `HashMap<i64, i64>` construction can route to
    /// `Op::BuildIntMap` instead of the generic `builtin_map_new`
    /// path.
    pub(crate) fn compile_call_ex(
        &mut self,
        callee: &HirExpr,
        args: &[HirExpr],
        result_ty: Ty,
    ) -> RuntimeResult<Reg> {
        // Typed-IntMap construction fast path: when the callee is
        // `HashMap::new` and the result type is `HashMap<i64, i64>`,
        // emit a dedicated `Op::BuildIntMap` so the receiver lands
        // as `Value::IntMap` and downstream typed ops fire.
        if args.is_empty() {
            if let HirExprKind::Path { segments, .. } = &callee.kind {
                let segs: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
                if matches!(segs.as_slice(), ["HashMap", "new"]) && self.is_int_map_ty(result_ty) {
                    let dst = self.alloc_reg();
                    self.emit(Op::BuildIntMap { dst_v: dst });
                    return Ok(dst);
                }
            }
        }
        let callee_reg = self.compile_expr(callee)?;
        let argc = u16::try_from(args.len()).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: args.len(),
        })?;
        // Reserve `argc` contiguous Value-register slots for the call's
        // argument vector before compiling any arg expression. Without
        // this, an arg whose `compile_expr` allocates a fresh register
        // (e.g. a literal or call result) lands inside the not-yet-
        // populated args region, and the subsequent `Move dst=slot
        // src=arg_reg` clobbers earlier args before they reach the
        // callee.
        let args_start = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(argc)
            .expect("register overflow reserving call args");
        let arg_regs: Vec<Reg> = args
            .iter()
            .map(|arg| self.compile_expr(arg))
            .collect::<RuntimeResult<Vec<_>>>()?;
        for (i, arg_reg) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i).unwrap())
                .expect("register overflow");
            self.emit(Op::Move {
                dst: slot,
                src: *arg_reg,
            });
        }
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        self.emit(Op::Call {
            dst,
            callee: callee_reg,
            args: args_start,
            argc,
            cache_idx,
        });
        Ok(dst)
    }
}
