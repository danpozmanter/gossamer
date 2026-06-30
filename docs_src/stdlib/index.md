# Gossamer standard library

One page per module. Source is `crates/gossamer-std/src/`; this index is regenerated from `manifest::ALL_MODULES` by `gos doc --emit-stdlib`.

| Module | Summary |
|---|---|
| [`std::archive::tar`](archive_tar.md) | Unix tar reader and writer (USTAR / PAX-aware decode). |
| [`std::archive::zip`](archive_zip.md) | ZIP archive reader and writer. |
| [`std::bufio`](bufio.md) | Buffered readers, writers, and line scanners. |
| [`std::bytes`](bytes.md) | Byte buffers, builders, and slice helpers. |
| [`std::collections`](collections.md) | Built-in container types. |
| [`std::collections::deque`](collections_deque.md) | Double-ended queue over Vec<i64>. Re-bind shape on every mutator. |
| [`std::collections::heap`](collections_heap.md) | Binary min-heap (priority queue) over Vec<i64>. Re-bind shape: `let h = heap::push(h, v)`. |
| [`std::collections::ordered_map`](collections_ordered_map.md) | Sorted key/value map (i64 -> i64) backed by a flat pair Vec. Re-bind on every mutator. |
| [`std::collections::ordered_set`](collections_ordered_set.md) | Sorted set of i64 with binary-search lookups. Re-bind shape on every mutator. |
| [`std::collections::ordered_vec`](collections_ordered_vec.md) | Sorted-on-insert Vec<i64> with binary-search lookups. |
| [`std::collections::queue`](collections_queue.md) | FIFO queue over Vec<i64>. Re-bind shape: `let q = queue::push(q, v)`. |
| [`std::collections::stack`](collections_stack.md) | LIFO stack over Vec<i64>. Re-bind shape: `let s = stack::push(s, v)`. |
| [`std::compress::bzip2`](compress_bzip2.md) | bzip2 encoder / decoder (BZh format). |
| [`std::compress::flate`](compress_flate.md) | Raw DEFLATE (RFC 1951) encoder / decoder. |
| [`std::compress::gzip`](compress_gzip.md) | gzip encoder / decoder (RFC 1952; flate2-backed). |
| [`std::compress::zlib`](compress_zlib.md) | zlib (RFC 1950) encoder / decoder. |
| [`std::compress::zstd`](compress_zstd.md) | Zstandard encoder / decoder (RFC 8478; libzstd-vendored). |
| [`std::context`](context.md) | Request-scoped cancellation, deadlines, and timeouts. |
| [`std::crypto::aead`](crypto_aead.md) | Authenticated encryption with associated data. |
| [`std::crypto::blake3`](crypto_blake3.md) | BLAKE3 hashing. |
| [`std::crypto::cipher`](crypto_cipher.md) | AES key handling + CBC / CTR block-cipher modes. |
| [`std::crypto::ecdsa`](crypto_ecdsa.md) | ECDSA over the NIST P-256 curve. |
| [`std::crypto::ed25519`](crypto_ed25519.md) | Ed25519 digital signatures. |
| [`std::crypto::hmac`](crypto_hmac.md) | HMAC-SHA-256 keyed MACs. |
| [`std::crypto::insecure`](crypto_insecure.md) | Legacy / broken hashes (MD5, SHA-1). Compat only - never use for new code. |
| [`std::crypto::kdf`](crypto_kdf.md) | Password-based key-derivation functions. |
| [`std::crypto::password`](crypto_password.md) | Argon2id password hashing facade: PHC-string hash / verify / re-hash policy. |
| [`std::crypto::rand`](crypto_rand.md) | Secure random bytes from the host CSPRNG. |
| [`std::crypto::sha256`](crypto_sha256.md) | SHA-256 hashing. |
| [`std::crypto::sha512`](crypto_sha512.md) | SHA-512 hashing. |
| [`std::crypto::subtle`](crypto_subtle.md) | Constant-time comparison helpers. |
| [`std::crypto::x509`](crypto_x509.md) | X.509 certificate parsing. |
| [`std::database::sql`](database_sql.md) | Driver-pluggable SQL database access. No driver ships in the box; bring your own (Postgres, MySQL, SQLite, ...) by registering one at startup. |
| [`std::encoding::ascii85`](encoding_ascii85.md) | ASCII85 / base85 encode / decode. |
| [`std::encoding::base32`](encoding_base32.md) | RFC 4648 base32 (uppercase) encode / decode. |
| [`std::encoding::base64`](encoding_base64.md) | RFC 4648 base64 encode/decode. |
| [`std::encoding::binary`](encoding_binary.md) | Big/little-endian integer packing and varint codecs. |
| [`std::encoding::csv`](encoding_csv.md) | CSV record reader and writer. |
| [`std::encoding::hex`](encoding_hex.md) | Lowercase hex encode/decode. |
| [`std::encoding::json`](encoding_json.md) | JSON parser, emitter, and derive support. |
| [`std::encoding::pem`](encoding_pem.md) | PEM block encoder and decoder. |
| [`std::encoding::toml`](encoding_toml.md) | TOML 1.0 parsing + emission. Pair with the turbofish `from_toml::<Type>` for typed decoding (struct auto-derive). |
| [`std::encoding::xml`](encoding_xml.md) | Streaming XML decoder + builder (quick-xml). |
| [`std::encoding::yaml`](encoding_yaml.md) | YAML 1.2 parser/emitter (serde_norway-backed). |
| [`std::env`](env.md) | Process environment, command-line arguments, working directory. |
| [`std::errors`](errors.md) | Error construction, wrapping, and chain traversal. |
| [`std::flag`](flag.md) | Batteries-included CLI argument parsing. |
| [`std::fmt`](fmt.md) | Formatted printing and string interpolation. |
| [`std::fs`](fs.md) | Filesystem reading, writing, and traversal (Rust std::fs shape). |
| [`std::hash::adler32`](hash_adler32.md) | Adler-32 checksums. |
| [`std::hash::crc32`](hash_crc32.md) | CRC-32 (IEEE) checksums. |
| [`std::hash::fnv`](hash_fnv.md) | FNV-1a non-cryptographic hash (32-bit, 64-bit). |
| [`std::html`](html.md) | HTML text escaping and unescaping. |
| [`std::html::template`](html_template.md) | Context-aware HTML templates with auto-escape (text/attr/URL/JS). The context classifier is heuristic - sound for typical server-rendered responses but NOT a content-security-policy substitute; sanitize untrusted HTML fragments with a dedicated sanitizer. |
| [`std::http`](http.md) | HTTP/1.1 and HTTP/2 client and server. HTTP/2 negotiates via ALPN over TLS automatically (Go-style); h2c entry points are explicit. |
| [`std::http::chunked`](http_chunked.md) | RFC 7230 §4.1 chunked transfer-encoding reader and writer. |
| [`std::http::cookie`](http_cookie.md) | RFC 6265 cookie parser and Set-Cookie builder. |
| [`std::http::csrf`](http_csrf.md) | Double-submit-cookie CSRF protection with Origin / Referer allowlist. |
| [`std::http::form`](http_form.md) | application/x-www-form-urlencoded parser and builder. |
| [`std::http::health`](http_health.md) | Liveness / readiness probes for HTTP health endpoints. |
| [`std::http::middleware`](http_middleware.md) | Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip. |
| [`std::http::multipart`](http_multipart.md) | RFC 7578 multipart/form-data streaming parser. |
| [`std::http::native_client`](http_native_client.md) | Goroutine-driven HTTP/1.1 client over std::net (no ureq, no blocking pool). |
| [`std::http::proxy`](http_proxy.md) | Reverse proxy on top of http::Client. Director-style request mutator + hop-by-hop strip + error handler. |
| [`std::http::query`](http_query.md) | Typed wrapper over URL query strings. |
| [`std::http::router`](http_router.md) | Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes. |
| [`std::http::session`](http_session.md) | Signed-cookie session store with pluggable backend trait. |
| [`std::http::sse`](http_sse.md) | Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint. |
| [`std::http::state`](http_state.md) | Handler-side dependency injection via a typed AppState. |
| [`std::http::static_files`](http_static_files.md) | Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff. |
| [`std::http::websocket`](http_websocket.md) | RFC 6455 WebSocket support. Server-side accept + send_text / send_binary / ping / pong / close. |
| [`std::http_h3`](http_h3.md) | First-party HTTP/3 server + client over QUIC (RFC 9114; quinn + h3). Each `serve` and `Client` instance owns a private tokio runtime; callers see only synchronous entry points. |
| [`std::io`](io.md) | Stream-oriented I/O abstractions. |
| [`std::iter`](iter.md) | Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc. |
| [`std::jwt`](jwt.md) | RFC 7519 sign / verify for HS256 / HS384 / HS512, ES256, and EdDSA tokens. |
| [`std::lifecycle`](lifecycle.md) | Graceful-shutdown coordinator with signal handling and sd_notify support. |
| [`std::math`](math.md) | Mathematical constants and f64 functions (Go's math package shape). |
| [`std::math::big`](math_big.md) | Arbitrary-precision integers (num-bigint). |
| [`std::math::bits`](math_bits.md) | Integer bit-manipulation operations (Go's math/bits shape). |
| [`std::math::rand`](math_rand.md) | Deterministic pseudo-random number generation. |
| [`std::metrics`](metrics.md) | Prometheus-compatible primitives (Counter, Gauge, Histogram) and a Registry rendering the standard text-exposition format. |
| [`std::mime`](mime.md) | RFC 2045 media type parsing, parameter extraction, and extension lookup. |
| [`std::net`](net.md) | TCP/UDP networking primitives. |
| [`std::net::ip`](net_ip.md) | String-level IPv4 / IPv6 parsing and classification helpers. |
| [`std::net::netip`](net_netip.md) | Typed IP-address parsing, classification, and addr:port helpers (Go's net/netip shape). |
| [`std::net::url`](net_url.md) | URL parsing, rendering, and query escaping. |
| [`std::option`](option.md) | Data-last Option combinators for pipeline chaining: map, filter, default, and_then, etc. |
| [`std::os`](os.md) | Operating-system identity and deprecated re-exports of env/process/fs. |
| [`std::os::exec`](os_exec.md) | Spawn / wait for child processes (Go's os/exec shape). |
| [`std::os::signal`](os_signal.md) | POSIX-style signal subscription (Go's os/signal shape). |
| [`std::os::user`](os_user.md) | POSIX user / group lookup. Unix-backed by `nix`; Windows falls back to env vars. |
| [`std::panic`](panic.md) | Panic / `catch_unwind` integration. |
| [`std::path`](path.md) | POSIX-style path manipulation. |
| [`std::process`](process.md) | Spawn child processes, exit the current process (Rust std::process shape). |
| [`std::regex`](regex.md) | Compiled regular expressions (Rust `regex` crate syntax; no backreferences or look-around). |
| [`std::result`](result.md) | Data-last Result combinators for pipeline chaining: map, map_err, default_with, etc. |
| [`std::runtime`](runtime.md) | Goroutine / scheduler introspection and tuning. |
| [`std::slog`](slog.md) | Structured, levelled logging. |
| [`std::strconv`](strconv.md) | Conversions between strings and primitive numeric types. |
| [`std::strings`](strings.md) | Polished `String` operations. |
| [`std::sync`](sync.md) | Synchronisation primitives beyond channels. |
| [`std::testing`](testing.md) | Assertions and sub-test harness helpers. |
| [`std::text::template`](text_template.md) | Plain-text templates (no escaping). |
| [`std::thread`](thread.md) | Native OS threads. For goroutines use the `go expr` syntax. |
| [`std::time`](time.md) | Wall-clock and monotonic time facilities. |
| [`std::tls`](tls.md) | TLS termination and TLS client dialling (rustls-backed). Wired through both http::Server::bind_and_run_tls and http::Client; mTLS / ALPN / SNI exposed. |
| [`std::trace`](trace.md) | W3C trace-context-compatible distributed tracing. Identifier types, request-scoped SpanContext, process-level Tracer, and OTLP JSON export. |
| [`std::unicode`](unicode.md) | Unicode general-category predicates, casing, normalization, and segmentation. |
| [`std::utf16`](utf16.md) | UTF-16 encoding/decoding and surrogate pair helpers. |
| [`std::utf8`](utf8.md) | UTF-8 validation and scalar decoding. |
| [`std::uuid`](uuid.md) | UUID v4 (random) and v7 (timestamp-ordered) generation, parse, and normalize. |
| [`std::validate`](validate.md) | Trait-based field validation: implement Validate, collect FieldErrors into Errors. |

