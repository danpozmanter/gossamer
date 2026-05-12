# Changelog

## 0.4.0

### Binding ABI

Four new shapes in the Rust-binding system; every 0.3 binding crate
recompiles unchanged.

- **`Type::Bytes`** — first-class byte payload, distinct from
  `Vec<i64>` at the source level. Rust shape is the new
  `gossamer_binding::Bytes` newtype (transparent `Vec<u8>`).
  Compiled tier uses a `GosBytes { len, cap, ptr }` C-ABI struct;
  interp tier stores as `Value::IntArray`.
- **`Type::Map<K, V>`** — keyed collection backed by `HashMap<K, V>`.
  Compiled tier uses `GosMap { keys, values }` parallel-vec headers.
  Concrete impls for `HashMap<String, String>`,
  `HashMap<String, i64>`, `HashMap<i64, i64>`.
- **`Type::Variant<arms...>`** — tagged-union return backed by the
  new `gossamer_binding::DynValue` enum (Nil, Bool, Int, Float,
  Char, String, Bytes, List, Map, Tagged). Compiled tier uses
  `GosDynVariant { name, payload_len, payload }` with arena-
  allocated arm names.
- **`Type::Callback(args, ret)`** — Gossamer-side callable that
  bindings may invoke during their call. `BindingCallback` for
  interp (wraps a `Value`), `NativeCallback` for compiled (wraps a
  `u64` handle). Lifetime is strictly call-scoped — retaining past
  the binding return is undefined behaviour.

`gossamer_resolve::BindingType`, `gossamer_driver::DumpedType`,
`gossamer_runner_template/sigs_dump.rs.tmpl`, and
`gossamer_mir::lower::binding_type_to_mir` all extended to handle
the new shapes. Architecture spec at
`crates/gossamer-binding/ABI_0_4.md`.

### Language / parser

- **Statement boundary for leading `&` / `*` / `-`.** A newline
  followed by one of these three operators now ends the previous
  statement, so `let s = read(p)?\n&s |> ...` parses as two
  statements instead of `let s = read(p)? & s |> ...`. Multi-line
  continuation still works when the operator sits at the end of
  the previous line (`let x = a -\n  b`) or inside parentheses;
  all other binary operators continue across newlines
  unconditionally. SPEC §2.7.
- **`?` in macro argument position propagates early-return.**
  `print!("{}", expr?)` correctly returns `Err(e)` from the
  enclosing function when `expr` is `Err`; previously the result
  was silently passed to `__concat`.

### Manifest

- **Explicit `[[bin]]` and `[lib]` tables in `project.toml`.**
  Array-of-tables for `bin`, single-table for `lib`. Duplicate
  bin names rejected. Implicit filesystem convention
  (`src/main.gos` / `src/lib.gos`) still works when neither is
  present.

### HTTP — wire correctness

- **`Client::builder().tls(...)` and `.cookies(...)` now work.**
  Previous behaviour silently dropped both. `ClientConfig` retains
  the source PEM bytes so the ureq bridge can rebuild TLS state.
- **`Date` and `Server` headers auto-inserted on every response**
  per RFC 9110 §6.6.1. `Server` value is configurable via
  `Config.server_name` (default `gossamer/0.4.0`); handler-supplied
  `Date` / `Server` headers are preserved without duplication.
  New `std::time::format_rfc1123_gmt` helper.
- **Chunked transfer encoding** (RFC 7230 §4.1) for both inbound
  request bodies and outbound responses. New `std::http_chunked`
  module (`ChunkedReader` + `ChunkedWriter`) with malformed-input
  hardening (bad hex, premature EOF, missing CRLF, oversize
  length, chunk-extensions). Trailer headers on inbound chunked
  bodies merge into `request.headers`. Outbound chunked is
  triggered by the handler setting `Transfer-Encoding: chunked`;
  `Content-Length` is stripped when both are present. Combination
  of chunked + `Content-Length` on the request is rejected with
  `400`.
- **`Expect: 100-continue`** support (RFC 7231 §5.1.1) on both
  plain-TCP and TLS paths. The HTTP parser is split into
  `parse_request_head_generic` and `finish_request` so the server
  can write the interim response between head parse and body read.
