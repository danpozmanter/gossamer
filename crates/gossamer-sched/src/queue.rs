//! FIFO run queue used by the scheduler.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use crate::task::Gid;

/// FIFO queue of runnable goroutine identifiers.
///
/// Kept as a separate type so later phases can
/// swap it for a lock-free deque without re-plumbing the scheduler.
#[derive(Debug, Default, Clone)]
pub struct RunQueue {
    entries: VecDeque<Gid>,
}

impl RunQueue {
    /// Returns an empty queue.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Enqueues a goroutine at the tail.
    pub fn push(&mut self, gid: Gid) {
        self.entries.push_back(gid);
    }

    /// Removes and returns the goroutine at the head, if any.
    pub fn pop(&mut self) -> Option<Gid> {
        self.entries.pop_front()
    }

    /// Returns the number of goroutines currently queued.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when the queue is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
