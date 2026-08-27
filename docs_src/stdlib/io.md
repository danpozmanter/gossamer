# `std::io`

Status: experimental

Stream-oriented I/O abstractions and process standard streams.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Buffering

Standard output is buffered so that the several writes a formatted line
arrives as - the literal segments, a rendered value, the newline - cost one
`write(2)`. The buffer drains on the newline that ends a write, so a line is
on its way out as soon as it is complete, whether standard output is a
terminal, a pipe, or a file. A program that announces a line and then blocks
is therefore visible to whatever is reading it:

```gossamer
println("{}", server.addr())
server.serve(routes)?
```

Text with no terminator accumulates: a prompt written with `print` needs an
explicit flush before the read.

```gossamer
use std::io
print("name: ")
io::stdout().flush()
```

Byte-at-a-time and byte-range writes accumulate too - that is the
high-throughput path, and it drains when the buffer fills, on an explicit
`flush`, and at exit. Standard error is never buffered, and writing to it
flushes standard output first, so the two streams keep their order.

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
