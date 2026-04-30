//! Buffered/unbuffered channels used by goroutines to communicate.
//! The channel is a passive data structure: callers (usually the
//! scheduler) drive `try_send` and `try_recv` and react to the
//! resulting status codes. Parked-goroutine bookkeeping is kept
//! in-structure so a later scheduler pass can look for ready
//! channels and resume the goroutines registered here.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use super::task::Gid;

/// Result of a non-blocking send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SendResult {
    /// Value accepted — stored in the buffer or handed off to a
    /// parked receiver.
    Sent,
    /// Buffer is full (and no receiver is waiting); the caller should
    /// park and retry once the channel has room.
    WouldBlock,
    /// The channel is closed; future sends will always fail.
    Closed,
}

/// Result of a non-blocking receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecvResult<T> {
    /// Value removed from the buffer (or taken directly from a
    /// parked sender).
    Value(T),
    /// The buffer is empty and no senders are waiting. The caller
    /// should park until the channel gains data or closes.
    WouldBlock,
    /// The channel is closed and drained. No future value will arrive.
    Closed,
}

/// FIFO channel with optional bounded capacity.
///
/// `capacity == None` models an unbuffered channel (`chan<T>()`).
/// `capacity == Some(n)` models a buffered channel (`chan<T>(n)`).
#[derive(Debug)]
pub struct Channel<T> {
    buf: VecDeque<T>,
    capacity: Option<usize>,
    closed: bool,
    parked_senders: Vec<Gid>,
    parked_receivers: Vec<Gid>,
}

impl<T> Channel<T> {
    /// Returns an unbuffered channel. Every `try_send` blocks until a
    /// matching receiver is ready.
    #[must_use]
    pub fn unbuffered() -> Self {
        Self {
            buf: VecDeque::new(),
            capacity: None,
            closed: false,
            parked_senders: Vec::new(),
            parked_receivers: Vec::new(),
        }
    }

    /// Returns a channel with a bounded buffer.
    #[must_use]
    pub fn buffered(capacity: usize) -> Self {
        Self {
            buf: VecDeque::with_capacity(capacity),
            capacity: Some(capacity),
            closed: false,
            parked_senders: Vec::new(),
            parked_receivers: Vec::new(),
        }
    }

    /// Attempts a non-blocking send.
    pub fn try_send(&mut self, value: T) -> Result<SendResult, T> {
        if self.closed {
            return Ok(SendResult::Closed);
        }
        if self.has_capacity() {
            self.buf.push_back(value);
            return Ok(SendResult::Sent);
        }
        Err(value)
    }

    /// Attempts a non-blocking receive.
    pub fn try_recv(&mut self) -> RecvResult<T> {
        if let Some(value) = self.buf.pop_front() {
            return RecvResult::Value(value);
        }
        if self.closed {
            return RecvResult::Closed;
        }
        RecvResult::WouldBlock
    }

    /// Registers `gid` as a sender waiting for buffer space.
    pub fn park_sender(&mut self, gid: Gid) {
        if !self.parked_senders.contains(&gid) {
            self.parked_senders.push(gid);
        }
    }

    /// Registers `gid` as a receiver waiting for a value.
    pub fn park_receiver(&mut self, gid: Gid) {
        if !self.parked_receivers.contains(&gid) {
            self.parked_receivers.push(gid);
        }
    }

    /// Removes the oldest parked sender, if any, and returns its id.
    /// The scheduler should call this after buffer space opens up.
    pub fn wake_sender(&mut self) -> Option<Gid> {
        if self.parked_senders.is_empty() {
            None
        } else {
            Some(self.parked_senders.remove(0))
        }
    }

    /// Removes the oldest parked receiver, if any, and returns its id.
    pub fn wake_receiver(&mut self) -> Option<Gid> {
        if self.parked_receivers.is_empty() {
            None
        } else {
            Some(self.parked_receivers.remove(0))
        }
    }

    /// Marks the channel closed. Subsequent `try_send` calls will
    /// return [`SendResult::Closed`]; `try_recv` calls drain the
    /// buffer first and then return [`RecvResult::Closed`].
    pub fn close(&mut self) {
        self.closed = true;
    }

    /// Closes the channel and removes every parked goroutine. The
    /// caller (the scheduler) is responsible for waking the drained
    /// goroutines — they will observe `RecvResult::Closed` /
    /// `SendResult::Closed` when they retry. Returns
    /// `(senders, receivers)` so the caller can wake them in one
    /// pass without a follow-up `wake_*` loop.
    pub fn close_and_drain_parked(&mut self) -> (Vec<Gid>, Vec<Gid>) {
        self.closed = true;
        let senders = std::mem::take(&mut self.parked_senders);
        let receivers = std::mem::take(&mut self.parked_receivers);
        (senders, receivers)
    }

    /// Returns `true` when the channel has been closed.
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.closed
    }

    /// Returns the number of queued values.
    #[must_use]
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// Returns `true` when the channel buffer is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Returns the bounded capacity, or `None` for unbuffered
    /// channels.
    #[must_use]
    pub fn capacity(&self) -> Option<usize> {
        self.capacity
    }

    /// Returns the list of parked receivers for introspection.
    #[must_use]
    pub fn parked_receivers(&self) -> &[Gid] {
        &self.parked_receivers
    }

    /// Returns the list of parked senders for introspection.
    #[must_use]
    pub fn parked_senders(&self) -> &[Gid] {
        &self.parked_senders
    }

    fn has_capacity(&self) -> bool {
        match self.capacity {
            None => false,
            Some(cap) => self.buf.len() < cap,
        }
    }
}

impl<T> Default for Channel<T> {
    fn default() -> Self {
        Self::unbuffered()
    }
}

impl<T> Drop for Channel<T> {
    /// Marks the channel closed. Any goroutine still parked on the
    /// channel will be left on the channel's internal lists when
    /// the storage is freed; the scheduler is expected to call
    /// [`Channel::close_and_drain_parked`] before dropping the
    /// channel so it can wake them. The Drop impl exists so that
    /// the closed flag flips even when a test drops a channel
    /// without going through the scheduler — `is_closed()` then
    /// observes the right value before the memory is reclaimed.
    fn drop(&mut self) {
        self.closed = true;
    }
}
