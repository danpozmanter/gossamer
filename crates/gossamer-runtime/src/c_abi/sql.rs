//! C-ABI shims for `std::database::sql`.
//!
//! Each `gos_rt_sql_*` symbol is callable directly from compiled
//! Gossamer code. The shims operate on handle registries
//! (Conn / Stmt / Rows / Row / Tx / Params) that store the trait-object
//! pointers behind opaque `i64` handles so the compiled tier
//! never sees a Rust trait fat-pointer.
//!
//! The shims share one process-global registry with `gos run`:
//! the interpreter's `__gos_sql_*_raw` builtins call the same safe
//! core functions (`sql_open_handle`, `sql_conn_execute_params`, …)
//! that these shims marshal to, so handles round-trip across tier
//! boundaries and semantics are identical by construction.

#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::too_many_lines)]
#![allow(missing_docs)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;

use crate::sql::{
    ConnectionImpl, Driver, Error, IsolationLevel, Notification, RowsImpl, StatementImpl,
    TransactionImpl, Value,
};

// Exported under an unmangled name on purpose: a `gos build` binary
// with `[rust-bindings]` links TWO copies of gossamer-runtime (the
// runtime staticlib and the bindings staticlib) under
// `--allow-multiple-definition`. Identical code is harmless, but a
// crate-internal static would exist once per copy — the binding's
// `register()` would write one registry while `gos_rt_sql_open`
// reads the other. An unmangled symbol is deduplicated by the
// linker, so every copy shares this one storage location.
#[unsafe(no_mangle)]
static GOS_RT_SQL_DRIVER_REGISTRY: Mutex<Vec<Arc<dyn Driver>>> = Mutex::new(Vec::new());

/// The process-wide SQL driver registry shared across linked
/// gossamer-runtime copies.
pub(crate) fn driver_registry() -> &'static Mutex<Vec<Arc<dyn Driver>>> {
    &GOS_RT_SQL_DRIVER_REGISTRY
}

// --- helpers -------------------------------------------------------

fn c_str_to_string(p: *const c_char) -> String {
    if p.is_null() {
        return String::new();
    }
    // SAFETY: caller must pass NUL-terminated UTF-8 valid for the
    // call. Gossamer codegen emits such pointers (string pool +
    // alloc_cstring).
    unsafe { CStr::from_ptr(p).to_str().unwrap_or("").to_string() }
}

fn alloc_cstring(bytes: &[u8]) -> *mut c_char {
    super::alloc_cstring(bytes)
}

fn empty_cstring() -> *mut c_char {
    alloc_cstring(b"")
}

// --- handle registries --------------------------------------------

static CONN_HANDLES: Mutex<Option<HashMap<i64, Box<dyn ConnectionImpl>>>> = Mutex::new(None);
static STMT_HANDLES: Mutex<Option<HashMap<i64, StmtEntry>>> = Mutex::new(None);
static ROWS_HANDLES: Mutex<Option<HashMap<i64, RowsEntry>>> = Mutex::new(None);
static ROW_HANDLES: Mutex<Option<HashMap<i64, Row>>> = Mutex::new(None);
static TX_HANDLES: Mutex<Option<HashMap<i64, TxEntry>>> = Mutex::new(None);
static PARAMS_HANDLES: Mutex<Option<HashMap<i64, Vec<Value>>>> = Mutex::new(None);
static POOL_HANDLES: Mutex<Option<HashMap<i64, crate::sql_pool::Pool>>> = Mutex::new(None);
/// Most recent notification delivered by `sql_conn_poll_notification`,
/// keyed by connection handle; the scalar getter shims read it.
static LAST_NOTIFICATION: Mutex<Option<HashMap<i64, Notification>>> = Mutex::new(None);
/// Bytes produced by the most recent `copy_out_run`, keyed by
/// connection handle, until `copy_out_take` claims them.
static COPY_OUT_SLOTS: Mutex<Option<HashMap<i64, Vec<u8>>>> = Mutex::new(None);

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

// Goroutines are not thread-pinned, so this is a process-global slot
// rather than a thread-local: the injected wrappers read it on the
// same goroutine immediately after a failing call. Concurrent
// failures on different goroutines may interleave messages; the
// failure itself is still reported.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// Records the message returned by the next `sql_take_last_error`.
pub fn sql_set_last_error(msg: impl Into<String>) {
    *LAST_ERROR.lock() = msg.into();
}

/// Returns and clears the most recent SQL error message.
pub fn sql_take_last_error() -> String {
    std::mem::take(&mut *LAST_ERROR.lock())
}

fn fail(msg: impl Into<String>) -> i64 {
    sql_set_last_error(msg);
    -1
}

/// One row resolved from a `RowsImpl::next_row`. Carries the column
/// metadata so `Row::get(&str)` can look up by name.
struct Row {
    values: Vec<Value>,
    columns: Vec<String>,
}

/// A live result-set cursor. `conn` is the owning connection handle
/// (closing the connection sweeps its cursors); `current_row` is the
/// Row handle returned by the most recent advance (0 = none). Rows
/// follow cursor semantics: advancing frees the previous Row, so a
/// Row is valid only until the next `next_row` / `close`.
struct RowsEntry {
    rows: Box<dyn RowsImpl>,
    conn: i64,
    current_row: i64,
}

/// A prepared statement; `conn` is the owning connection handle
/// (closing the connection sweeps its statements).
struct StmtEntry {
    stmt: Box<dyn StatementImpl>,
    conn: i64,
}

/// A live transaction; `conn` is the owning connection handle —
/// result-set cursors opened inside the transaction register under
/// it, and closing the connection sweeps surviving transactions
/// (driver `Drop` rolls back).
struct TxEntry {
    tx: Box<dyn TransactionImpl>,
    conn: i64,
}

fn next_handle() -> i64 {
    NEXT_HANDLE.fetch_add(1, Ordering::AcqRel)
}

fn conn_register(c: Box<dyn ConnectionImpl>) -> i64 {
    let id = next_handle();
    let mut guard = CONN_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, c);
    id
}

fn conn_with<R>(handle: i64, f: impl FnOnce(&mut dyn ConnectionImpl) -> R) -> Option<R> {
    let mut guard = CONN_HANDLES.lock();
    let map = guard.as_mut()?;
    let c = map.get_mut(&handle)?;
    Some(f(c.as_mut()))
}

fn rows_register(r: Box<dyn RowsImpl>, conn: i64) -> i64 {
    let id = next_handle();
    let entry = RowsEntry {
        rows: r,
        conn,
        current_row: 0,
    };
    let mut guard = ROWS_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, entry);
    id
}

fn rows_take(handle: i64) -> Option<RowsEntry> {
    let mut guard = ROWS_HANDLES.lock();
    guard.as_mut()?.remove(&handle)
}

