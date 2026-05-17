# `std::os::signal`

Status: shipped

POSIX-style signal subscription (Go's os/signal shape).

## Public items

| Name | Kind | Description |
|---|---|---|
| `Signal` | type | Opaque signal name; constructors live in `sigs`. |
| `Notifier` | type | Returned by `on(sig)`; supports wait / try_wait. |
| `on` | fn | Subscribes to a signal; returns a Notifier. |
| `deliver` | fn | Test helper: synthesise a signal delivery without involving the OS. |

