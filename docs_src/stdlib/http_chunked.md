# `std::http::chunked`

Status: experimental

RFC 7230 §4.1 chunked transfer-encoding reader and writer.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Decodes a chunked body from any Read source (Rust-side; streaming). |
| `Writer` | type | Encodes raw bytes into chunked frames over any Write sink (Rust-side; streaming). |
| `encode` | fn | One-shot: wraps a buffer in chunked transfer-encoding with terminator. Available in interp + compiled. |
| `decode` | fn | One-shot: concatenates data chunks from a complete chunked body. Available in interp + compiled. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_chunked.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Reader`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_chunked.rs) | `type` — see the source declaration | Decodes a chunked body from any Read source (Rust-side; streaming). |
| [`Writer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_chunked.rs) | `type` — see the source declaration | Encodes raw bytes into chunked frames over any Write sink (Rust-side; streaming). |
| [`decode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_chunked.rs) | `fn decode(body: String) -> String` | One-shot: concatenates data chunks from a complete chunked body. Available in interp + compiled. |
| [`encode`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_chunked.rs) | `fn encode(body: String) -> String` | One-shot: wraps a buffer in chunked transfer-encoding with terminator. Available in interp + compiled. |
