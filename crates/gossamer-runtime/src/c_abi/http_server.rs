#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::ffi::CStr;
use std::net::{TcpListener, TcpStream};
use std::os::raw::c_char;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

// ---------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------
//
// Blocking TCP listener with one OS thread per accepted
// connection. Per connection we keep a `ConnScratch` reused
// across keep-alive requests so the steady state allocates
// nothing on the parse / response paths beyond what the user's
// handler does inside the gossamer arena (which is reset
// between requests). Phase 2 of the http_optimizations plan
// swaps `parse_request_into` for httparse and adds
// BufReader/BufWriter; today the parser is a naive CRLF split
// that's enough for HTTP/1.1 keep-alive bench traffic.

const STATIC_OK_RESPONSE: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
const RESPONSE_500_BYTES: &[u8] =
    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
const RESPONSE_400_BYTES: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESPONSE_413_BYTES: &[u8] =
    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
const RESPONSE_431_BYTES: &[u8] =
    b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

// Upper bound on the request head (request line plus header block)
// before its terminating CRLFCRLF. A client that streams header bytes
// without ever completing the head would otherwise grow the read
// accumulator without limit. Parity source of truth: the interp
// server's 8 KiB `Config::default().max_header_bytes`
// (gossamer-std/src/http.rs).
const MAX_REQUEST_HEAD_BYTES: usize = 8 * 1024;

// Upper bound on a request body, Content-Length-declared or
// de-chunked. Parity source of truth: the interp server's
// `Config::default().max_body_bytes` (gossamer-std/src/http.rs,
// 1 MiB) - interp `http::serve` builds that default config
// (gossamer-interp builtins.rs, `http_std::server::Config::default()`)
// with no Gossamer-level or env override, so the compiled tier
// enforces the same fixed cap. Also keeps `header_end +
// content_length` from overflowing on a hostile Content-Length.
const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

// Longest accepted chunk-size line (hex size + optional chunk
// extension). The hex part itself is capped at 16 digits to mirror
// the interp decoder's `max_size_digits`; this bounds the
// extension tail so a hostile peer cannot stream an endless line.
const MAX_CHUNK_SIZE_LINE_BYTES: usize = 1024;

// Cap on the trailer block after the terminal 0-chunk. Mirrors the
// interp server's 8 KiB `max_header_bytes` header-block cap -
// trailers are headers.
const MAX_TRAILER_BYTES: usize = 8 * 1024;

// Cap on the raw (still-encoded) bytes of one chunked frame. The
// decoded cap above bounds payload; this bounds framing overhead,
// so a peer drip-feeding pathological 1-byte chunks with maximal
// extensions cannot grow the accumulator without limit. Any
// well-formed stream whose framing overhead is at most its payload
// (plus the trailer/slack allowance) fits; real encoders emit
// multi-KiB chunks with <1% overhead.
const MAX_CHUNKED_RAW_BYTES: usize = 2 * MAX_REQUEST_BODY_BYTES + 16 * 1024;

/// Per-connection mutable scratch. Reused across keep-alive
/// requests so steady state allocates only inside the gossamer
/// arena, which is reset between requests.
struct ConnScratch {
    /// Filled in place by `parse_request_into` and handed to
    /// the user handler as `*mut GosHttpRequest`. Lives for
    /// the entire connection.
    request: GosHttpRequest,
    /// Bytes written to the wire. Truncated, never freed,
    /// across requests.
    response_buf: Vec<u8>,
}

impl ConnScratch {
    fn new() -> Self {
        Self {
            request: GosHttpRequest {
                method: String::with_capacity(8),
                url: String::with_capacity(64),
                headers: Vec::with_capacity(16),
                body: Vec::with_capacity(0),
                body_offset: 0,
                params: Vec::new(),
                values: Vec::new(),
                agent: None,
            },
            response_buf: Vec::with_capacity(512),
        }
    }
}

/// Live count of per-connection HTTP server threads. Each accepted
/// connection bumps this on spawn and decrements on the thread's
/// final body line; the cap from `GOSSAMER_HTTP_MAX_CONN` rejects
/// further connections with a 503 once the count reaches its
/// ceiling. Process-global so multiple `http::serve` calls inside
/// the same program share back-pressure.
static HTTP_ACTIVE_CONNS: AtomicUsize = AtomicUsize::new(0);

/// RAII guard that decrements [`HTTP_ACTIVE_CONNS`] when the
/// per-connection thread's body unwinds or returns. Created
/// inside the spawn closure so the decrement runs even if
/// `handle_http_conn` panics.
struct HttpConnGuard;

impl Drop for HttpConnGuard {
    fn drop(&mut self) {
        HTTP_ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Default per-process cap on concurrent HTTP server connections,
/// overridable via the `GOSSAMER_HTTP_MAX_CONN` env var. 4096 is
/// well below the typical 65 535 fd ceiling and leaves headroom
/// for the listener, the netpoller, log files, and the rest of
/// the runtime's open files.
const DEFAULT_HTTP_MAX_CONN: usize = 4096;

fn http_max_conn() -> usize {
    static CACHE: AtomicUsize = AtomicUsize::new(0);
    // Sentinel 0 means "not yet read" - the cap can never legally
    // be zero (that would refuse every connection). Resolve once
    // per process and cache.
    let cached = CACHE.load(Ordering::Relaxed);
    if cached != 0 {
        return cached;
    }
    let cap = std::env::var("GOSSAMER_HTTP_MAX_CONN")
        .ok()
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|&n| n > 0)
        .unwrap_or(DEFAULT_HTTP_MAX_CONN);
    CACHE.store(cap, Ordering::Relaxed);
    cap
}

/// Per-connection idle / slow-read timeout in milliseconds. Mirrors the
/// interp tier's 30s default; `GOSSAMER_HTTP_READ_TIMEOUT_MS=0` disables
/// it.
const DEFAULT_HTTP_READ_TIMEOUT_MS: u64 = 30_000;

/// Sets `SO_RCVTIMEO` on a connection socket from
/// `GOSSAMER_HTTP_READ_TIMEOUT_MS`. Returns true when a timeout is in
/// force, so the read loop can treat a lapse as a dead connection.
fn apply_read_timeout(stream: &TcpStream) -> bool {
    let ms = std::env::var("GOSSAMER_HTTP_READ_TIMEOUT_MS")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .unwrap_or(DEFAULT_HTTP_READ_TIMEOUT_MS);
    if ms == 0 {
        return false;
    }
    stream
        .set_read_timeout(Some(std::time::Duration::from_millis(ms)))
        .is_ok()
}

const RESPONSE_503_BYTES: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Packs an `Err(errors::Error)` runtime `Result` carrying `msg` -
/// the bind-failure value `gos_rt_http_serve` and
/// `gos_rt_http2_bind_and_run_h2c` hand back to the caller's
/// `Result<(), http::Error>` match.
fn http_serve_err_result(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let err = unsafe { super::errors::gos_rt_error_new(cs.as_ptr()) };
    super::vec::pack_result(1, err as i64)
}

/// Starts an HTTP listener and dispatches each request to
/// `handler_fn(handler_env, request)`. Returns 200/payload from
/// the handler's `Ok(Response)`, 500 from `Err`, and a static
/// `200 OK\r\n\r\nok` when `handler_fn` is null (legacy stub).
///
/// Returns the Gossamer-visible `Result<(), http::Error>`: a
/// packed `Err` when the address cannot be bound (interp parity -
/// the VM hands the same `Err` to the caller's match), or a packed
/// `Ok(())` if the accept loop ever exits (graceful shutdown).
///
/// Concurrent connections are capped at `GOSSAMER_HTTP_MAX_CONN`
/// (default 4096). When the cap is hit the listener accepts the
/// connection, writes a 503 Service Unavailable response, closes
/// the socket without spawning a thread, and continues. This
/// turns the previous unbounded `thread::Builder::spawn` into
/// bounded back-pressure so a flood of clients cannot exhaust
/// the OS thread or file-descriptor budget.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn gos_rt_http_serve(
    addr: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> i128 {
    {
        let addr_s = if addr.is_null() {
            "0.0.0.0:8080".to_string()
        } else {
            unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
        };
        let listener = match TcpListener::bind(&addr_s) {
            Ok(l) => l,
            Err(e) => {
                // Startup-time failure: hand back `Err` with the
                // interp's message shape so all tiers print the same
                // text from `match http::serve(..) { Err(e) => .. }`.
                return http_serve_err_result(&format!("http::serve: {e}"));
            }
        };
        let env_addr = handler_env as usize;
        let fn_addr = handler_fn as usize;
        // Per-connection goroutine on the M:N work-stealing pool.
        // Each accepted socket is dispatched via
        // `crate::sched_global::try_spawn`, so the connection lifetime
        // is owned by a scheduler-managed worker rather than a fresh
        // OS thread (the previous design did the latter and silently
        // dropped connections whenever `std::thread::Builder::spawn`
        // returned `EAGAIN` under load).
        //
        // The [`HttpConn`] wrapper drives non-blocking I/O against the
        // global netpoller: when the kernel send/receive buffer is
        // empty or full, the goroutine parks via
        // [`crate::sched_global::wait_io`] and the worker thread is
        // freed to run other goroutines. The netpoller wakes the
        // waker when the kernel reports readiness - the same shape as
        // Go's `netpoll`.
        //
        // When [`crate::sched_global::try_spawn`] refuses (live-
        // goroutine cap reached - default 1M, set by
        // `GOSSAMER_MAX_GOROUTINES`), the connection is dropped and
        // the refusal is logged to stderr. Hitting that cap means
        // something pathological is happening upstream, so refusing
        // is the right back-pressure.
        //
        // Accept-loop errors retry on `EINTR` and break on anything
        // else; the listener's filesystem socket is then closed by
        // the OS at process exit.
        //
        // Handler safety: per-worker thread-local state survives only
        // across synchronous sequences. The handler-returns-pointer →
        // `extract_response_into` copy → `drop_handler_result` →
        // `gos_rt_gc_reset` sequence runs without yielding, so the
        // arena reset never wipes a pointer the goroutine still holds.
        // Handlers that yield *mid-execution* (e.g. user code that
        // performs blocking I/O inside the handler) would observe an
        // arena reset triggered by another goroutine on the same
        // worker and are not supported under this server. Keep
        // handlers CPU-bound; offload blocking work to a separate
        // goroutine and pass results back via a channel.
        // Thread-per-connection - matches Go's `net/http` shape.
        // Each accepted socket gets a dedicated OS thread that runs
        // `handle_http_conn` to completion (blocking reads/writes
        // are safe because they only stall their own thread, not a
        // shared worker pool). `HTTP_ACTIVE_CONNS` caps the live
        // thread count at `GOSSAMER_HTTP_MAX_CONN` (default 4096)
        // and responds 503 past the cap, so a runaway client cannot
        // exhaust the fd / thread budget.
        //
        // The previous (0.6.0 stability) shape was a fixed worker
        // pool + bounded sync_channel. That cap on in-flight workers
        // (`available_parallelism() * 2` ≈ 48 on a 12-core box)
        // throttled throughput at >48 concurrent clients and the
        // queue-full path silently `try_send`-dropped sockets, which
        // the bench saw as connection errors. The per-connection
        // thread design is what the 2026-05-12 web benchmark
        // (272 k RPS, 0 fails) measured.
        //
        // `GOSSAMER_HTTP_WORKERS` is retained as an env var for
        // backwards compatibility but is no longer consulted by
        // this path.
        //
        // Graceful shutdown: when `sched_global::request_shutdown`
        // is called (from `gos_rt_exit`), the accept loop exits its
        // next iteration. In-flight per-connection threads run to
        // completion; the listener fd is closed by the OS at
        // process exit.
        accept_serve(listener, move |stream| {
            let Some(mut conn) = HttpConn::wrap(stream) else {
                return;
            };
            handle_http_conn(&mut conn, env_addr, fn_addr);
        });
    }
    // The accept loop exited: graceful shutdown request or a fatal
    // listener error. Either way the server ran - report `Ok(())`,
    // matching the interp's `bind_and_run` return shape.
    super::vec::pack_result(0, 0)
}

/// Accept loop shared by the plaintext and TLS servers: each accepted
/// socket runs `serve_conn` on a dedicated OS thread (blocking I/O only
/// stalls its own thread). `HTTP_ACTIVE_CONNS` caps the live thread
/// count at `GOSSAMER_HTTP_MAX_CONN` (default 4096), replying 503 past
/// the cap so a runaway client cannot exhaust the fd / thread budget.
/// A graceful shutdown request (from `gos_rt_exit`) breaks the loop on
/// its next iteration; in-flight connection threads run to completion.
pub(crate) fn accept_serve<F>(listener: TcpListener, serve_conn: F)
where
    F: Fn(TcpStream) + Send + Sync + Clone + 'static,
{
    loop {
        if crate::sched_global::is_shutdown_requested() {
            break;
        }
        let stream = match listener.accept() {
            Ok((s, _)) => s,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(_) => break,
        };
        let _ = stream.set_nodelay(true);
        let cap = http_max_conn();
        let current = HTTP_ACTIVE_CONNS.fetch_add(1, Ordering::AcqRel);
        if current >= cap {
            HTTP_ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
            // Best-effort 503 + close; ignore write errors -
            // the client might already be gone.
            let mut stream = stream;
            use std::io::Write;
            let _ = stream.write_all(RESPONSE_503_BYTES);
            let _ = stream.flush();
            continue;
        }
        // Spawn a dedicated OS thread for this connection. On
        // EAGAIN (extremely rare; would mean the system is out
        // of thread quota), roll back the cap counter and drop
        // the socket - the kernel will RST it.
        let serve = serve_conn.clone();
        let spawn_result = std::thread::Builder::new()
            .name("gos-http-conn".to_string())
            .spawn(move || {
                let _guard = HttpConnGuard;
                serve(stream);
            });
        if spawn_result.is_err() {
            HTTP_ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
        }
    }
}

/// TLS-terminating HTTP/1.1 server:
/// `serve_tls(addr, cert_pem, key_pem, handler)`. Mirror of
/// [`gos_rt_http_serve`] that builds a rustls server config from the
/// PEM-encoded certificate chain and private key, then drives each
/// accepted connection through the same request/response core after TLS
/// termination - so HTTPS handlers behave identically to plaintext ones.
/// Returns the Gossamer `Result<(), http::Error>`: `Err` on bind or
/// TLS-config failure, `Ok(())` on graceful shutdown.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn gos_rt_http_serve_tls(
    addr: *const c_char,
    cert_pem: *const c_char,
    key_pem: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> i128 {
    {
        let addr_s = if addr.is_null() {
            "0.0.0.0:8443".to_string()
        } else {
            unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
        };
        let cert = if cert_pem.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(cert_pem).to_string_lossy().into_owned() }
        };
        let key = if key_pem.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(key_pem).to_string_lossy().into_owned() }
        };
        let server_config = match build_server_config_from_pem(cert.as_bytes(), key.as_bytes()) {
            Ok(c) => c,
            Err(e) => return http_serve_err_result(&format!("http::serve_tls: {e}")),
        };
        let listener = match TcpListener::bind(&addr_s) {
            Ok(l) => l,
            Err(e) => return http_serve_err_result(&format!("http::serve_tls: {e}")),
        };
        let env_addr = handler_env as usize;
        let fn_addr = handler_fn as usize;
        accept_serve(listener, move |stream| {
            serve_tls_conn(stream, env_addr, fn_addr, server_config.clone());
        });
    }
    super::vec::pack_result(0, 0)
}

