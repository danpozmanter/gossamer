# `std::http::state`

Status: experimental

Dependency injection is closure capture: build the router from closures that capture the pool, the cache, and the configuration, and each handler reads what it captured. A captured heap value is shared, so one map serves every request.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`AppState`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type AppState` | TypeMap of T values shared across handlers. |
| [`State`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type State` | Newtype wrapper T for ergonomic handler arguments. |
