# HTTP/2 in Gossamer — Architecture Notes

Reference for anyone touching `std::http_h2`, `std::async_tcp`, or
`std::runtime_future`. The big picture is:

- HTTP/2 is a first-party standard-library feature. The h2 stack
  is always compiled into `gossamer-std`. There is no feature
  gate. There is no separate runtime.
- Gossamer's own goroutine scheduler is the only executor. The
  `h2` crate's async API is driven by a one-page future-pump
  (`runtime_future::drive`) that runs on each goroutine.
- The only Tokio surface used is `tokio::io::{AsyncRead, AsyncWrite,
  ReadBuf}` — pure trait definitions. No `tokio::runtime`, no
  reactor, no time, no net. Same constraint applies to
  `tokio-rustls`, which is consumed only for its `TlsAcceptor`
  built on top of the same Async\* traits.

## Module map

| Module | What it is |
|--------|------------|
| `std::runtime_future` | Future-pump. Given any `Future`, polls it; on `Poll::Pending` parks the calling goroutine; resumes when the Waker fires. The Waker is rooted in the scheduler's `unpark(gid)` primitive. |
| `std::async_tcp` | `AsyncRead + AsyncWrite` over a non-blocking, mio-registered `net::TcpStream`. When a `try_read`/`try_write` returns `WouldBlock`, the bridge stores the current Waker, registers it under the goroutine's gid with the netpoller, and returns `Pending`. |
| `std::http_h2` | h2-crate server connection runner. Takes any `AsyncRead+AsyncWrite+Unpin+Send` source, drives the h2 handshake, multiplexes inbound streams onto one goroutine per stream. |
| `std::http::server::bind_and_run_tls_h2` | HTTPS server that does ALPN. Each TCP accept becomes a goroutine that drives the async TLS handshake via `tokio_rustls::TlsAcceptor`, inspects `ServerConnection::alpn_protocol()`, and dispatches to `http_h2::serve_connection_async` when `h2` was negotiated. |

## The goroutine-as-executor pattern

A standard async runtime needs a Reactor (IO readiness), a
Timer Wheel, and an Executor (poll loop). Gossamer already
ships the first two as part of its scheduler (mio-backed
netpoller; cooperative timing wheel under `std::time`). The
third — the executor — is fused into the goroutine itself.

```rust
// runtime_future::drive
pub fn drive<F: Future>(fut: F) -> F::Output {
    let mut fut = Box::pin(fut);
    let waker = goroutine_waker(current_gid().expect("on a goroutine"));
    let mut cx = Context::from_waker(&waker);
    loop {
        match fut.as_mut().poll(&mut cx) {
            Poll::Ready(out) => return out,
            Poll::Pending => {
                if waker_already_fired() {
                    continue; // race: woken between poll and park.
                }
                sched_global::park(ParkReason::Io, |_| {});
            }
        }
    }
}
```

The `goroutine_waker` is a `std::task::Wake` impl on an
`Arc<GoroutineWaker>`. `wake()` and `wake_by_ref()`:

1. Set an `AtomicBool` flag (`woke = true`).
2. Call `sched_global::scheduler().unpark(gid)`.

The `woke` flag closes the obvious race: between the time the
future returns `Pending` and the time we call `park`, an IO
event may fire and call `wake`. Without the flag we would park
forever. With it, we re-poll instead.

## Async IO bridge

`async_tcp::AsyncTcpStream` wraps the same `net::TcpStream` that
synchronous goroutines use. The async impls are mechanical:

```rust
fn poll_read(&mut self, cx, buf) -> Poll<io::Result<()>> {
    match self.inner.try_read(slice) {
        Ok(n) => { buf.advance(n); Ready(Ok(())) }
        Err(e) if e.kind() == WouldBlock => {
            arm_io_wake(&mut self.inner, Interest::Readable, cx, slot);
            Pending
        }
        Err(e) => Ready(Err(e)),
    }
}
```

`arm_io_wake` does the bookkeeping:

1. Stash `cx.waker().clone()` in a `Mutex<Option<Waker>>` slot on
   the stream.
2. Register a one-shot callback with the netpoller against the
   current goroutine's gid; the callback drains the slot and
   calls `Waker::wake()`.
3. Register the underlying `mio::TcpStream` with the netpoller
   for the chosen `Interest`.

When the kernel signals readiness, the netpoller fires the
callback, which fires the Waker, which unparks the goroutine,
which re-enters `drive`, which re-calls `poll_read`. The cycle
is closed.

## h2 server connection lifecycle

`http_h2::serve_connection(io, handler, config)` is the canonical
entry point. It:

1. Wraps the body inside `runtime_future::drive`, so it can
   `await` h2's async API while running synchronously from the
   caller's perspective.
2. Runs `h2::server::Builder::handshake(io).await` to do the
   HTTP/2 preface + initial SETTINGS exchange.
3. Loops on `conn.accept().await`, which yields one
   `(http::Request<RecvStream>, SendResponse<Bytes>)` per
   inbound stream.
4. For each stream, spawns a child goroutine via
   `gossamer_runtime::sched_global::spawn`. The child runs
   `drive(serve_one_stream(...))` — i.e. it does its own future
   pump for the body-read / handler-invoke / send-response
   sequence on its own goroutine. This is the model that gives
   us real per-stream isolation: a handler that blocks doesn't
   block the connection, and crash isolation is bounded to the
   one stream via `catch_unwind` around the handler call.

