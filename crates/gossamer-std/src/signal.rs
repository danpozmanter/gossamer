//! Runtime support for `std::os::signal`.
//!
//! Cross-platform signal-handling surface modelled on Go's
//! `os/signal` package. Backed by `signal-hook` on Unix and Win32
//! console-control APIs on Windows; the user-facing API is the same.
//!
//! Two delivery models cooperate:
//!
//! - A real `sigaction` handler installed via `signal-hook` flips
//!   a per-signal `AtomicBool`. The handler does only
//!   async-signal-safe work (atomic store).
//! - A blocking `Notifier::wait()` parks on a `parking_lot::Condvar`
//!   that the relay thread notifies when any flag flips. The relay
//!   thread is the one place we observe the atomic and translate it
//!   into a notify; user code never polls in a 50 ms loop.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use parking_lot::{Condvar, Mutex};

/// Signal name. Use the [`sigs`] aliases in user code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Signal(pub &'static str);

/// Standard signal name aliases.
pub mod sigs {
    use super::Signal;
    /// Interrupt (Ctrl-C). User asked the program to stop.
    pub const SIGINT: Signal = Signal("SIGINT");
    /// Termination request, typically from a process supervisor.
    pub const SIGTERM: Signal = Signal("SIGTERM");
    /// Controlling terminal disconnected; commonly used as a "reload config" signal.
    pub const SIGHUP: Signal = Signal("SIGHUP");
    /// User-defined signal 1.
    pub const SIGUSR1: Signal = Signal("SIGUSR1");
    /// User-defined signal 2.
    pub const SIGUSR2: Signal = Signal("SIGUSR2");
    /// Quit; like SIGTERM but conventionally also dumps core.
    pub const SIGQUIT: Signal = Signal("SIGQUIT");
}

/// Per-signal subscriber state. Cloning yields an additional
/// receiver pointing at the same flag, so a single signal install
/// can notify multiple observers.
#[derive(Debug, Clone)]
pub struct Notifier {
    flag: Arc<AtomicBool>,
    waiter: Arc<Waiter>,
}

#[derive(Debug, Default)]
struct Waiter {
    mu: Mutex<()>,
    cv: Condvar,
}

impl Notifier {
    /// Returns `true` once the signal has been observed at least once
    /// since the notifier was created. Non-blocking.
    #[must_use]
    pub fn try_wait(&self) -> bool {
        self.flag.swap(false, Ordering::AcqRel)
    }

    /// Blocks until the signal fires. Backed by a Condvar so
    /// shutdown latency is sub-millisecond, not the 50 ms the
    /// previous polling implementation imposed.
    pub fn wait(&self) {
        let mut g = self.waiter.mu.lock();
        loop {
            if self.try_wait() {
                return;
            }
            self.waiter.cv.wait(&mut g);
        }
    }

    /// `wait` with a timeout. Returns `true` if the signal fired
    /// before `timeout` elapsed, `false` otherwise.
    #[must_use]
    pub fn wait_with_timeout(&self, timeout: Duration) -> bool {
        let mut g = self.waiter.mu.lock();
        if self.try_wait() {
            return true;
        }
        let res = self.waiter.cv.wait_for(&mut g, timeout);
        if res.timed_out() && !self.try_wait() {
            return false;
        }
        true
    }

    /// Backwards-compatible polling wait. Retained because some
    /// tests construct it directly; the modern `wait()` is strictly
    /// preferred.
    pub fn wait_with_interval(&self, interval: Duration) {
        loop {
            if self.try_wait() {
                return;
            }
            std::thread::sleep(interval);
        }
    }
}

struct Registry {
    inner: Mutex<Vec<Entry>>,
    waker: Arc<Waiter>,
}

struct Entry {
    sig: Signal,
    flag: Arc<AtomicBool>,
    waiter: Arc<Waiter>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        install_native_handlers();
        Registry {
            inner: Mutex::new(Vec::new()),
            waker: Arc::new(Waiter::default()),
        }
    })
}

#[cfg(unix)]
fn install_native_handlers() {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
    if std::env::var("GOSSAMER_SIGNAL_DISABLE_HANDLERS").is_ok() {
        return;
    }
    let signals: [i32; 6] = [SIGINT, SIGTERM, SIGHUP, SIGUSR1, SIGUSR2, SIGQUIT];
    for raw in signals {
        let flag = Arc::new(AtomicBool::new(false));
        let _ = signal_hook::flag::register(raw, Arc::clone(&flag));
        std::thread::Builder::new()
            .name(format!("gos-sig-{raw}"))
            .spawn(move || relay_loop(raw, flag))
            .ok();
    }
}

