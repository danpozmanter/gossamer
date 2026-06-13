# HTTP Client/Server Gap Fixes (0.13.0) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.
>
> **Git policy override:** NO commits at any step. The repo carries uncommitted 0.13.0 work; these changes join it. The user commits when they choose. Do NOT use a worktree — a worktree would check out HEAD (0.12.0) and lose the in-flight 0.13.0 tree.

**Goal:** Close the five locurlfwd-migration gaps in `std::http` — client response headers, honored server response headers, selectable redirect policy, binary body fidelity, opt-in byte streaming — plus the discovered P0 (`http::request` is VM-only) and the discovered content-type tier divergence, all working identically on VM / Cranelift / LLVM.

**Architecture:** One HTTP engine (ureq, already a dependency of BOTH `gossamer-std` and `gossamer-runtime`) backs every client entry point on every tier; the hand-rolled TCP GET paths are deleted. New Gossamer surface is table-driven: each new name/field follows the established 5-place pattern (interp builtin → `gossamer-abi` registry → MIR lowering table → Cranelift name/sig/JIT tables → runtime `gos_rt_*` shim). LLVM lowering flows through `emit_named_call` automatically once the registry entry exists.

**Tech Stack:** Rust workspace (edition 2024), ureq 3 (rustls), existing `GosVec`/i128-Result C-ABI conventions.

**The mirror recipe** (used by several tasks): to wire a new builtin name exactly like an existing one, run `grep -rn "<existing-name>" crates/ --include="*.rs" | grep -v worktree | grep -v target` and add the new name beside the existing one in every table that appears. For HTTP names the canonical exemplar is `gos_rt_http_stream` / `http::stream`, which appears in:
- `crates/gossamer-interp/src/builtins.rs` (registration in `install_http_builtins`)
- `crates/gossamer-resolve/src/stdlib_exports.rs` (Gossamer-level name)
- `crates/gossamer-mir/src/lower/builder/stdlib_free.rs` (free-fn → shim mapping + types)
- `crates/gossamer-abi/src/registry.rs` (`rt!` entry)
- `crates/gossamer-codegen-cranelift/src/native/name_lookup.rs` (symbol list)
- `crates/gossamer-codegen-cranelift/src/native/lowering_calls.rs` (signature)
- `crates/gossamer-codegen-cranelift/src/jit.rs` (fn-pointer table)
- `crates/gossamer-runtime/src/c_abi/symbol_table.rs` (link-time symbol table)
- `crates/gossamer-runtime/src/c_abi/http_client.rs` (shim implementation)

Field accessors on opaque types additionally touch `crates/gossamer-mir/src/lower/builder/expr_field.rs` (the `("http::Response", "<field>") => ("gos_rt_...", <ty>)` table, two match sites: ~lines 236-270 and 436-470).

Validation loop after every task: `cargo build -p <touched crates> && cargo clippy -p <touched crates> -- -D warnings && cargo test -p <touched crates>`. Full-workspace gates run in Task 12.

---

### Task 1: Fix the content-type tier divergence + extend GosHttpResponse

Pre-existing bug: compiled `Response::text(200, "ok")` emits `Content-Type: application/json` (the `extract_response_into` default), interp emits `text/plain; charset=utf-8`. Root cause: `GosHttpResponse` doesn't carry a content type. Fix it there, and add the `stream_handle` field Task 9 needs so the struct changes once.

**Files:**
- Modify: `crates/gossamer-runtime/src/c_abi/http_client.rs` (GosHttpResponse ~line 62, `gos_rt_http_response_text_new` ~496, `gos_rt_http_response_json_new` ~520; also delete the stale "avoid pulling a TLS stack" header comment ~lines 24-29 — ureq is a dependency)
- Modify: `crates/gossamer-runtime/src/c_abi/http_server.rs` (`extract_response_into` ~580-625)
- Test: `crates/gossamer-std/tests/http_server.rs` pattern is for std; for the runtime use the existing c_abi unit-test module in `http_server.rs` (or add `#[cfg(test)] mod tests` at file bottom if absent)

- [ ] **Step 1: Write the failing test** — in the runtime crate, a unit test that builds a response via `gos_rt_http_response_text_new(200, c"ok".as_ptr())`, runs `extract_response_into`, and asserts the rendered bytes contain `content-type: text/plain; charset=utf-8` (case-insensitive). Add a sibling asserting `gos_rt_http_response_json_new` yields `application/json`.
- [ ] **Step 2: Run it, confirm both fail** (text case currently renders application/json). `cargo test -p gossamer-runtime content_type`
- [ ] **Step 3: Implement**:

