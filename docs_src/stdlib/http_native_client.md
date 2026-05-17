# `std::http::native_client`

Status: shipped

Goroutine-driven HTTP/1.1 client over std::net (no ureq, no blocking pool).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Client` | type | Native h1 client (Rust-side; full builder surface). |
| `Error` | type | Connect / Tls / Http / Redirect / Timeout / Io. |
| `get` | fn | One-shot GET → Result<Response, Error>. Interp tier (compiled tier shares http::get). |
| `post` | fn | One-shot POST: `(url, body, content_type)`. Interp tier. |
| `put` | fn | One-shot PUT: `(url, body, content_type)`. Interp tier. |
| `delete` | fn | One-shot DELETE → Result<Response, Error>. Interp tier. |

