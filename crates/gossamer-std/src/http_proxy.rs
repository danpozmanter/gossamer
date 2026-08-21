#![allow(
    clippy::similar_names,
    clippy::items_after_statements,
    clippy::redundant_closure_for_method_calls,
    clippy::redundant_closure,
    clippy::map_unwrap_or,
    clippy::uninlined_format_args,
    clippy::type_complexity,
    clippy::clone_on_copy,
    clippy::missing_errors_doc
)]

//! HTTP reverse proxy.
//!
//! `ReverseProxy` forwards inbound requests to a configured
//! upstream and pipes the response back. Mirrors Go's
//! `httputil.ReverseProxy` shape:
//!
//! - **Director** - caller-supplied function to rewrite the
//!   forwarded request (target URL, headers).
//! - **ModifyResponse** - caller-supplied function to mutate
//!   the upstream response before it's sent to the client.
//! - **ErrorHandler** - caller-supplied function invoked when
//!   the upstream fails; defaults to `502 Bad Gateway`.
//! - **Hop-by-hop header stripping** per RFC 7230 §6.1.
//! - **`X-Forwarded-For`** / **`X-Forwarded-Proto`** /
//!   **`X-Forwarded-Host`** appended automatically.
//!
//! Implemented on top of [`crate::http::Client`] for the upstream
//! HTTP call.

use std::sync::Arc;

use crate::http::{Client, ClientError, Headers, Method, Request, Response, StatusCode};

/// Reverse-proxy handler.
pub struct ReverseProxy {
    /// HTTP client used to talk to upstream. Re-use across
    /// requests so connection pooling kicks in.
    pub client: Arc<Client>,
    /// Rewrites the inbound request into the upstream request.
    /// Typical implementation sets the upstream URL on the
    /// `Director`'s associated state.
    pub director: Box<dyn Fn(&Request) -> ForwardedRequest + Send + Sync>,
    /// Optional post-processor for the upstream response.
    pub modify_response: Option<Box<dyn Fn(&mut Response) + Send + Sync>>,
    /// Optional handler invoked on transport error. Default
    /// returns a 502 with the error message.
    pub error_handler: Option<Box<dyn Fn(&Request, &ClientError) -> Response + Send + Sync>>,
}

/// Output of the director - the upstream call's parts.
#[derive(Debug, Clone)]
pub struct ForwardedRequest {
    /// Full upstream URL (scheme + host + path + query).
    pub url: String,
    /// Method to use upstream.
    pub method: Method,
    /// Headers to send. Hop-by-hop headers will be stripped.
    pub headers: Vec<(String, String)>,
    /// Body bytes.
    pub body: Vec<u8>,
}

impl ReverseProxy {
    /// Constructs a single-host reverse proxy. All inbound
    /// requests are forwarded verbatim (preserving path + query)
    /// to `upstream_origin`, which must be the
    /// scheme+host portion (e.g. `"https://backend.local"`,
    /// no trailing slash).
    pub fn single_host(upstream_origin: impl Into<String>) -> Self {
        let origin = upstream_origin.into();
        let origin_for_director = origin.clone();
        Self {
            client: Arc::new(Client::new()),
            director: Box::new(move |req| {
                let path_q = if req.query.is_empty() {
                    req.path.clone()
                } else {
                    format!("{}?{}", req.path, req.query)
                };
                let url = format!("{}{}", origin_for_director.trim_end_matches('/'), path_q);
                let headers: Vec<(String, String)> = req
                    .headers
                    .iter()
                    .map(|(k, v)| (k.to_string(), v.to_string()))
                    .collect();
                ForwardedRequest {
                    url,
                    method: req.method.clone(),
                    headers,
                    body: req.body.clone(),
                }
            }),
            modify_response: None,
            error_handler: None,
        }
    }

    /// Replaces the director.
    #[must_use]
    pub fn with_director(
        mut self,
        director: impl Fn(&Request) -> ForwardedRequest + Send + Sync + 'static,
    ) -> Self {
        self.director = Box::new(director);
        self
    }

    /// Replaces the response modifier.
    #[must_use]
    pub fn with_modify_response(
        mut self,
        f: impl Fn(&mut Response) + Send + Sync + 'static,
    ) -> Self {
        self.modify_response = Some(Box::new(f));
        self
    }

    /// Replaces the error handler.
    #[must_use]
    pub fn with_error_handler(
        mut self,
        f: impl Fn(&Request, &ClientError) -> Response + Send + Sync + 'static,
    ) -> Self {
        self.error_handler = Some(Box::new(f));
        self
    }

