# `std::errors`

Status: experimental

Error construction, wrapping, and chain traversal.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Error`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `type Error` | Reference-counted error value with optional cause chain. |
| [`is`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `fn is(error: errors::Error, needle: String) -> bool` | Checks whether an error's chain contains a matching message. |
| [`join`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `fn join(errors: Vec<errors::Error>) -> Option<errors::Error>` | Joins a list of errors into one; messages are joined with "; " (None for an empty list). |
| [`new`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `fn new(message: String) -> errors::Error` | Constructs a fresh error from a message. |
| [`newf`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `fn newf(format: String, args: Vec<String>) -> errors::Error` | Constructs a fresh error from a format template, e.g. `newf("status {}", code)`. |
| [`wrap`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/errors.rs) | `fn wrap(error: errors::Error, context: String) -> errors::Error` | Wraps a cause with a higher-level message. |


## Rendering

Displaying an error renders the full cause chain, colon-joined from the
outermost wrap down to the root cause:

```text
let root = errors::new("no such file")
let mid  = errors::wrap(root, "open /etc/app.toml")
let top  = errors::wrap(mid, "reading config")
println("{}", top)
// reading config: open /etc/app.toml: no such file
```

- `err.message()` returns only the top message (`"reading config"`),
  not the chain - pair it with `err.cause()` to walk levels manually.
- `errors::join([a, b]) -> Option<Error>` combines several errors into
  one whose message is the individual messages joined with `"; "`
  (`"a; b"`); an empty list joins to `None`.
- `errors::is(err, needle)` walks the same cause chain that Display
  renders; step through it yourself with `err.cause()`.
