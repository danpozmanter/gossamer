#![allow(clippy::unnecessary_wraps)]
//! Interpreter-tier dispatch for Gossamer-native SQL drivers
//! (`sql::register_native`).
//!
//! The compiled tiers register a Rust `GossamerDriver` into
//! `crate::sql` and dispatch into the `.gos` driver by transmuting
//! `gos_fn_addr("Type::dispatch")`. The interpreter has no machine
//! address to transmute and runs per-worker single-thread `Vm`s, so it
//! dispatches through [`NativeDispatch::call_fn`] from inside these
//! `native(...)` builtins, driving the same `SQL_NATIVE_SLOTS` +
//! `native_facade_*` orchestration in `gossamer_runtime::c_abi::sql`.
//! One slot table and one handle namespace are shared by both tiers.

use std::sync::Arc;

use gossamer_runtime::c_abi::sql::{self as sql_core, op};
use parking_lot::Mutex;

use crate::value::{NativeCall, NativeDispatch, RuntimeResult, Value};

/// A pure side-channel helper builtin (no dispatch context needed).
type HelperFn = fn(&[Value]) -> RuntimeResult<Value>;

/// A registered native driver: the driver `Value` (a stateless
/// struct) and the dispatch symbol `Type::dispatch`.
#[derive(Clone)]
struct NativeDriver {
    value: Value,
    dispatch_name: String,
}

/// Native drivers keyed by `sql::open` name.
static NATIVE_DRIVERS: Mutex<Vec<(String, NativeDriver)>> = Mutex::new(Vec::new());

/// Every token (conn / stmt / rows / tx) owned by a native driver,
/// mapped to that driver so any op routes to the right `dispatch`.
static NATIVE_TOKENS: Mutex<Vec<(i64, NativeDriver)>> = Mutex::new(Vec::new());

fn driver_by_name(name: &str) -> Option<NativeDriver> {
    NATIVE_DRIVERS
        .lock()
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, d)| d.clone())
}

fn driver_by_token(token: i64) -> Option<NativeDriver> {
    NATIVE_TOKENS
        .lock()
        .iter()
        .find(|(t, _)| *t == token)
        .map(|(_, d)| d.clone())
}

fn track_token(token: i64, driver: &NativeDriver) {
    NATIVE_TOKENS.lock().push((token, driver.clone()));
}

fn forget_token(token: i64) {
    NATIVE_TOKENS.lock().retain(|(t, _)| *t != token);
}

/// Records a native driver under `name`. The driver value must be a
/// `Value::Struct`; its name yields the `Type::dispatch` symbol.
fn register(name: String, value: Value) -> RuntimeResult<Value> {
    let dispatch_name = match &value {
        Value::Struct(inner) => format!("{}::dispatch", inner.name),
        _ => "dispatch".to_string(),
    };
    let driver = NativeDriver {
        value,
        dispatch_name,
    };
    let mut reg = NATIVE_DRIVERS.lock();
    reg.retain(|(n, _)| n != &name);
    reg.push((name, driver));
    Ok(Value::Unit)
}

/// Builds the `Fn(op, token) -> i64` the `native_facade_*` helpers
/// call, re-entering the VM through `dispatch`.
fn dispatcher<'a>(
    dispatch: &'a mut dyn NativeDispatch,
    driver: &'a NativeDriver,
) -> impl Fn(i64, i64) -> i64 + 'a {
    let cell = std::cell::RefCell::new(dispatch);
    move |op: i64, token: i64| {
        let mut guard = cell.borrow_mut();
        let result = guard.call_fn(
            &driver.dispatch_name,
            vec![driver.value.clone(), Value::Int(op), Value::Int(token)],
        );
        match result {
            Ok(Value::Int(n)) => n,
            Ok(_) => 0,
            Err(e) => {
                sql_core::native_set_error(token, &format!("{e}"));
                -1
            }
        }
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i)
        .and_then(crate::builtins::as_str)
        .unwrap_or("")
        .to_string()
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args.get(i)
        .and_then(crate::builtins::value_to_int)
        .unwrap_or(0)
}

