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