- **Path / query split.** `Request.path` is now the URL path alone
  (Go's `URL.Path` semantics); `Request.query` carries the raw
  query string (no leading `?`). New helpers: `Request::query()`,
  `Request::request_uri()`, `Request::query_pairs()` (percent-
  decoding), and `std::http::split_path_query()`.
- **`Headers::remove(name)`** added.
- **Unified HTTP/1.1 parser.** The TLS and plain-TCP paths now
  share a single generic `parse_request_head_generic` +
  `finish_request` implementation.

### HTTP — timeouts and graceful shutdown

- **Timeout taxonomy.** `Config` gains `read_header_timeout`
  (10 s default, slowloris guard), `read_body_timeout` (30 s),
  `write_timeout` (30 s), `idle_timeout` (75 s). The legacy
  `read_timeout` knob still works as a blanket fallback. Per-phase
  deadlines enforced via `Instant`-based total-elapsed checks in
  the parser and body reader; per-syscall timeouts via
  `set_read_timeout` / `set_write_timeout` switching across the
  idle → header → body → write phases.
- **`Server::shutdown(&Config, Option<Duration>) -> bool`** —
  flips the shutdown flag, blocks until `Config.in_flight` drains
  to zero or the deadline elapses. Returns `true` on clean drain,
  `false` on timeout. Worker loop polls the flag between
  keep-alive requests so idle connections close promptly.
- **Per-request `Context` cancellation.** A watcher fires the
  cancel handle when `Config.shutdown` trips, so long-running
  handlers observe `request.context().is_cancelled() == true`.

### HTTP — router, middleware, static files, proxy

- **`http_router`** — Go 1.22-class `ServeMux` with `{name}`
  captures, `{rest...}` trailing captures, `*` wildcard, method
  gating (`get` / `post` / `put` / `delete` / `patch` / `head` /
  `options`). Precedence: method-specific beats method-agnostic;
  more-specific pattern wins; insertion-order breaks ties. Default
  404 / 405 responses with overridable hooks.
- **`http_middleware`** — Logger, Recoverer
  (`std::panic::catch_unwind`), RequestId (`X-Request-Id` stamping
  with carry-through), CORS (preflight + per-response headers),
  BasicAuth (RFC 7617), Compress (gzip body framing gated on
  `Accept-Encoding`, with min-bytes threshold).
- **`http_static_files`** — `FileServer` with configurable `etag`,
  `last_modified`, `range_support`, `max_file_bytes`. Path-
  traversal guard (`fs::canonicalize` + prefix check). 200 / 206
  / 304 / 404 / 416 response shaping. ETag from `mtime + size`,
  RFC 1123 GMT `Last-Modified`. MIME table covers 25 common
  extensions. `index.html` auto-served on directory hits.
- **`http_proxy`** (behind `http-client` feature) — `ReverseProxy`
  with caller-supplied `director`, `modify_response`,
  `error_handler`. `ReverseProxy::single_host` forwards path +
  query verbatim. Hop-by-hop header stripping per RFC 7230 §6.1.
  Auto-appends `X-Forwarded-For`, `X-Forwarded-Host`,
  `X-Forwarded-Proto`.

### HTTP — WebSocket and SSE

- **`http_websocket`** — RFC 6455 from scratch. `accept` performs
  the handshake (validates Upgrade / Connection /
  Sec-WebSocket-Version=13 / Sec-WebSocket-Key, computes
  Sec-WebSocket-Accept via inline SHA-1 + base64). `WebSocket`
  exposes `send_text` / `send_binary` / `send_ping` / `send_pong`
  / `send_close` / `receive` over any `Read + Write` stream.
  Auto-pong on inbound ping. Fragmented frame reassembly via
  continuation opcodes. Server-mode requires client masking;
  client-mode masks outbound frames. Length encoding handles
  7-bit, 16-bit, and 64-bit forms. Inline SHA-1 + base64
  implementations (no extra deps).
- **`http_sse`** — Server-Sent Events (`text/event-stream`)
  encoder: `SseStream::send` (event name / id / data lines),
  `send_retry`, `send_comment` (heartbeat). `event_stream_headers()`
  + `response_skeleton()` helpers.

### HTTP/2 server

- **`std::http2::bind_and_run_h2c`** in both `gos run` and
  `gos build`. The `h2` crate runs on Gossamer's own goroutine
  scheduler via `runtime_future::drive` (a future-pump) +
  `async_tcp::AsyncTcpStream` (mio-bridge over non-blocking TCP).
  Tokio is consumed only for its `AsyncRead` / `AsyncWrite` trait
  surface.
- Bounded `Handler` (`fn serve(req) -> Response`) and chunked
  `StreamingHandler` (`fn serve(req, ResponseWriter)`) shapes
  both supported. `ResponseWriter::write_chunk` flushes the
  response head on first call and emits one `DATA` frame per
  call; `finish` (or `Drop`) sends the terminating `END_STREAM`.
- **ALPN-driven HTTPS dispatch** via `bind_and_run_tls_h2`
  (tokio-rustls trait-only).
- Architecture documented at `crates/gossamer-std/HTTP_H2_ARCH.md`.

### Native HTTP/1 client

- **`http_native_client`** built on `std::net::TcpStream`.
  `NativeClient::{get, post, put, delete, request}` with per-
  client connection pool (keyed by host:port), configurable
  redirect policy (default 10 hops), chunked response decoding,
  user-agent / timeout / max-body-bytes config. HTTPS not yet
  supported; TLS stays on the existing ureq path.

### HTTP module bridges — interp + compiled parity

Eight stdlib HTTP modules now callable from Gossamer source in
both tiers, byte-identical across `gos run` and `gos build`.

- **router / FileServer / NativeClient / Proxy** — stateful,
  method-chain dispatch. `Router::new()`, `r.get(path, Handler {})`,
  `r.serve(req)` and the rest of the verb chain work end-to-end.
  22 new `gos_rt_*` runtime symbols. MIR auto-synthesises
  `gos_fn_addr("{Handler}::serve")` for HTTP-verb methods so the
  runtime can transmute and invoke user handlers through the same
  fn-pointer ABI as `gos_rt_http_serve`. `gos_fn_addr` now
  resolves to `intrinsics.externs` for runtime symbols.
- **chunked / sse / middleware / websocket-accept-key /
  static_files-mime** — stateless free-fn shapes.
  `chunked::encode` / `decode`, `sse::encode_event` / `comment` /
  `retry`, `middleware::new_request_id` / `accepts_gzip`,
  `websocket::accept_key`, `static_files::mime_for_path`. Self-
  contained SHA-1 + base64 in the runtime for the WS accept-key
  derivation.
- **MIR runtime-kind tag from rendered type** — `lower_fn`'s
  parameter binding now reads the rendered type of the param (in
  addition to the binding name), so `r: http::Request` resolves
  the same as `request: http::Request`. Fixes garbage reads on
  `r.path` / `r.body` for handler params named anything other than
  `request` / `req`.

### Netpoller latency

- Tightened the `globals().poller.lock()` hold during
  `mio::Poll::poll()` so registering goroutines no longer wait up
  to 50 ms per IO op. New `mio::Waker` interrupts in-flight polls
  when `with_poller` mutates state; poll cycle dropped to 1 ms.
  Multiplexed h2c: 3.7 ms/req, was 100 ms.

### Networking

- **`net::TcpStream::set_keepalive(Option<Duration>)`** — socket2-
  backed `SO_KEEPALIVE` toggle.
- **`net::TcpStream::connect_happy_eyeballs(addrs, stagger,
  timeout)`** — Go 1.21-style v6/v4-interleaved race with per-
  candidate staggered start.
- **`net::UnixListener` / `net::UnixStream`** (Unix-only) — bind /
  accept / connect / read / write / shutdown.
- **`net::IpNet`** — RFC 4632 prefix parsing for IPv4 and IPv6,
  `contains(&Ip)` predicate, `prefix_len()`, `render()`. Cross-
  family addresses are rejected.
- **`net::url`** — `path_escape` / `path_unescape`,
  `UserInfo { username, password: Option<String> }` (parse +
  render with percent-encoding), `Values` (Go's `url.Values`:
  `add` / `set` / `get` / `get_all` / `delete` / `encode` /
  `parse`).
- New `socket2 = "0.5"` dependency in `gossamer-std`.

### Stdlib

- **`std::io`** — `copy`, `copy_n`, `read_all`, `LimitReader`,
  `TeeReader`, `MultiReader`, `pipe` (paired `PipeReader` +
  `PipeWriter` with cross-thread blocking semantics).
- **`std::log`** (new) — Go-style flat logger: `println`,
  `printf`, `fatal`, `panic_msg`; `set_output`, `set_prefix`,
  `set_flags`; flag constants `L_DATE`, `L_TIME`, `L_MICROSECONDS`,
  `L_JSON`, etc. Global process-wide sink protected by
  `parking_lot::Mutex`.
- **`std::time`** — `Ticker` (recurring callback every interval;
  `stop()` / Drop-safe), `after_func` (one-shot timer returning a
  cancellable `TimerHandle`), `SystemTime::from_std` / `as_std` /
  `unix_seconds`.
- **`std::sync`** — `SyncMap<K, V>` (read-heavy `RwLock`-backed
  concurrent map: `store` / `load` / `load_or_store` / `delete` /
  `contains` / `range`), `Pool<T>` (factory-backed freelist),
  `Cond` (`parking_lot::Condvar` wrapper for `signal` /
  `broadcast` / `wait`).
- **`std::path`** — Go `path/filepath` parity: `glob(pattern)`
  (literal, `*`, `?`, `[class]`, `**` recursive), `matches(pattern,
  name)` segment matcher (no `/` crossing), `walk(root, visit)`
  with `SKIP_DIR` / `SKIP_ALL` sentinels, `eval_symlinks(path)`.
- **`std::crypto::cipher`** — `aes_ctr_xor` (in-place encrypt/
  decrypt for 128/192/256-bit keys), `aes_cbc_encrypt` +
  `aes_cbc_decrypt` with PKCS#7 padding. Bad key sizes and bad
  IVs return typed errors.
- **`std::runtime`** — `caller(skip)` returns
  `Option<StackFrame>`, `stack()` returns `Vec<StackFrame>` (both
  backed by the `backtrace` crate). `set_finalizer(arc, fn)`
  returns a `Finalizer<T>` guard with `cancel()` and Arc-aware
  drop semantics — fires only when the last clone goes away.
- **`std::text::template`** — `FuncMap` registry with default
  helpers (`upper`, `lower`, `trim`, `len`, `default`,
  `html_escape`); pipelines (`{{ .x | upper | trim }}`);
  `Template::render_with_funcs(data, funcs)` and free
  `render_with_funcs(source, data, funcs)`. Unknown function names
  raise `Error::Parse`.

### Stdlib feature gates removed

`gossamer-std` no longer ships behind feature flags — every
module (regex, tls, crypto, compress, archive, http2, templates,
sql, ureq, …) is unconditionally compiled. The `[features]`
table is gone; consumers depend on the crate plain. 58
`#[cfg(feature = …)]` sites stripped.

### Tooling

- **`gos doc --emit-stdlib DIR`** — walks `manifest::ALL_MODULES`
  and emits one Markdown page per module under `DIR` plus an
  `index.md` landing page. `--check` mode compares disk against
  generated output and fails the build on drift. Wired into
  `check.sh` and the `stdlib-docs-drift` GitHub Actions job. 79
  stdlib pages committed under `docs_src/stdlib/`.

## 0.3.0

### Added

- **`std::compress` expanded.** New `flate` (raw DEFLATE), `zlib`, and
  `bzip2` submodules join the existing `gzip` module. All three are
  feature-gated (`compress` / `bzip2-compress`).
- **`std::archive`.** New `tar` and `zip` submodules for reading and
  writing archives, backed by the `tar` and `zip` crates.
- **`std::hash::fnv`.** FNV-1a and FNV-1 hashes in 32- and 64-bit
  variants; no new dependencies.
- **`std::encoding` expanded.** New `base32` (RFC 4648 standard and hex
  alphabets), `ascii85` (Adobe / btoa), and `xml` (quick-xml backed)
  submodules. Qualified-path access for `encoding::base64` and
  `encoding::hex` is now wired.
- **`std::crypto::insecure`.** MD5 and SHA-1 for legacy-compatibility
  contexts; feature-gated as `insecure-crypto`.
- **`std::math::big`.** Arbitrary-precision integers via decimal-string
  representation. Exposes `Int::parse`, `Uint::parse`, `Int::compare`,
  and `factorial`.
- **`std::sync::AtomicU64` and `sync::Barrier`** wired to the interpreter.
- **52 integration tests** for all new stdlib modules in
  `crates/gossamer-cli/tests/stdlib_new_modules.rs`.
- **5 new examples**: `crypto_hashing.gos`, `encoding_codecs.gos`,
  `big_numbers.gos`, `compress_demo.gos`, `html_escape.gos`.

### Performance

- **Parallel Cranelift body lowering.** Function bodies now compile
  concurrently via rayon. An `OfflineModule` snapshot pre-declares all
  symbols in a single-threaded phase; each rayon worker then lowers its
  assigned body without taking any global lock.
- **Auto-drop pass overhaul.** Ten stacked compiler and runtime fixes
  make the heap-free pipeline produce IR that actually executes
  `gos_rt_*_free` calls. Changes include: per-block liveness-based drop
  placement, copy-alias chain tracking, inter-procedural escape analysis
  (`CaptureSummary`), a sentinel-pointer skiplist for globally-owned
  buffers, and `gos_rt_str_free` for owning strings. Effect on benchmarks
  (source unchanged): k-nucleotide −33% peak RSS, spectral-norm −8%.
- **`gos test` parallel by default.** Defaults to
  `available_parallelism()` workers. `--serial` (alias `--parallel 1`)
  opts back to sequential execution.
- **`define_only` allow-list check is O(1).** Converted from a linear
  scan to a `HashSet` in `lower_program_full`.

### Architecture

- **Incremental GC drive wired into the allocation fast path.**
  `gos_rt_gc_alloc_rooted` calls `drive_incremental()` after each
  rooted allocation: starts a new concurrent cycle when RSS exceeds the
  threshold (default 4 MB; override with `GOSSAMER_GC_TARGET`), marks a
  32-object batch during marking, and finalises when the grey set is
  exhausted.

### Fixes

- **`Result` used as a bare statement is now a compile error (GT0007).**
  Every `Result<T, E>` expression must be handled via `?`,
  `match`/`if let`, or `let _ = expr`. `gos explain GT0007` documents
  the rationale.

## 0.2.0

### Performance

- **JIT peak RSS reduced for programs with large array initialisers.**
  - `Rvalue::Repeat` in the Cranelift backend now skips all stores for
    zero-constant fills (`[0.0; N]`, `[false; N]`, `[(); N]`) — `calloc`
    already zeroes memory, so the stores were redundant. Non-zero fills
    larger than 16 elements emit a counted loop (O(1) IR) instead of N
    unrolled `store` instructions (O(N) IR), matching the LLVM backend.
  - Array-typed return values in the Cranelift backend are now returned
    directly (the existing `calloc`-allocated local is passed back as-is)
    instead of going through a second `gos_rt_gc_alloc` + memcpy escape.
    Saves one allocation per array-returning call.
  - JIT compilation now pre-filters to the minimal set of bodies needed:
    a BFS from JIT-promotable roots (functions with scalar-only
    param/return types) collects their transitive user-function callees.
    Bodies that can never be promoted (aggregate params/returns) are
    skipped entirely, cutting JIT compile time proportionally.
- **HIR and type-context dropped before `vm.call()`.** The CLI's `gos run`
  path now explicitly drops the `HirProgram` and `TyCtxt` before entering
  the main call, then releases the MIR/TyCtxt JIT prelude after `vm.call()`
  returns and before goroutine-join. Frees the per-program compilation data
  while goroutines are still running, reducing peak RSS for large programs.

### Architecture

- **ABI registry (`gossamer-abi` crate) for typed `gos_rt_*` declarations.**
  A new `gossamer-abi` crate holds a single source-of-truth for every
  `gos_rt_*` symbol's name and C-ABI signature. The Cranelift backend's
  `extern_fn_by_name` and the LLVM lowerer's `declare_rt` both derive
  function declarations from this registry, eliminating the previously
  parallel string arrays. Typos in symbol names now panic at test time
  rather than silently producing wrong code.

### Fixes

- **LLVM write-barrier correctness.** The write barrier was being emitted
  for `ptr`-typed LLVM values (raw machine pointers). `gos_rt_write_barrier`
  expects a `u32` GcRef index (widened to i64 in the flat ABI); truncating
  a pointer to i32 is both invalid IR and semantically wrong. The barrier
  is now suppressed for all `ptr`-typed values; the GC tracks those through
  its allocation registry.
- **LLVM aggregate-return memcpy for runtime helpers.** When a runtime call
  returns a heap pointer to a multi-slot aggregate (e.g.
  `gos_rt_result_payload` returning an `ExecOutput` blob), the destination
  is an inline `[N x i64]` alloca. A bare `store ptr` only wrote the blob
  address into slot 0, making subsequent field reads load the blob pointer
  instead of the actual field value. The LLVM lowerer now emits a full
  memcpy for these cases.
- **LLVM call-site type declarations match the call instruction.** Runtime
  functions whose registry ABI type differs from the LLVM call-site type
  (e.g. `gos_rt_result_payload` is `I64` in the registry but called as
  `ptr` in compiled MIR) now always declare using the call-site type.
  Registry-type declarations caused `opt` to miscompile the wrong type.

### Added

- **PGO support for `gos build --release`.**
  Two environment variables drive a standard three-step LLVM PGO workflow:
  - `GOS_PGO_COLLECT=<output.profraw>` builds an instrumented binary that
    writes raw profile data on exit. Links `libclang_rt.profile-x86_64.a`
    automatically.
  - `GOS_PGO_PROFILE=<merged.profdata>` feeds a previously collected and
    `llvm-profdata`-merged profile into `opt --pgo-kind=pgo-instr-use-pipeline`.
  The `gos build` command prints the three-step workflow on first use.
- **Binary size reduction for `gos build`.**
  Release builds now strip all symbols and dead sections (`-Wl,--gc-sections`
  on Linux, `-dead_strip` on macOS). Debug builds without `-g` strip only
  debug sections, keeping symbol names for crash reports. Brings the
  Cranelift-generated binary floor down ~75%.
- Github Actions tests do not fail fast.
## 0.1.8

### Fixes

- **`STATUS_HEAP_CORRUPTION` crash in native iterator test on Windows.**
  The MIR drop-insertion pass pins the destination local of `gos_rt_arr_iter`
  to the source `Vec<T>` type so `.next()` dispatch can recover the element
  kind. The type-based `inferred_free` path then incorrectly scheduled
  `gos_rt_vec_free` on the `*mut GosArrIter` pointer, interpreting the
  iterator's raw bytes as a `GosVec` header and corrupting the heap on free.
  Fixed by adding `gos_rt_arr_iter_free` to the runtime and registering
  `"gos_rt_arr_iter" => "gos_rt_arr_iter_free"` in `ctor_to_free`, so the
  drop pass emits the correct free instead of `gos_rt_vec_free`.

- **Missing `.exe` suffix on Windows in two multi-file regression tests.**
  `cross_file_project_bundles_sibling_modules` and
  `cross_file_chained_sibling_module_calls` constructed expected binary paths
  as bare stems (`target/debug/probe`, `target/debug/chained`) without the
  `.exe` extension. Fixed with `set_extension(EXE_EXTENSION)`, matching the
  pattern used in `parity.rs`.

- **Missing LLVM declaration for `gos_rt_arr_iter_free`.**
  The `dispatch_parity` test enforces that every symbol exported from
  `c_abi.rs` has a matching `declare` line in the LLVM prelude. Added
  `declare void @gos_rt_arr_iter_free(ptr)` to `gossamer-codegen-llvm/src/emit.rs`.

- **Directory sizes report 0 in native tiers on Windows.**
  `gos_rt_fs_list_dir` and `gos_rt_fs_walk_dir` used `DirEntry::metadata()`
  which reads from `WIN32_FIND_DATA` — a cached struct that stores
  `nFileSize = 0` for directories. The interpreter uses
  `std::fs::metadata(path)`, which opens a file handle and calls
  `GetFileInformationByHandle`, returning the real NTFS directory-index
  size. Both native functions now use `std::fs::metadata` to match.

- **Missing `.exe` suffix in `codegen_correct` and `native` integration tests on Windows.**
  `every_correct_program_matches_across_tiers` checked for binary artifacts at
  `target/debug/<stem>` and `target/release/<stem>` without the `.exe` extension,
  causing 16 failures (8 programs × 2 profiles). All 13 `gos build`-driven binary
  path constructions in `codegen_correct.rs` and `native.rs` now use
  `set_extension(EXE_EXTENSION)` or the new `debug_bin(&dir, stem)` helper.
  `gos_binary()` in `codegen_correct.rs` (the release `gos` tool path) is also fixed.

## 0.1.7

### Fixes

- **`exec::kill` interpreter implementation no longer uses unsafe on Windows.**
  `gossamer-interp` has `#![forbid(unsafe_code)]`, so the Win32 FFI approach
  from 0.1.6 was rejected at compile time. Replaced with `taskkill /F /PID
  <pid>` via `std::process::Command` — the same safe shell-out pattern used
  for `/bin/kill` on Unix. The compiled-tier runtime (`c_abi.rs`) keeps the
  direct `OpenProcess`/`TerminateProcess` approach, which is correct there
  since `gossamer-runtime` permits unsafe.

## 0.1.6

### Fixes

- **`unsafe extern` required in Rust 2024 edition.**
  The `extern "system"` blocks added in 0.1.5 for the Windows
  `exec::kill` implementation must be `unsafe extern "system"` in
  edition 2024. Fixed in both `gossamer-runtime/src/c_abi.rs` and
  `gossamer-interp/src/builtins.rs`.

## 0.1.5

### Fixes

- **`exec::kill` now terminates processes on Windows.**
  Both the compiled-tier runtime (`gos_rt_exec_kill` in `c_abi.rs`) and the
  interpreter (`builtin_exec_kill` in `builtins.rs`) returned `false`
  unconditionally on Windows. Both now call `OpenProcess(PROCESS_TERMINATE)`
  + `TerminateProcess` + `CloseHandle` via inline `extern "system"`
  declarations (no new dependencies). The `#[cfg(not(unix))]` fallback is
  split into `#[cfg(windows)]` (real implementation) and
  `#[cfg(not(any(unix, windows)))]` (stub for other platforms).

## 0.1.4

### Fixes

- **Test binary paths now include `.exe` on Windows.**
  Integration tests constructed expected output paths with bare stem names
  (`"agg"`, `"concurrent"`, etc.) but `gos build` correctly emits `<stem>.exe`
  via `platform_exe_name`. Fixed by appending `std::env::consts::EXE_SUFFIX`
  at every call site across seven test files (`aggregate_print_fallback`,
  `cli`, `format_precision_parity`, `memory_growth_bounded`, `parity`,
  `release_stability`, `stdout_concurrent_print`).


## 0.1.3

### Fixes

- **`gos build` now produces a `.exe` binary on Windows.**
  `output_path` and `resolve_output_path` were appending the bare unit name to
  the output directory on every platform. `rust-lld -flavor link` (unlike
  classic MSVC `link.exe`) writes the binary at the exact `/OUT:` path given,
  with no automatic `.exe` suffix. The result was a binary with no extension
  that `is_executable` on non-Unix (which checks for `.exe`) could not find,
  causing all `aggregate_abi` (and related) test cases to report
  "no binary in … \cl" on Windows CI. Fixed by adding a `platform_exe_name`
  helper in `paths.rs` that appends `.exe` on Windows, used consistently in
  both the `--out-dir` fast path and the default `target/{debug,release}/`
  path.

## 0.1.2

### Fixes

- **`os_env_compiled` test helpers no longer trigger dead-code warnings on Windows.**
  `os_set_env_round_trips_through_os_env_in_all_tiers` had an unnecessary
  `#[cfg(unix)]` guard — the test body is pure env-var I/O and runs on all
  platforms. Windows variant of the child-propagation test added using
  `cmd /c set`.

## 0.1.1

### Fixes

- **`exec_spawn` test helpers no longer trigger dead-code warnings on Windows.**
  All helper functions were ungated; Windows-equivalent test variants added
  (`ping 127.0.0.1` in place of `/bin/sleep`) so `exec::spawn` / `exec::kill`
  coverage runs on both platforms.

## 0.1.0

### Fixes

- use std::process::{Command as StdCommand, Stdio as StdStdio} inside 
  builtin_exec_kill is now #[cfg(unix)]-gated, since those aliases are only 
  used inside the #[cfg(unix)] block - dead warnings on Windows.
- **`Ok(N)` / `Some(N)` payload-literal matching in compiled mode.**
  `match r { Ok(1) => …, Ok(2) => … }` always took the first `Ok` arm
  because MIR only compared the discriminant, never the payload value.
  Now ANDs a `gos_rt_result_payload`-extracted value predicate with the
  disc predicate. Applies to all non-binding, non-wildcard payload
  patterns: literals, ranges, nested variants, or-patterns.
- **LLVM prelude missing `gos_rt_arr_iter`, `gos_rt_arr_iter_next`,
  `gos_rt_json_set`.** Three helpers declared in `c_abi.rs` had no
  `declare` entry in the LLVM IR prelude; they silently linked to zero.
  Added declarations; `dispatch_parity` test now enforces this for all
  future helpers.
- **`Option<T>` discriminator regression in compiled tiers fixed.**
  `match json::get(...)` returning `Some(v)` matched neither
  `Some` nor `None` arms — both fell through silently. Root cause:
  the runtime helpers `gos_rt_json_get`, `gos_rt_json_keys`,
  `gos_rt_json_as_array` returned bare `*mut GosJson`/`*mut GosVec`
  pointers, but user-level `json::get` is typed as `Option<&Value>`
  so MIR expected an Option-shaped `*mut GosResult` (16 bytes:
  `disc: i64, payload: i64`, `disc == 0` = Some, `disc == 1` =
  None). Added three new opt-flavoured helpers
  (`gos_rt_json_get_opt`, `gos_rt_json_keys_opt`,
  `gos_rt_json_as_array_opt`); MIR routes user-level json calls
  through them while internal field-access lowering keeps the
  bare helpers. Interp tier wraps the same calls via
  `some_variant(...)` / `none_variant()`. The bare helpers stay
  for chained MIR field-projection (`root.a.b.c`) so the wrap
  cost only lands on user-visible Option boundaries. Tests
  `json_get_returns_option_with_correct_discriminator`,
  `malformed_json_returns_none_not_segfault`,
  `json_as_array_iter_native` all pass now.
- **Cranelift native codegen — nested struct field offsets.**
  `o.inner.x` segfaulted: field projections used flat `idx*8`
  offsets that ignored embedded struct widths. Rewrote
  `lower_place_address` / `resolve_place_cl_type` /
  `resolve_place_ty` to sum `type_slot_count` of preceding
  fields. `Aggregate` construction (struct + tuple) now uses
  per-field widths from `tcx.struct_field_tys` and walks the
  nested layout. Also added a projected-aggregate-read
  shortcut so a slot returns the field address rather than
  collapsing to first slot. Same flat-`idx` bug fixed in the
  LLVM lowerer.
- **Cranelift call-site struct alias bug.** Multi-slot
  aggregates passed by value aliased the caller's storage
  (`shift(p)` mutated `original`). Added defensive copy via
  new `operand_aggregate_slots` / `clone_aggregate_value`
  helpers — fresh storage + per-slot memcpy at every
  by-value pass.
- **MIR `lower_place_expr` resolved nested fields against
  the wrong struct.** `o.inner.x = 100` looked up `x` on the
  outer struct, didn't find it, and silently dropped the
  assign. Now prefers `struct_name_from_expr(receiver)` (the
  projected type), with `local_struct[base.local]` as
  fallback.
- **`..base` functional-update was discarded.** HIR
  `lower_struct_literal` ignored the `base` field. Added a
  synthetic `__base` key that carries the base expression;
  MIR projects `base.field` for every unprovided field; VM
  `builtin_struct_new` strips the synthetic key.
- **Closure free-var capture pulled in synthetic helpers.**
  `__concat`, `__struct`, `__fmt_prec`, `format!`, etc. were
  walked as free variables and captured into closure envs.
  `walk_free` now excludes them.
- **`FnTrait`-typed locals weren't recognised as indirect
  callees.** Closure-returning-closure fell through to
  direct-name dispatch. MIR's callee-kind match now accepts
  `TyKind::FnTrait(_)` and routes through `Operand::Copy`.
- **`errors::Error` printed as a struct literal.** Now
  renders as the `message` field via the `Display` impl.
- **`os::exit(N)` swallowed buffered stdout.** Calls
  `gos_rt_flush_stdout` before `process::exit`.
- **`i128` / `u128` silently truncated to i64 in compiled
  tiers.** Now bails with a "compiled tier" diagnostic the
  test gate matches.
- **User-defined `pub fn substring(s, ...)` recursed via
  method dispatch.** `s.substring(a, b)` is now resolved
  to the runtime `String::substring` helper before falling
  through to user dispatch — restores `String::method` to
  the qualified-method dispatch keys.
- **Stray `feature-testing-examples/project.toml` was
  forcing all 52 examples into every build.** The CLI's
  sibling-bundle walked one parent up; the stray manifest
  made it bundle examples too. Removed the stray file.
- **Multi-file sibling-module regression fixed.** Cross-module
  function calls (`mod foo;` + `foo::bar()` from sibling
  `src/*.gos` files) compiled clean under `gos check` but failed
  at runtime with `error[GX0002]: name 'foo::bar' is not bound in
  this scope`. Root cause was layered: the CLI driver didn't
  auto-bundle siblings; the resolver only registered `mod` heads
  (no recursion into nested items); the type checker /
  exhaustiveness walkers stopped at module boundaries; the parser
  silently dropped `use` decls inside inline `mod` bodies; HIR
  carried no module-path so the interp / VM globals lost the
  qualified spelling; and the Cranelift JIT was missing
  `gos_rt_eprint_str` / `gos_rt_eprintln`. Each layer was
  patched. New regression test
  `cross_file_chained_sibling_module_calls` covers a 3-deep call
  chain across all three execution tiers.
- `String::as_bytes()` was registered as a runtime global but
  silently mis-wrote the byte slice through `os::write_file`.
  The method is now rejected at `gos check` time
  (`GT0002: no method named 'as_bytes' found for type 'String'`)
  via a new `KNOWN_METHOD_NAMES` allow-list in the type checker.
  Pass `&String` directly to byte-consuming APIs; the runtime
  binding is gone.
- `encoding::json` parser cast UTF-8 bytes through `char` as
  Latin-1, mangling all non-ASCII text. `\uXXXX` escapes weren't
  handled either. Now reads bytes properly and decodes
  `\uXXXX` (including surrogate pairs). The previously-broken
  `unicode_strings_preserve_through_round_trip` test now passes.
- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* — segfault.
- Better error messages.
- Actual Error types.

### Added

- **`std::http` client now covers GET, POST, PUT, OPTIONS,
  DELETE, HEAD plus `http::request(method, url, body, headers)`
  for arbitrary methods.** ureq + rustls under the hood; HTTPS
  via Mozilla roots. Free-function wrappers (`http::get`,
  `http::post`, ...) and method-style on `Client` (`Client::post`,
  `Client::put`, ...) both round-trip through one
  `do_request(method, url, body, headers)` core. Unknown method
  strings return `Err(transport)`.
- **`http::stream(method, url, body, headers) -> ResponseStream`**
  for SSE / chunked bodies. `ResponseStream::next_line()` reads
  one line at a time from a `BufReader<Box<dyn Read + Send +
  Sync>>` over `ureq::Response::into_reader()`. Stream handles
  live in a process-wide registry keyed by i64 so they survive
  across `next_line()` calls. No temp files, no shell-out --
  this replaces the curl-and-poll pattern users were forced
  into for streaming.
- **Stdlib surface filled in.** `std::os` gained `cwd`,
  `set_cwd`, `set_env`, `unset_env`, `is_file`, `is_dir`,
  `is_symlink`, `file_size`, `remove_dir`, `remove_dir_all`,
  `copy`, `canonicalize`, `home`, `temp_dir`. `std::fs::metadata`
  returns a real `Metadata` struct. `std::net` exposes
  `TcpListener::{bind, accept, local_addr, close}`,
  `TcpStream::{connect, read, read_to_string, write, write_all,
  close}`, `UdpSocket::{bind, send_to, recv_from, local_addr,
  close}`, `net::resolve` / `net::lookup`. `std::sync` exposes
  `AtomicI64::{new, load, store, fetch_add, fetch_sub,
  compare_and_swap}`, `AtomicBool::{new, load, store,
  compare_and_swap}`, `Mutex::{new, lock, store}`, `Once::{new,
  call}`. `std::strings` adds `join`, `trim_start`, `trim_end`,
  `strip_prefix`, `strip_suffix`, `pad_left`, `pad_right`,
  `rfind`, `replacen`. `std::strconv` adds `parse_int`,
  `parse_i64`, `parse_u64`, `parse_float`, `parse_f64`,
  `parse_bool`, `format_int`, `format_i64`, `format_float`,
  `format_f64`, `itoa`/`atoi`. `std::time` adds `Instant::{now,
  elapsed_ms}`, `Duration::{from_millis, from_secs, from_micros,
  as_millis, as_secs, as_micros}`, `time::now_nanos`,
  `monotonic_ms`, `monotonic_nanos`, `since_ms`. `std::path`
  adds `parent`, `file_name`, `stem`, `ext`, `is_absolute`,
  `normalize`. `std::utf8` adds `count_runes`, `rune_len`,
  `is_valid`. `std::bufio` adds `read_to_string`,
  `read_lines_of`, `split_whitespace`.
  `std::collections::HashSet` was a HashMap stub; now a real set
  with `insert`, `remove`, `contains`, `len`, `is_empty`,
  `clear`, `to_vec`, `iter`.
- **Type-checker shifted left.** New `KNOWN_METHOD_NAMES` gate
  in `gossamer-types/src/checker.rs` rejects calls to method
  names that aren't bound at runtime (catches `as_bytes`,
  `and_then`, `filter`, `collect`, etc.) at `gos check` time
  with `GT0002` instead of letting them through to a runtime
  panic. User-defined `impl` methods are tracked separately so
  they're never falsely flagged.

