# `std::http::state`

Status: experimental

Handler-side dependency injection via a typed AppState.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`AppState`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type AppState` | TypeMap of Arc<T> values shared across handlers. |
| [`State`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type State` | Newtype wrapper Arc<T> for ergonomic handler arguments. |
