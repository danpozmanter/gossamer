#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

/// Peels any `&expr` / `&mut expr` borrow wrappers off an expression,
/// returning the underlying place. The for-loop desugar emits
/// `(&mut __for_iter).next()`, so the `&mut self` writeback target is
/// the value behind the borrow, not the borrow itself.
fn peel_ref_wrappers_expr(expr: &HirExpr) -> &HirExpr {
    let mut cur = expr;
    while let HirExprKind::Unary {
        op: HirUnaryOp::RefShared | HirUnaryOp::RefMut,
        operand,
    } = &cur.kind
    {
        cur = operand;
    }
    cur
}

/// Collects, in source order, the distinct names a pattern binds.
/// The or-pattern lowering uses this to allocate one shared register
/// per name so every alternative writes the same destinations. For
/// nested or-patterns only the first alternative is walked, since all
/// alternatives bind the same set of names by typecheck invariant.
pub(crate) fn collect_pattern_binding_names(pat: &HirPat, out: &mut Vec<String>) {
    fn push_unique(out: &mut Vec<String>, name: &str) {
        if !out.iter().any(|n| n == name) {
            out.push(name.to_string());
        }
    }
    match &pat.kind {
        HirPatKind::Binding { name, .. } => push_unique(out, &name.name),
        HirPatKind::At { name, sub, .. } => {
            push_unique(out, &name.name);
            collect_pattern_binding_names(sub, out);
        }
        HirPatKind::Tuple(ps) | HirPatKind::Variant { fields: ps, .. } => {
            for p in ps {
                collect_pattern_binding_names(p, out);
            }
        }
        HirPatKind::Slice {
            prefix,
            rest,
            suffix,
        } => {
            for p in prefix {
                collect_pattern_binding_names(p, out);
            }
            if let Some(rest) = rest {
                collect_pattern_binding_names(rest, out);
            }
            for p in suffix {
                collect_pattern_binding_names(p, out);
            }
        }
        HirPatKind::Struct { fields, .. } => {
            for f in fields {
                match &f.pattern {
                    Some(p) => collect_pattern_binding_names(p, out),
                    None => push_unique(out, &f.name.name),
                }
            }
        }
        HirPatKind::Ref { inner, .. } => collect_pattern_binding_names(inner, out),
        HirPatKind::Or(alts) => {
            if let Some(first) = alts.first() {
                collect_pattern_binding_names(first, out);
            }
        }
        HirPatKind::Wildcard
        | HirPatKind::Rest
        | HirPatKind::Literal(_)
        | HirPatKind::Range { .. } => {}
    }
}

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
                if let Some(tr) = self.try_build_empty_typed_vec(callee, args, expr.ty)? {
                    return Ok(tr);
                }
                if let Some(tr) = self.try_inline_user_call(callee, args)? {
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
                            // i64/isize: same bit width - identity.
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
                        // Remaining whitelisted combos - f32 / bool /
                        // char sources, `char` / `f32` targets - lower
                        // to the generic scalar-cast op so every
                        // GT0005-whitelisted cast is handled natively.
                        let target = self
                            .tcx
                            .kind(*target_ty)
                            .and_then(crate::cast::CastTarget::of)
                            // `as` only typechecks (passes the GT0005
                            // whitelist) for scalar targets, so a resolved
                            // cast always maps to a `CastTarget`. Reaching
                            // here means the target type never resolved - a
                            // frontend invariant violation, surfaced as a
                            // compile error.
                            .ok_or(RuntimeError::Unsupported(
                                "cast target type did not resolve to a scalar",
                            ))?;
                        let src_tr = self.compile_expr_ex(value)?;
                        let src = self.as_value(src_tr);
                        let dst = self.alloc_reg();
                        self.emit(Op::CastScalar { dst, src, target });
                        Ok(TypedReg {
                            reg: dst,
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
            // register file directly - no `Value::Int` box/unbox.
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
            // the flat-i64 path above but for `Value::FloatVec` -
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
                let reg = self.compile_array_list(elems)?;
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
                let reg = self.compile_array_repeat(value, count)?;
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
                    } else if let Some(tr) = self.try_inline_user_call(callee, args)? {
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
            HirExprKind::While {
                condition,
                body,
                label,
            } => {
                self.pending_loop_label.clone_from(label);
                self.compile_while(condition, body)
            }
            // `Loop { body }` - native register-VM lowering. The typed
            // for-loop fast paths (`for i in a..b`, `for x in xs.iter()`,
            // `for (k, v) in map.iter()`) are tried first as an
            // allocation-free index walk; otherwise the generic loop
            // emitter runs. A `for x in <custom iterator>` desugar
            // (`loop { match (&mut __for_iter).next() { … } }`) compiles
            // through the generic emitter: its `next()` call rides the
            // `&mut self` write-back path in `compile_method_call`, so
            // the iterator's state advances natively each iteration.
            HirExprKind::Loop { body, label } => {
                // Re-arm the pending label at each fast-path attempt: a
                // fast path that fires takes it at its own `LoopCtx`
                // push; one that bails leaves the next attempt to set
                // it again.
                self.pending_loop_label.clone_from(label);
                if let Some(reg) = self.try_compile_for_loop_range(body)? {
                    return Ok(reg);
                }
                self.pending_loop_label.clone_from(label);
                if let Some(reg) = self.try_compile_for_loop_vec_iter(body)? {
                    return Ok(reg);
                }
                self.pending_loop_label.clone_from(label);
                self.compile_loop(body)
            }
            HirExprKind::Block(block) => {
                let result = self.compile_block(block)?;
                Ok(match result {
                    BlockResult::Unit | BlockResult::Diverges => self.load_unit(),
                    BlockResult::ValueIn(reg) => reg,
                })
            }
            HirExprKind::Return(value) => self.compile_return(value.as_deref()),
            HirExprKind::Break { value, label } => {
                self.compile_break(value.as_deref(), label.as_deref())
            }
            // Native `continue` - emit a forward jump that the
            // enclosing loop emitter patches once it knows the
            // address of its per-iteration step op. Routing through
            // a patch list (rather than jumping straight to
            // `loop_start`) lets the for-range / for-vec-iter fast
            // paths advance their typed counter on `continue`; a
            // direct jump-to-header bypasses the counter
            // increment that lives at the bottom of the body and
            // produces a livelock.
            HirExprKind::Continue { label } => {
                let idx = self
                    .resolve_loop_target(label.as_deref())
                    .ok_or(RuntimeError::Unsupported("continue outside of loop"))?;
                let defer_depth = self.loop_stack[idx].defer_depth;
                // Run the defers of the blocks nested inside the loop body
                // before jumping to the next iteration; the loop's own
                // enclosing frames stay pending.
                self.emit_defers_above(defer_depth)?;
                let patch = self.emit(Op::Jump { target: 0 });
                self.loop_stack[idx].continue_patches.push(patch);
                Ok(self.load_unit())
            }
            // Native method dispatch - emits an `Op::MethodCall`
            // for the most common hot-path shape
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
                // An aggregate element (struct / tuple / array) cannot yield a
                // lenient zero value on an out-of-range index without feeding a
                // bogus value into a later field/element access, so it panics
                // with `index out of bounds` - matching the compiled tiers'
                // bounds assert. Primitive elements keep lenient `IndexGet`.
                let aggregate = matches!(
                    self.tcx.kind(expr.ty),
                    Some(TyKind::Tuple(_) | TyKind::Adt { .. } | TyKind::Array { .. })
                );
                self.emit(if aggregate {
                    Op::IndexGetChecked {
                        dst,
                        base: base_reg,
                        index: idx_reg,
                    }
                } else {
                    Op::IndexGet {
                        dst,
                        base: base_reg,
                        index: idx_reg,
                    }
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
            // Cast - delegate to the typed compile path so the
            // typed-numeric arms fire, then box back into a
            // Value reg for whoever asked for one.
            HirExprKind::Cast { .. } => {
                let tr = self.compile_expr_ex(expr)?;
                Ok(self.as_value(tr))
            }
            // Native tuple literal - `(a, b, c)` lands in
            // `count` consecutive value registers, then
            // `Op::BuildTuple` packs them.
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
            // Native `match` - test-and-branch chain per arm, including
            // or-patterns that bind (shared binding registers).
            HirExprKind::Match { scrutinee, arms } => self.compile_match(scrutinee, arms, expr),
            // Native closure: compile the body to its own `FnChunk`
            // with the captured upvalues as leading parameters and
            // emit `Op::MakeClosure`.
            HirExprKind::Closure { params, body, .. } => self.compile_closure(params, body),
            // Native `select`: evaluate each arm's channel/value into
            // registers, dispatch via `Op::Select`, and run the winning
            // arm's body block.
            HirExprKind::Select { arms } => self.compile_select(arms),
            // Native `go` in expression position. Call shapes
            // (`go f(args)` / `go obj.method(args)`) lower to
            // `Op::Spawn` / `Op::SpawnMethod`; non-call shapes lift
            // the spawned expression into a zero-arg closure and spawn
            // that. The expression yields `()`.
            HirExprKind::Go(inner) => {
                if !self.try_compile_go_native(inner)? {
                    self.compile_non_call_go(inner)?;
                }
                Ok(self.load_unit())
            }
            // Native array literal. In a `Value` context assemble a
            // generic `Value::Array` (or `[v; n]` repeat) directly. The
            // typed-storage specialisations (`Value::IntArray` /
            // `Value::FloatVec`) are reserved for the typed `_ex` entry,
            // where a known flat-storage consumer asks for them; a plain
            // `Value` consumer (a call argument, a struct field) gets the
            // uniform `Value::Array` the runtime builtins expect.
            HirExprKind::Array(gossamer_hir::HirArrayExpr::List(elems)) => {
                self.compile_array_list(elems)
            }
            HirExprKind::Array(gossamer_hir::HirArrayExpr::Repeat { value, count }) => {
                self.compile_array_repeat(value, count)
            }
            // Native standalone range value (`a..b` / `a..=b`): an eager
            // `Value::Array` of `Value::Int`. For-range loops never reach
            // here - they ride the desugar fast paths above.
            HirExprKind::Range {
                start,
                end,
                inclusive,
            } => self.compile_range_value(start.as_deref(), end.as_deref(), *inclusive),
            // `Placeholder` is the resolver's sentinel for forms it could
            // not rewrite; a well-typed program never carries one to
            // lowering. `LiftedClosure` exists only on the MIR-bound
            // build path (the `lift_closures` pass), which `gos run`
            // never runs. Either reaching here is a frontend invariant
            // violation, surfaced as a compile error.
            HirExprKind::Placeholder => Err(RuntimeError::Unsupported(
                "placeholder expression reached bytecode lowering",
            )),
            HirExprKind::LiftedClosure { .. } => Err(RuntimeError::Unsupported(
                "lifted closure reached the bytecode VM (lift runs only on the MIR path)",
            )),
        }
    }

    /// Lowers a generic array literal `[a, b, c]` to `Op::BuildArray`.
    /// Each element compiles into a pre-reserved contiguous value-register
    /// slot, then the op `Arc`-wraps them into a `Value::Array`. The
    /// typed-storage specialisations (`Value::IntArray` / `Value::FloatVec`)
    /// are tried first by the callers; this is the fallback for every
    /// other element type.
    pub(crate) fn compile_array_list(&mut self, elems: &[HirExpr]) -> RuntimeResult<Reg> {
        let n = elems.len();
        if n == 0 {
            let dst = self.alloc_reg();
            self.emit(Op::BuildArray {
                dst,
                first: 0,
                count: 0,
            });
            return Ok(dst);
        }
        // Reserve a contiguous block of value registers before compiling
        // any element, so an element whose compile allocates fresh
        // registers can't land inside the not-yet-populated span and
        // clobber an earlier element. Mirrors the `BuildTuple` lowering.
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
        let count = u16::try_from(n)
            .map_err(|_| RuntimeError::Unsupported("array literal exceeds 65535 elements"))?;
        self.emit(Op::BuildArray { dst, first, count });
        Ok(dst)
    }

    /// Lowers a generic `[value; count]` repeat to `Op::BuildArrayRepeat`,
    /// which clones `value` `count` times into a `Value::Array`. The count
    /// is read at runtime (`Value::Int`).
    pub(crate) fn compile_array_repeat(
        &mut self,
        value: &HirExpr,
        count: &HirExpr,
    ) -> RuntimeResult<Reg> {
        let value_reg = self.compile_expr(value)?;
        let count_reg = self.compile_expr(count)?;
        let dst = self.alloc_reg();
        self.emit(Op::BuildArrayRepeat {
            dst,
            value: value_reg,
            count: count_reg,
        });
        Ok(dst)
    }

    /// Lowers a standalone range value (`a..b` / `a..=b`) to
    /// `Op::BuildRange`. Open bounds default to `0`; the op materialises
    /// an eager `Value::Array` of `Value::Int`.
    pub(crate) fn compile_range_value(
        &mut self,
        start: Option<&HirExpr>,
        end: Option<&HirExpr>,
        inclusive: bool,
    ) -> RuntimeResult<Reg> {
        let start_reg = match start {
            Some(e) => self.compile_expr(e)?,
            None => {
                let idx = self.const_idx(ConstKey::Int(0), Value::Int(0));
                let r = self.alloc_reg();
                self.emit(Op::LoadConst { dst: r, idx });
                r
            }
        };
        // A missing upper bound mirrors `eval_range`'s degenerate empty
        // range (`end_val == start_val`), so reuse the start register.
        let end_reg = match end {
            Some(e) => self.compile_expr(e)?,
            None => start_reg,
        };
        let dst = self.alloc_reg();
        self.emit(Op::BuildRange {
            dst,
            start: start_reg,
            end: end_reg,
            inclusive,
        });
        Ok(dst)
    }

    /// Native `match` compilation. Emits the scrutinee once, then a
    /// test-and-branch chain per arm: each arm's pattern lowers to a
    /// sequence of shape tests (`VariantIs` / `StructIs` / literal
    /// `Eq` / range compares) that branch to the next arm on failure
    /// and extract sub-values into freshly-bound registers on
    /// success. Every pattern shape - including or-patterns that bind -
    /// lowers natively via [`Self::emit_pattern_test`].
    pub(crate) fn compile_match(
        &mut self,
        scrutinee: &HirExpr,
        arms: &[gossamer_hir::HirMatchArm],
        _whole: &HirExpr,
    ) -> RuntimeResult<Reg> {
        let scrut = self.compile_expr(scrutinee)?;
        let result = self.alloc_reg();
        let mut end_jumps: Vec<InstrIdx> = Vec::new();
        let scrut_ty = scrutinee.ty;
        for arm in arms {
            self.push_scope();
            let mut fails: Vec<InstrIdx> = Vec::new();
            self.emit_pattern_test(scrut, &arm.pattern, &mut fails)?;
            // Tag collection-typed pattern bindings (`Some(arr)`, …) so a
            // `for x in <binding>` in the arm body iterates by index. The
            // binding's own `Path` carries an unresolved var when inferred,
            // so the type comes from the resolved scrutinee type instead.
            let mut coll_names: Vec<String> = Vec::new();
            self.collect_collection_binding_names(&arm.pattern, Some(scrut_ty), &mut coll_names);
            for name in coll_names {
                if let Some(tr) = self.lookup_local(&name) {
                    self.collection_locals.insert(tr.reg);
                }
            }
            if let Some(guard) = &arm.guard {
                let g = self.compile_expr(guard)?;
                fails.push(self.emit(Op::BranchIfNot { cond: g, target: 0 }));
            }
            let body_reg = self.compile_expr(&arm.body)?;
            self.emit(Op::Move {
                dst: result,
                src: body_reg,
            });
            end_jumps.push(self.emit(Op::Jump { target: 0 }));
            self.pop_scope();
            let next = self.cur_idx();
            for f in fails {
                self.patch_jump(f, next);
            }
        }
        // No arm matched. Exhaustiveness covers well-typed programs, but a
        // checker blind spot (e.g. an unenumerable integer payload like
        // `Some(2)` against `Some(0) | Some(1) | None`) can reach here.
        // Panic cleanly with the same message the compiled tiers emit
        // rather than falling through with a zero/default value.
        let msg = self.const_idx(
            ConstKey::String(NON_EXHAUSTIVE_MATCH_MESSAGE.to_string()),
            Value::String(SmolStr::from(NON_EXHAUSTIVE_MATCH_MESSAGE)),
        );
        self.emit(Op::Panic { msg });
        let end = self.cur_idx();
        for j in end_jumps {
            self.patch_jump(j, end);
        }
        Ok(result)
    }

    /// Native `select { … }` compilation. Evaluates each arm's
    /// channel (and value, for sends) into registers up front, emits a
    /// single [`Op::Select`] referencing a contiguous range of
    /// [`crate::bytecode::SelectArmMeta`] entries, then compiles every
    /// arm body as a basic block the op jumps to. Recv arms destructure
    /// the received value (written into the arm's `bind_reg` by the
    /// handler) at the top of their block. The handler's poll/park loop
    /// operates over `Value::Channel`.
    pub(crate) fn compile_select(
        &mut self,
        arms: &[gossamer_hir::HirSelectArm],
    ) -> RuntimeResult<Reg> {
        use crate::bytecode::{SelectArmKind, SelectArmMeta};
        use gossamer_hir::HirSelectOp;

        // An empty `select {}` blocks forever in Go; degrade it to Unit
        // rather than emitting an arm-less op that would spin in the
        // park loop.
        if arms.is_empty() {
            return Ok(self.load_unit());
        }

        let result = self.alloc_reg();
        let first = u32::try_from(self.select_arms.len())
            .map_err(|_| RuntimeError::Unsupported("too many select arms in one function"))?;

        // Pass 1: evaluate operand expressions (channels / send values)
        // and pre-allocate recv binding registers. The `body_block`
        // index is filled in pass 2 once each body is laid down.
        for arm in arms {
            let meta = match &arm.op {
                HirSelectOp::Recv { channel, .. } => {
                    let channel_reg = self.compile_expr(channel)?;
                    let bind_reg = self.alloc_reg();
                    SelectArmMeta {
                        kind: SelectArmKind::Recv,
                        channel_reg,
                        value_reg: 0,
                        bind_reg,
                        body_block: 0,
                    }
                }
                HirSelectOp::Send { channel, value } => {
                    let channel_reg = self.compile_expr(channel)?;
                    let value_reg = self.compile_expr(value)?;
                    SelectArmMeta {
                        kind: SelectArmKind::Send,
                        channel_reg,
                        value_reg,
                        bind_reg: 0,
                        body_block: 0,
                    }
                }
                HirSelectOp::Default => SelectArmMeta {
                    kind: SelectArmKind::Default,
                    channel_reg: 0,
                    value_reg: 0,
                    bind_reg: 0,
                    body_block: 0,
                },
            };
            self.select_arms.push(meta);
        }
        let count = u16::try_from(arms.len())
            .map_err(|_| RuntimeError::Unsupported("too many select arms in one select"))?;
        self.emit(Op::Select { first, count });

        // Pass 2: each arm body becomes a basic block. The handler
        // jumps to one of them; every block moves its result into the
        // shared `result` register and jumps to the continuation.
        let mut end_jumps: Vec<InstrIdx> = Vec::new();
        for (i, arm) in arms.iter().enumerate() {
            let meta_idx = first as usize + i;
            self.select_arms[meta_idx].body_block = self.cur_idx();
            self.push_scope();
            if let HirSelectOp::Recv { pattern, .. } = &arm.op {
                let bind_reg = self.select_arms[meta_idx].bind_reg;
                self.bind_pattern_locals(pattern, bind_reg)?;
            }
            let body_reg = self.compile_expr(&arm.body)?;
            self.emit(Op::Move {
                dst: result,
                src: body_reg,
            });
            end_jumps.push(self.emit(Op::Jump { target: 0 }));
            self.pop_scope();
        }
        let end = self.cur_idx();
        for j in end_jumps {
            self.patch_jump(j, end);
        }
        Ok(result)
    }

    /// Emits the shape-test + binding-extraction sequence for one
    /// pattern against the value in `scrut`. Pushes a branch index
    /// onto `fails` for every test that must jump to the next arm on
    /// mismatch; on the fall-through (match) path the pattern's
    /// bindings are live in the current scope.
    pub(crate) fn emit_pattern_test(
        &mut self,
        scrut: Reg,
        pat: &HirPat,
        fails: &mut Vec<InstrIdx>,
    ) -> RuntimeResult<()> {
        match &pat.kind {
            HirPatKind::Wildcard | HirPatKind::Rest => {}
            HirPatKind::Binding { name, .. } => {
                // Copy into a fresh reg so a `let mut`-style rebind in
                // the arm body can't clobber the scrutinee register.
                let r = self.alloc_reg();
                self.emit(Op::Move { dst: r, src: scrut });
                self.bind_local(
                    &name.name,
                    TypedReg {
                        reg: r,
                        kind: RegKind::Value,
                    },
                );
            }
            HirPatKind::Literal(lit) => {
                let lit_reg = self.compile_literal(lit)?;
                let eq = self.alloc_reg();
                self.emit(Op::Eq {
                    dst: eq,
                    lhs: scrut,
                    rhs: lit_reg,
                });
                fails.push(self.emit(Op::BranchIfNot {
                    cond: eq,
                    target: 0,
                }));
            }
            HirPatKind::Variant { name, fields } => {
                let name_idx = self.shape_name_idx(name.name.as_str());
                let arity = u16::try_from(fields.len())
                    .map_err(|_| RuntimeError::Unsupported("variant arity exceeds 65535"))?;
                let test = self.alloc_reg();
                self.emit(Op::VariantIs {
                    dst: test,
                    src: scrut,
                    name_idx,
                    arity,
                });
                fails.push(self.emit(Op::BranchIfNot {
                    cond: test,
                    target: 0,
                }));
                for (i, fp) in fields.iter().enumerate() {
                    let fr = self.alloc_reg();
                    self.emit(Op::VariantField {
                        dst: fr,
                        src: scrut,
                        idx: u16::try_from(i).expect("field index overflow"),
                    });
                    self.emit_pattern_test(fr, fp, fails)?;
                }
            }
            HirPatKind::Struct { name, fields, .. } => {
                let name_idx = self.shape_name_idx(name.name.as_str());
                let test = self.alloc_reg();
                self.emit(Op::StructIs {
                    dst: test,
                    src: scrut,
                    name_idx,
                });
                fails.push(self.emit(Op::BranchIfNot {
                    cond: test,
                    target: 0,
                }));
                for fp in fields {
                    let fname_idx = self.const_idx(
                        ConstKey::String(fp.name.name.clone()),
                        Value::String(SmolStr::from(fp.name.name.as_str())),
                    );
                    let fr = self.alloc_reg();
                    let cache_idx = self.alloc_field_cache_idx();
                    self.emit(Op::FieldGet {
                        dst: fr,
                        receiver: scrut,
                        name_idx: fname_idx,
                        cache_idx,
                    });
                    if let Some(sub) = &fp.pattern {
                        self.emit_pattern_test(fr, sub, fails)?;
                    } else {
                        // `Struct { field }` shorthand binds `field`.
                        self.bind_local(
                            &fp.name.name,
                            TypedReg {
                                reg: fr,
                                kind: RegKind::Value,
                            },
                        );
                    }
                }
            }
            HirPatKind::Ref { inner, .. } => self.emit_pattern_test(scrut, inner, fails)?,
            HirPatKind::At { name, sub, .. } => {
                self.emit_pattern_test(scrut, sub, fails)?;
                let r = self.alloc_reg();
                self.emit(Op::Move { dst: r, src: scrut });
                self.bind_local(
                    &name.name,
                    TypedReg {
                        reg: r,
                        kind: RegKind::Value,
                    },
                );
            }
            HirPatKind::Range { lo, hi, inclusive } => {
                let lo_reg = self.compile_literal(lo)?;
                let ge = self.alloc_reg();
                self.emit(Op::Ge {
                    dst: ge,
                    lhs: scrut,
                    rhs: lo_reg,
                });
                fails.push(self.emit(Op::BranchIfNot {
                    cond: ge,
                    target: 0,
                }));
                let hi_reg = self.compile_literal(hi)?;
                let cmp = self.alloc_reg();
                if *inclusive {
                    self.emit(Op::Le {
                        dst: cmp,
                        lhs: scrut,
                        rhs: hi_reg,
                    });
                } else {
                    self.emit(Op::Lt {
                        dst: cmp,
                        lhs: scrut,
                        rhs: hi_reg,
                    });
                }
                fails.push(self.emit(Op::BranchIfNot {
                    cond: cmp,
                    target: 0,
                }));
            }
            HirPatKind::Tuple(parts) => {
                let rest_pos = parts
                    .iter()
                    .position(|p| matches!(p.kind, HirPatKind::Rest));
                for (i, part) in parts.iter().enumerate() {
                    if matches!(part.kind, HirPatKind::Rest) {
                        continue;
                    }
                    let elem = self.alloc_reg();
                    match rest_pos {
                        Some(rp) if i > rp => {
                            // Element after `..` - index from the end.
                            let from_end = parts.len() - 1 - i;
                            self.emit(Op::TupleTailIndex {
                                dst: elem,
                                receiver: scrut,
                                offset_from_end: u32::try_from(from_end)
                                    .expect("tuple tail index overflow"),
                            });
                        }
                        _ => {
                            self.emit(Op::TupleIndex {
                                dst: elem,
                                receiver: scrut,
                                index: u32::try_from(i).expect("tuple index overflow"),
                            });
                        }
                    }
                    self.emit_pattern_test(elem, part, fails)?;
                }
            }
            HirPatKind::Slice {
                prefix,
                rest,
                suffix,
            } => {
                // `len = scrut.len()` as a boxed `Value::Int`.
                let len_reg = self.alloc_reg();
                let len_name = self.global_idx("len");
                let cache_idx = self.alloc_cache_idx();
                self.emit(Op::MethodCall {
                    dst: len_reg,
                    receiver: scrut,
                    name_idx: len_name,
                    args: 0,
                    argc: 0,
                    cache_idx,
                });
                let n_prefix = i64::try_from(prefix.len())
                    .map_err(|_| RuntimeError::Unsupported("slice prefix too long"))?;
                let n_suffix = i64::try_from(suffix.len())
                    .map_err(|_| RuntimeError::Unsupported("slice suffix too long"))?;
                // Length guard: `len >= n_prefix + n_suffix` with a `..`,
                // `len == n_prefix` for a fixed-length slice pattern.
                let bound = if rest.is_some() {
                    n_prefix + n_suffix
                } else {
                    n_prefix
                };
                let bound_reg = self.load_int_value(bound);
                let test = self.alloc_reg();
                if rest.is_some() {
                    self.emit(Op::Ge {
                        dst: test,
                        lhs: len_reg,
                        rhs: bound_reg,
                    });
                } else {
                    self.emit(Op::Eq {
                        dst: test,
                        lhs: len_reg,
                        rhs: bound_reg,
                    });
                }
                fails.push(self.emit(Op::BranchIfNot {
                    cond: test,
                    target: 0,
                }));
                // Prefix elements: `scrut[i]`.
                for (i, sub) in prefix.iter().enumerate() {
                    let idx_reg = self.load_int_value(i as i64);
                    let elem = self.alloc_reg();
                    self.emit(Op::IndexGet {
                        dst: elem,
                        base: scrut,
                        index: idx_reg,
                    });
                    self.emit_pattern_test(elem, sub, fails)?;
                }
                // Suffix elements: `scrut[len - n_suffix + j]`.
                for (j, sub) in suffix.iter().enumerate() {
                    let off_reg = self.load_int_value(j as i64 - n_suffix);
                    let idx_reg = self.alloc_reg();
                    let add_cache = self.next_arith_cache();
                    self.emit(Op::AddInt {
                        dst: idx_reg,
                        lhs: len_reg,
                        rhs: off_reg,
                        cache_idx: add_cache,
                    });
                    let elem = self.alloc_reg();
                    self.emit(Op::IndexGet {
                        dst: elem,
                        base: scrut,
                        index: idx_reg,
                    });
                    self.emit_pattern_test(elem, sub, fails)?;
                }
                // `..rest` binding: `scrut.slice(n_prefix, len - n_suffix)`
                // yields `Ok(sub)`; extract the payload and bind it.
                if let Some(rest) = rest {
                    if let HirPatKind::Binding { name, .. } = &rest.kind {
                        let lo_reg = self.load_int_value(n_prefix);
                        let neg_suffix = self.load_int_value(-n_suffix);
                        let hi_reg = self.alloc_reg();
                        let hi_cache = self.next_arith_cache();
                        self.emit(Op::AddInt {
                            dst: hi_reg,
                            lhs: len_reg,
                            rhs: neg_suffix,
                            cache_idx: hi_cache,
                        });
                        let args_start = self.next_reg;
                        self.next_reg = self
                            .next_reg
                            .checked_add(2)
                            .expect("register overflow reserving slice args");
                        self.emit(Op::Move {
                            dst: args_start,
                            src: lo_reg,
                        });
                        self.emit(Op::Move {
                            dst: args_start + 1,
                            src: hi_reg,
                        });
                        let slice_res = self.alloc_reg();
                        let slice_name = self.global_idx("slice");
                        let slice_cache = self.alloc_cache_idx();
                        self.emit(Op::MethodCall {
                            dst: slice_res,
                            receiver: scrut,
                            name_idx: slice_name,
                            args: args_start,
                            argc: 2,
                            cache_idx: slice_cache,
                        });
                        let sub = self.alloc_reg();
                        self.emit(Op::VariantField {
                            dst: sub,
                            src: slice_res,
                            idx: 0,
                        });
                        self.bind_local(
                            &name.name,
                            TypedReg {
                                reg: sub,
                                kind: RegKind::Value,
                            },
                        );
                    }
                }
            }
            HirPatKind::Or(alts) if !pattern_has_binding(pat) => {
                // No alternative binds, so each alt is a pure test.
                // Emit them as a short-circuit OR: the first alt that
                // matches jumps past the rest to the shared
                // continuation; if every alt fails, fall through to
                // the arm-fail branch.
                let mut matched: Vec<InstrIdx> = Vec::new();
                for alt in alts {
                    let mut alt_fails: Vec<InstrIdx> = Vec::new();
                    self.emit_pattern_test(scrut, alt, &mut alt_fails)?;
                    // This alt matched - jump to the continuation.
                    matched.push(self.emit(Op::Jump { target: 0 }));
                    // This alt failed - next alt starts here.
                    let next_alt = self.cur_idx();
                    for f in alt_fails {
                        self.patch_jump(f, next_alt);
                    }
                }
                // All alternatives failed: jump to the arm-fail target.
                fails.push(self.emit(Op::Jump { target: 0 }));
                // Matched continuation.
                let cont = self.cur_idx();
                for m in matched {
                    self.patch_jump(m, cont);
                }
            }
            HirPatKind::Or(alts) => {
                // Binding or-pattern: every alternative binds the same
                // set of names (a typecheck invariant). One shared
                // register per name is the single home the arm body
                // reads, regardless of which alternative won - so each
                // alternative copies its freshly-extracted bindings
                // into those shared registers on its match path before
                // jumping to the continuation. Mirrors the MIR lowering
                // (`gossamer-mir/.../ctrl.rs`), which writes every
                // alternative's bindings into common slots.
                let mut names: Vec<String> = Vec::new();
                collect_pattern_binding_names(pat, &mut names);
                let shared: Vec<(String, Reg)> = names
                    .into_iter()
                    .map(|name| (name, self.alloc_reg()))
                    .collect();

                let mut matched: Vec<InstrIdx> = Vec::new();
                for alt in alts {
                    debug_assert!(
                        {
                            let mut alt_names = Vec::new();
                            collect_pattern_binding_names(alt, &mut alt_names);
                            alt_names.len() == shared.len()
                                && alt_names.iter().all(|n| shared.iter().any(|(s, _)| s == n))
                        },
                        "or-pattern alternatives must bind the same set of names"
                    );
                    let mut alt_fails: Vec<InstrIdx> = Vec::new();
                    // Compile the alternative in an inner scope so its
                    // leaf bindings land in fresh registers that this
                    // scope owns; relocate each into its shared home,
                    // then discard the scope.
                    self.push_scope();
                    self.emit_pattern_test(scrut, alt, &mut alt_fails)?;
                    for (name, dst) in &shared {
                        if let Some(src) = self.lookup_local(name) {
                            let src_v = self.as_value(src);
                            self.emit(Op::Move {
                                dst: *dst,
                                src: src_v,
                            });
                        }
                    }
                    self.pop_scope();
                    // This alt matched - jump to the continuation.
                    matched.push(self.emit(Op::Jump { target: 0 }));
                    // This alt failed - next alt starts here.
                    let next_alt = self.cur_idx();
                    for f in alt_fails {
                        self.patch_jump(f, next_alt);
                    }
                }
                // All alternatives failed: jump to the arm-fail target.
                fails.push(self.emit(Op::Jump { target: 0 }));
                // Matched continuation: expose each shared register to
                // the arm body (and any guard) under its bound name.
                let cont = self.cur_idx();
                for m in matched {
                    self.patch_jump(m, cont);
                }
                for (name, reg) in &shared {
                    self.bind_local(
                        name,
                        TypedReg {
                            reg: *reg,
                            kind: RegKind::Value,
                        },
                    );
                }
            }
        }
        Ok(())
    }

    pub(crate) fn compile_literal(&mut self, lit: &HirLiteral) -> RuntimeResult<Reg> {
        let (key, value) = literal_const(lit);
        let idx = self.const_idx(key, value);
        let dst = self.alloc_reg();
        self.emit(Op::LoadConst { dst, idx });
        Ok(dst)
    }

    /// Loads an `i64` constant into a fresh boxed `Value::Int` register.
    pub(crate) fn load_int_value(&mut self, value: i64) -> Reg {
        let idx = self.const_idx(ConstKey::Int(value), Value::Int(value));
        let dst = self.alloc_reg();
        self.emit(Op::LoadConst { dst, idx });
        dst
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
        // Both operands f64 - emit a typed f64 op. For `+-*/`
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
            let lhs_unsigned = self.is_unsigned64_ty(lhs.ty);
            let rhs_unsigned = self.is_unsigned64_ty(rhs.ty);
            let lhs_tr = self.compile_expr_ex(lhs)?;
            let rhs_tr = self.compile_expr_ex(rhs)?;
            let lhs_i = self.as_i64(lhs_tr);
            let rhs_i = self.as_i64(rhs_tr);
            return self.emit_binary_i64(op, lhs_i, rhs_i, lhs_unsigned, rhs_unsigned);
        }
        // Struct `==` / `!=` routes to the derived `<Type>::eq` method,
        // which the bytecode `Op::Eq` (scalar / structural) can't express
        // for a `Value::Struct`. `==` only typechecks on a struct that
        // derives or implements `PartialEq`, so the method is present.
        // Enums fall through to the structural `Op::Eq` below.
        if matches!(op, HirBinaryOp::Eq | HirBinaryOp::Ne) {
            if let Some(sname) = self
                .struct_eq_dispatch_name(lhs.ty)
                .or_else(|| self.struct_eq_dispatch_name(rhs.ty))
            {
                return self.compile_struct_eq(&sname, op, lhs, rhs);
            }
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

    /// Lowers a struct `==` / `!=` to a call of the derived
    /// `<sname>::eq(lhs, rhs)` method, negating the result for `!=`.
    /// Mirrors the compiled tiers, which route aggregate equality to the
    /// same synthesized method.
    fn compile_struct_eq(
        &mut self,
        sname: &str,
        op: HirBinaryOp,
        lhs: &HirExpr,
        rhs: &HirExpr,
    ) -> RuntimeResult<TypedReg> {
        let key = format!("{sname}::eq");
        let idx = self.global_idx(&key);
        let callee_reg = self.alloc_reg();
        self.emit(Op::LoadGlobal {
            dst: callee_reg,
            idx,
        });
        // Reserve the two argument slots before compiling either operand
        // so an operand whose compile allocates fresh registers can't land
        // inside the not-yet-populated span. Mirrors `compile_call_ex`.
        let args_start = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(2)
            .expect("register overflow reserving eq args");
        let lhs_reg = self.compile_expr(lhs)?;
        self.emit(Op::Move {
            dst: args_start,
            src: lhs_reg,
        });
        let rhs_reg = self.compile_expr(rhs)?;
        self.emit(Op::Move {
            dst: args_start + 1,
            src: rhs_reg,
        });
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        self.emit(Op::Call {
            dst,
            callee: callee_reg,
            args: args_start,
            argc: 2,
            cache_idx,
            may_have_cells: false,
        });
        if matches!(op, HirBinaryOp::Ne) {
            let neg = self.alloc_reg();
            self.emit(Op::Not {
                dst: neg,
                operand: dst,
            });
            return Ok(TypedReg {
                reg: neg,
                kind: RegKind::Value,
            });
        }
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
    /// wrap and lands directly in the float register file -
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
        // Fused `base[i].field` - avoids cloning the inner
        // struct `Arc`.
        if let HirExprKind::Index { base, index } = &receiver.kind {
            let base_reg = self.compile_expr(base)?;
            // Compile the index in its native register file. When it
            // lands in the int bank (the common loop-counter case) the
            // flat read can consume it directly, skipping the per-access
            // `BoxI64` that a `Value`-register index would force.
            let idx_tr = self.compile_expr_ex(index)?;
            if field_is_f64 {
                if let Some(offset) = offset {
                    // Known-flat local: emit the dedicated
                    // FloatArray-only read that skips the
                    // discriminant check.
                    if let Some(&stride) = self.flat_locals.get(&base_reg) {
                        let dst = self.alloc_float();
                        if idx_tr.kind == RegKind::I64 {
                            self.emit(Op::FlatGetF64I {
                                dst_f: dst,
                                base: base_reg,
                                index_i: idx_tr.reg,
                                stride,
                                offset,
                            });
                        } else {
                            let idx_reg = self.as_value(idx_tr);
                            self.emit(Op::FlatGetF64 {
                                dst_f: dst,
                                base: base_reg,
                                index: idx_reg,
                                stride,
                                offset,
                            });
                        }
                        return Ok(TypedReg {
                            reg: dst,
                            kind: RegKind::F64,
                        });
                    }
                    let idx_reg = self.as_value(idx_tr);
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
                let idx_reg = self.as_value(idx_tr);
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
            let idx_reg = self.as_value(idx_tr);
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
        // Plain `value.field` - the receiver itself is a
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

    /// Emits a dedicated in-place Vec op (`VecPush` / `VecInsert` /
    /// `VecRemove`) for a bare-local Vec receiver whose mutating
    /// method's result is discarded (statement position). Returns
    /// `true` when it handled the call. The op mutates the receiver
    /// register's backing storage directly via `Arc::make_mut`, so a
    /// `push` loop grows in amortized O(1) instead of deep-copying the
    /// whole Vec per call. Other receiver shapes (index / field /
    /// temporary) and expression-position uses keep the generic
    /// builtin + writeback path, which produces the same final state.
    pub(crate) fn try_compile_inplace_vec_stmt(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
    ) -> RuntimeResult<bool> {
        let HirExprKind::Path { segments, .. } = &receiver.kind else {
            return Ok(false);
        };
        let [seg] = segments.as_slice() else {
            return Ok(false);
        };
        // Concrete Vec-like receivers only. `String` / `bytes::Builder`
        // also carry `push` / `insert` / `remove` but back onto different
        // storage, and an unresolved `Var` receiver (e.g. `String::new()`)
        // can be any of them - those keep the generic dispatch, which
        // selects the right builtin by the runtime value's type.
        let is_concrete_vec = matches!(
            self.tcx.kind(receiver.ty),
            Some(TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. })
        );
        let target_reg = match self.lookup_local(&seg.name) {
            Some(target) if target.kind == RegKind::Value => target.reg,
            _ => return Ok(false),
        };
        // A flat typed-storage local (`IntArray` / `FloatVec`) or a local
        // tagged at `let` time as a Vec constructor / array literal is a
        // Vec even when its static type stayed an inference var.
        if !is_concrete_vec
            && !self.flat_int_locals.contains(&target_reg)
            && !self.flat_float_locals.contains(&target_reg)
            && !self.collection_locals.contains(&target_reg)
        {
            return Ok(false);
        }
        match (name.name.as_str(), args.len()) {
            ("push", 1) => {
                let recv = self.compile_expr(receiver)?;
                let value = self.compile_expr(&args[0])?;
                self.emit(Op::VecPush {
                    receiver: recv,
                    value,
                });
                Ok(true)
            }
            ("insert", 2) => {
                let recv = self.compile_expr(receiver)?;
                let index = self.compile_expr(&args[0])?;
                let value = self.compile_expr(&args[1])?;
                self.emit(Op::VecInsert {
                    receiver: recv,
                    index,
                    value,
                });
                Ok(true)
            }
            ("remove", 1) => {
                let recv = self.compile_expr(receiver)?;
                let index = self.compile_expr(&args[0])?;
                self.emit(Op::VecRemove {
                    receiver: recv,
                    index,
                });
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    pub(crate) fn compile_method_call(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
    ) -> RuntimeResult<Reg> {
        // A `&mut self` user method on a writeback place rides the cell
        // protocol so its mutation of `self` reaches the caller's
        // binding - the mechanism `for x in <custom iterator>` and every
        // stateful `obj.advance()` depend on. Tried first so a user
        // struct whose `&mut self` method shadows a builtin name (`pop`,
        // `swap`, `insert`) routes to the user method.
        if let Some(reg) = self.try_compile_mut_self_method(receiver, name, args)? {
            return Ok(reg);
        }
        // `d.as_millis()` / `d.as_secs()` / `d.as_micros()` - method form
        // of the `time::Duration` accessors. A Duration value is a bare
        // `Value::Int` at runtime with no qualified-key receiver, so the
        // generic `MethodCall` dispatch cannot reach the accessor by name.
        // Resolve it statically from the receiver's Duration type and emit
        // a direct call to the `time::Duration::<accessor>` global.
        if matches!(name.name.as_str(), "as_millis" | "as_secs" | "as_micros") && args.is_empty() {
            let mut k = self.tcx.kind(receiver.ty).cloned();
            while let Some(TyKind::Ref { inner, .. }) = k {
                k = self.tcx.kind(inner).cloned();
            }
            // A `flag::Set` duration cell (`fs.duration(...)`) carries no
            // Duration tag on its HIR type (an unresolved inference var),
            // so dispatch on the compile-time `duration_cell_locals` tag.
            // The cell is a `__Cell` handle at runtime; auto-derefing it at
            // the call boundary yields its backing `Value::Int`-of-ms, so
            // the accessor receives the same shape as a bare Duration.
            let is_duration_cell = self.receiver_is_duration_cell(receiver);
            if matches!(k, Some(TyKind::Duration)) || is_duration_cell {
                let qual = format!("time::Duration::{}", name.name);
                let idx = self.global_idx(&qual);
                let callee_reg = self.alloc_reg();
                self.emit(Op::LoadGlobal {
                    dst: callee_reg,
                    idx,
                });
                let args_start = self.next_reg;
                self.next_reg = self
                    .next_reg
                    .checked_add(1)
                    .expect("register overflow reserving duration accessor arg");
                let recv_reg = self.compile_expr(receiver)?;
                self.emit(Op::Move {
                    dst: args_start,
                    src: recv_reg,
                });
                let dst = self.alloc_reg();
                let cache_idx = self.alloc_cache_idx();
                self.emit(Op::Call {
                    dst,
                    callee: callee_reg,
                    args: args_start,
                    argc: 1,
                    cache_idx,
                    may_have_cells: is_duration_cell,
                });
                return Ok(dst);
            }
        }
        // `inst.elapsed_ms()` - method form of the `time::Instant`
        // accessor. An Instant value is a bare `Value::Int` of monotonic
        // ms at runtime with no qualified-key receiver, so resolve it
        // statically from the receiver's Instant type and emit a direct
        // call to the `time::Instant::elapsed_ms` global.
        if name.name.as_str() == "elapsed_ms" && args.is_empty() {
            let mut k = self.tcx.kind(receiver.ty).cloned();
            while let Some(TyKind::Ref { inner, .. }) = k {
                k = self.tcx.kind(inner).cloned();
            }
            if matches!(k, Some(TyKind::Instant)) {
                let idx = self.global_idx("time::Instant::elapsed_ms");
                let callee_reg = self.alloc_reg();
                self.emit(Op::LoadGlobal {
                    dst: callee_reg,
                    idx,
                });
                let args_start = self.next_reg;
                self.next_reg = self
                    .next_reg
                    .checked_add(1)
                    .expect("register overflow reserving instant accessor arg");
                let recv_reg = self.compile_expr(receiver)?;
                self.emit(Op::Move {
                    dst: args_start,
                    src: recv_reg,
                });
                let dst = self.alloc_reg();
                let cache_idx = self.alloc_cache_idx();
                self.emit(Op::Call {
                    dst,
                    callee: callee_reg,
                    args: args_start,
                    argc: 1,
                    cache_idx,
                    may_have_cells: false,
                });
                return Ok(dst);
            }
        }
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
        // function body and has no intrinsic for the writeback -
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
        // `xs.pop()` evaluates to `Option<last>` while shortening the
        // receiver. `Op::VecPop` does both in one in-place step: it
        // returns `Some(last)` / `None` and shrinks the receiver
        // register's backing storage without copying it. A bare-local
        // receiver's register is the local's own slot, so the mutation
        // persists; a temporary receiver's mutation is discarded, which
        // matches the compiled tiers (pop on a temporary is a no-op).
        // Unresolved receiver types (`Var` / missing) still take this
        // path: the dominant producer is a stdlib call like
        // `os::read_file` whose Vec result type the checker leaves open;
        // user receivers with their own `pop` are `Adt`-typed and excluded.
        if name.name == "pop"
            && args.is_empty()
            && matches!(
                self.tcx.kind(receiver.ty),
                None | Some(
                    TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. } | TyKind::Var(_)
                )
            )
        {
            let opt_dst = self.alloc_reg();
            self.emit(Op::VecPop {
                dst: opt_dst,
                receiver: receiver_reg,
            });
            return Ok(opt_dst);
        }
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
        // `Op::BoxI64` - the register allocator and downstream
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
        // `m.pop(k)` on a HashMap mutates the map in place (it is an
        // `Arc<Mutex<..>>`) and returns `Option<V>`. The name-global `pop`
        // resolves to the Vec pop builtin, and the mutating-writeback below
        // would then overwrite the map binding with that result; route to
        // the qualified map builtin and suppress the writeback instead.
        let is_map_pop = name.name == "pop"
            && args.len() == 1
            && matches!(self.tcx.kind(receiver.ty), Some(TyKind::HashMap { .. }));
        let name_idx = self.global_idx(if is_map_pop {
            "HashMap::pop"
        } else {
            &name.name
        });
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
        // Mutating-method writeback. The builtins for `push` /
        // `insert` / etc. return the *new* aggregate rather than
        // mutating in place, so the VM has to thread the result back
        // into the receiver's storage. A bare local receiver is the
        // common case (one `Op::Move`); an index / field place rooted
        // at a local (`groups[i].push(x)`, `bag.items.push(x)`) splices
        // the result back through the place-store protocol so the
        // mutation persists - matching the compiled tiers, which mutate
        // the backing storage in place.
        if !is_map_pop && Self::is_mutating_method_name(name.name.as_str()) {
            match &receiver.kind {
                HirExprKind::Path { segments, .. } if segments.len() == 1 => {
                    if let Some(target) = self.lookup_local(&segments[0].name) {
                        if target.kind == RegKind::Value && target.reg == receiver_reg {
                            self.emit(Op::Move {
                                dst: target.reg,
                                src: dst,
                            });
                        }
                    }
                }
                HirExprKind::Index { .. } | HirExprKind::Field { .. }
                    if self.place_root_is_local(receiver) =>
                {
                    self.compile_place_store(receiver, dst)?;
                }
                _ => {}
            }
        }
        Ok(dst)
    }

    /// Lowers a `&mut self` user-method call (`obj.bump()`,
    /// `(&mut __for_iter).next()`) through the write-back cell protocol
    /// so the method's mutation of `self` persists in the caller's
    /// binding. Returns `Some(result_reg)` when the receiver resolves to
    /// a local-rooted place whose `Type::method` is a known `&mut self`
    /// method; otherwise `None`, leaving the generic dispatch to handle
    /// it (a temporary receiver's mutation is discarded, matching the
    /// compiled tiers). The receiver crosses as a `MutCell`; the callee
    /// unwraps it (its `self` register is a `mut_ref_param`) and
    /// publishes the post-call `self` on return, which `Op::CellTake` +
    /// `compile_place_store` write back into the receiver place.
    fn try_compile_mut_self_method(
        &mut self,
        receiver: &HirExpr,
        name: &Ident,
        args: &[HirExpr],
    ) -> RuntimeResult<Option<Reg>> {
        let place = peel_ref_wrappers_expr(receiver);
        // Match the compiled tiers: only a direct local receiver's
        // `&mut self` mutation persists. A field / index / temporary
        // receiver (`w.c.bump()`, `arr[i].bump()`, `make().bump()`)
        // operates on a value copy whose mutation `gos build` discards,
        // so writing it back here would diverge from the AOT oracle. A
        // local Path is also the shape the `for x in <custom iterator>`
        // desugar produces (`(&mut __for_iter).next()`).
        let HirExprKind::Path { segments, .. } = &place.kind else {
            return Ok(None);
        };
        let [seg] = segments.as_slice() else {
            return Ok(None);
        };
        if self.lookup_local(&seg.name).is_none() {
            return Ok(None);
        }
        let Some(type_name) = self.adt_type_name(place.ty) else {
            return Ok(None);
        };
        let qual = format!("{type_name}::{}", name.name);
        if !self.method_muts.contains(&qual) {
            return Ok(None);
        }
        let total = args.len() + 1;
        let argc = u16::try_from(total).map_err(|_| RuntimeError::Arity {
            expected: u16::MAX as usize,
            found: total,
        })?;
        // `Type::method` is registered for every user `impl` method, so
        // the global resolves; loading it yields the callee identity the
        // `Op::Call` inline cache keys on.
        let global_idx = self.global_idx(&qual);
        let callee_reg = self.alloc_reg();
        self.emit(Op::LoadGlobal {
            dst: callee_reg,
            idx: global_idx,
        });
        // Reserve the contiguous arg block (receiver cell + declared
        // args) before compiling any operand, so an operand whose
        // compile allocates fresh registers can't clobber the span.
        let args_start = self.next_reg;
        self.next_reg = self
            .next_reg
            .checked_add(argc)
            .expect("register overflow reserving mut-self method args");
        let place_reg = self.compile_expr(place)?;
        // Evaluate every argument BEFORE capturing the receiver, so an argument
        // that reads the receiver (`c.bump(c.value)`) still sees its live value
        // - `CellNewMove` empties the receiver's register immediately after.
        let mut arg_regs: Vec<Reg> = Vec::with_capacity(args.len());
        for arg in args {
            arg_regs.push(self.compile_expr(arg)?);
        }
        let cell = self.alloc_reg();
        // Move (not clone) the receiver into the cell. `CellTake` below
        // republishes the post-call `self` into the same place, and moving
        // keeps the receiver's refcount at one so the callee's first field
        // write mutates in place instead of forcing a copy-on-write clone.
        self.emit(Op::CellNewMove {
            dst: cell,
            src: place_reg,
        });
        self.emit(Op::Move {
            dst: args_start,
            src: cell,
        });
        for (i, r) in arg_regs.iter().enumerate() {
            let slot = args_start
                .checked_add(u16::try_from(i + 1).expect("argc overflow"))
                .expect("register overflow");
            self.emit(Op::Move { dst: slot, src: *r });
        }
        let dst = self.alloc_reg();
        let cache_idx = self.alloc_cache_idx();
        // The receiver `MutCell` passes through `auto_deref_cell`
        // untouched (it only resolves `flag::Cell` handles); a declared
        // arg that is a `flag::Cell` still needs the auto-deref, so gate
        // it on a non-scalar arg being present.
        let may_have_cells = !args.iter().all(|a| {
            matches!(
                self.tcx.kind(a.ty),
                Some(TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char)
            )
        });
        self.emit(Op::Call {
            dst,
            callee: callee_reg,
            args: args_start,
            argc,
            cache_idx,
            may_have_cells,
        });
        let tmp = self.alloc_reg();
        self.emit(Op::CellTake { dst: tmp, cell });
        self.compile_place_store(place, tmp)?;
        Ok(Some(dst))
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
        // `Vec::remove(xs, i)` over a bare-local Vec: mutate the local in
        // place and return `Ok(element)`, matching the compiled tier's
        // in-place `gos_rt_vec_remove_safe`. Other receiver shapes keep the
        // generic builtin (which reads the element without mutating - a
        // tier divergence the in-place op avoids for the common case).
        if args.len() == 2
            && let HirExprKind::Path { segments, .. } = &callee.kind
        {
            let segs: Vec<&str> = segments.iter().map(|s| s.name.as_str()).collect();
            if matches!(
                segs.as_slice(),
                ["Vec", "remove"] | ["collections", "Vec", "remove"]
            ) && let HirExprKind::Path {
                segments: arg_segs, ..
            } = &args[0].kind
                && let [seg] = arg_segs.as_slice()
            {
                if let Some(target) = self.lookup_local(&seg.name)
                    && target.kind == RegKind::Value
                {
                    let reg = target.reg;
                    let is_vec = matches!(
                        self.tcx.kind(args[0].ty),
                        Some(TyKind::Vec(_) | TyKind::Slice(_) | TyKind::Array { .. })
                    ) || self.flat_int_locals.contains(&reg)
                        || self.flat_float_locals.contains(&reg)
                        || self.collection_locals.contains(&reg);
                    if is_vec {
                        let index = self.compile_expr(&args[1])?;
                        let dst = self.alloc_reg();
                        self.emit(Op::VecRemoveAt {
                            dst,
                            receiver: reg,
                            index,
                        });
                        return Ok(dst);
                    }
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
        // `&mut Vec<T>` / `&mut [T]` / `&mut <scalar>` arguments ride the
        // write-back cell protocol: wrap the current value in a cell, pass
        // the cell (the callee unwraps it via `mut_ref_params`), and read
        // the callee's final value back after the call. A `&mut <local
        // Vec>` writes straight back into the local's register
        // (`cell_takes`); a non-local place (`&mut arr[i]`, `&mut
        // obj.field`, `&mut <scalar local>`) takes the cell's inner into a
        // temp and re-stores it through the place (`place_takes`).
        let mut cell_takes: Vec<(Reg, Reg)> = Vec::new();
        let mut place_takes: Vec<(&HirExpr, Reg)> = Vec::new();
        let mut arg_regs: Vec<Reg> = Vec::with_capacity(args.len());
        // A `println!`/`format!` lowers to `__concat(...)`; an unsigned-64
        // argument is boxed as `Value::Uint` so it renders unsigned (a
        // `u64` above 2^63 prints as a large positive decimal). Mirrors the
        // LLVM tier choosing the unsigned printer by declared type.
        let concat_call = Self::callee_is_concat(callee);
        for arg in args {
            if let Some(home) = self.mut_ref_arg_home(arg) {
                let cell = self.alloc_reg();
                self.emit(Op::CellNew {
                    dst: cell,
                    src: home,
                });
                cell_takes.push((home, cell));
                arg_regs.push(cell);
            } else if let Some(place) = Self::mut_ref_writeback_place(self.tcx, arg) {
                let place_reg = self.compile_expr(place)?;
                let cell = self.alloc_reg();
                self.emit(Op::CellNew {
                    dst: cell,
                    src: place_reg,
                });
                place_takes.push((place, cell));
                arg_regs.push(cell);
            } else if concat_call && self.is_unsigned64_ty(arg.ty) {
                let tr = self.compile_expr_ex(arg)?;
                let src_i = self.as_i64(tr);
                let dst_v = self.alloc_reg();
                self.emit(Op::I64ToUint { dst_v, src_i });
                arg_regs.push(dst_v);
            } else {
                arg_regs.push(self.compile_expr(arg)?);
            }
        }
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
        // A `flag::Cell` handle is a `Value::Struct("__Cell")`; a
        // primitive-scalar argument can never be one, so a call whose
        // every argument is scalar-typed needs no per-argument
        // auto-deref check.
        let may_have_cells = !args.iter().all(|a| {
            matches!(
                self.tcx.kind(a.ty),
                Some(TyKind::Int(_) | TyKind::Float(_) | TyKind::Bool | TyKind::Char)
            )
        });
        self.emit(Op::Call {
            dst,
            callee: callee_reg,
            args: args_start,
            argc,
            cache_idx,
            may_have_cells,
        });
        for (home, cell) in cell_takes {
            self.emit(Op::CellTake { dst: home, cell });
        }
        for (place, cell) in place_takes {
            let tmp = self.alloc_reg();
            self.emit(Op::CellTake { dst: tmp, cell });
            self.compile_place_store(place, tmp)?;
        }
        Ok(dst)
    }

    /// True when a call's callee is the `__concat` string builder that
    /// `format!`/`println!` lower to. Used to box unsigned-64 arguments as
    /// `Value::Uint` so they render unsigned.
    fn callee_is_concat(callee: &HirExpr) -> bool {
        let HirExprKind::Path { segments, .. } = &callee.kind else {
            return false;
        };
        segments.last().is_some_and(|s| s.name == "__concat")
    }

    /// Returns the home register of a `&mut Vec<T>` / `&mut [T]`
    /// call argument when the argument is a plain local place -
    /// either `&mut x` over a local or a bare path forwarding a
    /// `&mut` parameter. Non-local places (fields, indexes) and
    /// non-`&mut`-vec types return `None` and take the ordinary
    /// pass-by-value path.
    fn mut_ref_arg_home(&self, arg: &HirExpr) -> Option<Reg> {
        if !crate::compile::is_mut_ref_vec(self.tcx, arg.ty) {
            return None;
        }
        let place = match &arg.kind {
            HirExprKind::Unary {
                op: HirUnaryOp::RefMut,
                operand,
            } => operand,
            HirExprKind::Path { .. } => arg,
            _ => return None,
        };
        let HirExprKind::Path { segments, .. } = &place.kind else {
            return None;
        };
        let [seg] = segments.as_slice() else {
            return None;
        };
        let tr = self.lookup_local(seg.name.as_str())?;
        (tr.kind == RegKind::Value).then_some(tr.reg)
    }

    /// Returns the lvalue place of a `&mut Vec<T>` / `&mut [T]` /
    /// `&mut <scalar>` call argument that is *not* a plain local Vec
    /// (`&mut s.field`, `&mut grid[i]`, `&mut <scalar local>`, or a bare
    /// path forwarding a `&mut` parameter). The caller wraps the place in
    /// a write-back cell and re-stores the callee's final value through it
    /// after the call. The plain-local-Vec case is handled separately by
    /// [`Self::mut_ref_arg_home`]; everything that isn't a write-through
    /// place (a temporary, a deref of a call result) returns `None`.
    fn mut_ref_writeback_place<'a>(tcx: &TyCtxt, arg: &'a HirExpr) -> Option<&'a HirExpr> {
        if !crate::compile::is_mut_ref_writeback(tcx, arg.ty) {
            return None;
        }
        let place = match &arg.kind {
            HirExprKind::Unary {
                op: HirUnaryOp::RefMut,
                operand,
            } => operand.as_ref(),
            _ => arg,
        };
        matches!(
            place.kind,
            HirExprKind::Path { .. } | HirExprKind::Field { .. } | HirExprKind::Index { .. }
        )
        .then_some(place)
    }
}
