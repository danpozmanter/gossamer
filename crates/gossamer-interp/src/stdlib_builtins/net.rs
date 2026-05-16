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
        ("TcpStream::close", builtin_tcp_stream_close),
        ("UdpSocket::bind", builtin_udp_bind),
        ("UdpSocket::send_to", builtin_udp_send_to),
        ("UdpSocket::recv_from", builtin_udp_recv_from),
        ("UdpSocket::local_addr", builtin_udp_local_addr),
        ("UdpSocket::close", builtin_udp_close),
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
                r.borrow_mut().insert(id, listener);
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
    let res = TCP_LISTENER_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(listener) = reg.get_mut(&id) else {
            return Err("TcpListener::accept: stale handle".to_string());
        };
        listener.accept().map_err(|e| e.to_string())
    });
    match res {
        Ok((stream, addr)) => {
            let sid = next_net_id();
            TCP_STREAM_REGISTRY.with(|r| {
                r.borrow_mut().insert(sid, stream);
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
    let res = TCP_LISTENER_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|l| l.local_addr().map_err(|e| e.to_string()))
    });
    match res {
        Some(Ok(addr)) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Some(Err(e)) => Ok(err_variant(e)),
        None => Ok(err_variant("TcpListener::local_addr: stale handle")),
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
                r.borrow_mut().insert(id, stream);
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
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::read: stale handle".to_string());
        };
        let mut buf = vec![0u8; max as usize];
        match stream.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            Err(e) => Err(e.to_string()),
        }
    });
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
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::read_to_string: stale handle".to_string());
        };
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    });
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
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::write: stale handle".to_string());
        };
        stream.write_all(&bytes).map_err(|e| e.to_string())
    });
    match res {
        Ok(()) => Ok(ok_variant(Value::Int(bytes.len() as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_tcp_stream_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        TCP_STREAM_REGISTRY.with(|r| {
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
                r.borrow_mut().insert(id, sock);
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
    let res = UDP_REGISTRY.with(|r| {
        let reg = r.borrow();
        match reg.get(&id) {
            Some(sock) => sock.send_to(&bytes, &addr).map_err(|e| e.to_string()),
            None => Err("UdpSocket::send_to: stale handle".to_string()),
        }
    });
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
    let res = UDP_REGISTRY.with(|r| {
        let reg = r.borrow();
        let Some(sock) = reg.get(&id) else {
            return Err("UdpSocket::recv_from: stale handle".to_string());
        };
        let mut buf = vec![0u8; max as usize];
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                buf.truncate(n);
                Ok((buf, addr))
            }
            Err(e) => Err(e.to_string()),
        }
    });
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
    let res = UDP_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|s| s.local_addr().map_err(|e| e.to_string()))
    });
    match res {
        Some(Ok(addr)) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Some(Err(e)) => Ok(err_variant(e)),
        None => Ok(err_variant("UdpSocket::local_addr: stale handle")),
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

// ----------------------------------------------------------------------
// HashSet (real set, distinct from HashMap)

thread_local! {
    pub(crate) static NEXT_SET_HANDLE: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    pub(crate) static SET_REGISTRY: RefCell<StdHashMap<i64, std::collections::HashSet<MapKey>>> =
        RefCell::new(StdHashMap::new());
}
