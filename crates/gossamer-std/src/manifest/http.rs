#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Static manifest of every registered stdlib module.
//! Each stdlib milestone extends this table with
//! the modules it adds. Entries are listed in phase-introduction order
//! so a `gos doc` walk renders modules in the same sequence as the
//! implementation plan.

#![forbid(unsafe_code)]
use crate::registry::{StdItem, StdItemKind, StdModule};

use super::*;

pub const TLS: StdModule = StdModule {
    path: "std::tls",
    summary: "TLS termination and TLS client dialling (rustls-backed). Wired through both http::Server::bind_and_run_tls and http::Client; mTLS / ALPN / SNI exposed.",
    items: &[
        StdItem {
            name: "CertKey",
            kind: StdItemKind::Type,
            doc: "PEM-encoded certificate chain + private key.",
        },
        StdItem {
            name: "ServerConfig",
            kind: StdItemKind::Type,
            doc: "Opaque server-side TLS configuration.",
        },
        StdItem {
            name: "ClientConfig",
            kind: StdItemKind::Type,
            doc: "Opaque client-side TLS configuration.",
        },
        StdItem {
            name: "server_config",
            kind: StdItemKind::Function,
            doc: "Builds a server config from a CertKey. Returns Err until rustls lands.",
        },
        StdItem {
            name: "client_config",
            kind: StdItemKind::Function,
            doc: "Builds a client config. Returns Err until rustls lands.",
        },
    ],
};

pub const HTML_TEMPLATE: StdModule = StdModule {
    path: "std::html::template",
    summary: "Context-aware HTML templates with auto-escape.",
    items: &[
        StdItem {
            name: "Template",
            kind: StdItemKind::Type,
            doc: "Compiled HTML template.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses a template string.",
        },
        StdItem {
            name: "render",
            kind: StdItemKind::Function,
            doc: "Renders a template with the supplied data context.",
        },
    ],
};

pub const TEXT_TEMPLATE: StdModule = StdModule {
    path: "std::text::template",
    summary: "Plain-text templates (no escaping).",
    items: &[
        StdItem {
            name: "Template",
            kind: StdItemKind::Type,
            doc: "Compiled text template.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Parses a template string.",
        },
        StdItem {
            name: "render",
            kind: StdItemKind::Function,
            doc: "Renders a template with the supplied data context.",
        },
    ],
};