```rust
pub struct GosHttpResponse {
    pub status: i64,
    pub body: SyncRawPtr<c_char>,
    pub headers: Vec<(String, String)>,
    pub body_bytes: Option<Vec<u8>>,
    /// Content type recorded by the constructor; used by the server
    /// writer only when `headers` carries no explicit content-type.
    pub content_type: String,
    /// Stream-registry handle for streamed bodies; -1 = buffered.
    pub stream_handle: i64,
}
```

`text_new` sets `content_type: "text/plain; charset=utf-8".to_string(), stream_handle: -1`; give `json_new` its own constructor body (no longer a passthrough) with `application/json`. Update every other `GosHttpResponse { .. }` literal in the crate (`grep -n "GosHttpResponse {" crates/gossamer-runtime/src/`) — client-side response literals set `content_type` from the response's own content-type header (empty string if none) and `stream_handle: -1`. In `extract_response_into`, replace the hardcoded `application/json` default with: explicit `headers` entry wins, else `response.content_type` if non-empty, else `text/plain; charset=utf-8` (matching the interp default in `value_to_response`).
- [ ] **Step 4: Tests pass**: `cargo test -p gossamer-runtime content_type`; then `cargo clippy -p gossamer-runtime -- -D warnings`.

### Task 2: Client `Response.headers` field — gap 1

**Files:**
- Modify: `crates/gossamer-interp/src/http_client_builtins.rs` (`lift_response` ~357)
- Modify: `crates/gossamer-mir/src/lower/builder/expr_field.rs`, `crates/gossamer-abi/src/registry.rs`, `crates/gossamer-codegen-cranelift/src/native/{name_lookup.rs,lowering_calls.rs}`, `crates/gossamer-codegen-cranelift/src/jit.rs`, `crates/gossamer-runtime/src/c_abi/{http_client.rs,symbol_table.rs}`
- Test: interp unit test beside existing ones in `http_client_builtins.rs`; compiled-tier coverage lands in the Task 10 fixture

- [ ] **Step 1: Failing interp test** — call `lift_response` on a `gossamer_std::http::Response` with two headers and assert the lifted struct has a `headers` field that is an Array of 2 tuple values.
- [ ] **Step 2: Implement interp side** — in `lift_response`, after `location`, append:

```rust
let headers: Vec<Value> = resp
    .headers
    .iter()
    .map(|(name, value)| {
        Value::Tuple(Arc::new(vec![
            Value::String(SmolStr::from(name.to_string())),
            Value::String(SmolStr::from(value.to_string())),
        ]))
    })
    .collect();
fields.push((Ident::new("headers"), Value::Array(Arc::new(headers))));
```

(Refactor `lift_response` to build `fields` as a `mut Vec` first.) The legacy `http_get_plain` / `http_get_tls` field lists are deleted entirely by Task 4 — do not extend them here.
- [ ] **Step 3: Implement the shim** in `http_client.rs`. Build the tuple-vec exactly the way `gos_rt_http_get` *reads* one (elem_bytes=16, two heap c-string pointers per slot); copy the allocation pattern from the existing tuple-vec-producing shim found via `grep -rn "elem_bytes = 16\|elem_bytes: 16" crates/gossamer-runtime/src/c_abi/ | grep -v worktree` (the HashMap `iter()` materializer). Signature:

```rust
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_headers(
    resp: *const GosHttpResponse,
) -> *mut crate::c_abi::vec::GosVec
```

- [ ] **Step 4: Wire the field** — `expr_field.rs` both match sites: `("http::Response", "headers") => Some(("gos_rt_http_response_headers", <Vec<(String,String)> ty as in report>))`; then the mirror recipe on `gos_rt_http_response_raw_bytes` for registry/name_lookup/lowering_calls (`(&[ptr_ty], Some(ptr_ty))`)/jit/symbol_table.
- [ ] **Step 5: Verify**: `cargo test -p gossamer-interp lift_response && cargo build -p gossamer-codegen-cranelift -p gossamer-codegen-llvm -p gossamer-runtime && cargo clippy -p gossamer-runtime -p gossamer-interp -p gossamer-mir -- -D warnings`

### Task 3: Native `http::request` + `http::request_bytes` — the P0 and gap 5 (upload half)