// --- dispatch-receiving builtins (one per native-routable op) -------

/// `register_native(name, driver)`.
pub(crate) fn native_register(
    _dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let name = arg_str(args, 0);
    let value = args.get(1).cloned().unwrap_or(Value::Unit);
    register(name, value)
}

/// `open(name, url)` for a native driver. Returns a connection token,
/// or -1 (message set).
pub(crate) fn native_open(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let name = arg_str(args, 0);
    let url = arg_str(args, 1);
    let Some(driver) = driver_by_name(&name) else {
        // Fall through to the Rust driver registry.
        return Ok(Value::Int(sql_core::sql_open_handle(&name, &url)));
    };
    let token = sql_core::native_facade_open(&url, dispatcher(dispatch, &driver));
    if token >= 0 {
        track_token(token, &driver);
    }
    Ok(Value::Int(token))
}

macro_rules! route {
    ($dispatch:expr, $token:expr, $rust:expr, $native:expr) => {{
        match driver_by_token($token) {
            Some(driver) => {
                let routed = driver.clone();
                let disp = dispatcher($dispatch, &driver);
                Ok(Value::Int($native(routed, disp)))
            }
            None => Ok(Value::Int($rust)),
        }
    }};
}

pub(crate) fn native_conn_prepare(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_prepare(conn, &sql),
        |driver: NativeDriver, disp| {
            let stmt = sql_core::native_facade_prepare(conn, &sql, disp);
            if stmt >= 0 {
                track_token(stmt, &driver);
            }
            stmt
        }
    )
}

pub(crate) fn native_conn_begin(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_begin(conn),
        |driver: NativeDriver, disp| {
            let tx = sql_core::native_facade_begin(conn, 0, disp);
            if tx >= 0 {
                track_token(tx, &driver);
            }
            tx
        }
    )
}

pub(crate) fn native_conn_begin_with(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let iso = arg_i64(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_begin_with(conn, iso),
        |driver: NativeDriver, disp| {
            let tx = sql_core::native_facade_begin(conn, iso, disp);
            if tx >= 0 {
                track_token(tx, &driver);
            }
            tx
        }
    )
}

pub(crate) fn native_conn_ping(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_ping(conn),
        |_driver: NativeDriver, disp| sql_core::native_facade_scalar(conn, op::PING, disp)
    )
}

pub(crate) fn native_conn_set_busy_timeout(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let ms = arg_i64(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_set_busy_timeout(conn, ms),
        |_driver: NativeDriver, disp| sql_core::native_facade_set_busy_timeout(conn, ms, disp)
    )
}

pub(crate) fn native_conn_interrupt(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_interrupt(conn),
        |_driver: NativeDriver, disp| sql_core::native_facade_scalar(conn, op::INTERRUPT, disp)
    )
}

pub(crate) fn native_conn_close(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    match driver_by_token(conn) {
        Some(driver) => {
            let rc = {
                let disp = dispatcher(dispatch, &driver);
                sql_core::native_facade_close(conn, disp)
            };
            forget_token(conn);
            Ok(Value::Int(rc))
        }
        None => Ok(Value::Int(sql_core::sql_conn_close(conn))),
    }
}

pub(crate) fn native_conn_execute(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    let params = arg_i64(args, 2);
    match driver_by_token(conn) {
        // A native connection has no prepared-statement façade step for
        // a bare execute: dispatch STMT_EXECUTE with a transient stmt.
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            let stmt = sql_core::native_facade_prepare(conn, &sql, &disp);
            if stmt < 0 {
                return Ok(Value::Int(stmt));
            }
            track_token(stmt, &driver);
            let n = sql_core::native_facade_execute(stmt, op::STMT_EXECUTE, &sql, params, &disp);
            let _ = sql_core::native_facade_stmt_close(stmt, &disp);
            forget_token(stmt);
            Ok(Value::Int(n))
        }
        None => Ok(Value::Int(sql_core::sql_conn_execute_params(
            conn, &sql, params,
        ))),
    }
}

