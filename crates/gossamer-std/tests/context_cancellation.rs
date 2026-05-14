//! 0.5.0 CONTEXT invariants.
//!
//! Pins three behaviours the pre-0.5.0 design did not provide:
//!
//! 1. `Cancel::cancel_with` unparks goroutines that registered
//!    themselves on the context's wait list. Concretely:
//!    `time::sleep_ctx` parked on a long sleep returns Err
//!    within milliseconds of an external `cancel()`.
//! 2. `with_deadline` schedules a real timer; a goroutine parked
//!    on a `with_timeout` context wakes up at the deadline even
//!    when nothing else observes the context.
//! 3. Cancellation propagates eagerly through the descendant
//!    graph: cancelling a parent unparks goroutines waiting on
//!    grand-children.
//!
//! Tests run goroutines via `gossamer_runtime::sched_global::spawn`,
//! the same surface user code reaches.

#![allow(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration as StdDuration, Instant};

use gossamer_std::context::{self, Context};
use gossamer_std::time::{self, Duration};

fn spawn_blocking<F>(f: F) -> std::sync::mpsc::Receiver<()>
where
    F: FnOnce() + Send + 'static,
{
    // Use the runtime's scheduler so the spawned closure runs as
    // a real goroutine — `current_gid()` inside the closure
    // returns a valid id, which `sleep_ctx` requires.
    let (tx, rx) = std::sync::mpsc::channel();
    gossamer_runtime::sched_global::spawn(Box::new(move || {
        f();
        let _ = tx.send(());
    }));
    rx
}

fn wait_with_timeout(rx: std::sync::mpsc::Receiver<()>, deadline: StdDuration) -> bool {
    match rx.recv_timeout(deadline) {
        Ok(()) => true,
        Err(_) => false,
    }
}

#[test]
fn cancel_wakes_sleep_ctx_within_bound() {
    let (ctx, cancel) = context::with_cancel(&Context::background());
    let cancelled_observed = Arc::new(AtomicBool::new(false));
    let elapsed_micros = Arc::new(AtomicU64::new(0));
    let observed = Arc::clone(&cancelled_observed);
    let elapsed_slot = Arc::clone(&elapsed_micros);
    let ctx_for_goroutine = ctx.clone();
    let start = Instant::now();
    let done = spawn_blocking(move || {
        // Park for 60 seconds; the test should never let this
        // complete naturally. Cancel must wake us up much sooner.
        let result = time::sleep_ctx(&ctx_for_goroutine, Duration::from_secs(60));
        elapsed_slot.store(start.elapsed().as_micros() as u64, Ordering::Release);
        if result.is_err() {
            observed.store(true, Ordering::Release);
        }
    });

    // Let the goroutine reach the park.
    std::thread::sleep(StdDuration::from_millis(50));
    cancel.cancel();

    // The goroutine should observe the cancel within a small
    // window. Bound generously to avoid flakiness on slow runners.
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "goroutine did not return within 3s of cancel — wait-list unpark broken",
    );
    assert!(
        cancelled_observed.load(Ordering::Acquire),
        "sleep_ctx returned Ok despite cancel — cancel propagation broken",
    );
    let elapsed = elapsed_micros.load(Ordering::Acquire);
    assert!(
        elapsed < 3_000_000,
        "sleep_ctx took {elapsed}us — expected wake-up within 3s of cancel",
    );
}

#[test]
fn with_timeout_fires_active_deadline() {
    let ctx = context::with_timeout(&Context::background(), StdDuration::from_millis(80));
    let cancelled_observed = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&cancelled_observed);
    let ctx_for_goroutine = ctx.clone();
    let start = Instant::now();
    let done = spawn_blocking(move || {
        let result = time::sleep_ctx(&ctx_for_goroutine, Duration::from_secs(60));
        if result.is_err() {
            observed.store(true, Ordering::Release);
        }
    });
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "deadline did not fire within 3s — active timer broken",
    );
    assert!(
        cancelled_observed.load(Ordering::Acquire),
        "sleep_ctx did not surface the deadline as Err",
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < StdDuration::from_secs(3),
        "deadline took {elapsed:?} — expected sub-3s",
    );
}