/// rustls-terminated server connection. Blocking I/O on a dedicated
/// per-connection thread; rustls drives the record layer synchronously,
/// the server-side mirror of the client `TlsStream` in gossamer-std.
struct TlsServerConn {
    inner: rustls::StreamOwned<rustls::ServerConnection, TcpStream>,
}

impl HttpIo for TlsServerConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        std::io::Read::read(&mut self.inner, buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        std::io::Write::write_all(&mut self.inner, buf)?;
        std::io::Write::flush(&mut self.inner)
    }
}

/// Wraps an accepted socket in a rustls server session and serves it
/// through the shared request/response core. A handshake that never
/// completes is dropped when the connection thread's read loop returns.
fn serve_tls_conn(
    stream: TcpStream,
    env_addr: usize,
    fn_addr: usize,
    server_config: std::sync::Arc<rustls::ServerConfig>,
) {
    let Ok(conn) = rustls::ServerConnection::new(server_config) else {
        return;
    };
    // Idle / slow-read timeout on the underlying socket: a stalled
    // handshake or request surfaces as a read error that ends the
    // connection thread (slowloris defense), parity with the plaintext
    // path.
    let _ = apply_read_timeout(&stream);
    let mut tls = TlsServerConn {
        inner: rustls::StreamOwned::new(conn, stream),
    };
    handle_http_conn(&mut tls, env_addr, fn_addr);
}

/// Builds a rustls `ServerConfig` from a PEM certificate chain and
/// private key. No client-certificate verification.
fn build_server_config_from_pem(
    cert_pem: &[u8],
    key_pem: &[u8],
) -> Result<std::sync::Arc<rustls::ServerConfig>, String> {
    use rustls::pki_types::pem::PemObject;
    use rustls::pki_types::{CertificateDer, PrivateKeyDer};
    let _ = rustls::crypto::ring::default_provider().install_default();
    let certs = CertificateDer::pem_slice_iter(cert_pem)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("parse certificate PEM: {e}"))?;
    if certs.is_empty() {
        return Err("no certificates in PEM".to_string());
    }
    let key = PrivateKeyDer::pem_slice_iter(key_pem)
        .next()
        .ok_or_else(|| "no private key in PEM".to_string())?
        .map_err(|e| format!("parse key PEM: {e}"))?;
    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("build server config: {e}"))?;
    Ok(std::sync::Arc::new(config))
}

type HandlerFn = unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> i128;

/// HTTP/2 cleartext server. Mirror of [`gos_rt_http_serve`] for
/// HTTP/2 - the MIR lowerer emits this call when the compiled
/// program invokes `http2::bind_and_run_h2c(addr, app, config)`.
/// The h2 server implementation lives in
/// [`crate::http2_server`]; this thunk just adapts the C-ABI
/// signature into the Rust API.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http2_bind_and_run_h2c(
    addr: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> i128 {
    let addr_s = if addr.is_null() {
        "0.0.0.0:8080".to_string()
    } else {
        unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
    };
    let env_addr = handler_env as usize;
    let fn_addr = handler_fn as usize;
    match crate::http2_server::serve_h2c_with_handler(&addr_s, env_addr, fn_addr) {
        Ok(()) => super::vec::pack_result(0, 0),
        Err(e) => http_serve_err_result(&format!("http::serve_h2c: {e}")),
    }
}

/// Byte transport for one HTTP connection. Implemented by [`HttpConn`]
/// (plaintext, goroutine-aware netpoller I/O) and [`TlsServerConn`]
/// (rustls-terminated, blocking I/O), so the request/response core
/// drives a cleartext or TLS socket through a single code path.
trait HttpIo {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()>;
}

impl HttpIo for HttpConn {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        HttpConn::read(self, buf)
    }
    fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
        HttpConn::write_all(self, buf)
    }
}

/// Pulls one read's worth of bytes into `accum`. Returns `false`
/// on clean EOF or a socket error - the caller closes the
/// connection.
fn read_more<C: HttpIo>(conn: &mut C, accum: &mut Vec<u8>, buf: &mut [u8]) -> bool {
    match conn.read(buf) {
        Ok(0) | Err(_) => false,
        Ok(n) => {
            accum.extend_from_slice(&buf[..n]);
            true
        }
    }
}

