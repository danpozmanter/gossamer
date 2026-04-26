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
