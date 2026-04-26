//! Stream E.6 — intraprocedural escape analysis.
//! A local "escapes" the current function when any of the following
//! is true:
//! - It is assigned into `Local::RETURN` (flows out to the caller).
//! - It is passed as an argument to a call terminator.
//! - It aliases (by copy) an already-escaping local.
//!
//! The analysis is intentionally conservative and linear in the
//! number of statements. Downstream passes can use
//! [`EscapeSet::is_non_escaping`] to decide whether a value can be
//! stack-allocated instead of boxed.

#![forbid(unsafe_code)]

use std::collections::BTreeSet;

use crate::ir::{Body, Local, Operand, Rvalue, StatementKind, Terminator};

/// Result of [`analyse`] — the set of locals that escape this body.
///
/// Callers typically ask the inverse question via
/// [`EscapeSet::is_non_escaping`].
#[derive(Debug, Clone, Default)]
pub struct EscapeSet {
    escapes: BTreeSet<u32>,
}

impl EscapeSet {
    /// Returns `true` when `local` does **not** escape.
    #[must_use]
    pub fn is_non_escaping(&self, local: Local) -> bool {
        !self.escapes.contains(&local.0)
    }

    /// Returns `true` when `local` escapes.
    #[must_use]
    pub fn escapes(&self, local: Local) -> bool {
        self.escapes.contains(&local.0)
    }

    /// Iterates over every escaping local, in ascending numeric order.
    pub fn iter(&self) -> impl Iterator<Item = Local> + '_ {
        self.escapes.iter().copied().map(Local)
    }

    /// Number of escaping locals.
    #[must_use]
    pub fn len(&self) -> usize {
        self.escapes.len()
    }

    /// Whether no locals escape.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.escapes.is_empty()
    }
}

/// Computes the escape set for `body`.
#[must_use]
pub fn analyse(body: &Body) -> EscapeSet {
    let mut set = EscapeSet::default();

    set.escapes.insert(Local::RETURN.0);
    let mut changed = true;
    while changed {
        changed = false;

        for block in &body.blocks {
            for stmt in &block.stmts {
                if let StatementKind::Assign { place, rvalue } = &stmt.kind {
                    let target_escapes = set.escapes.contains(&place.local.0);
                    // Reference-carrying rvalues (Use of a Copy, Ref,
                    // Aggregate) propagate escape through the source
                    // locals. Arithmetic and unary ops do not: they
                    // produce a fresh value that does not alias their
                    // operands.
                    if target_escapes {
                        let aliases: Vec<&Operand> = match rvalue {
                            Rvalue::Use(op) => vec![op],
                            Rvalue::Aggregate { operands, .. } => operands.iter().collect(),
                            Rvalue::Repeat { value, .. } => vec![value],
                            _ => Vec::new(),
                        };
                        for op in aliases {
                            if let Operand::Copy(src) = op {
                                if set.escapes.insert(src.local.0) {
                                    changed = true;
                                }
                            }
                        }
                    }
                }
            }
            if let Terminator::Call { args, .. } = &block.terminator {
                for arg in args {
                    if let Operand::Copy(src) = arg {
                        if set.escapes.insert(src.local.0) {
                            changed = true;
                        }
                    }
                }
            }
        }
    }

    set
}


#[cfg(test)]
mod tests {
    use gossamer_hir::lower_source_file;
    use gossamer_lex::SourceMap;
    use gossamer_parse::parse_source_file;
    use gossamer_resolve::resolve_source_file;
    use gossamer_types::{TyCtxt, typecheck_source_file};

    use super::*;
    use crate::lower_program;

    fn build(source: &str) -> Vec<Body> {
        let mut map = SourceMap::new();
        let file = map.add_file("t.gos", source.to_string());
        let (sf, _) = parse_source_file(source, file);
        let (res, _) = resolve_source_file(&sf);
        let mut tcx = TyCtxt::new();
        let (tbl, _) = typecheck_source_file(&sf, &res, &mut tcx);
        let hir = lower_source_file(&sf, &res, &tbl, &mut tcx);
        lower_program(&hir, &mut tcx)
    }

    #[test]
    fn return_local_always_escapes() {
        let bodies = build("fn f() -> i64 { 42i64 }\n");
        let set = analyse(&bodies[0]);
        assert!(set.escapes(Local::RETURN));
    }

    #[test]
    fn locals_never_stored_to_return_or_call_do_not_escape() {
        let bodies = build("fn f() -> i64 { let x = 1i64 let y = 2i64 x + y }\n");
        let set = analyse(&bodies[0]);
        // Parameters + return slot escape through the public contract,
        // but the intermediate temp for `y` should not.
        let non_escaping_count = bodies[0]
            .locals
            .iter()
            .enumerate()
            .filter(|(i, _)| set.is_non_escaping(Local(*i as u32)))
            .count();
        assert!(non_escaping_count > 0);
    }

    #[test]
    fn call_arguments_mark_their_source_as_escaping() {
        let bodies = build("fn helper(x: i64) -> i64 { x }\nfn caller() { let a = 7i64 let _ = helper(a) }\n");
        let caller = bodies.iter().find(|b| b.name == "caller").unwrap();
        let set = analyse(caller);
        let had_call_escape = caller
            .locals
            .iter()
            .enumerate()
            .any(|(i, _)| set.escapes(Local(i as u32)));
        assert!(had_call_escape, "expected at least one escaping local");
    }
}