fn handle_http_conn<C: HttpIo>(conn: &mut C, env_addr: usize, fn_addr: usize) {
    let mut scratch = ConnScratch::new();
    let mut accum: Vec<u8> = Vec::with_capacity(8192);
    let mut buf: Vec<u8> = vec![0u8; 8192];
    // Tracks whether a `100 Continue` interim response has already been
    // sent for the request currently being assembled, so the body-wait
    // re-entry below does not emit it more than once. Reset when a
    // request completes (a pipelined successor may also expect it).
    let mut continue_sent = false;
    loop {
        let Some(header_end) = find_header_end(&accum) else {
            // Bound the request head: a slow client that never sends the
            // terminating CRLFCRLF cannot grow `accum` without limit.
            // Mirrors the interp server's 8 KiB header cap.
            if accum.len() > MAX_REQUEST_HEAD_BYTES {
                let _ = conn.write_all(RESPONSE_431_BYTES);
                return;
            }
            if read_more(conn, &mut accum, &mut buf) {
                continue;
            }
            return;
        };
        // RFC 9110 §10.1.1: grant a client waiting on
        // `Expect: 100-continue` before its body is read, matching the
        // interp tier.
        if !continue_sent && request_expects_continue(&accum[..header_end]) {
            if conn.write_all(b"HTTP/1.1 100 Continue\r\n\r\n").is_err() {
                return;
            }
            continue_sent = true;
        }
        // Consume the body too: `req_end` covers the header section
        // plus the body bytes (Content-Length-declared, or the full
        // chunked frame). Anything past it is the next pipelined
        // request - keep it in `accum` for the next iteration.
        let mut chunked_req: Option<ChunkedRequest> = None;
        let req_end;
        if transfer_encoding_is_chunked(&accum[..header_end]) {
            // RFC 9112 §6.3.3 + interp-server parity (gossamer-std
            // http.rs `read_request_body`): a request carrying both
            // `Transfer-Encoding: chunked` and `Content-Length` is
            // request-smuggling-shaped - reject it.
            if header_value(&accum[..header_end], b"content-length").is_some() {
                let _ = conn.write_all(RESPONSE_400_BYTES);
                return;
            }
            match assemble_chunked_request(&accum, header_end, MAX_REQUEST_BODY_BYTES) {
                ChunkedAssembly::Incomplete => {
                    if read_more(conn, &mut accum, &mut buf) {
                        continue;
                    }
                    return;
                }
                ChunkedAssembly::Reject(response) => {
                    let _ = conn.write_all(response);
                    return;
                }
                ChunkedAssembly::Ready(decoded) => {
                    req_end = decoded.raw_end;
                    chunked_req = Some(decoded);
                }
            }
        } else {
            let body_len = content_length(&accum[..header_end]);
            if body_len > MAX_REQUEST_BODY_BYTES {
                let _ = conn.write_all(RESPONSE_413_BYTES);
                return;
            }
            req_end = header_end + body_len;
            if accum.len() < req_end {
                if read_more(conn, &mut accum, &mut buf) {
                    continue;
                }
                return;
            }
        }

        scratch.response_buf.clear();

        if fn_addr == 0 {
            // Legacy stub path: ignore the request, send static
            // 200/ok. No arena allocation happens here.
            scratch.response_buf.extend_from_slice(STATIC_OK_RESPONSE);
        } else {
            // Reset the request scratch in place. Field
            // capacities persist; we only push back into them.
            scratch.request.method.clear();
            scratch.request.url.clear();
            scratch.request.headers.clear();
            scratch.request.body.clear();
            scratch.request.body_offset = 0;

            // Chunked requests parse from the canonical de-chunked
            // rewrite; everything else parses straight from the
            // accumulator.
            let parsed = match &chunked_req {
                Some(c) => parse_request_into(&c.canonical, c.header_end, &mut scratch.request),
                None => parse_request_into(&accum[..req_end], header_end, &mut scratch.request),
            };
            if !parsed {
                // Malformed request: send 400 and close. Keeping
                // the connection open after an unparseable request
                // is unsafe - we don't know how many bytes the
                // bogus request claimed, so the next request would
                // be misaligned. The connection will be reopened
                // by the client.
                let _ = conn.write_all(RESPONSE_400_BYTES);
                return;
            }

            // Chunked trailer headers (RFC 7230 §4.1.2) need no
            // extra promotion here: `splice_canonical` splices them
            // into the canonical header section, so the eager parse
            // above already merged them into `request.headers` with
            // the interp's lowercase/dedupe semantics.

            // SAFETY: `fn_addr` came from `gos_fn_addr("T::serve")`
            // at the user's `http::serve(addr, app)` call site;
            // env_addr is the `&app` pointer passed alongside.
            let handler: HandlerFn = unsafe { std::mem::transmute(fn_addr) };
            let env_ptr = env_addr as *mut u8;
            let req_ptr: *mut GosHttpRequest = &raw mut scratch.request;
            let result_ptr = unsafe { handler(env_ptr, req_ptr) };
            if let Some(handle) = streamed_ok_handle(result_ptr) {
                // Streamed response (`Response::stream`): write the
                // head, then drain the upstream reader straight to
                // the connection in chunked frames - no buffering.
                extract_stream_head_into(result_ptr, &mut scratch.response_buf);
                unsafe { drop_handler_result(result_ptr) };
                unsafe { gos_rt_gc_reset() };
                if conn.write_all(&scratch.response_buf).is_err() {
                    return;
                }
                if !drain_stream_chunked(conn, handle) {
                    // Mid-stream failure: close WITHOUT the terminal
                    // frame so the client detects truncation
                    // (RFC 7230 §4.1 - an unterminated chunked body
                    // is incomplete). Mirrors the std server.
                    return;
                }
                accum.drain(..req_end);
                continue_sent = false;
                continue;
            }
            if !extract_response_into(result_ptr, &mut scratch.response_buf) {
                scratch.response_buf.extend_from_slice(RESPONSE_500_BYTES);
            }
            unsafe { drop_handler_result(result_ptr) };

            // Reset the per-worker gossamer arena. The handler
            // may have allocated strings/vecs into it (e.g.
            // `format!` output backing the response body, json
            // encoding output); without this the arena grows
            // unboundedly across requests on a long-lived
            // connection. Runs synchronously after the
            // `extract_response_into` copy, so it cannot wipe a
            // pointer the goroutine still holds.
            unsafe { gos_rt_gc_reset() };
        }

        if conn.write_all(&scratch.response_buf).is_err() {
            return;
        }
        // Drop the consumed request from the accumulator while
        // preserving any pipelined remainder. `drain` shifts the
        // tail into place; capacity is retained.
        accum.drain(..req_end);
        continue_sent = false;
    }
}

/// True when the request signals `Expect: 100-continue` (RFC 9110
/// §10.1.1), so the server should grant the interim response before the
/// body is read.
fn request_expects_continue(header_section: &[u8]) -> bool {
    header_value(header_section, b"expect").is_some_and(|v| v.eq_ignore_ascii_case(b"100-continue"))
}

/// Connection wrapper that bridges a non-blocking [`TcpStream`] to
/// the global netpoller. Reads and writes that would block register
/// interest with [`crate::sched_global`] and park the calling
/// goroutine on a Condvar; the netpoller wakes the waker when the
/// kernel reports readiness.
struct HttpConn {
    stream: TcpStream,
    mio_stream: mio::net::TcpStream,
    last_source: Option<crate::sched::PollSource>,
    /// When true, a read that blocks past `SO_RCVTIMEO` is treated as a
    /// timed-out connection rather than a netpoller-park retry.
    read_timeout: bool,
}