**Files:**
- Create shim in: `crates/gossamer-runtime/src/c_abi/http_client.rs`
- Modify: `crates/gossamer-interp/src/http_client_builtins.rs` (new `builtin_http_request_bytes`; delete the stale lines-17-26 "no native runtime helpers" comment), `crates/gossamer-interp/src/builtins.rs` (register `http::request_bytes`), `crates/gossamer-resolve/src/stdlib_exports.rs`, `crates/gossamer-mir/src/lower/builder/stdlib_free.rs`, plus the mirror-recipe tables
- Test: interp test for `request_bytes` arg decoding; compiled round-trip in Task 10 fixture

- [ ] **Step 1: Shim.** One shim serves both spellings:

```rust
/// `http::request` / `http::request_bytes` — full-method client entry
/// for compiled tiers. Body may be null (no body). Returns
/// Result<*mut GosHttpResponse> packed per the i128 convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request(
    method: *const c_char,
    url: *const c_char,
    body: *const crate::c_abi::vec::GosVec,
    headers: *const crate::c_abi::vec::GosVec,
) -> i128
```

Implementation: decode method/url c-strings; decode `headers` with the same tuple-vec walk `gos_rt_http_get` uses; decode `body` as a u8 GosVec into `Option<Vec<u8>>` (null or len 0 → None). Build the ureq agent exactly as `gos_rt_http_stream` does (same TLS roots, cookies, 30s timeout, `gossamer/{CARGO_PKG_VERSION}` UA, max 10 redirects) so defaults match `gossamer_std::http::Client::new()`. On success allocate a `GosHttpResponse` (status, body c-string via lossy copy, `body_bytes: Some(raw)`, full `headers`, `content_type` from header, `stream_handle: -1`) and return `gos_rt_result_new(0, ptr as i64)`; on transport error return disc 1 with a heap error string per the existing `gos_rt_http_get` error path.
- [ ] **Step 2: Wire `http::request`** via the mirror recipe on `http::stream`/`gos_rt_http_stream`: stdlib_free.rs maps `http::request(String, String, String, Vec<(String,String)>) -> Result<Response, String>` to the shim (String body lowers to a u8 GosVec of its bytes — mirror however stdlib_free passes a String arg to a `(Ptr)` byte-vec param elsewhere, e.g. the compress or encoding entries); registry `rt!("gos_rt_http_request", (Ptr, Ptr, Ptr, Ptr) -> I128, Cranelift, "...")`; cranelift sig `(&[ptr_ty, ptr_ty, ptr_ty, ptr_ty], Some(types::I128))` — copy the I128-returning row shape from `gos_rt_http_get`; jit + symbol_table rows.
- [ ] **Step 3: `http::request_bytes`** — same shim, body typed `[u8]`:
  - stdlib_free.rs: `http::request_bytes(String, String, Vec<u8>, Vec<(String,String)>) -> Result<Response, String>` → `gos_rt_http_request`.
  - stdlib_exports.rs: add `"http::request_bytes"` beside `"http::request"`.
  - Interp builtin:

```rust
/// `http::request_bytes(method, url, body: [u8], headers) -> Result<Response, String>`.
pub(crate) fn builtin_http_request_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let method_str = args.first().and_then(as_str).unwrap_or("");
    let Some(method) = Method::parse(method_str) else {
        return Ok(crate::builtins::err_variant(format!(
            "http::request_bytes: unknown method `{method_str}`"
        )));
    };
    let url = args.get(1).and_then(as_str).unwrap_or("");
    let body: Vec<u8> = match args.get(2) {
        Some(Value::Array(items)) => items
            .iter()
            .filter_map(|b| match b {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        _ => Vec::new(),
    };
    let header_pairs = extract_header_pairs(args.get(3));
    let headers = header_refs(&header_pairs);
    let body_opt = if body.is_empty() { None } else { Some(body.as_slice()) };
    let client = StdClient::new();
    match client.do_request(method, url, body_opt, &headers) {
        Ok(resp) => Ok(crate::builtins::ok_variant(lift_response(resp))),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}
```

  - Register in `install_http_builtins` beside `http::request`.
- [ ] **Step 4: Verify** interp test (`request_bytes` with `[104,105]` body against a loopback `TcpListener` echo per the `http_end_to_end.rs` pattern), then `cargo build` on mir/cranelift/llvm/runtime crates + clippy. Also confirm the stdlib-export drift test still passes: `cargo test -p gossamer-cli stdlib_export_drift`.

