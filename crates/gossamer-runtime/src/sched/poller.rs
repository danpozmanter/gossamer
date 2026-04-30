//! Network poller abstraction.
//!
//! The poller is the bridge between a parked goroutine waiting on
//! I/O readiness and the OS-level mechanism that delivers it. Two
//! concrete implementations live here:
//!
//! - [`MockPoller`] — deterministic, in-memory; readiness events are
//!   synthesised by the caller. Used by unit tests.
//! - [`OsPoller`] — `mio`-backed, wraps `epoll` (Linux), `kqueue`
//!   (macOS/BSD), or IOCP (Windows). Production runtime path.
//!
//! Both implementations satisfy the [`Poller`] trait. The scheduler
//! integrates with the trait, never with the underlying OS handle,
//! so swapping implementations during tests is mechanical.
//!
//! Timers piggy-back on the same event loop: callers register a
//! deadline through [`OsPoller::add_timer`]; the next call to
//! [`Poller::poll`] returns timer firings as ordinary [`Readiness`]
//! events with `interest == Interest::Timer`.

#![forbid(unsafe_code)]

use std::collections::{BinaryHeap, HashMap};
use std::io;
use std::time::{Duration, Instant};

use super::task::Gid;

/// Opaque identifier for a registered I/O source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PollSource(pub u32);

/// Direction (or kind) a goroutine is waiting on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Interest {
    /// Wait for the source to become readable.
    Readable,
    /// Wait for the source to become writable.
    Writable,
    /// Synthetic kind used for timer firings.
    Timer,
}

/// Ready event returned from the poller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Readiness {
    /// Source that is now ready (or the timer id for timer events).
    pub source: PollSource,
    /// Direction that fired.
    pub interest: Interest,
    /// Goroutine to resume.
    pub gid: Gid,
}

/// Minimal interface a poller must satisfy.
pub trait Poller: Send {
    /// Registers a goroutine's interest in `source` firing.
    fn register(&mut self, source: PollSource, interest: Interest, gid: Gid);

    /// Removes any outstanding registration matching `source` +
    /// `interest`.
    fn deregister(&mut self, source: PollSource, interest: Interest);

    /// Drains every readiness event accumulated since the last call,
    /// without blocking.
    fn drain(&mut self) -> Vec<Readiness>;

    /// Blocks until at least one event is ready or `timeout` elapses,
    /// then drains the readiness queue. Implementations are free to
    /// return early if `timeout` is `Some(Duration::ZERO)`.
    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Readiness>> {
        // Default fallback for pollers that do not have a real
        // blocking primitive — sleeps and then drains.
        if let Some(t) = timeout {
            std::thread::sleep(t);
        }
        Ok(self.drain())
    }
}

/// Deterministic in-memory poller used by tests and by platforms that
/// do not yet have an OS backend. Callers fire readiness events
/// explicitly via [`MockPoller::fire`].
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

/// Min-heap entry for the timer wheel. Sorted by expiry; the entry
/// with the soonest deadline pops first.
#[derive(Debug, Clone, Copy)]
struct TimerEntry {
    deadline: Instant,
    source: PollSource,
    gid: Gid,
}

impl PartialEq for TimerEntry {
    fn eq(&self, other: &Self) -> bool {
        self.deadline == other.deadline && self.source == other.source
    }
}

impl Eq for TimerEntry {}

impl PartialOrd for TimerEntry {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for TimerEntry {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse: BinaryHeap is max-heap by default and we want the
        // *earliest* deadline at the top.
        other
            .deadline
            .cmp(&self.deadline)
            .then_with(|| other.source.0.cmp(&self.source.0))
    }
}