pub(crate) fn native_conn_query(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    let params = arg_i64(args, 2);
    match driver_by_token(conn) {
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            let stmt = sql_core::native_facade_prepare(conn, &sql, &disp);
            if stmt < 0 {
                return Ok(Value::Int(stmt));
            }
            track_token(stmt, &driver);
            let rows = sql_core::native_facade_query(stmt, op::STMT_QUERY, &sql, params, &disp);
            if rows >= 0 {
                track_token(rows, &driver);
            }
            Ok(Value::Int(rows))
        }
        None => Ok(Value::Int(sql_core::sql_conn_query_params(
            conn, &sql, params,
        ))),
    }
}

pub(crate) fn native_conn_copy_in(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    let data = arg_bytes(args, 2);
    match driver_by_token(conn) {
        Some(driver) => {
            let disp = dispatcher(dispatch, &driver);
            Ok(Value::Int(sql_core::native_facade_copy_in(
                conn, &sql, data, disp,
            )))
        }
        None => Ok(Value::Int(sql_core::sql_conn_copy_in(conn, &sql, &data))),
    }
}

pub(crate) fn native_conn_copy_out_run(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    match driver_by_token(conn) {
        Some(driver) => {
            let disp = dispatcher(dispatch, &driver);
            Ok(Value::Int(sql_core::native_facade_copy_out(
                conn, &sql, disp,
            )))
        }
        None => Ok(Value::Int(run_rust_copy_out(conn, &sql))),
    }
}

pub(crate) fn native_conn_listen(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let channel = arg_str(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_listen(conn, &channel),
        |_driver: NativeDriver, disp| sql_core::native_facade_listen(
            conn,
            op::LISTEN,
            &channel,
            disp
        )
    )
}

pub(crate) fn native_conn_unlisten(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let channel = arg_str(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_unlisten(conn, &channel),
        |_driver: NativeDriver, disp| sql_core::native_facade_listen(
            conn,
            op::UNLISTEN,
            &channel,
            disp
        )
    )
}

pub(crate) fn native_conn_poll_notification(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let conn = arg_i64(args, 0);
    let timeout = arg_i64(args, 1);
    route!(
        dispatch,
        conn,
        sql_core::sql_conn_poll_notification(conn, timeout),
        |_driver: NativeDriver, disp| sql_core::native_facade_poll_notification(
            conn, timeout, disp
        )
    )
}

pub(crate) fn native_rows_next(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let rows = arg_i64(args, 0);
    route!(
        dispatch,
        rows,
        sql_core::sql_rows_next_row(rows),
        |_driver: NativeDriver, disp| {
            let r = sql_core::native_facade_rows_next(rows, disp);
            if r == 0 {
                forget_token(rows);
            }
            r
        }
    )
}

pub(crate) fn native_rows_close(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let rows = arg_i64(args, 0);
    match driver_by_token(rows) {
        Some(driver) => {
            let rc = {
                let disp = dispatcher(dispatch, &driver);
                sql_core::native_facade_rows_close(rows, disp)
            };
            forget_token(rows);
            Ok(Value::Int(rc))
        }
        None => Ok(Value::Int(sql_core::sql_rows_close(rows))),
    }
}

pub(crate) fn native_rows_columns(
    _dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let rows = arg_i64(args, 0);
    if sql_core::native_is_rows(rows) {
        Ok(Value::String(
            sql_core::native_facade_rows_columns(rows).into(),
        ))
    } else {
        Ok(Value::String(
            sql_core::sql_rows_columns_joined(rows).into(),
        ))
    }
}

pub(crate) fn native_stmt_execute(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let stmt = arg_i64(args, 0);
    let params = arg_i64(args, 1);
    match driver_by_token(stmt) {
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            Ok(Value::Int(sql_core::native_facade_execute(
                stmt,
                op::STMT_EXECUTE,
                "",
                params,
                disp,
            )))
        }
        None => Ok(Value::Int(sql_core::sql_stmt_execute(stmt, params))),
    }
}

