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
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
#[cfg(not(target_arch = "wasm32"))]
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
use crate::value::{MapKey, NativeCall, NativeDispatch, RuntimeError, RuntimeResult, Value};

/// Entry point invoked from `builtins::install`.
use super::*;

pub(crate) fn install_sync_barrier(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("Barrier::new", builtin_barrier_new),
        ("Barrier::wait", builtin_barrier_wait),
    ];
    for (name, call) in entries {
        let qualified: &'static str = Box::leak(format!("sync::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

pub(crate) fn barrier_handle(id: i64) -> Value {
    Value::struct_(
        "sync::Barrier",
        Arc::unwrap_or_clone(Arc::new(vec![("__barrier", Value::Int(id))])),
    )
}

pub(crate) fn barrier_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        for (ident, v) in &inner.fields {
            if (*ident) == "__barrier" {
                if let Value::Int(n) = v {
                    return Some(*n);
                }
            }
        }
    }
    None
}

pub(crate) fn with_barrier<R>(
    value: &Value,
    f: impl FnOnce(&Arc<gossamer_std::sync::Barrier>) -> R,
) -> Option<R> {
    let id = barrier_id_of(value)?;
    BARRIER_REGISTRY.with(|r| r.borrow().get(&id).map(f))
}

pub(crate) fn builtin_barrier_new(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(1);
    if n <= 0 {
        return Err(RuntimeError::Type(
            "Barrier::new: count must be positive".to_string(),
        ));
    }
    let n = usize::try_from(n)
        .map_err(|_| RuntimeError::Type("Barrier::new: count is too large".to_string()))?;
    let id = next_atomic_id();
    BARRIER_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(gossamer_std::sync::Barrier::new(n)));
    });
    Ok(barrier_handle(id))
}

pub(crate) fn builtin_barrier_wait(args: &[Value]) -> RuntimeResult<Value> {
    // Clone the `Arc<Barrier>` out and drop the registry lock BEFORE
    // blocking on the rendezvous. Calling `wait()` inside the registry
    // `with` closure would hold the global lock across the block, so the
    // first participant to arrive would never release it and every other
    // participant would deadlock trying to look up the same barrier.
    if let Some(handle) = args.first() {
        if let Some(id) = barrier_id_of(handle) {
            let arc = BARRIER_REGISTRY.with(|r| r.borrow().get(&id).cloned());
            if let Some(b) = arc {
                b.wait();
            }
        }
    }
    Ok(Value::Unit)
}

// ----------------------------------------------------------------------
// crypto breadth (sha512, blake3, aead, ed25519, ecdsa, kdf, x509)
