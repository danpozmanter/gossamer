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

pub(crate) fn install_http_websocket(globals: &mut Vec<(&'static str, Value)>) {
    for (name, call) in [
        ("accept_key", builtin_ws_accept_key as BuiltinFnPub),
        ("is_websocket_upgrade", builtin_ws_is_upgrade),
    ] {
        let qualified: &'static str =
            Box::leak(format!("http::websocket::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, call)));
        let short: &'static str = Box::leak(format!("websocket::{name}").into_boxed_str());
        globals.push((short, crate::builtins::builtin_pub(short, call)));
    }
}

pub(crate) const WS_GUID: &[u8] = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

pub(crate) fn builtin_ws_accept_key(args: &[Value]) -> RuntimeResult<Value> {
    let client_key = arg_str(args.first());
    let mut input = client_key.into_bytes();
    input.extend_from_slice(WS_GUID);
    let digest = gossamer_std::crypto::insecure::sha1(&input);
    let encoded = gossamer_std::encoding::base64::encode(&digest);
    Ok(Value::String(encoded.into()))
}

pub(crate) fn builtin_ws_is_upgrade(args: &[Value]) -> RuntimeResult<Value> {
    // Inspects a Request value's headers array for Upgrade +
    // Connection headers. The Request struct lays headers out
    // as Array<Tuple<String, String>>.
    let Some(Value::Struct(inner)) = args.first() else {
        return Ok(Value::Bool(false));
    };
    let mut has_upgrade_ws = false;
    let mut has_connection_upgrade = false;
    for (i, v) in &inner.fields {
        if i.name == "headers" {
            if let Value::Array(arr) = v {
                for entry in arr.iter() {
                    if let Value::Tuple(t) = entry {
                        if t.len() == 2 {
                            if let (Value::String(n), Value::String(val)) = (&t[0], &t[1]) {
                                let name = n.as_str().to_ascii_lowercase();
                                let value = val.as_str().to_ascii_lowercase();
                                if name == "upgrade" && value == "websocket" {
                                    has_upgrade_ws = true;
                                }
                                if name == "connection" && value.contains("upgrade") {
                                    has_connection_upgrade = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    Ok(Value::Bool(has_upgrade_ws && has_connection_upgrade))
}

// Router: free-fn API over a thread-local registry. The full
// method-chain shape (`r.get(...)`, `r.serve(req)`) lives in the
// follow-on bridge (#54); this surface is enough to write
// dispatchers in Gossamer source by hand.
thread_local! {
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static ROUTER_REGISTRY: RefCell<StdHashMap<i64, RefCell<RouterTable>>> =
        RefCell::new(StdHashMap::new());
    pub(crate) static NEXT_ROUTER_ID: RefCell<i64> = const { RefCell::new(1) };
}

#[derive(Default)]
pub(crate) struct RouterTable {
    pub(crate) routes: Vec<(String, String)>, // (method, pattern)
}

// Static router-builtin tables are kept at module scope rather than
// inside `install_http_router` so clippy's `items-after-statements`
// doesn't fire - and so we never need `Box::leak(format!(…))` for
// what is conceptually a static lookup. The previous shape leaked
// process-lifetime strings on first install, which leaksanitizer
// flagged on fuzz-target exit.
pub(crate) const ROUTER_FREE_FNS: &[(&str, &str, BuiltinFnPub)] = &[
    ("http::router::new", "router::new", builtin_router_new),
    ("http::router::add", "router::add", builtin_router_add),
    (
        "http::router::lookup",
        "router::lookup",
        builtin_router_lookup,
    ),
];

pub(crate) const ROUTER_METHODS: &[(&str, BuiltinFnPub)] = &[
    ("Router::get", builtin_router_method_get),
    ("Router::post", builtin_router_method_post),
    ("Router::put", builtin_router_method_put),
    ("Router::delete", builtin_router_method_delete),
    ("Router::patch", builtin_router_method_patch),
    ("Router::head", builtin_router_method_head),
    ("Router::options", builtin_router_method_options),
];
