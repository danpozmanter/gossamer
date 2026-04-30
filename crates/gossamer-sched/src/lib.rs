//! Re-export facade over the M:N scheduler that now lives inside the
//! `gossamer-runtime` crate.
//!
//! The scheduler core (work-stealing M:N runtime, channels, netpoller,
//! select, run-queue) was relocated into `gossamer-runtime` so the
//! static library every compiled Gossamer binary links carries the
//! scheduler too. This crate keeps its name and public type re-exports
//! so existing dependents (`gossamer-std`, the interpreter, tests)
//! continue to compile unchanged.

#![forbid(unsafe_code)]

pub use gossamer_runtime::sched::{
    Channel, Gid, Interest, MockPoller, MultiScheduler, MultiStats, OsPoller, ParkReason,
    PollSource, Poller, Readiness, RecvResult, RunQueue, SchedStats, SchedTask, Scheduler,
    SelectOp, SelectOutcome, SendResult, SendTask, Step, Task, poll_select,
};
