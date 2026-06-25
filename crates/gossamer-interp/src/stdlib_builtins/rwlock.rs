#![allow(
    unused_imports,
    dead_code,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value
)]
//! `std::sync::RwLock` builtins for the bytecode VM - a reader-writer
//! lock guarding a single `i64`. The handle is a struct carrying an
//! `id`; the real `parking_lot::RwLock` lives in a process-global
//! registry keyed by `id` (mirrors `sync::Map` / `sync::Once`), so a
//! handle minted on one goroutine worker thread resolves on another.
//!
//! `with_read` / `with_write` are `native` builtins so the closure can
//! be invoked through the interpreter dispatcher - a plain
//! `BuiltinFnPub` cannot call the callback. They are registered as
//! free data-last calls (`sync::RwLock::with_read(lock, f)`), the same
//! shape as `sync::Once::call`, and are the bit-identical VM mirror of
//! the compiled `gos_rt_rwlock_with_read` / `_with_write` shims. The
//! guarded value is an `i64` for this first cut; a String-guarded
//! variant is a documented follow-up.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use gossamer_ast::Ident;

use crate::builtins::{BuiltinFnPub, value_to_int};
use crate::value::{NativeDispatch, RuntimeResult, Value};

static RWLOCK_REGISTRY: LazyLock<
    parking_lot::Mutex<StdHashMap<i64, Arc<parking_lot::RwLock<i64>>>>,
> = LazyLock::new(|| parking_lot::Mutex::new(StdHashMap::new()));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

pub(crate) fn install_rwlock(globals: &mut Vec<(&'static str, Value)>) {
    let plain: &[(&str, BuiltinFnPub)] = &[
        ("RwLock::new", builtin_rwlock_new),
        ("sync::RwLock::new", builtin_rwlock_new),
        ("sync::RwLock::get", builtin_rwlock_get),
        ("sync::RwLock::set", builtin_rwlock_set),
    ];
    for (name, call) in plain {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
    // `with_read` / `with_write` run a closure, so they must be native.
    let native: &[&str] = &[
        "sync::RwLock::with_read",
        "RwLock::with_read",
        "sync::RwLock::with_write",
        "RwLock::with_write",
    ];
    for name in native {
        let call = if name.ends_with("with_read") {
            native_rwlock_with_read
        } else {
            native_rwlock_with_write
        };
        globals.push((*name, Value::native(name, call)));
    }
}

fn lock_handle(id: i64) -> Value {
    Value::struct_(
        "sync::RwLock",
        Arc::unwrap_or_clone(Arc::new(vec![("__rwlock", Value::Int(id))])),
    )
}

fn lock_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "sync::RwLock" {
            for (i, v) in &inner.fields {
                if (*i) == "__rwlock" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn arc_of(id: i64) -> Option<Arc<parking_lot::RwLock<i64>>> {
    RWLOCK_REGISTRY.lock().get(&id).cloned()
}

pub(crate) fn builtin_rwlock_new(args: &[Value]) -> RuntimeResult<Value> {
    let init = args.first().and_then(value_to_int).unwrap_or(0);
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    RWLOCK_REGISTRY
        .lock()
        .insert(id, Arc::new(parking_lot::RwLock::new(init)));
    Ok(lock_handle(id))
}

pub(crate) fn builtin_rwlock_get(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(lock_id_of)
        .and_then(arc_of)
        .map(|a| *a.read())
        .unwrap_or(0);
    Ok(Value::Int(v))
}

pub(crate) fn builtin_rwlock_set(args: &[Value]) -> RuntimeResult<Value> {
    let val = args.get(1).and_then(value_to_int).unwrap_or(0);
    if let Some(arc) = args.first().and_then(lock_id_of).and_then(arc_of) {
        *arc.write() = val;
    }
    Ok(Value::Unit)
}

pub(crate) fn native_rwlock_with_read(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let Some(arc) = args.first().and_then(lock_id_of).and_then(arc_of) else {
        return Ok(Value::Int(0));
    };
    let Some(f) = args.get(1).cloned() else {
        return Ok(Value::Int(0));
    };
    let value = *arc.read();
    dispatch.call_value(&f, vec![Value::Int(value)])
}

pub(crate) fn native_rwlock_with_write(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let Some(arc) = args.first().and_then(lock_id_of).and_then(arc_of) else {
        return Ok(Value::Int(0));
    };
    let Some(f) = args.get(1).cloned() else {
        return Ok(Value::Int(0));
    };
    let mut guard = arc.write();
    let current = *guard;
    let result = dispatch.call_value(&f, vec![Value::Int(current)])?;
    let next = value_to_int(&result).unwrap_or(current);
    *guard = next;
    Ok(Value::Int(next))
}