#[test]
fn ancestor_cancel_propagates_to_descendant_waiters() {
    let (parent, parent_cancel) = context::with_cancel(&Context::background());
    let (child, _child_cancel) = context::with_cancel(&parent);
    let (grand, _grand_cancel) = context::with_cancel(&child);
    let cancelled_observed = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&cancelled_observed);
    let grand_for_goroutine = grand.clone();
    let done = spawn_blocking(move || {
        let result = time::sleep_ctx(&grand_for_goroutine, Duration::from_secs(60));
        if result.is_err() {
            observed.store(true, Ordering::Release);
        }
    });
    std::thread::sleep(StdDuration::from_millis(50));
    // Cancel the grandparent. The grandchild's waiter should be
    // unparked because cancel walks descendants.
    parent_cancel.cancel();
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "descendant goroutine not unparked by ancestor cancel",
    );
    assert!(
        cancelled_observed.load(Ordering::Acquire),
        "sleep_ctx in grandchild did not surface ancestor cancel",
    );
    assert!(grand.is_cancelled());
    assert!(child.is_cancelled());
}

#[test]
fn already_cancelled_context_returns_immediately() {
    let (ctx, cancel) = context::with_cancel(&Context::background());
    cancel.cancel();
    let ctx_for_goroutine = ctx.clone();
    let start = Instant::now();
    let done = spawn_blocking(move || {
        let _ = time::sleep_ctx(&ctx_for_goroutine, Duration::from_secs(60));
    });
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(1)),
        "sleep_ctx on already-cancelled context did not return immediately",
    );
    let elapsed = start.elapsed();
    assert!(
        elapsed < StdDuration::from_secs(1),
        "elapsed = {elapsed:?}; pre-cancelled context should short-circuit",
    );
}

#[test]
fn cancel_records_reason() {
    let (ctx, cancel) = context::with_cancel(&Context::background());
    cancel.cancel_with("request superseded");
    let err = ctx
        .err()
        .expect("context cancelled but err() returned None");
    assert!(
        err.message().contains("request superseded"),
        "expected reason in error message, got {:?}",
        err.message(),
    );
}

#[test]
fn done_try_recv_reflects_cancellation_state() {
    let (ctx, cancel) = context::with_cancel(&Context::background());
    let done = ctx.done();
    assert!(!done.try_recv(), "done.try_recv() must be false pre-cancel");
    cancel.cancel_with("explicit done test");
    assert!(done.try_recv(), "done.try_recv() must be true after cancel");
}

#[test]
fn done_recv_blocks_until_cancel() {
    let (ctx, cancel) = context::with_cancel(&Context::background());
    let ctx_for_goroutine = ctx.clone();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed);
    let done_rx = spawn_blocking(move || {
        let _err = ctx_for_goroutine.done().recv();
        observed_clone.store(true, Ordering::Release);
    });
    std::thread::sleep(StdDuration::from_millis(50));
    assert!(
        !observed.load(Ordering::Acquire),
        "Done::recv returned before cancel"
    );
    cancel.cancel_with("done.recv test");
    assert!(
        wait_with_timeout(done_rx, StdDuration::from_secs(3)),
        "Done::recv did not return within 3s of cancel",
    );
    assert!(observed.load(Ordering::Acquire));
}

#[test]
fn deadline_records_reason() {
    let ctx = context::with_timeout(&Context::background(), StdDuration::from_millis(20));
    std::thread::sleep(StdDuration::from_millis(80));
    let err = ctx.err().expect("deadline elapsed but err() returned None");
    assert!(
        err.message().contains("deadline"),
        "expected 'deadline' in reason, got {:?}",
        err.message(),
    );
}

#[test]
fn waitgroup_wait_ctx_returns_err_on_cancel() {
    use gossamer_std::sync::WaitGroup;
    let wg = Arc::new(WaitGroup::new());
    wg.add(1);
    let (ctx, cancel) = context::with_cancel(&Context::background());
    let wg_for_goroutine = Arc::clone(&wg);
    let ctx_for_goroutine = ctx.clone();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed);
    let done = spawn_blocking(move || {
        let result = wg_for_goroutine.wait_ctx(&ctx_for_goroutine);
        if result.is_err() {
            observed_clone.store(true, Ordering::Release);
        }
    });
    std::thread::sleep(StdDuration::from_millis(50));
    cancel.cancel_with("wg_wait_ctx test");
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "WaitGroup::wait_ctx did not return within 3s of cancel",
    );
    assert!(
        observed.load(Ordering::Acquire),
        "wait_ctx returned Ok despite cancel",
    );
    wg.done();
}

