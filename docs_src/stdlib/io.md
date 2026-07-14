# `std::io`

Status: experimental

Stream-oriented I/O abstractions and process standard streams.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Reader` | trait | Pull-style byte source. |
| `Writer` | trait | Push-style byte sink. |
| `BufReader` | type | Buffered wrapper around any `Reader`. |
| `BufWriter` | type | Buffered wrapper around any `Writer`. |
| `stdin` | fn | Returns a handle to the process's standard input stream. Use read_line(&mut String) for interactive prompts. |
| `stdout` | fn | Returns a handle to the process's standard output stream. |
| `stderr` | fn | Returns a handle to the process's standard error stream. |
| `ReadAll` | fn | Drains a reader to a String. Mirrors Go's io.ReadAll. |
| `Copy` | fn | Copies all bytes from src to dst; returns the byte count. |
| `Error` | type | Errors raised by I/O operations. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`BufReader`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type` — see the source declaration | Buffered wrapper around any `Reader`. |
| [`BufWriter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type` — see the source declaration | Buffered wrapper around any `Writer`. |
| [`Copy`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn Copy(dst: io::Writer, src: io::Reader) -> Result<i64, io::Error>` | Copies all bytes from src to dst; returns the byte count. |
| [`Error`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type` — see the source declaration | Errors raised by I/O operations. |
| [`ReadAll`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn ReadAll(reader: io::Reader) -> Result<String, io::Error>` | Drains a reader to a String. Mirrors Go's io.ReadAll. |
| [`Reader`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `trait` — see the source declaration | Pull-style byte source. |
| [`Writer`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `trait` — see the source declaration | Push-style byte sink. |
| [`stderr`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stderr() -> io::Writer` | Returns a handle to the process's standard error stream. |
| [`stdin`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stdin() -> io::Reader` | Returns a handle to the process's standard input stream. Use read_line(&mut String) for interactive prompts. |
| [`stdout`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stdout() -> io::Writer` | Returns a handle to the process's standard output stream. |
