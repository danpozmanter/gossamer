//! Native dispatch for `std::database::sql` method calls.
//!
//! When `lower_method_call` sees a receiver whose Adt name is one
//! of `Conn`, `Rows`, `Row`, or `Tx`, it routes the call through
//! `sql_runtime_symbol` (here) to find the matching `gos_rt_sql_*`
//! C-ABI shim, then `dispatch_sql_runtime` emits the runtime call.
//!
//! This is the linchpin of 0.9.0's "no VM-only" rule for SQL: the
//! MIR layer translates user-visible `conn.execute(...)` syntax
//! into a direct call to `gos_rt_sql_conn_execute`, which the
//! cranelift JIT and LLVM AOT both resolve to the runtime shim in
//! `gossamer-runtime::c_abi::sql`.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::wildcard_imports)]

use gossamer_ast::Ident;
use gossamer_hir::HirExpr;
use gossamer_lex::Span;
use gossamer_types::{Ty, TyCtxt};

use crate::ir::{ConstValue, Local, Operand, Place, Terminator};

use super::Builder;

/// Resolves a SQL `(receiver_adt_name, method_name)` pair to the
/// `gos_rt_sql_*` runtime symbol. Returns `None` if the pair is
/// not a known SQL method (caller falls back to the generic
/// dispatch table).
#[must_use]
pub(crate) fn sql_runtime_symbol(adt_name: &str, method: &str) -> Option<&'static str> {
    match (adt_name, method) {
        ("Conn", "execute") => Some("gos_rt_sql_conn_execute"),
        ("Conn", "query") => Some("gos_rt_sql_conn_query"),
        ("Conn", "begin") => Some("gos_rt_sql_conn_begin"),
        ("Conn", "begin_with") => Some("gos_rt_sql_conn_begin_with"),
        ("Conn", "ping") => Some("gos_rt_sql_conn_ping"),
        ("Conn", "set_busy_timeout") => Some("gos_rt_sql_conn_set_busy_timeout"),
        ("Rows", "next_row") => Some("gos_rt_sql_rows_next_row"),
        ("Rows", "columns") => Some("gos_rt_sql_rows_columns"),
        ("Row", "get_i64") => Some("gos_rt_sql_row_get_i64"),
        ("Row", "get_f64") => Some("gos_rt_sql_row_get_f64"),
        ("Row", "get_bool") => Some("gos_rt_sql_row_get_bool"),
        ("Row", "get_text") => Some("gos_rt_sql_row_get_text"),
        ("Row", "get_opt_i64") => Some("gos_rt_sql_row_get_opt_i64"),
        ("Row", "get_opt_f64") => Some("gos_rt_sql_row_get_opt_f64"),
        ("Row", "get_opt_bool") => Some("gos_rt_sql_row_get_opt_bool"),
        ("Row", "get_opt_text") => Some("gos_rt_sql_row_get_opt_text"),
        ("Row", "is_null") => Some("gos_rt_sql_row_is_null"),
        ("Row", "width") => Some("gos_rt_sql_row_width"),
        ("Tx", "commit") => Some("gos_rt_sql_tx_commit"),
        ("Tx", "rollback") => Some("gos_rt_sql_tx_rollback"),
        ("Tx", "execute") => Some("gos_rt_sql_tx_execute"),
        ("Tx", "savepoint") => Some("gos_rt_sql_tx_savepoint"),
        ("Tx", "release_savepoint") => Some("gos_rt_sql_tx_release_savepoint"),
        ("Tx", "rollback_to_savepoint") => Some("gos_rt_sql_tx_rollback_to_savepoint"),
        _ => None,
    }
}

impl<'a> Builder<'a> {
    /// Lowers a SQL method call to a direct call of its
    /// `gos_rt_sql_*` C-ABI shim. The receiver's `__handle: i64`
    /// is extracted and passed as the first argument; remaining
    /// arguments lower the same way as any other call.
    pub(crate) fn dispatch_sql_runtime(
        &mut self,
        sym: &'static str,
        _method: &str,
        _adt_name: &str,
        receiver: &HirExpr,
        args: &[HirExpr],
        ty: Ty,
        span: Span,
    ) -> Option<Local> {
        // Lower the receiver — handle types are
        // `Value::Struct { name, __handle: i64 }`. The compiled
        // tier represents them as a pointer to the aggregate;
        // the runtime shim reads the handle field by offset 0
        // (struct layout is single i64 field). We pass the
        // receiver pointer through unchanged; the shim will
        // re-interpret. Same shape as `mutex.lock()` /
        // `wg.add()` already in the dispatch table.
        let recv_local = self.lower_expr(receiver)?;
        let mut arg_locals = vec![Operand::Copy(Place::local(recv_local))];
        for arg in args {
            let l = self.lower_expr(arg)?;
            arg_locals.push(Operand::Copy(Place::local(l)));
        }
        let dest = self.fresh(ty);
        let next = self.new_block(span);
        self.terminate(Terminator::Call {
            callee: Operand::Const(ConstValue::Str(sym.to_string())),
            args: arg_locals,
            destination: Place::local(dest),
            target: Some(next),
        });
        self.set_current(next);
        Some(dest)
    }
}

