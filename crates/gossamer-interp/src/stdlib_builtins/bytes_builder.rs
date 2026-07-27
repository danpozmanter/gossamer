#![allow(
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::unnecessary_wraps,
    clippy::type_complexity
)]
//! `std::bytes` builtins for the bytecode VM - the `Builder` (string
//! assembly) and `Buffer` (byte accumulation) handle types plus the
//! stateless `index_of` / `split` / `replace` helpers.
//!
//! Each handle is a struct `{ __builder: id }` / `{ __buffer: id }`;
//! the real accumulator lives in a process-global registry keyed by
//! `id`, so `&mut self` methods mutate through the registry instead of
//! relying on the VM's receiver write-back (mirrors `math::rand::Rng`
//! and `sync::Map`). Global (not `thread_local!`) so a handle minted
//! on one goroutine worker thread resolves on another. Handles are
//! never reclaimed (they leak at process exit), matching the compiled
//! tier's opaque-`Box` lifetime.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use gossamer_ast::Ident;
use gossamer_std::bytes::{index_of, replace, split};

use super::string_array;
use crate::builtins::{BuiltinFnPub, as_str, none_variant, some_variant, value_to_int};
use crate::value::{RuntimeError, RuntimeResult, Value};

static BUILDER_REGISTRY: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, String>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static BUFFER_REGISTRY: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, Vec<u8>>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static NEXT_BYTES_ID: AtomicI64 = AtomicI64::new(1);

fn with_builders<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, String>>) -> R) -> R {
    let guard = BUILDER_REGISTRY.lock();
    f(&guard)
}

fn with_buffers<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, Vec<u8>>>) -> R) -> R {
    let guard = BUFFER_REGISTRY.lock();
    f(&guard)
}

pub(crate) fn install_bytes_builder(globals: &mut Vec<(&'static str, Value)>) {
    // Handle constructors + methods: register the bare name and the
    // `bytes::`-qualified form. The struct handles are named
    // `bytes::Builder` / `bytes::Buffer`, so `qualified_method_key`
    // emits `bytes::Builder::write` etc., which the `bytes::{name}`
    // spelling covers; the bare name covers a direct `Builder::new()`.
    let handle_entries: &[(&str, BuiltinFnPub)] = &[
        ("Builder::new", builtin_builder_new),
        ("Builder::with_capacity", builtin_builder_new),
        ("Builder::write", builtin_builder_write),
        ("Builder::write_char", builtin_builder_write_char),
        ("Builder::build", builtin_builder_build),
        ("Builder::as_str", builtin_builder_build),
        ("Builder::len", builtin_builder_len),
        ("Buffer::new", builtin_buffer_new),
        ("Buffer::with_capacity", builtin_buffer_new),
        ("Buffer::write_str", builtin_buffer_write_str),
        ("Buffer::push", builtin_buffer_push),
        ("Buffer::len", builtin_buffer_len),
        ("Buffer::is_empty", builtin_buffer_is_empty),
        ("Buffer::clear", builtin_buffer_clear),
        ("Buffer::to_string", builtin_buffer_to_string),
    ];
    for (name, call) in handle_entries {
        let q: &'static str = Box::leak(format!("bytes::{name}").into_boxed_str());
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
        globals.push((q, crate::builtins::builtin_pub(q, *call)));
    }

    // Stateless free functions: `bytes::`-qualified only, so the bare
    // `split` / `replace` / `index_of` names don't shadow other
    // modules' free functions of the same name.
    let free_entries: &[(&str, BuiltinFnPub)] = &[
        ("bytes::index_of", builtin_bytes_index_of),
        ("bytes::split", builtin_bytes_split),
        ("bytes::replace", builtin_bytes_replace),
    ];
    for (name, call) in free_entries {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

// ---------------------------------------------------------------
// bytes::Builder
// ---------------------------------------------------------------

fn builder_handle(id: i64) -> Value {
    Value::struct_("bytes::Builder", vec![("__builder", Value::Int(id))])
}

fn builder_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "bytes::Builder" {
            for (i, v) in &inner.fields {
                if (*i) == "__builder" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn builtin_builder_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_BYTES_ID.fetch_add(1, Ordering::Relaxed);
    with_builders(|r| {
        r.borrow_mut().insert(id, String::new());
    });
    Ok(builder_handle(id))
}

pub(crate) fn builtin_builder_write(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(builder_id_of) else {
        return Ok(Value::Unit);
    };
    let text = args
        .get(1)
        .and_then(as_str)
        .ok_or_else(|| RuntimeError::Type("Builder::write expects a String".to_string()))?
        .to_string();
    with_builders(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            s.push_str(&text);
        }
    });
    Ok(Value::Unit)
}

pub(crate) fn builtin_builder_write_char(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(builder_id_of) else {
        return Ok(Value::Unit);
    };
    let Some(Value::Char(ch)) = args.get(1) else {
        return Err(RuntimeError::Type(
            "Builder::write_char expects a char".to_string(),
        ));
    };
    with_builders(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            s.push(*ch);
        }
    });
    Ok(Value::Unit)
}

