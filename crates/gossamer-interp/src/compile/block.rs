#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn compile_block(&mut self, block: &HirBlock) -> RuntimeResult<BlockResult> {
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
            let reg = self.compile_expr(tail)?;
            BlockResult::ValueIn(reg)
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
