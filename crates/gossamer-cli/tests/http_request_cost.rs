//! CPU cost per request for the release-tier HTTP server.
//!
//! The floor a handler cannot go below is what the server itself spends:
//! accept, parse, route, dispatch, and write. This measures it against a
//! constant handler over keep-alive connections and holds it to a ceiling, so
//! a change that adds per-request work to the server path is caught here
//! rather than in a downstream benchmark.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command};
use std::time::{Duration, Instant};

const SOURCE: &str = r#"
use std::{env, http}
use std::http::router

fn hello(_r: http::Request) -> Result<http::Response, errors::Error> {
    Ok(http::Response::json(200, "{\"ok\":true}"))
}

let port = env::args()[0].to_i64().unwrap_or(18777)
let rt = router::Router::new().get("/x", hello)
println("listening")
http::serve("127.0.0.1:" + port.to_string(), rt)?
"#;

fn build_release(name: &str, source: &str) -> PathBuf {
    let dir = env::temp_dir().join(format!("gos-http-cost-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir).expect("create fixture directory");
    let source_path = dir.join(format!("{name}.gos"));
    fs::write(&source_path, source).expect("write fixture");
    let output = Command::new(env!("CARGO_BIN_EXE_gos"))
        .args(["build", "--release", "--out-dir"])
        .arg(&dir)
        .arg(&source_path)
        .output()
        .expect("run gos build --release");
    assert!(
        output.status.success(),
        "release build failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    dir.join(name)
}

/// `utime + stime` of `pid` in clock ticks, read from `/proc/<pid>/stat`.
///
/// Field 14 and 15 follow the executable name, which may itself contain
/// spaces and parentheses, so the scan starts after the last `)`.
#[cfg(target_os = "linux")]
fn cpu_ticks(pid: u32) -> u64 {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).expect("read /proc/<pid>/stat");
    let tail = &stat[stat.rfind(')').expect("stat comm field") + 1..];
    let fields: Vec<&str> = tail.split_whitespace().collect();
    // `tail` starts at field 3 (state), so utime is index 11 and stime 12.
    let utime: u64 = fields[11].parse().expect("utime");
    let stime: u64 = fields[12].parse().expect("stime");
    utime + stime
}

fn wait_ready(port: u16) -> bool {
    for _ in 0..200 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return true;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    false
}

/// Drives `count` keep-alive GETs down one connection, returning when every
/// response has been read.
fn drive(port: u16, count: usize) {
    let stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to server");
    stream.set_nodelay(true).expect("nodelay");
    let mut writer = stream.try_clone().expect("clone stream");
    let mut reader = BufReader::new(stream);
    let request = b"GET /x HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: keep-alive\r\n\r\n";
    let mut line = String::new();
    for _ in 0..count {
        writer.write_all(request).expect("write request");
        writer.flush().expect("flush request");
        let mut content_length: Option<usize> = None;
        loop {
            line.clear();
            let n = reader.read_line(&mut line).expect("read header line");
            assert!(n > 0, "server closed the connection mid-response");
            let trimmed = line.trim_end();
            if trimmed.is_empty() {
                break;
            }
            if let Some(value) = trimmed
                .strip_prefix("Content-Length:")
                .or_else(|| trimmed.strip_prefix("content-length:"))
            {
                content_length = Some(value.trim().parse().expect("content length"));
            }
        }
        let body_len = content_length.expect("server must send Content-Length");
        let mut body = vec![0u8; body_len];
        std::io::Read::read_exact(&mut reader, &mut body).expect("read body");
    }
}

struct ServerGuard(Child);

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(target_os = "linux")]
#[test]
fn release_http_server_holds_its_per_request_cpu_ceiling() {
    const PORT: u16 = 23996;
    const WORKERS: usize = 8;
    const PER_WORKER: usize = 4_000;
    const TOTAL: usize = WORKERS * PER_WORKER;

    let binary = build_release("httpbase", SOURCE);
    let child = Command::new(&binary)
        .arg(PORT.to_string())
        .spawn()
        .expect("start the server");
    let server = ServerGuard(child);
    let pid = server.0.id();
    assert!(wait_ready(PORT), "server never accepted a connection");

    // One warm pass so the measured window excludes first-connection setup.
    drive(PORT, 200);

    let before = cpu_ticks(pid);
    let start = Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..WORKERS {
            scope.spawn(|| drive(PORT, PER_WORKER));
        }
    });
    let wall = start.elapsed();
    let after = cpu_ticks(pid);

    // `/proc` reports CPU in clock ticks; every Linux target this runs on
    // uses 100 per second.
    let ticks = after - before;
    let cpu_us_per_request = (ticks as f64 * 10_000.0) / TOTAL as f64;
    println!(
        "http constant handler: {cpu_us_per_request:.2} us cpu/request over {TOTAL} requests in {wall:?}"
    );
    // Measured at 4.4 us on a 24-core Linux box; the ceiling leaves room for
    // a loaded runner while still catching a return to the 6.6 us floor the
    // per-request peer-watch registration, the eager request context, and the
    // per-response HTTP-date rendering used to cost.
    assert!(
        cpu_us_per_request <= 6.0,
        "HTTP server per-request CPU regressed: {cpu_us_per_request:.2} us > 6.00 us"
    );
}
