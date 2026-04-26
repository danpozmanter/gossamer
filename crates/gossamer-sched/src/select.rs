//! `select` over multiple channel operations.

#![forbid(unsafe_code)]

use crate::channel::{Channel, RecvResult, SendResult};

/// Operation to attempt when polling a [`select`] set.
pub enum SelectOp<'a, T> {
    /// Attempt to receive a value from `chan`.
    Recv {
        /// Channel to poll.
        chan: &'a mut Channel<T>,
    },
    /// Attempt to send `value` on `chan`.
    Send {
        /// Channel to poll.
        chan: &'a mut Channel<T>,
        /// Value to send. Taken by value on success.
        value: T,
    },
}

/// Outcome of a single poll over a [`select`] set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SelectOutcome<T> {
    /// Arm index `index` received `value`.
    Received {
        /// Which arm fired.
        index: usize,
        /// Value returned by the receive.
        value: T,
    },
    /// Arm index `index` completed a send.
    Sent {
        /// Which arm fired.
        index: usize,
    },
    /// Arm `index` observed that its channel was closed.
    Closed {
        /// Which arm fired.
        index: usize,
    },
    /// No arm could make progress; the scheduler should park the
    /// caller on every operand channel and wait for one to fire.
    WouldBlock,
}

/// Tries each arm of a select set in order. The first arm that can
/// make progress wins; ordering keeps the implementation
/// deterministic even when several arms are ready. Randomised
/// ordering (per Go's spec) is layered on once real
/// concurrency arrives.
#[must_use]
pub fn poll_select<T>(ops: Vec<SelectOp<'_, T>>) -> SelectOutcome<T> {
    for (index, op) in ops.into_iter().enumerate() {
        match op {
            SelectOp::Recv { chan } => match chan.try_recv() {
                RecvResult::Value(value) => return SelectOutcome::Received { index, value },
                RecvResult::Closed => return SelectOutcome::Closed { index },
                RecvResult::WouldBlock => {}
            },
            SelectOp::Send { chan, value } => match chan.try_send(value) {
                Ok(SendResult::Sent) => return SelectOutcome::Sent { index },
                Ok(SendResult::Closed) => return SelectOutcome::Closed { index },
                Ok(SendResult::WouldBlock) | Err(_) => {}
            },
        }
    }
    SelectOutcome::WouldBlock
}
