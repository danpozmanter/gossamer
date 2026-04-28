//!: `std::net` + `std::http` API tests.

use std::io::{Read, Write};
use std::net::TcpStream as StdTcpStream;
use std::thread;

use gossamer_std::http::{
    Headers, Method, Request, Response, Server, StatusCode, parse_request_line, parse_status_line,
};
use gossamer_std::net::{TcpListener, resolve};

#[test]
fn method_round_trips_through_parse() {
    for method in [
        Method::Get,
        Method::Post,
        Method::Put,
        Method::Delete,
        Method::Patch,
        Method::Head,
        Method::Options,
    ] {
        let parsed = Method::parse(method.as_str()).unwrap();
        assert_eq!(parsed, method);
    }
    assert_eq!(Method::parse("get"), Some(Method::Get));
    assert_eq!(Method::parse("nope"), None);
}

#[test]
fn status_code_reason_phrases_cover_common_codes() {
    assert_eq!(StatusCode::OK.as_u16(), 200);
    assert_eq!(StatusCode::OK.reason(), Some("OK"));
    assert_eq!(StatusCode::NOT_FOUND.reason(), Some("Not Found"));
    assert!(StatusCode::OK.is_success());
    assert!(!StatusCode::BAD_REQUEST.is_success());
    assert!(StatusCode(999).reason().is_none());
}

#[test]
fn headers_are_case_insensitive() {
    let mut h = Headers::new();
    h.insert("Content-Type", "text/plain");
    assert_eq!(h.get("content-type"), Some("text/plain"));
    assert_eq!(h.get("CONTENT-TYPE"), Some("text/plain"));
    assert!(h.contains("Content-Type"));
    assert!(!h.contains("X-Missing"));
    assert_eq!(h.len(), 1);
}

#[test]
fn response_text_sets_content_type_and_length() {
    let resp = Response::text(StatusCode::OK, "hi");
    assert_eq!(resp.status, StatusCode::OK);
    assert_eq!(
        resp.headers.get("content-type"),
        Some("text/plain; charset=utf-8")
    );
    assert_eq!(resp.headers.get("content-length"), Some("2"));
    assert_eq!(resp.body, b"hi");
}

#[test]
fn response_json_sets_json_content_type() {
    let resp = Response::json(StatusCode::CREATED, b"{\"ok\":true}".to_vec());
    assert_eq!(resp.status, StatusCode::CREATED);
    assert_eq!(resp.headers.get("content-type"), Some("application/json"));
    assert_eq!(resp.headers.get("content-length"), Some("11"));
}

#[test]
fn request_line_parser_handles_well_formed_input() {
    let parsed = parse_request_line("GET /health HTTP/1.1");
    let (method, path, version) = parsed.unwrap();
    assert_eq!(method, Method::Get);
    assert_eq!(path, "/health");
    assert_eq!(version, "HTTP/1.1");
    assert!(parse_request_line("GARBAGE").is_none());
}

#[test]
fn status_line_parser_extracts_reason_phrase() {
    let (version, code, reason) = parse_status_line("HTTP/1.1 200 OK").unwrap();
    assert_eq!(version, "HTTP/1.1");
    assert_eq!(code, StatusCode::OK);
    assert_eq!(reason, "OK");
    assert!(parse_status_line("not a status line").is_none());
}

#[test]
fn request_accessor_returns_path() {
    let req = Request {
        method: Method::Get,
        path: "/a/b".to_string(),
        headers: Headers::new(),
        body: Vec::new(),
        context: gossamer_std::context::Context::background(),
    };
    assert_eq!(req.path(), "/a/b");
}

#[test]
fn stub_server_and_client_construct_without_error() {
    let _server = Server::new();
    let _client = gossamer_std::http::Client::new();
}

#[test]
fn tcp_listener_accepts_a_local_connection() {
    let mut listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("local_addr");
    let addr_string = addr.to_string();
    let handle = thread::spawn(move || {
        let mut stream = StdTcpStream::connect(addr).expect("connect");
        stream.write_all(b"ping").unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).unwrap();
        buf
    });
    let (mut stream, _) = listener.accept().expect("accept");
    let mut buf = [0u8; 4];
    stream.read(&mut buf).unwrap();
    assert_eq!(&buf, b"ping");
    stream.write_all(b"pong").unwrap();
    let buf = handle.join().unwrap();
    assert_eq!(&buf, b"pong");
    assert!(!addr_string.is_empty());
}

#[test]
fn resolve_returns_loopback_for_localhost() {
    let addrs = resolve("localhost:80").expect("resolve localhost");
    assert!(!addrs.is_empty());
    assert!(addrs.iter().any(|a| a.ip().is_loopback()));
}
