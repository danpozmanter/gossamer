# `std::io`

Status: shipped

Stream-oriented I/O abstractions.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | trait | Pull-style byte source. |
| `Writer` | trait | Push-style byte sink. |
| `BufReader` | type | Buffered wrapper around any `Reader`. |
| `BufWriter` | type | Buffered wrapper around any `Writer`. |
| `stdin` | fn | Returns a handle to the process's standard input stream. |
| `stdout` | fn | Returns a handle to the process's standard output stream. |
| `stderr` | fn | Returns a handle to the process's standard error stream. |
| `ReadAll` | fn | Drains a reader to a String. Mirrors Go's io.ReadAll. |
| `Copy` | fn | Copies all bytes from src to dst; returns the byte count. |
| `Error` | type | Errors raised by I/O operations. |

