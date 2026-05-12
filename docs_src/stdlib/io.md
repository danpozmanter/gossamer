# `std::io`

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
| `Error` | type | Errors raised by I/O operations. |

