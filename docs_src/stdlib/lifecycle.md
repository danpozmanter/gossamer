# `std::lifecycle`

Status: shipped

Graceful-shutdown coordinator with signal handling and sd_notify support.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Lifecycle` | type | Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd. |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Lifecycle`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) | `type` — see the source declaration | Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd. |
