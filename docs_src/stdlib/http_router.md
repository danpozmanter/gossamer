# `std::http::router`

Status: shipped

Go 1.22-class ServeMux: method-aware path patterns with parameter captures + prefix routes.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Router` | type | Routing table. Build with `Router::new()`, register routes via the verb methods, then pass to `http::serve`. Verb methods return the router so they chain with `\|>`. |
| `Params` | type | Captured path parameters. Read inside a handler with `r.path_value(name) -> String`; returns `""` for an undeclared name. All tiers. |
| `Handler` | trait | Anything callable as `Fn(&Request, &Params) -> Response`. |
| `new` | fn | Allocate a fresh Router handle. |
| `add` | fn | Register a pattern-only route: `(router, method, pattern)`. Used with `lookup` for low-level dispatch. |
| `lookup` | fn | Find the index of the first route matching `(method, path)`. Returns `Option<i64>`. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Handler`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `trait` — see the source declaration | Anything callable as `Fn(&Request, &Params) -> Response`. |
| [`Params`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `type` — see the source declaration | Captured path parameters. Read inside a handler with `r.path_value(name) -> String`; returns `""` for an undeclared name. All tiers. |
| [`Router`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `type` — see the source declaration | Routing table. Build with `Router::new()`, register routes via the verb methods, then pass to `http::serve`. Verb methods return the router so they chain with `\|>`. |
| [`add`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `fn add(router: http::router::Router, method: String, pattern: String) -> Result<(), errors::Error>` | Register a pattern-only route: `(router, method, pattern)`. Used with `lookup` for low-level dispatch. |
| [`lookup`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `fn lookup(router: http::router::Router, method: String, path: String) -> Option<http::router::Match>` | Find the index of the first route matching `(method, path)`. Returns `Option<i64>`. |
| [`new`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_router.rs) | `fn new() -> http::router::Router` | Allocate a fresh Router handle. |


## Routing syntax

Patterns follow Go 1.22 `http.ServeMux` style:

```text
/users/{id}          single-segment capture (no /).
/files/{path...}     trailing greedy capture.
/static/*            wildcard (any path under /static/).
/health              literal exact match.
```

Matching precedence: method-specific beats method-agnostic; more
specific (literal > capture > wildcard) beats less specific; first
registered wins among ties.

## Building a router

Verb methods return the router, so `|>` chains are the idiomatic form:

```gos
use std::http
use std::http::router

fn main() -> Result<(), http::Error> {
    router::Router::new()
        |> _.get("/health", health)
        |> _.get("/users", list_users)
        |> _.post("/users", create_user)
        |> _.get("/users/{id}", show_user)
        |> http::serve("0.0.0.0:8080")
}
```

The mutating form (`r.get(...)` without `|>`) also works and is
equivalent - it is just less idiomatic.

## Path parameters

Inside a handler, read captured segments via the request:

```gos
fn show_user(r: http::Request) -> http::Response {
    let id = r.path_value("id")           // -> String, "" if absent
    let n  = r.path_int("id")             // -> Option<i64>
    let f  = r.path_float("qty")          // -> Option<f64>
    http::Response::text(200, format!("id={}", id))
}
```
