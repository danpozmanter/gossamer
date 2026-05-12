# `std::slog`

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

