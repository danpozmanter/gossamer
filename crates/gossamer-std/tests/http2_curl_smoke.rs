//! End-to-end smoke test: boot the h2c server, hit it with
//! `curl --http2-prior-knowledge`, verify the response. Gated on
//! the `GOS_H2_LIVE` env var so CI without a network-stack curl
//! installed can skip cleanly.

use std::process::Command;
use std::time::Duration;

use gossamer_std::http::{Headers, Request, Response, StatusCode};
use gossamer_std::http_h2 as h2;

#[derive(Clone)]
struct StreamHandler;

impl h2::StreamingHandler for StreamHandler {
    fn serve(&self, _req: Request, mut w: h2::ResponseWriter) -> Result<(), h2::Error> {
        w.set_status(200);
        w.header("content-type", "text/plain");
        for n in 0..3 {
            w.write_chunk(format!("chunk-{n}\n").as_bytes())?;
        }
        w.finish()
    }
}

const STREAM_HANDLER: StreamHandler = StreamHandler;

fn curl_available_with_http2() -> bool {
    Command::new("curl")
        .arg("--version")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("nghttp2"))
}

#[test]
fn bounded_handler_responds_over_h2c() {
    if std::env::var("GOS_H2_LIVE").is_err() {
        eprintln!("skipping: set GOS_H2_LIVE=1 to run");
        return;
    }
    if !curl_available_with_http2() {
        eprintln!("skipping: curl missing HTTP/2 support");
        return;
    }

    // Find a free port.
    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let addr = format!("127.0.0.1:{port}");

    // Boot the server in a thread; will run forever until process exit.
    let server_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = h2::bind_and_run_h2c(
            &server_addr,
            |req: Request| -> Response {
                Response {
                    status: StatusCode(200),
                    headers: {
                        let mut h = Headers::new();
                        h.insert("content-type", "text/plain");
                        h
                    },
                    body: format!("h2: {} {}", req.method.as_str(), req.path).into_bytes(),
                }
            },
            h2::Config::default(),
        );
    });

    // Give the listener a beat to come up.
    std::thread::sleep(Duration::from_millis(150));

    let out = Command::new("curl")
        .arg("--http2-prior-knowledge")
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("5")
        .arg(format!("http://{addr}/ping"))
        .output()
        .expect("curl failed");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "curl exited non-zero: {out:?}");
    assert!(body.contains("h2: GET /ping"), "unexpected body: {body:?}");
}

#[test]
fn streaming_handler_emits_multiple_chunks_over_h2c() {
    if std::env::var("GOS_H2_LIVE").is_err() {
        eprintln!("skipping: set GOS_H2_LIVE=1 to run");
        return;
    }
    if !curl_available_with_http2() {
        eprintln!("skipping: curl missing HTTP/2 support");
        return;
    }

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let addr = format!("127.0.0.1:{port}");

    let server_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = h2::bind_and_run_h2c_streaming(&server_addr, STREAM_HANDLER, h2::Config::default());
    });

    std::thread::sleep(Duration::from_millis(150));

    let out = Command::new("curl")
        .arg("--http2-prior-knowledge")
        .arg("--silent")
        .arg("--show-error")
        .arg("--max-time")
        .arg("5")
        .arg(format!("http://{addr}/stream"))
        .output()
        .expect("curl failed");
    let body = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "curl exited non-zero: {out:?}");
    assert!(body.contains("chunk-0"), "missing chunk-0: {body:?}");
    assert!(body.contains("chunk-1"), "missing chunk-1: {body:?}");
    assert!(body.contains("chunk-2"), "missing chunk-2: {body:?}");
}
