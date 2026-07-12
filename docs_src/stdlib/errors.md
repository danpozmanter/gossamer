# `std::errors`

Status: experimental

Error construction, wrapping, and chain traversal.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Error` | type | Reference-counted error value with optional cause chain. |
| `new` | fn | Constructs a fresh error from a message. |
| `newf` | fn | Constructs a fresh error from a format template, e.g. `newf("status {}", code)`. |
| `wrap` | fn | Wraps a cause with a higher-level message. |
| `is` | fn | Checks whether an error's chain contains a matching message. |
| `join` | fn | Joins a list of errors into one; messages are joined with "; " (None for an empty list). |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## Rendering

Displaying an error renders the full cause chain, colon-joined from the
outermost wrap down to the root cause:

```text
let root = errors::new("no such file")
let mid  = errors::wrap(root, "open /etc/app.toml")
let top  = errors::wrap(mid, "reading config")
println!("{}", top)
// reading config: open /etc/app.toml: no such file
```

- `err.message()` returns only the top message (`"reading config"`),
  not the chain - pair it with `err.cause()` to walk levels manually.
- `errors::join([a, b]) -> Option<Error>` combines several errors into
  one whose message is the individual messages joined with `"; "`
  (`"a; b"`); an empty list joins to `None`.
- `errors::is(err, needle)` walks the same cause chain that Display
  renders; step through it yourself with `err.cause()`.
