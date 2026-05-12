# Cross-Tier HTTP Consolidation Audit (P2)

Date: 2026-05-11. Status: pointer document — full consolidation
deferred pending compiled-tier perf testing.

## What's duplicated

The compiled tier (`crates/gossamer-runtime/src/c_abi.rs`) has
its own HTTP/1.1 implementation alongside the interp tier's
`gossamer-std::http::server`:

| Function | c_abi.rs line | Equivalent in std::http |
|---|---|---|
| `gos_rt_http_serve` | ~4802 | `server::run` |
| `handle_http_conn` | ~4859 | `server::worker_loop` |
| `find_header_end` | ~5022 | (inlined in parser) |
| `parse_request_into` | ~5100 | `parse_request_head_generic` |
| `extract_response_into` | ~5129 | `write_response_generic` |
| `static_ok_response` | ~4764 | (no equivalent — fast-path bypass) |

The compiled tier achieves ~270k RPS on the web bench (per
`web_perf_v2.md`) precisely because it bypasses the std::http
path. Two implementations means HTTP P0/P1 features
(Date/Server headers, chunked, 100-continue, timeouts) currently
land only on the interp tier.

## Consolidation seam

The std::http parser is now generic over `R: BufRead`:

```rust
pub(crate) fn parse_request_head_generic<R: BufRead>(
    reader: &mut R,
    config: &Config,
    header_deadline: Option<Instant>,
) -> io::Result<Option<RequestHead>>;

pub(crate) fn finish_request<R: BufRead>(
    reader: &mut R,
    head: RequestHead,
    config: &Config,
    body_deadline: Option<Instant>,
) -> io::Result<Option<(Request, bool, bool, Cancel)>>;

fn write_response_generic<W: Write>(
    stream: &mut W,
    response: &Response,
    server_name: Option<&str>,
) -> io::Result<()>;
```

These take any `Read + Write` source. The c_abi handler can
wrap its `TcpStream` in a `BufReader` and call directly.

## Migration steps (when scheduled)

1. **Make `parse_request_head_generic` / `finish_request` /
   `write_response_generic` pub(crate)** for `gossamer-runtime`
   to call. Today they're private to the inner `server` module.
   Either:
   - Re-export them in `crates/gossamer-std/src/lib.rs` under
     a `pub mod http::wire { ... }` namespace (preferred —
     keeps the API stable).
   - Or move them to a new `gossamer-http-wire` crate so the
     stdlib AND the runtime can pull from a shared dep
     without `gossamer-runtime → gossamer-std` (which today
     would be a layering violation since std uses the runtime).

2. **Adapt `c_abi::handle_http_conn`** to call
   `parse_request_head_generic` followed by
   `finish_request` rather than the inline `parse_request_into`.
   The handler-trampoline call shape stays the same —
   c_abi already produces a `GosHttpRequest` from a
   `Request` struct, just under a different code path.

3. **Adapt `c_abi::extract_response_into`** to consume the
   `Response` struct produced by std::http and call
   `write_response_generic` instead of formatting the
   response in c_abi.

4. **Delete the duplicated parsers** (`find_header_end`,
   `parse_request_into`, `extract_response_into`,
   `static_ok_response`) once the c_abi worker calls into
   `http::wire::*`.

## Open risk

The c_abi fast-path bypass (`static_ok_response` etc.) is
measurable in the web bench. Consolidation must preserve at
least the same hot-path allocator behaviour:

- Per-connection scratch `Vec<u8>` for response bytes.
- Per-connection `BufReader` reused across requests.
- `writev`-style header+body emission.

The std::http path already does the first two (per HTTP P0
changes). The writev optimisation is a follow-up; until then,
compiled-tier perf may regress 10–15 % on the bench until the
consolidated path catches up.

## When to schedule

P6 (native h1 client) is the next compiled-tier-affecting
change. P2 consolidation should land **after** P6 so the
client and server share the same wire module from day one.
Order:

1. P6 lands → new `http::client::native` module on
   `gossamer-std::net::TcpStream`.
2. Promote `parse_*` / `write_response_generic` to
   `gossamer-std::http::wire`.
3. P2 consolidation: c_abi delegates.

After this, the compiled tier ships with the full HTTP/1.1
feature set (Date/Server, chunked, 100-Continue, timeouts,
context cancel, graceful shutdown) automatically — no parallel
implementation to maintain.

## Today's state

Audit complete. No code changes yet. The c_abi handler keeps
its hand-rolled implementation; std::http is the source of
truth for the interp tier and the new compiled-tier path will
adopt it post-P6.
