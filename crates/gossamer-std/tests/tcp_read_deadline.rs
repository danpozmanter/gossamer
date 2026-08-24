//! The socket read deadline bounds a wait on a peer that stays silent.

#![cfg(not(target_arch = "wasm32"))]

use std::io::Write;
use std::net::TcpListener as StdTcpListener;
use std::time::{Duration, Instant};

use gossamer_std::net::TcpStream;

/// A listener that holds its connections in the backlog, so a client
/// that connects to it has a live socket and nothing to read.
fn silent_listener() -> (StdTcpListener, String) {
    let listener = StdTcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let addr = listener.local_addr().expect("local address").to_string();
    (listener, addr)
}

#[test]
fn a_read_deadline_ends_a_wait_on_a_silent_peer() {
    let (_listener, addr) = silent_listener();
    let mut stream = TcpStream::connect(&addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_millis(150)))
        .expect("set read deadline");

    let started = Instant::now();
    let mut buf = [0u8; 16];
    let outcome = stream.read(&mut buf);
    let waited = started.elapsed();

    let err = outcome.expect_err("a silent peer ends at the deadline");
    assert!(
        err.to_string().contains("read timed out"),
        "the report names the wait that ended: {err}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "the wait ended at the deadline, not at the kernel's own: {waited:?}"
    );
}

#[test]
fn a_duplicate_waits_on_the_same_deadline() {
    let (_listener, addr) = silent_listener();
    let original = TcpStream::connect(&addr).expect("connect");
    let mut duplicate = original.try_clone().expect("duplicate the socket");
    original
        .set_read_timeout(Some(Duration::from_millis(150)))
        .expect("set read deadline");

    let mut buf = [0u8; 16];
    let err = duplicate
        .read(&mut buf)
        .expect_err("the duplicate reads the same socket on the same terms");
    assert!(
        err.to_string().contains("read timed out"),
        "the report names the wait that ended: {err}"
    );
}

#[test]
fn a_read_inside_its_deadline_ends_when_the_data_arrives() {
    let (listener, addr) = silent_listener();
    let answering = std::thread::spawn(move || {
        let (mut peer, _) = listener.accept().expect("accept");
        std::thread::sleep(Duration::from_millis(100));
        peer.write_all(b"pong").expect("answer the client");
        // The client reads before the peer's socket is dropped.
        std::thread::sleep(Duration::from_millis(500));
    });
    let mut stream = TcpStream::connect(&addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set read deadline");

    let started = Instant::now();
    let mut buf = [0u8; 16];
    let read = stream.read(&mut buf).expect("the answer arrives");
    let waited = started.elapsed();

    assert_eq!(&buf[..read], b"pong");
    assert!(
        waited < Duration::from_secs(2),
        "a deadline bounds a wait rather than lengthening it: {waited:?}"
    );
    answering.join().expect("the peer finishes");
}

#[test]
fn a_cleared_deadline_leaves_a_read_to_its_data() {
    let (listener, addr) = silent_listener();
    let mut stream = TcpStream::connect(&addr).expect("connect");
    stream
        .set_read_timeout(Some(Duration::from_millis(50)))
        .expect("set read deadline");
    stream.set_read_timeout(None).expect("clear read deadline");

    let (mut peer, _) = listener.accept().expect("accept");
    peer.write_all(b"pong").expect("answer the client");

    let mut buf = [0u8; 16];
    let read = stream.read(&mut buf).expect("the answer arrives");
    assert_eq!(&buf[..read], b"pong");
}
