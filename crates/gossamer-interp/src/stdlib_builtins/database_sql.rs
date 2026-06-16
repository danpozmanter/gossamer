#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::unnecessary_wraps,
    clippy::module_name_repetitions
)]
//! VM bindings for the `__gos_sql_*_raw` leaf intrinsics
//! behind the injected `std::database::sql` wrappers
//! (gossamer-parse autoderive).
//!
//! Every builtin delegates to the safe core in
//! `gossamer_runtime::c_abi::sql`, the same functions the compiled
//! tiers' `gos_rt_sql_*` shims marshal to — one implementation, one
//! handle registry, identical semantics on every tier.

use std::sync::Arc;

use gossamer_runtime::c_abi::sql as sql_core;
use gossamer_runtime::sql::Value as SqlValue;

use crate::builtins::{BuiltinFnPub, as_str, builtin_pub, value_to_int};
use crate::value::{RuntimeResult, Value};

pub(crate) fn install_database_sql(globals: &mut Vec<(&'static str, Value)>) {
    for (name, call) in [
        ("__gos_sql_open_raw", builtin_sql_open_raw as BuiltinFnPub),
        ("__gos_sql_last_error_raw", builtin_sql_last_error_raw),
        ("__gos_sql_drivers_raw", builtin_sql_drivers_raw),
        ("__gos_sql_params_new_raw", builtin_sql_params_new_raw),
        (
            "__gos_sql_params_push_null_raw",
            builtin_sql_params_push_null_raw,
        ),
        (
            "__gos_sql_params_push_bool_raw",
            builtin_sql_params_push_bool_raw,
        ),
        (
            "__gos_sql_params_push_int_raw",
            builtin_sql_params_push_int_raw,
        ),
        (
            "__gos_sql_params_push_float_raw",
            builtin_sql_params_push_float_raw,
        ),
        (
            "__gos_sql_params_push_text_raw",
            builtin_sql_params_push_text_raw,
        ),
        (
            "__gos_sql_params_push_blob_raw",
            builtin_sql_params_push_blob_raw,
        ),
        ("__gos_sql_conn_execute_raw", builtin_sql_conn_execute_raw),
        ("__gos_sql_conn_query_raw", builtin_sql_conn_query_raw),
        ("__gos_sql_conn_begin_raw", builtin_sql_conn_begin_raw),
        (
            "__gos_sql_conn_begin_with_raw",
            builtin_sql_conn_begin_with_raw,
        ),
        ("__gos_sql_conn_ping_raw", builtin_sql_conn_ping_raw),
        (
            "__gos_sql_conn_set_busy_timeout_raw",
            builtin_sql_conn_set_busy_timeout_raw,
        ),
        (
            "__gos_sql_conn_interrupt_raw",
            builtin_sql_conn_interrupt_raw,
        ),
        ("__gos_sql_conn_close_raw", builtin_sql_conn_close_raw),
        ("__gos_sql_rows_next_row_raw", builtin_sql_rows_next_row_raw),
        ("__gos_sql_rows_close_raw", builtin_sql_rows_close_raw),
        ("__gos_sql_rows_columns_raw", builtin_sql_rows_columns_raw),
        ("__gos_sql_row_kind_raw", builtin_sql_row_kind_raw),
        ("__gos_sql_row_get_i64_raw", builtin_sql_row_get_i64_raw),
        ("__gos_sql_row_get_f64_raw", builtin_sql_row_get_f64_raw),
        ("__gos_sql_row_get_bool_raw", builtin_sql_row_get_bool_raw),
        ("__gos_sql_row_get_text_raw", builtin_sql_row_get_text_raw),
        ("__gos_sql_row_get_blob_raw", builtin_sql_row_get_blob_raw),
        ("__gos_sql_row_width_raw", builtin_sql_row_width_raw),
        ("__gos_sql_tx_commit_raw", builtin_sql_tx_commit_raw),
        ("__gos_sql_tx_rollback_raw", builtin_sql_tx_rollback_raw),
        ("__gos_sql_tx_execute_raw", builtin_sql_tx_execute_raw),
        ("__gos_sql_tx_savepoint_raw", builtin_sql_tx_savepoint_raw),
        (
            "__gos_sql_tx_release_savepoint_raw",
            builtin_sql_tx_release_savepoint_raw,
        ),
        (
            "__gos_sql_tx_rollback_to_savepoint_raw",
            builtin_sql_tx_rollback_to_savepoint_raw,
        ),
        (
            "__gos_sql_tx_execute_params_raw",
            builtin_sql_tx_execute_params_raw,
        ),
        (
            "__gos_sql_tx_query_params_raw",
            builtin_sql_tx_query_params_raw,
        ),
        ("__gos_sql_conn_prepare_raw", builtin_sql_conn_prepare_raw),
        ("__gos_sql_stmt_execute_raw", builtin_sql_stmt_execute_raw),
        ("__gos_sql_stmt_query_raw", builtin_sql_stmt_query_raw),
        ("__gos_sql_stmt_close_raw", builtin_sql_stmt_close_raw),
        ("__gos_sql_conn_copy_in_raw", builtin_sql_conn_copy_in_raw),
        (
            "__gos_sql_conn_copy_out_run_raw",
            builtin_sql_conn_copy_out_run_raw,
        ),
        (
            "__gos_sql_conn_copy_out_take_raw",
            builtin_sql_conn_copy_out_take_raw,
        ),
        ("__gos_sql_conn_listen_raw", builtin_sql_conn_listen_raw),
        ("__gos_sql_conn_unlisten_raw", builtin_sql_conn_unlisten_raw),
        (
            "__gos_sql_conn_poll_notification_raw",
            builtin_sql_conn_poll_notification_raw,
        ),
        (
            "__gos_sql_notification_channel_raw",
            builtin_sql_notification_channel_raw,
        ),
        (
            "__gos_sql_notification_payload_raw",
            builtin_sql_notification_payload_raw,
        ),
        (
            "__gos_sql_notification_pid_raw",
            builtin_sql_notification_pid_raw,
        ),
        ("__gos_sql_pool_new_raw", builtin_sql_pool_new_raw),
        ("__gos_sql_pool_get_raw", builtin_sql_pool_get_raw),
        ("__gos_sql_pool_live_raw", builtin_sql_pool_live_raw),
        ("__gos_sql_pool_idle_raw", builtin_sql_pool_idle_raw),
        (
            "__gos_sql_pool_close_idle_raw",
            builtin_sql_pool_close_idle_raw,
        ),
        ("__gos_sql_migrate_up_raw", builtin_sql_migrate_up_raw),
    ] {
        globals.push((name, builtin_pub(name, call)));
    }
}

fn arg_str(args: &[Value], i: usize) -> String {
    args.get(i).and_then(as_str).unwrap_or("").to_string()
}

fn arg_i64(args: &[Value], i: usize) -> i64 {
    args.get(i).and_then(value_to_int).unwrap_or(0)
}

fn arg_f64(args: &[Value], i: usize) -> f64 {
    match args.get(i) {
        Some(Value::Float(f)) => *f,
        Some(other) => value_to_int(other).map_or(0.0, |n| n as f64),
        None => 0.0,
    }
}

fn arg_bytes(args: &[Value], i: usize) -> Vec<u8> {
    match args.get(i) {
        Some(Value::Array(items)) => items
            .iter()
            .map(|v| value_to_int(v).unwrap_or(0) as u8)
            .collect(),
        _ => Vec::new(),
    }
}

fn builtin_sql_open_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_open_handle(
        &arg_str(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_last_error_raw(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(sql_core::sql_take_last_error().into()))
}

fn builtin_sql_drivers_raw(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(sql_core::sql_drivers_joined().into()))
}

fn builtin_sql_params_new_raw(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_new()))
}