### Task 4: One engine — delete the hand-rolled GET paths

`http_get_plain` / `http_get_tls` (interp) hardcode status 200 on TLS and parse HTTP by hand; `gos_rt_http_get` (runtime) hand-rolls redirects for bare GETs only. All become thin calls over the ureq engine, eliminating the redirect/status/header divergences.

**Files:**
- Modify: `crates/gossamer-interp/src/http_client_builtins.rs` — route the `http::get` builtin through `StdClient::new().do_request(Method::Get, ...)` + `lift_response`; delete `http_get_plain`, `http_get_tls`, `absolute_redirect`, `parse_http_url` and their callers.
- Modify: `crates/gossamer-runtime/src/c_abi/http_client.rs` — `gos_rt_http_get` body becomes a call into the same ureq request helper Task 3 added (method GET, no body); delete `http_request_ureq`, `http_get_follow_redirects`, `http_get_tls`, `http_get_plain`, `absolute_redirect` and the bespoke parsing they used.

- [ ] **Step 1:** Interp rewrite + dead-code deletion; run `cargo test -p gossamer-interp`.
- [ ] **Step 2:** Runtime rewrite + dead-code deletion; `cargo test -p gossamer-runtime && cargo clippy -p gossamer-runtime -p gossamer-interp -- -D warnings`.
- [ ] **Step 3:** Behavior check — existing http end-to-end tests still green: `cargo test -p gossamer-interp --test http_end_to_end`.

### Task 5: Server `Request.raw_body` — gap 5 (inbound half)

**Files:**
- Modify: `crates/gossamer-interp/src/builtins.rs` (`request_to_value` ~2575-2619)
- Modify: `crates/gossamer-runtime/src/c_abi/http_server.rs` or `http_client.rs` (beside `gos_rt_http_request_body_str`), `expr_field.rs`, + mirror-recipe tables
- Test: extend `crates/gossamer-interp/tests/http_end_to_end.rs`

- [ ] **Step 1: Failing test** — POST a body containing byte 0xFF to an interp-hosted handler that returns `format!("{}", request.raw_body.len())`; assert the length equals the sent byte count (the lossy `body` field would have inflated it to a 3-byte replacement char).
- [ ] **Step 2: Interp** — in `request_to_value`, keep `body` (lossy, compatibility) and add:

```rust
let raw_body: Vec<Value> = request.body.iter().map(|b| Value::Int(i64::from(*b))).collect();
fields.push((Ident::new("raw_body"), Value::Array(Arc::new(raw_body))));
```

- [ ] **Step 3: Compiled** — shim `gos_rt_http_request_raw_body(req: *const GosHttpRequest) -> *mut GosVec` returning a u8 GosVec of the body bytes. CRITICAL: `parse_request_into` stores the raw buffer for lazy parsing — locate how `gos_rt_http_request_body_str` finds the body slice (after the `\r\n\r\n` split) and reuse that exact resolution, minus the lossy conversion. Field entry `("http::Request", "raw_body")` in both `expr_field.rs` sites with `Vec<u8>` type (copy the `raw_bytes` row); mirror recipe for the tables.
- [ ] **Step 4: Verify** — interp test green; build + clippy on touched crates.

### Task 6: Honored server response headers + `Response.with_header` — gap 3

**Files:**
- Modify: `crates/gossamer-interp/src/builtins.rs` (`value_to_response` ~2621-2676; new `builtin_http_response_with_header`; register `Response::with_header` beside `Response::text` in `install_http_builtins`)
- Modify: `crates/gossamer-runtime/src/c_abi/http_client.rs` (new chainable shim), MIR method-dispatch (find via `grep -rn "gos_rt_http_response_text_new" crates/gossamer-mir/ | grep -v worktree` — add `Response::with_header` beside `Response::text` in the same qualified-call table), + mirror-recipe tables
- Test: interp unit tests beside `value_to_response`

- [ ] **Step 1: Failing tests** — (a) `value_to_response` on a struct with `headers: [("x-custom","1"), ("content-type","text/csv")]` yields both headers and does NOT override the explicit content-type; (b) `with_header` on a `Response::text` struct returns a struct whose `headers` field contains the pair.
- [ ] **Step 2: `value_to_response`** — replace the silent `_ => {}` for `headers`:

