//! `select` over multiple channel operations.
//!
//! The Go specification requires that, when more than one arm of a
//! `select` is ready, the runtime picks one of the ready arms
//! uniformly at random — never deterministically. [`poll_select`]
//! enforces that by walking the arms in a permuted order seeded from
//! a fast thread-local PRNG. The randomisation is deliberately cheap
//! (xorshift64 over a thread-local seed) so a hot select loop does
//! not need to call into the OS RNG every poll.

#![forbid(unsafe_code)]

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

use super::channel::{Channel, RecvResult, SendResult};

thread_local! {
    /// Thread-local xorshift64 seed used to permute select arms.
    /// Lazily initialised from the wall clock + thread-id hash so
    /// every worker starts with a different sequence.
    static SELECT_RNG: Cell<u64> = const { Cell::new(0) };
}

fn next_random() -> u64 {
    SELECT_RNG.with(|cell| {
        let mut x = cell.get();
        if x == 0 {
            // SystemTime nanos give plenty of entropy for a per-thread
            // seed; XOR in the thread-id to ensure two workers with
            // identical wake-up timing still diverge immediately.
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0u128, |d| d.as_nanos());
            let tid = std::thread::current().id();
            // ThreadId Debug format like "ThreadId(N)" — hash the
            // string representation rather than rely on an unstable
            // accessor.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for byte in format!("{tid:?}").bytes() {
                h ^= u64::from(byte);
                h = h.wrapping_mul(0x100_0000_01b3);
            }
            x = (nanos as u64) ^ h ^ 0x9E37_79B9_7F4A_7C15;
            if x == 0 {
                x = 0xdead_beef_cafe_babe;
            }
        }
        // xorshift64 step.
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        cell.set(x);
        x
    })
}

/// Returns a permutation of `0..n` chosen uniformly at random.
/// Used by [`poll_select`] to enforce Go-spec arm fairness.
fn shuffled_indices(n: usize) -> Vec<usize> {
    let mut order: Vec<usize> = (0..n).collect();
    // Fisher-Yates using the thread-local xorshift.
    for i in (1..n).rev() {
        let j = (next_random() as usize) % (i + 1);
        order.swap(i, j);
    }
    order
}

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

/// Tries every arm of the select set in a permutation chosen
/// uniformly at random. The first arm in that order that can make
/// progress wins, matching Go's `select` spec ("If one or more of the
/// communications can proceed, a single one that can proceed is
/// chosen via a uniform pseudo-random selection.")
///
/// Returns [`SelectOutcome::WouldBlock`] when no arm is ready; the
/// caller is then expected to park itself on every operand channel.
#[must_use]
pub fn poll_select<T>(ops: Vec<SelectOp<'_, T>>) -> SelectOutcome<T> {
    let n = ops.len();
    let order = shuffled_indices(n);
    // Move ops into a Vec<Option<...>> so we can take individual arms
    // out in random order without disturbing their original indices.
    let mut wrapped: Vec<Option<SelectOp<'_, T>>> = ops.into_iter().map(Some).collect();
    for index in order {
        let Some(op) = wrapped[index].take() else {
            continue;
        };
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
