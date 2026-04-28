//! M:N goroutine scheduler for the Gossamer runtime.
//! Stackful coroutines would require assembly (per SPEC §8 and the
//! plan) and `unsafe` code. The project style guide forbids
//! `unsafe` in library code, so this implementation models goroutines
//! as cooperative step-returning tasks. Each task yields control back
//! to the scheduler at explicit safepoints; the scheduler rotates
//! through its run queue FIFO until every task reaches [`Step::Done`].
//! The data model follows the Go runtime terminology so later phases
//! can graft on real stack switching without reshaping the API:
//! - [`Gid`] identifies a goroutine ("G"),
//! - [`Scheduler`] plays the role of a processor ("P"),
//! - The caller's thread is the machine ("M"); multi-M parallelism
//!   arrives once stack switching lands.

#![forbid(unsafe_code)]

mod channel;
mod multi;
mod poller;
mod queue;
mod scheduler;
mod select;
mod task;

pub use channel::{Channel, RecvResult, SendResult};
pub use multi::{MultiScheduler, MultiStats, ParkReason, SchedTask, SendTask};
pub use poller::{Interest, MockPoller, OsPoller, PollSource, Poller, Readiness};
pub use queue::RunQueue;
pub use scheduler::{SchedStats, Scheduler};
pub use select::{SelectOp, SelectOutcome, poll_select};
pub use task::{Gid, Step, Task};
