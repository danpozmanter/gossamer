//! HTTP/2 server compiled into the runtime staticlib.
//!
//! Mirror of `gossamer_std::http_h2` that lives inside
//! `gossamer-runtime` so the compiled tier can call into it from
//! `gos_rt_http2_bind_and_run_h2c`. Implements:
//!
//! - A goroutine-driven future driver (`drive`).
//! - An `AsyncRead+AsyncWrite` bridge over a non-blocking
//!   `std::net::TcpStream` + mio mirror.
//! - An `h2::server`-backed serve loop that converts each
//!   inbound stream into a [`crate::c_abi::GosHttpRequest`] and
//!   dispatches through the user handler's function pointer.
//!
//! The goroutine scheduler is the only executor; no Tokio
//! runtime is involved. See
//! `crates/gossamer-std/HTTP_H2_ARCH.md` for the architectural
//! rationale (the std-side module documents the same model).

#![allow(
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::module_name_repetitions,
    clippy::similar_names,
    clippy::cast_possible_truncation,
    clippy::manual_let_else,
    clippy::let_unit_value,
    clippy::ignored_unit_patterns,
    clippy::explicit_iter_loop,
    clippy::map_unwrap_or,
    clippy::items_after_statements,
    clippy::doc_markdown
)]

use std::future::Future;
use std::io;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use bytes::Bytes;
use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use crate::c_abi::{
    GosHttpRequest, GosResult, drop_handler_result, extract_response_into, gos_rt_gc_reset,
};
use crate::sched::{Gid, Interest, ParkReason};
use crate::sched_global;

/// Boots the h2c server. Each accepted TCP connection is wrapped
/// in an [`AsyncTcpStream`] and driven by a goroutine that calls
/// `h2::server::handshake` + an accept loop. Per-stream handler
/// dispatch happens inside child goroutines so a slow handler
/// doesn't block other streams on the same connection.
pub fn serve_h2c_with_handler(addr: &str, env_addr: usize, fn_addr: usize) {
    let listener = match std::net::TcpListener::bind(addr) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gos_rt_http2_bind_and_run_h2c: bind {addr} failed: {e}");
            std::process::exit(1);
        }
    };
    let _ = listener.set_nonblocking(false);
    loop {
        let (sock, _peer) = match listener.accept() {
            Ok(p) => p,
            Err(_) => break,
        };
        let _ = sock.set_nodelay(true);
        sched_global::spawn(Box::new(move || {
            if sock.set_nonblocking(true).is_err() {
                return;
            }
            let mio_clone = match sock.try_clone() {
                Ok(c) => mio::net::TcpStream::from_std(c),
                Err(_) => return,
            };
            let async_io = AsyncTcpStream {
                inner: sock,
                mio: mio_clone,
                read_waker: Arc::new(Mutex::new(None)),
                write_waker: Arc::new(Mutex::new(None)),
            };
            let _ = drive(serve_one_connection(async_io, env_addr, fn_addr));
        }));
    }
}

async fn serve_one_connection(io: AsyncTcpStream, env_addr: usize, fn_addr: usize) {
    let mut conn = match h2::server::Builder::new().handshake::<_, Bytes>(io).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!("h2 handshake: {e}");
            return;
        }
    };
    let in_flight = Arc::new(AtomicUsize::new(0));
    loop {
        match conn.accept().await {
            Some(Ok((req, respond))) => {
                in_flight.fetch_add(1, Ordering::AcqRel);
                let in_flight_for_stream = Arc::clone(&in_flight);
                sched_global::spawn(Box::new(move || {
                    drive(serve_one_stream(req, respond, env_addr, fn_addr));
                    in_flight_for_stream.fetch_sub(1, Ordering::AcqRel);
                }));
            }
            Some(Err(e)) => {
                eprintln!("h2 accept: {e}");
                break;
            }
            None => break,
        }
    }
}

async fn serve_one_stream(
    h2_req: http::Request<h2::RecvStream>,
    mut respond: h2::server::SendResponse<Bytes>,
    env_addr: usize,
    fn_addr: usize,
) {
    let (parts, mut body_stream) = h2_req.into_parts();
    let method = parts.method.as_str().to_string();
    let path_and_query = parts
        .uri
        .path_and_query()
        .map_or_else(|| "/".to_string(), |pq| pq.as_str().to_string());
    let mut headers: Vec<(String, String)> = Vec::new();
    for (name, value) in parts.headers.iter() {
        if let Ok(v) = value.to_str() {
            headers.push((name.as_str().to_string(), v.to_string()));
        }
    }
    let mut body: Vec<u8> = Vec::new();
    while let Some(chunk) = body_stream.data().await {
        let Ok(chunk) = chunk else {
            break;
        };
        let _ = body_stream.flow_control().release_capacity(chunk.len());
        body.extend_from_slice(&chunk);
    }
    let mut gos_req = GosHttpRequest::for_h2(method, path_and_query, headers, body);

    let (status, response_body) = if fn_addr == 0 {
        (200u16, b"ok".to_vec())
    } else {
        // SAFETY: fn_addr / env_addr come from a `gos_fn_addr`
        // intrinsic at the user's call site. The handler ABI is
        // the same `(env, req) -> *mut GosResult` shape that
        // `gos_rt_http_serve` uses. Handlers must return
        // `Result<http::Response, http::Error>` — the runtime
        // reads disc==0 + payload as the GosHttpResponse.
        let handler: HandlerFn = unsafe { std::mem::transmute(fn_addr) };
        let env_ptr = env_addr as *mut u8;
        let req_ptr: *mut GosHttpRequest = &raw mut gos_req;
        let result_ptr = unsafe { handler(env_ptr, req_ptr) };
        let mut wire_buf: Vec<u8> = Vec::with_capacity(256);
        let ok = extract_response_into(result_ptr, &mut wire_buf);
        unsafe { drop_handler_result(result_ptr) };
        gos_rt_gc_reset();
        if ok {
            parse_http1_wire_buf(&wire_buf)
        } else {
            (500u16, b"handler error".to_vec())
        }
    };

    let status_code = http::StatusCode::from_u16(status).unwrap_or(http::StatusCode::OK);
    let head = match http::Response::builder().status(status_code).body(()) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("h2 response build: {e}");
            return;
        }
    };
    let body_empty = response_body.is_empty();
    let mut sender = match respond.send_response(head, body_empty) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("h2 send_response: {e}");
            return;
        }
    };
    if !body_empty {
        let _ = sender.send_data(Bytes::from(response_body), true);
    }
}