pub(crate) fn native_stmt_query(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let stmt = arg_i64(args, 0);
    let params = arg_i64(args, 1);
    match driver_by_token(stmt) {
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            let rows = sql_core::native_facade_query(stmt, op::STMT_QUERY, "", params, disp);
            if rows >= 0 {
                track_token(rows, &driver);
            }
            Ok(Value::Int(rows))
        }
        None => Ok(Value::Int(sql_core::sql_stmt_query(stmt, params))),
    }
}

pub(crate) fn native_stmt_close(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let stmt = arg_i64(args, 0);
    match driver_by_token(stmt) {
        Some(driver) => {
            let rc = {
                let disp = dispatcher(dispatch, &driver);
                sql_core::native_facade_stmt_close(stmt, disp)
            };
            forget_token(stmt);
            Ok(Value::Int(rc))
        }
        None => Ok(Value::Int(sql_core::sql_stmt_close(stmt))),
    }
}

pub(crate) fn native_tx_commit(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let tx = arg_i64(args, 0);
    finish_tx(dispatch, tx, op::COMMIT, sql_core::sql_tx_commit(tx))
}

pub(crate) fn native_tx_rollback(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let tx = arg_i64(args, 0);
    finish_tx(dispatch, tx, op::ROLLBACK, sql_core::sql_tx_rollback(tx))
}

fn finish_tx(
    dispatch: &mut dyn NativeDispatch,
    tx: i64,
    op: i64,
    rust_fallback: i64,
) -> RuntimeResult<Value> {
    match driver_by_token(tx) {
        Some(driver) => {
            let rc = {
                let disp = dispatcher(dispatch, &driver);
                sql_core::native_facade_tx_finish(tx, op, disp)
            };
            forget_token(tx);
            Ok(Value::Int(rc))
        }
        None => Ok(Value::Int(rust_fallback)),
    }
}

pub(crate) fn native_tx_execute(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let tx = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    route!(
        dispatch,
        tx,
        sql_core::sql_tx_execute(tx, &sql),
        |_driver: NativeDriver, disp| sql_core::native_facade_execute(
            tx,
            op::TX_EXECUTE,
            &sql,
            Vec::new(),
            disp
        )
    )
}

pub(crate) fn native_tx_execute_params(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let tx = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    let params = arg_i64(args, 2);
    match driver_by_token(tx) {
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            Ok(Value::Int(sql_core::native_facade_execute(
                tx,
                op::TX_EXECUTE_PARAMS,
                &sql,
                params,
                disp,
            )))
        }
        None => Ok(Value::Int(sql_core::sql_tx_execute_params(
            tx, &sql, params,
        ))),
    }
}

pub(crate) fn native_tx_query_params(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let tx = arg_i64(args, 0);
    let sql = arg_str(args, 1);
    let params = arg_i64(args, 2);
    match driver_by_token(tx) {
        Some(driver) => {
            let params = take_params(params);
            let disp = dispatcher(dispatch, &driver);
            let rows = sql_core::native_facade_query(tx, op::TX_QUERY_PARAMS, &sql, params, disp);
            if rows >= 0 {
                track_token(rows, &driver);
            }
            Ok(Value::Int(rows))
        }
        None => Ok(Value::Int(sql_core::sql_tx_query_params(tx, &sql, params))),
    }
}

// --- helpers shared with the Rust-driver path ----------------------

fn take_params(params_handle: i64) -> Vec<gossamer_runtime::sql::Value> {
    sql_core::params_take_public(params_handle)
}

fn arg_bytes(args: &[Value], i: usize) -> Vec<u8> {
    match args.get(i) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| crate::builtins::value_to_int(v).unwrap_or(0) as u8)
            .collect(),
        Some(Value::IntArray(items)) => items.iter().map(|b| *b as u8).collect(),
        _ => Vec::new(),
    }
}

