# `std::http::proxy`

Status: shipped

Reverse proxy on top of http::Client. Director-style request mutator + hop-by-hop strip + error handler.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Proxy` | type | Reverse-proxy handler (Rust-side). |
| `Director` | type | Fn(&mut Request) request mutator (Rust-side). |
| `forward` | fn | One-shot upstream forward: `(url, method, body) -> Result<Response, Error>`. Interp tier. |