fn builtin_sql_params_push_null_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Null,
    )))
}

fn builtin_sql_params_push_bool_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Bool(arg_i64(args, 1) != 0),
    )))
}

fn builtin_sql_params_push_int_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Int(arg_i64(args, 1)),
    )))
}

fn builtin_sql_params_push_float_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Float(arg_f64(args, 1)),
    )))
}

fn builtin_sql_params_push_text_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Text(arg_str(args, 1)),
    )))
}

fn builtin_sql_params_push_blob_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_params_push(
        arg_i64(args, 0),
        SqlValue::Blob(arg_bytes(args, 1)),
    )))
}

fn builtin_sql_conn_execute_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_execute_params(
        arg_i64(args, 0),
        &arg_str(args, 1),
        arg_i64(args, 2),
    )))
}

fn builtin_sql_conn_query_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_query_params(
        arg_i64(args, 0),
        &arg_str(args, 1),
        arg_i64(args, 2),
    )))
}

fn builtin_sql_conn_begin_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_begin(arg_i64(args, 0))))
}

fn builtin_sql_conn_begin_with_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_begin_with(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn builtin_sql_conn_ping_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_ping(arg_i64(args, 0))))
}

fn builtin_sql_conn_set_busy_timeout_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_set_busy_timeout(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn builtin_sql_conn_interrupt_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_interrupt(arg_i64(args, 0))))
}

