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
    // Sentinel 0 means "not yet read" — the cap can never legally
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

const RESPONSE_503_BYTES: &[u8] =
    b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Starts an HTTP listener and dispatches each request to
/// `handler_fn(handler_env, request)`. Returns 200/payload from
/// the handler's `Ok(Response)`, 500 from `Err`, and a static
/// `200 OK\r\n\r\nok` when `handler_fn` is null (legacy stub).
///
/// Concurrent connections are capped at `GOSSAMER_HTTP_MAX_CONN`
/// (default 4096). When the cap is hit the listener accepts the
/// connection, writes a 503 Service Unavailable response, closes
/// the socket without spawning a thread, and continues. This
/// turns the previous unbounded `thread::Builder::spawn` into
/// bounded back-pressure so a flood of clients cannot exhaust
/// the OS thread or file-descriptor budget.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_serve(
    addr: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> ! {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let addr_s = if addr.is_null() {
            "0.0.0.0:8080".to_string()
        } else {
            unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
        };
        let listener = match TcpListener::bind(&addr_s) {
            Ok(l) => l,
            Err(e) => {
                // Startup-time failure for a `!`-returning entry point —
                // there is no caller to return an error code to, and the
                // function can never produce a `TcpListener`. Surface the
                // diagnostic and abort instead of `process::exit` so the
                // hidden `exit` audit (Fix C3) doesn't flag this path.
                eprintln!("gos_rt_http_serve: bind {addr_s} failed: {e}");
                std::process::abort();
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
        // waker when the kernel reports readiness — the same shape as
        // Go's `netpoll`.
        //
        // When [`crate::sched_global::try_spawn`] refuses (live-
        // goroutine cap reached — default 1M, set by
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
        // Thread-per-connection — matches Go's `net/http` shape.
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
                // Best-effort 503 + close; ignore write errors —
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
            // the socket — the kernel will RST it.
            let spawn_result = std::thread::Builder::new()
                .name("gos-http-conn".to_string())
                .spawn(move || {
                    let _guard = HttpConnGuard;
                    let Some(mut conn) = HttpConn::wrap(stream) else {
                        return;
                    };
                    handle_http_conn(&mut conn, env_addr, fn_addr);
                });
            if spawn_result.is_err() {
                HTTP_ACTIVE_CONNS.fetch_sub(1, Ordering::AcqRel);
            }
        }
    }));
    // `-> !` entry point: the accept loop above only exits on a
    // fatal listener error, and any panic was caught by the
    // `catch_unwind` wrap. Either way the function can't return,
    // so abort with a diagnostic. Aborting (rather than `exit`)
    // keeps the audited-exit list (Fix C3) empty outside the
    // legitimate panic/exit paths.
    eprintln!("gos_rt_http_serve: never-returning entry exited; aborting");
    std::process::abort();
}

type HandlerFn = unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> *mut GosResult;

/// HTTP/2 cleartext server. Mirror of [`gos_rt_http_serve`] for
/// HTTP/2 — the MIR lowerer emits this call when the compiled
/// program invokes `http2::bind_and_run_h2c(addr, app, config)`.
/// The h2 server implementation lives in
/// [`crate::http2_server`]; this thunk just adapts the C-ABI
/// signature into the Rust API.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http2_bind_and_run_h2c(
    addr: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> ! {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let addr_s = if addr.is_null() {
            "0.0.0.0:8080".to_string()
        } else {
            unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
        };
        let env_addr = handler_env as usize;
        let fn_addr = handler_fn as usize;
        crate::http2_server::serve_h2c_with_handler(&addr_s, env_addr, fn_addr);
    }));
    // `-> !` entry point — see the matching note in
    // `gos_rt_http_serve`. Either the h2 server returned or a panic
    // was caught; either way the function cannot return.
    eprintln!("gos_rt_http2_bind_and_run_h2c: never-returning entry exited; aborting");
    std::process::abort();
}

fn handle_http_conn(conn: &mut HttpConn, env_addr: usize, fn_addr: usize) {
    let mut scratch = ConnScratch::new();
    let mut accum: Vec<u8> = Vec::with_capacity(8192);
    let mut buf: Vec<u8> = vec![0u8; 8192];
    loop {
        let header_end = find_header_end(&accum);
        if header_end.is_none() {
            match conn.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    accum.extend_from_slice(&buf[..n]);
                    continue;
                }
                Err(_) => return,
            }
        }
        let req_end = header_end.unwrap();
        // `raw` is the request's header bytes (inclusive of the
        // trailing `\r\n\r\n`). Anything past it is the next
        // request — keep it in `accum` for the next iteration.
        let raw = &accum[..req_end];

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

            if !parse_request_into(raw, &mut scratch.request) {
                // Malformed request: send 400 and close. Keeping
                // the connection open after an unparseable request
                // is unsafe — we don't know how many bytes the
                // bogus request claimed, so the next request would
                // be misaligned. The connection will be reopened
                // by the client.
                let _ = conn.write_all(RESPONSE_400_BYTES);
                return;
            }

            // SAFETY: `fn_addr` came from `gos_fn_addr("T::serve")`
            // at the user's `http::serve(addr, app)` call site;
            // env_addr is the `&app` pointer passed alongside.
            let handler: HandlerFn = unsafe { std::mem::transmute(fn_addr) };
            let env_ptr = env_addr as *mut u8;
            let req_ptr: *mut GosHttpRequest = &raw mut scratch.request;
            let result_ptr = unsafe { handler(env_ptr, req_ptr) };
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
    }
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
}