pub const HTTP: StdModule = StdModule {
    path: "std::http",
    summary: "HTTP/1.1 and HTTP/2 client and server. HTTP/2 negotiates via ALPN over TLS automatically (Go-style); h2c entry points are explicit.",
    items: &[
        StdItem {
            name: "Request",
            kind: StdItemKind::Type,
            doc: "HTTP request value passed to a handler.",
        },
        StdItem {
            name: "Response",
            kind: StdItemKind::Type,
            doc: "HTTP response value returned from a handler.",
        },
        StdItem {
            name: "Method",
            kind: StdItemKind::Type,
            doc: "HTTP method enumeration.",
        },
        StdItem {
            name: "StatusCode",
            kind: StdItemKind::Type,
            doc: "HTTP status code.",
        },
        StdItem {
            name: "Headers",
            kind: StdItemKind::Type,
            doc: "Case-insensitive header map.",
        },
        StdItem {
            name: "Server",
            kind: StdItemKind::Type,
            doc: "HTTP server bound to a TCP listener.",
        },
        StdItem {
            name: "serve",
            kind: StdItemKind::Function,
            doc: "Convenience: bind and serve an HTTP handler. `Result<(), Error>` — a bind failure is an Err value.",
        },
        StdItem {
            name: "Client",
            kind: StdItemKind::Type,
            doc: "HTTP client; configure redirects and timeout via `Client::builder()`.",
        },
        StdItem {
            name: "ResponseStream",
            kind: StdItemKind::Type,
            doc: "Streaming response body from `http::stream`; `next_line` / `next_chunk`, consumed by `Response::stream`.",
        },
        StdItem {
            name: "request",
            kind: StdItemKind::Function,
            doc: "One-shot request with a string body: `(method, url, body, headers) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "request_bytes",
            kind: StdItemKind::Function,
            doc: "One-shot request with a byte body: `(method, url, body: [u8], headers) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "stream",
            kind: StdItemKind::Function,
            doc: "One-shot request read incrementally: `(method, url, body, headers) -> Result<ResponseStream, Error>`.",
        },
        StdItem {
            name: "get",
            kind: StdItemKind::Function,
            doc: "One-shot GET: `(url, headers) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "post",
            kind: StdItemKind::Function,
            doc: "One-shot POST: `(url, body, content_type) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "put",
            kind: StdItemKind::Function,
            doc: "One-shot PUT: `(url, body, content_type) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "delete",
            kind: StdItemKind::Function,
            doc: "One-shot DELETE: `(url, body, headers) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "head",
            kind: StdItemKind::Function,
            doc: "One-shot HEAD: `(url, headers) -> Result<Response, Error>`.",
        },
        StdItem {
            name: "options",
            kind: StdItemKind::Function,
            doc: "One-shot OPTIONS: `(url, headers) -> Result<Response, Error>`.",
        },
        // HTTP/2 surface — folded in per the Go model.
        StdItem {
            name: "Http2Handler",
            kind: StdItemKind::Trait,
            doc: "Bounded-body HTTP/2 handler: serve(Request) -> Response.",
        },
        StdItem {
            name: "Http2StreamingHandler",
            kind: StdItemKind::Trait,
            doc: "Chunked-body HTTP/2 handler: serve(Request, StreamingResponseWriter) -> Result.",
        },
        StdItem {
            name: "StreamingResponseWriter",
            kind: StdItemKind::Type,
            doc: "Streaming HTTP/2 response writer; set_status / header / write_chunk / finish.",
        },
        StdItem {
            name: "Http2Config",
            kind: StdItemKind::Type,
            doc: "Per-connection HTTP/2 tuning (window sizes, max concurrent streams, frame caps).",
        },
        StdItem {
            name: "Http2ServerHandle",
            kind: StdItemKind::Type,
            doc: "Handle to a running HTTP/2 connection for shutdown / in-flight counts.",
        },
        StdItem {
            name: "Http2Error",
            kind: StdItemKind::Type,
            doc: "HTTP/2 server error: Io, Protocol, Handler.",
        },
        StdItem {
            name: "serve_h2_connection",
            kind: StdItemKind::Function,
            doc: "Drive an HTTP/2 connection on the calling goroutine (bounded handler).",
        },
        StdItem {
            name: "serve_h2_connection_streaming",
            kind: StdItemKind::Function,
            doc: "Same shape for Http2StreamingHandler.",
        },
        StdItem {
            name: "serve_h2c",
            kind: StdItemKind::Function,
            doc: "Bind a plain-TCP listener and serve h2c (HTTP/2 cleartext).",
        },
        StdItem {
            name: "serve_h2c_streaming",
            kind: StdItemKind::Function,
            doc: "Same shape for Http2StreamingHandler.",
        },
        StdItem {
            name: "Trailers",
            kind: StdItemKind::Type,
            doc: "HTTP/2 trailing HEADERS (alias for Headers) — used by `ResponseWriter::write_trailers` and `Request::trailers`.",
        },
        StdItem {
            name: "PushOptions",
            kind: StdItemKind::Type,
            doc: "Prioritization knobs for `ResponseWriter::push_promise` (weight, depends_on, exclusive).",
        },
        StdItem {
            name: "PushStream",
            kind: StdItemKind::Type,
            doc: "Server-initiated push stream returned by `ResponseWriter::push_promise`. Supports send_head / write / write_trailers / end.",
        },
    ],
};

pub const HTTP_ROUTER: StdModule = StdModule {
    path: "std::http::router",
    summary: "Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes.",
    items: &[
        StdItem {
            name: "Router",
            kind: StdItemKind::Type,
            doc: "Routing table (Rust-side full surface; method-chain bridge is interp tier).",
        },
        StdItem {
            name: "Params",
            kind: StdItemKind::Type,
            doc: "Captured path parameters.",
        },
        StdItem {
            name: "Handler",
            kind: StdItemKind::Trait,
            doc: "Anything callable as Fn(&Request, &Params) -> Response.",
        },
        StdItem {
            name: "new",
            kind: StdItemKind::Function,
            doc: "Allocate a fresh Router handle. Interp tier.",
        },
        StdItem {
            name: "add",
            kind: StdItemKind::Function,
            doc: "Register a route: `(router, method, pattern)`. Interp tier.",
        },
        StdItem {
            name: "lookup",
            kind: StdItemKind::Function,
            doc: "Find the index of the first route matching `(method, path)`. Interp tier.",
        },
    ],
};

