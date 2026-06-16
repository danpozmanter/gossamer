#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Registers a coverage counter slot for `span`'s `(file, line)`
    /// and emits an [`Op::CovHit`] against it. No-op unless the VM
    /// loaded the program with coverage active (`self.cov` set). The
    /// slot is registered at compile time so uncovered lines still
    /// appear in the lcov report with a zero hit count.
    fn emit_coverage_hit(&mut self, span: gossamer_lex::Span) {
        let Some(map) = self.cov else {
            return;
        };
        let line = map.line_col(span.file, span.start).line;
        let file = map.file_name(span.file);
        let slot = gossamer_runtime::coverage::register(file, line, 0);
        let slot = u32::try_from(slot).unwrap_or(u32::MAX);
        self.emit(Op::CovHit { slot });
    }

    /// Tags a freshly-bound register when its initializer constructs a
    /// `flag::Set` or a duration-flag cell. The typechecker leaves both
    /// shapes as unresolved inference vars, so the method-form
    /// `time::Duration` accessors (`cell.as_millis()`) rely on the
    /// `duration_cell_locals` tag rather than the receiver's static type.
    fn record_flag_init(&mut self, init: &HirExpr, reg: Reg) {
        match &init.kind {
            HirExprKind::Call { callee, .. } => {
                if let HirExprKind::Path { segments, .. } = &callee.kind {
                    let n = segments.len();
                    if n >= 2
                        && segments[n - 2].name.as_str() == "Set"
                        && segments[n - 1].name.as_str() == "new"
                    {
                        self.flag_set_locals.insert(reg);
                    }
                }
            }
            HirExprKind::MethodCall { receiver, name, .. } if name.name.as_str() == "duration" => {
                if let HirExprKind::Path { segments, .. } = &receiver.kind
                    && let [seg] = segments.as_slice()
                    && let Some(tr) = self.lookup_local(&seg.name)
                    && self.flag_set_locals.contains(&tr.reg)
                {
                    self.duration_cell_locals.insert(reg);
                }
            }
            _ => {}
        }
    }

    /// `true` when `receiver` is a path bound to a `flag::Set` duration
    /// cell, so a `time::Duration` accessor in method form dispatches on
    /// the cell's element type.
    pub(crate) fn receiver_is_duration_cell(&self, receiver: &HirExpr) -> bool {
        if let HirExprKind::Path { segments, .. } = &receiver.kind
            && let [seg] = segments.as_slice()
            && let Some(tr) = self.lookup_local(&seg.name)
        {
            return self.duration_cell_locals.contains(&tr.reg);
        }
        false
    }

    /// Returns `true` when the statement diverges (return/break/continue).
    pub(crate) fn compile_stmt(&mut self, stmt: &HirStmt) -> RuntimeResult<bool> {
        self.emit_coverage_hit(stmt.span);
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
                        self.record_flag_init(init, typed.reg);
                        self.bind_local(&name.name, typed);
                    } else {
                        // Declared-only - default to Value; an
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
                    // Destructuring - compile init to a Value reg,
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
                // A bare assignment statement throws its `()` result away;
                // compile the store directly so no `LoadConst(Unit)` is
                // emitted for a value nothing reads.
                if let HirExprKind::Assign { place, value } = &expr.kind {
                    self.compile_assign_store(place, value)?;
                } else {
                    let _ = self.compile_expr(expr)?;
                }
                Ok(expr_diverges(expr))
            }
            HirStmtKind::Go(expr) => {
                // Native goroutine spawn: compile the callee and
                // args directly into VM ops and emit `Op::Spawn`.
                // The dispatcher creates a fresh `Vm` on the new
                // thread that executes the call entirely in bytecode.
                if self.try_compile_go_native(expr)? {
                    return Ok(false);
                }
                // Non-call shapes (`go { block }`, `go` over a bare
                // expression) lift the spawned expression into a
                // zero-arg closure and spawn that closure natively.
                self.compile_non_call_go(expr)?;
                Ok(false)
            }
            HirStmtKind::Defer(expr) => {
                // Register for block-scoped execution: emitted LIFO when
                // control leaves the enclosing block by any path.
                // `compile_block` emits the frame on normal fall-through;
                // `return` / `break` / `continue` emit the pending frames
                // before their jump. A `defer` never diverges the enclosing
                // statement sequence.
                if let Some(frame) = self.defer_stack.last_mut() {
                    frame.push(expr.clone());
                }
                Ok(false)
            }
            HirStmtKind::Item(_) => Err(RuntimeError::Unsupported("nested items")),
        }
    }

    /// Compiles `place = value` in expression position, yielding the
    /// `()` result in a register so callers that consume it (a block
    /// tail, `let x = (a = b)`) see a real value.
    pub(crate) fn compile_assign(
        &mut self,
        place: &HirExpr,
        value: &HirExpr,
    ) -> RuntimeResult<Reg> {
        self.compile_assign_store(place, value)?;
        Ok(self.load_unit())
    }

    /// Emits the store(s) for `place = value` and stops there. The unit
    /// such an expression yields is dead in statement position, so the
    /// statement path calls this to avoid one `LoadConst(Unit)` per
    /// assignment. Sub-expressions still compile normally, so a nested
    /// `a = (b = c)` materialises its inner unit through the wrapper.
    pub(crate) fn compile_assign_store(
        &mut self,
        place: &HirExpr,
        value: &HirExpr,
    ) -> RuntimeResult<()> {
        if let HirExprKind::Path { segments, .. } = &place.kind {
            if let Some(first) = segments.first() {
                if let Some(target) = self.lookup_local(&first.name) {
                    // Typed-local reassignment: compile the
                    // RHS in the destination kind so no box /
                    // unbox round-trip happens in hot loops.
                    let src_tr = self.compile_expr_ex(value)?;
                    self.emit_move_into(target, src_tr);
                    return Ok(());
                }
            }
        }
        // Native `local.field = value` and `local[i] = value`
        // writes: emit FieldSet / IndexSet directly so the VM's
        // hot loops (nbody's body.vx / body.x updates) stay on the
        // fast path.
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
                            return Ok(());
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
                                return Ok(());
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
                                return Ok(());
                            }
                            let idx_reg = self.compile_expr(index)?;
                            let value_reg = self.compile_expr(value)?;
                            self.emit(Op::IndexSet {
                                base: target.reg,
                                index: idx_reg,
                                value: value_reg,
                            });
                            return Ok(());
                        }
                    }
                }
            }
        }
        // `local[idx].field = value` - fused in-place write.
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
                                    // Compile the index in its native
                                    // register file: an int-bank index
                                    // (loop counter) feeds the flat write
                                    // directly, skipping the per-access
                                    // `BoxI64`.
                                    let idx_tr = self.compile_expr_ex(index)?;
                                    let value_tr = self.compile_expr_ex(value)?;
                                    let value_f = self.as_f64(value_tr);
                                    // Known-flat fast path.
                                    if let (Some(offset), Some(&stride)) =
                                        (offset, self.flat_locals.get(&target.reg))
                                    {
                                        if idx_tr.kind == RegKind::I64 {
                                            self.emit(Op::FlatSetF64I {
                                                base: target.reg,
                                                index_i: idx_tr.reg,
                                                stride,
                                                offset,
                                                value_f,
                                            });
                                        } else {
                                            let idx_reg = self.as_value(idx_tr);
                                            self.emit(Op::FlatSetF64 {
                                                base: target.reg,
                                                index: idx_reg,
                                                stride,
                                                offset,
                                                value_f,
                                            });
                                        }
                                        return Ok(());
                                    }
                                    let idx_reg = self.as_value(idx_tr);
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
                                    return Ok(());
                                }
                                let idx_reg = self.compile_expr(index)?;
                                let value_reg = self.compile_expr(value)?;
                                self.emit(Op::IndexedFieldSet {
                                    base: target.reg,
                                    index: idx_reg,
                                    name_idx,
                                    value: value_reg,
                                });
                                return Ok(());
                            }
                        }
                    }
                }
            }
        }
        // Anything more complex (`a.b.c = x`, `grid[i][j] = x`, `*p = v`,
        // assignment through a temporary) lowers natively via the
        // recursive place store. A local-rooted place bottoms out in a
        // register move; a `static mut`-rooted place (`COUNTER = …`,
        // `STATIC.field = …`) bottoms out in an `Op::StoreStatic` against
        // the shared `Global::MutStatic` cell. Those are the only
        // assignable place roots - a `const` or immutable `static` is
        // rejected by the typechecker before it reaches here.
        if self.place_root_is_local(place) || self.place_root_is_mut_static(place) {
            let value_reg = self.compile_expr(value)?;
            return self.compile_place_store(place, value_reg);
        }
        Err(RuntimeError::Unsupported(
            "assignment to a place that is neither a local nor a mutable static",
        ))
    }

    /// `true` when the lvalue `place` is rooted at a bound local - a
    /// chain of field / index / deref projections bottoming out at a
    /// local binding. A static-rooted place returns `false`.
    pub(crate) fn place_root_is_local(&self, place: &HirExpr) -> bool {
        match &place.kind {
            HirExprKind::Path { segments, .. } => segments
                .first()
                .is_some_and(|s| self.lookup_local(&s.name).is_some()),
            HirExprKind::Field { receiver, .. } => self.place_root_is_local(receiver),
            HirExprKind::Index { base, .. } => self.place_root_is_local(base),
            HirExprKind::Unary {
                op: HirUnaryOp::Deref,
                operand,
            } => self.place_root_is_local(operand),
            _ => false,
        }
    }

    /// `true` when the lvalue `place` is rooted at a `static mut` - the
    /// static side of [`Self::place_root_is_local`]. A field / index /
    /// deref chain bottoming out at a known mutable static matches.
    fn place_root_is_mut_static(&self, place: &HirExpr) -> bool {
        match &place.kind {
            HirExprKind::Path { segments, .. } => self.mut_static_global_name(segments).is_some(),
            HirExprKind::Field { receiver, .. } => self.place_root_is_mut_static(receiver),
            HirExprKind::Index { base, .. } => self.place_root_is_mut_static(base),
            HirExprKind::Unary {
                op: HirUnaryOp::Deref,
                operand,
            } => self.place_root_is_mut_static(operand),
            _ => false,
        }
    }

    /// Resolves `segments` to the global name of a `static mut`, or
    /// `None` when the path isn't one. A single-segment same-module
    /// reference (`COUNTER`) and a qualified reference (`mod::COUNTER`)
    /// both match when the final segment names a known mutable static and
    /// the first segment isn't a local shadowing it. The returned name is
    /// the `::`-joined path, under which the cell is registered in the
    /// global table.
    fn mut_static_global_name(&self, segments: &[Ident]) -> Option<String> {
        // `super::COUNTER` / `crate::COUNTER` inside an inline module
        // addresses the flat cell registered under the unqualified or
        // module-joined name, so strip the relative prefix first.
        let segments = strip_module_relative(segments);
        let first = segments.first()?;
        if self.lookup_local(&first.name).is_some() {
            return None;
        }
        let last = segments.last()?;
        if !self.mut_statics.contains(last.name.as_str()) {
            return None;
        }
        Some(
            segments
                .iter()
                .map(|s| s.name.as_str())
                .collect::<Vec<_>>()
                .join("::"),
        )
    }

    /// Stores `value_reg` into the lvalue `place`, recursing up a
    /// field / index / deref chain. Each step reads the receiver, mutates
    /// the leaf in place (`FieldSet` / `IndexSet`, which clone-on-write
    /// through `Arc::make_mut`), then writes the updated receiver back to
    /// its own place so value-aggregate aliasing semantics match the
    /// compiled tiers.
    pub(crate) fn compile_place_store(
        &mut self,
        place: &HirExpr,
        value_reg: Reg,
    ) -> RuntimeResult<()> {
        match &place.kind {
            HirExprKind::Path { segments, .. } => {
                let Some(first) = segments.first() else {
                    return Err(RuntimeError::Unsupported("assignment to empty path"));
                };
                if let Some(target) = self.lookup_local(&first.name) {
                    self.emit_move_into(
                        target,
                        TypedReg {
                            reg: value_reg,
                            kind: RegKind::Value,
                        },
                    );
                    return Ok(());
                }
                // `static mut` root: publish into the shared global cell.
                let Some(name) = self.mut_static_global_name(segments) else {
                    return Err(RuntimeError::UnresolvedName(first.name.clone()));
                };
                let idx = self.global_idx(&name);
                self.emit(Op::StoreStatic {
                    name_idx: idx,
                    src: value_reg,
                });
                Ok(())
            }
            HirExprKind::Field { receiver, name } => {
                let recv_reg = self.compile_expr(receiver)?;
                let name_idx = self.const_idx(
                    ConstKey::String(name.name.clone()),
                    Value::String(SmolStr::from(name.name.clone())),
                );
                self.emit(Op::FieldSet {
                    receiver: recv_reg,
                    name_idx,
                    value: value_reg,
                });
                self.compile_place_store(receiver, recv_reg)
            }
            HirExprKind::Index { base, index } => {
                let base_reg = self.compile_expr(base)?;
                let idx_reg = self.compile_expr(index)?;
                self.emit(Op::IndexSet {
                    base: base_reg,
                    index: idx_reg,
                    value: value_reg,
                });
                self.compile_place_store(base, base_reg)
            }
            HirExprKind::Unary {
                op: HirUnaryOp::Deref,
                operand,
            } => self.compile_place_store(operand, value_reg),
            _ => Err(RuntimeError::Unsupported(
                "assignment to non-place expression",
            )),
        }
    }
}
