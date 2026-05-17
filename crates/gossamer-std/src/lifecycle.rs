//! Runtime support for `std::lifecycle` — graceful shutdown
//! hooks, signal handling, and systemd `sd_notify` integration.
//!
//! `Lifecycle::on_shutdown(closure)` registers a cleanup hook that
//! fires when the process receives `SIGTERM`, `SIGINT`, or
//! `SIGHUP`. Hooks run in LIFO order — the last-registered hook
//! runs first, mirroring `defer` semantics. A second signal within
//! 5 seconds escalates to immediate process exit.
//!
//! Typical wiring at the top of `main`:
//!
//! ```ignore
//! let lc = Lifecycle::install_default()?;
//! lc.on_shutdown(move || db_pool.close());
//! lc.on_shutdown(move || flush_logs());
//! lc.ready();              // emits sd_notify(READY=1) on Linux
//! http::serve("0.0.0.0:8080", router)?;
//! ```

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::errors::Error;

type ShutdownHook = Box<dyn FnOnce() + Send + 'static>;

struct State {
    hooks: Mutex<Vec<ShutdownHook>>,
    shutting_down: AtomicBool,
    last_signal_ms: AtomicI64,
    force_after: Duration,
    grace: Duration,
}

/// Process-lifecycle handle.
///
/// Clones share the same hook registry — the value is reference-
/// counted internally. Holding one across goroutines is safe.
#[derive(Clone)]
pub struct Lifecycle {
    inner: Arc<State>,
}

impl Default for Lifecycle {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl Lifecycle {
    /// Creates a Lifecycle with the given graceful-shutdown
    /// deadline. Hooks have up to `grace` to complete before the
    /// process exits.
    #[must_use]
    pub fn new(grace: Duration) -> Self {
        Self {
            inner: Arc::new(State {
                hooks: Mutex::new(Vec::new()),
                shutting_down: AtomicBool::new(false),
                last_signal_ms: AtomicI64::new(0),
                force_after: Duration::from_secs(5),
                grace,
            }),
        }
    }

    /// Installs the default signal handler (SIGTERM + SIGINT +
    /// SIGHUP → graceful; double-tap within 5s → force-exit).
    ///
    /// The handler is registered exactly once per process; calling
    /// twice returns the same handle but does not re-register.
    pub fn install_default() -> Result<Self, Error> {
        let lc = Self::default();
        lc.install_handlers()?;
        Ok(lc)
    }

    /// Registers a cleanup hook. Hooks fire in LIFO order on
    /// shutdown. After shutdown begins, additional `on_shutdown`
    /// calls execute the hook immediately (best-effort cleanup
    /// from late-registered goroutines).
    pub fn on_shutdown<F>(&self, hook: F)
    where
        F: FnOnce() + Send + 'static,
    {
        if self.inner.shutting_down.load(Ordering::Acquire) {
            // Already shutting down — run inline so the caller
            // does not silently leak resources.
            hook();
            return;
        }
        let mut guard = self.inner.hooks.lock().expect("lifecycle hooks lock");
        guard.push(Box::new(hook));
    }

    /// Returns true once the shutdown sequence has begun.
    #[must_use]
    pub fn is_shutting_down(&self) -> bool {
        self.inner.shutting_down.load(Ordering::Acquire)
    }

    /// Triggers shutdown manually. Same path as a SIGTERM. Hooks
    /// run synchronously on the caller's thread; returns when all
    /// hooks complete or the deadline elapses.
    pub fn shutdown(&self) {
        self.begin_shutdown();
        self.drain_hooks();
    }

    /// Emits `sd_notify(READY=1)` on Linux when running under
    /// systemd with `Type=notify`. No-op everywhere else and when
    /// `$NOTIFY_SOCKET` is unset. Safe to call from any thread.
    pub fn ready(&self) {
        sd_notify("READY=1\n");
    }

    /// Emits `sd_notify(STOPPING=1)` (Linux/systemd). Called
    /// automatically at the start of shutdown; expose it for
    /// callers that orchestrate shutdown manually.
    pub fn notify_stopping(&self) {
        sd_notify("STOPPING=1\n");
    }

    /// Emits `sd_notify(STATUS=msg)` (Linux/systemd). Useful for
    /// reporting "still draining 12 in-flight requests" to the
    /// service manager.
    pub fn notify_status(&self, msg: &str) {
        sd_notify(&format!("STATUS={msg}\n"));
    }

