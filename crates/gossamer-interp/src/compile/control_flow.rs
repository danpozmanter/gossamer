#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn compile_if(
        &mut self,
        condition: &HirExpr,
        then_branch: &HirExpr,
        else_branch: Option<&HirExpr>,
    ) -> RuntimeResult<Reg> {
        let cond_reg = self.compile_expr(condition)?;
        let result = self.alloc_reg();
        let branch_idx = self.emit(Op::BranchIfNot {
            cond: cond_reg,
            target: 0,
        });
        let then_reg = self.compile_expr(then_branch)?;
        self.emit(Op::Move {
            dst: result,
            src: then_reg,
        });
        let jump_end = self.emit(Op::Jump { target: 0 });
        let else_start = self.cur_idx();
        self.patch_jump(branch_idx, else_start);
        if let Some(else_branch) = else_branch {
            let else_reg = self.compile_expr(else_branch)?;
            self.emit(Op::Move {
                dst: result,
                src: else_reg,
            });
        } else {
            let unit_reg = self.load_unit();
            self.emit(Op::Move {
                dst: result,
                src: unit_reg,
            });
        }
        let after = self.cur_idx();
        self.patch_jump(jump_end, after);
        Ok(result)
    }

    pub(crate) fn compile_while(
        &mut self,
        condition: &HirExpr,
        body: &HirExpr,
    ) -> RuntimeResult<Reg> {
        // Fused-branch fast path: `while lhs < rhs` / `while
        // lhs >= rhs` etc. on typed i64 / f64 operands gets
        // lowered to a single `BranchIfGeI64` / `BranchIfLtI64`
        // that pairs the comparison with the exit jump. Cuts
        // two dispatched ops (typed compare + BranchIfNot /
        // BranchIf) down to one per loop iteration.
        //
        // Loop-invariant literal operands get hoisted above
        // `loop_start` so the LoadConst ops don't re-execute
        // per iteration.
        let label = self.pending_loop_label.take();
        let hoisted = self.try_hoist_condition_literals(condition)?;
        let loop_start = self.cur_idx();
        let exit_patch = if let Some((lhs_reg, rhs_reg, op, kind)) = hoisted {
            Some(self.emit_fused_exit_branch(op, kind, lhs_reg, rhs_reg))
        } else {
            self.try_compile_fused_exit_branch(condition)?
        }
        .unwrap_or_else(|| {
            let cond_reg = self.compile_expr(condition).unwrap_or(0);
            self.emit(Op::BranchIfNot {
                cond: cond_reg,
                target: 0,
            })
        });
        let result = self.alloc_reg();
        self.loop_stack.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            result_reg: result,
            defer_depth: self.defer_stack.len(),
            label,
        });
        self.compile_loop_body(body)?;
        self.emit(Op::Jump { target: loop_start });
        let after = self.cur_idx();
        self.patch_jump(exit_patch, after);
        let ctx = self
            .loop_stack
            .pop()
            .expect("loop stack underflow on while");
        for patch in ctx.break_patches {
            self.patch_jump(patch, after);
        }
        // `continue` in a `while` loop re-evaluates the condition,
        // so route every recorded patch back to `loop_start` -
        // identical semantics to the previous direct-jump form.
        for patch in ctx.continue_patches {
            self.patch_jump(patch, loop_start);
        }
        Ok(self.load_unit())
    }

    pub(crate) fn compile_loop(&mut self, body: &HirExpr) -> RuntimeResult<Reg> {
        let label = self.pending_loop_label.take();
        let loop_start = self.cur_idx();
        let result = self.alloc_reg();
        self.loop_stack.push(LoopCtx {
            break_patches: Vec::new(),
            continue_patches: Vec::new(),
            result_reg: result,
            defer_depth: self.defer_stack.len(),
            label,
        });
        self.compile_loop_body(body)?;
        self.emit(Op::Jump { target: loop_start });
        let after = self.cur_idx();
        let ctx = self.loop_stack.pop().expect("loop stack underflow on loop");
        for patch in ctx.break_patches {
            self.patch_jump(patch, after);
        }
        // Bare `loop` re-enters at the body's first op, so
        // `continue` routes back to `loop_start`.
        for patch in ctx.continue_patches {
            self.patch_jump(patch, loop_start);
        }
        Ok(result)
    }

    pub(crate) fn compile_return(&mut self, value: Option<&HirExpr>) -> RuntimeResult<Reg> {
        let reg = match value {
            Some(value) => self.compile_expr(value)?,
            None => self.load_unit(),
        };
        // `return` leaves every enclosing block: run all pending defer
        // frames (LIFO, innermost first) after the return value is
        // computed, before the actual Return. The HIR desugars `?` into a
        // `match` with an early `return Err(...)`, so this also runs the
        // defers above the `?` site before the error propagates.
        self.emit_defers_above(0)?;
        self.emit(Op::Return { value: reg });
        Ok(reg)
    }

    pub(crate) fn compile_break(
        &mut self,
        value: Option<&HirExpr>,
        label: Option<&str>,
    ) -> RuntimeResult<Reg> {
        let reg = match value {
            Some(value) => self.compile_expr(value)?,
            None => self.load_unit(),
        };
        let idx = self
            .resolve_loop_target(label)
            .ok_or(RuntimeError::Unsupported("break outside of loop"))?;
        let (result_reg, defer_depth) = {
            let ctx = &self.loop_stack[idx];
            (ctx.result_reg, ctx.defer_depth)
        };
        self.emit(Op::Move {
            dst: result_reg,
            src: reg,
        });
        // Run the defers of the blocks being exited (the loop body and any
        // nested blocks), but not the loop's enclosing frames.
        self.emit_defers_above(defer_depth)?;
        let patch = self.emit(Op::Jump { target: 0 });
        self.loop_stack[idx].break_patches.push(patch);
        Ok(reg)
    }

    /// Index into `loop_stack` of the loop a `break`/`continue` targets:
    /// the innermost loop carrying a matching label, or the innermost
    /// loop of any label when no label is given.
    pub(crate) fn resolve_loop_target(&self, label: Option<&str>) -> Option<usize> {
        match label {
            None => self.loop_stack.len().checked_sub(1),
            Some(name) => self
                .loop_stack
                .iter()
                .rposition(|ctx| ctx.label.as_deref() == Some(name)),
        }
    }
}
