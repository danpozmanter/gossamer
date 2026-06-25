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

use super::set::GlobalReg;

// Process-global (not `thread_local!`): goroutines run on an OS
// worker-thread pool, so a deque handle minted on one thread must
// resolve on another after the goroutine migrates between workers.
// Mirrors the `sync::*` registries.
static NEXT_DEQUE_HANDLE: GlobalReg<i64> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(1)));
static DEQUE_REGISTRY: GlobalReg<StdHashMap<i64, StdVecDeque<Value>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));

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
        Arc::unwrap_or_clone(Arc::new(vec![("__deque", Value::Int(id))])),
    )
}

fn deque_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "VecDeque" {
            for (i, v) in &inner.fields {
                if (*i) == "__deque" {
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

#[cfg(test)]
mod deque_registry_tests {
    use super::*;
    use std::thread;

    fn int_of(v: &Value) -> i64 {
        match v {
            Value::Int(n) => *n,
            other => panic!("expected Int, got {other:?}"),
        }
    }

    // A deque handle minted on one worker thread must stay usable from
    // another: goroutines migrate across the OS worker pool, so the
    // backing registry has to be process-global, not thread-local. A
    // thread-local registry would leave the handle's entry invisible
    // (an empty map) on the second thread, so the push would no-op and
    // `len` would read 0.
    #[test]
    fn deque_handle_survives_thread_boundary() {
        let handle = thread::spawn(|| builtin_deque_new(&[]).unwrap())
            .join()
            .unwrap();
        builtin_deque_push_back(&[handle.clone(), Value::Int(10)]).unwrap();
        builtin_deque_push_back(&[handle.clone(), Value::Int(20)]).unwrap();
        assert_eq!(
            int_of(&builtin_deque_len(std::slice::from_ref(&handle)).unwrap()),
            2
        );
        match builtin_deque_pop_front(std::slice::from_ref(&handle)).unwrap() {
            Value::Variant(inner) => {
                assert_eq!(inner.name, "Some");
                assert_eq!(int_of(&inner.fields[0]), 10);
            }
            other => panic!("expected Some, got {other:?}"),
        }
    }
}