```rust
"headers" => {
    if let Value::Array(items) = v {
        for item in items.iter() {
            if let Value::Tuple(t) = item {
                if t.len() >= 2 {
                    if let (Some(name), Some(val)) = (as_str(&t[0]), as_str(&t[1])) {
                        explicit_headers.push((name.to_string(), val.to_string()));
                    }
                }
            }
        }
    }
}
```

then at response build: insert all `explicit_headers`; insert the `content_type` default ONLY if no explicit `content-type` (case-insensitive) was provided; keep the computed content-length.
- [ ] **Step 3: Interp `with_header`** — rebuild the receiver struct, appending `(name, value)` to its `headers` array field (creating the field if missing), and return it. Chainable because it returns the new struct.
- [ ] **Step 4: Compiled shim**:

```rust
/// Chainable header attach for compiled `resp.with_header(name, value)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_with_header(
    resp: *mut GosHttpResponse,
    name: *const c_char,
    value: *const c_char,
) -> *mut GosHttpResponse
```

Body: same replace-then-push logic as `gos_rt_http_response_set_header` (~603-626), returning `resp`. Wire `Response::with_header` in the MIR qualified-call table; mirror recipe (`(&[ptr_ty, ptr_ty, ptr_ty], Some(ptr_ty))`).
- [ ] **Step 5: Verify** — interp tests green; build + clippy on interp/mir/cranelift/runtime.

### Task 7: Redirect policy — `Client::builder()` — gap 4

Rust-side `ClientBuilder` (http.rs:1905-1965) already has `max_redirects`/`timeout`. This task exposes it to Gossamer on all tiers.

Surface: `http::Client::builder() -> ClientBuilder`; `.max_redirects(n) -> ClientBuilder`; `.timeout_ms(n) -> ClientBuilder`; `.build() -> Client`; `client.request(method, url, body, headers) -> Result<Response, String>` and `client.request_bytes(...)`. `max_redirects(0)` = never follow (raw 3xx back — the proxy-correct mode).

**Files:**
- Modify: `crates/gossamer-interp/src/http_client_builtins.rs` (builder/Client as plain struct Values carrying `max_redirects`/`timeout_ms` ints; `client.request{_bytes}` builds `StdClient::builder().max_redirects(..).timeout(..)` then `do_request`), registration in `builtins.rs`, `stdlib_exports.rs`
- Modify: `crates/gossamer-runtime/src/c_abi/http_client.rs` — extend `GosHttpClient` with `max_redirects: u32, timeout_ms: u64` (defaults 10 / 30_000); shims `gos_rt_http_client_builder_new() -> ptr`, `gos_rt_http_client_builder_max_redirects(b, i64) -> ptr`, `gos_rt_http_client_builder_timeout_ms(b, i64) -> ptr`, `gos_rt_http_client_builder_build(b) -> ptr` (returns a configured `GosHttpClient`), `gos_rt_http_client_request(client, method, url, body GosVec, headers GosVec) -> i128` (Task 3's request helper parameterized by the client's config)
- Modify: MIR qualified-call/method tables + mirror-recipe tables for the five shims
- Test: `crates/gossamer-interp/tests/http_end_to_end.rs` — loopback redirect chain

- [ ] **Step 1: Failing test** — loopback server replies `302 Location: /two` on `/one` and `200 ok` on `/two`. Assert: default `http::request` lands on 200; `Client::builder().max_redirects(0).build().request(...)` returns status 302 with the `location` field set.
- [ ] **Step 2: Interp implementation** (builder = `Value::struct_("ClientBuilder", [("max_redirects", Int(10)), ("timeout_ms", Int(30000))])`; each setter rebuilds; `build` re-tags as `"Client"`; `client.request` reads the fields and uses `StdClient::builder()`).
- [ ] **Step 3: Compiled implementation** — the five shims + ureq agent built from the client's stored config; wire names via mirror recipe. `Client::builder` and methods join the MIR table beside the existing `Client::get`/`Client::post` entries (find with `grep -rn "gos_rt_http_client_post" crates/gossamer-mir/ | grep -v worktree`).
- [ ] **Step 4: Verify** — redirect test green on interp; build + clippy everywhere touched; `cargo test -p gossamer-cli stdlib_export_drift`.

### Task 8: `ResponseStream.next_chunk` — gap 6 (client half)

**Files:**
- Modify: `crates/gossamer-std/src/http.rs` (`StreamResponse` ~1793-1831)
- Modify: `crates/gossamer-interp/src/http_client_builtins.rs` (builtin beside `builtin_response_stream_next_line`), registration, `crates/gossamer-runtime/src/c_abi/http_client.rs` (shim beside `gos_rt_http_stream_next_line` ~905-938), + mirror-recipe tables
- Test: `crates/gossamer-std/src/http.rs` `#[cfg(test)]` (StreamResponse over an in-memory reader)

