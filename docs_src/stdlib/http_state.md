# `std::http::state`

Status: experimental

Handler-side dependency injection via a typed AppState.

## Public items

| Name | Kind | Description |
|---|---|---|
| `AppState` | type | TypeMap of Arc<T> values shared across handlers. |
| `State` | type | Newtype wrapper Arc<T> for ergonomic handler arguments. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`AppState`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type` — see the source declaration | TypeMap of Arc<T> values shared across handlers. |
| [`State`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/http_state.rs) | `type` — see the source declaration | Newtype wrapper Arc<T> for ergonomic handler arguments. |
