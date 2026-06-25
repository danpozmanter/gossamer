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

