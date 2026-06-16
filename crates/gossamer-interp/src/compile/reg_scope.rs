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
        r
    }

    pub(crate) fn alloc_float(&mut self) -> Reg {
        let r = self.next_float_reg;
        self.next_float_reg = self
            .next_float_reg
            .checked_add(1)
            .expect("float register overflow");
        r
    }

    pub(crate) fn alloc_int(&mut self) -> Reg {
        let r = self.next_int_reg;
        self.next_int_reg = self
            .next_int_reg
            .checked_add(1)
            .expect("int register overflow");
        r
    }

    pub(crate) fn push_scope(&mut self) {
        self.scopes.push(Scope::default());
    }

    pub(crate) fn pop_scope(&mut self) {
        self.scopes.pop();
    }

    /// Compiles a defer frame's expressions for their side effects in
    /// LIFO (reverse-registration) order. Emitted at every edge that
    /// leaves the frame's block — normal fall-through, `return`,
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
    /// block first, without removing them — each owning `compile_block`
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
        if tr.kind == RegKind::F64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_float();
            self.emit(Op::UnboxF64 {
                dst_f: dst,
                src_v: v,
            });
            dst
        }
    }

    /// Coerces a typed-reg into the int register file.
    pub(crate) fn as_i64(&mut self, tr: TypedReg) -> Reg {
        if tr.kind == RegKind::I64 {
            tr.reg
        } else {
            let v = self.as_value(tr);
            let dst = self.alloc_int();
            self.emit(Op::UnboxI64 {
                dst_i: dst,
                src_v: v,
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
}