pub(crate) fn builtin_builder_build(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(builder_id_of) else {
        return Ok(Value::String(String::new().into()));
    };
    let s = with_builders(|r| r.borrow().get(&id).cloned()).unwrap_or_default();
    Ok(Value::String(s.into()))
}

pub(crate) fn builtin_builder_len(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(builder_id_of) else {
        return Ok(Value::Int(0));
    };
    let n = with_builders(|r| r.borrow().get(&id).map_or(0, String::len));
    Ok(Value::Int(n as i64))
}

// ---------------------------------------------------------------
// bytes::Buffer
// ---------------------------------------------------------------

fn buffer_handle(id: i64) -> Value {
    Value::struct_("bytes::Buffer", vec![("__buffer", Value::Int(id))])
}

fn buffer_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "bytes::Buffer" {
            for (i, v) in &inner.fields {
                if (*i) == "__buffer" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn builtin_buffer_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_BYTES_ID.fetch_add(1, Ordering::Relaxed);
    with_buffers(|r| {
        r.borrow_mut().insert(id, Vec::new());
    });
    Ok(buffer_handle(id))
}

pub(crate) fn builtin_buffer_write_str(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::Unit);
    };
    let text = args
        .get(1)
        .and_then(as_str)
        .ok_or_else(|| RuntimeError::Type("Buffer::write_str expects a String".to_string()))?
        .to_string();
    with_buffers(|r| {
        if let Some(b) = r.borrow_mut().get_mut(&id) {
            b.extend_from_slice(text.as_bytes());
        }
    });
    Ok(Value::Unit)
}

pub(crate) fn builtin_buffer_push(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::Unit);
    };
    let byte = args
        .get(1)
        .and_then(value_to_int)
        .and_then(|value| u8::try_from(value).ok())
        .ok_or_else(|| RuntimeError::Type("Buffer::push expects a u8".to_string()))?;
    with_buffers(|r| {
        if let Some(b) = r.borrow_mut().get_mut(&id) {
            b.push(byte);
        }
    });
    Ok(Value::Unit)
}

pub(crate) fn builtin_buffer_len(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::Int(0));
    };
    let n = with_buffers(|r| r.borrow().get(&id).map_or(0, Vec::len));
    Ok(Value::Int(n as i64))
}

pub(crate) fn builtin_buffer_is_empty(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::Bool(true));
    };
    let empty = with_buffers(|r| r.borrow().get(&id).is_none_or(Vec::is_empty));
    Ok(Value::Bool(empty))
}

pub(crate) fn builtin_buffer_clear(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::Unit);
    };
    with_buffers(|r| {
        if let Some(b) = r.borrow_mut().get_mut(&id) {
            b.clear();
        }
    });
    Ok(Value::Unit)
}

pub(crate) fn builtin_buffer_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(buffer_id_of) else {
        return Ok(Value::String(String::new().into()));
    };
    let bytes = with_buffers(|r| r.borrow().get(&id).cloned()).unwrap_or_default();
    let s = String::from_utf8_lossy(&bytes).into_owned();
    Ok(Value::String(s.into()))
}

// ---------------------------------------------------------------
// Stateless helpers
// ---------------------------------------------------------------

pub(crate) fn builtin_bytes_index_of(args: &[Value]) -> RuntimeResult<Value> {
    let haystack = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    Ok(match index_of(haystack.as_bytes(), needle.as_bytes()) {
        Some(i) => some_variant(Value::Int(i as i64)),
        None => none_variant(),
    })
}

pub(crate) fn builtin_bytes_split(args: &[Value]) -> RuntimeResult<Value> {
    let haystack = args.first().and_then(as_str).unwrap_or("");
    let sep = args.get(1).and_then(as_str).unwrap_or("");
    let parts: Vec<String> = split(haystack.as_bytes(), sep.as_bytes())
        .into_iter()
        .map(|chunk| String::from_utf8_lossy(&chunk).into_owned())
        .collect();
    Ok(string_array(parts))
}

pub(crate) fn builtin_bytes_replace(args: &[Value]) -> RuntimeResult<Value> {
    let haystack = args.first().and_then(as_str).unwrap_or("");
    let from = args.get(1).and_then(as_str).unwrap_or("");
    let to = args.get(2).and_then(as_str).unwrap_or("");
    let out = replace(haystack.as_bytes(), from.as_bytes(), to.as_bytes());
    Ok(Value::String(
        String::from_utf8_lossy(&out).into_owned().into(),
    ))
}
