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
use gossamer_std::http::{Headers, Request, Response, Server, StatusCode};

/// Binds a fresh loopback listener and hands back the listener
/// plus the address it ended up on. Returning both avoids the
/// classic bind-twice race: prior code dropped the listener
/// before re-binding the same port, which is reliably exploited
/// by parallel test workers (and by Windows CI agents whose
/// ephemeral-port allocator recycles aggressively).
fn bind_loopback() -> (TcpListener, SocketAddr) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    (listener, addr)
}

#[test]
fn server_responds_to_a_real_http_request() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
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
    let lower = body_text.to_ascii_lowercase();
    assert!(body_text.contains("ok"), "body was: {body_text}");
    assert!(
        lower.contains("x-handler"),
        "custom header should round-trip, body: {body_text}"
    );

    // ABI 0.4: every response carries auto-inserted Date + Server
    // headers (RFC 9110 §6.6.1).
    assert!(
        lower.contains("date:"),
        "response must carry an auto-inserted Date header, body: {body_text}"
    );
    assert!(
        lower.contains("server: gossamer-test"),
        "response must carry the configured Server header, body: {body_text}"
    );

    server_handle.join().unwrap();
}

#[test]
fn server_skips_date_and_server_when_handler_set_them() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |_request: Request| {
            let mut headers = Headers::new();
            headers.insert("date", "Wed, 21 Oct 2015 07:28:00 GMT");
            headers.insert("server", "custom-stack/2.0");
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
        .write_all(b"GET /x HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut all = Vec::new();
    reader.read_to_end(&mut all).unwrap();
    let lower = String::from_utf8_lossy(&all).to_ascii_lowercase();
    assert!(
        lower.contains("date: wed, 21 oct 2015 07:28:00 gmt"),
        "handler-set Date must be preserved, body: {}",
        String::from_utf8_lossy(&all)
    );
    assert!(
        lower.contains("server: custom-stack/2.0"),
        "handler-set Server must be preserved, body: {}",
        String::from_utf8_lossy(&all)
    );
    // And no duplicate Date / Server.
    assert_eq!(
        lower.matches("date:").count(),
        1,
        "must not duplicate Date when handler set one"
    );
    assert_eq!(
        lower.matches("server:").count(),
        1,
        "must not duplicate Server when handler set one"
    );

    server_handle.join().unwrap();
}

#[test]
fn server_splits_path_and_query() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            assert_eq!(request.path, "/search");
            assert_eq!(request.query, "q=hello+world&page=2");
            let pairs = request.query_pairs();
            assert_eq!(
                pairs,
                vec![
                    ("q".to_string(), "hello world".to_string()),
                    ("page".to_string(), "2".to_string()),
                ]
            );
            assert_eq!(request.request_uri(), "/search?q=hello+world&page=2");
            Response {
                status: StatusCode::OK,
                headers: Headers::new(),
                body: b"ok".to_vec(),
            }
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    stream
        .write_all(b"GET /search?q=hello+world&page=2 HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    assert!(status_line.starts_with("HTTP/1.1 200"));

    server_handle.join().unwrap();
}

#[test]
fn server_aborts_slowloris_at_header_timeout() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(5)),
        // Tight header_timeout — drip-feed must trip it.
        read_header_timeout: Some(Duration::from_millis(300)),
        read_body_timeout: Some(Duration::from_secs(5)),
        write_timeout: Some(Duration::from_secs(5)),
        idle_timeout: Some(Duration::from_mins(1)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        server_name: None,
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        let _ = run(listener, &config, |_request: Request| {
            panic!("handler must not run on slowloris");
        });
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    // Drip-feed one byte per 150ms; the total head time will exceed
    // the 300ms header_timeout long before the head completes.
    let req = b"GET /slow HTTP/1.1\r\nHost: localhost\r\nX-Slow: aaaaaaaaaaaaa\r\n\r\n";
    let start = std::time::Instant::now();
    for byte in req {
        if stream.write_all(&[*byte]).is_err() {
            break;
        }
        if stream.flush().is_err() {
            break;
        }
        thread::sleep(Duration::from_millis(150));
        if start.elapsed() > Duration::from_secs(3) {
            break;
        }
    }
    // Send FIN so the server goroutine gets EOF from read_line
    // immediately rather than waiting for SO_RCVTIMEO (5 s). On
    // macOS under constrained CI the socket timeout fires late,
    // causing the server to hold the connection open for the full
    // 5 s and the test to stall at stream.read() below.
    let _ = stream.shutdown(std::net::Shutdown::Write);
    // Server should now close — wait up to 1 s for the RST/EOF.
    let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
    let mut buf = [0u8; 16];
    let _ = stream.read(&mut buf);

    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = TcpStream::connect(actual_addr);
    server_handle.join().unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(8),
        "slowloris connection must be dropped before read_timeout: {:?}",
        start.elapsed()
    );
}