pub const HTTP_MIDDLEWARE: StdModule = StdModule {
    path: "std::http::middleware",
    summary: "Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip.",
    items: &[
        StdItem {
            name: "Handler",
            kind: StdItemKind::Trait,
            doc: "Anything serving (Request, Params) -> Response.",
        },
        StdItem {
            name: "Chain",
            kind: StdItemKind::Type,
            doc: "Helper for composing middleware in a single value.",
        },
        StdItem {
            name: "logger",
            kind: StdItemKind::Function,
            doc: "Logs method path status bytes elapsed_ms per request.",
        },
        StdItem {
            name: "recoverer",
            kind: StdItemKind::Function,
            doc: "Catches handler panics; returns 500.",
        },
        StdItem {
            name: "request_id",
            kind: StdItemKind::Function,
            doc: "Stamps each response with X-Request-Id.",
        },
        StdItem {
            name: "cors",
            kind: StdItemKind::Function,
            doc: "CORS preflight + per-response headers.",
        },
        StdItem {
            name: "basic_auth",
            kind: StdItemKind::Function,
            doc: "HTTP Basic auth gate; constant-time compare.",
        },
        StdItem {
            name: "compress_gzip",
            kind: StdItemKind::Function,
            doc: "Gzips bodies above a size threshold when client advertises Accept-Encoding: gzip (Rust-side wrapper).",
        },
        StdItem {
            name: "new_request_id",
            kind: StdItemKind::Function,
            doc: "Generate a process-monotonic request id string. Available in interp + compiled.",
        },
        StdItem {
            name: "accepts_gzip",
            kind: StdItemKind::Function,
            doc: "Check an Accept-Encoding header for a gzip token. Available in interp + compiled.",
        },
        StdItem {
            name: "decode_basic_auth",
            kind: StdItemKind::Function,
            doc: "Decode a Basic-auth Authorization header into (user, password). Interp tier.",
        },
    ],
};

pub const HTTP_STATIC_FILES: StdModule = StdModule {
    path: "std::http::static_files",
    summary: "Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff.",
    items: &[
        StdItem {
            name: "FileServer",
            kind: StdItemKind::Type,
            doc: "Static-file handler rooted at a directory (Rust-side; streaming).",
        },
        StdItem {
            name: "serve_file",
            kind: StdItemKind::Function,
            doc: "Read a single file and return it as a Response struct. Interp tier.",
        },
        StdItem {
            name: "mime_for_path",
            kind: StdItemKind::Function,
            doc: "Guess a MIME type from a file path's extension. Available in interp + compiled.",
        },
    ],
};

pub const HTTP_PROXY: StdModule = StdModule {
    path: "std::http::proxy",
    summary: "Reverse proxy on top of http::Client. Director-style request mutator + hop-by-hop strip + error handler.",
    items: &[
        StdItem {
            name: "Proxy",
            kind: StdItemKind::Type,
            doc: "Reverse-proxy handler (Rust-side).",
        },
        StdItem {
            name: "Director",
            kind: StdItemKind::Type,
            doc: "Fn(&mut Request) request mutator (Rust-side).",
        },
        StdItem {
            name: "forward",
            kind: StdItemKind::Function,
            doc: "One-shot upstream forward: `(url, method, body) -> Result<Response, Error>`. Interp tier.",
        },
    ],
};

pub const HTTP_WEBSOCKET: StdModule = StdModule {
    path: "std::http::websocket",
    summary: "RFC 6455 WebSocket support. Server-side accept + send_text / send_binary / ping / pong / close.",
    items: &[
        StdItem {
            name: "WebSocket",
            kind: StdItemKind::Type,
            doc: "Accepted WebSocket connection (Rust-side framing).",
        },
        StdItem {
            name: "Message",
            kind: StdItemKind::Type,
            doc: "Text / Binary / Ping / Pong / Close.",
        },
        StdItem {
            name: "accept",
            kind: StdItemKind::Function,
            doc: "Upgrade an incoming Request to a WebSocket (Rust-side).",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Io / Protocol / BadHandshake.",
        },
        StdItem {
            name: "accept_key",
            kind: StdItemKind::Function,
            doc: "Compute RFC 6455 Sec-WebSocket-Accept from a client nonce. Available in interp + compiled.",
        },
        StdItem {
            name: "is_websocket_upgrade",
            kind: StdItemKind::Function,
            doc: "Test whether an incoming Request carries a WebSocket upgrade handshake. Interp tier.",
        },
    ],
};

