//! `std::lifecycle` builtins - process readiness and graceful shutdown.
//!
//! Every one delegates to `gossamer_runtime::c_abi::lifecycle`, the same
//! process-global state the compiled tiers' shims read and write, so a
//! program behaves identically under `gos run` and a native build.

use gossamer_runtime::c_abi::lifecycle as lc;

use crate::value::{RuntimeResult, Value};

/// Registers the `lifecycle::*` builtins.
pub(crate) fn install_lifecycle(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, crate::builtins::BuiltinFnPub)] = &[
        ("lifecycle::ready", builtin_ready),
        ("lifecycle::set_ready", builtin_set_ready),
        ("lifecycle::is_ready", builtin_is_ready),
        ("lifecycle::shutdown", builtin_shutdown),
        ("lifecycle::is_shutting_down", builtin_is_shutting_down),
        ("lifecycle::await_shutdown", builtin_await_shutdown),
        ("lifecycle::notify_status", builtin_notify_status),
    ];
    for (name, call) in entries {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn builtin_ready(_args: &[Value]) -> RuntimeResult<Value> {
    lc::set_ready(true);
    Ok(Value::Unit)
}

fn builtin_set_ready(args: &[Value]) -> RuntimeResult<Value> {
    lc::set_ready(matches!(args.first(), Some(Value::Bool(true))));
    Ok(Value::Unit)
}

fn builtin_is_ready(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(lc::is_ready()))
}

fn builtin_shutdown(_args: &[Value]) -> RuntimeResult<Value> {
    lc::begin_shutdown();
    // The VM's contexts live in the interpreter's own registry, so the
    // runtime's shutdown walk cannot reach them.
    super::context::cancel_live_requests();
    Ok(Value::Unit)
}

fn builtin_is_shutting_down(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(lc::is_shutting_down()))
}

fn builtin_await_shutdown(_args: &[Value]) -> RuntimeResult<Value> {
    lc::await_shutdown();
    Ok(Value::Unit)
}

fn builtin_notify_status(args: &[Value]) -> RuntimeResult<Value> {
    let message = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(other) => format!("{other}"),
        None => String::new(),
    };
    lc::notify_status(&message);
    Ok(Value::Unit)
}
