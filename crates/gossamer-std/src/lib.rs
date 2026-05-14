//! Standard library for Gossamer (Phases 22 → 26).
//! Introduces this crate as the manifest + Rust-side runtime
//! support for every stdlib module. Subsequent phases extend the
//! manifest with their own module entries while reusing the shared
//! infrastructure exposed here. The Gossamer source files that
//! eventually compile via `gos build` will live alongside this crate
//! in `crates/gossamer-std/std/*.gos` and call into the helpers here
//! for primitives the language can't yet express in itself.

#![deny(unsafe_code)]

/// Archive readers and writers (zip, tar).
pub mod archive;
/// `AsyncRead`/`AsyncWrite` bridge over `net::TcpStream`.
pub mod async_tcp;
pub mod blocking_pool;
pub mod bufio;
pub mod bytes;
pub mod collections;
/// Compression and decompression codecs (gzip, flate, zlib, bzip2).
pub mod compress;
pub mod context;
pub mod crypto;
pub mod database;
pub mod encoding;
/// Process environment, command-line arguments, working directory
/// (Rust `std::env` shape).
pub mod env;
pub mod errors;
pub mod exec;
pub mod ffi;
pub mod flag;
pub mod fmt;
pub mod fs;
/// Non-cryptographic hash functions (FNV, etc.).
pub mod hash;
/// HTML escaping utilities and optional template engine.
pub mod html;
pub mod http;
/// HTTP/1.1 chunked transfer-encoding (RFC 7230 §4.1) reader / writer.
pub mod http_chunked;
/// HTTP/2 server (h2 crate over goroutine future-driver).
pub mod http_h2;
/// HTTP middleware suite (logger, recoverer, request-id, CORS, basic-auth, gzip).
pub mod http_middleware;
/// Native h1 HTTP client over `std::net::TcpStream`.
pub mod http_native_client;
/// HTTP reverse proxy (Director, hop-by-hop strip, error handler).
pub mod http_proxy;
/// HTTP router (Go 1.22-class ServeMux with captures + method gating).
pub mod http_router;
/// Server-Sent Events (`text/event-stream`).
pub mod http_sse;
/// HTTP static file server (ETag, Last-Modified, Range, MIME sniff).
pub mod http_static_files;
/// WebSocket (RFC 6455) — first-party stdlib support.
pub mod http_websocket;
pub mod io;
pub mod iter;
/// Go-style `log` package: flat `Println` / `Printf` / `Fatal`.
pub mod log;
/// Goroutine-driven future polling (no tokio runtime). Always
/// compiled — drives HTTP/2 + future async stacks.
pub mod runtime_future;

/// Chaining combinators for `Option<T>`. F#-style free functions
/// with data-last argument order so they thread through `|>`.
/// See SPEC §10.4a.
pub mod option;

/// Chaining combinators for `Result<T, E>`. F#-style free functions
/// with data-last argument order so they thread through `|>`.
/// See SPEC §10.4b. The `?` operator (SPEC §4.5) remains the right
/// tool for short-circuit propagation.
pub mod result;

pub mod json;
pub mod manifest;
/// Mathematical constants, f64 functions, and integer bit operations.
pub mod math;
pub mod mathrand;
pub mod net;
pub mod os;
pub mod panic;
pub mod path;
pub mod pprof;
/// Child processes and process control (Rust `std::process` shape).
pub mod process;
pub mod regex;
pub mod registry;
pub mod runtime;
pub mod sched_global;
pub mod signal;
pub mod slog;
pub mod sort;
pub mod strconv;
pub mod strings;
pub mod sync;
pub mod testing;
pub mod text;
/// OS thread spawn and sleep helpers.
pub mod thread;
pub mod time;
pub mod tls;
pub mod unicode;
pub mod url;
pub mod utf16;
pub mod utf8;

pub use registry::{StdItem, StdItemKind, StdModule, item, module, modules};
