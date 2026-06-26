//! M:N goroutine scheduler + netpoller integrated directly into the
//! runtime crate.
//!
//! Lives inside `gossamer-runtime` (instead of a sibling
//! `gossamer-sched` crate) so the static library that every compiled
//! Gossamer binary links against carries the scheduler. The
//! `gossamer-sched` crate continues to exist as a thin re-export
//! facade so existing dependents (`gossamer-std`, the interpreter,
//! tests) keep their import paths.
//!
//! See `multi.rs` for the work-stealing M:N implementation, `poller.rs`
//! for the mio-backed netpoller, and `super::sched_global` for the
//! process-global singleton that ties everything together.

#![forbid(unsafe_code)]

pub mod channel;
// The work-stealing M:N scheduler needs OS threads, crossbeam deques,
// and a mio netpoller - none available on wasm32. The wasm playground
// runs goroutines cooperatively to completion (the eager coro shim),
// so it links a single-threaded `MultiScheduler` that re-exports the
// same public types. Native is unaffected.
#[cfg(not(target_arch = "wasm32"))]
pub mod multi;
#[cfg(target_arch = "wasm32")]
#[path = "multi_wasm.rs"]
pub mod multi;
pub mod poller;
pub mod queue;
pub mod scheduler;
pub mod select;
pub mod task;

pub use channel::{Channel, RecvResult, SendResult};
pub use multi::{MultiScheduler, MultiStats, ParkReason, SchedTask, SendTask};
pub use poller::{Interest, MockPoller, OsPoller, PollSource, Poller, Readiness};
pub use queue::RunQueue;
pub use scheduler::{SchedStats, Scheduler};
pub use select::{SelectOp, SelectOutcome, poll_select};
pub use task::{Gid, Step, Task};