#[test]
fn waitgroup_wait_ctx_returns_ok_on_drain() {
    use gossamer_std::sync::WaitGroup;
    let wg = Arc::new(WaitGroup::new());
    wg.add(1);
    let ctx = Context::background();
    let wg_for_goroutine = Arc::clone(&wg);
    let ctx_for_goroutine = ctx.clone();
    let observed_ok = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed_ok);
    let done = spawn_blocking(move || {
        let result = wg_for_goroutine.wait_ctx(&ctx_for_goroutine);
        if result.is_ok() {
            observed_clone.store(true, Ordering::Release);
        }
    });
    std::thread::sleep(StdDuration::from_millis(50));
    wg.done();
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "WaitGroup::wait_ctx did not return Ok after drain",
    );
    assert!(observed_ok.load(Ordering::Acquire));
}

#[test]
fn mutex_with_ctx_returns_err_on_cancel_when_held() {
    use gossamer_std::sync::Mutex;
    let m: Arc<Mutex<i64>> = Arc::new(Mutex::new(0));
    let m_holder = Arc::clone(&m);
    let (hold_started_tx, hold_started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let _holder = std::thread::spawn(move || {
        m_holder.with(|_v| {
            let _ = hold_started_tx.send(());
            let _ = release_rx.recv();
        });
    });
    let _ = hold_started_rx.recv_timeout(StdDuration::from_secs(2));

    let (ctx, cancel) = context::with_cancel(&Context::background());
    let m_for_goroutine = Arc::clone(&m);
    let ctx_for_goroutine = ctx.clone();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed);
    let done = spawn_blocking(move || {
        let result = m_for_goroutine.with_ctx(&ctx_for_goroutine, |v| *v + 1);
        if result.is_err() {
            observed_clone.store(true, Ordering::Release);
        }
    });
    std::thread::sleep(StdDuration::from_millis(50));
    cancel.cancel_with("mutex_with_ctx test");
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "Mutex::with_ctx did not return within 3s of cancel",
    );
    assert!(
        observed.load(Ordering::Acquire),
        "with_ctx returned Ok despite cancel",
    );
    let _ = release_tx.send(());
}

#[test]
fn mutex_with_ctx_returns_ok_when_uncontended() {
    use gossamer_std::sync::Mutex;
    let m: Arc<Mutex<i64>> = Arc::new(Mutex::new(7));
    let ctx = Context::background();
    let m_for_goroutine = Arc::clone(&m);
    let ctx_for_goroutine = ctx.clone();
    let observed_value = Arc::new(AtomicU64::new(0));
    let observed_clone = Arc::clone(&observed_value);
    let done = spawn_blocking(move || {
        if let Ok(v) = m_for_goroutine.with_ctx(&ctx_for_goroutine, |inner| *inner) {
            observed_clone.store(v as u64, Ordering::Release);
        }
    });
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "Mutex::with_ctx (uncontended) hung",
    );
    assert_eq!(observed_value.load(Ordering::Acquire), 7);
}

#[test]
fn blocking_pool_run_ctx_returns_err_on_cancel() {
    use gossamer_std::blocking_pool;
    let (ctx, cancel) = context::with_cancel(&Context::background());
    let ctx_for_goroutine = ctx.clone();
    let observed = Arc::new(AtomicBool::new(false));
    let observed_clone = Arc::clone(&observed);
    let done = spawn_blocking(move || {
        let result: Result<i64, _> = blocking_pool::run_ctx(&ctx_for_goroutine, || {
            std::thread::sleep(StdDuration::from_secs(60));
            42
        });
        if result.is_err() {
            observed_clone.store(true, Ordering::Release);
        }
    });
    std::thread::sleep(StdDuration::from_millis(50));
    cancel.cancel_with("blocking_pool_ctx test");
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "blocking_pool::run_ctx did not return within 3s of cancel",
    );
    assert!(
        observed.load(Ordering::Acquire),
        "run_ctx returned Ok despite cancel",
    );
}

#[test]
fn blocking_pool_run_ctx_returns_ok_when_job_finishes_first() {
    use gossamer_std::blocking_pool;
    let ctx = Context::background();
    let ctx_for_goroutine = ctx.clone();
    let observed = Arc::new(AtomicU64::new(0));
    let observed_clone = Arc::clone(&observed);
    let done = spawn_blocking(move || {
        let result: Result<i64, _> = blocking_pool::run_ctx(&ctx_for_goroutine, || 17);
        if let Ok(v) = result {
            observed_clone.store(v as u64, Ordering::Release);
        }
    });
    assert!(
        wait_with_timeout(done, StdDuration::from_secs(3)),
        "blocking_pool::run_ctx hung on fast job",
    );
    assert_eq!(observed.load(Ordering::Acquire), 17);
}
