//! Network poller abstraction.
//! The production runtime will wire OS-level I/O readiness primitives
//! (`epoll`, `kqueue`, IOCP) in behind this trait. Ships the
//! trait + an in-memory [`MockPoller`] so scheduler code depending on
//! "wait for readiness" can be exercised deterministically in tests.

#![forbid(unsafe_code)]

use std::collections::HashMap;

use crate::task::Gid;

/// Opaque identifier for a registered I/O source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PollSource(pub u32);

/// Directions a goroutine can wait for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interest {
    /// Wait for the source to become readable.
    Readable,
    /// Wait for the source to become writable.
    Writable,
}

/// Ready-event returned from the poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    /// Source that is now ready.
    pub source: PollSource,
    /// Direction that fired.
    pub interest: Interest,
    /// Goroutine to resume.
    pub gid: Gid,
}

/// Minimal interface a real poller must satisfy.
pub trait Poller {
    /// Registers a goroutine's interest in `source` firing.
    fn register(&mut self, source: PollSource, interest: Interest, gid: Gid);

    /// Removes any outstanding registration matching `source` +
    /// `interest`.
    fn deregister(&mut self, source: PollSource, interest: Interest);

    /// Drains every fired readiness event accumulated since the last
    /// call.
    fn drain(&mut self) -> Vec<Readiness>;
}

/// Deterministic, in-memory poller used by tests and by platforms that
/// do not yet have an OS-specific implementation. Callers fire
/// readiness events explicitly via [`MockPoller::fire`].
#[derive(Debug, Default)]
pub struct MockPoller {
    registrations: HashMap<(PollSource, Interest), Gid>,
    pending: Vec<Readiness>,
}

impl MockPoller {
    /// Returns a fresh poller with no registered sources.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Synthesises a readiness event for `source` + `interest`. The
    /// corresponding goroutine, if registered, will be delivered on
    /// the next call to [`Poller::drain`].
    pub fn fire(&mut self, source: PollSource, interest: Interest) {
        if let Some(gid) = self.registrations.remove(&(source, interest)) {
            self.pending.push(Readiness {
                source,
                interest,
                gid,
            });
        }
    }
}

impl Poller for MockPoller {
    fn register(&mut self, source: PollSource, interest: Interest, gid: Gid) {
        self.registrations.insert((source, interest), gid);
    }

    fn deregister(&mut self, source: PollSource, interest: Interest) {
        self.registrations.remove(&(source, interest));
    }

    fn drain(&mut self) -> Vec<Readiness> {
        std::mem::take(&mut self.pending)
    }
}