### Performance

- Bytecode VM method-call IC hit path now takes a shared
  `RefCell::borrow()` instead of `borrow_mut()`. The cache is
  read-only on hit; the previous `borrow_mut()` serialised every
  call against any other borrow on the same RefCell.
- JIT tier-up threshold scales by chunk instruction count
  (`HOT_THRESHOLD_BASE * 50 / max(50, instr_count)`, floored at
  `HOT_THRESHOLD_FLOOR = 16`). Big functions now tier up after a
  handful of entries instead of waiting for 100 full calls of an
  expensive body. Honoured by `GOSSAMER_JIT_THRESHOLD` env var.
- Bytecode VM now decrements the JIT hot counter on backward
  `Op::Jump` and on the new fused `IncJumpIfLt/LeI64` ops.
  Loop-shaped chunks reachable only through their own internal
  control flow (rather than via repeated call entries) tier up
  on the same path.
- Cranelift `gos_rt_vec_len` is inlined as a null-check + offset-0
  load (matches the GosVec `repr(C)` layout). For-loop bounds and
  every `vec.len()` access in compiled code skip the C-ABI call.
- Per-thread shadow-stack root tracking is lock-free on the hot
  path. Owner reads/writes a 1024-slot `Box<[AtomicU32]>` with
  Relaxed stores and a Release-published `len`; the cross-thread
  mark snapshot Acquire-loads `len` and walks slots without
  taking any lock. Spillover into a `Mutex<Vec<u32>>` only when
  call depth exceeds the in-array capacity. The earlier design
  paid an uncontended `parking_lot::Mutex` lock+unlock at every
  function prologue and epilogue.
