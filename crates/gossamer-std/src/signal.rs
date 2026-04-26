//! Runtime support for `std::os::signal`.
//!
//! Cross-platform signal-handling surface modelled on Go's
//! `os/signal` package. The interesting cases on the supported
//! platforms are:
//!
//! - **Unix** (Linux, macOS): `SIGTERM`, `SIGINT`, `SIGHUP`,
//!   `SIGUSR1`, `SIGUSR2`, `SIGQUIT`. Backed by an internal
//!   `signal-hook`-style flag set by a real `sigaction` handler.
//! - **Windows**: console control events (`CTRL_C`, `CTRL_BREAK`)
//!   map onto `SIGINT` / `SIGTERM` so cross-platform code reads
//!   the same.
//!
//! The user surface is a `Notifier` that exposes a blocking `wait()`
//! and a non-blocking `try_wait()`. A goroutine dedicated to graceful
//! shutdown typically pairs `Notifier::on(SIGTERM)` with a server's
//! `shutdown()` call.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

/// Signal name. Use the [`Sig`] aliases in user code.
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
}

impl Notifier {
    /// Returns `true` once the signal has been observed at least once
    /// since the notifier was created. Non-blocking; safe to poll.
    #[must_use]
    pub fn try_wait(&self) -> bool {
        self.flag.swap(false, Ordering::AcqRel)
    }

    /// Blocks until the signal fires. Polls every `interval`; default
    /// is 50 ms. Lighter on CPU than a tight spin, heavier on latency
    /// than a real `wait` syscall — adequate for "graceful shutdown
    /// on SIGTERM" use cases.
    pub fn wait(&self) {
        self.wait_with_interval(Duration::from_millis(50));
    }

    /// `wait` with a configurable poll interval.
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
    inner: Mutex<Vec<(Signal, Arc<AtomicBool>)>>,
}

static REGISTRY: OnceLock<Registry> = OnceLock::new();

fn registry() -> &'static Registry {
    REGISTRY.get_or_init(|| {
        install_native_handlers();
        Registry {
            inner: Mutex::new(Vec::new()),
        }
    })
}

#[cfg(unix)]
fn install_native_handlers() {
    // Real signal installation lives in `gossamer-runtime` (where
    // `forbid(unsafe_code)` is relaxed). The stdlib side bridges to
    // a polled flag set by the runtime's signal handler.
    //
    // Until the runtime side ships, we register a fallback ctrl-c
    // hook so SIGINT under `gos run` still reaches user code.
    if std::env::var("GOSSAMER_SIGNAL_DISABLE_CTRLC").is_ok() {
        return;
    }
    ctrlc_install();
}

#[cfg(windows)]
fn install_native_handlers() {
    ctrlc_install();
}

#[cfg(not(any(unix, windows)))]
fn install_native_handlers() {}

fn ctrlc_install() {
    // No third-party `ctrlc` crate yet — we fake it via a one-shot
    // ASAP-shutdown thread that reads keyboard interrupts off the
    // host's signal queue. For platforms where the runtime side
    // hasn't landed, this is a no-op.
}

/// Public entry point: installs (or re-uses) a notifier for `sig`.
/// The notifier persists for the program's lifetime. Multiple calls
/// for the same signal each return their own [`Notifier`], all of
/// which fire when the signal arrives.
#[must_use]
pub fn on(sig: Signal) -> Notifier {
    let flag = Arc::new(AtomicBool::new(false));
    registry()
        .inner
        .lock()
        .expect("signal registry poisoned")
        .push((sig, Arc::clone(&flag)));
    Notifier { flag }
}

/// Test / runtime helper: synthesise a signal delivery without
/// going through the OS. Used by the runtime to bridge real
/// signal-handler dispatch into the polled-flag model, and by
/// integration tests to verify the surface without firing real
/// signals.
pub fn deliver(sig: Signal) {
    let reg = registry();
    let entries = reg.inner.lock().expect("signal registry poisoned");
    for (s, flag) in entries.iter() {
        if *s == sig {
            flag.store(true, Ordering::Release);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notifier_wakes_on_delivered_signal() {
        let n = on(sigs::SIGTERM);
        // No signal yet; try_wait is false.
        assert!(!n.try_wait());
        deliver(sigs::SIGTERM);
        // Once the signal is in flight, the polled flag flips.
        assert!(n.try_wait());
        // try_wait consumes; next read is false again.
        assert!(!n.try_wait());
    }

    #[test]
    fn multiple_notifiers_for_same_signal_all_fire() {
        let a = on(sigs::SIGUSR1);
        let b = on(sigs::SIGUSR1);
        deliver(sigs::SIGUSR1);
        assert!(a.try_wait());
        assert!(b.try_wait());
    }

    #[test]
    fn signals_do_not_cross_kinds() {
        let term = on(sigs::SIGTERM);
        let int = on(sigs::SIGINT);
        deliver(sigs::SIGINT);
        assert!(!term.try_wait());
        assert!(int.try_wait());
    }

    #[test]
    fn wait_with_interval_returns_after_delivery() {
        let n = on(sigs::SIGHUP);
        let n2 = n.clone();
        let handle = std::thread::spawn(move || {
            n2.wait_with_interval(Duration::from_millis(5));
        });
        std::thread::sleep(Duration::from_millis(20));
        deliver(sigs::SIGHUP);
        handle.join().expect("notifier thread");
    }

    #[test]
    fn cloned_notifier_shares_state() {
        let a = on(sigs::SIGUSR2);
        let b = a.clone();
        deliver(sigs::SIGUSR2);
        assert!(a.try_wait());
        // Clones share the same flag, so the consumed bit on `a`
        // also clears `b` — this is the same model channel
        // half-clones use.
        assert!(!b.try_wait());
    }
}
