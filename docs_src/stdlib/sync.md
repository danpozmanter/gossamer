# `std::sync`

Status: experimental

Synchronisation primitives beyond channels.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`AtomicBool`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type AtomicBool` | Atomic boolean. |
| [`AtomicI64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type AtomicI64` | Atomic 64-bit signed integer. |
| [`AtomicU64`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type AtomicU64` | Atomic 64-bit unsigned integer. |
| [`Barrier`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type Barrier` | Synchronisation barrier across goroutines. |
| [`Mutex`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type Mutex` | Mutual-exclusion lock. |
| [`Once`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type Once` | One-shot initialisation latch. |
| [`RwLock`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type RwLock` | Reader-writer lock. |
| [`WaitGroup`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `type WaitGroup` | Counts goroutines and waits for them to finish. |
| [`channel`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `fn channel<T>(capacity: i64) -> sync::Channel<T>` | Creates a typed channel, returning (Sender, Receiver). |
| [`channel_unbounded`](https://github.com/danpozmanter/gossamer/blob/main/crates/gossamer-std/src/sync.rs) | `fn channel_unbounded<T>() -> sync::Channel<T>` | Creates an explicit unbounded typed channel, returning (Sender, Receiver). |
