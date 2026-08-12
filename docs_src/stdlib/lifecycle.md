# `std::lifecycle`

Status: unproven

Graceful-shutdown coordinator with signal handling and sd_notify support.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Lifecycle`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/lifecycle.rs) | `type Lifecycle` | Registers shutdown hooks, listens for SIGTERM / SIGINT, and notifies systemd. |
