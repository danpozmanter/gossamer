//! C-ABI dispatch shims for `std::net::TcpListener` / `TcpStream`.
//! Mirrors the bytecode-VM builtins in
//! `gossamer-interp/src/stdlib_builtins/net.rs` so the compiled
//! (Cranelift / LLVM) tier resolves the same calls natively instead of
//! failing to link.
//!
//! Handle model - process-global registries keyed by an `i64` handle,
//! the same shape the SQL handle registry uses (`c_abi/sql.rs`). A
//! handle is a plain integer at the Gossamer level, so it crosses
//! goroutine boundaries freely; the underlying `std::net` sockets are
//! `Send + Sync`. Sockets are stored behind `Arc` so a blocking call
//! never holds the registry lock: each op clones the `Arc` under the
//! lock, releases it, then performs the (possibly blocking) `accept` /
//! `read` / `write` on the shared handle outside the lock. This is
//! deadlock-free under the goroutine scheduler - a server parked in
//! `accept()` does not block a peer goroutine from registering its
//! `connect()`ed stream. `close` drops the registry's `Arc`; any
//! in-flight clone keeps the socket alive until it too drops (no
//! use-after-free on a concurrent close).
//!
//! All reads/writes go through `&TcpStream` (`std` implements
//! `Read`/`Write` for `&TcpStream`), and `TcpListener::accept` /
//! `local_addr` take `&self`, so no `&mut` ownership is needed past the
//! `Arc` - the registry never hands out exclusive access.
//!
//! Cross-platform: built entirely on `std::net` (Linux / macOS /
//! Windows). `std` performs `WSAStartup` lazily on Windows; there is no
//! libc / raw-fd / unix-only surface here.
//!
//! Result shapes (packed `i128` via `gos_rt_result_new`):
//! - `bind` / `connect`  -> `Result<TcpListener|TcpStream, Error>` (Ok payload = i64 handle)
//! - `accept`            -> `Result<(TcpStream, String), Error>` (Ok payload = *Pair{handle, addr})
//! - `local_addr`        -> `Result<String, Error>`
//! - `read`              -> `Result<[u8], Error>`
//! - `read_to_string`    -> `Result<String, Error>`
//! - `write`             -> `Result<i64, Error>` (bytes written)
//! - `close`             -> () (Void)

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;

// Process-global handle registries shared with every linked copy of the
// runtime. `Option` so the `Mutex::new(None)` initialiser is const.
static TCP_LISTENERS: Mutex<Option<HashMap<i64, Arc<TcpListener>>>> = Mutex::new(None);
static TCP_STREAMS: Mutex<Option<HashMap<i64, Arc<TcpStream>>>> = Mutex::new(None);
static NEXT_TCP_HANDLE: AtomicI64 = AtomicI64::new(1);

fn next_handle() -> i64 {
    NEXT_TCP_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn cstr_to_str(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

/// Packs `Err(errors::Error)` as the runtime's `i128` Result.
fn tcp_err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg)
        .unwrap_or_else(|_| std::ffi::CString::new("net::tcp error").expect("static"));
    let err = unsafe { super::errors::gos_rt_error_new(cs.as_ptr()) };
    super::vec::gos_rt_result_new(1, err as i64)
}

fn listener_clone(h: i64) -> Option<Arc<TcpListener>> {
    TCP_LISTENERS
        .lock()
        .as_ref()
        .and_then(|m| m.get(&h).cloned())
}

fn stream_clone(h: i64) -> Option<Arc<TcpStream>> {
    TCP_STREAMS.lock().as_ref().and_then(|m| m.get(&h).cloned())
}

fn insert_stream(s: TcpStream) -> i64 {
    let h = next_handle();
    TCP_STREAMS
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(h, Arc::new(s));
    h
}