fn builtin_sql_conn_close_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_close(arg_i64(args, 0))))
}

fn builtin_sql_rows_next_row_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_rows_next_row(arg_i64(args, 0))))
}

fn builtin_sql_rows_close_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_rows_close(arg_i64(args, 0))))
}

fn builtin_sql_rows_columns_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::sql_rows_columns_joined(arg_i64(args, 0)).into(),
    ))
}

fn builtin_sql_row_kind_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_row_kind(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_row_get_i64_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_row_get_i64(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_row_get_f64_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(sql_core::sql_row_get_f64(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_row_get_bool_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_row_get_bool(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_row_get_text_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::sql_row_get_text(arg_i64(args, 0), &arg_str(args, 1)).into(),
    ))
}

fn builtin_sql_row_get_blob_raw(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = sql_core::sql_row_get_blob(arg_i64(args, 0), &arg_str(args, 1));
    Ok(Value::Array(Arc::new(
        bytes
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    )))
}

fn builtin_sql_row_width_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_row_width(arg_i64(args, 0))))
}

fn builtin_sql_tx_commit_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_commit(arg_i64(args, 0))))
}

fn builtin_sql_tx_rollback_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_rollback(arg_i64(args, 0))))
}

fn builtin_sql_tx_execute_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_execute(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_tx_savepoint_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_savepoint(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_tx_release_savepoint_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_release_savepoint(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_tx_rollback_to_savepoint_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_rollback_to_savepoint(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_tx_execute_params_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_execute_params(
        arg_i64(args, 0),
        &arg_str(args, 1),
        arg_i64(args, 2),
    )))
}

fn builtin_sql_tx_query_params_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_tx_query_params(
        arg_i64(args, 0),
        &arg_str(args, 1),
        arg_i64(args, 2),
    )))
}

fn builtin_sql_conn_prepare_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_prepare(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_stmt_execute_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_stmt_execute(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn builtin_sql_stmt_query_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_stmt_query(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn builtin_sql_stmt_close_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_stmt_close(arg_i64(args, 0))))
}

fn builtin_sql_conn_copy_in_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_copy_in(
        arg_i64(args, 0),
        &arg_str(args, 1),
        &arg_bytes(args, 2),
    )))
}

fn builtin_sql_conn_copy_out_run_raw(args: &[Value]) -> RuntimeResult<Value> {
    let handle = arg_i64(args, 0);
    match sql_core::sql_conn_copy_out(handle, &arg_str(args, 1)) {
        Some(bytes) => {
            let n = bytes.len() as i64;
            sql_core::sql_copy_out_store(handle, bytes);
            Ok(Value::Int(n))
        }
        None => Ok(Value::Int(-1)),
    }
}

fn builtin_sql_conn_copy_out_take_raw(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = sql_core::sql_copy_out_take(arg_i64(args, 0));
    Ok(Value::Array(Arc::new(
        bytes
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    )))
}

fn builtin_sql_conn_listen_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_listen(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_conn_unlisten_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_unlisten(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}

fn builtin_sql_conn_poll_notification_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_conn_poll_notification(
        arg_i64(args, 0),
        arg_i64(args, 1),
    )))
}

fn builtin_sql_notification_channel_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::sql_notification_channel(arg_i64(args, 0)).into(),
    ))
}

fn builtin_sql_notification_payload_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(
        sql_core::sql_notification_payload(arg_i64(args, 0)).into(),
    ))
}

fn builtin_sql_notification_pid_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_notification_pid(arg_i64(args, 0))))
}

fn builtin_sql_pool_new_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_pool_new(
        &arg_str(args, 0),
        &arg_str(args, 1),
        arg_i64(args, 2),
        arg_i64(args, 3),
        arg_i64(args, 4),
        arg_i64(args, 5),
        arg_i64(args, 6),
    )))
}

fn builtin_sql_pool_get_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_pool_get(arg_i64(args, 0))))
}

fn builtin_sql_pool_live_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_pool_live(arg_i64(args, 0))))
}

fn builtin_sql_pool_idle_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_pool_idle(arg_i64(args, 0))))
}

fn builtin_sql_pool_close_idle_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_pool_close_idle(arg_i64(args, 0))))
}

fn builtin_sql_migrate_up_raw(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(sql_core::sql_migrate_up(
        arg_i64(args, 0),
        &arg_str(args, 1),
    )))
}