/// Production poller backed by `mio` (epoll / kqueue / IOCP).
///
/// The poller owns a `mio::Poll` plus a `mio::Registry`, and tracks
/// every registered source in a side map so deregister calls can
/// look up the underlying [`mio::event::Source`] handle. Sources are
/// registered by the network code via [`OsPoller::register_io`],
/// which wraps the registration with the [`Interest`] -> mio
/// translation.
pub struct OsPoller {
    poll: mio::Poll,
    events: mio::Events,
    /// Map registered `PollSource` -> `(mio::Token, Gid)` so
    /// `deregister` can find the entry to remove.
    by_source: HashMap<(PollSource, Interest), Gid>,
    /// Pending readiness events accumulated between `poll` and
    /// `drain` calls.
    pending: Vec<Readiness>,
    /// Outstanding timer wheel.
    timers: BinaryHeap<TimerEntry>,
    /// Next free token id used when registering with mio.
    next_token: usize,
    /// Map from mio Token to `(PollSource, Gid, Interest)`.
    by_token: HashMap<mio::Token, (PollSource, Interest, Gid)>,
}

impl std::fmt::Debug for OsPoller {
    fn fmt(&self, out: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        out.debug_struct("OsPoller")
            .field("registered", &self.by_source.len())
            .field("pending", &self.pending.len())
            .field("timers", &self.timers.len())
            .finish_non_exhaustive()
    }
}

impl OsPoller {
    /// Builds a fresh OS-backed poller. Returns an error if the
    /// kernel rejects the underlying `epoll_create1` / `kqueue` /
    /// `CreateIoCompletionPort` syscall.
    pub fn new() -> io::Result<Self> {
        Ok(Self {
            poll: mio::Poll::new()?,
            events: mio::Events::with_capacity(1024),
            by_source: HashMap::new(),
            pending: Vec::new(),
            timers: BinaryHeap::new(),
            next_token: 1,
            by_token: HashMap::new(),
        })
    }

    /// Registers a goroutine `gid` to wake when `io` reports the
    /// requested `interest`. Returns the [`PollSource`] handle the
    /// caller should later use to deregister.
    pub fn register_io<S: mio::event::Source + ?Sized>(
        &mut self,
        io: &mut S,
        interest: Interest,
        gid: Gid,
    ) -> io::Result<PollSource> {
        let token = mio::Token(self.next_token);
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let source = PollSource(u32::try_from(token.0 & 0xFFFF_FFFF).unwrap_or(0));
        let mio_int = match interest {
            Interest::Readable => mio::Interest::READABLE,
            Interest::Writable => mio::Interest::WRITABLE,
            Interest::Timer => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Interest::Timer is reserved for add_timer",
                ));
            }
        };
        self.poll.registry().register(io, token, mio_int)?;
        self.by_source.insert((source, interest), gid);
        self.by_token.insert(token, (source, interest, gid));
        Ok(source)
    }

    /// Removes a previously registered source.
    pub fn deregister_io<S: mio::event::Source + ?Sized>(
        &mut self,
        io: &mut S,
        source: PollSource,
        interest: Interest,
    ) -> io::Result<()> {
        self.by_source.remove(&(source, interest));
        // Find and forget the matching token entry.
        let token_to_remove: Option<mio::Token> = self
            .by_token
            .iter()
            .find_map(|(t, (s, i, _))| (s == &source && i == &interest).then_some(*t));
        if let Some(t) = token_to_remove {
            self.by_token.remove(&t);
        }
        self.poll.registry().deregister(io)
    }

    /// Adds a one-shot timer that fires at `deadline`. Returns the
    /// [`PollSource`] handle that identifies this timer.
    pub fn add_timer(&mut self, deadline: Instant, gid: Gid) -> PollSource {
        let token = mio::Token(self.next_token);
        self.next_token = self.next_token.wrapping_add(1).max(1);
        let source = PollSource(u32::try_from(token.0 & 0xFFFF_FFFF).unwrap_or(0));
        self.timers.push(TimerEntry {
            deadline,
            source,
            gid,
        });
        source
    }

    /// Returns the duration until the next timer fires, or `None` if
    /// no timer is pending.
    fn next_timeout(&self, base: Option<Duration>) -> Option<Duration> {
        let now = Instant::now();
        let timer_dur = self.timers.peek().map(|entry| {
            if entry.deadline <= now {
                Duration::ZERO
            } else {
                entry.deadline - now
            }
        });
        match (base, timer_dur) {
            (None, t) => t,
            (Some(b), None) => Some(b),
            (Some(b), Some(t)) => Some(b.min(t)),
        }
    }

    fn drain_expired_timers(&mut self) {
        let now = Instant::now();
        while let Some(top) = self.timers.peek() {
            if top.deadline > now {
                break;
            }
            let entry = self.timers.pop().expect("peeked timer disappeared");
            self.pending.push(Readiness {
                source: entry.source,
                interest: Interest::Timer,
                gid: entry.gid,
            });
        }
    }
}