fn rows_reinsert(handle: i64, entry: RowsEntry) {
    let mut guard = ROWS_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(handle, entry);
}

fn row_unregister(handle: i64) {
    if handle <= 0 {
        return;
    }
    let mut guard = ROW_HANDLES.lock();
    if let Some(map) = guard.as_mut() {
        map.remove(&handle);
    }
}

fn row_register(r: Row) -> i64 {
    let id = next_handle();
    let mut guard = ROW_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, r);
    id
}

fn row_with<R>(handle: i64, f: impl FnOnce(&Row) -> R) -> Option<R> {
    let guard = ROW_HANDLES.lock();
    let map = guard.as_ref()?;
    let r = map.get(&handle)?;
    Some(f(r))
}

fn tx_register(t: Box<dyn TransactionImpl>, conn: i64) -> i64 {
    let id = next_handle();
    let mut guard = TX_HANDLES.lock();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(id, TxEntry { tx: t, conn });
    id
}

fn tx_take(handle: i64) -> Option<TxEntry> {
    let mut guard = TX_HANDLES.lock();
    guard.as_mut()?.remove(&handle)
}

fn tx_reinsert(handle: i64, t: TxEntry) {
    let mut guard = TX_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(handle, t);
}

fn stmt_register(stmt: Box<dyn StatementImpl>, conn: i64) -> i64 {
    let id = next_handle();
    let mut guard = STMT_HANDLES.lock();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(id, StmtEntry { stmt, conn });
    id
}

fn stmt_take(handle: i64) -> Option<StmtEntry> {
    let mut guard = STMT_HANDLES.lock();
    guard.as_mut()?.remove(&handle)
}

fn stmt_reinsert(handle: i64, entry: StmtEntry) {
    let mut guard = STMT_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(handle, entry);
}

// --- value shims ---------------------------------------------------
//
// SQL `Value` is a wide enum. Compiled code holds opaque `i64`
// handles into a tagged-payload registry rather than trying to
// marshal the discriminator + payload into a single register.

static VALUE_HANDLES: Mutex<Option<HashMap<i64, Value>>> = Mutex::new(None);

fn value_register(v: Value) -> i64 {
    let id = next_handle();
    let mut guard = VALUE_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, v);
    id
}

// --- safe core -----------------------------------------------------
//
// One implementation shared by the C-ABI shims below and the
// interpreter's `__gos_sql_*_raw` builtins (gossamer-interp calls
// these directly). Sentinel conventions: handles are > 0; `-1`
// means error (`-2` for `open` means driver error vs `-1` unknown
// driver); `0` from `sql_rows_next_row` means end-of-set. Every
// error path records a message readable via `sql_take_last_error`.

const INVALID_CONN: &str = "sql: invalid connection handle";
const INVALID_ROWS: &str = "sql: invalid rows handle";
const INVALID_TX: &str = "sql: invalid transaction handle";

/// Opens a connection. Returns a Conn handle, -1 on unknown driver,
/// -2 on driver error.
pub fn sql_open_handle(name: &str, url: &str) -> i64 {
    match crate::sql::open(name, url) {
        Ok(conn) => conn_register(conn),
        Err(e @ Error::UnknownDriver(_)) => {
            sql_set_last_error(e.to_string());
            -1
        }
        Err(e) => {
            sql_set_last_error(e.to_string());
            -2
        }
    }
}

/// Comma-joined registered driver names.
pub fn sql_drivers_joined() -> String {
    crate::sql::drivers().join(",")
}

/// Allocates a fresh parameter list and returns its handle.
pub fn sql_params_new() -> i64 {
    let id = next_handle();
    let mut guard = PARAMS_HANDLES.lock();
    guard
        .get_or_insert_with(HashMap::new)
        .insert(id, Vec::new());
    id
}

/// Appends a value to the parameter list. Returns 0, or -1 on a bad
/// handle.
pub fn sql_params_push(handle: i64, v: Value) -> i64 {
    let mut guard = PARAMS_HANDLES.lock();
    match guard.as_mut().and_then(|m| m.get_mut(&handle)) {
        Some(list) => {
            list.push(v);
            0
        }
        None => fail("sql: invalid params handle"),
    }
}

fn params_take(handle: i64) -> Vec<Value> {
    let mut guard = PARAMS_HANDLES.lock();
    guard
        .as_mut()
        .and_then(|m| m.remove(&handle))
        .unwrap_or_default()
}

