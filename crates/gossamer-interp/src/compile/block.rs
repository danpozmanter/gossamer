#![allow(clippy::too_many_lines, clippy::wildcard_imports)]
use super::*;

impl<'tcx> FnBuilder<'tcx> {
    pub(crate) fn compile_block(&mut self, block: &HirBlock) -> RuntimeResult<BlockResult> {
        self.push_scope();
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
        self.pop_scope();
        Ok(result)
    }
}