pub const HTTP_SSE: StdModule = StdModule {
    path: "std::http::sse",
    summary: "Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint.",
    items: &[
        StdItem {
            name: "Stream",
            kind: StdItemKind::Type,
            doc: "Active SSE stream — handler writes events through it (Rust-side).",
        },
        StdItem {
            name: "Event",
            kind: StdItemKind::Type,
            doc: "One SSE event (id, event, data, retry).",
        },
        StdItem {
            name: "serve",
            kind: StdItemKind::Function,
            doc: "Wraps a handler closure into an SSE response (Rust-side; streaming).",
        },
        StdItem {
            name: "encode_event",
            kind: StdItemKind::Function,
            doc: "Render one event block as a string: `(event, data, id) -> String`. Available in interp + compiled.",
        },
        StdItem {
            name: "encode_comment",
            kind: StdItemKind::Function,
            doc: "Render a `:`-prefixed keepalive line. Available in interp + compiled.",
        },
        StdItem {
            name: "encode_retry",
            kind: StdItemKind::Function,
            doc: "Render a `retry:` reconnect-hint directive in milliseconds. Available in interp + compiled.",
        },
    ],
};

pub const HTTP_CHUNKED: StdModule = StdModule {
    path: "std::http::chunked",
    summary: "RFC 7230 §4.1 chunked transfer-encoding reader and writer.",
    items: &[
        StdItem {
            name: "Reader",
            kind: StdItemKind::Type,
            doc: "Decodes a chunked body from any Read source (Rust-side; streaming).",
        },
        StdItem {
            name: "Writer",
            kind: StdItemKind::Type,
            doc: "Encodes raw bytes into chunked frames over any Write sink (Rust-side; streaming).",
        },
        StdItem {
            name: "encode",
            kind: StdItemKind::Function,
            doc: "One-shot: wraps a buffer in chunked transfer-encoding with terminator. Available in interp + compiled.",
        },
        StdItem {
            name: "decode",
            kind: StdItemKind::Function,
            doc: "One-shot: concatenates data chunks from a complete chunked body. Available in interp + compiled.",
        },
    ],
};

pub const HTTP_NATIVE_CLIENT: StdModule = StdModule {
    path: "std::http::native_client",
    summary: "Goroutine-driven HTTP/1.1 client over std::net (no ureq, no blocking pool).",
    items: &[
        StdItem {
            name: "Client",
            kind: StdItemKind::Type,
            doc: "Native h1 client (Rust-side; full builder surface).",
        },
        StdItem {
            name: "Error",
            kind: StdItemKind::Type,
            doc: "Connect / Tls / Http / Redirect / Timeout / Io.",
        },
        StdItem {
            name: "get",
            kind: StdItemKind::Function,
            doc: "One-shot GET → Result<Response, Error>. Interp tier (compiled tier shares http::get).",
        },
        StdItem {
            name: "post",
            kind: StdItemKind::Function,
            doc: "One-shot POST: `(url, body, content_type)`. Interp tier.",
        },
        StdItem {
            name: "put",
            kind: StdItemKind::Function,
            doc: "One-shot PUT: `(url, body, content_type)`. Interp tier.",
        },
        StdItem {
            name: "delete",
            kind: StdItemKind::Function,
            doc: "One-shot DELETE → Result<Response, Error>. Interp tier.",
        },
    ],
};

pub const HTTP_COOKIE: StdModule = StdModule {
    path: "std::http::cookie",
    summary: "RFC 6265 cookie parser and Set-Cookie builder.",
    items: &[
        StdItem {
            name: "Cookie",
            kind: StdItemKind::Type,
            doc: "Parsed cookie with name, value, and Set-Cookie attributes.",
        },
        StdItem {
            name: "CookieBuilder",
            kind: StdItemKind::Type,
            doc: "Fluent builder for Set-Cookie response headers.",
        },
        StdItem {
            name: "SameSite",
            kind: StdItemKind::Type,
            doc: "SameSite attribute: Strict / Lax / None.",
        },
        StdItem {
            name: "parse_cookie_header",
            kind: StdItemKind::Function,
            doc: "Parse a Cookie request header into (name, value) pairs.",
        },
        StdItem {
            name: "parse_set_cookie",
            kind: StdItemKind::Function,
            doc: "Parse a Set-Cookie response header into a Cookie.",
        },
    ],
};

