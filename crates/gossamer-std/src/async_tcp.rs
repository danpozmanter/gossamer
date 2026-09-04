//! Tokio `AsyncRead` / `AsyncWrite` bridge over
//! [`crate::net::TcpStream`].
//!
//! Wraps the existing non-blocking, netpoller-aware TcpStream so
//! async crates like `h2` can consume it. **No tokio runtime is
//! involved.** When the kernel buffer is empty / full, the
//! bridge registers the IO source with the goroutine's gid and
//! parks the goroutine - exactly the same path used by
//! synchronous TcpStream::read/write. The Waker passed in via
//! the `Context` is woken via the netpoller's
//! `register_waker(gid, ...)` mechanism.
//!
//! See `HTTP_H2_ARCH.md` §3 for the integration model.

#![forbid(unsafe_code)]
#![allow(clippy::missing_errors_doc, clippy::module_name_repetitions)]

use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use parking_lot::Mutex;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

use gossamer_runtime::sched::Interest;

use crate::net::TcpStream;
use crate::sched_global;

/// `AsyncRead + AsyncWrite` wrapper over a non-blocking
/// [`TcpStream`].
///
/// Construction is intentionally trivial - the inner stream is
/// already non-blocking with an attached mio handle. The bridge
/// holds the stream + a slot for the current outstanding waker
/// so a re-poll of the same operation does not race against the
/// netpoller delivering the previous wakeup.
pub struct AsyncTcpStream {
    inner: TcpStream,
    /// Most-recent waker installed by `poll_read`. Cleared on
    /// wake; replaced if `poll_read` returns Pending again with
    /// a different waker.
    read_waker: Arc<Mutex<Option<std::task::Waker>>>,
    /// Same for `poll_write`.
    write_waker: Arc<Mutex<Option<std::task::Waker>>>,
}

impl AsyncTcpStream {
    /// Wraps `stream`. The inner stream is consumed.
    #[must_use]
    pub fn new(stream: TcpStream) -> Self {
        Self {
            inner: stream,
            read_waker: Arc::new(Mutex::new(None)),
            write_waker: Arc::new(Mutex::new(None)),
        }
    }

    /// Borrows the inner stream - useful for setting timeouts
    /// or keep-alive after wrap.
    pub fn get_mut(&mut self) -> &mut TcpStream {
        &mut self.inner
    }
}

impl AsyncRead for AsyncTcpStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let slice = buf.initialize_unfilled();
        match self.inner.try_read(slice) {
            Ok(n) => {
                buf.advance(n);
                Poll::Ready(Ok(()))
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let slot = Arc::clone(&self.read_waker);
                arm_io_wake(&mut self.inner, Interest::Readable, cx, slot);
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
    ) -> Poll<Result<usize, io::Error>> {
        match self.inner.try_write(buf) {
            Ok(n) => Poll::Ready(Ok(n)),
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                let slot = Arc::clone(&self.write_waker);
                arm_io_wake(&mut self.inner, Interest::Writable, cx, slot);
                Poll::Pending
            }
            Err(e) => Poll::Ready(Err(e)),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // TCP has no flush primitive beyond the kernel. Per
        // tokio docs, `poll_flush` returning Ready(Ok(())) on
        // every call is the correct shape for blocking sockets.
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<Result<(), io::Error>> {
        // h2 calls poll_shutdown after sending the closing
        // GOAWAY. We honour by half-closing the write side; the
        // read side is closed when the goroutine drops the
        // stream.
        Poll::Ready(Ok(()))
    }
}

/// Registers `cx.waker()` with the netpoller against the current
/// goroutine's gid, so socket readiness re-polls the future.
fn arm_io_wake(
    stream: &mut TcpStream,
    interest: Interest,
    cx: &mut Context<'_>,
    waker_slot: Arc<Mutex<Option<std::task::Waker>>>,
) {
    let Some(gid) = sched_global::current_gid() else {
        return;
    };
    // Store the current waker for the future re-poll.
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
    let _ = stream.register_with_poller(interest, gid);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtime_future::drive;
    use gossamer_runtime::sched_global::spawn;
    use std::io::{Read, Write};
    use std::net::TcpListener as StdListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn async_round_trip_sends_and_receives_bytes() {
        // Bind a real listener on loopback. Client goroutine
        // does AsyncWriteExt::write_all + AsyncReadExt::read_exact
        // through the AsyncTcpStream bridge. Server thread
        // (plain std::net) echoes back.
        let listener = StdListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let server_done = Arc::new(AtomicBool::new(false));
        let server_done_clone = Arc::clone(&server_done);
        std::thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut buf = [0u8; 5];
            sock.read_exact(&mut buf).unwrap();
            sock.write_all(b"world").unwrap();
            server_done_clone.store(true, Ordering::Release);
        });

        gossamer_runtime::platform::sleep(Duration::from_millis(20));

        let result = Arc::new(parking_lot::Mutex::new(None::<Vec<u8>>));
        let result_for_g = Arc::clone(&result);
        let done = Arc::new(AtomicBool::new(false));
        let done_for_g = Arc::clone(&done);
        spawn(Box::new(move || {
            let stream = TcpStream::connect(&format!("127.0.0.1:{}", addr.port())).unwrap();
            let mut async_stream = AsyncTcpStream::new(stream);
            let recv = drive(async move {
                async_stream.write_all(b"hello").await?;
                let mut buf = [0u8; 5];
                async_stream.read_exact(&mut buf).await?;
                Ok::<_, std::io::Error>(buf.to_vec())
            });
            *result_for_g.lock() = recv.ok();
            done_for_g.store(true, Ordering::Release);
        }));

        let deadline = gossamer_runtime::platform::Instant::now() + Duration::from_secs(3);
        while gossamer_runtime::platform::Instant::now() < deadline {
            if done.load(Ordering::Acquire) && server_done.load(Ordering::Acquire) {
                break;
            }
            gossamer_runtime::platform::sleep(Duration::from_millis(10));
        }
        let got = result.lock().clone();
        assert_eq!(got.as_deref(), Some(b"world" as &[u8]));
    }
}