/// `net::TcpListener::bind(addr) -> Result<TcpListener, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_listener_bind(addr: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let a = cstr_to_str(addr);
        match TcpListener::bind(&a) {
            Ok(l) => {
                let h = next_handle();
                TCP_LISTENERS
                    .lock()
                    .get_or_insert_with(HashMap::new)
                    .insert(h, Arc::new(l));
                super::vec::gos_rt_result_new(0, h)
            }
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpListener::accept(handle) -> Result<(TcpStream, String), Error>`.
/// The Ok payload is a heap `#[repr(C)] Pair { stream: i64, addr: i64 }` -
/// the 2-slot tuple `(TcpStream-handle, peer-address-string)` exactly
/// as `gos_rt_regex_find_opt` packs its triple.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_listener_accept(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(listener) = listener_clone(h) else {
            return tcp_err("TcpListener::accept: stale handle");
        };
        match listener.accept() {
            Ok((stream, peer)) => {
                let sh = insert_stream(stream);
                let addr_cs = super::string::alloc_cstring(peer.to_string().as_bytes());
                #[repr(C)]
                struct Pair {
                    stream: i64,
                    addr: i64,
                }
                let pair = Box::into_raw(Box::new(Pair {
                    stream: sh,
                    addr: addr_cs as i64,
                }));
                super::vec::gos_rt_result_new(0, pair as i64)
            }
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpListener::local_addr(handle) -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_listener_local_addr(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(listener) = listener_clone(h) else {
            return tcp_err("TcpListener::local_addr: stale handle");
        };
        match listener.local_addr() {
            Ok(a) => super::vec::gos_rt_result_new(
                0,
                super::string::alloc_cstring(a.to_string().as_bytes()) as i64,
            ),
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpListener::close(handle)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_listener_close(h: i64) {
    ffi_entry!((), {
        if let Some(m) = TCP_LISTENERS.lock().as_mut() {
            m.remove(&h);
        }
    });
}

/// `net::TcpStream::connect(addr) -> Result<TcpStream, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_stream_connect(addr: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let a = cstr_to_str(addr);
        match TcpStream::connect(&a) {
            Ok(s) => super::vec::gos_rt_result_new(0, insert_stream(s)),
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpStream::read(handle, max) -> Result<[u8], Error>`. One read,
/// up to `max` bytes (clamped to a 16 MiB ceiling, matching the VM).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_stream_read(h: i64, max: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(stream) = stream_clone(h) else {
            return tcp_err("TcpStream::read: stale handle");
        };
        let cap = max.clamp(1, 1 << 24) as usize;
        let mut buf = vec![0u8; cap];
        let mut reader: &TcpStream = &stream;
        match reader.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                super::vec::gos_rt_result_new(0, super::encoding::bytes_to_gosvec(&buf) as i64)
            }
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpStream::read_to_string(handle) -> Result<String, Error>`.
/// Reads until the peer closes (EOF); UTF-8-lossy, matching the VM.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_stream_read_to_string(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(stream) = stream_clone(h) else {
            return tcp_err("TcpStream::read_to_string: stale handle");
        };
        let mut reader: &TcpStream = &stream;
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match reader.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) => return tcp_err(&format!("{e}")),
            }
        }
        let s = String::from_utf8_lossy(&out);
        super::vec::gos_rt_result_new(0, super::string::alloc_cstring(s.as_bytes()) as i64)
    })
}

/// `net::TcpStream::write(handle, data: [u8]) -> Result<i64, Error>`.
/// `write_all`s the byte vector and returns the byte count. The MIR
/// dispatch coerces a `String` / byte-array-literal argument to the
/// `Vec<u8>` ABI before the call (see the delta report).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_stream_write(h: i64, data: *const super::vec::GosVec) -> i128 {
    ffi_entry!(0i128, {
        let Some(stream) = stream_clone(h) else {
            return tcp_err("TcpStream::write: stale handle");
        };
        let bytes = unsafe { super::encoding::gosvec_u8(data) };
        let mut writer: &TcpStream = &stream;
        match writer.write_all(&bytes) {
            Ok(()) => super::vec::gos_rt_result_new(0, bytes.len() as i64),
            Err(e) => tcp_err(&format!("{e}")),
        }
    })
}

/// `net::TcpStream::close(handle)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_tcp_stream_close(h: i64) {
    ffi_entry!((), {
        if let Some(m) = TCP_STREAMS.lock().as_mut() {
            m.remove(&h);
        }
    });
}
