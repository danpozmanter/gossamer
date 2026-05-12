# Gossamer standard library

One page per module. Source is `crates/gossamer-std/src/`; this index is regenerated from `manifest::ALL_MODULES` by `gos doc --emit-stdlib`.

| Module | Summary |
|---|---|
| [`std::archive::tar`](archive_tar.md) | Unix tar reader and writer (USTAR / PAX-aware decode). |
| [`std::archive::zip`](archive_zip.md) | ZIP archive reader and writer. |
| [`std::bufio`](bufio.md) | Buffered readers, writers, and line scanners. |
| [`std::bytes`](bytes.md) | Byte buffers, builders, and slice helpers. |
| [`std::collections`](collections.md) | Built-in container types. |
| [`std::compress::bzip2`](compress_bzip2.md) | bzip2 encoder / decoder (BZh format). |
| [`std::compress::flate`](compress_flate.md) | Raw DEFLATE (RFC 1951) encoder / decoder. |
| [`std::compress::gzip`](compress_gzip.md) | gzip encoder / decoder (RFC 1952; flate2-backed). |
| [`std::compress::zlib`](compress_zlib.md) | zlib (RFC 1950) encoder / decoder. |
| [`std::context`](context.md) | Request-scoped cancellation, deadlines, and timeouts. |
| [`std::crypto::aead`](crypto_aead.md) | Authenticated encryption with associated data. |
| [`std::crypto::blake3`](crypto_blake3.md) | BLAKE3 hashing. |
| [`std::crypto::cipher`](crypto_cipher.md) | AES key handling + CBC / CTR block-cipher modes. |
| [`std::crypto::ecdsa`](crypto_ecdsa.md) | ECDSA over the NIST P-256 curve. |
| [`std::crypto::ed25519`](crypto_ed25519.md) | Ed25519 digital signatures. |
| [`std::crypto::hmac`](crypto_hmac.md) | HMAC-SHA-256 keyed MACs. |
| [`std::crypto::insecure`](crypto_insecure.md) | Legacy / broken hashes (MD5, SHA-1). Compat only — never use for new code. |
| [`std::crypto::kdf`](crypto_kdf.md) | Password-based key-derivation functions. |
| [`std::crypto::rand`](crypto_rand.md) | Secure random bytes from the host CSPRNG. |
| [`std::crypto::sha256`](crypto_sha256.md) | SHA-256 hashing. |
| [`std::crypto::sha512`](crypto_sha512.md) | SHA-512 hashing. |
| [`std::crypto::subtle`](crypto_subtle.md) | Constant-time comparison helpers. |
| [`std::crypto::x509`](crypto_x509.md) | X.509 certificate parsing. |
| [`std::database::sql`](database_sql.md) | Driver-pluggable SQL database access. |
| [`std::encoding::ascii85`](encoding_ascii85.md) | ASCII85 / base85 encode / decode. |
| [`std::encoding::base32`](encoding_base32.md) | RFC 4648 base32 (uppercase) encode / decode. |
| [`std::encoding::base64`](encoding_base64.md) | RFC 4648 base64 encode/decode. |
| [`std::encoding::binary`](encoding_binary.md) | Big-endian / little-endian integer packing. |
| [`std::encoding::csv`](encoding_csv.md) | CSV record reader and writer. |
| [`std::encoding::hex`](encoding_hex.md) | Lowercase hex encode/decode. |
| [`std::encoding::json`](encoding_json.md) | JSON parser, emitter, and derive support. |
| [`std::encoding::pem`](encoding_pem.md) | PEM block encoder and decoder. |
| [`std::encoding::xml`](encoding_xml.md) | Streaming XML decoder + builder (quick-xml). |
| [`std::encoding::yaml`](encoding_yaml.md) | YAML 1.2 parser/emitter (serde_yaml-backed). |
| [`std::errors`](errors.md) | Error construction, wrapping, and chain traversal. |
| [`std::flag`](flag.md) | Batteries-included CLI argument parsing. |
| [`std::fmt`](fmt.md) | Formatted printing and string interpolation. |
| [`std::fs`](fs.md) | Filesystem walking and mutation helpers. |
| [`std::hash::fnv`](hash_fnv.md) | FNV-1a non-cryptographic hash (32-bit, 64-bit). |
| [`std::html::template`](html_template.md) | Context-aware HTML templates with auto-escape. |
| [`std::http`](http.md) | HTTP/1.1 client and server. |
| [`std::http2`](http2.md) | HTTP/2 server (h2 crate over goroutine future-driver). Bounded and streaming handler shapes; ALPN-aware HTTPS dispatch via http::server::bind_and_run_tls_h2. |
| [`std::http::chunked`](http_chunked.md) | RFC 7230 §4.1 chunked transfer-encoding reader and writer. |
| [`std::http::middleware`](http_middleware.md) | Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip. |
| [`std::http::native_client`](http_native_client.md) | Goroutine-driven HTTP/1.1 client over std::net (no ureq, no blocking pool). |
| [`std::http::proxy`](http_proxy.md) | Reverse proxy on top of http::Client. Director-style request mutator + hop-by-hop strip + error handler. |
| [`std::http::router`](http_router.md) | Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes. |
| [`std::http::sse`](http_sse.md) | Server-Sent Events (text/event-stream) emitter with heartbeat ticks and retry hint. |
| [`std::http::static_files`](http_static_files.md) | Caching static-file handler: ETag, Last-Modified, byte ranges, MIME sniff. |
| [`std::http::websocket`](http_websocket.md) | RFC 6455 WebSocket support. Server-side accept + send_text / send_binary / ping / pong / close. |
| [`std::io`](io.md) | Stream-oriented I/O abstractions. |
| [`std::iter`](iter.md) | Sequence adapters over Vec<T>: map, filter, fold, zip, enumerate, chain, etc. |
| [`std::math`](math.md) | Mathematical constants and f64 functions (Go's math package shape). |
| [`std::math::big`](math_big.md) | Arbitrary-precision integers (num-bigint). |
| [`std::math::bits`](math_bits.md) | Integer bit-manipulation operations (Go's math/bits shape). |
| [`std::math::rand`](math_rand.md) | Deterministic pseudo-random number generation. |
| [`std::net`](net.md) | TCP/UDP networking primitives. |
| [`std::net::url`](net_url.md) | URL parsing, rendering, and query escaping. |
| [`std::os`](os.md) | Operating-system primitives: filesystem, env, process. |
| [`std::os::exec`](os_exec.md) | Spawn / wait for child processes (Go's os/exec shape). |
| [`std::os::signal`](os_signal.md) | POSIX-style signal subscription (Go's os/signal shape). |
| [`std::panic`](panic.md) | Panic / `catch_unwind` integration. |
| [`std::path`](path.md) | POSIX-style path manipulation. |
| [`std::path::native`](path_native.md) | Native-separator wrappers over `std::path` (backslash on Windows). |
| [`std::regex`](regex.md) | Compiled regular expressions (Rust `regex` crate syntax; no backreferences or look-around). |
| [`std::runtime`](runtime.md) | Goroutine / GC / scheduler introspection and tuning. |
| [`std::slog`](slog.md) | Structured, levelled logging. |
| [`std::sort`](sort.md) | Slice sorting and binary search. |
| [`std::strconv`](strconv.md) | Conversions between strings and primitive numeric types. |
| [`std::strings`](strings.md) | Polished `String` operations. |
| [`std::sync`](sync.md) | Synchronisation primitives beyond channels. |
| [`std::testing`](testing.md) | Assertions and sub-test harness helpers. |
| [`std::text::template`](text_template.md) | Plain-text templates (no escaping). |
| [`std::time`](time.md) | Wall-clock and monotonic time facilities. |
| [`std::tls`](tls.md) | TLS termination and TLS client dialling (rustls-backed). Wired through both http::Server::bind_and_run_tls and http::Client; mTLS / ALPN / SNI exposed. |
| [`std::unicode`](unicode.md) | Unicode character property predicates and casing operations. |
| [`std::utf16`](utf16.md) | UTF-16 encoding/decoding and surrogate pair helpers. |
| [`std::utf8`](utf8.md) | UTF-8 validation and scalar decoding. |