impl Poller for OsPoller {
    fn register(&mut self, source: PollSource, interest: Interest, gid: Gid) {
        // Type-erased entry point; the OsPoller's authoritative entry
        // is `register_io`. Recording in `by_source` is enough to
        // satisfy the trait contract for code paths that only use
        // synthetic sources (e.g. timers via `add_timer`).
        self.by_source.insert((source, interest), gid);
    }

    fn deregister(&mut self, source: PollSource, interest: Interest) {
        self.by_source.remove(&(source, interest));
    }

    fn drain(&mut self) -> Vec<Readiness> {
        std::mem::take(&mut self.pending)
    }

    fn poll(&mut self, timeout: Option<Duration>) -> io::Result<Vec<Readiness>> {
        // mio's `poll` can return early without events on every
        // platform — spurious wakeups, signal interruption, or
        // simply rounding the remaining timeout down to zero.
        // Loop until we have at least one event ready or the
        // caller-supplied deadline passes; recompute `combined`
        // each iteration so the remaining wait shrinks toward
        // both the user's timeout and the next timer's deadline.
        let user_deadline = timeout.map(|t| Instant::now() + t);
        loop {
            self.drain_expired_timers();
            if !self.pending.is_empty() {
                return Ok(self.drain());
            }
            let user_remaining = user_deadline.map(|d| d.saturating_duration_since(Instant::now()));
            if matches!(user_remaining, Some(d) if d.is_zero()) && self.timers.peek().is_none() {
                return Ok(self.drain());
            }
            let combined = self.next_timeout(user_remaining);
            self.poll.poll(&mut self.events, combined)?;
            for event in &self.events {
                let token = event.token();
                if let Some(&(source, interest, gid)) = self.by_token.get(&token) {
                    let fired = match interest {
                        Interest::Readable => event.is_readable(),
                        Interest::Writable => event.is_writable(),
                        Interest::Timer => false,
                    };
                    if fired {
                        self.pending.push(Readiness {
                            source,
                            interest,
                            gid,
                        });
                    }
                }
            }
            self.drain_expired_timers();
            if !self.pending.is_empty() {
                return Ok(self.drain());
            }
            // No events. If the user gave a timeout and it has
            // elapsed, return empty. Otherwise loop and re-poll.
            if let Some(d) = user_deadline {
                if Instant::now() >= d {
                    return Ok(self.drain());
                }
            } else if self.timers.peek().is_none() {
                // No deadline and no timer pending — re-polling
                // would block forever. Return empty.
                return Ok(self.drain());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn os_poller_round_trips_a_timer() {
        let mut poller = OsPoller::new().expect("OsPoller::new");
        let when = Instant::now() + Duration::from_millis(5);
        let _ = poller.add_timer(when, Gid(7));
        let events = poller
            .poll(Some(Duration::from_millis(50)))
            .expect("OsPoller::poll");
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].gid, Gid(7));
        assert!(matches!(events[0].interest, Interest::Timer));
    }

    #[test]
    fn os_poller_returns_empty_with_no_work() {
        let mut poller = OsPoller::new().expect("OsPoller::new");
        let events = poller
            .poll(Some(Duration::from_millis(1)))
            .expect("OsPoller::poll");
        assert!(events.is_empty());
    }

    #[test]
    fn next_timeout_picks_earlier_value() {
        let mut poller = OsPoller::new().expect("OsPoller::new");
        let when = Instant::now() + Duration::from_secs(60);
        let _ = poller.add_timer(when, Gid(1));
        let dt = poller.next_timeout(Some(Duration::from_millis(10)));
        // Caller's 10 ms is the earlier deadline.
        assert!(dt.unwrap() <= Duration::from_millis(10));
    }
}