/// Prepares + executes `sql` with the bound parameter list (consumed).
/// Returns rows affected, or -1 on error.
pub fn sql_conn_execute_params(handle: i64, sql: &str, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    conn_with(handle, |c| match c.prepare(sql) {
        Ok(mut stmt) => match stmt.execute(&params) {
            Ok(n) => n as i64,
            Err(e) => fail(e.to_string()),
        },
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Prepares + queries `sql` with the bound parameter list (consumed).
/// Returns a Rows handle, or -1 on error.
pub fn sql_conn_query_params(handle: i64, sql: &str, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    conn_with(handle, |c| match c.prepare(sql) {
        Ok(mut stmt) => match stmt.query(&params) {
            Ok(rows) => rows_register(rows, handle),
            Err(e) => fail(e.to_string()),
        },
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Begins a transaction. Returns a Tx handle, or -1 on error.
pub fn sql_conn_begin(handle: i64) -> i64 {
    conn_with(handle, |c| match c.begin() {
        Ok(tx) => tx_register(tx, handle),
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Begins a transaction at isolation level `iso` (0=Default,
/// 1=ReadUncommitted, 2=ReadCommitted, 3=RepeatableRead,
/// 4=Serializable). Returns a Tx handle, or -1 on error.
pub fn sql_conn_begin_with(handle: i64, iso: i64) -> i64 {
    let level = match iso {
        1 => IsolationLevel::ReadUncommitted,
        2 => IsolationLevel::ReadCommitted,
        3 => IsolationLevel::RepeatableRead,
        4 => IsolationLevel::Serializable,
        _ => IsolationLevel::Default,
    };
    conn_with(handle, |c| match c.begin_with(level) {
        Ok(tx) => tx_register(tx, handle),
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Pings the connection. Returns 0 on success, -1 on error.
pub fn sql_conn_ping(handle: i64) -> i64 {
    conn_with(handle, |c| match c.ping() {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Sets the driver busy timeout. Returns 0 on success, -1 on error.
pub fn sql_conn_set_busy_timeout(handle: i64, ms: i64) -> i64 {
    conn_with(handle, |c| match c.set_busy_timeout(ms) {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Cancels any in-flight statement on the connection. Returns 0, or
/// -1 on a bad handle.
pub fn sql_conn_interrupt(handle: i64) -> i64 {
    conn_with(handle, |c| {
        c.interrupt();
        0
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Closes the connection and releases its handle, sweeping any
/// cursors still open on it (so an abandoned iteration is bounded by
/// the connection's lifetime). Returns 0 on success, -1 on error.
pub fn sql_conn_close(handle: i64) -> i64 {
    let conn = {
        let mut guard = CONN_HANDLES.lock();
        guard.as_mut().and_then(|m| m.remove(&handle))
    };
    let Some(mut conn) = conn else {
        return fail(INVALID_CONN);
    };
    sweep_conn_cursors(handle);
    sweep_conn_children(handle);
    match conn.close() {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    }
}

/// Releases every Rows cursor (and its current Row) opened on
/// `conn`. Driver-side drops run outside the registry lock — a
/// blocking driver Drop must not stall unrelated SQL calls.
/// Releases every prepared statement and live transaction opened on
/// `conn`, plus its pending-notification slot. Driver-side drops run
/// outside the registry locks (a transaction `Drop` typically issues
/// a ROLLBACK).
fn sweep_conn_children(conn: i64) {
    let stmts: Vec<StmtEntry> = {
        let mut guard = STMT_HANDLES.lock();
        match guard.as_mut() {
            Some(map) => {
                let keys: Vec<i64> = map
                    .iter()
                    .filter(|(_, e)| e.conn == conn)
                    .map(|(k, _)| *k)
                    .collect();
                keys.iter().filter_map(|k| map.remove(k)).collect()
            }
            None => Vec::new(),
        }
    };
    drop(stmts);
    let txs: Vec<TxEntry> = {
        let mut guard = TX_HANDLES.lock();
        match guard.as_mut() {
            Some(map) => {
                let keys: Vec<i64> = map
                    .iter()
                    .filter(|(_, e)| e.conn == conn)
                    .map(|(k, _)| *k)
                    .collect();
                keys.iter().filter_map(|k| map.remove(k)).collect()
            }
            None => Vec::new(),
        }
    };
    drop(txs);
    let mut guard = LAST_NOTIFICATION.lock();
    if let Some(map) = guard.as_mut() {
        map.remove(&conn);
    }
    drop(guard);
    let mut guard = COPY_OUT_SLOTS.lock();
    if let Some(map) = guard.as_mut() {
        map.remove(&conn);
    }
}

fn sweep_conn_cursors(conn: i64) {
    let swept: Vec<RowsEntry> = {
        let mut guard = ROWS_HANDLES.lock();
        match guard.as_mut() {
            Some(map) => {
                let keys: Vec<i64> = map
                    .iter()
                    .filter(|(_, e)| e.conn == conn)
                    .map(|(k, _)| *k)
                    .collect();
                keys.iter().filter_map(|k| map.remove(k)).collect()
            }
            None => Vec::new(),
        }
    };
    for entry in &swept {
        row_unregister(entry.current_row);
    }
}

/// Advances the result set. Returns a Row handle, 0 on end-of-set
/// (the Rows handle is released), or -1 on error. Cursor semantics:
/// advancing frees the previous Row handle, so at most one Row per
/// cursor is live at a time and a fully drained iteration leaks
/// nothing.
pub fn sql_rows_next_row(handle: i64) -> i64 {
    let Some(mut entry) = rows_take(handle) else {
        return fail(INVALID_ROWS);
    };
    let columns: Vec<String> = entry.rows.columns().to_vec();
    match entry.rows.next_row() {
        Ok(Some(values)) => {
            row_unregister(entry.current_row);
            let row = row_register(Row { values, columns });
            entry.current_row = row;
            rows_reinsert(handle, entry);
            row
        }
        // End-of-set: release the final Row and drop the entry (not
        // reinserted) — the natural completion path reclaims both.
        Ok(None) => {
            row_unregister(entry.current_row);
            0
        }
        // The advance failed; the cursor (and its current Row) stay
        // live so the caller can inspect or close.
        Err(e) => {
            rows_reinsert(handle, entry);
            fail(e.to_string())
        }
    }
}

/// Releases a Rows cursor and its current Row. Idempotent: closing
/// an already-released (or exhausted) handle is a no-op returning 0,
/// so `defer rows.close()` composes with the drain loop.
pub fn sql_rows_close(handle: i64) -> i64 {
    if let Some(entry) = rows_take(handle) {
        row_unregister(entry.current_row);
    }
    0
}

/// Comma-joined column names for the Rows handle.
pub fn sql_rows_columns_joined(handle: i64) -> String {
    let Some(entry) = rows_take(handle) else {
        return String::new();
    };
    let joined = entry.rows.columns().join(",");
    rows_reinsert(handle, entry);
    joined
}

/// Coarse kind of the named column: -1 missing, 0 Null, 1 Bool,
/// 2 Int, 3 Float, 4 Text, 5 Blob; -1 missing column, -2 stale Row
/// handle (the cursor advanced past it or was closed).
pub fn sql_row_kind(handle: i64, column: &str) -> i64 {
    row_with(handle, |row| match row_value_by_column(row, column) {
        None => -1,
        Some(Value::Null) => 0,
        Some(Value::Bool(_)) => 1,
        Some(Value::Int(_)) => 2,
        Some(Value::Float(_)) => 3,
        Some(Value::Text(_)) => 4,
        Some(Value::Blob(_)) => 5,
    })
    .unwrap_or(-2)
}

/// Int column value, or 0 when absent / not Int.
pub fn sql_row_get_i64(handle: i64, column: &str) -> i64 {
    row_with(handle, |row| match row_value_by_column(row, column) {
        Some(Value::Int(n)) => n,
        _ => 0,
    })
    .unwrap_or(0)
}

/// Float column value (Int coerces), or 0.0 when absent.
pub fn sql_row_get_f64(handle: i64, column: &str) -> f64 {
    row_with(handle, |row| match row_value_by_column(row, column) {
        Some(Value::Float(f)) => f,
        Some(Value::Int(n)) => n as f64,
        _ => 0.0,
    })
    .unwrap_or(0.0)
}

/// Bool column value as 0/1 (Int coerces), or 0 when absent.
pub fn sql_row_get_bool(handle: i64, column: &str) -> i64 {
    row_with(handle, |row| match row_value_by_column(row, column) {
        Some(Value::Bool(b)) => i64::from(b),
        Some(Value::Int(n)) => i64::from(n != 0),
        _ => 0,
    })
    .unwrap_or(0)
}

/// Text column value, or "" when absent / not Text.
pub fn sql_row_get_text(handle: i64, column: &str) -> String {
    row_with(handle, |row| match row_value_by_column(row, column) {
        Some(Value::Text(s)) => s,
        _ => String::new(),
    })
    .unwrap_or_default()
}

/// Blob column bytes, or empty when absent / not Blob.
pub fn sql_row_get_blob(handle: i64, column: &str) -> Vec<u8> {
    row_with(handle, |row| match row_value_by_column(row, column) {
        Some(Value::Blob(b)) => b,
        _ => Vec::new(),
    })
    .unwrap_or_default()
}

/// Number of columns in the row.
pub fn sql_row_width(handle: i64) -> i64 {
    row_with(handle, |row| row.values.len() as i64).unwrap_or(0)
}

/// Commits and releases the transaction. Returns 0, or -1 on error.
pub fn sql_tx_commit(handle: i64) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    match tx.tx.commit() {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    }
}

/// Rolls back and releases the transaction. Returns 0, or -1 on
/// error.
pub fn sql_tx_rollback(handle: i64) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    match tx.tx.rollback() {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    }
}

/// Executes raw SQL inside the transaction. Returns rows affected,
/// or -1 on error.
pub fn sql_tx_execute(handle: i64, sql: &str) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    let n = match tx.tx.execute(sql) {
        Ok(n) => n as i64,
        Err(e) => fail(e.to_string()),
    };
    tx_reinsert(handle, tx);
    n
}

fn tx_savepoint_op(
    handle: i64,
    name: &str,
    f: impl FnOnce(&mut dyn TransactionImpl, &str) -> Result<(), Error>,
) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    let r = match f(tx.tx.as_mut(), name) {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    };
    tx_reinsert(handle, tx);
    r
}

/// Creates a savepoint. Returns 0, or -1 on error.
pub fn sql_tx_savepoint(handle: i64, name: &str) -> i64 {
    tx_savepoint_op(handle, name, |tx, n| tx.savepoint(n))
}

/// Releases (commits) a savepoint. Returns 0, or -1 on error.
pub fn sql_tx_release_savepoint(handle: i64, name: &str) -> i64 {
    tx_savepoint_op(handle, name, |tx, n| tx.release_savepoint(n))
}

/// Rolls back to a savepoint. Returns 0, or -1 on error.
pub fn sql_tx_rollback_to_savepoint(handle: i64, name: &str) -> i64 {
    tx_savepoint_op(handle, name, |tx, n| tx.rollback_to_savepoint(n))
}

/// Executes a statement with bound parameters (consumed) inside the
/// transaction. Returns rows affected, or -1 on error.
pub fn sql_tx_execute_params(handle: i64, sql: &str, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    let Some(mut entry) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    let n = match entry.tx.execute_params(sql, &params) {
        Ok(n) => n as i64,
        Err(e) => fail(e.to_string()),
    };
    tx_reinsert(handle, entry);
    n
}

/// Runs a query with bound parameters (consumed) inside the
/// transaction. Returns a Rows handle (registered under the
/// transaction's connection), or -1 on error.
pub fn sql_tx_query_params(handle: i64, sql: &str, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    let Some(mut entry) = tx_take(handle) else {
        return fail(INVALID_TX);
    };
    let conn = entry.conn;
    let r = match entry.tx.query_params(sql, &params) {
        Ok(rows) => rows_register(rows, conn),
        Err(e) => fail(e.to_string()),
    };
    tx_reinsert(handle, entry);
    r
}

/// Prepares a statement for repeated execution. Returns a Stmt
/// handle (registered under the connection), or -1 on error.
pub fn sql_conn_prepare(handle: i64, sql: &str) -> i64 {
    conn_with(handle, |c| match c.prepare(sql) {
        Ok(stmt) => stmt_register(stmt, handle),
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Executes a prepared statement with bound parameters (consumed).
/// Returns rows affected, or -1 on error.
pub fn sql_stmt_execute(handle: i64, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    let Some(mut entry) = stmt_take(handle) else {
        return fail("sql: invalid statement handle");
    };
    let n = match entry.stmt.execute(&params) {
        Ok(n) => n as i64,
        Err(e) => fail(e.to_string()),
    };
    stmt_reinsert(handle, entry);
    n
}

/// Runs a prepared statement with bound parameters (consumed).
/// Returns a Rows handle, or -1 on error.
pub fn sql_stmt_query(handle: i64, params_handle: i64) -> i64 {
    let params = params_take(params_handle);
    let Some(mut entry) = stmt_take(handle) else {
        return fail("sql: invalid statement handle");
    };
    let conn = entry.conn;
    let r = match entry.stmt.query(&params) {
        Ok(rows) => rows_register(rows, conn),
        Err(e) => fail(e.to_string()),
    };
    stmt_reinsert(handle, entry);
    r
}

/// Releases a prepared statement. Idempotent.
pub fn sql_stmt_close(handle: i64) -> i64 {
    let _ = stmt_take(handle);
    0
}

/// Bulk-loads `data` through the dialect's copy mechanism. Returns
/// rows written, or -1 on error.
pub fn sql_conn_copy_in(handle: i64, sql: &str, data: &[u8]) -> i64 {
    conn_with(handle, |c| match c.copy_in(sql, data) {
        Ok(n) => n as i64,
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Bulk-extracts rows through the dialect's copy mechanism. `None`
/// means error (message via `sql_take_last_error`).
pub fn sql_conn_copy_out(handle: i64, sql: &str) -> Option<Vec<u8>> {
    match conn_with(handle, |c| c.copy_out(sql)) {
        Some(Ok(bytes)) => Some(bytes),
        Some(Err(e)) => {
            sql_set_last_error(e.to_string());
            None
        }
        None => {
            sql_set_last_error(INVALID_CONN);
            None
        }
    }
}

/// Subscribes the connection to `channel`. Returns 0, or -1 on error.
pub fn sql_conn_listen(handle: i64, channel: &str) -> i64 {
    conn_with(handle, |c| match c.listen(channel) {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Unsubscribes the connection from `channel`. Returns 0, or -1 on
/// error.
pub fn sql_conn_unlisten(handle: i64, channel: &str) -> i64 {
    conn_with(handle, |c| match c.unlisten(channel) {
        Ok(()) => 0,
        Err(e) => fail(e.to_string()),
    })
    .unwrap_or_else(|| fail(INVALID_CONN))
}

/// Waits up to `timeout_ms` for a notification. Returns 1 when one
/// arrived (readable via the `sql_notification_*` getters), 0 on
/// timeout, -1 on error.
pub fn sql_conn_poll_notification(handle: i64, timeout_ms: i64) -> i64 {
    let polled = conn_with(handle, |c| c.poll_notification(timeout_ms));
    match polled {
        Some(Ok(Some(n))) => {
            let mut guard = LAST_NOTIFICATION.lock();
            guard.get_or_insert_with(HashMap::new).insert(handle, n);
            1
        }
        Some(Ok(None)) => 0,
        Some(Err(e)) => fail(e.to_string()),
        None => fail(INVALID_CONN),
    }
}

/// Stores copy-out bytes for the connection (interpreter-side
/// counterpart of `gos_rt_sql_conn_copy_out_run`).
pub fn sql_copy_out_store(conn: i64, bytes: Vec<u8>) {
    let mut guard = COPY_OUT_SLOTS.lock();
    guard.get_or_insert_with(HashMap::new).insert(conn, bytes);
}

/// Takes the connection's stored copy-out bytes (empty if none).
pub fn sql_copy_out_take(conn: i64) -> Vec<u8> {
    let mut guard = COPY_OUT_SLOTS.lock();
    guard
        .as_mut()
        .and_then(|m| m.remove(&conn))
        .unwrap_or_default()
}

fn last_notification<R>(conn: i64, f: impl FnOnce(&Notification) -> R) -> Option<R> {
    let guard = LAST_NOTIFICATION.lock();
    guard.as_ref()?.get(&conn).map(f)
}

/// Channel of the connection's most recently polled notification.
pub fn sql_notification_channel(conn: i64) -> String {
    last_notification(conn, |n| n.channel.clone()).unwrap_or_default()
}

/// Payload of the connection's most recently polled notification.
pub fn sql_notification_payload(conn: i64) -> String {
    last_notification(conn, |n| n.payload.clone()).unwrap_or_default()
}

/// Backend pid of the connection's most recently polled notification.
pub fn sql_notification_pid(conn: i64) -> i64 {
    last_notification(conn, |n| n.process_id).unwrap_or(0)
}

/// Builds a connection pool. Timeout/lifetime arguments are in
/// milliseconds; 0 disables idle/lifetime eviction. Returns a Pool
/// handle, or -1 on error.
pub fn sql_pool_new(
    driver: &str,
    url: &str,
    min: i64,
    max: i64,
    acquire_timeout_ms: i64,
    idle_timeout_ms: i64,
    max_lifetime_ms: i64,
) -> i64 {
    let mut config = crate::sql_pool::PoolConfig {
        min: min.max(0) as usize,
        max: max.max(0) as usize,
        idle_timeout: (idle_timeout_ms > 0)
            .then(|| std::time::Duration::from_millis(idle_timeout_ms as u64)),
        max_lifetime: (max_lifetime_ms > 0)
            .then(|| std::time::Duration::from_millis(max_lifetime_ms as u64)),
        ..Default::default()
    };
    if acquire_timeout_ms > 0 {
        config.acquire_timeout = std::time::Duration::from_millis(acquire_timeout_ms as u64);
    }
    match crate::sql_pool::Pool::new(driver, url, config) {
        Ok(pool) => {
            let id = next_handle();
            let mut guard = POOL_HANDLES.lock();
            guard.get_or_insert_with(HashMap::new).insert(id, pool);
            id
        }
        Err(e) => fail(e.to_string()),
    }
}

fn pool_with<R>(handle: i64, f: impl FnOnce(&crate::sql_pool::Pool) -> R) -> Option<R> {
    let guard = POOL_HANDLES.lock();
    guard.as_ref()?.get(&handle).map(f)
}

/// Checks a connection out of the pool. The result is an ordinary
/// Conn handle; closing it returns the connection to the pool.
pub fn sql_pool_get(handle: i64) -> i64 {
    let checkout = pool_with(handle, crate::sql_pool::Pool::get);
    match checkout {
        Some(Ok(conn)) => conn_register(Box::new(conn)),
        Some(Err(e)) => fail(e.to_string()),
        None => fail("sql: invalid pool handle"),
    }
}

/// Live connections (idle + in-flight), or -1 on a bad handle.
pub fn sql_pool_live(handle: i64) -> i64 {
    pool_with(handle, |p| p.live() as i64).unwrap_or_else(|| fail("sql: invalid pool handle"))
}

/// Idle connections, or -1 on a bad handle.
pub fn sql_pool_idle(handle: i64) -> i64 {
    pool_with(handle, |p| p.idle() as i64).unwrap_or_else(|| fail("sql: invalid pool handle"))
}

/// Closes all idle pooled connections. Returns 0, or -1 on a bad
/// handle.
pub fn sql_pool_close_idle(handle: i64) -> i64 {
    pool_with(handle, |p| {
        p.close_idle();
        0
    })
    .unwrap_or_else(|| fail("sql: invalid pool handle"))
}

/// Applies pending migrations from `dir` on the connection. Returns
/// the number applied, or -1 on error.
pub fn sql_migrate_up(conn: i64, dir: &str) -> i64 {
    let result = conn_with(conn, |c| crate::sql_migrate::up(c, dir));
    match result {
        Some(Ok(applied)) => applied.len() as i64,
        Some(Err(e)) => fail(e.to_string()),
        None => fail(INVALID_CONN),
    }
}

// Each SQL shim is `pub extern "C"` with `#[unsafe(no_mangle)]`, so the
// linker keeps every symbol on its own. No `#[used]` anchor needed.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_value_null() -> i64 {
    value_register(Value::Null)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_value_bool(b: i32) -> i64 {
    value_register(Value::Bool(b != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_value_int(n: i64) -> i64 {
    value_register(Value::Int(n))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_value_float(f: f64) -> i64 {
    value_register(Value::Float(f))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_value_text(s: *const c_char) -> i64 {
    value_register(Value::Text(c_str_to_string(s)))
}

// --- conn / open ---------------------------------------------------

/// Opens a connection. Returns -1 on unknown driver, -2 on driver
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_open(name: *const c_char, url: *const c_char) -> i64 {
    sql_open_handle(&c_str_to_string(name), &c_str_to_string(url))
}

/// Returns the most recent SQL error message as a c-string (and
/// clears it). Caller frees via `gos_rt_free_cstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_last_error() -> *mut c_char {
    alloc_cstring(sql_take_last_error().as_bytes())
}

/// Returns a c-string of `,`-separated driver names. Caller frees
/// via `gos_rt_free_cstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_drivers() -> *mut c_char {
    alloc_cstring(sql_drivers_joined().as_bytes())
}

/// Allocates a parameter list; returns its handle.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_new() -> i64 {
    sql_params_new()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_null(p: i64) -> i64 {
    sql_params_push(p, Value::Null)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_bool(p: i64, b: i64) -> i64 {
    sql_params_push(p, Value::Bool(b != 0))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_int(p: i64, n: i64) -> i64 {
    sql_params_push(p, Value::Int(n))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_float(p: i64, f: f64) -> i64 {
    sql_params_push(p, Value::Float(f))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_text(p: i64, s: *const c_char) -> i64 {
    sql_params_push(p, Value::Text(c_str_to_string(s)))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_params_push_blob(
    p: i64,
    bytes: *const super::vec::GosVec,
) -> i64 {
    // SAFETY: codegen passes a live GosVec pointer for a `[u8]` arg.
    let data = unsafe { super::encoding::gosvec_u8(bytes) };
    sql_params_push(p, Value::Blob(data))
}

/// Executes `sql` against the connection identified by `handle`.
/// Returns rows affected, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_execute(handle: i64, sql: *const c_char) -> i64 {
    sql_conn_execute_params(handle, &c_str_to_string(sql), 0)
}

/// Executes `sql` with bound parameters (the params handle is
/// consumed). Returns rows affected, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_execute_params(
    handle: i64,
    sql: *const c_char,
    params: i64,
) -> i64 {
    sql_conn_execute_params(handle, &c_str_to_string(sql), params)
}

/// Runs a query. Returns a Rows handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_query(handle: i64, sql: *const c_char) -> i64 {
    sql_conn_query_params(handle, &c_str_to_string(sql), 0)
}

/// Runs a query with bound parameters (the params handle is
/// consumed). Returns a Rows handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_query_params(
    handle: i64,
    sql: *const c_char,
    params: i64,
) -> i64 {
    sql_conn_query_params(handle, &c_str_to_string(sql), params)
}

/// Begins a transaction. Returns a Tx handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_begin(handle: i64) -> i64 {
    sql_conn_begin(handle)
}

/// Begins a transaction at the requested isolation level (0=Default,
/// 1=ReadUncommitted, 2=ReadCommitted, 3=RepeatableRead,
/// 4=Serializable). Returns a Tx handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_begin_with(handle: i64, iso: i64) -> i64 {
    sql_conn_begin_with(handle, iso)
}

/// Pings the connection. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_ping(handle: i64) -> i64 {
    sql_conn_ping(handle)
}

/// Sets the driver-specific busy timeout in milliseconds. Returns 0
/// on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_set_busy_timeout(handle: i64, ms: i64) -> i64 {
    sql_conn_set_busy_timeout(handle, ms)
}

/// Cancels any in-flight statement on the connection.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_interrupt(handle: i64) -> i64 {
    sql_conn_interrupt(handle)
}

/// Closes the connection and releases its handle. Returns 0 on
/// success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_close(handle: i64) -> i64 {
    sql_conn_close(handle)
}

// --- rows iteration ------------------------------------------------

/// Advances `rows` and returns a Row handle, 0 on end-of-set, -1 on
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_rows_next_row(handle: i64) -> i64 {
    sql_rows_next_row(handle)
}

/// Releases a Rows cursor and its current Row. Idempotent; always 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_rows_close(handle: i64) -> i64 {
    sql_rows_close(handle)
}

/// Returns a c-string of `,`-separated column names for `rows`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_rows_columns(handle: i64) -> *mut c_char {
    let joined = sql_rows_columns_joined(handle);
    if joined.is_empty() {
        return empty_cstring();
    }
    alloc_cstring(joined.as_bytes())
}

// --- row column readers --------------------------------------------

fn row_value_by_column(row: &Row, column: &str) -> Option<Value> {
    row.columns
        .iter()
        .position(|c| c == column)
        .and_then(|i| row.values.get(i).cloned())
}

/// Returns the coarse kind of the named column: -1 missing, 0 Null,
/// 1 Bool, 2 Int, 3 Float, 4 Text, 5 Blob.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_kind(handle: i64, column: *const c_char) -> i64 {
    sql_row_kind(handle, &c_str_to_string(column))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_i64(handle: i64, column: *const c_char) -> i64 {
    sql_row_get_i64(handle, &c_str_to_string(column))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_f64(handle: i64, column: *const c_char) -> f64 {
    sql_row_get_f64(handle, &c_str_to_string(column))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_bool(handle: i64, column: *const c_char) -> i32 {
    sql_row_get_bool(handle, &c_str_to_string(column)) as i32
}

/// Like `gos_rt_sql_row_get_bool` with a uniform i64 return for the
/// injected-wrapper call path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_bool_i64(handle: i64, column: *const c_char) -> i64 {
    sql_row_get_bool(handle, &c_str_to_string(column))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_text(
    handle: i64,
    column: *const c_char,
) -> *mut c_char {
    let text = sql_row_get_text(handle, &c_str_to_string(column));
    alloc_cstring(text.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_blob(
    handle: i64,
    column: *const c_char,
) -> *mut c_char {
    let bytes = sql_row_get_blob(handle, &c_str_to_string(column));
    alloc_cstring(&bytes)
}

/// Blob column as a `[u8]` GosVec (one byte per i64 slot).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_blob_vec(
    handle: i64,
    column: *const c_char,
) -> *mut super::vec::GosVec {
    let bytes = sql_row_get_blob(handle, &c_str_to_string(column));
    let out = unsafe { super::vec::gos_rt_vec_with_capacity(8, bytes.len() as i64) };
    // SAFETY: gos_rt_vec_with_capacity returns a live GosVec sized
    // for `bytes.len()` 8-byte slots.
    let vref = unsafe { &mut *out };
    if !vref.ptr.is_null() {
        let dst = vref.ptr.as_ptr().cast::<i64>();
        for (idx, b) in bytes.iter().enumerate() {
            unsafe { *dst.add(idx) = i64::from(*b) };
        }
        vref.len = bytes.len() as i64;
    }
    out
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_opt_i64(
    handle: i64,
    column: *const c_char,
    out_present: *mut i32,
) -> i64 {
    let col = c_str_to_string(column);
    let result = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Null) | None => (0, 0_i32),
        Some(Value::Int(n)) => (n, 1),
        _ => (0, 0),
    })
    .unwrap_or((0, 0));
    if !out_present.is_null() {
        unsafe {
            *out_present = result.1;
        }
    }
    result.0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_opt_f64(
    handle: i64,
    column: *const c_char,
    out_present: *mut i32,
) -> f64 {
    let col = c_str_to_string(column);
    let result = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Null) | None => (0.0, 0_i32),
        Some(Value::Float(f)) => (f, 1),
        Some(Value::Int(n)) => (n as f64, 1),
        _ => (0.0, 0),
    })
    .unwrap_or((0.0, 0));
    if !out_present.is_null() {
        unsafe {
            *out_present = result.1;
        }
    }
    result.0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_opt_bool(
    handle: i64,
    column: *const c_char,
    out_present: *mut i32,
) -> i32 {
    let col = c_str_to_string(column);
    let result = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Null) | None => (0_i32, 0_i32),
        Some(Value::Bool(b)) => (i32::from(b), 1),
        _ => (0, 0),
    })
    .unwrap_or((0, 0));
    if !out_present.is_null() {
        unsafe {
            *out_present = result.1;
        }
    }
    result.0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_opt_text(
    handle: i64,
    column: *const c_char,
    out_present: *mut i32,
) -> *mut c_char {
    let col = c_str_to_string(column);
    let (text, present) = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Null) | None => (String::new(), 0_i32),
        Some(Value::Text(s)) => (s, 1),
        _ => (String::new(), 0),
    })
    .unwrap_or_default();
    if !out_present.is_null() {
        unsafe {
            *out_present = present;
        }
    }
    alloc_cstring(text.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_is_null(handle: i64, column: *const c_char) -> i32 {
    let col = c_str_to_string(column);
    row_with(handle, |row| {
        matches!(row_value_by_column(row, &col), Some(Value::Null) | None)
    })
    .map_or(0, i32::from)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_width(handle: i64) -> i64 {
    sql_row_width(handle)
}

// --- transaction ---------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_commit(handle: i64) -> i64 {
    sql_tx_commit(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_rollback(handle: i64) -> i64 {
    sql_tx_rollback(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_execute(handle: i64, sql: *const c_char) -> i64 {
    sql_tx_execute(handle, &c_str_to_string(sql))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_savepoint(handle: i64, name: *const c_char) -> i64 {
    sql_tx_savepoint(handle, &c_str_to_string(name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_release_savepoint(handle: i64, name: *const c_char) -> i64 {
    sql_tx_release_savepoint(handle, &c_str_to_string(name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_rollback_to_savepoint(
    handle: i64,
    name: *const c_char,
) -> i64 {
    sql_tx_rollback_to_savepoint(handle, &c_str_to_string(name))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_prepare(handle: i64, sql: *const c_char) -> i64 {
    sql_conn_prepare(handle, &c_str_to_string(sql))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_stmt_execute(handle: i64, params: i64) -> i64 {
    sql_stmt_execute(handle, params)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_stmt_query(handle: i64, params: i64) -> i64 {
    sql_stmt_query(handle, params)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_stmt_close(handle: i64) -> i64 {
    sql_stmt_close(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_execute_params(
    handle: i64,
    sql: *const c_char,
    params: i64,
) -> i64 {
    sql_tx_execute_params(handle, &c_str_to_string(sql), params)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_query_params(
    handle: i64,
    sql: *const c_char,
    params: i64,
) -> i64 {
    sql_tx_query_params(handle, &c_str_to_string(sql), params)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_copy_in(
    handle: i64,
    sql: *const c_char,
    data: *const super::vec::GosVec,
) -> i64 {
    // SAFETY: codegen passes a live GosVec pointer for a `[u8]` arg.
    let bytes = unsafe { super::encoding::gosvec_u8(data) };
    sql_conn_copy_in(handle, &c_str_to_string(sql), &bytes)
}

/// Runs COPY TO and stores the bytes in the connection's copy-out
/// slot for `gos_rt_sql_conn_copy_out_take`. Returns the byte count,
/// or -1 on error. Two-step so the injected wrappers can branch on a
/// scalar status before materializing the bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_copy_out_run(handle: i64, sql: *const c_char) -> i64 {
    match sql_conn_copy_out(handle, &c_str_to_string(sql)) {
        Some(bytes) => {
            let n = bytes.len() as i64;
            let mut guard = COPY_OUT_SLOTS.lock();
            guard.get_or_insert_with(HashMap::new).insert(handle, bytes);
            n
        }
        None => -1,
    }
}

/// Takes the bytes stored by the most recent
/// `gos_rt_sql_conn_copy_out_run` on this connection as a `[u8]`
/// GosVec (empty if none).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_copy_out_take(handle: i64) -> *mut super::vec::GosVec {
    let bytes = {
        let mut guard = COPY_OUT_SLOTS.lock();
        guard
            .as_mut()
            .and_then(|m| m.remove(&handle))
            .unwrap_or_default()
    };
    let out = unsafe { super::vec::gos_rt_vec_with_capacity(8, bytes.len() as i64) };
    // SAFETY: gos_rt_vec_with_capacity returns a live GosVec sized
    // for `bytes.len()` 8-byte slots.
    let vref = unsafe { &mut *out };
    if !vref.ptr.is_null() {
        let dst = vref.ptr.as_ptr().cast::<i64>();
        for (idx, b) in bytes.iter().enumerate() {
            unsafe { *dst.add(idx) = i64::from(*b) };
        }
        vref.len = bytes.len() as i64;
    }
    out
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_listen(handle: i64, channel: *const c_char) -> i64 {
    sql_conn_listen(handle, &c_str_to_string(channel))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_unlisten(handle: i64, channel: *const c_char) -> i64 {
    sql_conn_unlisten(handle, &c_str_to_string(channel))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_poll_notification(handle: i64, timeout_ms: i64) -> i64 {
    sql_conn_poll_notification(handle, timeout_ms)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_notification_channel(conn: i64) -> *mut c_char {
    alloc_cstring(sql_notification_channel(conn).as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_notification_payload(conn: i64) -> *mut c_char {
    alloc_cstring(sql_notification_payload(conn).as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_notification_pid(conn: i64) -> i64 {
    sql_notification_pid(conn)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_pool_new(
    driver: *const c_char,
    url: *const c_char,
    min: i64,
    max: i64,
    acquire_timeout_ms: i64,
    idle_timeout_ms: i64,
    max_lifetime_ms: i64,
) -> i64 {
    sql_pool_new(
        &c_str_to_string(driver),
        &c_str_to_string(url),
        min,
        max,
        acquire_timeout_ms,
        idle_timeout_ms,
        max_lifetime_ms,
    )
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_pool_get(handle: i64) -> i64 {
    sql_pool_get(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_pool_live(handle: i64) -> i64 {
    sql_pool_live(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_pool_idle(handle: i64) -> i64 {
    sql_pool_idle(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_pool_close_idle(handle: i64) -> i64 {
    sql_pool_close_idle(handle)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_migrate_up(conn: i64, dir: *const c_char) -> i64 {
    sql_migrate_up(conn, &c_str_to_string(dir))
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::sql::{ConnectionImpl, Driver, RowsImpl, StatementImpl, TransactionImpl};

    /// Stub driver yielding `n` single-column Int rows per query,
    /// optionally erroring at row index `fail_at`.
    struct StubDriver;

    struct StubConn;

    struct StubStmt {
        n: usize,
        fail_at: Option<usize>,
    }

    struct StubRows {
        n: usize,
        fail_at: Option<usize>,
        idx: usize,
        cols: Vec<String>,
    }

    struct StubTx;

    impl Driver for StubDriver {
        fn name(&self) -> &'static str {
            "stub-cursor-test"
        }
        fn open(&self, _url: &str) -> Result<Box<dyn ConnectionImpl>, Error> {
            Ok(Box::new(StubConn))
        }
    }

    impl ConnectionImpl for StubConn {
        fn prepare(&mut self, sql: &str) -> Result<Box<dyn StatementImpl>, Error> {
            // "rows:N" yields N rows; "rows:N:fail:K" errors at index K.
            let mut parts = sql.split(':');
            let _ = parts.next();
            let n = parts.next().and_then(|p| p.parse().ok()).unwrap_or(0);
            let fail_at = match (parts.next(), parts.next()) {
                (Some("fail"), Some(k)) => k.parse().ok(),
                _ => None,
            };
            Ok(Box::new(StubStmt { n, fail_at }))
        }
        fn begin(&mut self) -> Result<Box<dyn TransactionImpl>, Error> {
            Ok(Box::new(StubTx))
        }
        fn close(&mut self) -> Result<(), Error> {
            Ok(())
        }
    }

    impl StatementImpl for StubStmt {
        fn execute(&mut self, _params: &[Value]) -> Result<u64, Error> {
            Ok(0)
        }
        fn query(&mut self, _params: &[Value]) -> Result<Box<dyn RowsImpl>, Error> {
            Ok(Box::new(StubRows {
                n: self.n,
                fail_at: self.fail_at,
                idx: 0,
                cols: vec!["c".into()],
            }))
        }
    }

    impl TransactionImpl for StubTx {
        fn commit(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn rollback(&mut self) -> Result<(), Error> {
            Ok(())
        }
        fn execute(&mut self, _sql: &str) -> Result<u64, Error> {
            Ok(0)
        }
    }

    impl RowsImpl for StubRows {
        fn next_row(&mut self) -> Result<Option<Vec<Value>>, Error> {
            if Some(self.idx) == self.fail_at {
                return Err(Error::driver("stub", "forced failure"));
            }
            if self.idx >= self.n {
                return Ok(None);
            }
            let v = self.idx as i64;
            self.idx += 1;
            Ok(Some(vec![Value::Int(v)]))
        }
        fn columns(&self) -> &[String] {
            &self.cols
        }
    }

    /// Serializes the stub-driver tests: they all read or drain the
    /// process-global `LAST_ERROR` slot (errno-style by design — see
    /// its doc), so parallel test threads race on take/overwrite.
    static ERROR_SLOT_LOCK: Mutex<()> = Mutex::new(());

    fn open_stub() -> i64 {
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| crate::sql::register(Arc::new(StubDriver)));
        let conn = sql_open_handle("stub-cursor-test", "mem");
        assert!(conn > 0, "stub open failed: {}", sql_take_last_error());
        conn
    }

    fn stale(row: i64) -> bool {
        sql_row_kind(row, "c") == -2
    }

    #[test]
    fn drained_iteration_releases_every_handle() {
        let _guard = ERROR_SLOT_LOCK.lock();
        let conn = open_stub();
        let rows = sql_conn_query_params(conn, "rows:3", 0);
        assert!(rows > 0);
        let mut returned = Vec::new();
        loop {
            let row = sql_rows_next_row(rows);
            assert!(row >= 0);
            if row == 0 {
                break;
            }
            assert_eq!(sql_row_kind(row, "c"), 2, "live row must read as Int");
            returned.push(row);
        }
        assert_eq!(returned.len(), 3);
        for row in returned {
            assert!(stale(row), "drained rows must release every Row handle");
        }
        assert_eq!(sql_rows_next_row(rows), -1, "exhausted cursor is released");
        let _ = sql_take_last_error();
        assert_eq!(sql_rows_close(rows), 0, "close after exhaustion is a no-op");
        assert_eq!(sql_conn_close(conn), 0);
    }

    #[test]
    fn advancing_frees_the_previous_row() {
        let _guard = ERROR_SLOT_LOCK.lock();
        let conn = open_stub();
        let rows = sql_conn_query_params(conn, "rows:2", 0);
        let first = sql_rows_next_row(rows);
        assert!(first > 0);
        let second = sql_rows_next_row(rows);
        assert!(second > 0);
        assert!(
            stale(first),
            "cursor semantics: previous row freed on advance"
        );
        assert_eq!(sql_row_kind(second, "c"), 2);
        assert_eq!(sql_rows_close(rows), 0);
        assert!(stale(second), "close frees the current row");
        assert_eq!(sql_conn_close(conn), 0);
    }

    #[test]
    fn rows_close_is_idempotent() {
        let _guard = ERROR_SLOT_LOCK.lock();
        let conn = open_stub();
        let rows = sql_conn_query_params(conn, "rows:2", 0);
        let first = sql_rows_next_row(rows);
        assert!(first > 0);
        assert_eq!(sql_rows_close(rows), 0);
        assert_eq!(sql_rows_close(rows), 0);
        assert_eq!(sql_rows_next_row(rows), -1, "closed cursor is invalid");
        let _ = sql_take_last_error();
        assert_eq!(sql_conn_close(conn), 0);
    }

    #[test]
    fn conn_close_sweeps_abandoned_cursors() {
        let _guard = ERROR_SLOT_LOCK.lock();
        let conn = open_stub();
        let rows_a = sql_conn_query_params(conn, "rows:5", 0);
        let rows_b = sql_conn_query_params(conn, "rows:5", 0);
        let row_a = sql_rows_next_row(rows_a);
        assert!(row_a > 0);
        assert_eq!(sql_conn_close(conn), 0);
        assert!(stale(row_a), "conn close frees swept cursors' rows");
        assert_eq!(sql_rows_next_row(rows_a), -1);
        let _ = sql_take_last_error();
        assert_eq!(sql_rows_next_row(rows_b), -1);
        let _ = sql_take_last_error();
    }

    #[test]
    fn failed_advance_keeps_cursor_and_current_row() {
        let _guard = ERROR_SLOT_LOCK.lock();
        let conn = open_stub();
        let rows = sql_conn_query_params(conn, "rows:3:fail:1", 0);
        let first = sql_rows_next_row(rows);
        assert!(first > 0);
        assert_eq!(sql_rows_next_row(rows), -1, "stub fails at index 1");
        assert!(
            sql_take_last_error().contains("forced failure"),
            "error message recorded"
        );
        assert_eq!(sql_row_kind(first, "c"), 2, "row survives a failed advance");
        assert_eq!(sql_rows_close(rows), 0, "cursor still closable after error");
        assert!(stale(first));
        assert_eq!(sql_conn_close(conn), 0);
    }
}
