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

pub(crate) fn install_net(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "net",
        &[
            ("resolve", builtin_net_resolve),
            ("lookup", builtin_net_resolve),
        ],
        globals,
    );
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("TcpListener::bind", builtin_tcp_listener_bind),
        ("TcpListener::accept", builtin_tcp_listener_accept),
        ("TcpListener::local_addr", builtin_tcp_listener_local_addr),
        ("TcpListener::close", builtin_tcp_listener_close),
        ("TcpStream::connect", builtin_tcp_stream_connect),
        ("TcpStream::read", builtin_tcp_stream_read),
        (
            "TcpStream::read_to_string",
            builtin_tcp_stream_read_to_string,
        ),
        ("TcpStream::write", builtin_tcp_stream_write),
        ("TcpStream::write_all", builtin_tcp_stream_write),
        ("TcpStream::start_tls", builtin_tcp_stream_start_tls),
        (
            "TcpStream::start_tls_insecure",
            builtin_tcp_stream_start_tls_insecure,
        ),
        ("TcpStream::start_tls_ca", builtin_tcp_stream_start_tls_ca),
        ("TcpStream::close", builtin_tcp_stream_close),
        ("UdpSocket::bind", builtin_udp_bind),
        ("UdpSocket::send_to", builtin_udp_send_to),
        ("UdpSocket::recv_from", builtin_udp_recv_from),
        ("UdpSocket::local_addr", builtin_udp_local_addr),
        ("UdpSocket::close", builtin_udp_close),
        ("UnixListener::bind", builtin_unix_listener_bind),
        ("UnixListener::accept", builtin_unix_listener_accept),
        ("UnixListener::close", builtin_unix_listener_close),
        ("UnixStream::connect", builtin_unix_stream_connect),
        ("UnixStream::read", builtin_unix_stream_read),
        (
            "UnixStream::read_to_string",
            builtin_unix_stream_read_to_string,
        ),
        ("UnixStream::write", builtin_unix_stream_write),
        ("UnixStream::write_all", builtin_unix_stream_write),
        ("UnixStream::close", builtin_unix_stream_close),
    ];
    for (short, call) in entries {
        let qualified: &'static str = Box::leak(format!("net::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*short, crate::builtins::builtin_pub(short, *call)));
    }
    // Bare-name dispatch for method-call shape (`stream.read(n)`).
    for (short, call) in &[
        ("recv_from", builtin_udp_recv_from as BuiltinFnPub),
        ("send_to", builtin_udp_send_to as BuiltinFnPub),
        ("accept", builtin_tcp_listener_accept as BuiltinFnPub),
        (
            "local_addr",
            builtin_tcp_listener_local_addr as BuiltinFnPub,
        ),
    ] {
        globals.push((*short, crate::builtins::builtin_pub(short, *call)));
    }
}