`serve_connection_async` is the same logic exposed for callers
that are already inside a future (e.g. the ALPN dispatcher) and
want to `.await` the connection rather than re-enter `drive`.

## ALPN dispatch

`http::server::bind_and_run_tls_h2(addr, tls_config, h2_config,
handler)` is the HTTPS entry. The flow per accepted TCP socket:

1. Spawn a goroutine.
2. Wrap the std `TcpStream` in `net::TcpStream::from_std_blocking`
   (which actually flips to non-blocking + attaches a mio mirror —
   the name is historical) and then in `AsyncTcpStream`.
3. Build a `tokio_rustls::TlsAcceptor` from the `ServerConfig`
   (auto-prepending `b"h2"` to the ALPN list if not already
   advertised — `ensure_alpn_h2`).
4. `acceptor.accept(async_tcp).await` drives the rustls handshake
   over the async transport. Internally tokio-rustls does the same
   state-machine poll loop we'd otherwise hand-roll.
5. After the handshake, inspect
   `tls_stream.get_ref().1.alpn_protocol()`. If it's `Some(b"h2")`,
   call `serve_connection_async(tls_stream, ...)`. Otherwise close
   the TLS stream cleanly. Mixed h1+h2 dispatch on the same TLS
   listener is on the v0.4.1 roadmap (it needs an async h1 reader
   on top of `AsyncTcpStream`). For the current release, run
   `bind_and_run_tls` for h1 and `bind_and_run_tls_h2` for h2.

## Things we do not do

- **No nested executors.** We never call
  `futures::executor::block_on` from within a goroutine that's
  already pumping a future. The single executor is `drive`.
- **No `tokio::spawn`.** Goroutines come from
  `gossamer_runtime::sched_global::spawn`. The h2 stream
  dispatch path spawns child goroutines explicitly.
- **No `tokio::time::sleep`.** `std::time::sleep` is the only
  sleep primitive, and it parks the goroutine on the scheduler's
  timer wheel.
- **No blocking pool by default.** `std::blocking_pool` exists
  for explicit opt-in (`http::Client` uses it because `ureq` is
  synchronous), but it is not in the h2 server path.
- **No feature gates on standard library modules.** Every module
  in `gossamer-std` is part of the language surface and is
  always compiled. ALPN h2 is always available.

## Race windows considered

| Race | Resolution |
|------|------------|
| Waker fires between `poll()` returning Pending and `park()` running | `goroutine_waker` sets a `woke: AtomicBool` flag *before* calling `unpark`. `drive` checks the flag after Pending and skips park if it's set. |
| Same Waker installed twice for the same direction (concurrent re-poll of the same future) | The stream's `read_waker` / `write_waker` slot overwrites. Latest waker wins; previous is dropped. tokio's docs explicitly bless this. |
| Stream dropped mid-park | The goroutine's `register_waker` callback fires unconditionally on poller events; if the stream is gone, its slot is `None` and the callback is a no-op. |
| Handler panic | `serve_one_stream` wraps the handler in `catch_unwind(AssertUnwindSafe(...))`. On unwind, the stream returns 500 and the parent connection keeps running. |
| Connection shutdown with in-flight streams | `ServerHandle::shutdown(deadline)` sets the shutdown flag, calls `conn.graceful_shutdown()` (which queues a GOAWAY), and spins-on-zero `in_flight` with the deadline. |

## File-by-file

- `crates/gossamer-std/src/runtime_future.rs` — `drive`,
  `goroutine_waker`. ~170 lines incl. tests.
- `crates/gossamer-std/src/async_tcp.rs` — `AsyncTcpStream`,
  `arm_io_wake`. ~210 lines incl. round-trip integration test.
- `crates/gossamer-std/src/http_h2.rs` — `Handler`, `Config`,
  `Error`, `ServerHandle`, `serve_connection`,
  `serve_connection_async`, `serve_one_stream`,
  `bind_and_run_h2c`. ~470 lines incl. 6 unit tests.
- `crates/gossamer-std/src/http.rs` — `server::bind_and_run_tls_h2`
  uses the above to do ALPN-driven HTTP/2 over TLS.

## Wire compatibility notes

- The server advertises `max_concurrent_streams = 100`,
  `initial_window_size = 1 MiB`,
  `initial_connection_window_size = 8 MiB`,
  `max_frame_size = 16 KiB`,
  `max_header_list_size = 16 KiB` by default. All configurable
  via `Config`.
- HTTP/2 trailers from the peer are merged into the request
  headers map before the handler is invoked. Handler-supplied
  trailers on the response side land in v0.4.1.
- Two handler shapes ship in v0.4.0:
  - `Handler` — bounded body. Returns a complete `Response`;
    body sent as one `DATA` frame.
  - `StreamingHandler` — chunked emission. Receives a
    `ResponseWriter` and calls `write_chunk` per frame. The
    response head is sent on the first chunk; `finish` sends
    the terminator (or `Drop` does it on the writer's behalf).

## Tests + CI

- `crates/gossamer-std/src/runtime_future.rs` —
  immediate-ready, external-wake.
- `crates/gossamer-std/src/async_tcp.rs` —
  goroutine-driven loopback round-trip.
- `crates/gossamer-std/src/http_h2.rs` — handler trait impl
  for closures, config defaults, method conversion, server
  handle shutdown semantics, listener-binds-and-drops.

Integration tests against real h2 clients (`curl --http2`,
`h2load`, `h2spec`) live under `crates/gossamer-cli/tests/`
and are gated on the `GOS_H2_LIVE` env var so CI doesn't need
the binaries installed by default.
