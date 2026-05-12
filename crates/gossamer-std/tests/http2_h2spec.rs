//! h2spec compliance run. Boots the h2c server and points
//! `h2spec` at it. Gated on the `GOS_H2SPEC` env var because
//! the h2spec binary isn't ubiquitous; CI installs it on the
//! HTTP/2 integration runner.
//!
//! Usage:
//!     `GOS_H2SPEC=1 cargo test --test http2_h2spec`
//!     # or: `GOS_H2SPEC=/path/to/h2spec` ...

use std::process::Command;
use std::time::Duration;

use gossamer_std::http::{Headers, Request, Response, StatusCode};
use gossamer_std::http_h2 as h2;

fn h2spec_binary() -> Option<String> {
    if let Ok(p) = std::env::var("GOS_H2SPEC") {
        if p != "1" && !p.is_empty() {
            return Some(p);
        }
    } else {
        return None;
    }
    if Command::new("h2spec").arg("--version").output().is_ok() {
        Some("h2spec".to_string())
    } else {
        None
    }
}

#[test]
fn h2spec_generic_section_passes() {
    let Some(binary) = h2spec_binary() else {
        eprintln!("skipping: set GOS_H2SPEC=1 (with h2spec on PATH) or GOS_H2SPEC=/abs/path");
        return;
    };

    let probe = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = probe.local_addr().unwrap().port();
    drop(probe);
    let addr = format!("127.0.0.1:{port}");

    let server_addr = addr.clone();
    std::thread::spawn(move || {
        let _ = h2::bind_and_run_h2c(
            &server_addr,
            |_req: Request| -> Response {
                Response {
                    status: StatusCode(200),
                    headers: {
                        let mut h = Headers::new();
                        h.insert("content-type", "text/plain");
                        h
                    },
                    body: b"ok".to_vec(),
                }
            },
            h2::Config::default(),
        );
    });

    std::thread::sleep(Duration::from_millis(250));

    let out = Command::new(&binary)
        .arg("-h")
        .arg("127.0.0.1")
        .arg("-p")
        .arg(port.to_string())
        .arg("--strict")
        .arg("generic")
        .output()
        .expect("h2spec failed to launch");

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    eprintln!("--- h2spec stdout ---\n{stdout}");
    eprintln!("--- h2spec stderr ---\n{stderr}");

    // h2spec returns non-zero when any tests fail. Surface
    // the report so CI logs make the failure obvious.
    assert!(
        out.status.success(),
        "h2spec generic section failed; see stdout above"
    );
}