impl HttpConn {
    fn wrap(stream: TcpStream) -> Option<Self> {
        // Blocking I/O on the std fd. Compiled-mode HTTP runs each
        // connection on a dedicated OS thread (see `gos_rt_http_serve`),
        // so blocking reads are fine - they only stall the per-
        // connection thread, not a shared goroutine pool. The mio
        // clone is retained so any other path that needs non-blocking
        // semantics can still register it with the netpoller.
        let cloned = stream.try_clone().ok()?;
        // Idle / slow-read timeout (slowloris defense), parity with the
        // interp tier's `read_timeout`. `GOSSAMER_HTTP_READ_TIMEOUT_MS=0`
        // disables it.
        let read_timeout = apply_read_timeout(&stream);
        Some(Self {
            mio_stream: mio::net::TcpStream::from_std(cloned),
            stream,
            last_source: None,
            read_timeout,
        })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match std::io::Read::read(&mut self.stream, buf) {
                Ok(n) => return Ok(n),
                // With `SO_RCVTIMEO` set the blocking socket surfaces a
                // lapsed read as WouldBlock / TimedOut; treat it as a
                // dead connection so a stalled peer cannot hold a thread.
                Err(e)
                    if self.read_timeout
                        && matches!(
                            e.kind(),
                            std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                        ) =>
                {
                    return Err(e);
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(crate::sched::Interest::Readable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn write_all(&mut self, mut buf: &[u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match std::io::Write::write(&mut self.stream, buf) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "wrote zero bytes",
                    ));
                }
                Ok(n) => buf = &buf[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(crate::sched::Interest::Writable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn wait(&mut self, interest: crate::sched::Interest) -> std::io::Result<()> {
        // Goroutine-aware wait: park the calling coroutine on the
        // netpoller's readiness signal. The worker thread is freed
        // to run other goroutines while we wait. When called from
        // a non-goroutine OS thread (e.g. tooling code), the helper
        // falls back to a brief OS-thread sleep.
        crate::sched_global::wait_io(&mut self.mio_stream, interest)
    }
}

impl Drop for HttpConn {
    fn drop(&mut self) {
        if let Some(source) = self.last_source.take() {
            // Best-effort deregistration; the netpoller's `by_source`
            // map will leak the slot otherwise.
            let _ = crate::sched_global::with_poller(|p| {
                p.deregister_io(
                    &mut self.mio_stream,
                    source,
                    crate::sched::Interest::Readable,
                )
            });
        }
    }
}

/// Returns the index *one past* the trailing `\r\n\r\n` of the
/// first complete header section in `buf`, or `None` when the
/// buffer doesn't yet contain a full request header.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    let needle = b"\r\n\r\n";
    buf.windows(4).position(|w| w == needle).map(|p| p + 4)
}

/// Drops the `GosHttpResponse` referenced by the handler's
/// `Result` so each request doesn't leak. Every response reaching
/// here was Box-allocated (`gos_rt_http_response_text_new` /
/// `_json_new` use `Box::into_raw`); this is the unique reclaim
/// site. The gos-allocated body c-string is freed explicitly via
/// `gos_rt_str_free`, then `Box::from_raw` drops the struct,
/// structurally freeing `headers`, `body_bytes`, and
/// `content_type`. A null pointer or an `Err` result is a no-op.
pub(crate) unsafe fn drop_handler_result(result: i128) {
    if super::vec::gos_rt_result_disc(result) == 0 {
        let response_ptr = super::vec::gos_rt_result_payload(result) as *mut GosHttpResponse;
        if !response_ptr.is_null() {
            unsafe { crate::c_abi::string::gos_rt_str_free((*response_ptr).body.as_ptr()) };
            drop(unsafe { Box::from_raw(response_ptr) });
        }
    }
    // Result is now a 2-word by-value `i128` (no heap box), so there is
    // nothing to free here - this is exactly the per-request box leak that
    // the by-value representation eliminated everywhere.
}

/// Returns the trimmed value of the first `name` header in a raw
/// header section (request line + headers, up to the trailing
/// `\r\n\r\n`), or `None` when absent. Header-name match is
/// case-insensitive per RFC 7230 §3.2.
pub(crate) fn header_value<'a>(header_section: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for line in header_section.split(|&b| b == b'\n') {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let Some(colon) = line.iter().position(|&b| b == b':') else {
            continue;
        };
        if line[..colon].eq_ignore_ascii_case(name) {
            return Some(line[colon + 1..].trim_ascii());
        }
    }
    None
}

/// Parses the `Content-Length` value out of a raw header section.
/// Returns 0 when the header is absent or unparseable - the
/// no-body fast path.
fn content_length(header_section: &[u8]) -> usize {
    header_value(header_section, b"content-length")
        .and_then(|v| std::str::from_utf8(v).ok())
        .and_then(|s| s.parse().ok())
        .unwrap_or(0)
}

/// True when the request declares `Transfer-Encoding: chunked`.
/// Mirrors the interp server's detection (gossamer-std http.rs
/// `read_request_body`): the whole value must equal `chunked`
/// case-insensitively, so multi-coding values like `gzip, chunked`
/// are not treated as chunked on either tier.
fn transfer_encoding_is_chunked(header_section: &[u8]) -> bool {
    header_value(header_section, b"transfer-encoding")
        .is_some_and(|v| v.eq_ignore_ascii_case(b"chunked"))
}

/// A fully decoded inbound chunked request, ready for dispatch.
struct ChunkedRequest {
    /// One past the final CRLF of the chunked frame in the
    /// accumulator - everything beyond it is the next pipelined
    /// request and must stay in the accumulator.
    raw_end: usize,
    /// Canonical rewrite of the request: the original header
    /// section with trailer headers spliced in before the blank
    /// line, followed by the de-chunked body. This is exactly the
    /// buffer shape `parse_request_into` stores, so the body
    /// accessors (`gos_rt_http_request_body_str`,
    /// `gos_rt_http_request_raw_body`) see clean payload bytes.
    canonical: Vec<u8>,
    /// Offset of the body within `canonical` (one past `\r\n\r\n`).
    header_end: usize,
}

/// Outcome of scanning the accumulator for a complete chunked frame.
enum ChunkedAssembly {
    /// The frame has not terminated yet - read more bytes.
    Incomplete,
    /// Protocol violation or cap breach: write these response
    /// bytes and close the connection.
    Reject(&'static [u8]),
    /// Frame complete and decoded.
    Ready(ChunkedRequest),
}

/// Finds the `\r\n` terminating a line that starts at `from`,
/// scanning at most `cap` bytes of line content. Returns the
/// absolute index of the `\r`, or `None` when no CRLF lies within
/// the window (more data needed, or the line is over `cap`).
fn find_crlf(buf: &[u8], from: usize, cap: usize) -> Option<usize> {
    let end = buf.len().min(from.saturating_add(cap).saturating_add(2));
    buf.get(from..end)?
        .windows(2)
        .position(|w| w == b"\r\n")
        .map(|p| from + p)
}

/// Decodes the chunked frame that starts at `header_end` in
/// `accum` (RFC 7230 §4.1): hex size lines with optional ignored
/// chunk extensions, CRLF-framed data, terminal 0-chunk, then a
/// trailer block up to a blank line. The decoded body is capped at
/// `max_body` - declared chunk sizes count against the cap the
/// moment the size line is parsed, so a hostile declaration is
/// rejected before its data arrives. The still-encoded frame is
/// capped at [`MAX_CHUNKED_RAW_BYTES`] while incomplete.
///
/// The scan restarts from `header_end` on every call; cost is
/// bounded by the caps, and chunked uploads are not the keep-alive
/// fast path.
fn assemble_chunked_request(accum: &[u8], header_end: usize, max_body: usize) -> ChunkedAssembly {
    let incomplete = || {
        if accum.len() - header_end > MAX_CHUNKED_RAW_BYTES {
            ChunkedAssembly::Reject(RESPONSE_413_BYTES)
        } else {
            ChunkedAssembly::Incomplete
        }
    };
    let mut pos = header_end;
    let mut body: Vec<u8> = Vec::new();
    loop {
        let Some(line_end) = find_crlf(accum, pos, MAX_CHUNK_SIZE_LINE_BYTES) else {
            return if accum.len() - pos > MAX_CHUNK_SIZE_LINE_BYTES {
                ChunkedAssembly::Reject(RESPONSE_400_BYTES)
            } else {
                incomplete()
            };
        };
        let line = &accum[pos..line_end];
        // Chunk extensions (after `;`) are ignored, as in the
        // interp decoder.
        let size_part = line
            .split(|&b| b == b';')
            .next()
            .unwrap_or(line)
            .trim_ascii();
        // 16 hex digits mirrors the interp decoder's
        // `max_size_digits`; anything longer is malformed.
        if size_part.is_empty() || size_part.len() > 16 {
            return ChunkedAssembly::Reject(RESPONSE_400_BYTES);
        }
        let Some(size) = std::str::from_utf8(size_part)
            .ok()
            .and_then(|s| u64::from_str_radix(s, 16).ok())
        else {
            return ChunkedAssembly::Reject(RESPONSE_400_BYTES);
        };
        // u128 arithmetic: `size` can be up to 2^64-1 from a
        // hostile declaration; the sum must not wrap before the
        // cap comparison.
        if body.len() as u128 + u128::from(size) > max_body as u128 {
            return ChunkedAssembly::Reject(RESPONSE_413_BYTES);
        }
        let size = size as usize;
        pos = line_end + 2;
        if size == 0 {
            let trailer_start = pos;
            let mut trailers: Vec<(String, String)> = Vec::new();
            loop {
                let Some(line_end) = find_crlf(accum, pos, MAX_TRAILER_BYTES) else {
                    return if accum.len() - trailer_start > MAX_TRAILER_BYTES {
                        ChunkedAssembly::Reject(RESPONSE_400_BYTES)
                    } else {
                        incomplete()
                    };
                };
                if line_end + 2 - trailer_start > MAX_TRAILER_BYTES {
                    return ChunkedAssembly::Reject(RESPONSE_400_BYTES);
                }
                if line_end == pos {
                    let raw_end = line_end + 2;
                    let (canonical, canonical_header_end) =
                        splice_canonical(&accum[..header_end], &body, &trailers);
                    return ChunkedAssembly::Ready(ChunkedRequest {
                        raw_end,
                        canonical,
                        header_end: canonical_header_end,
                    });
                }
                // Trailer lines without a colon are ignored -
                // interp `read_trailers_block` parity.
                if let Ok(text) = std::str::from_utf8(&accum[pos..line_end])
                    && let Some((name, value)) = text.split_once(':')
                {
                    trailers.push((name.trim().to_string(), value.trim().to_string()));
                }
                pos = line_end + 2;
            }
        }
        let data_end = pos + size;
        if accum.len() < data_end + 2 {
            return incomplete();
        }
        if &accum[data_end..data_end + 2] != b"\r\n" {
            return ChunkedAssembly::Reject(RESPONSE_400_BYTES);
        }
        body.extend_from_slice(&accum[pos..data_end]);
        pos = data_end + 2;
    }
}

/// Rewrites a chunked request into the canonical buffer shape
/// `parse_request_into` expects: the header section (sans its
/// blank line) + trailer headers + blank line + de-chunked body.
/// Returns the buffer and the body offset within it. The
/// `Transfer-Encoding` header line is kept verbatim, matching the
/// interp server (which leaves it in `request.headers`).
fn splice_canonical(head: &[u8], body: &[u8], trailers: &[(String, String)]) -> (Vec<u8>, usize) {
    let trailer_len: usize = trailers.iter().map(|(k, v)| k.len() + v.len() + 4).sum();
    let mut out = Vec::with_capacity(head.len() + trailer_len + body.len());
    // `head` ends with the blank-line `\r\n\r\n`; keep the last
    // header's CRLF, drop the blank line, splice trailers, close.
    out.extend_from_slice(&head[..head.len() - 2]);
    for (k, v) in trailers {
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    let header_end = out.len();
    out.extend_from_slice(body);
    (out, header_end)
}

/// Parses `raw` (header section + body; `header_end` is one past
/// the `\r\n\r\n`) into `request` in place. Returns false on
/// malformed input. The request line and the header lines are both
/// extracted eagerly: `request.headers` must mirror the interp
/// server's `Headers` map (lowercase names, trimmed values,
/// last-wins dedupe, sorted by name) so `r.headers` /
/// `r.headers.get(..)` agree across tiers. The whole raw buffer is
/// stashed in `request.body` with `body_offset` marking the body
/// start, so the body accessors (`gos_rt_http_request_body_str`,
/// `gos_rt_http_request_raw_body`) slice it out without another
/// copy. The body may be arbitrary binary; only the header section
/// must be UTF-8.
fn parse_request_into(raw: &[u8], header_end: usize, request: &mut GosHttpRequest) -> bool {
    let header_end = header_end.min(raw.len());
    let Ok(text) = std::str::from_utf8(&raw[..header_end]) else {
        return false;
    };
    let Some(request_line_end) = text.find("\r\n") else {
        return false;
    };
    let request_line = &text[..request_line_end];
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return false;
    };
    let Some(url) = parts.next() else {
        return false;
    };
    request.method.push_str(method);
    request.url.push_str(url);
    for line in text[request_line_end + 2..].split("\r\n") {
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            request
                .headers
                .push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
        }
    }
    normalize_header_bag(&mut request.headers);
    request.body.extend_from_slice(raw);
    request.body_offset = header_end;
    true
}

/// Collapses a parsed header list to the interp `Headers` map view:
/// name sort, then last-wins dedupe of equal names (the BTreeMap
/// insert-overwrite semantics the interp server applies).
pub(crate) fn normalize_header_bag(headers: &mut Vec<(String, String)>) {
    // Stable sort keeps insertion order within equal names, so the
    // last element of each equal-name run is the latest occurrence.
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let mut write = 0;
    for read in 0..headers.len() {
        if read + 1 < headers.len() && headers[read + 1].0 == headers[read].0 {
            continue;
        }
        headers.swap(write, read);
        write += 1;
    }
    headers.truncate(write);
}

/// A header value is safe to write only if it carries no CR, LF, or
/// NUL - the bytes that would terminate the line and split the
/// response. Mirrors the interpreter server's gate.
fn is_valid_header_value(value: &str) -> bool {
    !value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0)
}

