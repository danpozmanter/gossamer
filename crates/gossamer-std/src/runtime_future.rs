//! Goroutine-driven future-polling bridge.
//!
//! Gossamer's scheduler is the only executor in this build. To
//! use async crates like `h2` (which take an `AsyncRead +
//! AsyncWrite` source and return `Future`s for connection /
//! stream operations) we provide a thin "driver" that
//!
//! 1. runs inside a regular goroutine,
//! 2. polls a future once,
//! 3. on `Poll::Pending`, parks the goroutine via the existing
//!    scheduler park primitive,
//! 4. resumes on `unpark(gid)` — which is what every netpoller
//!    wakeup and every `Waker::wake()` call resolves to.
//!
//! There is no nested executor, no `block_on`, no tokio runtime,
//! no `LocalPool`. The goroutine itself is the executor and the
//! scheduler handles wakeups.
//!
//! See `HTTP_H2_ARCH.md` for the broader architecture.

#![forbid(unsafe_code)]
#![allow(
    clippy::missing_errors_doc,
    clippy::module_name_repetitions,
    clippy::missing_panics_doc
)]

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Wake, Waker};

use gossamer_runtime::sched::{Gid, ParkReason};

use crate::sched_global;

/// Constructs a `std::task::Waker` whose `wake()` calls
/// `scheduler::unpark(gid)`. Used inside the future driver.
#[must_use]
pub fn goroutine_waker(gid: Gid) -> Waker {
    Waker::from(Arc::new(GoroutineWaker {
        gid,
        woke: AtomicBool::new(false),
    }))
}

/// Internal waker implementation. The `woke` flag exists so the
/// driver can short-circuit a park if a wake fired during the
/// poll itself — closing the classic park-after-wake race window
/// without depending on the scheduler's `pre_unpark` machinery.
struct GoroutineWaker {
    gid: Gid,
    woke: AtomicBool,
}

impl Wake for GoroutineWaker {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }
    fn wake_by_ref(self: &Arc<Self>) {
        self.woke.store(true, Ordering::Release);
        sched_global::scheduler().unpark(self.gid);
    }
}

/// Polls `fut` to completion on the calling goroutine. The
/// future's deepest layers (e.g. an `h2::Connection` driving a
/// socket) wake the same gid via either the netpoller (gid-based
/// readiness) or the std `Waker` returned from [`goroutine_waker`].
///
/// Both routes call `scheduler::unpark(gid)` which resumes this
/// driver loop. Idempotent on multiple wakes — the driver simply
/// re-polls.
///
/// # Panics
///
/// Panics if the calling thread is not driving a goroutine.
/// Production paths spawn the driver via `go fn() { drive(fut) }`
/// or, in Rust-level harnesses, via
/// `gossamer_runtime::sched_global::scheduler().spawn(...)`.
pub fn drive<F>(fut: F) -> F::Output
where
    F: Future,
{
    let gid =
        sched_global::current_gid().expect("runtime_future::drive must run inside a goroutine");
    drive_with_gid(fut, gid)
}

fn drive_with_gid<F>(fut: F, gid: Gid) -> F::Output
where
    F: Future,
{
    let waker_arc = Arc::new(GoroutineWaker {
        gid,
        woke: AtomicBool::new(false),
    });
    let waker: Waker = waker_arc.clone().into();
    let mut ctx = Context::from_waker(&waker);
    // SAFETY: this Pin is created via the stable `pin!` macro
    // equivalent (Box::pin). The future never escapes the
    // function so the pinning invariant holds for the lifetime
    // of the loop.
    let mut pinned: Pin<Box<F>> = Box::pin(fut);
    loop {
        // Consume any wake events queued before this poll.
        waker_arc.woke.store(false, Ordering::Release);
        match pinned.as_mut().poll(&mut ctx) {
            Poll::Ready(value) => return value,
            Poll::Pending => {
                // If a wake landed between poll() entering and
                // returning Pending, the AcqRel store above
                // would have been overwritten by the waker — and
                // the scheduler will have called unpark(gid)
                // before we park. The scheduler's `pre_unpark`
                // side-set covers that race; we additionally
                // skip parking when we observe `woke` set, which
                // saves the suspend/resume hop.
                if waker_arc.woke.load(Ordering::Acquire) {
                    continue;
                }
                sched_global::park(ParkReason::Io, |_parker| {});
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    use std::time::Duration;

    /// Future that returns Ready on the first poll. Sanity test
    /// the driver doesn't park when the future is already ready.
    struct ImmediateReady<T: Clone>(T);
    impl<T: Clone + Unpin> Future for ImmediateReady<T> {
        type Output = T;
        fn poll(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<T> {
            Poll::Ready(self.get_mut().0.clone())
        }
    }

    #[test]
    fn drives_immediately_ready_future_off_goroutine_path() {
        // Run the driver inside a goroutine — that's the only
        // legitimate path. We spawn a goroutine, drive a future,
        // and read the result via a channel.
        let result: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        let result_for_g = Arc::clone(&result);
        let done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let done_for_g = Arc::clone(&done);
        gossamer_runtime::sched_global::spawn(Box::new(move || {
            let v = drive(ImmediateReady(42_i64));
            *result_for_g.lock().unwrap() = Some(v);
            done_for_g.store(true, Ordering::Release);
        }));
        // Spin briefly waiting for the goroutine to finish.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while std::time::Instant::now() < deadline {
            if done.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*result.lock().unwrap(), Some(42));
    }

    /// Future that returns Pending on the first poll, schedules a
    /// wake via a separate thread, returns Ready on the second
    /// poll. Verifies park+wake+repoll happen.
    struct WakesOnce {
        polled: AtomicBool,
    }
    impl Future for WakesOnce {
        type Output = i64;
        fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<i64> {
            if self.polled.swap(true, Ordering::AcqRel) {
                Poll::Ready(7)
            } else {
                let waker = cx.waker().clone();
                std::thread::spawn(move || {
                    std::thread::sleep(Duration::from_millis(30));
                    waker.wake();
                });
                Poll::Pending
            }
        }
    }

    #[test]
    fn drives_pending_future_with_external_wake() {
        let result: Arc<Mutex<Option<i64>>> = Arc::new(Mutex::new(None));
        let result_for_g = Arc::clone(&result);
        let done: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
        let done_for_g = Arc::clone(&done);
        gossamer_runtime::sched_global::spawn(Box::new(move || {
            let v = drive(WakesOnce {
                polled: AtomicBool::new(false),
            });
            *result_for_g.lock().unwrap() = Some(v);
            done_for_g.store(true, Ordering::Release);
        }));
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            if done.load(Ordering::Acquire) {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(*result.lock().unwrap(), Some(7));
    }
}
