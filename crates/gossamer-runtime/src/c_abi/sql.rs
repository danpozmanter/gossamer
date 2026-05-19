//! C-ABI shims for `std::database::sql`.
//!
//! Each `gos_rt_sql_*` symbol is callable directly from compiled
//! Gossamer code. The shims operate on five handle registries
//! (Conn / Stmt / Rows / Row / Tx) that store the trait-object
//! pointers behind opaque `i64` handles so the compiled tier
//! never sees a Rust trait fat-pointer.
//!
//! The shims share one process-global registry with `gos run` so
//! handles round-trip across tier boundaries: a `Conn` opened
//! under interp can be dispatched through the JIT, and vice
//! versa.

#![allow(clippy::missing_safety_doc)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::too_many_lines)]
#![allow(missing_docs)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;

use crate::sql::{
    ConnectionImpl, Error, IsolationLevel, RowsImpl, StatementImpl, TransactionImpl, Value,
};

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
#[allow(
    dead_code,
    reason = "reserved for the upcoming prepared-statement handle API"
)]
static STMT_HANDLES: Mutex<Option<HashMap<i64, Box<dyn StatementImpl>>>> = Mutex::new(None);
static ROWS_HANDLES: Mutex<Option<HashMap<i64, Box<dyn RowsImpl>>>> = Mutex::new(None);
static ROW_HANDLES: Mutex<Option<HashMap<i64, Row>>> = Mutex::new(None);
static TX_HANDLES: Mutex<Option<HashMap<i64, Box<dyn TransactionImpl>>>> = Mutex::new(None);

static NEXT_HANDLE: AtomicI64 = AtomicI64::new(1);

/// One row resolved from a `RowsImpl::next_row`. Carries the column
/// metadata so `Row::get(&str)` can look up by name.
struct Row {
    values: Vec<Value>,
    columns: Vec<String>,
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

fn rows_register(r: Box<dyn RowsImpl>) -> i64 {
    let id = next_handle();
    let mut guard = ROWS_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, r);
    id
}

fn rows_take(handle: i64) -> Option<Box<dyn RowsImpl>> {
    let mut guard = ROWS_HANDLES.lock();
    guard.as_mut()?.remove(&handle)
}

fn rows_reinsert(handle: i64, r: Box<dyn RowsImpl>) {
    let mut guard = ROWS_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(handle, r);
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

fn tx_register(t: Box<dyn TransactionImpl>) -> i64 {
    let id = next_handle();
    let mut guard = TX_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(id, t);
    id
}

fn tx_take(handle: i64) -> Option<Box<dyn TransactionImpl>> {
    let mut guard = TX_HANDLES.lock();
    guard.as_mut()?.remove(&handle)
}

fn tx_reinsert(handle: i64, t: Box<dyn TransactionImpl>) {
    let mut guard = TX_HANDLES.lock();
    guard.get_or_insert_with(HashMap::new).insert(handle, t);
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
    let n = c_str_to_string(name);
    let u = c_str_to_string(url);
    match crate::sql::open(&n, &u) {
        Ok(conn) => conn_register(conn),
        Err(Error::UnknownDriver(_)) => -1,
        Err(_) => -2,
    }
}

/// Returns a c-string of `,`-separated driver names. Caller frees
/// via `gos_rt_free_cstring`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_drivers() -> *mut c_char {
    let joined = crate::sql::drivers().join(",");
    alloc_cstring(joined.as_bytes())
}

