# `std::panic`

Status: shipped

Panic / `catch_unwind` integration.

## Public items

| Name | Kind | Description |
|---|---|---|
| `panic` | macro | Aborts the current goroutine with a message. |
| `catch_unwind` | fn | Runs a closure, catching any panic it raises. |