#[test]
fn server_decodes_chunked_request_body() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: None,
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            assert_eq!(request.body, b"hello world");
            assert_eq!(
                request
                    .headers
                    .get("trailer-key")
                    .map(std::string::ToString::to_string),
                Some("trailer-val".to_string()),
                "trailer headers should merge into request.headers"
            );
            Response {
                status: StatusCode::OK,
                headers: Headers::new(),
                body: b"ok".to_vec(),
            }
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    let req = b"POST /echo HTTP/1.1\r\n\
                Host: localhost\r\n\
                Transfer-Encoding: chunked\r\n\
                Trailer: Trailer-Key\r\n\r\n\
                5\r\nhello\r\n\
                1\r\n \r\n\
                5\r\nworld\r\n\
                0\r\nTrailer-Key: trailer-val\r\n\r\n";
    stream.write_all(req).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    assert!(
        status_line.starts_with("HTTP/1.1 200"),
        "unexpected status: {status_line:?}"
    );

    server_handle.join().unwrap();
}

#[test]
fn server_rejects_chunked_with_content_length() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let shutdown_for_config = Arc::clone(&shutdown);
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: shutdown_for_config,
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: None,
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        // Handler should never be invoked — request is malformed.
        let result = run(listener, &config, |_request: Request| {
            panic!("handler must not be invoked on malformed request");
        });
        // The accept loop returns Ok even when a connection
        // dispatch errors at the parser level; we don't assert on
        // its result.
        let _ = result;
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    let req = b"POST /x HTTP/1.1\r\n\
                Host: localhost\r\n\
                Transfer-Encoding: chunked\r\n\
                Content-Length: 5\r\n\r\n\
                5\r\nhello\r\n0\r\n\r\n";
    stream.write_all(req).unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    // Wait long enough for the server to error out on parse.
    thread::sleep(Duration::from_millis(100));
    shutdown.store(true, std::sync::atomic::Ordering::SeqCst);
    let _ = TcpStream::connect(actual_addr);
    server_handle.join().unwrap();
}

#[test]
fn server_writes_100_continue_before_reading_expect_body() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: None,
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |request: Request| {
            assert_eq!(request.body, b"continued body");
            Response {
                status: StatusCode::OK,
                headers: Headers::new(),
                body: b"received".to_vec(),
            }
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(50));
    let stream = TcpStream::connect(actual_addr).unwrap();
    let mut write_half = stream.try_clone().unwrap();

    // Send the head with Expect: 100-continue, NOT the body yet.
    write_half
        .write_all(
            b"POST /upload HTTP/1.1\r\n\
              Host: localhost\r\n\
              Content-Length: 14\r\n\
              Expect: 100-continue\r\n\r\n",
        )
        .unwrap();
    write_half.flush().unwrap();

    // Read the interim 100 Continue response.
    let mut reader = BufReader::new(stream);
    let mut interim = String::new();
    reader.read_line(&mut interim).unwrap();
    assert!(
        interim.starts_with("HTTP/1.1 100"),
        "expected 100 Continue, got: {interim:?}"
    );
    // Consume the blank line that terminates the interim header block.
    let mut blank = String::new();
    reader.read_line(&mut blank).unwrap();
    assert_eq!(blank, "\r\n", "expected blank line, got: {blank:?}");

    // Now send the body.
    write_half.write_all(b"continued body").unwrap();
    write_half.shutdown(std::net::Shutdown::Write).unwrap();

    // Read the final 200 response.
    let mut status = String::new();
    reader.read_line(&mut status).unwrap();
    assert!(
        status.starts_with("HTTP/1.1 200"),
        "expected 200 OK, got: {status:?}"
    );

    server_handle.join().unwrap();
}

