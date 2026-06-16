//! VM builtins for `std::collections::VecDeque<T>`.
//!
//! On the bytecode tier, a deque value is represented as
//! `Value::Struct { name: "VecDeque", fields: [("__deque", id)] }`
//! where `id` is a handle into `DEQUE_REGISTRY`. Method dispatch
//! routes through `qualified_key("VecDeque", method)` so each method
//! must be registered under the `"VecDeque::<method>"` name.

use std::cell::RefCell;
use std::collections::{HashMap as StdHashMap, VecDeque as StdVecDeque};
use std::sync::Arc;

use gossamer_ast::Ident;

use crate::builtins::{BuiltinFnPub, builtin_pub, none_variant, some_variant};
use crate::value::{RuntimeResult, Value};

thread_local! {
    static NEXT_DEQUE_HANDLE: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    static DEQUE_REGISTRY: RefCell<StdHashMap<i64, StdVecDeque<Value>>> =
        RefCell::new(StdHashMap::new());
}

/// Install all VecDeque VM builtins into the global table.
pub(crate) fn install_deque(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("VecDeque::new", builtin_deque_new),
        ("collections::VecDeque::new", builtin_deque_new),
        ("VecDeque::push_back", builtin_deque_push_back),
        ("VecDeque::pop_front", builtin_deque_pop_front),
        ("VecDeque::len", builtin_deque_len),
        ("VecDeque::is_empty", builtin_deque_is_empty),
    ];
    for (name, call) in entries {
        globals.push((*name, builtin_pub(name, *call)));
    }
}

fn next_deque_handle() -> i64 {
    NEXT_DEQUE_HANDLE.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

fn deque_handle(id: i64) -> Value {
    Value::struct_(
        "VecDeque",
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__deque"), Value::Int(id))])),
    )
}

fn deque_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "VecDeque" {
            for (i, v) in &inner.fields {
                if i.name == "__deque" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn builtin_deque_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = next_deque_handle();
    DEQUE_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, StdVecDeque::new());
    });
    Ok(deque_handle(id))
}

fn builtin_deque_push_back(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args.first().cloned().unwrap_or(Value::Unit);
    let Some(id) = deque_id_of(&handle) else {
        return Ok(handle);
    };
    let value = args.get(1).cloned().unwrap_or(Value::Unit);
    DEQUE_REGISTRY.with(|r| {
        if let Some(d) = r.borrow_mut().get_mut(&id) {
            d.push_back(value);
        }
    });
    Ok(handle)
}

fn builtin_deque_pop_front(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(deque_id_of) else {
        return Ok(none_variant());
    };
    let result = DEQUE_REGISTRY.with(|r| r.borrow_mut().get_mut(&id).and_then(|d| d.pop_front()));
    match result {
        Some(v) => Ok(some_variant(v)),
        None => Ok(none_variant()),
    }
}

fn builtin_deque_len(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(deque_id_of) else {
        return Ok(Value::Int(0));
    };
    let n = DEQUE_REGISTRY.with(|r| r.borrow().get(&id).map_or(0, StdVecDeque::len));
    Ok(Value::Int(n as i64))
}

fn builtin_deque_is_empty(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(deque_id_of) else {
        return Ok(Value::Bool(true));
    };
    let empty = DEQUE_REGISTRY.with(|r| r.borrow().get(&id).is_none_or(StdVecDeque::is_empty));
    Ok(Value::Bool(empty))
}
