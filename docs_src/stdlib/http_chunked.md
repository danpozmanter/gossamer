# `std::http::chunked`

RFC 7230 §4.1 chunked transfer-encoding reader and writer.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | type | Decodes a chunked body from any Read source (Rust-side; streaming). |
| `Writer` | type | Encodes raw bytes into chunked frames over any Write sink (Rust-side; streaming). |
| `encode` | fn | One-shot: wraps a buffer in chunked transfer-encoding with terminator. Available in interp + compiled. |
| `decode` | fn | One-shot: concatenates data chunks from a complete chunked body. Available in interp + compiled. |