type HandlerFn = unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> *mut GosResult;

/// Parses the HTTP/1.1 wire buffer `extract_response_into`
/// emits — `HTTP/1.1 <status> <reason>\r\n[headers]\r\n\r\n<body>` —
/// into `(status, body)`.
fn parse_http1_wire_buf(buf: &[u8]) -> (u16, Vec<u8>) {
    let header_end = buf
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .map(|p| p + 4)
        .unwrap_or(buf.len());
    let body = if header_end < buf.len() {
        buf[header_end..].to_vec()
    } else {
        Vec::new()
    };
    let status = buf
        .split(|&b| b == b'\n')
        .next()
        .and_then(|line| {
            let s = std::str::from_utf8(line).ok()?;
            let mut parts = s.split_whitespace();
            let _version = parts.next()?;
            parts.next().and_then(|c| c.parse::<u16>().ok())
        })
        .unwrap_or(200);
    (status, body)
}

// ---------------------------------------------------------------------------
// Goroutine future-driver. Mirrors gossamer-std::runtime_future.
// ---------------------------------------------------------------------------

struct GoroutineWaker {
    gid: Gid,
    woke: AtomicBool,
}

impl Wake for GoroutineWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.woke.store(true, Ordering::Release);
        sched_global::scheduler().unpark(self.gid);
    }
}

/// Polls `fut` to completion on the calling goroutine; parks on
/// `Pending` and resumes when the registered waker fires.
fn drive<F>(fut: F) -> F::Output
where
    F: Future,
{
    let gid = sched_global::current_gid().expect("drive must run inside a goroutine");
    let waker_arc = Arc::new(GoroutineWaker {
        gid,
        woke: AtomicBool::new(false),
    });
    let waker: Waker = waker_arc.clone().into();
    let mut ctx = Context::from_waker(&waker);
    let mut pinned: Pin<Box<F>> = Box::pin(fut);
    loop {
        waker_arc.woke.store(false, Ordering::Release);
        match pinned.as_mut().poll(&mut ctx) {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                if waker_arc.woke.load(Ordering::Acquire) {
                    continue;
                }
                sched_global::park(ParkReason::Io, |_p| {});
            }
        }
    }
}

// ---------------------------------------------------------------------------
// AsyncRead+AsyncWrite over a mio-registered non-blocking TcpStream.
// Mirrors gossamer-std::async_tcp.
// ---------------------------------------------------------------------------

struct AsyncTcpStream {
    inner: std::net::TcpStream,
    mio: mio::net::TcpStream,
    read_waker: Arc<Mutex<Option<Waker>>>,
    write_waker: Arc<Mutex<Option<Waker>>>,
}

impl AsyncRead for AsyncTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let slice = buf.initialize_unfilled();
        use std::io::Read;
        match self.inner.read(slice) {
            Ok(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let slot = Arc::clone(&self.read_waker);
                arm_io_wake(&mut self.mio, Interest::Readable, cx, slot);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }
}

impl AsyncWrite for AsyncTcpStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        use std::io::Write;
        match self.inner.write(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let slot = Arc::clone(&self.write_waker);
                arm_io_wake(&mut self.mio, Interest::Writable, cx, slot);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

fn arm_io_wake(
    stream: &mut mio::net::TcpStream,
    interest: Interest,
    cx: &mut Context<'_>,
    waker_slot: Arc<Mutex<Option<Waker>>>,
) {
    let Some(gid) = sched_global::current_gid() else {
        return;
    };
    *waker_slot.lock() = Some(cx.waker().clone());
    let waker_slot_for_fire = Arc::clone(&waker_slot);
    sched_global::register_waker(
        gid,
        Box::new(move || {
            if let Some(w) = waker_slot_for_fire.lock().take() {
                w.wake();
            }
        }),
    );
    let _ = sched_global::with_poller(|p| p.register_io(stream, interest, gid));
    // Keep `as_raw_fd` import live on platforms that don't grow
    // the trait method (no-op call); silences an unused-import
    // warning when AsyncTcpStream is the only consumer.
    let _ = stream.as_raw_fd();
}
