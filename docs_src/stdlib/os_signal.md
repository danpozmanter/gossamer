# `std::os::signal`

Status: experimental

POSIX-style signal subscription (Go's os/signal shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Signal` | type | Opaque signal name; constructors live in `sigs`. |
| `Notifier` | type | Returned by `on(sig)`; supports wait / try_wait. |
| `on` | fn | Subscribes to a signal; returns a Notifier. |
| `wait` | fn | Blocks the calling goroutine until the subscribed signal fires. |
| `try_wait` | fn | Non-blocking poll: returns true if the subscribed signal has fired. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Notifier`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) | `type` — see the source declaration | Returned by `on(sig)`; supports wait / try_wait. |
| [`Signal`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) | `type` — see the source declaration | Opaque signal name; constructors live in `sigs`. |
| [`on`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) | `fn on(signum: i64) -> os::signal::Notifier` | Subscribes to a signal; returns a Notifier. |
| [`try_wait`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) | `fn try_wait(notifier: os::signal::Notifier) -> bool` | Non-blocking poll: returns true if the subscribed signal has fired. |
| [`wait`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/signal.rs) | `fn wait(notifier: os::signal::Notifier) -> ()` | Blocks the calling goroutine until the subscribed signal fires. |