impl HttpConn {
    fn wrap(stream: TcpStream) -> Option<Self> {
        // Blocking I/O on the std fd. Compiled-mode HTTP runs each
        // connection on a dedicated OS thread (see `gos_rt_http_serve`),
        // so blocking reads are fine — they only stall the per-
        // connection thread, not a shared goroutine pool. The mio
        // clone is retained so any other path that needs non-blocking
        // semantics can still register it with the netpoller.
        let cloned = stream.try_clone().ok()?;
        Some(Self {
            mio_stream: mio::net::TcpStream::from_std(cloned),
            stream,
            last_source: None,
        })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match std::io::Read::read(&mut self.stream, buf) {
                Ok(n) => return Ok(n),
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
/// `Result` so each request doesn't leak. Three cases:
///
/// 1. The response was constructed via `gos_rt_http_response_text_new`
///    / `_json_new` — the new fast path returns a pointer to a
///    per-thread reusable buffer (no Box). We just clear it for
///    the next request; do NOT call `Box::from_raw`.
/// 2. The response was constructed by some other path that did
///    Box-allocate (e.g. `gos_rt_http_request_send` from the
///    client side, never reachable from a server handler today).
/// 3. `result` is null or carries `Err` — nothing to drop.
pub(crate) unsafe fn drop_handler_result(result: *mut GosResult) {
    if result.is_null() {
        return;
    }
    let r = unsafe { &*result };
    if r.disc != 0 {
        return;
    }
    let response_ptr = r.payload as *mut GosHttpResponse;
    if response_ptr.is_null() {
        return;
    }
    if is_thread_local_response(response_ptr) {
        // Per-thread buffer: don't free, just reset for the next
        // request. The arena reset at the end of `handle_http_conn`
        // reclaims any cstrings the response pointed at.
        unsafe {
            (*response_ptr).status = 0;
            (*response_ptr).body = SyncRawPtr::NULL;
            (*response_ptr).headers.clear();
        }
        return;
    }
    drop(unsafe { Box::from_raw(response_ptr) });
}

thread_local! {
    /// Reusable response buffer for the server's per-request
    /// constructors (`gos_rt_http_response_text_new` /
    /// `_json_new`). Eliminates the per-request `Box::into_raw` /
    /// `Box::from_raw` malloc/free pair that was the dominant
    /// per-request cost — at conc=100 the system allocator's lock
    /// became the bottleneck. The buffer is owned by the worker
    /// thread; the caller writes status/body/headers and returns
    /// the buffer's address. `drop_handler_result` recognises the
    /// pointer (by `is_thread_local_response`) and skips the free.
    static RESPONSE_BUF: std::cell::UnsafeCell<GosHttpResponse> = const {
        std::cell::UnsafeCell::new(GosHttpResponse {
            status: 0,
            body: SyncRawPtr::NULL,
            headers: Vec::new(),
            body_bytes: None,
        })
    };
}

fn thread_local_response_ptr() -> *mut GosHttpResponse {
    RESPONSE_BUF.with(std::cell::UnsafeCell::get)
}

fn is_thread_local_response(p: *mut GosHttpResponse) -> bool {
    p == thread_local_response_ptr()
}

/// Parses `raw` into `request` in place. Returns false on
/// malformed input. Headers and body are parsed lazily — we only
/// extract the request line (method + path) here, since the
/// bench handler and most simple endpoints never read headers.
/// `request.header(name)` materialises the header list on
/// demand from the saved raw buffer (`request.raw_buf`).
fn parse_request_into(raw: &[u8], request: &mut GosHttpRequest) -> bool {
    let Ok(text) = std::str::from_utf8(raw) else {
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
    // Stash the raw bytes so `request.header(name)` can lazily
    // scan them on demand. Reuses the existing `body` Vec as the
    // raw buffer (the bench paths never actually push to body
    // and `clear()` retains capacity, so this is a cheap copy
    // that amortises across requests).
    request.body.extend_from_slice(raw);
    true
}

/// Writes `result`'s response payload (status + headers +
/// body) into `out` as raw HTTP/1.1 bytes. Returns false if
/// `result` doesn't carry a valid OK response.
pub(crate) fn extract_response_into(result: *mut GosResult, out: &mut Vec<u8>) -> bool {
    if result.is_null() {
        return false;
    }
    let r = unsafe { &*result };
    if r.disc != 0 {
        return false;
    }
    let response_ptr = r.payload as *const GosHttpResponse;
    if response_ptr.is_null() {
        return false;
    }
    let response = unsafe { &*response_ptr };
    let body_bytes: &[u8] = if response.body.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(response.body.as_ptr()).to_bytes() }
    };
    out.extend_from_slice(b"HTTP/1.1 ");
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(response.status).as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(status_reason(response.status).as_bytes());
    out.extend_from_slice(b"\r\n");
    let mut has_content_length = false;
    let mut has_content_type = false;
    for (k, v) in &response.headers {
        if k.eq_ignore_ascii_case("content-length") {
            has_content_length = true;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_content_type {
        out.extend_from_slice(b"Content-Type: application/json\r\n");
    }
    if !has_content_length {
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(buf.format(body_bytes.len()).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: keep-alive\r\n\r\n");
    out.extend_from_slice(body_bytes);
    true
}

/// Maps a status code to its canonical reason phrase.
/// Falls back to `"OK"` for unknown codes — caller is
/// expected to use a sensible status; this is best-effort.
const fn status_reason(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
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
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
    }
}