/// Resolves a method name (+ arg count) to a `gos_rt_sql_*`
/// symbol when the receiver's static type stays an inference
/// variable after typecheck (the common case for
/// `let conn = sql::open(...)?; conn.execute(...)` because
/// `Conn` is a stdlib opaque type without a Gossamer-side
/// `StructDecl` to register a def_name on).
///
/// The mapping is conservative: names that ALSO exist on
/// non-SQL stdlib types (`HashMap::insert`, `Mutex::lock`,
/// etc.) are excluded. Within Gossamer's stdlib the names
/// below are SQL-only, so misdispatch only happens if a user
/// crate defines a method with the same name — in which case
/// the user's `use std::database::sql` decision pulled SQL
/// into the same namespace and the runtime shim's handle
/// check returns a structured error rather than UB.
#[must_use]
pub(crate) fn sql_unique_method_symbol(method: &str, arg_count: usize) -> Option<&'static str> {
    match (method, arg_count) {
        // Rows
        ("next_row", 0) => Some("gos_rt_sql_rows_next_row"),
        ("columns", 0) => Some("gos_rt_sql_rows_columns"),
        // Row
        ("get_i64", 1) => Some("gos_rt_sql_row_get_i64"),
        ("get_f64", 1) => Some("gos_rt_sql_row_get_f64"),
        ("get_bool", 1) => Some("gos_rt_sql_row_get_bool"),
        ("get_text", 1) => Some("gos_rt_sql_row_get_text"),
        ("get_blob", 1) => Some("gos_rt_sql_row_get_blob"),
        ("get_opt_i64", 1) => Some("gos_rt_sql_row_get_opt_i64"),
        ("get_opt_f64", 1) => Some("gos_rt_sql_row_get_opt_f64"),
        ("get_opt_bool", 1) => Some("gos_rt_sql_row_get_opt_bool"),
        ("get_opt_text", 1) => Some("gos_rt_sql_row_get_opt_text"),
        ("is_null", 1) => Some("gos_rt_sql_row_is_null"),
        ("width", 0) => Some("gos_rt_sql_row_width"),
        // Conn vs Stmt vs Tx — disambiguate by arg count.
        // Conn::execute(sql, params) -> 2 args; Tx::execute(sql) -> 1 arg.
        ("execute", 2) => Some("gos_rt_sql_conn_execute"),
        ("execute", 1) => Some("gos_rt_sql_tx_execute"),
        // Conn::query(sql, params) -> 2 args.
        ("query", 2) => Some("gos_rt_sql_conn_query"),
        // Conn::begin / begin_with / ping / set_busy_timeout.
        ("begin", 0) => Some("gos_rt_sql_conn_begin"),
        ("begin_with", 1) => Some("gos_rt_sql_conn_begin_with"),
        ("ping", 0) => Some("gos_rt_sql_conn_ping"),
        ("set_busy_timeout", 1) => Some("gos_rt_sql_conn_set_busy_timeout"),
        // Tx::commit / rollback (0 args after receiver — the wrapper
        // consumes self at the language level).
        ("commit", 0) => Some("gos_rt_sql_tx_commit"),
        ("rollback", 0) => Some("gos_rt_sql_tx_rollback"),
        ("savepoint", 1) => Some("gos_rt_sql_tx_savepoint"),
        ("release_savepoint", 1) => Some("gos_rt_sql_tx_release_savepoint"),
        ("rollback_to_savepoint", 1) => Some("gos_rt_sql_tx_rollback_to_savepoint"),
        _ => None,
    }
}

#[allow(dead_code)]
fn _force_use(_tcx: &TyCtxt, _i: Ident) {}