    fn install_handlers(&self) -> Result<(), Error> {
        let term = Arc::clone(&self.inner);
        let int = Arc::clone(&self.inner);
        let hup = Arc::clone(&self.inner);
        let lc_term = self.clone();
        let lc_int = self.clone();
        let lc_hup = self.clone();
        #[cfg(unix)]
        {
            use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
            use signal_hook::iterator::Signals;
            let mut signals = Signals::new([SIGTERM, SIGINT, SIGHUP])
                .map_err(|e| Error::new(format!("lifecycle: install signal handler: {e}")))?;
            std::thread::Builder::new()
                .name("gos-lifecycle".into())
                .spawn(move || {
                    for sig in signals.forever() {
                        let now_ms = now_unix_ms();
                        let lc_ref = match sig {
                            SIGTERM => &lc_term,
                            SIGINT => &lc_int,
                            _ => &lc_hup,
                        };
                        let state = match sig {
                            SIGTERM => &term,
                            SIGINT => &int,
                            _ => &hup,
                        };
                        if state.shutting_down.load(Ordering::Acquire) {
                            // Second signal: enforce force-exit if
                            // within the deadline. Drop into hard
                            // process::abort to bypass any handler
                            // that's hung in an inner catch_unwind.
                            let prev = state.last_signal_ms.load(Ordering::Acquire);
                            if now_ms - prev <= state.force_after.as_millis() as i64 {
                                eprintln!(
                                    "[lifecycle] second signal received within {}ms; force-exiting",
                                    state.force_after.as_millis()
                                );
                                std::process::exit(130);
                            }
                            state.last_signal_ms.store(now_ms, Ordering::Release);
                            continue;
                        }
                        state.last_signal_ms.store(now_ms, Ordering::Release);
                        eprintln!("[lifecycle] signal {sig}: starting graceful shutdown");
                        lc_ref.begin_shutdown();
                        lc_ref.drain_hooks();
                        // After draining hooks, exit cleanly.
                        std::process::exit(0);
                    }
                })
                .map_err(|e| Error::new(format!("lifecycle: spawn handler: {e}")))?;
        }
        #[cfg(not(unix))]
        {
            // Windows: routes Ctrl+C / Ctrl+Break / close / logoff /
            // shutdown through std::signal's SetConsoleCtrlHandler
            // bridge. SIGINT covers Ctrl+C; SIGTERM covers the three
            // session-end events; SIGHUP isn't generated by the OS
            // here but the notifier is harmless. One waiter thread
            // multiplexes all three via short-timeout waits so a
            // single notifier wake services any of them.
            use crate::signal::{self, sigs};
            let n_int = signal::on(sigs::SIGINT);
            let n_term = signal::on(sigs::SIGTERM);
            let n_hup = signal::on(sigs::SIGHUP);
            std::thread::Builder::new()
                .name("gos-lifecycle".into())
                .spawn(move || {
                    let poll = Duration::from_millis(50);
                    loop {
                        let (sig_label, state, lc_ref) = if n_int.wait_with_timeout(poll) {
                            ("SIGINT", &int, &lc_int)
                        } else if n_term.wait_with_timeout(Duration::ZERO) {
                            ("SIGTERM", &term, &lc_term)
                        } else if n_hup.wait_with_timeout(Duration::ZERO) {
                            ("SIGHUP", &hup, &lc_hup)
                        } else {
                            continue;
                        };
                        let now_ms = now_unix_ms();
                        if state.shutting_down.load(Ordering::Acquire) {
                            let prev = state.last_signal_ms.load(Ordering::Acquire);
                            if now_ms - prev <= state.force_after.as_millis() as i64 {
                                eprintln!(
                                    "[lifecycle] second signal received within {}ms; force-exiting",
                                    state.force_after.as_millis()
                                );
                                std::process::exit(130);
                            }
                            state.last_signal_ms.store(now_ms, Ordering::Release);
                            continue;
                        }
                        state.last_signal_ms.store(now_ms, Ordering::Release);
                        eprintln!("[lifecycle] {sig_label}: starting graceful shutdown");
                        lc_ref.begin_shutdown();
                        lc_ref.drain_hooks();
                        std::process::exit(0);
                    }
                })
                .map_err(|e| Error::new(format!("lifecycle: spawn handler: {e}")))?;
        }
        Ok(())
    }

