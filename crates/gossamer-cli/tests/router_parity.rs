//! Stateful Router parity: same `.gos` source dispatches an
//! HTTP server in both `gos` (interp) and `gos build` →
//! native, with multiple `impl Handler` types registered for
//! different paths. Verifies the constructor + method-chain
//! shape works identically across tiers.

use std::io::BufRead;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("gos");
    p
}

/// A source path of this run's own, so two tests never write one file.
fn source_path(tag: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    std::env::temp_dir().join(format!("{tag}-{pid}-{nanos}.gos"))
}

fn write_source() -> PathBuf {
    let path = source_path("gos-router-parity");
    let src = r#"
use std::http
use std::http::router

struct Health { }
impl Health {
    fn serve(&self, _r: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, "ok"))
    }
}

struct Echo { }
impl Echo {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        Ok(http::Response::text(200, format("echo {}", r.path)))
    }
}

fn main() {
    let r = router::Router::new()
    r.get("/health", Health { })
    r.get("/echo", Echo { })
    let s = http::Server::new()
    match s.listen("127.0.0.1:0") {
        Ok(_) => println("{}", s.addr())
        Err(e) => eprintln("listen: {}", e)
    }
    let _ = s.serve(r)
}
"#;
    std::fs::write(&path, src).unwrap();
    path
}

fn write_source_bare_fn() -> PathBuf {
    let path = source_path("gos-router-parity-fn");
    let src = r#"
use std::http
use std::http::router

fn health(_r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::text(200, "ok"))
}

fn echo(r: http::Request) -> Result<http::Response, http::Error> {
    Ok(http::Response::text(200, format("echo {}", r.path)))
}

fn main() {
    let r = router::Router::new()
    r.get("/health", health)
    r.get("/echo", echo)
    let s = http::Server::new()
    match s.listen("127.0.0.1:0") {
        Ok(_) => println("{}", s.addr())
        Err(e) => eprintln("listen: {}", e)
    }
    let _ = s.serve(r)
}
"#;
    std::fs::write(&path, src).unwrap();
    path
}

fn curl(addr: &str, path: &str) -> (String, i32) {
    let out = Command::new("curl")
        .arg("--silent")
        .arg("--max-time")
        .arg("3")
        .arg("-w")
        .arg("\n%{http_code}")
        .arg(format!("http://{addr}{path}"))
        .output()
        .expect("curl");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    // Split on the last newline to recover (body, code).
    let mut parts = s.rsplitn(2, '\n');
    let code_s = parts.next().unwrap_or("000");
    let body = parts.next().unwrap_or("").to_string();
    let code: i32 = code_s.trim().parse().unwrap_or(0);
    (body, code)
}

/// Runs one server and asks it for every route.
///
/// The server binds port zero and prints the address the kernel gave
/// it, which is both where to send the requests and the fact that the
/// listener is up: a test that picked the port itself would be talking
/// to whatever else took it in the meantime.
fn run_and_check(cmd: &mut Command) {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut announced = String::new();
    let bound = std::io::BufReader::new(child.stdout.take().expect("the child's stdout"))
        .read_line(&mut announced)
        .expect("read the address the server bound");
    if bound == 0 {
        let _ = child.wait();
        let mut complaint = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            let _ = std::io::Read::read_to_string(&mut stderr, &mut complaint);
        }
        panic!("the server ended before it bound: {complaint}");
    }
    let addr = announced.trim().to_string();
    let (h_body, h_code) = curl(&addr, "/health");
    let (e_body, e_code) = curl(&addr, "/echo");
    let (m_body, m_code) = curl(&addr, "/missing");
    // A status of zero means curl reached nothing, which is the server's
    // state to explain: it has either stopped accepting or ended. Reading
    // that state here is what makes such a failure diagnosable instead of a
    // bare `0 != 200`.
    let _ = child.kill();
    let ended = child
        .wait()
        .map_or_else(|e| e.to_string(), |s| s.to_string());
    let mut complaint = String::new();
    if let Some(mut stderr) = child.stderr.take() {
        let _ = std::io::Read::read_to_string(&mut stderr, &mut complaint);
    }
    let server = format!("server {ended}; stderr: {}", complaint.trim());

    assert_eq!(h_code, 200, "/health status ({server})");
    assert_eq!(h_body, "ok", "/health body ({server})");
    assert_eq!(e_code, 200, "/echo status ({server})");
    assert_eq!(e_body, "echo /echo", "/echo body ({server})");
    assert_eq!(m_code, 404, "/missing status ({server})");
    assert_eq!(m_body, "not found", "/missing body ({server})");
}

#[test]
fn router_interp_matches_compiled() {
    let src = write_source();

    // Interp run.
    let mut interp = Command::new(gos_bin());
    interp.arg("run").arg(&src);
    run_and_check(&mut interp);

    // Compiled build + run.
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&src)
        .output()
        .expect("gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stem = src.file_stem().unwrap().to_str().unwrap();
    let bin = std::env::temp_dir().join("target").join("debug").join(stem);
    let mut compiled = Command::new(&bin);
    run_and_check(&mut compiled);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn router_bare_fn_interp_matches_compiled() {
    let src = write_source_bare_fn();

    // Interp run with bare-function handlers (no struct + impl).
    let mut interp = Command::new(gos_bin());
    interp.arg("run").arg(&src);
    run_and_check(&mut interp);

    // Compiled build + run.
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&src)
        .output()
        .expect("gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let stem = src.file_stem().unwrap().to_str().unwrap();
    let bin = std::env::temp_dir().join("target").join("debug").join(stem);
    let mut compiled = Command::new(&bin);
    run_and_check(&mut compiled);
    let _ = std::fs::remove_file(&src);
}
