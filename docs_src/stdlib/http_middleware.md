# `std::http::middleware`

Status: shipped

Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Handler` | trait | Anything serving (Request, Params) -> Response. |
| `Chain` | type | Helper for composing middleware in a single value. |
| `logger` | fn | Logs method path status bytes elapsed_ms per request. |
| `recoverer` | fn | Catches handler panics; returns 500. |
| `request_id` | fn | Stamps each response with X-Request-Id. |
| `cors` | fn | CORS preflight + per-response headers. |
| `basic_auth` | fn | HTTP Basic auth gate; constant-time compare. |
| `compress_gzip` | fn | Gzips bodies above a size threshold when client advertises Accept-Encoding: gzip (Rust-side wrapper). |
| `new_request_id` | fn | Generate a process-monotonic request id string. Available in interp + compiled. |
| `tag` | fn | Wrap a handler (`tag(inner) -> Handler`), prepending `mw:` to each response body. Deterministic composition primitive; available in interp + compiled. |
| `accepts_gzip` | fn | Check an Accept-Encoding header for a gzip token. Available in interp + compiled. |
| `decode_basic_auth` | fn | Decode a Basic-auth Authorization header into (user, password). Interp tier. |
| `bearer_ok` | fn | Run a verify closure on the request's Bearer token; false (without calling verify) when no Bearer header is present. Available in interp + compiled. |