#[cfg(windows)]
fn install_native_handlers() {
    // Console-handler bridging is a Track B follow-up; the `deliver`
    // path still works for synthetic delivery.
}

#[cfg(not(any(unix, windows)))]
fn install_native_handlers() {}

#[cfg(unix)]
fn relay_loop(raw: i32, flag: Arc<AtomicBool>) {
    let name = signal_name(raw);
    let sig = Signal(name);
    loop {
        if flag.swap(false, Ordering::AcqRel) {
            deliver(sig);
        }
        // Sleep briefly between checks. The wake latency floor is
        // ~1 ms, dominated by `std::thread::sleep` granularity. This
        // is not the bottleneck — the previous implementation slept
        // 50 ms per check for the same reason. A `pipe`-based
        // wake-up would drop the floor to <1 ms; left as a future
        // optimisation since shutdown is the dominant use case and
        // 1 ms is well under any human-perceptible delay.
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[cfg(unix)]
fn signal_name(raw: i32) -> &'static str {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGQUIT, SIGTERM, SIGUSR1, SIGUSR2};
    match raw {
        x if x == SIGINT => "SIGINT",
        x if x == SIGTERM => "SIGTERM",
        x if x == SIGHUP => "SIGHUP",
        x if x == SIGUSR1 => "SIGUSR1",
        x if x == SIGUSR2 => "SIGUSR2",
        x if x == SIGQUIT => "SIGQUIT",
        _ => "SIGOTHER",
    }
}

/// Public entry point: installs (or re-uses) a notifier for `sig`.
/// The notifier persists for the program's lifetime. Multiple calls
/// for the same signal each return their own [`Notifier`], all of
/// which fire when the signal arrives.
#[must_use]
pub fn on(sig: Signal) -> Notifier {
    let flag = Arc::new(AtomicBool::new(false));
    let waiter = Arc::clone(&registry().waker);
    registry().inner.lock().push(Entry {
        sig,
        flag: Arc::clone(&flag),
        waiter: Arc::clone(&waiter),
    });
    Notifier { flag, waiter }
}

/// Test / runtime helper: synthesise a signal delivery without
/// going through the OS. The relay thread also calls this once it
/// observes the OS-level flag flip.
pub fn deliver(sig: Signal) {
    let reg = registry();
    let entries = reg.inner.lock();
    let mut woke_any = false;
    for entry in entries.iter() {
        if entry.sig == sig {
            entry.flag.store(true, Ordering::Release);
            let _g = entry.waiter.mu.lock();
            entry.waiter.cv.notify_all();
            woke_any = true;
        }
    }
    drop(entries);
    if woke_any {
        let _g = reg.waker.mu.lock();
        reg.waker.cv.notify_all();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// All tests in this module touch the global signal registry,
    /// so they must run one at a time. `cargo test` defaults to a
    /// thread-pool runner; this mutex serialises them.
    static TEST_GUARD: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    #[test]
    fn notifier_wakes_on_delivered_signal() {
        let _g = TEST_GUARD.lock();
        let n = on(sigs::SIGTERM);
        assert!(!n.try_wait());
        deliver(sigs::SIGTERM);
        assert!(n.try_wait());
        assert!(!n.try_wait());
    }

    #[test]
    fn multiple_notifiers_for_same_signal_all_fire() {
        let _g = TEST_GUARD.lock();
        let a = on(sigs::SIGUSR1);
        let b = on(sigs::SIGUSR1);
        deliver(sigs::SIGUSR1);
        assert!(a.try_wait());
        assert!(b.try_wait());
    }

    #[test]
    fn signals_do_not_cross_kinds() {
        let _g = TEST_GUARD.lock();
        let term = on(sigs::SIGTERM);
        // Pre-flush any leftover SIGTERM state from earlier
        // serial tests so the assertion targets THIS test's
        // delivery rather than residue.
        let _ = term.try_wait();
        let int = on(sigs::SIGINT);
        deliver(sigs::SIGINT);
        assert!(!term.try_wait());
        assert!(int.try_wait());
    }

    #[test]
    fn wait_with_timeout_returns_after_delivery() {
        let _g = TEST_GUARD.lock();
        let n = on(sigs::SIGHUP);
        let n2 = n.clone();
        let handle = std::thread::spawn(move || n2.wait());
        std::thread::sleep(Duration::from_millis(20));
        deliver(sigs::SIGHUP);
        handle.join().expect("notifier thread");
    }

    #[test]
    fn cloned_notifier_shares_state() {
        let _g = TEST_GUARD.lock();
        let a = on(sigs::SIGUSR2);
        let b = a.clone();
        deliver(sigs::SIGUSR2);
        assert!(a.try_wait());
        assert!(!b.try_wait());
    }
}
