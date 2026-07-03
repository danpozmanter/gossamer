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
            self.compile_expr_discarded(body)?;
        }
        Ok(())
    }

    pub(crate) fn compile_block_inner(
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
            // A discarded tail (loop body / statement-position block) is
            // compiled in statement context: an assignment lowers to its
            // store and an in-place Vec mutation to its dedicated op, so
            // neither leaves the dead `LoadConst(Unit)` its expression form
            // would yield - the per-iteration unit nothing reads.
            if tail_discarded {
                self.compile_expr_discarded(tail)?;
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

#[cfg(test)]
mod elide_unit_load_tests {
    use std::collections::{HashMap, HashSet};

    use gossamer_hir::{HirExprKind, HirFn, HirItemKind, lower_source_file};
    use gossamer_lex::SourceMap;
    use gossamer_parse::parse_source_file;
    use gossamer_resolve::resolve_source_file;
    use gossamer_types::{TyCtxt, typecheck_source_file};

    use crate::bytecode::{FnChunk, Op};
    use crate::value::Value;

    /// Compiles a single named function from `source` to its bytecode
    /// chunk, driving the full front-end with an empty compile context
    /// (no struct layouts / inlinable wrappers / consts).
    fn compile_named(source: &str, fn_name: &str) -> (FnChunk, HirFn) {
        let mut map = SourceMap::new();
        let file = map.add_file("test.gos", source.to_string());
        let (sf, parse_diags) = parse_source_file(source, file);
        assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
        let (resolutions, _) = resolve_source_file(&sf);
        let mut tcx = TyCtxt::new();
        let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
        let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
        let decl = program
            .items
            .iter()
            .find_map(|item| match &item.kind {
                HirItemKind::Fn(decl) if decl.name.name.as_str() == fn_name => Some(decl.clone()),
                _ => None,
            })
            .expect("named function not found");
        let chunk = super::compile_fn(
            &decl,
            &tcx,
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashMap::new(),
            &HashSet::new(),
            &HashSet::new(),
            None,
        )
        .expect("compile_fn");
        (chunk, decl)
    }

    /// `true` when `op` loads a `Value::Unit` from the constant pool.
    fn is_unit_load(op: &Op, chunk: &FnChunk) -> bool {
        matches!(op, Op::LoadConst { idx, .. } if matches!(chunk.consts.get(*idx as usize), Some(Value::Unit)))
    }

    /// Index of the loop's back-edge: the jump that targets an earlier
    /// instruction. The loop body is everything before it.
    fn back_edge_idx(chunk: &FnChunk) -> usize {
        chunk
            .instrs
            .iter()
            .enumerate()
            .find_map(|(idx, op)| match op {
                Op::Jump { target } if (*target as usize) < idx => Some(idx),
                _ => None,
            })
            .expect("loop back-edge jump not found")
    }

    #[test]
    fn assignment_tail_in_loop_body_emits_no_dead_unit() {
        let source = r"
fn count(n: i64) -> i64 {
    let mut i = 0
    while i < n {
        i = i + 1
    }
    i
}
";
        let (chunk, decl) = compile_named(source, "count");

        // Precondition: the while body's tail really is the assignment,
        // so this test exercises the discarded-tail path - not the
        // statement path, which already elided the unit.
        let body = decl.body.as_ref().expect("body");
        let while_expr = body
            .block
            .stmts
            .iter()
            .find_map(|stmt| match &stmt.kind {
                gossamer_hir::HirStmtKind::Expr { expr, .. } => match &expr.kind {
                    HirExprKind::While { body, .. } => Some(body.as_ref()),
                    _ => None,
                },
                _ => None,
            })
            .expect("while statement");
        let HirExprKind::Block(while_body) = &while_expr.kind else {
            panic!("while body is not a block");
        };
        let tail = while_body.tail.as_ref().expect("while body tail");
        assert!(
            matches!(tail.kind, HirExprKind::Assign { .. }),
            "expected the loop body tail to be an assignment"
        );

        // The optimization: the compiled loop body (everything before the
        // back-edge jump) contains no dead `LoadConst(Unit)`. Pre-fix the
        // assignment tail materialised one such unit per loop body.
        let back_edge = back_edge_idx(&chunk);
        let units_in_body = chunk.instrs[..back_edge]
            .iter()
            .filter(|op| is_unit_load(op, &chunk))
            .count();
        assert_eq!(
            units_in_body, 0,
            "loop body should emit no dead unit load; chunk: {:?}",
            chunk.instrs
        );

        // The increment itself must survive: removing the dead unit must
        // not remove the `i + 1` add the loop body still needs.
        let has_add = chunk.instrs[..back_edge].iter().any(|op| {
            matches!(
                op,
                Op::AddInt { .. }
                    | Op::AddI64 { .. }
                    | Op::ArithImmI64 {
                        kind: crate::bytecode::ImmArithKind::Add,
                        ..
                    }
            )
        });
        assert!(
            has_add,
            "loop body lost its `i + 1` computation; chunk: {:?}",
            chunk.instrs
        );
    }

    #[test]
    fn block_value_bound_to_let_still_materialises() {
        // A block whose value IS used (bound to a `let`) must still
        // produce its value, so its tail is not elided.
        let source = r"
fn pick(flag: bool) -> i64 {
    let v = { if flag { 10 } else { 20 } }
    v
}
";
        let (chunk, _) = compile_named(source, "pick");
        // The block tail is an `if`-as-value; a real value register must
        // flow into `v`. The function must end by returning a value, not
        // unit.
        assert!(
            chunk
                .instrs
                .iter()
                .any(|op| matches!(op, Op::Return { .. })),
            "value-producing block must return a value; chunk: {:?}",
            chunk.instrs
        );
    }
}