pub(crate) fn builtin_net_resolve(args: &[Value]) -> RuntimeResult<Value> {
    let host = args.first().and_then(as_str).unwrap_or("");
    let needle = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:0")
    };
    match net_std::resolve(&needle) {
        Ok(addrs) => {
            let values: Vec<Value> = addrs
                .into_iter()
                .map(|a| Value::String(a.ip().to_string().into()))
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(values))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_tcp_listener_bind(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "TcpListener::bind", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::TcpListener::bind(&addr) {
        Ok(listener) => {
            let id = next_net_id();
            TCP_LISTENER_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(id, Arc::new(parking_lot::Mutex::new(listener)));
            });
            Ok(ok_variant(handle_struct("net::TcpListener", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_tcp_listener_accept(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpListener::accept: missing handle"));
    };
    let Some(listener) = fetch_socket(&TCP_LISTENER_REGISTRY, id) else {
        return Ok(err_variant("TcpListener::accept: stale handle"));
    };
    let res = listener.lock().accept().map_err(|e| e.to_string());
    match res {
        Ok((stream, addr)) => {
            let sid = next_net_id();
            TCP_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(sid, Arc::new(parking_lot::Mutex::new(Some(stream))));
            });
            let pair = Value::Tuple(Arc::new(vec![
                handle_struct("net::TcpStream", sid),
                Value::String(addr.to_string().into()),
            ]));
            Ok(ok_variant(pair))
        }
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_tcp_listener_local_addr(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpListener::local_addr: missing handle"));
    };
    let Some(listener) = fetch_socket(&TCP_LISTENER_REGISTRY, id) else {
        return Ok(err_variant("TcpListener::local_addr: stale handle"));
    };
    match listener.lock().local_addr() {
        Ok(addr) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Err(e) => Ok(err_variant(e.to_string())),
    }
}

pub(crate) fn builtin_tcp_listener_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        TCP_LISTENER_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_tcp_stream_connect(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "TcpStream::connect", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::TcpStream::connect(&addr) {
        Ok(stream) => {
            let id = next_net_id();
            TCP_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(id, Arc::new(parking_lot::Mutex::new(Some(stream))));
            });
            Ok(ok_variant(handle_struct("net::TcpStream", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_tcp_stream_read(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::read: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(4096)
        .clamp(1, 1 << 24);
    let res = if tls_has(id) {
        match fetch_socket(&TLS_STREAM_REGISTRY, id) {
            Some(arc) => {
                let mut buf = vec![0u8; max as usize];
                match arc.lock().read(&mut buf) {
                    Ok(n) => {
                        buf.truncate(n);
                        Ok(buf)
                    }
                    Err(e) => Err(e.to_string()),
                }
            }
            None => Err("TcpStream::read: stale handle".to_string()),
        }
    } else {
        match fetch_socket(&TCP_STREAM_REGISTRY, id) {
            Some(arc) => {
                let mut guard = arc.lock();
                match guard.as_mut() {
                    Some(stream) => {
                        let mut buf = vec![0u8; max as usize];
                        match stream.read(&mut buf) {
                            Ok(n) => {
                                buf.truncate(n);
                                Ok(buf)
                            }
                            Err(e) => Err(e.to_string()),
                        }
                    }
                    None => Err("TcpStream::read: closed handle".to_string()),
                }
            }
            None => Err("TcpStream::read: stale handle".to_string()),
        }
    };
    match res {
        Ok(bytes) => Ok(ok_variant(Value::Array(Arc::new(
            bytes
                .into_iter()
                .map(|b| Value::Int(i64::from(b)))
                .collect(),
        )))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_tcp_stream_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::read_to_string: missing handle"));
    };
    let read_all = |stream: &mut dyn FnMut(&mut [u8]) -> Result<usize, String>| {
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    };
    let res = if tls_has(id) {
        match fetch_socket(&TLS_STREAM_REGISTRY, id) {
            Some(arc) => {
                let mut guard = arc.lock();
                read_all(&mut |b| guard.read(b).map_err(|e| e.to_string()))
            }
            None => Err("TcpStream::read_to_string: stale handle".to_string()),
        }
    } else {
        match fetch_socket(&TCP_STREAM_REGISTRY, id) {
            Some(arc) => {
                let mut guard = arc.lock();
                match guard.as_mut() {
                    Some(stream) => read_all(&mut |b| stream.read(b).map_err(|e| e.to_string())),
                    None => Err("TcpStream::read_to_string: closed handle".to_string()),
                }
            }
            None => Err("TcpStream::read_to_string: stale handle".to_string()),
        }
    };
    match res {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_tcp_stream_write(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::write: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "TcpStream::write: expected string or byte array",
            ));
        }
    };
    let res = if tls_has(id) {
        match fetch_socket(&TLS_STREAM_REGISTRY, id) {
            Some(arc) => arc.lock().write_all(&bytes).map_err(|e| e.to_string()),
            None => Err("TcpStream::write: stale handle".to_string()),
        }
    } else {
        match fetch_socket(&TCP_STREAM_REGISTRY, id) {
            Some(arc) => {
                let mut guard = arc.lock();
                match guard.as_mut() {
                    Some(stream) => stream.write_all(&bytes).map_err(|e| e.to_string()),
                    None => Err("TcpStream::write: closed handle".to_string()),
                }
            }
            None => Err("TcpStream::write: stale handle".to_string()),
        }
    };
    match res {
        Ok(()) => Ok(ok_variant(Value::Int(bytes.len() as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

// --- Unix-domain sockets ------------------------------------------
//
// AF_UNIX stream sockets. The real implementation is `#[cfg(unix)]`;
// on non-unix targets every entry point returns an `Err`, matching the
// compiled tier's Windows stub.

#[cfg(unix)]
pub(crate) fn builtin_unix_listener_bind(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "UnixListener::bind", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match std::os::unix::net::UnixListener::bind(&path) {
        Ok(l) => {
            let id = next_net_id();
            UNIX_LISTENER_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(id, Arc::new(parking_lot::Mutex::new(l)));
            });
            Ok(ok_variant(handle_struct("net::UnixListener", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_listener_accept(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UnixListener::accept: missing handle"));
    };
    let Some(listener) = fetch_socket(&UNIX_LISTENER_REGISTRY, id) else {
        return Ok(err_variant("UnixListener::accept: stale handle"));
    };
    let res = listener.lock().accept().map_err(|e| e.to_string());
    match res {
        Ok((stream, addr)) => {
            let sid = next_net_id();
            UNIX_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(sid, Arc::new(parking_lot::Mutex::new(stream)));
            });
            let addr_str = addr
                .as_pathname()
                .map(|p| p.to_string_lossy().into_owned())
                .unwrap_or_default();
            let pair = Value::Tuple(Arc::new(vec![
                handle_struct("net::UnixStream", sid),
                Value::String(addr_str.into()),
            ]));
            Ok(ok_variant(pair))
        }
        Err(e) => Ok(err_variant(e)),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_listener_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        UNIX_LISTENER_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

#[cfg(unix)]
pub(crate) fn builtin_unix_stream_connect(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "UnixStream::connect", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match std::os::unix::net::UnixStream::connect(&path) {
        Ok(s) => {
            let id = next_net_id();
            UNIX_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(id, Arc::new(parking_lot::Mutex::new(s)));
            });
            Ok(ok_variant(handle_struct("net::UnixStream", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_stream_read(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UnixStream::read: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(4096)
        .clamp(1, 1 << 24);
    let res = match fetch_socket(&UNIX_STREAM_REGISTRY, id) {
        Some(arc) => {
            let mut buf = vec![0u8; max as usize];
            match arc.lock().read(&mut buf) {
                Ok(n) => {
                    buf.truncate(n);
                    Ok(buf)
                }
                Err(e) => Err(e.to_string()),
            }
        }
        None => Err("UnixStream::read: stale handle".to_string()),
    };
    match res {
        Ok(bytes) => Ok(ok_variant(Value::Array(Arc::new(
            bytes
                .into_iter()
                .map(|b| Value::Int(i64::from(b)))
                .collect(),
        )))),
        Err(e) => Ok(err_variant(e)),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_stream_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UnixStream::read_to_string: missing handle"));
    };
    let res = match fetch_socket(&UNIX_STREAM_REGISTRY, id) {
        Some(arc) => {
            let mut guard = arc.lock();
            let mut out = Vec::new();
            let mut chunk = [0u8; 4096];
            loop {
                match guard.read(&mut chunk) {
                    Ok(0) => break Ok(String::from_utf8_lossy(&out).into_owned()),
                    Ok(n) => out.extend_from_slice(&chunk[..n]),
                    Err(e) => break Err(e.to_string()),
                }
            }
        }
        None => Err("UnixStream::read_to_string: stale handle".to_string()),
    };
    match res {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(e)),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_stream_write(args: &[Value]) -> RuntimeResult<Value> {
    use std::io::Write as _;
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UnixStream::write: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "UnixStream::write: expected string or byte array",
            ));
        }
    };
    let res = match fetch_socket(&UNIX_STREAM_REGISTRY, id) {
        Some(arc) => arc.lock().write_all(&bytes).map_err(|e| e.to_string()),
        None => Err("UnixStream::write: stale handle".to_string()),
    };
    match res {
        Ok(()) => Ok(ok_variant(Value::Int(bytes.len() as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

#[cfg(unix)]
pub(crate) fn builtin_unix_stream_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        UNIX_STREAM_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

#[cfg(not(unix))]
fn unix_unsupported(op: &str) -> RuntimeResult<Value> {
    Ok(err_variant(format!(
        "net::{op}: Unix-domain sockets are not supported on this platform"
    )))
}

#[cfg(not(unix))]
pub(crate) fn builtin_unix_listener_bind(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixListener::bind")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_listener_accept(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixListener::accept")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_listener_close(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Unit)
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_stream_connect(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixStream::connect")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_stream_read(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixStream::read")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_stream_read_to_string(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixStream::read_to_string")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_stream_write(_args: &[Value]) -> RuntimeResult<Value> {
    unix_unsupported("UnixStream::write")
}
#[cfg(not(unix))]
pub(crate) fn builtin_unix_stream_close(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Unit)
}

/// True when `id` names an upgraded TLS stream rather than a plaintext
/// socket. `read` / `write` / `close` consult this so the existing
/// `net::TcpStream` method surface drives either transport.
fn tls_has(id: i64) -> bool {
    TLS_STREAM_REGISTRY.with(|r| r.borrow().contains_key(&id))
}

pub(crate) fn builtin_tcp_stream_start_tls(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::start_tls: missing handle"));
    };
    let host = match arg_str_at(args, 1, "TcpStream::start_tls", "host") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let Some(stream) = take_tcp_stream(id) else {
        return Ok(err_variant("TcpStream::start_tls: stale handle"));
    };
    match stream.start_tls(&host) {
        Ok(tls) => {
            let nid = next_net_id();
            TLS_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(nid, Arc::new(parking_lot::Mutex::new(tls)));
            });
            Ok(ok_variant(handle_struct("net::TcpStream", nid)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

/// Removes the plaintext stream behind `id` and moves the inner
/// `TcpStream` out by value, so a TLS upgrade's blocking handshake runs
/// with no registry lock and no per-socket lock held.
fn take_tcp_stream(id: i64) -> Option<net_std::TcpStream> {
    let arc = TCP_STREAM_REGISTRY.with(|r| r.borrow_mut().remove(&id))?;
    arc.lock().take()
}

pub(crate) fn builtin_tcp_stream_start_tls_insecure(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::start_tls_insecure: missing handle"));
    };
    let host = match arg_str_at(args, 1, "TcpStream::start_tls_insecure", "host") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let Some(stream) = take_tcp_stream(id) else {
        return Ok(err_variant("TcpStream::start_tls_insecure: stale handle"));
    };
    match stream.start_tls_insecure(&host) {
        Ok(tls) => {
            let nid = next_net_id();
            TLS_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(nid, Arc::new(parking_lot::Mutex::new(tls)));
            });
            Ok(ok_variant(handle_struct("net::TcpStream", nid)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_tcp_stream_start_tls_ca(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::start_tls_ca: missing handle"));
    };
    let host = match arg_str_at(args, 1, "TcpStream::start_tls_ca", "host") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let ca_pem = match arg_str_at(args, 2, "TcpStream::start_tls_ca", "ca_pem") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let Some(stream) = take_tcp_stream(id) else {
        return Ok(err_variant("TcpStream::start_tls_ca: stale handle"));
    };
    match stream.start_tls_ca(&host, &ca_pem) {
        Ok(tls) => {
            let nid = next_net_id();
            TLS_STREAM_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(nid, Arc::new(parking_lot::Mutex::new(tls)));
            });
            Ok(ok_variant(handle_struct("net::TcpStream", nid)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_tcp_stream_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        TCP_STREAM_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
        TLS_STREAM_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_udp_bind(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "UdpSocket::bind", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::UdpSocket::bind(&addr) {
        Ok(sock) => {
            let id = next_net_id();
            UDP_REGISTRY.with(|r| {
                r.borrow_mut()
                    .insert(id, Arc::new(parking_lot::Mutex::new(sock)));
            });
            Ok(ok_variant(handle_struct("net::UdpSocket", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

pub(crate) fn builtin_udp_send_to(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::send_to: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "UdpSocket::send_to: expected string or byte array",
            ));
        }
    };
    let addr = args.get(2).and_then(as_str).unwrap_or("").to_string();
    let res = match fetch_socket(&UDP_REGISTRY, id) {
        Some(arc) => arc.lock().send_to(&bytes, &addr).map_err(|e| e.to_string()),
        None => Err("UdpSocket::send_to: stale handle".to_string()),
    };
    match res {
        Ok(n) => Ok(ok_variant(Value::Int(n as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_udp_recv_from(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::recv_from: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(1500)
        .clamp(1, 1 << 16);
    let res = match fetch_socket(&UDP_REGISTRY, id) {
        Some(arc) => {
            let mut buf = vec![0u8; max as usize];
            match arc.lock().recv_from(&mut buf) {
                Ok((n, addr)) => {
                    buf.truncate(n);
                    Ok((buf, addr))
                }
                Err(e) => Err(e.to_string()),
            }
        }
        None => Err("UdpSocket::recv_from: stale handle".to_string()),
    };
    match res {
        Ok((bytes, addr)) => {
            let bytes_v = Value::Array(Arc::new(
                bytes
                    .into_iter()
                    .map(|b| Value::Int(i64::from(b)))
                    .collect(),
            ));
            Ok(ok_variant(Value::Tuple(Arc::new(vec![
                bytes_v,
                Value::String(addr.to_string().into()),
            ]))))
        }
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_udp_local_addr(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::local_addr: missing handle"));
    };
    let Some(arc) = fetch_socket(&UDP_REGISTRY, id) else {
        return Ok(err_variant("UdpSocket::local_addr: stale handle"));
    };
    match arc.lock().local_addr() {
        Ok(addr) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Err(e) => Ok(err_variant(e.to_string())),
    }
}

pub(crate) fn builtin_udp_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        UDP_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

#[cfg(test)]
mod net_registry_tests {
    use super::*;
    use std::thread;

    fn unwrap_ok(v: Value) -> Value {
        match v {
            Value::Variant(inner) if inner.name == "Ok" => inner.fields[0].clone(),
            other => panic!("expected Ok variant, got {other:?}"),
        }
    }

    fn str_of(v: &Value) -> String {
        match v {
            Value::String(s) => s.to_string(),
            other => panic!("expected String, got {other:?}"),
        }
    }

    fn bytes_of(v: &Value) -> Vec<u8> {
        match v {
            Value::Array(arr) => arr
                .iter()
                .map(|x| match x {
                    Value::Int(n) => *n as u8,
                    other => panic!("expected Int byte, got {other:?}"),
                })
                .collect(),
            other => panic!("expected Array, got {other:?}"),
        }
    }

    fn byte_array(bytes: &[u8]) -> Value {
        Value::Array(Arc::new(
            bytes.iter().map(|b| Value::Int(i64::from(*b))).collect(),
        ))
    }

    // Reads from `stream` until `want` bytes arrive or the peer closes
    // (an empty read = EOF). `read` busy-polls inside `net_std` until
    // data is ready, so this blocks on readiness, not a fixed sleep.
    fn read_exact(stream: &Value, want: usize) -> Vec<u8> {
        let mut got = Vec::new();
        while got.len() < want {
            let chunk = bytes_of(&unwrap_ok(
                builtin_tcp_stream_read(&[stream.clone(), Value::Int(want as i64)]).unwrap(),
            ));
            if chunk.is_empty() {
                break;
            }
            got.extend_from_slice(&chunk);
        }
        got
    }

    // A socket handle minted on one OS worker thread must stay usable
    // from another: goroutines migrate across the worker pool, so the
    // backing registry has to be process-global, not thread-local. A
    // thread-local registry would make `local_addr` on the second thread
    // report a stale handle (empty map), and the ephemeral port assigned
    // at bind would be invisible.
    #[test]
    fn listener_handle_survives_thread_boundary() {
        let handle = thread::spawn(|| {
            unwrap_ok(builtin_tcp_listener_bind(&[Value::String("127.0.0.1:0".into())]).unwrap())
        })
        .join()
        .unwrap();

        let addr_v = {
            let handle = handle.clone();
            thread::spawn(move || {
                unwrap_ok(builtin_tcp_listener_local_addr(std::slice::from_ref(&handle)).unwrap())
            })
            .join()
            .unwrap()
        };

        let addr = str_of(&addr_v);
        assert!(addr.starts_with("127.0.0.1:"), "addr = {addr}");
        assert!(
            !addr.ends_with(":0"),
            "ephemeral port should be bound: {addr}"
        );

        builtin_tcp_listener_close(std::slice::from_ref(&handle)).unwrap();
    }

    // End-to-end loopback round trip exercising the accept-then-handle
    // pattern across a thread boundary: the accepted stream's handle is
    // created on the accepting thread and used on a different thread.
    // While the handler is parked in `read`, it holds only its own
    // per-socket mutex - the client thread keeps touching the global
    // registry (connect / write / read), which would deadlock if the
    // registry lock were held across blocking I/O. Readiness comes from
    // connect-after-bind; no fixed sleep.
    #[test]
    fn accepted_stream_round_trips_across_thread_boundary() {
        let listener =
            unwrap_ok(builtin_tcp_listener_bind(&[Value::String("127.0.0.1:0".into())]).unwrap());
        let addr = str_of(&unwrap_ok(
            builtin_tcp_listener_local_addr(std::slice::from_ref(&listener)).unwrap(),
        ));

        let client = thread::spawn(move || {
            let stream =
                unwrap_ok(builtin_tcp_stream_connect(&[Value::String(addr.into())]).unwrap());
            unwrap_ok(
                builtin_tcp_stream_write(&[stream.clone(), Value::String("ping".into())]).unwrap(),
            );
            let echoed = read_exact(&stream, 4);
            builtin_tcp_stream_close(std::slice::from_ref(&stream)).unwrap();
            echoed
        });

        let accepted =
            unwrap_ok(builtin_tcp_listener_accept(std::slice::from_ref(&listener)).unwrap());
        let stream = match accepted {
            Value::Tuple(parts) => parts[0].clone(),
            other => panic!("expected (stream, addr) tuple, got {other:?}"),
        };

        let handler = thread::spawn(move || {
            let request = read_exact(&stream, 4);
            unwrap_ok(builtin_tcp_stream_write(&[stream.clone(), byte_array(&request)]).unwrap());
            builtin_tcp_stream_close(std::slice::from_ref(&stream)).unwrap();
        });

        handler.join().unwrap();
        let echoed = client.join().unwrap();
        assert_eq!(echoed, b"ping", "round-trip payload mismatch");

        builtin_tcp_listener_close(std::slice::from_ref(&listener)).unwrap();
    }
}
