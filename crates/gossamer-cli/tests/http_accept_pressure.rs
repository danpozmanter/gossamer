//! A momentary descriptor shortage must not end an HTTP server.
//!
//! `accept` answers `EMFILE` while the process is at its descriptor
//! budget, and answers a client again as soon as one closes. A server
//! that treats that as the end of its listener stops serving for the rest
//! of its life over a burst it survived, and reports a clean exit while
//! doing it - every later connection is refused with nothing on stderr.

#![cfg(unix)]
#![allow(missing_docs)]

use std::io::{BufRead, BufReader, Write};
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// How many descriptors the server is given. Low enough that a client
/// burst reaches the limit, high enough for the runtime's own files.
const DESCRIPTOR_BUDGET: usize = 64;

/// Ceiling on the connections held open at once. The burst stops as soon
/// as the server is at its budget, well before this.
const BURST: usize = 128;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("gos");
    p
}

/// A source path of this run's own, so a concurrent test never writes one
/// file.
fn write_source() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let path = std::env::temp_dir().join(format!(
        "gos-accept-pressure-{}-{nanos}.gos",
        std::process::id()
    ));
    std::fs::write(
        &path,
        r#"
use std::http

fn health(_r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::text(200, "ok"))
}

fn main() {
    let s = http::Server::new()
    match s.listen("127.0.0.1:0") {
        Ok(_) => println("{}", s.addr())
        Err(e) => eprintln("listen: {}", e)
    }
    let _ = s.serve(health)
}
"#,
    )
    .unwrap();
    path
}

/// Ends the server whatever exit the test takes, so a failed assertion
/// leaves no listener behind.
struct Server {
    child: Child,
    source: PathBuf,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_file(&self.source);
    }
}

/// The status line of one request, or the error that stopped it.
fn status_line(addr: SocketAddr, path: &str) -> std::io::Result<String> {
    let mut stream = TcpStream::connect_timeout(&addr, Duration::from_secs(3))?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.write_all(
        format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n").as_bytes(),
    )?;
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line)?;
    Ok(line.trim_end().to_string())
}

/// Drives one server through a descriptor shortage and back.
///
/// `shell` is the command that starts it, run under a smaller descriptor
/// budget than this process holds. `ulimit` is how a child is given one:
/// the limit has to be in place before the server starts, which rules out
/// setting it from the test.
fn survives_a_shortage(shell: &str, source: PathBuf) {
    let mut child = Command::new("sh")
        .arg("-c")
        .arg(format!("ulimit -n {DESCRIPTOR_BUDGET}; exec {shell}"))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn the server");

    let mut announced = String::new();
    let bound = BufReader::new(child.stdout.take().expect("the child's stdout"))
        .read_line(&mut announced)
        .expect("read the address the server bound");
    assert!(bound > 0, "the server ended before it bound");
    let addr: SocketAddr = announced.trim().parse().expect("the announced address");
    let server = Server { child, source };

    assert_eq!(
        status_line(addr, "/health").expect("the first request"),
        "HTTP/1.1 200 OK"
    );

    // Hold connections open until the server is at its budget, which is
    // what an unanswered probe says: every descriptor is spoken for, so
    // `accept` has nothing left to hand the next client.
    let mut held = Vec::with_capacity(BURST);
    let mut saturated = false;
    let saturate_by = Instant::now() + Duration::from_secs(30);
    while !saturated && held.len() < BURST && Instant::now() < saturate_by {
        for _ in 0..8 {
            let Ok(stream) = TcpStream::connect_timeout(&addr, Duration::from_millis(250)) else {
                saturated = true;
                break;
            };
            held.push(stream);
        }
        saturated |= status_line(addr, "/health").is_err();
    }
    assert!(
        saturated,
        "the burst never reached the server's descriptor budget"
    );
    drop(held);

    // The descriptors come back as the server's connection threads
    // observe the closes, and the retry that follows the shortage is
    // paced, so the answer is asked for until it arrives rather than
    // once at a moment picked here.
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let refused = match status_line(addr, "/health") {
            Ok(status) if status == "HTTP/1.1 200 OK" => break,
            Ok(status) => status,
            Err(e) => e.to_string(),
        };
        assert!(
            Instant::now() < deadline,
            "the server stopped answering after the descriptor shortage lifted: {refused}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    drop(server);
}

#[test]
fn a_descriptor_shortage_does_not_end_the_interpreted_server() {
    let source = write_source();
    let shell = format!("{} run {}", gos_bin().display(), source.display());
    survives_a_shortage(&shell, source);
}

/// The compiled tier accepts through a loop of its own, so it is asked the
/// same question rather than trusted to share the interpreter's answer.
#[test]
fn a_descriptor_shortage_does_not_end_the_compiled_server() {
    let source = write_source();
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source)
        .output()
        .expect("gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stem = source.file_stem().expect("a stem").to_string_lossy();
    let binary = std::env::temp_dir()
        .join("target")
        .join("debug")
        .join(stem.as_ref());
    let shell = binary.display().to_string();
    survives_a_shortage(&shell, source);
}
