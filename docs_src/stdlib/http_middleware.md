# `std::http::middleware`

Status: shipped

Composable middleware: logger, recoverer, request_id, cors, basic_auth, compress_gzip.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Handler` | trait | Anything serving (Request, Params) -> Response. |
| `Chain` | type | Helper for composing middleware in a single value. |
| `new_request_id` | fn | Generate a process-monotonic request id string. Available in interp + compiled. |
| `tag` | fn | Wrap a handler (`tag(inner) -> Handler`), prepending `mw:` to each response body. Deterministic composition primitive; available in interp + compiled. |
| `accepts_gzip` | fn | Check an Accept-Encoding header for a gzip token. Available in interp + compiled. |
| `decode_basic_auth` | fn | Decode a Basic-auth Authorization header into (user, password). Interp tier. |
| `bearer_ok` | fn | Run a verify closure on the request's Bearer token; false (without calling verify) when no Bearer header is present. Available in interp + compiled. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Chain`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `type` — see the source declaration | Helper for composing middleware in a single value. |
| [`Handler`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `trait` — see the source declaration | Anything serving (Request, Params) -> Response. |
| [`accepts_gzip`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `fn accepts_gzip(request: http::Request) -> bool` | Check an Accept-Encoding header for a gzip token. Available in interp + compiled. |
| [`bearer_ok`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `fn bearer_ok(request: http::Request, verify: Fn(String) -> bool) -> bool` | Run a verify closure on the request's Bearer token; false (without calling verify) when no Bearer header is present. Available in interp + compiled. |
| [`decode_basic_auth`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `fn decode_basic_auth(request: http::Request) -> Option<(String, String)>` | Decode a Basic-auth Authorization header into (user, password). Interp tier. |
| [`new_request_id`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `fn new_request_id() -> String` | Generate a process-monotonic request id string. Available in interp + compiled. |
| [`tag`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_middleware.rs) | `fn tag(handler: http::Handler) -> http::Handler` | Wrap a handler (`tag(inner) -> Handler`), prepending `mw:` to each response body. Deterministic composition primitive; available in interp + compiled. |
