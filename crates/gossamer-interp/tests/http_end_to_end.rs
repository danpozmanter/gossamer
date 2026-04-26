//! Stream A.2 — end-to-end test that a Gossamer handler is actually
//! dispatched when a real HTTP request lands on `http::serve`.
//! The test drives the interpreter on a small source program. It
//! picks a free port, launches the server in a background thread via
//! `GOSSAMER_HTTP_MAX_REQUESTS=1`, fires a real HTTP GET, and asserts
//! that the handler closure was invoked by inspecting the response.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use gossamer_hir::lower_source_file;
use gossamer_interp::Interpreter;
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

fn run_interp(source: &str) -> Result<(), String> {
    let mut map = SourceMap::new();
    let file = map.add_file("server.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    if !parse_diags.is_empty() {
        return Err(format!("parse: {parse_diags:?}"));
    }
    let (resolutions, _) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut interp = Interpreter::new();
    interp.load(&program);
    interp
        .call("main", Vec::new())
        .map(|_| ())
        .map_err(|e| format!("runtime: {e}"))
}

#[test]
fn native_http_serve_dispatches_user_handler() {
    // The interpreter's `http::serve` exits after
    // `gossamer_interp::set_http_max_requests(n)`; without it the
    // server loops forever and the test's `server_thread.join()`
    // deadlocks.
    gossamer_interp::set_http_max_requests(1);

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);

    let source = format!(
        "struct App {{ }}\n\
         impl App {{\n\
             fn new() -> App {{ App {{ }} }}\n\
         }}\n\
         impl http::Handler for App {{\n\
             fn serve(&self, request: http::Request) -> http::Response {{\n\
                 http::Response::text(200, \"alive\")\n\
             }}\n\
         }}\n\
         fn main() {{\n\
             let app = App::new()\n\
             http::serve(\"{addr}\", app)\n\
         }}\n",
    );

    let ready = Arc::new(AtomicBool::new(false));
    let ready_clone = Arc::clone(&ready);
    let server_thread = thread::spawn(move || {
        ready_clone.store(true, Ordering::Relaxed);
        run_interp(&source)
    });

    while !ready.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(5));
    }
    thread::sleep(Duration::from_millis(100));

    let mut stream = None;
    for _ in 0..40 {
        match TcpStream::connect(addr) {
            Ok(s) => {
                stream = Some(s);
                break;
            }
            Err(_) => thread::sleep(Duration::from_millis(25)),
        }
    }
    let mut stream = stream.expect("connect to interpreter-hosted server");
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    stream
        .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .unwrap();
    stream.shutdown(std::net::Shutdown::Write).unwrap();

    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader.read_line(&mut status_line).unwrap();
    assert!(
        status_line.starts_with("HTTP/1.1 "),
        "unexpected status: {status_line:?}"
    );
    let mut rest = Vec::new();
    reader.read_to_end(&mut rest).unwrap();

    let result = server_thread.join().expect("server thread panicked");
    assert!(result.is_ok(), "interpreter reported: {result:?}");
}
