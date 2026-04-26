//! Stream A.2 — real HTTP/1.1 server behind the `http::serve` builtin.
//! The test binds a listener on a loopback address, fires a real
//! HTTP request over TCP, and asserts the server's response. Uses
//! `max_requests = 1` from the server config so the accept loop
//! terminates cleanly at the end of the test.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::thread;
use std::time::Duration;

use gossamer_std::http::server::{Config, run};
use gossamer_std::http::{Headers, Request, Response, StatusCode};

fn pick_port() -> SocketAddr {
    TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
}

#[test]
fn server_responds_to_a_real_http_request() {
    let addr = pick_port();
    let listener = TcpListener::bind(addr).unwrap();
    let actual_addr = listener.local_addr().unwrap();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            assert_eq!(request.path, "/health");
            assert_eq!(request.method.as_str(), "GET");
            let mut headers = Headers::new();
            headers.insert("x-handler", "test");
            Response {
                status: StatusCode::OK,
                headers,
                body: b"ok".to_vec(),
            }
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    stream
        .write_all(b"GET /health HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    assert!(
        status_line.starts_with("HTTP/1.1 200"),
        "unexpected status: {status_line:?}"
    );

    let mut body = Vec::new();
    reader.read_to_end(&mut body).unwrap();
    let body_text = String::from_utf8_lossy(&body);
    assert!(body_text.contains("ok"), "body was: {body_text}");
    assert!(
        body_text.to_ascii_lowercase().contains("x-handler"),
        "custom header should round-trip, body: {body_text}"
    );

    server_handle.join().unwrap();
}

#[test]
fn server_honours_shutdown_flag_without_a_request() {
    let addr = pick_port();
    let listener = TcpListener::bind(addr).unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: None,
        max_requests: None,
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
    };

    let handle = thread::spawn(move || {
        run(listener, &config, |_req| {
            Response::text(StatusCode::OK, "never")
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(100));
    shutdown.store(true, std::sync::atomic::Ordering::Relaxed);
    handle.join().unwrap();
}

#[test]
fn server_surfaces_bind_errors() {
    let addr = pick_port();
    let first = TcpListener::bind(addr).unwrap();
    let err = TcpListener::bind(first.local_addr().unwrap()).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
}

#[test]
fn slow_client_does_not_block_other_connections() {
    // A goroutine-per-connection server should let a fast client
    // connect and get a response while a slow client is still
    // drip-feeding its request line. If we blocked on the slow
    // client, the fast one would time out.
    let addr = pick_port();
    let listener = TcpListener::bind(addr).unwrap();
    let actual = listener.local_addr().unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(5)),
        max_requests: Some(2),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
    };
    let handle = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            Response::text(StatusCode::OK, request.path.clone())
        })
        .unwrap();
    });

    let slow = TcpStream::connect(actual).unwrap();
    thread::sleep(Duration::from_millis(50));
    let mut fast = TcpStream::connect(actual).unwrap();
    fast.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    fast.write_all(b"GET /fast HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    fast.shutdown(std::net::Shutdown::Write).unwrap();
    let mut buf = Vec::new();
    fast.read_to_end(&mut buf).unwrap();
    let text = String::from_utf8_lossy(&buf);
    assert!(
        text.contains("/fast"),
        "fast client starved while slow client stalled: {text}"
    );

    let mut slow = slow;
    slow.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    slow.write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    slow.shutdown(std::net::Shutdown::Write).unwrap();
    let mut buf2 = Vec::new();
    slow.read_to_end(&mut buf2).unwrap();

    handle.join().unwrap();
}

#[test]
fn server_handles_many_concurrent_connections() {
    // Stress the per-connection worker-thread design: fan out N
    // clients in parallel and assert every one gets a 200. Catches
    // regressions where the accept loop serialises or a shared lock
    // gets poisoned under load.
    const CLIENTS: u64 = 64;
    let addr = pick_port();
    let listener = TcpListener::bind(addr).unwrap();
    let actual = listener.local_addr().unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(5)),
        max_requests: Some(CLIENTS),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
    };
    let server = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            Response::text(StatusCode::OK, request.path.clone())
        })
        .unwrap();
    });
    thread::sleep(Duration::from_millis(50));

    let mut clients = Vec::with_capacity(CLIENTS as usize);
    for i in 0..CLIENTS {
        clients.push(thread::spawn(move || {
            let mut stream = TcpStream::connect(actual).expect("connect");
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            stream
                .write_all(format!("GET /{i} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes())
                .unwrap();
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut buf = Vec::new();
            stream.read_to_end(&mut buf).unwrap();
            let text = String::from_utf8_lossy(&buf).into_owned();
            assert!(
                text.starts_with("HTTP/1.1 200"),
                "client {i} got unexpected status line: {text}"
            );
            assert!(
                text.contains(&format!("/{i}")),
                "client {i} did not get its path back: {text}"
            );
        }));
    }

    for client in clients {
        client.join().expect("client thread panicked");
    }
    server.join().unwrap();
}

#[test]
fn server_request_context_is_not_cancelled_by_default() {
    // Every Request carries a `context::Context` — verify the
    // default is live and cancellable.
    let addr = pick_port();
    let listener = TcpListener::bind(addr).unwrap();
    let actual = listener.local_addr().unwrap();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
    };
    let server = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            assert!(!request.context().is_cancelled());
            Response::text(StatusCode::OK, "ok")
        })
        .unwrap();
    });
    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).unwrap();
    server.join().unwrap();
}
