#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Returns `true` when the statement diverges (return/break/continue).
    pub(crate) fn compile_stmt(&mut self, stmt: &HirStmt) -> RuntimeResult<bool> {
        match &stmt.kind {
            HirStmtKind::Let { pattern, init, .. } => {
                if let HirPatKind::Binding { name, .. } = &pattern.kind {
                    if let Some(init) = init {
                        // Compile the init in its natural kind.
                        // Most exprs produce a freshly-allocated
                        // reg we can bind directly; only a bare
                        // path lookup (`let y = x`) aliases an
                        // existing reg, so in that case copy
                        // into a fresh slot.
                        let tr = self.compile_expr_ex(init)?;
                        let typed = if is_path_expr(init) {
                            self.bind_to_fresh(tr)
                        } else {
                            tr
                        };
                        self.bind_local(&name.name, typed);
                    } else {
                        // Declared-only — default to Value; an
                        // assignment before read will overwrite.
                        let reg = self.alloc_reg();
                        self.bind_local(
                            &name.name,
                            TypedReg {
                                reg,
                                kind: RegKind::Value,
                            },
                        );
                    }
                } else if let Some(init) = init {
                    // Destructuring — compile init to a Value reg,
                    // then bind sub-patterns via TupleIndex. Only
                    // tuple/wildcard/binding shapes are handled
                    // here; complex patterns error out instead of
                    // silently dropping the bindings.
                    let init_reg = self.compile_expr(init)?;
                    self.bind_pattern_locals(pattern, init_reg)?;
                }
                Ok(false)
            }
            HirStmtKind::Expr { expr, .. } => {
                let _ = self.compile_expr(expr)?;
                Ok(expr_diverges(expr))
            }
            HirStmtKind::Go(expr) => {
                // Native goroutine spawn: compile the callee and
                // args directly into VM ops and emit `Op::Spawn`.
                // The dispatcher creates a fresh `Vm` on the new
                // thread that executes the call entirely in
                // bytecode — never re-entering the tree-walker.
                if self.try_compile_go_native(expr)? {
                    return Ok(false);
                }
                // Non-call shapes (e.g., `go { block }`) keep the
                // deferred path until we lower them too — but they
                // don't appear in the bench-game programs.
                let _ = self.compile_deferred(expr)?;
                Ok(false)
            }
            HirStmtKind::Defer(expr) => {
                // `defer` keeps tree-walker delegation: the VM
                // doesn't model the cleanup ordering it needs.
                let _ = self.compile_deferred(expr)?;
                Ok(false)
            }
            HirStmtKind::Item(_) => Err(RuntimeError::Unsupported("nested items")),
        }
    }

    pub(crate) fn compile_assign(
        &mut self,
        place: &HirExpr,
        value: &HirExpr,
    ) -> RuntimeResult<Reg> {
        if let HirExprKind::Path { segments, .. } = &place.kind {
            if let Some(first) = segments.first() {
                if let Some(target) = self.lookup_local(&first.name) {
                    // Typed-local reassignment: compile the
                    // RHS in the destination kind so no box /
                    // unbox round-trip happens in hot loops.
                    let src_tr = self.compile_expr_ex(value)?;
                    self.emit_move_into(target, src_tr);
                    return Ok(self.load_unit());
                }
            }
        }
        // Native `local.field = value` and `local[i] = value`
        // writes: emit FieldSet / IndexSet directly so the VM's
        // hot loops (nbody's body.vx / body.x updates) don't
        // incur the `Op::EvalDeferred` env-rebuild cost.
        if let HirExprKind::Field { receiver, name } = &place.kind {
            if let HirExprKind::Path { segments, .. } = &receiver.kind {
                if let Some(first) = segments.first() {
                    if let Some(target) = self.lookup_local(&first.name) {
                        if target.kind == RegKind::Value {
                            let value_reg = self.compile_expr(value)?;
                            let name_idx = self.const_idx(
                                ConstKey::String(name.name.clone()),
                                Value::String(SmolStr::from(name.name.clone())),
                            );
                            self.emit(Op::FieldSet {
                                receiver: target.reg,
                                name_idx,
                                value: value_reg,
                            });
                            return Ok(self.load_unit());
                        }
                        // Typed local can't be a struct;
                        // fall through to the deferred path.
                    }
                }
            }
        }
        if let HirExprKind::Index { base, index } = &place.kind {
            if let HirExprKind::Path { segments, .. } = &base.kind {
                if let Some(first) = segments.first() {
                    if let Some(target) = self.lookup_local(&first.name) {
                        if target.kind == RegKind::Value {
                            // Typed flat-f64 store fast path: when
                            // the receiver is a `Value::FloatVec`
                            // and the RHS is f64-typed, write
                            // straight from the f64 register file.
                            // Mirrors the IntArray IndexSet bypass
                            // in `IntArray` users.
                            let value_is_f64 = matches!(
                                self.tcx.kind(value.ty),
                                Some(TyKind::Float(FloatTy::F64))
                            );
                            if value_is_f64 && self.flat_float_locals.contains(&target.reg) {
                                let idx_tr = self.compile_expr_ex(index)?;
                                let idx_i = self.as_i64(idx_tr);
                                let value_tr = self.compile_expr_ex(value)?;
                                let value_f = self.as_f64(value_tr);
                                self.emit(Op::FloatVecSetF64 {
                                    base: target.reg,
                                    index_i: idx_i,
                                    value_f,
                                });
                                return Ok(self.load_unit());
                            }
                            // Mirror typed-i64 store fast path for
                            // `Value::IntArray`-backed locals:
                            // `arr[i] = v` where `arr: [i64; N]`
                            // skips the box/unbox the generic
                            // `Op::IndexSet` would impose. fannkuch's
                            // `perm[j] = perm1[j]` and count
                            // manipulation rely on this path. The
                            // gate is purely the receiver tracking
                            // (any `flat_int_locals` member is
                            // guaranteed `Value::IntArray` storage),
                            // because the value's HIR type can be a
                            // still-bound `TyKind::Var` for arithmetic
                            // expressions that typeck couldn't
                            // substitute in-place.
                            if self.flat_int_locals.contains(&target.reg) {
                                let idx_tr = self.compile_expr_ex(index)?;
                                let idx_i = self.as_i64(idx_tr);
                                let value_tr = self.compile_expr_ex(value)?;
                                let value_i = self.as_i64(value_tr);
                                self.emit(Op::IntArraySetI64 {
                                    base: target.reg,
                                    index_i: idx_i,
                                    value_i,
                                });
                                return Ok(self.load_unit());
                            }
                            let idx_reg = self.compile_expr(index)?;
                            let value_reg = self.compile_expr(value)?;
                            self.emit(Op::IndexSet {
                                base: target.reg,
                                index: idx_reg,
                                value: value_reg,
                            });
                            return Ok(self.load_unit());
                        }
                    }
                }
            }
        }
        // `local[idx].field = value` — fused in-place write.
        // The `IndexedFieldSet` op mutates the array and the
        // body's field vec via `Arc::make_mut`, which is O(1)
        // here because `target` is the sole holder of the
        // array's Arc. This is the nbody inner-loop hot path
        // (`bodies[i].vx = ...`).
        if let HirExprKind::Field { receiver, name } = &place.kind {
            if let HirExprKind::Index { base, index } = &receiver.kind {
                if let HirExprKind::Path { segments, .. } = &base.kind {
                    if let Some(first) = segments.first() {
                        if let Some(target) = self.lookup_local(&first.name) {
                            if target.kind == RegKind::Value {
                                let name_idx = self.const_idx(
                                    ConstKey::String(name.name.clone()),
                                    Value::String(SmolStr::from(name.name.clone())),
                                );
                                // Phase-2 typed store: when the RHS is
                                // an f64 expression, write straight
                                // from the float register file into
                                // `base[i].field`, skipping the
                                // `BoxF64` that the generic path
                                // would emit.
                                let value_is_f64 = matches!(
                                    self.tcx.kind(value.ty),
                                    Some(TyKind::Float(FloatTy::F64))
                                );
                                if value_is_f64 {
                                    // Resolve the struct's field
                                    // offset for this write; emit the
                                    // offset-based op when possible.
                                    let elem_ty = self.array_elem_ty(base.ty);
                                    let offset = elem_ty.and_then(|t| {
                                        self.resolve_struct_field_offset(t, name.name.as_str())
                                    });
                                    let idx_reg = self.compile_expr(index)?;
                                    let value_tr = self.compile_expr_ex(value)?;
                                    let value_f = self.as_f64(value_tr);
                                    // Known-flat fast path.
                                    if let (Some(offset), Some(&stride)) =
                                        (offset, self.flat_locals.get(&target.reg))
                                    {
                                        self.emit(Op::FlatSetF64 {
                                            base: target.reg,
                                            index: idx_reg,
                                            stride,
                                            offset,
                                            value_f,
                                        });
                                        return Ok(self.load_unit());
                                    }
                                    if let Some(offset) = offset {
                                        self.emit(Op::IndexedFieldSetF64ByOffset {
                                            base: target.reg,
                                            index: idx_reg,
                                            offset,
                                            value_f,
                                        });
                                    } else {
                                        self.emit(Op::IndexedFieldSetF64 {
                                            base: target.reg,
                                            index: idx_reg,
                                            name_idx,
                                            value_f,
                                        });
                                    }
                                    return Ok(self.load_unit());
                                }
                                let idx_reg = self.compile_expr(index)?;
                                let value_reg = self.compile_expr(value)?;
                                self.emit(Op::IndexedFieldSet {
                                    base: target.reg,
                                    index: idx_reg,
                                    name_idx,
                                    value: value_reg,
                                });
                                return Ok(self.load_unit());
                            }
                        }
                    }
                }
            }
        }
        // Anything more complex (e.g. `a.b.c = x`, indexed
        // assignment through a temporary) still delegates to
        // the tree-walker via a synthetic Assign expression.
        let synthetic = HirExpr {
            id: place.id,
            span: place.span,
            ty: place.ty,
            kind: HirExprKind::Assign {
                place: Box::new(place.clone()),
                value: Box::new(value.clone()),
            },
        };
        self.compile_deferred(&synthetic)
    }
}