fn run_rust_copy_out(conn: i64, sql: &str) -> i64 {
    match sql_core::sql_conn_copy_out(conn, sql) {
        Some(bytes) => {
            let n = bytes.len() as i64;
            sql_core::sql_copy_out_store(conn, bytes);
            n
        }
        None => -1,
    }
}

/// Builds the `(name, Value::native)` pairs for the dispatch-receiving
/// native-driver builtins.
pub(crate) fn native_dispatch_builtins() -> Vec<(&'static str, NativeCall)> {
    vec![
        ("__gos_sql_register_native", native_register),
        ("__gos_sql_open_raw", native_open),
        ("__gos_sql_conn_prepare_raw", native_conn_prepare),
        ("__gos_sql_conn_begin_raw", native_conn_begin),
        ("__gos_sql_conn_begin_with_raw", native_conn_begin_with),
        ("__gos_sql_conn_ping_raw", native_conn_ping),
        (
            "__gos_sql_conn_set_busy_timeout_raw",
            native_conn_set_busy_timeout,
        ),
        ("__gos_sql_conn_interrupt_raw", native_conn_interrupt),
        ("__gos_sql_conn_close_raw", native_conn_close),
        ("__gos_sql_conn_execute_raw", native_conn_execute),
        ("__gos_sql_conn_query_raw", native_conn_query),
        ("__gos_sql_conn_copy_in_raw", native_conn_copy_in),
        ("__gos_sql_conn_copy_out_run_raw", native_conn_copy_out_run),
        ("__gos_sql_conn_listen_raw", native_conn_listen),
        ("__gos_sql_conn_unlisten_raw", native_conn_unlisten),
        (
            "__gos_sql_conn_poll_notification_raw",
            native_conn_poll_notification,
        ),
        ("__gos_sql_rows_next_row_raw", native_rows_next),
        ("__gos_sql_rows_close_raw", native_rows_close),
        ("__gos_sql_rows_columns_raw", native_rows_columns),
        ("__gos_sql_stmt_execute_raw", native_stmt_execute),
        ("__gos_sql_stmt_query_raw", native_stmt_query),
        ("__gos_sql_stmt_close_raw", native_stmt_close),
        ("__gos_sql_tx_commit_raw", native_tx_commit),
        ("__gos_sql_tx_rollback_raw", native_tx_rollback),
        ("__gos_sql_tx_execute_raw", native_tx_execute),
        ("__gos_sql_tx_execute_params_raw", native_tx_execute_params),
        ("__gos_sql_tx_query_params_raw", native_tx_query_params),
    ]
}

/// The pure side-channel helpers a `.gos` driver calls (no dispatch
/// re-entry needed - they only read/write slots + value handles).
pub(crate) fn native_helper_builtins() -> Vec<(&'static str, HelperFn)> {
    vec![
        ("__gos_sql_native_url", helper_url),
        ("__gos_sql_native_sql", helper_sql),
        ("__gos_sql_native_parent", helper_parent),
        ("__gos_sql_native_out_handle", helper_out_handle),
        ("__gos_sql_native_iso", helper_iso),
        ("__gos_sql_native_timeout", helper_timeout),
        ("__gos_sql_native_channel", helper_channel),
        ("__gos_sql_native_param_count", helper_param_count),
        ("__gos_sql_native_param", helper_param),
        ("__gos_sql_native_data", helper_data),
        ("__gos_sql_native_push_column", helper_push_column),
        ("__gos_sql_native_push_value", helper_push_value),
        ("__gos_sql_native_row_ready", helper_row_ready),
        ("__gos_sql_native_set_error", helper_set_error),
        ("__gos_sql_native_emit_bytes", helper_emit_bytes),
        ("__gos_sql_native_set_notification", helper_set_notification),
        ("__gos_sql_native_set_handle", helper_set_handle),
        ("__gos_sql_native_handle", helper_handle),
        ("__gos_sql_native_value_null", helper_value_null),
        ("__gos_sql_native_value_bool", helper_value_bool),
        ("__gos_sql_native_value_int", helper_value_int),
        ("__gos_sql_native_value_float", helper_value_float),
        ("__gos_sql_native_value_text", helper_value_text),
        ("__gos_sql_native_value_blob", helper_value_blob),
        ("__gos_sql_native_value_kind", helper_value_kind),
        ("__gos_sql_native_value_int_of", helper_value_int_of),
        ("__gos_sql_native_value_float_of", helper_value_float_of),
        ("__gos_sql_native_value_text_of", helper_value_text_of),
        ("__gos_sql_native_value_blob_of", helper_value_blob_of),
    ]
}

