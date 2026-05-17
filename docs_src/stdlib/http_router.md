# `std::http::router`

Status: shipped

Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Router` | type | Routing table (Rust-side full surface; method-chain bridge is interp tier). |
| `Params` | type | Captured path parameters. |
| `Handler` | trait | Anything callable as Fn(&Request, &Params) -> Response. |
| `new` | fn | Allocate a fresh Router handle. Interp tier. |
| `add` | fn | Register a route: `(router, method, pattern)`. Interp tier. |
| `lookup` | fn | Find the index of the first route matching `(method, path)`. Interp tier. |