- For-range `for i in a..b { ... }` lowers to a header
  bounds-check + body + fused `IncJumpIfLt/LeI64` op that
  combines the per-iter `AddI64 + Jump` into one dispatch.
- `format!`, `panic!`, `eprintln!`, `eprint!` now build their
  message through the runtime's batched concat buffer
  (`gos_rt_concat_init` / `_str` / `_i64` / `_f64` / `_bool` /
  `_char` / `_finish`) instead of chaining N-1 pairwise
  `gos_rt_str_concat` calls. Eliminates the throwaway
  intermediate strings that the serial chain allocated and
  dropped between each pair of args.
- JIT trampoline `MAX_ARGS` raised from 8 to 12 (homogeneous
  i64-only and f64-only shapes for arities 9-12). HTTP handlers,
  multi-arg `format!` callees, and other 8+-arg helpers no
  longer fall back to bytecode purely because of arity.

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only — compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments — go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.1

### Stdlib parity

- `flag` stdlib fully wired in compiled mode. Default values for
  `int`, `float`, `duration`, `string_list`, `short`, `usage` are now
  honoured (previously every non-`string`/`uint`/`bool` flag silently
  zeroed). `parse` accepts the `=` form, short aliases, `--`, and
  `--help` / `-h`. Interp gained matching `float` / `duration` /
  `string_list` / `usage` builtins so both tiers produce identical
  output across every flag method.