fn helper_url(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(sql_core::native_url(arg_i64(args, 0)).into()))
}

fn helper_sql(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(sql_core::native_sql(arg_i64(args, 0)).into()))
}

fn helper_parent(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_parent(arg_i64(args, 0))))
}

fn helper_out_handle(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_out_handle(arg_i64(args, 0))))
}

fn helper_iso(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_iso(arg_i64(args, 0))))
}

fn helper_timeout(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_timeout(arg_i64(args, 0))))
}

fn helper_channel(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::native_channel(arg_i64(args, 0)).into(),
    ))
}

fn helper_param_count(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_param_count(arg_i64(args, 0))))
}

fn helper_param(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_param(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn helper_data(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = sql_core::native_data(arg_i64(args, 0));
    Ok(Value::Array(Arc::new(
        bytes
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    )))
}

fn helper_push_column(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_push_column(arg_i64(args, 0), &arg_str(args, 1));
    Ok(Value::Unit)
}

fn helper_push_value(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_push_value(arg_i64(args, 0), arg_i64(args, 1));
    Ok(Value::Unit)
}

fn helper_row_ready(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_row_ready(arg_i64(args, 0));
    Ok(Value::Unit)
}

fn helper_set_error(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_set_error(arg_i64(args, 0), &arg_str(args, 1));
    Ok(Value::Unit)
}

fn helper_emit_bytes(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_emit_bytes(arg_i64(args, 0), &arg_bytes(args, 1));
    Ok(Value::Unit)
}

fn helper_set_notification(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_set_notification(
        arg_i64(args, 0),
        &arg_str(args, 1),
        &arg_str(args, 2),
        arg_i64(args, 3),
    );
    Ok(Value::Unit)
}

fn helper_set_handle(args: &[Value]) -> RuntimeResult<Value> {
    sql_core::native_set_handle(arg_i64(args, 0), arg_i64(args, 1));
    Ok(Value::Unit)
}

fn helper_handle(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_handle(arg_i64(args, 0))))
}

fn helper_value_null(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_null()))
}

fn helper_value_bool(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_bool(
        arg_i64(args, 0) != 0,
    )))
}

fn helper_value_int(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_int(arg_i64(args, 0))))
}

fn helper_value_float(args: &[Value]) -> RuntimeResult<Value> {
    let f = match args.first() {
        Some(Value::Float(f)) => *f,
        other => other
            .and_then(crate::builtins::value_to_int)
            .map_or(0.0, |n| n as f64),
    };
    Ok(Value::Int(sql_core::native_value_float(f)))
}

fn helper_value_text(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_text(&arg_str(args, 0))))
}

fn helper_value_blob(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_blob(&arg_bytes(args, 0))))
}

fn helper_value_kind(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_kind(arg_i64(args, 0))))
}

fn helper_value_int_of(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::native_value_int_of(arg_i64(args, 0))))
}

fn helper_value_float_of(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(sql_core::native_value_float_of(arg_i64(
        args, 0,
    ))))
}

fn helper_value_text_of(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::native_value_text_of(arg_i64(args, 0)).into(),
    ))
}

fn helper_value_blob_of(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = sql_core::native_value_blob_of(arg_i64(args, 0));
    Ok(Value::Array(Arc::new(
        bytes
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    )))
}