    /// Dispatches `request` to the upstream and returns the
    /// processed response. Use as the handler in a router.
    pub fn serve(&self, request: &Request) -> Response {
        let mut forwarded = (self.director)(request);
        strip_hop_by_hop(&mut forwarded.headers);
        // Add X-Forwarded-* metadata.
        let xff_existing = request.headers.get("x-forwarded-for").map(String::from);
        let xff = if let Some(existing) = xff_existing {
            format!("{existing}, gossamer-proxy")
        } else {
            "gossamer-proxy".to_string()
        };
        upsert_header(&mut forwarded.headers, "x-forwarded-for", &xff);
        if let Some(host) = request.headers.get("host") {
            upsert_header(&mut forwarded.headers, "x-forwarded-host", host);
        }
        upsert_header(&mut forwarded.headers, "x-forwarded-proto", "http");

        // Make the upstream call.
        let header_refs: Vec<(&str, &str)> = forwarded
            .headers
            .iter()
            .map(|(k, v)| (k.as_str(), v.as_str()))
            .collect();
        let result = self.client.request(
            forwarded.method.as_str(),
            &forwarded.url,
            Some(&forwarded.body),
            &header_refs,
        );
        match result {
            Ok(upstream) => {
                let mut downstream = Response {
                    status: upstream.status,
                    headers: upstream.headers,
                    body: upstream.body,
                    raw_header_pairs: Vec::new(),
                    body_stream: None,
                };
                strip_hop_by_hop_headers(&mut downstream.headers);
                if let Some(modifier) = &self.modify_response {
                    modifier(&mut downstream);
                }
                downstream
            }
            Err(err) => {
                if let Some(handler) = &self.error_handler {
                    return handler(request, &err);
                }
                let mut headers = Headers::new();
                headers.insert("content-type", "text/plain; charset=utf-8");
                Response {
                    status: StatusCode(502),
                    headers,
                    body: format!("bad gateway: {err}").into_bytes(),
                    raw_header_pairs: Vec::new(),
                    body_stream: None,
                }
            }
        }
    }
}

/// Hop-by-hop headers per RFC 7230 §6.1 that must NOT be
/// forwarded.
const HOP_BY_HOP: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

fn strip_hop_by_hop(headers: &mut Vec<(String, String)>) {
    headers.retain(|(k, _)| !HOP_BY_HOP.contains(&k.to_ascii_lowercase().as_str()));
}

fn strip_hop_by_hop_headers(headers: &mut Headers) {
    for name in HOP_BY_HOP {
        headers.remove(name);
    }
}

fn upsert_header(headers: &mut Vec<(String, String)>, name: &str, value: &str) {
    let lower = name.to_ascii_lowercase();
    if let Some(slot) = headers
        .iter_mut()
        .find(|(k, _)| k.to_ascii_lowercase() == lower)
    {
        slot.1 = value.to_string();
    } else {
        headers.push((name.to_string(), value.to_string()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Context;
    use crate::http::server::{Config, run};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::Arc as StdArc;
    use std::sync::atomic::AtomicBool;
    use std::thread;
    use std::time::Duration;

    fn bind_loopback() -> (TcpListener, SocketAddr) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    /// Returns an ephemeral port that was bound, then released -
    /// useful for negative tests that need a port nothing is
    /// listening on. The brief gap between drop and reuse is OK
    /// since the caller's intent is "no listener here".
    fn pick_unbound_port() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn empty_request() -> Request {
        Request {
            method: Method::Get,
            path: "/x".to_string(),
            query: String::new(),
            headers: Headers::new(),
            body: Vec::new(),
            context: Context::background(),
            trailers: None,
            peer_addr: String::new(),
        }
    }

    #[test]
    fn strip_hop_by_hop_removes_listed_headers() {
        let mut h = vec![
            ("Connection".to_string(), "keep-alive".to_string()),
            ("X-Real".to_string(), "value".to_string()),
            ("transfer-encoding".to_string(), "chunked".to_string()),
            ("trailer".to_string(), "x".to_string()),
        ];
        strip_hop_by_hop(&mut h);
        assert_eq!(h.len(), 1);
        assert_eq!(h[0].0, "X-Real");
    }

    #[test]
    fn upsert_header_replaces_existing_case_insensitive() {
        let mut h = vec![("Host".to_string(), "old".to_string())];
        upsert_header(&mut h, "host", "new");
        assert_eq!(h, vec![("Host".to_string(), "new".to_string())]);
    }

    #[test]
    fn single_host_proxy_forwards_path_and_query() {
        // Start a real upstream that echoes back its path + query.
        let (listener, actual_addr) = bind_loopback();
        let shutdown = StdArc::new(AtomicBool::new(false));
        let config = Config {
            max_requests: Some(1),
            shutdown: StdArc::clone(&shutdown),
            ..Config::default()
        };
        let upstream = thread::spawn(move || {
            let _ = run(listener, &config, |req: Request| {
                let body = format!("{}|{}", req.path, req.query);
                Response {
                    status: StatusCode(200),
                    headers: Headers::new(),
                    body: body.into_bytes(),
                    raw_header_pairs: Vec::new(),
                    body_stream: None,
                }
            });
        });
        thread::sleep(Duration::from_millis(50));

        let proxy = ReverseProxy::single_host(format!("http://{actual_addr}"));
        let mut req = empty_request();
        req.path = "/echo".to_string();
        req.query = "name=jane".to_string();
        let resp = proxy.serve(&req);

        let _ = std::net::TcpStream::connect(actual_addr);
        upstream.join().unwrap();

        assert_eq!(resp.status, StatusCode(200));
        assert_eq!(resp.body, b"/echo|name=jane");
    }

    #[test]
    fn error_handler_runs_on_transport_failure() {
        // Point at an unbound port so the request fails.
        let port = pick_unbound_port();
        let proxy = ReverseProxy::single_host(format!("http://127.0.0.1:{}", port.port()))
            .with_error_handler(|_req, _err| Response {
                status: StatusCode(503),
                headers: Headers::new(),
                body: b"custom 503".to_vec(),
                raw_header_pairs: Vec::new(),
                body_stream: None,
            });
        let resp = proxy.serve(&empty_request());
        assert_eq!(resp.status, StatusCode(503));
        assert_eq!(resp.body, b"custom 503");
    }
}