/// Executes `sql` against the connection identified by `handle`.
/// Returns rows affected, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_execute(handle: i64, sql: *const c_char) -> i64 {
    let s = c_str_to_string(sql);
    conn_with(handle, |c| match c.prepare(&s) {
        Ok(mut stmt) => stmt.execute(&[]).map_or(-1, |n| n as i64),
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Runs a query. Returns a Rows handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_query(handle: i64, sql: *const c_char) -> i64 {
    let s = c_str_to_string(sql);
    conn_with(handle, |c| match c.prepare(&s) {
        Ok(mut stmt) => match stmt.query(&[]) {
            Ok(rows) => rows_register(rows),
            Err(_) => -1,
        },
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Begins a transaction. Returns a Tx handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_begin(handle: i64) -> i64 {
    conn_with(handle, |c| match c.begin() {
        Ok(tx) => tx_register(tx),
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Begins a transaction at the requested isolation level (0=Default,
/// 1=ReadUncommitted, 2=ReadCommitted, 3=RepeatableRead,
/// 4=Serializable). Returns a Tx handle, or -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_begin_with(handle: i64, iso: i64) -> i64 {
    let level = match iso {
        1 => IsolationLevel::ReadUncommitted,
        2 => IsolationLevel::ReadCommitted,
        3 => IsolationLevel::RepeatableRead,
        4 => IsolationLevel::Serializable,
        _ => IsolationLevel::Default,
    };
    conn_with(handle, |c| match c.begin_with(level) {
        Ok(tx) => tx_register(tx),
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Pings the connection. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_ping(handle: i64) -> i64 {
    conn_with(handle, |c| match c.ping() {
        Ok(()) => 0,
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Sets the driver-specific busy timeout in milliseconds. Returns 0
/// on success, -1 on error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_set_busy_timeout(handle: i64, ms: i64) -> i64 {
    conn_with(handle, |c| match c.set_busy_timeout(ms) {
        Ok(()) => 0,
        Err(_) => -1,
    })
    .unwrap_or(-1)
}

/// Cancels any in-flight statement on the connection.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_conn_interrupt(handle: i64) -> i64 {
    conn_with(handle, |c| {
        c.interrupt();
        0
    })
    .unwrap_or(-1)
}

// --- rows iteration ------------------------------------------------

/// Advances `rows` and returns a Row handle, 0 on end-of-set, -1 on
/// error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_rows_next_row(handle: i64) -> i64 {
    let Some(mut rows) = rows_take(handle) else {
        return -1;
    };
    let columns: Vec<String> = rows.columns().to_vec();
    let result = match rows.next_row() {
        Ok(Some(values)) => row_register(Row { values, columns }),
        Ok(None) => 0,
        Err(_) => -1,
    };
    rows_reinsert(handle, rows);
    result
}

/// Returns a c-string of `,`-separated column names for `rows`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_rows_columns(handle: i64) -> *mut c_char {
    let Some(rows) = rows_take(handle) else {
        return empty_cstring();
    };
    let joined = rows.columns().join(",");
    let out = alloc_cstring(joined.as_bytes());
    rows_reinsert(handle, rows);
    out
}

// --- row column readers --------------------------------------------

fn row_value_by_column(row: &Row, column: &str) -> Option<Value> {
    row.columns
        .iter()
        .position(|c| c == column)
        .and_then(|i| row.values.get(i).cloned())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_i64(handle: i64, column: *const c_char) -> i64 {
    let col = c_str_to_string(column);
    row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Int(n)) => n,
        _ => 0,
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_f64(handle: i64, column: *const c_char) -> f64 {
    let col = c_str_to_string(column);
    row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Float(f)) => f,
        Some(Value::Int(n)) => n as f64,
        _ => 0.0,
    })
    .unwrap_or(0.0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_bool(handle: i64, column: *const c_char) -> i32 {
    let col = c_str_to_string(column);
    row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Bool(b)) => i32::from(b),
        Some(Value::Int(n)) => i32::from(n != 0),
        _ => 0,
    })
    .unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_text(
    handle: i64,
    column: *const c_char,
) -> *mut c_char {
    let col = c_str_to_string(column);
    let text = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Text(s)) => s,
        _ => String::new(),
    })
    .unwrap_or_default();
    alloc_cstring(text.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_row_get_blob(
    handle: i64,
    column: *const c_char,
) -> *mut c_char {
    let col = c_str_to_string(column);
    let bytes = row_with(handle, |row| match row_value_by_column(row, &col) {
        Some(Value::Blob(b)) => b,
        _ => Vec::new(),
    })
    .unwrap_or_default();
    alloc_cstring(&bytes)
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
    row_with(handle, |row| row.values.len() as i64).unwrap_or(0)
}

// --- transaction ---------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_commit(handle: i64) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    match tx.commit() {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_rollback(handle: i64) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    match tx.rollback() {
        Ok(()) => 0,
        Err(_) => -1,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_execute(handle: i64, sql: *const c_char) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    let s = c_str_to_string(sql);
    let n = match tx.execute(&s) {
        Ok(n) => n as i64,
        Err(_) => -1,
    };
    tx_reinsert(handle, tx);
    n
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_savepoint(handle: i64, name: *const c_char) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    let n = c_str_to_string(name);
    let r = match tx.savepoint(&n) {
        Ok(()) => 0,
        Err(_) => -1,
    };
    tx_reinsert(handle, tx);
    r
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_release_savepoint(handle: i64, name: *const c_char) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    let n = c_str_to_string(name);
    let r = match tx.release_savepoint(&n) {
        Ok(()) => 0,
        Err(_) => -1,
    };
    tx_reinsert(handle, tx);
    r
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sql_tx_rollback_to_savepoint(
    handle: i64,
    name: *const c_char,
) -> i64 {
    let Some(mut tx) = tx_take(handle) else {
        return -1;
    };
    let n = c_str_to_string(name);
    let r = match tx.rollback_to_savepoint(&n) {
        Ok(()) => 0,
        Err(_) => -1,
    };
    tx_reinsert(handle, tx);
    r
}
