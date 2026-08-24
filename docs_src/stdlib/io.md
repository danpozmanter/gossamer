# `std::io`

Status: experimental

Stream-oriented I/O abstractions and process standard streams.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`BufReader`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type BufReader` | Buffered wrapper around any `Reader`. |
| [`BufWriter`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type BufWriter` | Buffered wrapper around any `Writer`. |
| [`Copy`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn Copy(dst: io::Writer, src: io::Reader) -> Result<i64, io::Error>` | Copies all bytes from src to dst; returns the byte count. |
| [`Error`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `type Error` | Errors raised by I/O operations. |
| [`ReadAll`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn ReadAll(reader: io::Reader) -> Result<String, io::Error>` | Drains a reader to a String. Mirrors Go's io.ReadAll. |
| [`Reader`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `trait Reader` | Pull-style byte source. |
| [`Writer`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `trait Writer` | Push-style byte sink. |
| [`stderr`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stderr() -> io::Writer` | Returns a handle to the process's standard error stream. |
| [`stdin`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stdin() -> io::Reader` | Returns a handle to the process's standard input stream. Use read_line(&mut String) for interactive prompts. |
| [`stdout`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/io.rs) | `fn stdout() -> io::Writer` | Returns a handle to the process's standard output stream. |