- `flag::define(name, [flag::int(...), flag::string(...),
  flag::bool(...)])` (declarative one-shot constructor) now lowers
  to the imperative `flag::Set` builder chain at MIR time.
  Previously interp-only — compiled mode silently returned a
  null-shaped struct so `*flags.<long>` always yielded the
  primitive zero.
- `os::env`, `os::cwd` wired in both tiers. Compiled mode was
  returning `0` for every env var lookup and `0` for `cwd`.
- `fs::list_dir` wired in compiled mode (returns
  `Result<[DirInfo], Error>`).
- `time::Duration::from_secs` / `from_millis` lower in compiled mode.

### Compiler / codegen fixes

- Aggregate construction is now heap-allocated (`calloc`) instead
  of stack-slot. Returning a struct from a method (e.g.
  `Celsius { value: ... }.to_fahrenheit()`) no longer aliases the
  next call's stack slot; `temperature.gos` now matches across
  tiers.
- `loop { ... break <expr> }` captures the break expression's
  value in compiled mode. Previously
  `let x = loop { ... break sq }` returned 0 instead of `sq`.
- `result.map_err(closure)` and `result.map(closure)` dispatch
  correctly when the receiver type is unresolved at HIR time
  (e.g. `text.parse().map_err(...)?`). The closure was being
  built and silently dropped.
