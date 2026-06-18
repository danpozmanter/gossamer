#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn compile_block(&mut self, block: &HirBlock) -> RuntimeResult<BlockResult> {
        self.compile_block_inner(block, false)
    }

    /// Compiles a block whose tail value is discarded (a loop body or a
    /// statement-position block). The tail is compiled in statement
    /// context, so a discardable in-place mutation (`v.push(x)`) lowers
    /// to its dedicated op rather than the value-returning builtin path.
    pub(crate) fn compile_loop_body(&mut self, body: &HirExpr) -> RuntimeResult<()> {
        if let HirExprKind::Block(block) = &body.kind {
            self.compile_block_inner(block, true)?;
        } else {
            let _ = self.compile_expr(body)?;
        }
        Ok(())
    }

    fn compile_block_inner(
        &mut self,
        block: &HirBlock,
        tail_discarded: bool,
    ) -> RuntimeResult<BlockResult> {
        self.push_scope();
        self.defer_stack.push(Vec::new());
        let mut diverges = false;
        for stmt in &block.stmts {
            if self.compile_stmt(stmt)? {
                diverges = true;
            }
        }
        let result = if diverges {
            BlockResult::Diverges
        } else if let Some(tail) = &block.tail {
            // A discarded tail is compiled in statement context: an
            // in-place Vec mutation lowers to its dedicated op (matching
            // a `v.push(x)` written with a trailing newline elsewhere).
            if tail_discarded
                && let HirExprKind::MethodCall {
                    receiver,
                    name,
                    args,
                } = &tail.kind
                && self.try_compile_inplace_vec_stmt(receiver, name, args)?
            {
                BlockResult::Unit
            } else {
                let reg = self.compile_expr(tail)?;
                BlockResult::ValueIn(reg)
            }
        } else {
            BlockResult::Unit
        };
        // Block-scoped `defer`: on a normal (non-diverging) exit, run this
        // block's deferred expressions LIFO after its value is computed. A
        // diverging block already emitted the pending frames at the
        // `return` / `break` / `continue` edge via `emit_defers_above`, so
        // it must not re-emit here. Deferred expressions allocate fresh
        // registers, so the result register stays intact.
        let frame = self.defer_stack.pop().unwrap_or_default();
        if !diverges {
            self.emit_defer_frame(&frame)?;
        }
        self.pop_scope();
        Ok(result)
    }
}
