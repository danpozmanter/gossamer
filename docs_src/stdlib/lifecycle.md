# `std::lifecycle`

Status: unproven

Process readiness and graceful shutdown, with systemd sd_notify. Shutdown is observed, not dispatched: wait for it, then drain with ordinary statements - `spawn(|| serve())`, `lifecycle::ready()`, `lifecycle::await_shutdown()`, then the cleanup.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Lifecycle`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) | `type Lifecycle` | Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd. |