/// A header name is safe only if it is a non-empty token with no
/// CR/LF/NUL and no framing characters (`:`, space).
fn is_valid_header_name(name: &str) -> bool {
    !name.is_empty()
        && !name
            .bytes()
            .any(|b| b == b'\r' || b == b'\n' || b == 0 || b == b':' || b == b' ')
}

/// Writes `result`'s response payload (status + headers +
/// body) into `out` as raw HTTP/1.1 bytes. Returns false if
/// `result` doesn't carry a valid OK response.
/// Default `Server` identity, byte-identical to the interp tier
/// (`gossamer-std/src/http.rs` Config default).
const SERVER_HEADER: &str = concat!("gossamer/", env!("CARGO_PKG_VERSION"));

/// Current wall-clock time as an RFC 1123 / RFC 9110 HTTP-date
/// (`Sun, 06 Nov 1994 08:49:37 GMT`). Uses the same civil-time
/// conversion as `gos_rt_time_format_rfc3339`, so the rendering matches
/// the interp tier's `time::format_rfc1123_gmt` byte for byte.
fn http_date_now() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let days = secs.div_euclid(86_400);
    let is_leap = |yr: i64| (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0;
    let year_days = |yr: i64| if is_leap(yr) { 366 } else { 365 };
    let mut year = 1970_i64;
    let mut remain = days;
    while remain >= year_days(year) {
        remain -= year_days(year);
        year += 1;
    }
    let dim = |m: i64, yr: i64| -> i64 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 if is_leap(yr) => 29,
            2 => 28,
            _ => 30,
        }
    };
    let mut month = 1_i64;
    while remain >= dim(month, year) {
        remain -= dim(month, year);
        month += 1;
    }
    let day = remain + 1;
    let tod = secs.rem_euclid(86_400);
    let dow =
        ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"][((days + 4).rem_euclid(7)) as usize];
    let mo = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][(month - 1) as usize];
    format!(
        "{dow}, {day:02} {mo} {year:04} {h:02}:{mi:02}:{s:02} GMT",
        h = tod / 3600,
        mi = (tod % 3600) / 60,
        s = tod % 60,
    )
}

