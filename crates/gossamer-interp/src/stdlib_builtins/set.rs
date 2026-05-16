#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Wires up Gossamer-callable builtins for stdlib modules whose
//! Rust-side implementation already exists but had no user-facing
//! exposure. Each `install_*` helper is invoked from
//! `builtins::install` so user code that writes
//! `strings::join`, `strconv::parse_i64`, `net::TcpStream::connect`,
//! `time::Instant::now`, etc. resolves to a real callable.
//!
//! All builtins return a `Result`-shaped variant (`Ok` / `Err`) on
//! fallible operations so callers can chain `?` without wrapping.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::io::Read as IoRead;
use std::sync::Arc;

use gossamer_ast::Ident;

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
use gossamer_std::net as net_std;
use gossamer_std::os as os_std;
use gossamer_std::path as path_std;
use gossamer_std::strconv as strconv_std;
use gossamer_std::strings as strings_std;
use gossamer_std::unicode as unicode_std;
use gossamer_std::utf8 as utf8_std;

use gossamer_std::iter as iter_std;
use gossamer_std::utf16 as utf16_std;

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, install_module_pub, none_variant, ok_variant, some_variant,
    value_to_int,
};
use crate::value::{MapKey, NativeCall, NativeDispatch, RuntimeResult, Value};

/// Entry point invoked from `builtins::install`.
use super::*;

pub(crate) fn install_set(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("HashSet::new", builtin_set_new),
        ("HashSet::insert", builtin_set_insert),
        ("HashSet::remove", builtin_set_remove),
        ("HashSet::contains", builtin_set_contains),
        ("HashSet::len", builtin_set_len),
        ("HashSet::is_empty", builtin_set_is_empty),
        ("HashSet::clear", builtin_set_clear),
        ("HashSet::to_vec", builtin_set_to_vec),
        ("HashSet::iter", builtin_set_to_vec),
        ("collections::HashSet::new", builtin_set_new),
    ];
    for (name, call) in entries {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

pub(crate) fn next_set_handle() -> i64 {
    NEXT_SET_HANDLE.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

pub(crate) fn set_handle(id: i64) -> Value {
    Value::struct_(
        "HashSet",
        Arc::new(vec![(Ident::new("__set"), Value::Int(id))]),
    )
}

pub(crate) fn set_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "HashSet" {
            for (i, v) in inner.fields.iter() {
                if i.name == "__set" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

pub(crate) fn builtin_set_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = next_set_handle();
    SET_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, std::collections::HashSet::new());
    });
    Ok(set_handle(id))
}

pub(crate) fn builtin_set_insert(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args.first().cloned().unwrap_or(Value::Unit);
    let Some(id) = set_id_of(&handle) else {
        return Ok(handle);
    };
    let Some(value) = args.get(1) else {
        return Ok(handle);
    };
    let key = MapKey::from_value(value);
    SET_REGISTRY.with(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            let _ = s.insert(key);
        }
    });
    // Return the handle so the VM writeback-move is idempotent.
    Ok(handle)
}

pub(crate) fn builtin_set_remove(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args.first().cloned().unwrap_or(Value::Unit);
    let Some(id) = set_id_of(&handle) else {
        return Ok(handle);
    };
    let Some(value) = args.get(1) else {
        return Ok(handle);
    };
    let key = MapKey::from_value(value);
    SET_REGISTRY.with(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            let _ = s.remove(&key);
        }
    });
    // Return the handle so the VM writeback-move is idempotent.
    Ok(handle)
}

pub(crate) fn builtin_set_contains(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Bool(false));
    };
    let Some(value) = args.get(1) else {
        return Ok(Value::Bool(false));
    };
    let key = MapKey::from_value(value);
    let has = SET_REGISTRY.with(|r| r.borrow().get(&id).is_some_and(|s| s.contains(&key)));
    Ok(Value::Bool(has))
}

pub(crate) fn builtin_set_len(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Int(0));
    };
    let n = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map_or(0, std::collections::HashSet::len)
    });
    Ok(Value::Int(n as i64))
}

pub(crate) fn builtin_set_is_empty(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Bool(true));
    };
    let empty = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .is_none_or(std::collections::HashSet::is_empty)
    });
    Ok(Value::Bool(empty))
}

pub(crate) fn builtin_set_clear(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(set_id_of) {
        SET_REGISTRY.with(|r| {
            if let Some(set) = r.borrow_mut().get_mut(&id) {
                set.clear();
            }
        });
    }
    // Return the receiver so the VM writeback preserves the struct handle.
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

pub(crate) fn builtin_set_to_vec(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Array(Arc::new(Vec::new())));
    };
    let values: Vec<Value> = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|s| s.iter().map(MapKey::to_value).collect())
            .unwrap_or_default()
    });
    Ok(Value::Array(Arc::new(values)))
}

// ----------------------------------------------------------------------
// sync extras (atomics + mutex + once)

use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

thread_local! {
    pub(crate) static NEXT_ATOMIC_ID: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static ATOMIC_I64_REGISTRY: RefCell<StdHashMap<i64, Arc<StdAtomicI64>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static ATOMIC_BOOL_REGISTRY: RefCell<StdHashMap<i64, Arc<StdAtomicBool>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static MUTEX_REGISTRY: RefCell<StdHashMap<i64, Arc<parking_lot::Mutex<Value>>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static ONCE_REGISTRY: RefCell<StdHashMap<i64, Arc<parking_lot::Once>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local, clippy::type_complexity)]
    pub(crate) static SYNC_MAP_REGISTRY: RefCell<
        StdHashMap<i64, Arc<parking_lot::RwLock<StdHashMap<String, String>>>>,
    > = RefCell::new(StdHashMap::new());
}

pub(crate) fn next_atomic_id() -> i64 {
    NEXT_ATOMIC_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

pub(crate) fn atomic_handle(name: &'static str, id: i64) -> Value {
    Value::struct_(
        name,
        Arc::new(vec![(Ident::new("__atomic"), Value::Int(id))]),
    )
}

pub(crate) fn atomic_id_of(value: &Value, expected: &str) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == expected {
            for (i, v) in inner.fields.iter() {
                if i.name == "__atomic" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}
