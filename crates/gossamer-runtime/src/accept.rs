//! Classification and pacing for the errors a listening socket answers.
//!
//! `accept(2)` names two very different kinds of failure under one return
//! value. One kind belongs to the connection being accepted or to a
//! resource the process is momentarily at the end of - the peer reset
//! before the handshake finished, the descriptor table is full, the
//! kernel had no buffer - and the listener answers the next client
//! normally. The other names the listening socket itself, and every
//! further call answers the same way.
//!
//! A loop that ends on the first kind stops serving forever because one
//! client went away or one descriptor was briefly unavailable, which is
//! why `accept` is retried here rather than propagated.

use std::io;
use std::time::Duration;

/// Errno values `accept(2)` documents as belonging to the connection or to
/// a momentary shortage. The manual page says to treat them as `EAGAIN`
/// and ask again.
#[cfg(unix)]
const TRANSIENT_OS_ERRORS: &[i32] = &[
    libc::EAGAIN,
    libc::ECONNABORTED,
    libc::EHOSTDOWN,
    libc::EHOSTUNREACH,
    libc::EINTR,
    libc::EMFILE,
    libc::ENETDOWN,
    libc::ENETUNREACH,
    libc::ENFILE,
    libc::ENOBUFS,
    libc::ENOMEM,
    libc::EPROTO,
    libc::ETIMEDOUT,
];

/// Winsock's counterparts, by value: `windows-sys` publishes them as
/// `WSAE*` constants of a different integer type than `raw_os_error`
/// answers, and the numbers are fixed by the API.
#[cfg(windows)]
const TRANSIENT_OS_ERRORS: &[i32] = &[
    10004, // WSAEINTR
    10024, // WSAEMFILE
    10035, // WSAEWOULDBLOCK
    10050, // WSAENETDOWN
    10051, // WSAENETUNREACH
    10053, // WSAECONNABORTED
    10054, // WSAECONNRESET
    10055, // WSAENOBUFS
    10060, // WSAETIMEDOUT
    10065, // WSAEHOSTUNREACH
];

#[cfg(not(any(unix, windows)))]
const TRANSIENT_OS_ERRORS: &[i32] = &[];

/// Whether the listener still answers clients after `error`, so the loop
/// pauses and calls `accept` again rather than ending the server.
#[must_use]
pub fn accept_error_is_transient(error: &io::Error) -> bool {
    if matches!(
        error.kind(),
        io::ErrorKind::Interrupted
            | io::ErrorKind::WouldBlock
            | io::ErrorKind::TimedOut
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::OutOfMemory
    ) {
        return true;
    }
    error
        .raw_os_error()
        .is_some_and(|code| TRANSIENT_OS_ERRORS.contains(&code))
}

/// The pause an accept loop takes between retries, doubling while the
/// shortage lasts and reset by the next client.
///
/// A descriptor table that is full stays full until something closes, so
/// asking again immediately spins a core for as long as the pressure
/// lasts; the ceiling keeps the wait short enough that a server is
/// serving again within a second of the pressure lifting.
#[derive(Debug, Clone, Copy)]
pub struct AcceptBackoff {
    delay: Duration,
}

impl AcceptBackoff {
    const FIRST: Duration = Duration::from_millis(5);
    const CEILING: Duration = Duration::from_secs(1);

    /// A backoff at its shortest pause.
    #[must_use]
    pub const fn new() -> Self {
        Self { delay: Self::FIRST }
    }

    /// Waits out this failure's pause before the loop asks again.
    pub fn settle(&mut self) {
        crate::platform::sleep(self.take());
    }

    /// Returns to the shortest pause; a client got through.
    pub fn reset(&mut self) {
        self.delay = Self::FIRST;
    }

    /// This failure's pause, lengthening the next one toward the ceiling.
    fn take(&mut self) -> Duration {
        let current = self.delay;
        self.delay = (self.delay * 2).min(Self::CEILING);
        current
    }
}

impl Default for AcceptBackoff {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_descriptor_table_is_transient() {
        #[cfg(unix)]
        let full = io::Error::from_raw_os_error(libc::EMFILE);
        #[cfg(windows)]
        let full = io::Error::from_raw_os_error(10024);
        #[cfg(not(any(unix, windows)))]
        let full = io::Error::from(io::ErrorKind::OutOfMemory);
        assert!(accept_error_is_transient(&full));
    }

    #[test]
    fn a_peer_that_left_during_the_handshake_is_transient() {
        assert!(accept_error_is_transient(&io::Error::from(
            io::ErrorKind::ConnectionAborted
        )));
        assert!(accept_error_is_transient(&io::Error::from(
            io::ErrorKind::Interrupted
        )));
    }

    #[test]
    fn a_closed_listener_is_not_transient() {
        #[cfg(unix)]
        let closed = io::Error::from_raw_os_error(libc::EBADF);
        #[cfg(not(unix))]
        let closed = io::Error::from(io::ErrorKind::InvalidInput);
        assert!(!accept_error_is_transient(&closed));
    }

    #[test]
    fn backoff_doubles_to_a_ceiling_and_resets() {
        let mut backoff = AcceptBackoff::new();
        assert_eq!(backoff.take(), AcceptBackoff::FIRST);
        assert_eq!(backoff.take(), AcceptBackoff::FIRST * 2);
        for _ in 0..12 {
            let _ = backoff.take();
        }
        assert_eq!(backoff.take(), AcceptBackoff::CEILING);
        backoff.reset();
        assert_eq!(backoff.take(), AcceptBackoff::FIRST);
    }
}
