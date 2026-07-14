# `std::option`

Status: experimental

Data-last Option combinators for pipeline chaining: map, filter, unwrap_or, and_then, etc.

## Public items

| Name | Kind | Description |
|---|---|---|
| `and_then` | fn | Chains a fallible step: Some(v) -> f(v), None stays None. |
| `unwrap_or` | fn | Unwraps with a fallback value for None. |
| `unwrap_or_else` | fn | Unwraps with a lazily computed fallback for None. |
| `filter` | fn | Keeps Some(v) only when the predicate holds. |
| `flatten` | fn | Collapses Option<Option<T>> one level. |
| `is_none` | fn | True for None. |
| `is_some` | fn | True for Some. |
| `iter` | fn | Zero-or-one element sequence view. |
| `map` | fn | Transforms the Some payload, None stays None. |
| `or` | fn | First Some of self and the alternative. |
| `or_else` | fn | First Some of self and a lazily built alternative. |
| `zip` | fn | Pairs two Somes into Some((a, b)). |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`and_then`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn and_then<T, U>(f: Fn(T) -> Option<U>, value: Option<T>) -> Option<U>` | Chains a fallible step: Some(v) -> f(v), None stays None. |
| [`filter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn filter<T>(predicate: Fn(T) -> bool, value: Option<T>) -> Option<T>` | Keeps Some(v) only when the predicate holds. |
| [`flatten`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn flatten<T>(value: Option<Option<T>>) -> Option<T>` | Collapses Option<Option<T>> one level. |
| [`is_none`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn is_none<T>(value: Option<T>) -> bool` | True for None. |
| [`is_some`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn is_some<T>(value: Option<T>) -> bool` | True for Some. |
| [`iter`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn iter<T>(value: Option<T>) -> Vec<T>` | Zero-or-one element sequence view. |
| [`map`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn map<T, U>(f: Fn(T) -> U, value: Option<T>) -> Option<U>` | Transforms the Some payload, None stays None. |
| [`or`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn or<T>(fallback: Option<T>, value: Option<T>) -> Option<T>` | First Some of self and the alternative. |
| [`or_else`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn or_else<T>(fallback: Fn() -> Option<T>, value: Option<T>) -> Option<T>` | First Some of self and a lazily built alternative. |
| [`unwrap_or`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn unwrap_or<T>(fallback: T, value: Option<T>) -> T` | Unwraps with a fallback value for None. |
| [`unwrap_or_else`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn unwrap_or_else<T>(fallback: Fn() -> T, value: Option<T>) -> T` | Unwraps with a lazily computed fallback for None. |
| [`zip`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/option.rs) | `fn zip<T, U>(other: Option<U>, value: Option<T>) -> Option<(T, U)>` | Pairs two Somes into Some((a, b)). |
