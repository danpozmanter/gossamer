//! C-ABI dispatch shims for `std::net::UdpSocket`. Mirrors the
//! bytecode-VM builtins in `gossamer-interp/src/stdlib_builtins/net.rs`
//! so the compiled (Cranelift / LLVM) tier resolves the same calls
//! natively instead of failing to link.
//!
//! Handle model identical to `c_abi/net_tcp.rs`: a process-global
//! `i64`-keyed registry of `Arc<UdpSocket>`; each op clones the `Arc`
//! under the lock and performs the (possibly blocking) `recv_from`
//! outside it. `UdpSocket::{send_to, recv_from, local_addr}` all take
//! `&self`, so no exclusive access is ever handed out. Cross-platform
//! via `std::net` (Linux / macOS / Windows); no libc / raw-fd surface.
//!
//! Result shapes (packed `i128` via `gos_rt_result_new`):
//! - `bind`        -> `Result<UdpSocket, Error>` (Ok payload = i64 handle)
//! - `send_to`     -> `Result<i64, Error>` (bytes sent)
//! - `recv_from`   -> `Result<([u8], String), Error>` (Ok payload = *Pair{bytes, addr})
//! - `local_addr`  -> `Result<String, Error>`
//! - `close`       -> () (Void)

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::wildcard_imports)]

use std::collections::HashMap;
use std::ffi::CStr;
use std::net::UdpSocket;
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use parking_lot::Mutex;

static UDP_SOCKETS: Mutex<Option<HashMap<i64, Arc<UdpSocket>>>> = Mutex::new(None);
static NEXT_UDP_HANDLE: AtomicI64 = AtomicI64::new(1);

fn next_handle() -> i64 {
    NEXT_UDP_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn cstr_to_str(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

fn udp_err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg)
        .unwrap_or_else(|_| std::ffi::CString::new("net::udp error").expect("static"));
    let err = unsafe { super::errors::gos_rt_error_new(cs.as_ptr()) };
    super::vec::gos_rt_result_new(1, err as i64)
}

fn socket_clone(h: i64) -> Option<Arc<UdpSocket>> {
    UDP_SOCKETS.lock().as_ref().and_then(|m| m.get(&h).cloned())
}

/// `net::UdpSocket::bind(addr) -> Result<UdpSocket, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_udp_bind(addr: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let a = cstr_to_str(addr);
        match UdpSocket::bind(&a) {
            Ok(s) => {
                let h = next_handle();
                UDP_SOCKETS
                    .lock()
                    .get_or_insert_with(HashMap::new)
                    .insert(h, Arc::new(s));
                super::vec::gos_rt_result_new(0, h)
            }
            Err(e) => udp_err(&format!("{e}")),
        }
    })
}

/// `net::UdpSocket::send_to(handle, data: [u8], addr) -> Result<i64, Error>`.
/// Returns the byte count sent. The MIR dispatch coerces a `String` /
/// byte-array-literal payload to the `Vec<u8>` ABI before the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_udp_send_to(
    h: i64,
    data: *const super::vec::GosVec,
    addr: *const c_char,
) -> i128 {
    ffi_entry!(0i128, {
        let Some(sock) = socket_clone(h) else {
            return udp_err("UdpSocket::send_to: stale handle");
        };
        let bytes = unsafe { super::encoding::gosvec_u8(data) };
        let target = cstr_to_str(addr);
        match sock.send_to(&bytes, &target) {
            Ok(n) => super::vec::gos_rt_result_new(0, n as i64),
            Err(e) => udp_err(&format!("{e}")),
        }
    })
}

/// `net::UdpSocket::recv_from(handle, max) -> Result<([u8], String), Error>`.
/// Ok payload is a heap `#[repr(C)] Pair { bytes: i64, addr: i64 }` —
/// the 2-slot tuple `([u8]-vec, sender-address-string)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_udp_recv_from(h: i64, max: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(sock) = socket_clone(h) else {
            return udp_err("UdpSocket::recv_from: stale handle");
        };
        let cap = max.clamp(1, 1 << 16) as usize;
        let mut buf = vec![0u8; cap];
        match sock.recv_from(&mut buf) {
            Ok((n, from)) => {
                buf.truncate(n);
                let bytes_vec = super::encoding::bytes_to_gosvec(&buf);
                let addr_cs = super::string::alloc_cstring(from.to_string().as_bytes());
                #[repr(C)]
                struct Pair {
                    bytes: i64,
                    addr: i64,
                }
                let pair = Box::into_raw(Box::new(Pair {
                    bytes: bytes_vec as i64,
                    addr: addr_cs as i64,
                }));
                super::vec::gos_rt_result_new(0, pair as i64)
            }
            Err(e) => udp_err(&format!("{e}")),
        }
    })
}

/// `net::UdpSocket::local_addr(handle) -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_udp_local_addr(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(sock) = socket_clone(h) else {
            return udp_err("UdpSocket::local_addr: stale handle");
        };
        match sock.local_addr() {
            Ok(a) => super::vec::gos_rt_result_new(
                0,
                super::string::alloc_cstring(a.to_string().as_bytes()) as i64,
            ),
            Err(e) => udp_err(&format!("{e}")),
        }
    })
}

/// `net::UdpSocket::close(handle)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_udp_close(h: i64) {
    ffi_entry!((), {
        if let Some(m) = UDP_SOCKETS.lock().as_mut() {
            m.remove(&h);
        }
    });
}
