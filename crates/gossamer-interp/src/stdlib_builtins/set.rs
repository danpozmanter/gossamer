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
        ("HashSet::union", builtin_set_union),
        ("HashSet::intersection", builtin_set_intersection),
        ("HashSet::difference", builtin_set_difference),
        (
            "HashSet::symmetric_difference",
            builtin_set_symmetric_difference,
        ),
        ("HashSet::is_subset", builtin_set_is_subset),
        ("HashSet::is_superset", builtin_set_is_superset),
        ("HashSet::is_disjoint", builtin_set_is_disjoint),
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
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__set"), Value::Int(id))])),
    )
}

pub(crate) fn set_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "HashSet" {
            for (i, v) in &inner.fields {
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
            .map(|s| {
                // Sort for deterministic, cross-tier-identical order — a
                // `HashSet`'s iteration order is otherwise unstable
                // (RandomState) and differs run-to-run and across tiers.
                let mut keys: Vec<MapKey> = s.iter().cloned().collect();
                keys.sort();
                keys.iter().map(MapKey::to_value).collect()
            })
            .unwrap_or_default()
    });
    Ok(Value::Array(Arc::new(values)))
}

fn set_pair_ids(args: &[Value]) -> Option<(i64, i64)> {
    Some((
        args.first().and_then(set_id_of)?,
        args.get(1).and_then(set_id_of)?,
    ))
}

/// Runs a binary set operation over the two operand handles and stores the
/// result under a fresh handle.
fn set_binary_op(
    args: &[Value],
    op: impl Fn(
        &std::collections::HashSet<MapKey>,
        &std::collections::HashSet<MapKey>,
    ) -> std::collections::HashSet<MapKey>,
) -> RuntimeResult<Value> {
    let result = match set_pair_ids(args) {
        Some((a, b)) => SET_REGISTRY.with(|r| {
            let r = r.borrow();
            let sa = r.get(&a).cloned().unwrap_or_default();
            let sb = r.get(&b).cloned().unwrap_or_default();
            op(&sa, &sb)
        }),
        None => std::collections::HashSet::new(),
    };
    let id = next_set_handle();
    SET_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, result);
    });
    Ok(set_handle(id))
}

fn set_predicate(
    args: &[Value],
    pred: impl Fn(&std::collections::HashSet<MapKey>, &std::collections::HashSet<MapKey>) -> bool,
) -> RuntimeResult<Value> {
    let result = match set_pair_ids(args) {
        Some((a, b)) => SET_REGISTRY.with(|r| {
            let r = r.borrow();
            let sa = r.get(&a).cloned().unwrap_or_default();
            let sb = r.get(&b).cloned().unwrap_or_default();
            pred(&sa, &sb)
        }),
        None => false,
    };
    Ok(Value::Bool(result))
}

pub(crate) fn builtin_set_union(args: &[Value]) -> RuntimeResult<Value> {
    set_binary_op(args, |a, b| a.union(b).cloned().collect())
}

pub(crate) fn builtin_set_intersection(args: &[Value]) -> RuntimeResult<Value> {
    set_binary_op(args, |a, b| a.intersection(b).cloned().collect())
}

pub(crate) fn builtin_set_difference(args: &[Value]) -> RuntimeResult<Value> {
    set_binary_op(args, |a, b| a.difference(b).cloned().collect())
}

pub(crate) fn builtin_set_symmetric_difference(args: &[Value]) -> RuntimeResult<Value> {
    set_binary_op(args, |a, b| a.symmetric_difference(b).cloned().collect())
}

pub(crate) fn builtin_set_is_subset(args: &[Value]) -> RuntimeResult<Value> {
    set_predicate(args, |a, b| a.is_subset(b))
}

pub(crate) fn builtin_set_is_superset(args: &[Value]) -> RuntimeResult<Value> {
    set_predicate(args, |a, b| a.is_superset(b))
}

pub(crate) fn builtin_set_is_disjoint(args: &[Value]) -> RuntimeResult<Value> {
    set_predicate(args, |a, b| a.is_disjoint(b))
}

// ----------------------------------------------------------------------
// sync extras (atomics + mutex + once)

use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

