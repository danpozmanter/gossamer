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

