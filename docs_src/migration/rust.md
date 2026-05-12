# Migrating from Rust to Gossamer

Gossamer looks a lot like Rust — the lexical grammar, keyword list,
and item shape are deliberate references. The differences compress
into a handful of rules.

## Differences that matter

| Rust | Gossamer |
|------|----------|
| Manual lifetimes (`'a`, `'static`) on references. | No explicit lifetimes. The GC owns every heap aggregate; `&T` is a plain shared reference with lifetime inferred from scope. |
| Ownership-by-move, `Copy` marker trait. | No move semantics. Non-trivial values are GC-heap and shared; primitives are copied the same as Rust. |
| Procedural and declarative macros. | **No user macros at all.** Six fixed `format!` / `println!`-family macros expand at parse time. |
| `async fn`, `Future`, `await`. | `go expr` spawns a goroutine. No futures, no awaits — blocking IO is fine. |
| Multiple separate compilation units, workspace member graph. | Same workspace idea (`gos new --template workspace`). Individual crates are called *packages* and resolve through `project.toml`. |
| `unsafe` blocks. | **Forbidden at the language level.** No `unsafe` keyword in Gossamer source. `std` is safe-Rust too. |
| `panic!` unwinds by default. | `panic` aborts the current goroutine; handlers observe a 500 but the process keeps running. |
| `Result<T, E>` + `?` + `thiserror`. | `Result<T, E>` + `?` + `std::errors::Error` (single concrete error type). |

## What stays the same

- `struct`, `enum`, `impl`, `trait` syntax.
- `match` with exhaustiveness checking, guards, or-patterns.
- `if let` / `while let`.
- Iterators (`for n in 0..10`).
- Module tree (`mod`, `use`, `pub`).
- `cargo`-shaped CLI: `gos build`, `gos test`, `gos fmt`, `gos check`.

## Translation examples

Rust:

```rust
pub fn fetch(url: &str) -> Result<Vec<u8>, reqwest::Error> {
    let response = reqwest::blocking::get(url)?;
    Ok(response.bytes()?.to_vec())
}
```

Gossamer:

```gos
pub fn fetch(url: &str) -> Result<[u8], errors::Error> {
    let response = http::get(url)?
    Ok(response.body())
}
```

Rust:

```rust
struct Server { handler: Box<dyn Fn(Request) -> Response + Send + Sync> }
```

Gossamer:

```gos
struct Server { handler: fn(http::Request) -> http::Response }
```

(Trait objects stay available but rarely needed — concrete closure
types are preferred and the GC keeps their captures alive.)

## Collection combinators (one obvious way)

Rust:

```rust
let total: i64 = xs.iter()
    .filter(|n| **n % 2 == 0)
    .map(|n| n * n)
    .sum();
```

Gossamer:

```gos
let total = xs
    |> iter::filter(|n: i64| n % 2 == 0)
    |> iter::sum_by(|n: i64| n * n)
```

`Vec<T>`, `HashMap<K, V>`, and `HashSet<T>` deliberately **do not**
carry `.map` / `.filter` / `.fold` methods in Gossamer. The
free-function form in `std::iter` is the one obvious way to chain
transformations, and the `|>` operator (SPEC §4.6) threads each
value through with the data-last convention. Mutating helpers like
`xs.push`, `xs.sort`, `m.inc`, `m.or_insert` stay as methods —
they operate by side-effect on the receiver and don't compose
through `|>`.

The same pattern applies to `Option<T>` / `Result<T, E>` chaining:
Rust-style methods (`opt.map`, `opt.unwrap_or`, `result.map_err`)
remain available, but the free-function siblings in `std::option`
and `std::result` are the pipe-friendly form. `?` stays the right
tool for short-circuit propagation; the combinators are for
in-pipeline transformation.

## Standard library mapping (Rust → Gossamer)

The Gossamer stdlib follows Rust's `fs`/`env`/`process` split for
process-level primitives, and Go's flat `strings`/`strconv`/`bytes`
shape for text and numeric formatting. Filesystem entry points are
all in `fs::*`, environment and CLI in `env::*`, child processes
and exit in `process::*`.

| Rust | Gossamer |
| --- | --- |
| `std::fs::read_to_string` | `fs::read_to_string` |
| `std::fs::read` | `fs::read` |
| `std::fs::write` | `fs::write` |
| `std::fs::remove_file` | `fs::remove_file` |
| `std::fs::create_dir_all` | `fs::create_dir_all` |
| `std::fs::read_dir` | `fs::read_dir` (returns `Vec<DirInfo>`) |
| `std::fs::copy` | `fs::copy` |
| `std::env::args` | `env::args` |
| `std::env::var(name).ok()` | `env::var(name)` |
| `std::env::current_dir` | `env::current_dir` |
| `std::env::temp_dir` | `env::temp_dir` |
| `std::process::Command::new(...)` | `process::Command::new(...)` |
| `std::process::exit` | `process::exit` |
| `std::path::Path::new(p).join(...)` | `path::join(p, ...)` |
| `std::sync::Mutex` | `sync::Mutex` |
| `std::time::Duration::from_millis` | `time::Duration::from_millis` |
| `std::thread::spawn` | `go expr` (goroutine) or `thread::spawn` (OS thread) |

HTTP/2 is integrated into `std::http` directly (Go-style) —
`http::serve_h2c` for cleartext h2c, automatic ALPN negotiation
when serving over TLS. There is no separate `std::http2`.
