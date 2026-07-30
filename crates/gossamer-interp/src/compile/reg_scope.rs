#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    /// Reserves a fresh inline-cache slot index for the current
    /// dispatch site and returns it. The matching slot is allocated
    /// by `finish` once the total count is known.
    pub(crate) fn alloc_cache_idx(&mut self) -> u16 {
        let idx = self.next_cache_idx;
        self.next_cache_idx = self.next_cache_idx.saturating_add(1);
        idx
    }

    /// Allocates a fresh `field_caches` slot for an
    /// `Op::FieldGet` site. Mirrors [`Self::alloc_cache_idx`]
    /// but for the PEP 659-style field-shape cache.
    pub(crate) fn alloc_field_cache_idx(&mut self) -> u16 {
        let idx = self.next_field_cache_idx;
        self.next_field_cache_idx = self.next_field_cache_idx.saturating_add(1);
        idx
    }

    pub(crate) fn alloc_reg(&mut self) -> Reg {
        let r = self.next_reg;
        self.next_reg = self.next_reg.checked_add(1).expect("register overflow");
        self.max_reg = self.max_reg.max(self.next_reg);
        r
    }

    pub(crate) fn alloc_float(&mut self) -> Reg {
        let r = self.next_float_reg;
        self.next_float_reg = self
            .next_float_reg
            .checked_add(1)
            .expect("float register overflow");
        self.max_float_reg = self.max_float_reg.max(self.next_float_reg);
        r
    }

    pub(crate) fn alloc_int(&mut self) -> Reg {
        let r = self.next_int_reg;
        self.next_int_reg = self
            .next_int_reg
            .checked_add(1)
            .expect("int register overflow");
        self.max_int_reg = self.max_int_reg.max(self.next_int_reg);
        r
    }

    /// Captures the three physical register cursors before compiling a region
    /// whose results cannot escape. Restoring the mark lets the next disjoint
    /// region reuse the same slots. High-water counts remain monotonic, so a
    /// chunk is always sized for every register referenced by its bytecode.
    pub(crate) fn register_mark(&self) -> (Reg, Reg, Reg) {
        (self.next_reg, self.next_float_reg, self.next_int_reg)
    }

    pub(crate) fn restore_register_mark(&mut self, mark: (Reg, Reg, Reg)) {
        self.max_reg = self.max_reg.max(self.next_reg);
        self.max_float_reg = self.max_float_reg.max(self.next_float_reg);
        self.max_int_reg = self.max_int_reg.max(self.next_int_reg);
        self.next_reg = mark.0.max(self.escaped_reference_reg_floor);
        self.next_float_reg = mark.1;
        self.next_int_reg = mark.2;
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Compiles a defer frame's expressions for their side effects in
    /// LIFO (reverse-registration) order. Emitted at every edge that
    /// leaves the frame's block - normal fall-through, `return`,
    /// `break`, `continue`. Each expression's result register is
    /// discarded: a `defer` body yields no value and never redirects
    /// control flow out of the deferred expression.
    pub(crate) fn emit_defer_frame(&mut self, frame: &[HirExpr]) -> RuntimeResult<()> {
        for expr in frame.iter().rev() {
            let _ = self.compile_expr(expr)?;
        }
        Ok(())
    }

    /// Emits every defer frame at stack index `>= from_depth`, innermost
    /// block first, without removing them - each owning `compile_block`
    /// pops its own frame as control unwinds. `return` passes `0` (all
    /// frames); `break` / `continue` pass the target loop's `defer_depth`
    /// (only the frames nested inside the loop body). Mirrors
    /// `gossamer-mir`'s `emit_defers_above`.
    pub(crate) fn emit_defers_above(&mut self, from_depth: usize) -> RuntimeResult<()> {
        for i in (from_depth..self.defer_stack.len()).rev() {
            let frame = self.defer_stack[i].clone();
            self.emit_defer_frame(&frame)?;
        }
        Ok(())
    }

    pub(crate) fn bind_local(&mut self, name: &str, typed: TypedReg) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), typed);
        }
    }

    pub(crate) fn bind_reference_local(&mut self, name: &str, typed: TypedReg) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.locals.insert(name.to_string(), typed);
            scope.reference_bindings.insert(name.to_string());
        }
    }

    pub(crate) fn is_reference_binding(&self, name: &str) -> bool {
        for scope in self.scopes.iter().rev() {
            if scope.locals.contains_key(name) {
                return scope.reference_bindings.contains(name);
            }
        }
        false
    }

    pub(crate) fn rebind_reference_local(&mut self, name: &str, typed: TypedReg) -> bool {
        for scope in self.scopes.iter_mut().rev() {
            if scope.locals.contains_key(name) {
                if scope.reference_bindings.contains(name) {
                    scope.locals.insert(name.to_string(), typed);
                    return true;
                }
                return false;
            }
        }
        false
    }

    pub(crate) fn lookup_local(&self, name: &str) -> Option<TypedReg> {
        for scope in self.scopes.iter().rev() {
            if let Some(typed) = scope.locals.get(name) {
                return Some(*typed);
            }
        }
        None
    }

    /// Coerces a typed-reg into the `Value` register file,
    /// emitting `BoxF64` / `BoxI64` as required.
    pub(crate) fn as_value(&mut self, tr: TypedReg) -> Reg {
        match tr.kind {
            RegKind::Value => tr.reg,
            RegKind::F64 => {
                let dst = self.alloc_reg();
                self.emit(Op::BoxF64 {
                    dst_v: dst,
                    src_f: tr.reg,
                });
                dst
            }
            RegKind::I64 => {
                let dst = self.alloc_reg();
                self.emit(Op::BoxI64 {
                    dst_v: dst,
                    src_i: tr.reg,
                });
                dst
            }
        }
    }

    /// Coerces a typed-reg into the float register file.
    pub(crate) fn as_f64(&mut self, tr: TypedReg) -> Reg {
        self.as_f64_with_peer(tr, None)
    }

    /// Coerces a typed-reg into the float register file and, for binary
    /// operations, retains the peer value for a complete type diagnostic.
    pub(crate) fn as_f64_with_peer(&mut self, tr: TypedReg, peer_v: Option<Reg>) -> Reg {
        if tr.kind == RegKind::F64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_float();
            self.emit(Op::UnboxF64 {
                dst_f: dst,
                src_v: v,
                peer_v,
            });
            dst
        }
    }

    /// Coerces a typed-reg into the int register file.
    pub(crate) fn as_i64(&mut self, tr: TypedReg) -> Reg {
        self.as_i64_with_peer(tr, None)
    }

    /// Coerces a typed-reg into the integer register file and, for binary
    /// operations, retains the peer value for a complete type diagnostic.
    pub(crate) fn as_i64_with_peer(&mut self, tr: TypedReg, peer_v: Option<Reg>) -> Reg {
        if tr.kind == RegKind::I64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_int();
            self.emit(Op::UnboxI64 {
                dst_i: dst,
                src_v: v,
                peer_v,
            });
            dst
        }
    }

    /// Allocates a fresh register of `tr`'s kind and emits the
    /// appropriate kind-specific move. Used by `let` bindings
    /// so subsequent reassignments can always target the
    /// local's fixed slot.
    pub(crate) fn bind_to_fresh(&mut self, tr: TypedReg) -> TypedReg {
        match tr.kind {
            RegKind::Value => {
                let dst = self.alloc_reg();
                self.emit(Op::Move { dst, src: tr.reg });
                TypedReg {
                    reg: dst,
                    kind: RegKind::Value,
                }
            }
            RegKind::F64 => {
                let dst = self.alloc_float();
                self.emit(Op::MoveF64 {
                    dst_f: dst,
                    src_f: tr.reg,
                });
                TypedReg {
                    reg: dst,
                    kind: RegKind::F64,
                }
            }
            RegKind::I64 => {
                let dst = self.alloc_int();
                self.emit(Op::MoveI64 {
                    dst_i: dst,
                    src_i: tr.reg,
                });
                TypedReg {
                    reg: dst,
                    kind: RegKind::I64,
                }
            }
        }
    }

    /// Moves a typed source into an existing destination
    /// register of the same kind. Used by `x = expr`
    /// reassignments so the local's slot stays put.
    pub(crate) fn emit_move_into(&mut self, dst: TypedReg, src: TypedReg) {
        match dst.kind {
            RegKind::Value => {
                let src_v = self.as_value(src);
                self.emit(Op::Move {
                    dst: dst.reg,
                    src: src_v,
                });
            }
            RegKind::F64 => {
                let src_f = self.as_f64(src);
                self.emit(Op::MoveF64 {
                    dst_f: dst.reg,
                    src_f,
                });
            }
            RegKind::I64 => {
                let src_i = self.as_i64(src);
                self.emit(Op::MoveI64 {
                    dst_i: dst.reg,
                    src_i,
                });
            }
        }
    }

    /// Folds the `MoveI64` an i64 local reassignment would pay when the
    /// RHS result was produced by the immediately-preceding typed-i64
    /// arith op, redirecting that op's destination into the local's slot.
    /// Sound only for a straight-line RHS: typed arith always writes a
    /// fresh scratch register (so the elided temp has no other reader),
    /// and requiring the `[rhs_start, here)` window to be free of
    /// control-flow ops guarantees nothing can jump to the elided move's
    /// slot (an in-flight patch to a mid-statement index can only be
    /// created by the RHS's own branches).
    pub(crate) fn try_fold_i64_move(
        &mut self,
        rhs_start: InstrIdx,
        src_i: Reg,
        new_dst: Reg,
    ) -> bool {
        let here = self.cur_idx();
        if here <= rhs_start {
            return false;
        }
        let window = &self.instrs[rhs_start as usize..here as usize];
        if window.iter().any(|op| {
            matches!(
                op,
                Op::Jump { .. }
                    | Op::BranchIf { .. }
                    | Op::BranchIfNot { .. }
                    | Op::BranchIfLtI64 { .. }
                    | Op::BranchIfGeI64 { .. }
                    | Op::BranchIfGtI64 { .. }
                    | Op::BranchIfLtF64 { .. }
                    | Op::BranchIfGeF64 { .. }
                    | Op::IncJumpIfLtI64 { .. }
                    | Op::IncJumpIfLeI64 { .. }
                    | Op::Select { .. }
            )
        }) {
            return false;
        }
        match self.instrs.last_mut() {
            Some(
                Op::AddI64 { dst_i, .. }
                | Op::SubI64 { dst_i, .. }
                | Op::MulI64 { dst_i, .. }
                | Op::DivI64 { dst_i, .. }
                | Op::RemI64 { dst_i, .. }
                | Op::DivU64 { dst_i, .. }
                | Op::RemU64 { dst_i, .. }
                | Op::ArithImmI64 { dst_i, .. },
            ) if *dst_i == src_i => {
                *dst_i = new_dst;
                true
            }
            _ => false,
        }
    }
}
