# `std::slog`

Status: shipped

Structured, levelled logging.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Logger` | type | Logger handle. |
| `Field` | type | Key/value pair threaded through a logger. |
| `TextHandler` | type | Line-oriented handler. |
| `JsonHandler` | type | JSON-lines handler. |
| `info` | fn | Logs a JSON record at INFO level. Trailing args are key/value pairs. |
| `warn` | fn | Logs a JSON record at WARN level. |
| `error` | fn | Logs a JSON record at ERROR level. |
| `debug` | fn | Logs a JSON record at DEBUG level. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Field`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `type` — see the source declaration | Key/value pair threaded through a logger. |
| [`JsonHandler`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `type` — see the source declaration | JSON-lines handler. |
| [`Logger`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `type` — see the source declaration | Logger handle. |
| [`TextHandler`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `type` — see the source declaration | Line-oriented handler. |
| [`debug`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `fn debug(message: String) -> ()` | Logs a JSON record at DEBUG level. |
| [`error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `fn error(message: String) -> ()` | Logs a JSON record at ERROR level. |
| [`info`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `fn info(message: String) -> ()` | Logs a JSON record at INFO level. Trailing args are key/value pairs. |
| [`warn`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/slog.rs) | `fn warn(message: String) -> ()` | Logs a JSON record at WARN level. |
