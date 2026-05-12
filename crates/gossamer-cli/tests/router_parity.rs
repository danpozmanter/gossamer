//! Stateful Router parity: same `.gos` source dispatches an
//! HTTP server in both `gos run` (interp) and `gos build` →
//! native, with multiple `impl Handler` types registered for
//! different paths. Verifies the constructor + method-chain
//! shape works identically across tiers.

use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("gos");
    p
}

fn write_source(port: u16) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let path = std::env::temp_dir().join(format!("gos-router-parity-{pid}-{nanos}.gos"));
    let src = format!(
        r#"
use std::http
use std::http::router

struct Health {{ }}
impl Health {{
    fn serve(&self, _r: http::Request) -> Result<http::Response, http::Error> {{
        Ok(http::Response::text(200, "ok"))
    }}
}}

struct Echo {{ }}
impl Echo {{
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {{
        Ok(http::Response::text(200, &format!("echo {{}}", r.path)))
    }}
}}

fn main() {{
    let r = router::Router::new()
    r.get("/health", Health {{ }})
    r.get("/echo", Echo {{ }})
    let _ = http::serve("127.0.0.1:{port}", r)
}}
"#,
    );
    std::fs::write(&path, src).unwrap();
    path
}

fn write_source_bare_fn(port: u16) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let path = std::env::temp_dir().join(format!("gos-router-parity-fn-{pid}-{nanos}.gos"));
    let src = format!(
        r#"
use std::http
use std::http::router

fn health(_r: http::Request) -> Result<http::Response, http::Error> {{
    Ok(http::Response::text(200, "ok"))
}}

fn echo(r: http::Request) -> Result<http::Response, http::Error> {{
    Ok(http::Response::text(200, &format!("echo {{}}", r.path)))
}}

fn main() {{
    let r = router::Router::new()
    r.get("/health", health)
    r.get("/echo", echo)
    let _ = http::serve("127.0.0.1:{port}", r)
}}
"#,
    );
    std::fs::write(&path, src).unwrap();
    path
}

fn free_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let p = l.local_addr().unwrap().port();
    drop(l);
    p
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

fn run_and_check(cmd: &mut Command, port: u16) {
    let mut child = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn");
    std::thread::sleep(Duration::from_millis(800));
    let addr = format!("127.0.0.1:{port}");
    let (h_body, h_code) = curl(&addr, "/health");
    let (e_body, e_code) = curl(&addr, "/echo");
    let (m_body, m_code) = curl(&addr, "/missing");
    let _ = child.kill();
    let _ = child.wait();

    assert_eq!(h_code, 200, "/health status");
    assert_eq!(h_body, "ok", "/health body");
    assert_eq!(e_code, 200, "/echo status");
    assert_eq!(e_body, "echo /echo", "/echo body");
    assert_eq!(m_code, 404, "/missing status");
    assert_eq!(m_body, "not found", "/missing body");
}

#[test]
fn router_interp_matches_compiled() {
    let port = free_port();
    let src = write_source(port);

    // Interp run.
    let mut interp = Command::new(gos_bin());
    interp.arg("run").arg(&src);
    run_and_check(&mut interp, port);

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
    run_and_check(&mut compiled, port);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn router_bare_fn_interp_matches_compiled() {
    let port = free_port();
    let src = write_source_bare_fn(port);

    // Interp run with bare-function handlers (no struct + impl).
    let mut interp = Command::new(gos_bin());
    interp.arg("run").arg(&src);
    run_and_check(&mut interp, port);

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
    run_and_check(&mut compiled, port);
    let _ = std::fs::remove_file(&src);
}