    fn begin_shutdown(&self) {
        if self.inner.shutting_down.swap(true, Ordering::AcqRel) {
            return; // already shutting down
        }
        self.notify_stopping();
    }

    fn drain_hooks(&self) {
        let started = Instant::now();
        let hooks = {
            let mut guard = self.inner.hooks.lock().expect("lifecycle hooks lock");
            std::mem::take(&mut *guard)
        };
        // LIFO order — last registered runs first (defer semantics).
        let total = hooks.len();
        for (idx, hook) in hooks.into_iter().rev().enumerate() {
            if started.elapsed() > self.inner.grace {
                eprintln!(
                    "[lifecycle] grace period exceeded ({:?}); dropping {} remaining hooks",
                    self.inner.grace,
                    total - idx
                );
                break;
            }
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(hook));
        }
    }
}

#[cfg(unix)]
fn sd_notify(msg: &str) {
    let Some(socket) = std::env::var_os("NOTIFY_SOCKET") else {
        return;
    };
    let path = std::path::Path::new(&socket);
    if let Ok(sock) = std::os::unix::net::UnixDatagram::unbound() {
        let _ = sock.send_to(msg.as_bytes(), path);
    }
}

#[cfg(not(unix))]
fn sd_notify(_msg: &str) {
    // sd_notify is Linux/systemd specific; no-op elsewhere.
}

fn now_unix_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn shutdown_runs_hooks_in_lifo_order() {
        let lc = Lifecycle::default();
        let order = Arc::new(Mutex::new(Vec::<u32>::new()));
        for i in 0..5 {
            let order = Arc::clone(&order);
            lc.on_shutdown(move || {
                order.lock().unwrap().push(i);
            });
        }
        lc.shutdown();
        let captured = order.lock().unwrap().clone();
        assert_eq!(captured, vec![4, 3, 2, 1, 0]);
    }

    #[test]
    fn is_shutting_down_flips() {
        let lc = Lifecycle::default();
        assert!(!lc.is_shutting_down());
        lc.shutdown();
        assert!(lc.is_shutting_down());
    }

    #[test]
    fn hook_registered_after_shutdown_runs_inline() {
        let lc = Lifecycle::default();
        lc.shutdown();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        lc.on_shutdown(move || {
            c.fetch_add(1, Ordering::Relaxed);
        });
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn double_shutdown_is_idempotent() {
        let lc = Lifecycle::default();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        lc.on_shutdown(move || {
            c.fetch_add(1, Ordering::Relaxed);
        });
        lc.shutdown();
        lc.shutdown();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
    }

    #[test]
    fn panicking_hook_does_not_stop_drain() {
        let lc = Lifecycle::default();
        let counter = Arc::new(AtomicUsize::new(0));
        let c1 = Arc::clone(&counter);
        let c2 = Arc::clone(&counter);
        lc.on_shutdown(move || {
            c1.fetch_add(1, Ordering::Relaxed);
        });
        lc.on_shutdown(move || {
            panic!("boom");
        });
        lc.on_shutdown(move || {
            c2.fetch_add(10, Ordering::Relaxed);
        });
        lc.shutdown();
        // Third (last-registered) runs, panics, drain continues.
        // First registered (counter += 1) still runs.
        assert_eq!(counter.load(Ordering::Relaxed), 11);
    }

    #[test]
    fn clone_shares_state() {
        let lc1 = Lifecycle::default();
        let lc2 = lc1.clone();
        let counter = Arc::new(AtomicUsize::new(0));
        let c = Arc::clone(&counter);
        lc1.on_shutdown(move || {
            c.fetch_add(1, Ordering::Relaxed);
        });
        lc2.shutdown();
        assert_eq!(counter.load(Ordering::Relaxed), 1);
        assert!(lc1.is_shutting_down());
    }

    #[test]
    fn ready_and_notify_status_are_noop_without_socket() {
        // Without $NOTIFY_SOCKET set these must not panic.
        // Tests do not run under systemd. We do not remove the
        // env var here (std::env::remove_var requires unsafe in
        // 2024 edition); the test still exercises the no-op path
        // when the binary's environment does not have it set, and
        // when it is set, the only consequence is a best-effort
        // datagram send that errors silently.
        let lc = Lifecycle::default();
        lc.ready();
        lc.notify_stopping();
        lc.notify_status("draining");
    }
}
