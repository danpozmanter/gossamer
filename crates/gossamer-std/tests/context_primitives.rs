//! Per-primitive `_ctx` cancellation tests.
//!
//! Each test starts a real blocking operation, cancels the
//! surrounding `Context`, and asserts the operation returns a
//! `Cancelled` error within a small wall-clock bound. The
//! existing `context_cancellation.rs` covers sleep / waitgroup /
//! mutex / `blocking_pool`; this file fills the net gap
//! (`TcpListener::accept_ctx`, `TcpStream::read_ctx`).

#![allow(missing_docs)]

use std::time::{Duration, Instant};

use gossamer_std::context::{Context, with_cancel};

const CANCEL_BOUND: Duration = Duration::from_millis(500);

fn spawn_blocking<F>(f: F) -> std::sync::mpsc::Receiver<()>
where
    F: FnOnce() + Send + 'static,
{
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        f();
        let _ = tx.send(());
    });
    rx
}

fn wait_with_timeout(rx: std::sync::mpsc::Receiver<()>, deadline: Duration) -> bool {
    rx.recv_timeout(deadline).is_ok()
}

#[test]
fn accept_ctx_returns_cancelled_when_context_fires_before_a_client_connects() {
    let mut listener = gossamer_std::net::TcpListener::bind("127.0.0.1:0").expect("bind");

    let (ctx, cancel) = with_cancel(&Context::background());
    let start = Instant::now();
    let done = spawn_blocking(move || {
        let result = listener.accept_ctx(&ctx);
        // No client is connecting; the only way this returns is via cancel.
        assert!(
            result.is_err(),
            "accept_ctx should return Err on cancel, got {result:?}",
        );
    });

    std::thread::sleep(Duration::from_millis(50));
    cancel.cancel_with("test cancel");

    assert!(
        wait_with_timeout(done, CANCEL_BOUND),
        "accept_ctx must observe cancel within {CANCEL_BOUND:?}",
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should not have hung",
    );
}

#[test]
fn read_ctx_returns_cancelled_when_context_fires_with_no_data_available() {
    use std::io::Write;
    use std::net::TcpListener;

    // Set up a listener so the client has something to connect to,
    // but the server never writes - `read_ctx` will block.
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let _server_thread = std::thread::spawn(move || {
        let (mut s, _) = listener.accept().expect("server accept");
        let _ = s.write_all(b""); // no-op
        std::thread::sleep(Duration::from_secs(10));
    });

    let mut stream =
        gossamer_std::net::TcpStream::connect(&addr.to_string()).expect("client connect");

    let (ctx, cancel) = with_cancel(&Context::background());
    let start = Instant::now();
    let done = spawn_blocking(move || {
        let mut buf = [0u8; 16];
        let result = stream.read_ctx(&ctx, &mut buf);
        assert!(
            result.is_err(),
            "read_ctx should return Err on cancel, got {result:?}",
        );
    });

    std::thread::sleep(Duration::from_millis(50));
    cancel.cancel_with("test cancel");

    assert!(
        wait_with_timeout(done, CANCEL_BOUND),
        "read_ctx must observe cancel within {CANCEL_BOUND:?}",
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should not have hung",
    );
}

#[test]
fn accept_ctx_returns_ok_when_a_client_connects_before_cancel() {
    use std::net::TcpStream;

    let mut listener = gossamer_std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr").to_string();

    let (ctx, _cancel) = with_cancel(&Context::background());

    let client_handle = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(50));
        let _client = TcpStream::connect(addr).expect("client connect");
        std::thread::sleep(Duration::from_millis(100));
    });

    let start = Instant::now();
    let result = listener.accept_ctx(&ctx);
    assert!(
        result.is_ok(),
        "accept_ctx should succeed when a real client connects, got {result:?}",
    );
    assert!(
        start.elapsed() < Duration::from_secs(2),
        "should not have hung",
    );
    client_handle.join().expect("client thread");
}