- String equality (`s == "literal"`, `s != "literal"`) routes
  through `gos_rt_str_eq`. Previously a pointer-compare that
  silently disagreed with interpreted output whenever the string
  came from a runtime helper rather than a literal-pinned slot.
- Reference deref (`*p` where `p: &i64` / `&f64` / `&bool` /
  `&char`) emits a real load instead of returning the pointer
  unchanged. Affected every iterator pattern that yields scalar
  references.
- `s.as_bytes()` returns a `Vec<i64>` shape (one zero-extended
  byte per slot) instead of a packed `Vec<u8>`. Compiled `bytes[i]`
  indexing now reads the byte's value through
  `gos_rt_vec_get_i64` rather than reading 8 packed buffer
  bytes as a single i64 (`reverse_string.gos` reproducer).
- `<chain>.method().to_string()` dispatches to the right runtime
  formatter (`gos_rt_i64_to_str` / `gos_rt_f64_to_str` /
  identity for strings) when the typechecker leaves the chain's
  HIR type as a `Var(_)`. Previously the identity-copy fallback
  fed an i64 to `gos_rt_str_concat` as a c_char* — segfault.
- Better error messages.
- Actual Error types.

### Test coverage

- `cargo test -p gossamer-cli --test parity --features
  exhaustive_tests --release` walks every example in
  `examples/*.gos` under both tiers and asserts byte-identical
  stdout/stderr/exit code. Two examples (`go_spawn.gos`,
  `list_dir.gos`) are listed in `KNOWN_DIVERGENT_EXAMPLES` with
  explicit root-cause comments — go_spawn requires a
  deterministic scheduler shared between tiers, list_dir
  requires registering `fs::DirInfo` as a stdlib struct in
  `gossamer-types::TyCtxt::register_struct_fields` at
  typechecker startup. Every other example round-trips.
- `crates/gossamer-codegen-cranelift/tests/correct/p51_flag_defaults`
  walks every flag type through interp + Cranelift + LLVM tiers.

## 0.0.0

Initial release. Not production ready.