#[test]
fn server_emits_chunked_response_when_handler_requests() {
    let (listener, actual_addr) = bind_loopback();

    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: None,
        ..Config::default()
    };

    let server_handle = thread::spawn(move || {
        run(listener, &config, |_request: Request| {
            let mut headers = Headers::new();
            headers.insert("transfer-encoding", "chunked");
            Response {
                status: StatusCode::OK,
                headers,
                body: b"hello chunked world".to_vec(),
            }
        })
        .unwrap();
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut all = Vec::new();
    stream.read_to_end(&mut all).unwrap();
    let s = String::from_utf8_lossy(&all);
    let lower = s.to_ascii_lowercase();
    assert!(
        lower.contains("transfer-encoding: chunked"),
        "response missing chunked header: {s}"
    );
    assert!(
        !lower.contains("content-length:"),
        "chunked response must not carry content-length: {s}"
    );
    // Look for the literal hex-prefixed chunk + zero terminator.
    let body_idx = s.find("\r\n\r\n").expect("header/body separator");
    let body = &s[body_idx + 4..];
    // The body should be `13\r\nhello chunked world\r\n0\r\n\r\n`
    // (0x13 = 19 bytes).
    assert!(
        body.starts_with("13\r\nhello chunked world\r\n"),
        "body: {body:?}"
    );
    assert!(body.ends_with("0\r\n\r\n"), "body terminator: {body:?}");

    server_handle.join().unwrap();
}

#[test]
fn graceful_shutdown_drains_in_flight_handler() {
    let (listener, actual_addr) = bind_loopback();
    let config = Config::default();
    let config_for_server = config.clone();
    let shutdown_for_test = Arc::clone(&config.shutdown);

    let (handler_started_tx, handler_started_rx) = std::sync::mpsc::channel::<()>();
    let (release_handler_tx, release_handler_rx) = std::sync::mpsc::channel::<()>();
    let handler_started_tx = std::sync::Mutex::new(Some(handler_started_tx));
    let release_handler_rx = std::sync::Mutex::new(Some(release_handler_rx));

    let server_handle = thread::spawn(move || {
        let _ = run(listener, &config_for_server, |_request: Request| {
            // Signal start, then wait for the test to flip
            // shutdown + tell us we may finish.
            if let Some(tx) = handler_started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            if let Some(rx) = release_handler_rx.lock().unwrap().take() {
                let _ = rx.recv_timeout(Duration::from_secs(2));
            }
            Response {
                status: StatusCode::OK,
                headers: Headers::new(),
                body: b"done".to_vec(),
            }
        });
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();

    // Wait for handler to start.
    handler_started_rx
        .recv_timeout(Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        config.in_flight.load(std::sync::atomic::Ordering::Acquire),
        1,
        "handler should be in-flight"
    );

    // Kick off shutdown in a background thread (it blocks).
    let cfg_for_shutdown = config.clone();
    let shutdown_join =
        thread::spawn(move || Server::shutdown(&cfg_for_shutdown, Some(Duration::from_secs(2))));

    // Release the handler so it finishes.
    thread::sleep(Duration::from_millis(50));
    release_handler_tx.send(()).unwrap();

    let drained = shutdown_join.join().unwrap();
    assert!(drained, "shutdown should drain cleanly");
    assert_eq!(
        config.in_flight.load(std::sync::atomic::Ordering::Acquire),
        0,
        "in-flight should reach zero before shutdown returns"
    );

    // Wake the acceptor and let the server exit.
    let _ = TcpStream::connect(actual_addr);
    shutdown_for_test.store(true, std::sync::atomic::Ordering::Release);
    server_handle.join().unwrap();
}

#[test]
fn handler_request_context_cancels_on_shutdown() {
    let (listener, actual_addr) = bind_loopback();
    let config = Config::default();
    let config_for_server = config.clone();

    let observed = Arc::new(std::sync::Mutex::new(false));
    let observed_for_handler = Arc::clone(&observed);
    let handler_started = Arc::new(std::sync::Mutex::new(
        Some(std::sync::mpsc::channel::<()>()),
    ));
    let handler_started_for_handler = Arc::clone(&handler_started);
    let (started_tx, started_rx) = std::sync::mpsc::channel::<()>();
    let started_tx = std::sync::Mutex::new(Some(started_tx));

    let server_handle = thread::spawn(move || {
        let _ = run(listener, &config_for_server, |request: Request| {
            let _ = handler_started_for_handler.lock().unwrap().take();
            if let Some(tx) = started_tx.lock().unwrap().take() {
                let _ = tx.send(());
            }
            // Poll for cancellation up to 2 seconds.
            let deadline = std::time::Instant::now() + Duration::from_secs(2);
            while std::time::Instant::now() < deadline {
                if request.context().is_cancelled() {
                    *observed_for_handler.lock().unwrap() = true;
                    return Response {
                        status: StatusCode::OK,
                        headers: Headers::new(),
                        body: b"cancelled".to_vec(),
                    };
                }
                thread::sleep(Duration::from_millis(10));
            }
            Response {
                status: StatusCode::OK,
                headers: Headers::new(),
                body: b"timed-out".to_vec(),
            }
        });
    });

    thread::sleep(Duration::from_millis(50));
    let mut stream = TcpStream::connect(actual_addr).unwrap();
    stream
        .write_all(b"GET /slow HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.flush().unwrap();

    started_rx.recv_timeout(Duration::from_secs(2)).unwrap();
    // Trip shutdown; the watcher in the worker should fire
    // cancel into the handler's context.
    config
        .shutdown
        .store(true, std::sync::atomic::Ordering::Release);

    // Drain the server's response then verify handler observed cancel.
    let mut buf = Vec::new();
    let _ = stream.read_to_end(&mut buf);
    let _ = TcpStream::connect(actual_addr);
    server_handle.join().unwrap();

    assert!(
        *observed.lock().unwrap(),
        "handler should observe context cancellation on shutdown"
    );
    drop(handler_started);
}

#[test]
fn server_honours_shutdown_flag_without_a_request() {
    let (listener, _addr) = bind_loopback();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: None,
        max_requests: None,
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
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
    let (first, addr) = bind_loopback();
    let err = TcpListener::bind(addr).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AddrInUse);
    drop(first);
}

#[test]
fn slow_client_does_not_block_other_connections() {
    // A goroutine-per-connection server should let a fast client
    // connect and get a response while a slow client is still
    // drip-feeding its request line. If we blocked on the slow
    // client, the fast one would time out.
    let (listener, actual) = bind_loopback();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(5)),
        max_requests: Some(2),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
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
    let (listener, actual) = bind_loopback();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(5)),
        max_requests: Some(CLIENTS),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
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
    let (listener, actual) = bind_loopback();
    let shutdown = Arc::new(AtomicBool::new(false));
    let config = Config {
        read_timeout: Some(Duration::from_secs(2)),
        max_requests: Some(1),
        shutdown: Arc::clone(&shutdown),
        max_header_bytes: 8 * 1024,
        max_body_bytes: 1024 * 1024,
        server_name: Some("gossamer-test".to_string()),
        ..Config::default()
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