pub const HTTP_CSRF: StdModule = StdModule {
    path: "std::http::csrf",
    summary: "Double-submit-cookie CSRF protection with Origin / Referer allowlist.",
    items: &[
        StdItem {
            name: "Config",
            kind: StdItemKind::Type,
            doc: "Signing key, cookie / header names, and origin allowlist.",
        },
        StdItem {
            name: "RouteAuth",
            kind: StdItemKind::Type,
            doc: "Per-route policy: Required, Optional, or Skipped.",
        },
        StdItem {
            name: "issue_token",
            kind: StdItemKind::Function,
            doc: "Mint a fresh CSRF token bound to the configured signing key.",
        },
        StdItem {
            name: "verify_token",
            kind: StdItemKind::Function,
            doc: "Constant-time verify of a presented token against the cookie value.",
        },
        StdItem {
            name: "extract_token",
            kind: StdItemKind::Function,
            doc: "Pull a token from the configured header or form field.",
        },
        StdItem {
            name: "origin_allowed",
            kind: StdItemKind::Function,
            doc: "Origin / Referer allowlist check for unsafe methods.",
        },
        StdItem {
            name: "check",
            kind: StdItemKind::Function,
            doc: "Combined origin + token gate; returns Err on failure.",
        },
        StdItem {
            name: "attach_cookie",
            kind: StdItemKind::Function,
            doc: "Set the CSRF cookie on a Response.",
        },
    ],
};

pub const HTTP_FORM: StdModule = StdModule {
    path: "std::http::form",
    summary: "application/x-www-form-urlencoded parser and builder.",
    items: &[
        StdItem {
            name: "Form",
            kind: StdItemKind::Type,
            doc: "Parsed url-encoded body, queryable by field name.",
        },
        StdItem {
            name: "FormBuilder",
            kind: StdItemKind::Type,
            doc: "Builder for url-encoded request bodies.",
        },
    ],
};

pub const HTTP_HEALTH: StdModule = StdModule {
    path: "std::http::health",
    summary: "Liveness / readiness probes for HTTP health endpoints.",
    items: &[
        StdItem {
            name: "Probe",
            kind: StdItemKind::Trait,
            doc: "One health check returning Ok or Err with a short message.",
        },
        StdItem {
            name: "Health",
            kind: StdItemKind::Type,
            doc: "Aggregates a set of named probes into a single status.",
        },
        StdItem {
            name: "always_ok",
            kind: StdItemKind::Function,
            doc: "Probe that always reports healthy.",
        },
        StdItem {
            name: "always_fail",
            kind: StdItemKind::Function,
            doc: "Probe that always reports unhealthy with the given message.",
        },
        StdItem {
            name: "tcp_probe",
            kind: StdItemKind::Function,
            doc: "Probe that opens a TCP connection within a deadline.",
        },
    ],
};

pub const HTTP_MULTIPART: StdModule = StdModule {
    path: "std::http::multipart",
    summary: "RFC 7578 multipart/form-data streaming parser.",
    items: &[
        StdItem {
            name: "Config",
            kind: StdItemKind::Type,
            doc: "Per-form size, part-count, and disk-spill limits.",
        },
        StdItem {
            name: "Part",
            kind: StdItemKind::Type,
            doc: "One field or file entry from a multipart body.",
        },
        StdItem {
            name: "PartData",
            kind: StdItemKind::Type,
            doc: "In-memory bytes or spilled-to-disk path for a part.",
        },
        StdItem {
            name: "Form",
            kind: StdItemKind::Type,
            doc: "Parsed multipart envelope: fields + file parts.",
        },
        StdItem {
            name: "parse_boundary",
            kind: StdItemKind::Function,
            doc: "Extract the boundary token from a Content-Type header.",
        },
        StdItem {
            name: "parse_bytes",
            kind: StdItemKind::Function,
            doc: "Parse a full body buffer into a Form.",
        },
        StdItem {
            name: "parse",
            kind: StdItemKind::Function,
            doc: "Stream-parse from any Read source into a Form.",
        },
    ],
};

pub const HTTP_QUERY: StdModule = StdModule {
    path: "std::http::query",
    summary: "Typed wrapper over URL query strings.",
    items: &[StdItem {
        name: "Query",
        kind: StdItemKind::Type,
        doc: "Parsed query string with typed get / get_all / contains.",
    }],
};

