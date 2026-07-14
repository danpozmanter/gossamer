# `std::result`

Status: experimental

Data-last Result combinators for pipeline chaining: map, map_err, unwrap_or_else, etc.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`and_then`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn and_then<T, E, U>(f: Fn(T) -> Result<U, E>, value: Result<T, E>) -> Result<U, E>` | Chains a fallible step on the Ok payload. |
| [`err`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn err<T, E>(value: Result<T, E>) -> Option<E>` | Err payload as an Option. |
| [`is_err`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn is_err<T, E>(value: Result<T, E>) -> bool` | True for Err. |
| [`is_ok`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn is_ok<T, E>(value: Result<T, E>) -> bool` | True for Ok. |
| [`map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn map<T, E, U>(f: Fn(T) -> U, value: Result<T, E>) -> Result<U, E>` | Transforms the Ok payload, Err passes through. |
| [`map_err`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn map_err<T, E, F>(f: Fn(E) -> F, value: Result<T, E>) -> Result<T, F>` | Transforms the Err payload, Ok passes through. |
| [`ok`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn ok<T, E>(value: Result<T, E>) -> Option<T>` | Ok payload as an Option. |
| [`or_else`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn or_else<T, E, F>(f: Fn(E) -> Result<T, F>, value: Result<T, E>) -> Result<T, F>` | Recovers from Err with a fallback computation. |
| [`unwrap_or`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn unwrap_or<T, E>(fallback: T, value: Result<T, E>) -> T` | Unwraps Ok with a fallback value for Err. |
| [`unwrap_or_else`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/result.rs) | `fn unwrap_or_else<T, E>(f: Fn(E) -> T, value: Result<T, E>) -> T` | Consumes the result, handling Err with a callback. |