- [ ] **Step 1: Failing std test** — construct a `StreamResponse` over a `Cursor` of 10 bytes (add a `#[cfg(test)]` constructor if none exists), call `next_chunk(4)` thrice: `Some(4)`, `Some(4)`, `Some(2)` byte chunks, then `None`.
- [ ] **Step 2: Std implementation**:

```rust
/// Next raw chunk of the body, at most `max_bytes` long; `None` at EOF.
pub fn next_chunk(&mut self, max_bytes: usize) -> Result<Option<Vec<u8>>, ClientError> {
    use std::io::Read;
    let cap = max_bytes.clamp(1, 1 << 20);
    let mut buf = vec![0u8; cap];
    match self.reader.read(&mut buf) {
        Ok(0) => Ok(None),
        Ok(n) => {
            buf.truncate(n);
            Ok(Some(buf))
        }
        Err(e) => Err(ClientError::Io(e.to_string())),
    }
}
```

- [ ] **Step 3: Interp builtin** `ResponseStream::next_chunk(rs, max) -> Option<[u8]>` via the interp `STREAM_REGISTRY` (mirror `builtin_response_stream_next_line`; map bytes to `Value::Array` of ints; `Ok(None)`/`Err` → none_variant).
- [ ] **Step 4: Compiled shim** `gos_rt_http_stream_next_chunk(rs: *const i64, max_bytes: i64) -> i128` — handle from slot 0, registry lookup, read into a u8 GosVec, pack `Some` as disc 0 + vec ptr, EOF/error as disc 1 (mirror `next_line`'s packing exactly). Mirror recipe for tables; MIR method entry beside `ResponseStream::next_line`.
- [ ] **Step 5: Verify** — std test green; build + clippy.

### Task 9: Streamed server responses — `Response::stream` — gap 6 (server half)

Proxy shape: `Ok(http::Response::stream(upstream.status, upstream.content_type, upstream))` — the server drains the upstream `ResponseStream` to the client with chunked transfer encoding instead of buffering.

**Files:**
- Modify: `crates/gossamer-std/src/http.rs` — `Response` gains `pub body_stream: Option<BodyStream>`; new `pub struct BodyStream(pub Box<dyn std::io::Read + Send>)` with a one-line `///` doc and a `Debug` impl printing `"BodyStream(..)"`. Update every in-repo `Response { ... }` literal (`grep -rn "Response {" crates/ --include="*.rs" | grep -v worktree | grep -v GosHttp`) to add `body_stream: None`. In `server::run`'s write path: if `body_stream` is `Some`, write status line + headers + `Transfer-Encoding: chunked` (and NO content-length), then drain the reader in 8 KiB chunks writing `{len:x}\r\n{bytes}\r\n` frames and the terminal `0\r\n\r\n` frame.
- Modify: `crates/gossamer-interp/src/builtins.rs` — builtin `Response::stream(status, content_type, rs) -> Response` builds a struct carrying `status`, `content_type`, and `__stream_handle` (the interp registry handle read from the `ResponseStream` struct via the existing `handle_field` helper). `value_to_response` on seeing `__stream_handle`: look up the interp `STREAM_REGISTRY` arc and set `body_stream: Some(BodyStream(Box::new(adapter)))` where the adapter implements `Read` by locking the arc and delegating (add `impl Read` plumbing via a small `StreamBody(Arc<parking_lot::Mutex<StreamResponse>>)` adapter in gossamer-interp; `StreamResponse` needs `pub fn read_raw(&mut self, buf: &mut [u8]) -> std::io::Result<usize>` in gossamer-std delegating to `self.reader.read(buf)`).
- Modify: `crates/gossamer-runtime/src/c_abi/http_client.rs` — shim `gos_rt_http_response_stream_new(status: i64, content_type: *const c_char, rs: *const i64) -> *mut GosHttpResponse` storing `stream_handle` (slot 0 of the rs blob) into the Task 1 field; `crates/gossamer-runtime/src/c_abi/http_server.rs` — the connection writer, on `stream_handle >= 0`, takes the reader from the runtime `STREAM_REGISTRY` and writes the same chunked framing directly to the socket.
- Modify: MIR table (`Response::stream` beside `Response::text`) + mirror-recipe tables.
- Test: `crates/gossamer-std/tests/http_server.rs` — handler returns a `Response` with `body_stream` over a `Cursor`; client-side raw TCP read asserts `Transfer-Encoding: chunked` and the de-chunked payload.

- [ ] **Step 1: Failing std server test** (chunked framing as above).
- [ ] **Step 2: gossamer-std implementation** (BodyStream + chunked writer + literal updates + `read_raw`).
- [ ] **Step 3: Interp wiring** (`Response::stream` builtin + `value_to_response` stream branch + adapter).
- [ ] **Step 4: Compiled wiring** (shim, server writer branch, MIR + tables).
- [ ] **Step 5: Verify** — std test green; `cargo test -p gossamer-std -p gossamer-interp -p gossamer-runtime && cargo clippy` on the four touched crates.

### Task 10: Tier-parity fixtures + release smoke

**Files:**
- Create: `feature-testing-examples/http_surface.gos` (offline — no sockets)
- Create: `feature-testing-examples/http_roundtrip.gos` (loopback only)
- Modify: `crates/gossamer-cli/tests/tier_parity.rs` (SPECS), `crates/gossamer-cli/tests/release_stability.rs`

- [ ] **Step 1: `http_surface.gos`** — offline surface probe, deterministic output:

```gossamer
// Offline parity probe for the 0.13.0 http surface: response
// construction with explicit headers, the chainable with_header,
// builder construction, and method validation. No sockets.
use std::http

fn main() {
    let resp = http::Response::text(201, "made")
    let tagged = resp.with_header("x-proxy", "locurlfwd").with_header("x-pass", "2")
    println!("status={}", tagged.status)
    println!("body={}", tagged.body)
    let _client = http::Client::builder().max_redirects(0).timeout_ms(5000).build()
    println!("builder=ok")
    let bad = http::request("BOGUS", "http://127.0.0.1:1", "", [])
    match bad {
        Ok(_) => println!("bogus=unexpected-ok"),
        Err(e) => println!("bogus_rejected={}", e.contains("unknown method")),
    }
}
```

(Adjust the error-text assertion to the exact message Task 3 produces; if `[]` needs a type hint use `let none: [(String, String)] = []`.)
- [ ] **Step 2: `http_roundtrip.gos`** — loopback server in a goroutine + client exercising every gap, then `process::exit(0)`:

```gossamer
// Loopback round-trip parity probe: response headers (gap 1), honored
// server headers (gap 3), redirect policy (gap 4), binary bodies
// (gap 5), and chunked streaming passthrough (gap 6). 127.0.0.1 only.
use std::http
use std::process
use std::time

struct App { }

impl http::Handler for App {
    fn serve(&self, r: http::Request) -> Result<http::Response, http::Error> {
        let path = r.path()
        if path == "/hop" {
            return Ok(http::Response::text(302, "go").with_header("location", "/data"))
        }
        if path == "/echo-len" {
            return Ok(http::Response::text(200, format!("{}", r.raw_body.len())))
        }
        Ok(http::Response::text(200, "payload-data").with_header("x-served-by", "fixture"))
    }
}

fn main() {
    go http::serve("127.0.0.1:8097", App { })
    time::sleep(300)

    let none: [(String, String)] = []
    let resp = http::request("GET", "http://127.0.0.1:8097/data", "", none).unwrap_or_else(|e| panic!("get: {e}"))
    println!("served_by_present={}", header_of(&resp.headers, "x-served-by") == "fixture")

    let pinned = http::Client::builder().max_redirects(0).build()
    let hop = pinned.request("GET", "http://127.0.0.1:8097/hop", "", none).unwrap_or_else(|e| panic!("hop: {e}"))
    println!("redirect_held={}", hop.status)

    let body = [0, 255, 1, 254]
    let echoed = http::request_bytes("POST", "http://127.0.0.1:8097/echo-len", body, none)
        .unwrap_or_else(|e| panic!("post: {e}"))
    println!("binary_len={}", echoed.body)

    let stream = http::stream("GET", "http://127.0.0.1:8097/data", "", none)
        .unwrap_or_else(|e| panic!("stream: {e}"))
    let mut total = 0
    while let Some(chunk) = stream.next_chunk(4) {
        total += chunk.len()
    }
    println!("streamed_bytes={}", total)
    process::exit(0)
}

fn header_of(headers: &[(String, String)], want: &String) -> String {
    for (name, value) in headers {
        if name == want { return value }
    }
    ""
}
```

(Adapt to real surface details at implementation time: `?`-vs-unwrap shapes, `next_chunk` Option signature, sleep units. Keep output lines EXACTLY stable across tiers. Port 8097 must not collide with web_server.gos's 8080; reuse the `SERVER_PORT_LOCK` serialization if the harness requires it.)
- [ ] **Step 3: Register both** in SPECS (plain `spec("feature-testing-examples/http_surface.gos")`; the roundtrip entry needs no ServerFixture since it self-terminates, but give it the port-lock treatment used by the web_server entry).
- [ ] **Step 4: Release smoke** — add a `release_stability.rs` case compiling `http_surface.gos`'s body with `gos build --release` and asserting the exact stdout. This is the strict-lowering gate proof for every new name.
- [ ] **Step 5: Run them**: `cargo test -p gossamer-cli --test tier_parity -- http_surface http_roundtrip` and `cargo test -p gossamer-cli --test release_stability -- http`.

### Task 11: Feature status, CHANGELOG, docs, SKILL.md

**Files:**
- Modify: `crates/gossamer-std/src/manifest/feature_status.rs` — shipped entries for `std::http::client_request_native`, `std::http::response_headers`, `std::http::redirect_policy`, `std::http::binary_bodies`, `std::http::streaming_responses` (one-line docs each, follow neighboring entry style)
- Modify: `CHANGELOG.md` — under the existing `## 0.13.0` header add a `### std::http — proxy-grade client and server on every tier` subsection: native `http::request`/`request_bytes` on compiled tiers (was VM-only), client `Response.headers`, honored server `headers` + `with_header`, `Client::builder()` redirect policy, `Request.raw_body`, `ResponseStream.next_chunk`, `Response::stream` chunked responses, the content-type parity fix, and the hand-rolled GET path removal
- Modify: `docs_src/stdlib/http.md` (Response fields/methods, Request.raw_body, Client::builder, Response::stream), `docs_src/stdlib/http_native_client.md` (redirect policy, request_bytes, next_chunk), `docs_src/stdlib/http_proxy.md` (passthrough example using the new surface)
- Modify: `SKILL.md` — update the `std::http` bullet (client surface list, Response fields, builder, streaming)

- [ ] **Step 1:** feature_status entries; `cargo test -p gossamer-std feature_status` (registry self-checks).
- [ ] **Step 2:** CHANGELOG subsection.
- [ ] **Step 3:** Doc pages — every new function documented with a runnable fenced example (these become doc-tests under `gos test`; keep examples offline).
- [ ] **Step 4:** SKILL.md http bullets.

### Task 12: Full validation sweep

- [ ] **Step 1:** `cargo fmt --all && cargo clippy --workspace --all-targets -- -D warnings`
- [ ] **Step 2:** `cargo test --workspace` (expect long; capture failures, fix, re-run)
- [ ] **Step 3:** `cargo test -p gossamer-cli --test tier_parity` (full SPECS run — the new fixtures must be bit-identical across VM / Cranelift / LLVM)
- [ ] **Step 4:** `./check.sh` if that is the repo's canonical gate (read it first; run what it runs)
- [ ] **Step 5:** Re-read this plan top to bottom; confirm every gap (1, 3, 4, 5, 6 + P0 + content-type divergence) has a passing test that proves it. Report results; do NOT commit.

---

## Self-review notes

- Gap 1 → Task 2; gap 3 → Task 6; gap 4 → Tasks 4+7; gap 5 → Tasks 3+5; gap 6 → Tasks 8+9; P0 VM-only `http::request` → Task 3; discovered content-type divergence → Task 1; fixtures/smoke → Task 10; docs/meta → Task 11.
- Type names: `ClientBuilder`, `BodyStream`, `ResponseStream` (existing). Field names: `headers`, `raw_body`, `body_stream`, `stream_handle`, `content_type` — consistent across tasks.
- Known uncertainty flagged in-place: exact MIR table shapes (resolved by the mirror recipe at implementation time), fixture surface details (Step notes say adapt while keeping output stable).
