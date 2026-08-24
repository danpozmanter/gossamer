# `std::testing`

Status: experimental

Assertions and sub-test harness helpers.

<!-- hand-maintained from here: preserved by `gos doc --emit-stdlib` -->

## API details and source

The [implementation source](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) contains the complete declarations and implementation notes. The table below lists canonical Gossamer call signatures; every item name links directly to its implementation file.

| Item | Canonical signature or declaration | Description |
|---|---|---|
| [`Runner`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) | `type Runner` | Sub-test collector. |
| [`check`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) | `fn check(cond: bool, message: String) -> Result<(), errors::Error>` | Asserts a condition. |
| [`check_eq`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) | `fn check_eq<T: Debug + Eq>(left: T, right: T, message: String) -> Result<(), errors::Error>` | Asserts equality, rendering a diff on failure. |
| [`check_ok`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) | `fn check_ok<T, E: Debug>(result: Result<T, E>, message: String) -> Result<T, errors::Error>` | Asserts a Result is Ok, recording without panicking. |
| [`wait_for_scheduler_idle`](https://github.com/gossamer-lang/gossamer/blob/main/crates/gossamer-std/src/testing.rs) | `fn wait_for_scheduler_idle(timeout_ms: i64) -> bool` | Waits for the scheduler to become idle within a timeout. |