pub const HTTP_SESSION: StdModule = StdModule {
    path: "std::http::session",
    summary: "Signed-cookie session store with pluggable backend trait.",
    items: &[
        StdItem {
            name: "Session",
            kind: StdItemKind::Type,
            doc: "Per-request session view; mutations persist on response.",
        },
        StdItem {
            name: "SessionConfig",
            kind: StdItemKind::Type,
            doc: "Cookie name, domain, signing key, serialization mode.",
        },
        StdItem {
            name: "SessionStore",
            kind: StdItemKind::Trait,
            doc: "Backend interface: load / save / delete by session id.",
        },
        StdItem {
            name: "SignedCookieStore",
            kind: StdItemKind::Type,
            doc: "Cookie-backed store with HMAC signature; no server state.",
        },
        StdItem {
            name: "SerializationMode",
            kind: StdItemKind::Type,
            doc: "Session payload encoding: Json or Bincode.",
        },
        StdItem {
            name: "with_session",
            kind: StdItemKind::Function,
            doc: "Run a closure with the session bound; persist any mutations.",
        },
    ],
};

pub const HTTP_STATE: StdModule = StdModule {
    path: "std::http::state",
    summary: "Handler-side dependency injection via a typed AppState.",
    items: &[
        StdItem {
            name: "AppState",
            kind: StdItemKind::Type,
            doc: "TypeMap of Arc<T> values shared across handlers.",
        },
        StdItem {
            name: "State",
            kind: StdItemKind::Type,
            doc: "Newtype wrapper Arc<T> for ergonomic handler arguments.",
        },
    ],
};

pub const JWT: StdModule = StdModule {
    path: "std::jwt",
    summary: "RFC 7519 sign / verify for HS256 / HS384 / HS512, ES256, and EdDSA tokens.",
    items: &[
        StdItem {
            name: "Alg",
            kind: StdItemKind::Type,
            doc: "Signing algorithm: HS256 / HS384 / HS512 / ES256 / EdDSA.",
        },
        StdItem {
            name: "Header",
            kind: StdItemKind::Type,
            doc: "JWS / JWT header (alg, kid, typ).",
        },
        StdItem {
            name: "Claims",
            kind: StdItemKind::Type,
            doc: "Standard registered claims plus a free-form custom map.",
        },
        StdItem {
            name: "VerifyOpts",
            kind: StdItemKind::Type,
            doc: "Expected issuer / audience / clock leeway used by verify.",
        },
        StdItem {
            name: "sign_hs",
            kind: StdItemKind::Function,
            doc: "Sign claims with HMAC-SHA family using a shared key.",
        },
        StdItem {
            name: "verify_hs",
            kind: StdItemKind::Function,
            doc: "Verify an HS* token; checks signature plus VerifyOpts.",
        },
        StdItem {
            name: "sign_es256",
            kind: StdItemKind::Function,
            doc: "Sign with ECDSA P-256 from a PEM-encoded private key.",
        },
        StdItem {
            name: "verify_es256",
            kind: StdItemKind::Function,
            doc: "Verify an ES256 token against a PEM-encoded public key.",
        },
        StdItem {
            name: "sign_eddsa",
            kind: StdItemKind::Function,
            doc: "Sign with Ed25519 from a PEM-encoded private key.",
        },
        StdItem {
            name: "verify_eddsa",
            kind: StdItemKind::Function,
            doc: "Verify an EdDSA token against a PEM-encoded public key.",
        },
    ],
};

pub const HTTP_H3: StdModule = StdModule {
    path: "std::http_h3",
    summary: "First-party HTTP/3 server + client over QUIC (RFC 9114; quinn + h3). Each `serve` and `Client` instance owns a private tokio runtime; callers see only synchronous entry points.",
    items: &[
        StdItem {
            name: "Handler",
            kind: StdItemKind::Trait,
            doc: "Per-request handler. `fn serve(&self, request: Request) -> Response`.",
        },
        StdItem {
            name: "H3Error",
            kind: StdItemKind::Type,
            doc: "Transport / protocol error variants surfaced from quinn + h3.",
        },
        StdItem {
            name: "serve",
            kind: StdItemKind::Function,
            doc: "Run an HTTP/3 server bound to `addr` with TLS certificate + key paths and the supplied handler.",
        },
        StdItem {
            name: "Client",
            kind: StdItemKind::Type,
            doc: "HTTP/3 client. `new` validates against the Mozilla root store; `insecure` skips verification (dev only). Methods: `get`, `post`, `put`, `delete`, `head`, `request`.",
        },
    ],
};
