# `std::http::state`

Status: shipped

Handler-side dependency injection via a typed AppState.

## Public items

| Name | Kind | Description |
|---|---|---|
| `AppState` | type | TypeMap of Arc<T> values shared across handlers. |
| `State` | type | Newtype wrapper Arc<T> for ergonomic handler arguments. |

