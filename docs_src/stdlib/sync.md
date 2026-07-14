# `std::sync`

Status: shipped

Synchronisation primitives beyond channels.

## Public items

| Name | Kind | Description |
|---|---|---|
| `Mutex` | type | Mutual-exclusion lock. |
| `RwLock` | type | Reader-writer lock. |
| `Once` | type | One-shot initialisation latch. |
| `WaitGroup` | type | Counts goroutines and waits for them to finish. |
| `Barrier` | type | Synchronisation barrier across goroutines. |
| `AtomicI64` | type | Atomic 64-bit signed integer. |
| `AtomicU64` | type | Atomic 64-bit unsigned integer. |
| `AtomicBool` | type | Atomic boolean. |
| `channel` | fn | Creates a typed channel, returning (Sender, Receiver). |
| `channel_unbounded` | fn | Creates an explicit unbounded typed channel, returning (Sender, Receiver). |

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) contains the complete declarations and implementation notes. The table below expands the quick index above with canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`AtomicBool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Atomic boolean. |
| [`AtomicI64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Atomic 64-bit signed integer. |
| [`AtomicU64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Atomic 64-bit unsigned integer. |
| [`Barrier`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Synchronisation barrier across goroutines. |
| [`Mutex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Mutual-exclusion lock. |
| [`Once`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | One-shot initialisation latch. |
| [`RwLock`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Reader-writer lock. |
| [`WaitGroup`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type` — see the source declaration | Counts goroutines and waits for them to finish. |
| [`channel`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `fn channel<T>(capacity: i64) -> sync::Channel<T>` | Creates a typed channel, returning (Sender, Receiver). |
| [`channel_unbounded`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `fn channel_unbounded<T>() -> sync::Channel<T>` | Creates an explicit unbounded typed channel, returning (Sender, Receiver). |