pub(crate) fn extract_response_into(result: i128, out: &mut Vec<u8>) -> bool {
    if super::vec::gos_rt_result_disc(result) != 0 {
        return false;
    }
    let response_ptr = super::vec::gos_rt_result_payload(result) as *const GosHttpResponse;
    if response_ptr.is_null() {
        return false;
    }
    let response = unsafe { &*response_ptr };
    // Streamed responses are handled by the h1 server's chunked
    // drain before this function is reached. Callers that buffer
    // (the h2c bridge) serve the empty `body` instead - release the
    // pending reader here so the upstream connection closes rather
    // than leaking in the registry, matching the interp tier.
    if response.stream_handle >= 0 {
        drop(super::http_client::stream_take_for_serve(
            response.stream_handle,
        ));
    }
    // Prefer the exact byte body when present: `body_bytes` is set
    // for byte-array bodies (`gos_rt_http_response_set_body_bytes`)
    // and client-lifted responses, and may legally contain NUL bytes
    // that the c-string `body` mirror truncates at.
    let body_bytes: &[u8] = match &response.body_bytes {
        Some(bytes) => bytes.as_slice(),
        None if response.body.is_null() => b"",
        None => unsafe { CStr::from_ptr(response.body.as_ptr()).to_bytes() },
    };
    out.extend_from_slice(b"HTTP/1.1 ");
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(response.status).as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(status_reason(response.status).as_bytes());
    out.extend_from_slice(b"\r\n");
    let mut has_content_length = false;
    let mut has_content_type = false;
    let mut has_date = false;
    let mut has_server = false;
    for (k, v) in &response.headers {
        // Never emit a header whose name or value carries a CR, LF, or
        // NUL - those bytes would split the response and let an attacker
        // inject headers or a body (HTTP response splitting). Drop the
        // malformed header rather than write it, matching the interp
        // server so untrusted input reflected into a header or cookie
        // cannot smuggle a new line onto the wire.
        if !is_valid_header_name(k) || !is_valid_header_value(v) {
            continue;
        }
        if k.eq_ignore_ascii_case("content-length") {
            has_content_length = true;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if k.eq_ignore_ascii_case("date") {
            has_date = true;
        }
        if k.eq_ignore_ascii_case("server") {
            has_server = true;
        }
        // Canonical wire casing is lowercase on every tier: the
        // interp server writes through the lowercase-keyed
        // `Headers` map, so handler-given names normalize here.
        out.extend_from_slice(k.to_ascii_lowercase().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    // RFC 9110 origin-server headers, matching the interp tier: a Date
    // (unless the handler set one) and a Server identity.
    if !has_date {
        out.extend_from_slice(b"date: ");
        out.extend_from_slice(http_date_now().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_server {
        out.extend_from_slice(b"server: ");
        out.extend_from_slice(SERVER_HEADER.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_content_type {
        // Precedence: explicit header (above) > constructor-recorded
        // content type > the interp tier's default of text/plain.
        out.extend_from_slice(b"content-type: ");
        if response.content_type.is_empty() {
            out.extend_from_slice(b"text/plain; charset=utf-8");
        } else {
            out.extend_from_slice(response.content_type.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    if !has_content_length {
        out.extend_from_slice(b"content-length: ");
        out.extend_from_slice(buf.format(body_bytes.len()).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"connection: keep-alive\r\n\r\n");
    out.extend_from_slice(body_bytes);
    true
}

/// A handler response decomposed into `(status, headers, body)` for
/// transports that frame the response themselves (HTTP/3, HTTP/2).
pub(crate) type StructuredResponse = (u16, Vec<(String, String)>, Vec<u8>);

/// Extracts a handler `result`'s response as a [`StructuredResponse`]
/// for transports that frame the response themselves (HTTP/3, HTTP/2)
/// rather than emitting HTTP/1.1 wire bytes. Header names are
/// lowercased and CR/LF/NUL-bearing headers are dropped with the same
/// gate as [`extract_response_into`]; an absent content-type is
/// defaulted identically so the body the handler returned is served
/// byte-for-byte across tiers. Returns `None` when `result` is an
/// `Err` or carries a null response.
pub(crate) fn extract_response_struct(result: i128) -> Option<StructuredResponse> {
    if super::vec::gos_rt_result_disc(result) != 0 {
        return None;
    }
    let response_ptr = super::vec::gos_rt_result_payload(result) as *const GosHttpResponse;
    if response_ptr.is_null() {
        return None;
    }
    let response = unsafe { &*response_ptr };
    // A streamed response cannot be framed by the buffered h3 path;
    // release the pending reader so the upstream connection closes
    // rather than leaking, matching the h2c bridge and the interp
    // tier. The buffered `body` (empty for a pure stream) is served.
    if response.stream_handle >= 0 {
        drop(super::http_client::stream_take_for_serve(
            response.stream_handle,
        ));
    }
    let body: Vec<u8> = match &response.body_bytes {
        Some(bytes) => bytes.clone(),
        None if response.body.is_null() => Vec::new(),
        None => unsafe { CStr::from_ptr(response.body.as_ptr()).to_bytes().to_vec() },
    };
    let mut headers: Vec<(String, String)> = Vec::with_capacity(response.headers.len() + 1);
    let mut has_content_type = false;
    for (k, v) in &response.headers {
        if !is_valid_header_name(k) || !is_valid_header_value(v) {
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        headers.push((k.to_ascii_lowercase(), v.clone()));
    }
    if !has_content_type {
        let ct = if response.content_type.is_empty() {
            "text/plain; charset=utf-8".to_string()
        } else {
            response.content_type.clone()
        };
        headers.push(("content-type".to_string(), ct));
    }
    Some((response.status as u16, headers, body))
}

/// Returns the stream-registry handle when `result` is an Ok
/// response built by `gos_rt_http_response_stream_new`
/// (`stream_handle >= 0`); `None` for buffered responses and errors.
fn streamed_ok_handle(result: i128) -> Option<i64> {
    if super::vec::gos_rt_result_disc(result) != 0 {
        return None;
    }
    let response_ptr = super::vec::gos_rt_result_payload(result) as *const GosHttpResponse;
    if response_ptr.is_null() {
        return None;
    }
    let handle = unsafe { (*response_ptr).stream_handle };
    (handle >= 0).then_some(handle)
}

/// Writes the head of a streamed response (status line + headers +
/// `Transfer-Encoding: chunked` + keep-alive) into `out`. Handler
/// headers are honored with the same content-type precedence as
/// `extract_response_into`; any handler-set `Content-Length` or
/// `Transfer-Encoding` is dropped - chunked framing is unconditional
/// and RFC 7230 §3.3.3 forbids carrying both.
fn extract_stream_head_into(result: i128, out: &mut Vec<u8>) {
    let response_ptr = super::vec::gos_rt_result_payload(result) as *const GosHttpResponse;
    let response = unsafe { &*response_ptr };
    out.extend_from_slice(b"HTTP/1.1 ");
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(response.status).as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(status_reason(response.status).as_bytes());
    out.extend_from_slice(b"\r\n");
    let mut has_content_type = false;
    let mut has_date = false;
    let mut has_server = false;
    for (k, v) in &response.headers {
        // Drop CR/LF/NUL-bearing headers - see `extract_response_into`.
        if !is_valid_header_name(k) || !is_valid_header_value(v) {
            continue;
        }
        if k.eq_ignore_ascii_case("content-length") || k.eq_ignore_ascii_case("transfer-encoding") {
            continue;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        if k.eq_ignore_ascii_case("date") {
            has_date = true;
        }
        if k.eq_ignore_ascii_case("server") {
            has_server = true;
        }
        // Lowercase wire casing - same rule as `extract_response_into`.
        out.extend_from_slice(k.to_ascii_lowercase().as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_date {
        out.extend_from_slice(b"date: ");
        out.extend_from_slice(http_date_now().as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_server {
        out.extend_from_slice(b"server: ");
        out.extend_from_slice(SERVER_HEADER.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_content_type {
        out.extend_from_slice(b"content-type: ");
        if response.content_type.is_empty() {
            out.extend_from_slice(b"text/plain; charset=utf-8");
        } else {
            out.extend_from_slice(response.content_type.as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"transfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n");
}

/// Drains the pending stream for `handle` to `conn` as chunked
/// frames of at most 8 KiB (`{len:x}\r\n{bytes}\r\n`), ending with
/// the `0\r\n\r\n` terminal frame on clean EOF. Returns `false` on
/// any failure - the caller closes the connection without the
/// terminal frame so the client sees the truncation. A handle that
/// was already served (or never registered) drains as an empty
/// chunked body, matching the interp tier.
fn drain_stream_chunked<C: HttpIo>(conn: &mut C, handle: i64) -> bool {
    let Some(arc) = super::http_client::stream_take_for_serve(handle) else {
        return conn.write_all(b"0\r\n\r\n").is_ok();
    };
    let mut reader = arc.lock();
    let mut buf = [0u8; 8192];
    loop {
        match std::io::Read::read(&mut *reader, &mut buf) {
            Ok(0) => return conn.write_all(b"0\r\n\r\n").is_ok(),
            Ok(n) => {
                let mut frame = Vec::with_capacity(n + 16);
                frame.extend_from_slice(format!("{n:x}\r\n").as_bytes());
                frame.extend_from_slice(&buf[..n]);
                frame.extend_from_slice(b"\r\n");
                if conn.write_all(&frame).is_err() {
                    return false;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
            Err(_) => return false,
        }
    }
}

/// Maps a status code to its canonical reason phrase.
/// Falls back to `"OK"` for unknown codes - caller is
/// expected to use a sensible status; this is best-effort.
const fn status_reason(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        206 => "Partial Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        416 => "Range Not Satisfiable",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// In-memory `HttpIo` for driving `handle_http_conn` in tests:
    /// serves `input` to `read`, records every `write_all` in `written`.
    struct MockConn {
        input: std::io::Cursor<Vec<u8>>,
        written: Vec<u8>,
    }

    impl HttpIo for MockConn {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            std::io::Read::read(&mut self.input, buf)
        }
        fn write_all(&mut self, buf: &[u8]) -> std::io::Result<()> {
            self.written.extend_from_slice(buf);
            Ok(())
        }
    }

    #[test]
    fn oversized_request_head_is_rejected_with_431() {
        // A request line followed by 16 KiB of header bytes with no
        // terminating CRLFCRLF: the head cap must fire and reply 431
        // before the accumulator can grow without limit. The request
        // never completes, so the (null) handler address is never used.
        let mut raw = b"GET / HTTP/1.1\r\n".to_vec();
        raw.extend(std::iter::repeat_n(b'X', 16 * 1024));
        let mut conn = MockConn {
            input: std::io::Cursor::new(raw),
            written: Vec::new(),
        };
        handle_http_conn(&mut conn, 0, 0);
        let resp = String::from_utf8_lossy(&conn.written);
        assert!(
            resp.starts_with("HTTP/1.1 431"),
            "expected 431 for oversized head, got: {resp}"
        );
    }

    fn rendered(result: i128) -> String {
        let mut out = Vec::new();
        assert!(extract_response_into(result, &mut out));
        unsafe { drop_handler_result(result) };
        String::from_utf8_lossy(&out).to_ascii_lowercase()
    }

    #[test]
    fn text_response_renders_text_plain_content_type() {
        let resp = unsafe { gos_rt_http_response_text_new(200, c"ok".as_ptr()) };
        let result = super::super::vec::pack_result(0, resp as i64);
        let bytes = rendered(result);
        assert!(
            bytes.contains("content-type: text/plain; charset=utf-8"),
            "rendered response: {bytes}"
        );
        assert!(bytes.ends_with("ok"), "rendered response: {bytes}");
    }

    #[test]
    fn nul_embedded_byte_body_serves_full_bytes_with_correct_length() {
        // A byte-array body may contain NUL bytes; the writer must
        // serve `body_bytes` in full instead of the c-string mirror
        // (which stops at the first NUL).
        let resp = unsafe { gos_rt_http_response_text_new(200, c"A".as_ptr()) };
        unsafe { (*resp).body_bytes = Some(vec![0x41, 0x00, 0x42, 0x00, 0x43]) };
        let result = super::super::vec::pack_result(0, resp as i64);
        let mut out = Vec::new();
        assert!(extract_response_into(result, &mut out));
        unsafe { drop_handler_result(result) };
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains("content-length: 5"),
            "rendered response: {text:?}"
        );
        assert!(
            out.ends_with(&[0x41, 0x00, 0x42, 0x00, 0x43]),
            "rendered response: {out:?}"
        );
    }

    #[test]
    fn handler_header_names_render_lowercase_on_the_wire() {
        // Wire casing is canonical-lowercase on every tier; a
        // handler-supplied mixed-case name must normalize.
        let resp = unsafe { gos_rt_http_response_text_new(200, c"ok".as_ptr()) };
        unsafe {
            (*resp)
                .headers
                .push(("X-Mixed-Case".to_string(), "Value-Kept".to_string()));
        }
        let result = super::super::vec::pack_result(0, resp as i64);
        let mut out = Vec::new();
        assert!(extract_response_into(result, &mut out));
        unsafe { drop_handler_result(result) };
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(
            text.contains("x-mixed-case: Value-Kept"),
            "rendered response: {text:?}"
        );
        assert!(
            !text.contains("X-Mixed-Case"),
            "mixed-case name must not reach the wire: {text:?}"
        );
    }

    #[test]
    fn json_response_renders_application_json_content_type() {
        let resp = unsafe { gos_rt_http_response_json_new(200, c"{}".as_ptr()) };
        let result = super::super::vec::pack_result(0, resp as i64);
        let bytes = rendered(result);
        assert!(
            bytes.contains("content-type: application/json"),
            "rendered response: {bytes}"
        );
    }

    #[test]
    fn explicit_content_type_header_wins_over_constructor_default() {
        let resp = unsafe { gos_rt_http_response_text_new(200, c"<p>hi</p>".as_ptr()) };
        unsafe {
            (*resp)
                .headers
                .push(("Content-Type".to_string(), "text/html".to_string()));
        }
        let result = super::super::vec::pack_result(0, resp as i64);
        let bytes = rendered(result);
        assert!(
            bytes.contains("content-type: text/html"),
            "rendered response: {bytes}"
        );
        assert!(
            !bytes.contains("text/plain"),
            "constructor default must not also render: {bytes}"
        );
    }

    #[test]
    fn empty_content_type_falls_back_to_text_plain() {
        let resp = unsafe { gos_rt_http_response_text_new(200, c"ok".as_ptr()) };
        unsafe { (*resp).content_type.clear() };
        let result = super::super::vec::pack_result(0, resp as i64);
        let bytes = rendered(result);
        assert!(
            bytes.contains("content-type: text/plain; charset=utf-8"),
            "rendered response: {bytes}"
        );
    }

    #[test]
    fn content_length_scans_header_section_case_insensitively() {
        assert_eq!(
            content_length(b"POST / HTTP/1.1\r\ncOnTeNt-LeNgTh: 42\r\n\r\n"),
            42
        );
        assert_eq!(content_length(b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"), 0);
        assert_eq!(
            content_length(b"POST / HTTP/1.1\r\nContent-Length: nope\r\n\r\n"),
            0
        );
    }

    #[test]
    fn hostile_content_length_is_rejected_before_req_end_arithmetic() {
        let huge =
            content_length(b"POST / HTTP/1.1\r\nContent-Length: 18446744073709551615\r\n\r\n");
        assert_eq!(huge, usize::MAX);
        assert!(huge > MAX_REQUEST_BODY_BYTES);
        let modest = content_length(b"POST / HTTP/1.1\r\nContent-Length: 4096\r\n\r\n");
        assert!(modest <= MAX_REQUEST_BODY_BYTES);
    }

    #[test]
    fn raw_body_resolves_h1_lazy_buffer_past_header_section() {
        let mut raw: Vec<u8> = b"POST /upload HTTP/1.1\r\nContent-Length: 4\r\n\r\n".to_vec();
        let header_end = raw.len();
        let payload = [0x68u8, 0xFF, 0x00, 0x69];
        raw.extend_from_slice(&payload);

        let mut request = GosHttpRequest {
            method: String::new(),
            url: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            body_offset: 0,
            params: Vec::new(),
            values: Vec::new(),
            agent: None,
        };
        assert!(parse_request_into(&raw, header_end, &mut request));
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/upload");

        let v = unsafe { gos_rt_http_request_raw_body(std::ptr::from_ref(&request)) };
        assert!(!v.is_null());
        let vec_ref = unsafe { &*v };
        assert_eq!(vec_ref.len, 4);
        // Canonical byte-vec shape: one zero-extended i64 slot per
        // byte, so compiled-tier word loads read the byte's value.
        assert_eq!(vec_ref.elem_bytes, 8);
        let got = unsafe { std::slice::from_raw_parts(vec_ref.ptr.as_ptr().cast::<i64>(), 4) };
        assert_eq!(got, &[0x68, 0xFF, 0x00, 0x69]);
        unsafe { super::super::map::gos_rt_vec_free(v) };

        // The lossy `body` accessor resolves the same region: its
        // c-string keeps the bytes up to the embedded NUL.
        let s = unsafe { gos_rt_http_request_body_str(std::ptr::from_ref(&request)) };
        assert_eq!(unsafe { CStr::from_ptr(s) }.to_bytes(), &[0x68, 0xFF]);
        unsafe { crate::c_abi::string::gos_rt_str_free(s) };
    }

    #[test]
    fn parse_request_into_populates_normalized_headers() {
        let raw: &[u8] =
            b"GET /x HTTP/1.1\r\nHost: h\r\nX-Dup: first\r\nAccept: */*\r\nx-dup:  second \r\n\r\n";
        let header_end = raw.len();
        let mut request = GosHttpRequest {
            method: String::new(),
            url: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            body_offset: 0,
            params: Vec::new(),
            values: Vec::new(),
            agent: None,
        };
        assert!(parse_request_into(raw, header_end, &mut request));
        // Interp `Headers` map view: lowercase names, trimmed
        // values, last-wins dedupe, sorted by name.
        assert_eq!(
            request.headers,
            vec![
                ("accept".to_string(), "*/*".to_string()),
                ("host".to_string(), "h".to_string()),
                ("x-dup".to_string(), "second".to_string()),
            ]
        );
    }

    #[test]
    fn parse_request_into_merges_chunked_trailers_into_headers() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"3\r\nabc\r\n0\r\nX-Trailer: tval\r\n\r\n");
        let ChunkedAssembly::Ready(c) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("complete frame must assemble");
        };
        let mut request = fresh_request();
        assert!(parse_request_into(&c.canonical, c.header_end, &mut request));
        assert!(
            request
                .headers
                .contains(&("x-trailer".to_string(), "tval".to_string())),
            "trailer promoted with interp lowercase semantics: {:?}",
            request.headers
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn streamed_response_drains_chunked_frames_to_loopback() {
        use std::io::Read;

        // > 8 KiB so the drain emits two data frames + terminal.
        let payload: Vec<u8> = (0..10_000u32).map(|i| (i % 251) as u8).collect();
        let reader: super::http_client::StreamReader =
            std::io::BufReader::new(Box::new(std::io::Cursor::new(payload.clone()))
                as Box<dyn std::io::Read + Send + Sync>);
        let handle = super::http_client::stream_registry_register(reader);
        let blob = [handle, 200i64, 0i64];
        let ct = std::ffi::CString::new("application/octet-stream").unwrap();
        let resp = unsafe { gos_rt_http_response_stream_new(201, ct.as_ptr(), blob.as_ptr()) };
        let result = super::super::vec::pack_result(0, resp as i64);
        assert_eq!(streamed_ok_handle(result), Some(handle));

        let mut head = Vec::new();
        extract_stream_head_into(result, &mut head);
        unsafe { drop_handler_result(result) };
        let head_text = String::from_utf8_lossy(&head).to_ascii_lowercase();
        assert!(head_text.starts_with("http/1.1 201"), "head: {head_text}");
        assert!(head_text.contains("transfer-encoding: chunked"));
        assert!(head_text.contains("content-type: application/octet-stream"));
        assert!(
            !head_text.contains("content-length"),
            "chunked head must not carry Content-Length: {head_text}"
        );

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut sock = std::net::TcpStream::connect(addr).unwrap();
            let mut raw = Vec::new();
            sock.read_to_end(&mut raw).unwrap();
            raw
        });
        let (server_side, _) = listener.accept().unwrap();
        let mut conn = HttpConn::wrap(server_side).expect("wrap");
        assert!(drain_stream_chunked(&mut conn, handle), "clean drain");
        drop(conn);

        let raw = client.join().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(b"2000\r\n");
        expected.extend_from_slice(&payload[..8192]);
        expected.extend_from_slice(b"\r\n");
        expected.extend_from_slice(format!("{:x}\r\n", payload.len() - 8192).as_bytes());
        expected.extend_from_slice(&payload[8192..]);
        expected.extend_from_slice(b"\r\n0\r\n\r\n");
        assert_eq!(raw, expected, "exact chunked framing with terminal frame");

        // The handle was taken at serve time - a second drain answers
        // only the terminal frame (empty chunked body).
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut sock = std::net::TcpStream::connect(addr).unwrap();
            let mut raw = Vec::new();
            sock.read_to_end(&mut raw).unwrap();
            raw
        });
        let (server_side, _) = listener.accept().unwrap();
        let mut conn = HttpConn::wrap(server_side).expect("wrap");
        assert!(drain_stream_chunked(&mut conn, handle));
        drop(conn);
        assert_eq!(client.join().unwrap(), b"0\r\n\r\n");
    }

    /// Reader that yields one chunk and then fails, modelling an
    /// upstream connection dying mid-proxy.
    struct FailAfterFirstRead {
        first: Option<Vec<u8>>,
    }

    impl std::io::Read for FailAfterFirstRead {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.first.take() {
                Some(bytes) => {
                    buf[..bytes.len()].copy_from_slice(&bytes);
                    Ok(bytes.len())
                }
                None => Err(std::io::Error::other("upstream died")),
            }
        }
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn streamed_response_mid_stream_error_closes_without_terminal_frame() {
        use std::io::Read;

        let reader: super::http_client::StreamReader =
            std::io::BufReader::new(Box::new(FailAfterFirstRead {
                first: Some(b"partial".to_vec()),
            }) as Box<dyn std::io::Read + Send + Sync>);
        let handle = super::http_client::stream_registry_register(reader);
        super::http_client::stream_consume_for_response(handle);

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let client = std::thread::spawn(move || {
            let mut sock = std::net::TcpStream::connect(addr).unwrap();
            let mut raw = Vec::new();
            sock.read_to_end(&mut raw).unwrap();
            raw
        });
        let (server_side, _) = listener.accept().unwrap();
        let mut conn = HttpConn::wrap(server_side).expect("wrap");
        assert!(
            !drain_stream_chunked(&mut conn, handle),
            "mid-stream read error must report failure so the caller closes"
        );
        drop(conn);

        let raw = client.join().unwrap();
        assert_eq!(
            raw, b"7\r\npartial\r\n",
            "the delivered frame reaches the wire; no terminal frame follows"
        );
    }

    fn fresh_request() -> GosHttpRequest {
        GosHttpRequest {
            method: String::new(),
            url: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            body_offset: 0,
            params: Vec::new(),
            values: Vec::new(),
            agent: None,
        }
    }

    fn chunked_head() -> Vec<u8> {
        b"POST /up HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec()
    }

    #[test]
    fn transfer_encoding_detection_matches_interp_semantics() {
        assert!(transfer_encoding_is_chunked(
            b"POST / HTTP/1.1\r\nTransfer-Encoding: Chunked\r\n\r\n"
        ));
        assert!(transfer_encoding_is_chunked(
            b"POST / HTTP/1.1\r\ntransfer-encoding:chunked\r\n\r\n"
        ));
        // Multi-coding values are not "chunked" on the interp tier
        // (whole-value match) - same here.
        assert!(!transfer_encoding_is_chunked(
            b"POST / HTTP/1.1\r\nTransfer-Encoding: gzip, chunked\r\n\r\n"
        ));
        assert!(!transfer_encoding_is_chunked(
            b"GET / HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
    }

    #[test]
    fn chunked_assembly_round_trips_binary_chunks() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"4\r\n");
        raw.extend_from_slice(&[0x00, 0xFF, 0x68, 0x69]);
        raw.extend_from_slice(b"\r\n2\r\n");
        raw.extend_from_slice(&[0xFE, 0x00]);
        raw.extend_from_slice(b"\r\n0\r\n\r\n");

        let ChunkedAssembly::Ready(c) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("complete frame must assemble");
        };
        assert_eq!(c.raw_end, raw.len());

        let mut request = fresh_request();
        assert!(parse_request_into(&c.canonical, c.header_end, &mut request));
        assert_eq!(request.method, "POST");
        assert_eq!(request.url, "/up");
        assert_eq!(
            &request.body[request.body_offset..],
            &[0x00, 0xFF, 0x68, 0x69, 0xFE, 0x00],
            "body accessors must see the de-chunked binary payload"
        );
    }

    #[test]
    fn chunked_assembly_incomplete_until_terminal_then_exact_boundary() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\nX-T: v\r\n\r\n");
        let frame_end = raw.len();

        for cut in header_end..frame_end {
            assert!(
                matches!(
                    assemble_chunked_request(&raw[..cut], header_end, MAX_REQUEST_BODY_BYTES),
                    ChunkedAssembly::Incomplete
                ),
                "prefix of {cut} bytes must be Incomplete"
            );
        }

        // Pipelined bytes after the frame must not move the boundary.
        raw.extend_from_slice(b"GET /next HTTP/1.1\r\nHost: x\r\n\r\n");
        let ChunkedAssembly::Ready(c) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("complete frame must assemble");
        };
        assert_eq!(
            c.raw_end, frame_end,
            "raw_end must stop at the frame boundary"
        );
    }

    #[test]
    fn chunked_assembly_merges_trailer_headers_into_canonical() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"3\r\nabc\r\n0\r\nX-Trailer: tval\r\nbogus-no-colon\r\n\r\n");

        let ChunkedAssembly::Ready(c) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("complete frame must assemble");
        };
        let canonical = String::from_utf8_lossy(&c.canonical).into_owned();
        let header_section = &canonical[..c.header_end];
        assert!(
            header_section.contains("X-Trailer: tval\r\n"),
            "trailer spliced into the header section: {header_section}"
        );
        assert!(
            header_section.contains("Transfer-Encoding: chunked\r\n"),
            "original headers preserved verbatim: {header_section}"
        );
        assert_eq!(&canonical[c.header_end..], "abc");
    }

    #[test]
    fn chunked_assembly_rejects_malformed_size_line() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"zz\r\nWiki\r\n0\r\n\r\n");
        let ChunkedAssembly::Reject(resp) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("bad hex size must be rejected");
        };
        assert_eq!(resp, RESPONSE_400_BYTES);

        // Missing CRLF after the chunk data is framing corruption.
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(b"4\r\nWikiXX0\r\n\r\n");
        let ChunkedAssembly::Reject(resp) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("missing data CRLF must be rejected");
        };
        assert_eq!(resp, RESPONSE_400_BYTES);
    }

    #[test]
    fn chunked_assembly_rejects_declared_oversize_before_data_arrives() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        raw.extend_from_slice(format!("{:x}\r\n", MAX_REQUEST_BODY_BYTES + 1).as_bytes());
        let ChunkedAssembly::Reject(resp) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("hostile declared size must be rejected from the size line alone");
        };
        assert_eq!(resp, RESPONSE_413_BYTES);
    }

    #[test]
    fn chunked_assembly_rejects_cumulative_body_over_cap() {
        let mut raw = chunked_head();
        let header_end = raw.len();
        let chunk = vec![b'a'; 64 * 1024];
        for _ in 0..17 {
            raw.extend_from_slice(format!("{:x}\r\n", chunk.len()).as_bytes());
            raw.extend_from_slice(&chunk);
            raw.extend_from_slice(b"\r\n");
        }
        raw.extend_from_slice(b"0\r\n\r\n");
        assert!(17 * chunk.len() > MAX_REQUEST_BODY_BYTES);
        let ChunkedAssembly::Reject(resp) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("cumulative de-chunked size past the cap must be rejected");
        };
        assert_eq!(resp, RESPONSE_413_BYTES);
    }

    #[test]
    fn chunked_assembly_rejects_runaway_raw_framing() {
        // Endless 1-byte chunks with no terminal frame: the decoded
        // size stays under the cap, but the raw frame keeps growing
        // - the raw cap must stop it while still Incomplete.
        let mut raw = chunked_head();
        let header_end = raw.len();
        while raw.len() - header_end <= MAX_CHUNKED_RAW_BYTES {
            raw.extend_from_slice(b"1\r\nA\r\n");
        }
        let ChunkedAssembly::Reject(resp) =
            assemble_chunked_request(&raw, header_end, MAX_REQUEST_BODY_BYTES)
        else {
            panic!("unterminated chunk stream past the raw cap must be rejected");
        };
        assert_eq!(resp, RESPONSE_413_BYTES);
    }

    /// Echo handler used by the loopback tests: renders the body
    /// length, the body text, and the `X-Trailer` header value so
    /// assertions can see exactly what the handler observed.
    unsafe extern "C" fn echo_handler(_env: *mut u8, req: *mut GosHttpRequest) -> i128 {
        let request = unsafe { &*req };
        let body = &request.body[request.body_offset.min(request.body.len())..];
        let trailer = request
            .headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("x-trailer"))
            .map_or(String::new(), |(_, v)| v.clone());
        let text = format!(
            "len={};body={};trailer={}",
            body.len(),
            String::from_utf8_lossy(body),
            trailer
        );
        let c = std::ffi::CString::new(text).unwrap();
        let resp = unsafe { gos_rt_http_response_text_new(200, c.as_ptr()) };
        super::super::vec::pack_result(0, resp as i64)
    }

    /// Runs `handle_http_conn` for one accepted connection, returns
    /// everything the client read until the server closed.
    fn roundtrip_raw_bytes(client_bytes: &[u8], fn_addr: usize) -> Vec<u8> {
        use std::io::{Read, Write};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server = std::thread::spawn(move || {
            let (stream, _) = listener.accept().unwrap();
            let mut conn = HttpConn::wrap(stream).expect("wrap");
            handle_http_conn(&mut conn, 0, fn_addr);
        });
        let mut sock = std::net::TcpStream::connect(addr).unwrap();
        sock.write_all(client_bytes).unwrap();
        let _ = sock.shutdown(std::net::Shutdown::Write);
        let mut raw = Vec::new();
        let _ = sock.read_to_end(&mut raw);
        server.join().unwrap();
        raw
    }

    fn echo_fn_addr() -> usize {
        (echo_handler as HandlerFn) as usize
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn chunked_request_dechunks_for_handler_and_preserves_pipelined_boundary() {
        let mut wire: Vec<u8> =
            b"POST /up HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\n\r\n".to_vec();
        wire.extend_from_slice(b"4\r\nWiki\r\n5\r\npedia\r\n0\r\nX-Trailer: tval\r\n\r\n");
        wire.extend_from_slice(b"GET /next HTTP/1.1\r\nHost: x\r\n\r\n");

        let raw = roundtrip_raw_bytes(&wire, echo_fn_addr());
        let text = String::from_utf8_lossy(&raw).into_owned();
        let responses = text.matches("HTTP/1.1 ").count();
        assert_eq!(
            responses, 2,
            "one response per request - chunked body must not be misparsed as a pipelined request: {text}"
        );
        assert!(
            text.contains("len=9;body=Wikipedia;trailer=tval"),
            "handler must see the de-chunked body and the promoted trailer: {text}"
        );
        assert!(
            text.contains("len=0;body=;trailer="),
            "pipelined GET after the chunked frame must parse cleanly: {text}"
        );
        assert!(
            !text.contains("400"),
            "no request in this exchange is malformed: {text}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn chunked_with_content_length_rejected_400_on_wire() {
        let wire = b"POST /up HTTP/1.1\r\nHost: x\r\nTransfer-Encoding: chunked\r\nContent-Length: 4\r\n\r\n4\r\nWiki\r\n0\r\n\r\n";
        let raw = roundtrip_raw_bytes(wire, echo_fn_addr());
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            text.starts_with("HTTP/1.1 400 "),
            "smuggling-shaped request must get 400 and close: {text}"
        );
        assert_eq!(text.matches("HTTP/1.1 ").count(), 1);
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn content_length_over_cap_rejected_413_on_wire() {
        let wire = format!(
            "POST /up HTTP/1.1\r\nHost: x\r\nContent-Length: {}\r\n\r\n",
            MAX_REQUEST_BODY_BYTES + 1
        );
        let raw = roundtrip_raw_bytes(wire.as_bytes(), echo_fn_addr());
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            text.starts_with("HTTP/1.1 413 "),
            "declared Content-Length past the cap must get 413: {text}"
        );
    }

    #[test]
    #[cfg_attr(miri, ignore)] // network round-trip: Miri has no socket syscalls
    fn declared_chunk_over_cap_rejected_413_mid_stream() {
        // The hostile size line alone triggers the reject - the
        // server must not wait for (or buffer) the declared body.
        let mut wire = chunked_head();
        wire.extend_from_slice(format!("{:x}\r\nAA", MAX_REQUEST_BODY_BYTES + 1).as_bytes());
        let raw = roundtrip_raw_bytes(&wire, echo_fn_addr());
        let text = String::from_utf8_lossy(&raw).into_owned();
        assert!(
            text.starts_with("HTTP/1.1 413 "),
            "oversize chunk declaration must get 413 mid-stream: {text}"
        );
    }

    #[test]
    fn parse_request_into_accepts_binary_body_after_utf8_headers() {
        let mut raw: Vec<u8> = b"POST / HTTP/1.1\r\nContent-Length: 2\r\n\r\n".to_vec();
        let header_end = raw.len();
        raw.extend_from_slice(&[0xFE, 0xFD]);
        let mut request = GosHttpRequest {
            method: String::new(),
            url: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            body_offset: 0,
            params: Vec::new(),
            values: Vec::new(),
            agent: None,
        };
        assert!(parse_request_into(&raw, header_end, &mut request));
        assert_eq!(request.body_offset, header_end);
        assert_eq!(&request.body[request.body_offset..], &[0xFE, 0xFD]);
    }

    #[test]
    fn handler_header_names_write_lowercase_on_the_wire() {
        let resp = unsafe { gos_rt_http_response_text_new(200, c"ok".as_ptr()) };
        unsafe {
            super::http_client::gos_rt_http_response_set_header(
                resp,
                c"X-Custom-Thing".as_ptr(),
                c"v".as_ptr(),
            );
        }
        let result = super::super::vec::pack_result(0, resp as i64);
        let mut out = Vec::new();
        assert!(extract_response_into(result, &mut out));
        unsafe { drop_handler_result(result) };
        // Raw bytes - no case folding before the assertions.
        let text = String::from_utf8_lossy(&out).into_owned();
        assert!(text.contains("x-custom-thing: v\r\n"), "wire: {text}");
        assert!(!text.contains("X-Custom-Thing"), "wire: {text}");
        assert!(text.contains("content-type: text/plain; charset=utf-8\r\n"));
        assert!(text.contains("connection: keep-alive\r\n"));
        assert!(text.contains("content-length: 2\r\n"));
    }

    #[test]
    fn stream_head_header_names_write_lowercase_on_the_wire() {
        let reader: super::http_client::StreamReader = std::io::BufReader::new(Box::new(
            std::io::Cursor::new(Vec::new()),
        )
            as Box<dyn std::io::Read + Send + Sync>);
        let handle = super::http_client::stream_registry_register(reader);
        let blob = [handle, 200i64, 0i64];
        let resp =
            unsafe { gos_rt_http_response_stream_new(200, c"text/csv".as_ptr(), blob.as_ptr()) };
        unsafe {
            super::http_client::gos_rt_http_response_set_header(
                resp,
                c"X-MiXeD".as_ptr(),
                c"v".as_ptr(),
            );
        }
        let result = super::super::vec::pack_result(0, resp as i64);
        let mut head = Vec::new();
        extract_stream_head_into(result, &mut head);
        unsafe { drop_handler_result(result) };
        drop(super::http_client::stream_take_for_serve(handle));
        let text = String::from_utf8_lossy(&head).into_owned();
        assert!(text.contains("x-mixed: v\r\n"), "head: {text}");
        assert!(!text.contains("X-MiXeD"), "head: {text}");
        assert!(text.contains("transfer-encoding: chunked\r\nconnection: keep-alive\r\n"));
        assert!(text.contains("content-type: text/csv\r\n"));
    }
}