/// Process-global registry shared across goroutines. The sync
/// primitives (atomics, mutex, once, map) hand user code an integer
/// handle; the real `Arc<...>` lives here keyed by that handle. This
/// MUST be global rather than `thread_local!`: goroutines run on an OS
/// worker-thread pool, so a handle minted on one thread has to resolve
/// on another (a `thread_local!` registry silently lost every
/// cross-goroutine update). A `ReentrantMutex<RefCell<_>>` keeps the
/// old `.with(|r| r.borrow()/.borrow_mut())` call sites unchanged while
/// serializing cross-thread access; same-thread reentrancy still hits
/// RefCell's borrow checks exactly as the previous thread_local did.
pub(crate) struct GlobalReg<T: 'static>(LazyLock<parking_lot::ReentrantMutex<RefCell<T>>>);

impl<T: 'static> GlobalReg<T> {
    pub(crate) const fn new(init: fn() -> parking_lot::ReentrantMutex<RefCell<T>>) -> Self {
        Self(LazyLock::new(init))
    }

    pub(crate) fn with<R>(&self, f: impl FnOnce(&RefCell<T>) -> R) -> R {
        let guard = self.0.lock();
        f(&guard)
    }
}

pub(crate) static NEXT_ATOMIC_ID: GlobalReg<i64> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(1)));
pub(crate) static ATOMIC_I64_REGISTRY: GlobalReg<StdHashMap<i64, Arc<StdAtomicI64>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
pub(crate) static ATOMIC_BOOL_REGISTRY: GlobalReg<StdHashMap<i64, Arc<StdAtomicBool>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
pub(crate) static MUTEX_REGISTRY: GlobalReg<StdHashMap<i64, Arc<MutexCell>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));

/// Backing state for an interpreter `sync::Mutex`. The lock is held
/// from a `lock()` call until the matching `unlock()`, so a
/// read-modify-write on shared state performed by user code between
/// the two is serialized across goroutines — mirroring the compiled
/// tier's `gos_rt_mutex_lock`/`gos_rt_mutex_unlock`. Acquisition parks
/// the contending goroutine's worker thread on the condvar (the same
/// blocking discipline `Channel::recv` uses) rather than spinning.
pub(crate) struct MutexCell {
    /// `true` while a goroutine holds the lock.
    held: parking_lot::Mutex<bool>,
    /// Signalled by `unlock()` so one parked acquirer can proceed.
    available: parking_lot::Condvar,
    /// Protected payload, readable on `lock()` and writable via
    /// `store()`. Guarded by its own short-held lock so it never
    /// contends with the held mutual-exclusion lock.
    value: parking_lot::Mutex<Value>,
}

impl MutexCell {
    /// Creates an unlocked cell protecting `value`.
    pub(crate) fn new(value: Value) -> Self {
        Self {
            held: parking_lot::Mutex::new(false),
            available: parking_lot::Condvar::new(),
            value: parking_lot::Mutex::new(value),
        }
    }

    /// Acquires the lock, parking until it is free, and returns a
    /// clone of the protected value.
    pub(crate) fn lock(&self) -> Value {
        let mut held = self.held.lock();
        while *held {
            self.available.wait(&mut held);
        }
        *held = true;
        drop(held);
        self.value.lock().clone()
    }

    /// Releases the lock and wakes one parked acquirer.
    pub(crate) fn unlock(&self) {
        let mut held = self.held.lock();
        *held = false;
        drop(held);
        self.available.notify_one();
    }

    /// Overwrites the protected value.
    pub(crate) fn store(&self, value: Value) {
        *self.value.lock() = value;
    }
}
pub(crate) static ONCE_REGISTRY: GlobalReg<StdHashMap<i64, Arc<parking_lot::Once>>> =
    GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
#[allow(clippy::type_complexity)]
pub(crate) static SYNC_MAP_REGISTRY: GlobalReg<
    StdHashMap<i64, Arc<parking_lot::RwLock<StdHashMap<String, String>>>>,
> = GlobalReg::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));

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
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__atomic"), Value::Int(id))])),
    )
}

pub(crate) fn atomic_id_of(value: &Value, expected: &str) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == expected {
            for (i, v) in &inner.fields {
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
