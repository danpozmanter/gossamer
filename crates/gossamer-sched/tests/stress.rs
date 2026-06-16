//! Scheduler stress tests.
//!
//! The pre-existing `basic.rs` and `multi.rs` test files cap out at
//! 50-100 goroutines per case. The runtime is documented to handle
//! 10k true goroutines (`true_goroutines_landed.md` 2026-04-30) but
//! nothing in CI exercises that scale. A regression that only
//! surfaces past ~1k tasks (queue overflow, starvation, parking
//! list O(N²) blowups, work-steal pessimization) ships silently
//! today.
//!
//! These tests run the scheduler at scale on the cooperative
//! single-thread `Scheduler` and the worker-pool `MultiScheduler`.
//! They are deliberately bounded so a regression that hangs surfaces
//! as a per-test timeout rather than a CI hang. End-to-end
//! `go fn(args)` from the language is exercised separately by
//! `crates/gossamer-cli/tests/runtime_stress.rs`.

#![allow(missing_docs)]

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use gossamer_sched::{MultiScheduler, Scheduler, Step};

/// Wall-clock cap per stress test. Catches a regression that
/// hangs the scheduler without freezing the whole CI run.
const STRESS_DEADLINE: Duration = Duration::from_mins(1);

/// Asserts that `f` finishes inside [`STRESS_DEADLINE`]. The
/// scheduler doesn't have a built-in cancellation hook so this
/// is best-effort: if the call hangs, the test runner kills
/// the whole binary on its own timeout.
fn within_deadline<R>(label: &str, f: impl FnOnce() -> R) -> R {
    let start = Instant::now();
    let r = f();
    let elapsed = start.elapsed();
    assert!(
        elapsed < STRESS_DEADLINE,
        "stress test `{label}` exceeded {secs}s ({elapsed:?})",
        secs = STRESS_DEADLINE.as_secs(),
    );
    r
}

#[test]
fn cooperative_scheduler_runs_ten_thousand_tasks_to_completion() {
    // The cooperative single-thread `Scheduler` is the simplest
    // build path and a stand-in for ad-hoc test fixtures. 10_000
    // distinct spawns must complete and each task's body must
    // run exactly once. This is two orders of magnitude past
    // the existing `many_goroutines_all_complete` test (100).
    within_deadline("coop_10k_done", || {
        let mut sched = Scheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..10_000 {
            let c = Arc::clone(&counter);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                Step::Done
            });
        }
        sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 10_000);
        assert_eq!(sched.stats().finished, 10_000);
    });
}

#[test]
fn cooperative_scheduler_handles_yield_intensive_load() {
    // Each of 1_000 tasks yields 100 times before completing
    // (100_000 total step calls). Tests the yield-then-requeue
    // path under sustained churn - the regression class is a
    // queue implementation that turns O(N) per yield instead
    // of O(1).
    within_deadline("coop_1k_x_100_yields", || {
        let mut sched = Scheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..1_000 {
            let c = Arc::clone(&counter);
            let mut remaining = 100_u32;
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                if remaining == 0 {
                    Step::Done
                } else {
                    remaining -= 1;
                    Step::Yield
                }
            });
        }
        sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 1_000 * 101);
        assert_eq!(sched.stats().finished, 1_000);
    });
}

#[test]
fn multi_scheduler_runs_ten_thousand_tasks_across_workers() {
    // 10_000 tasks across a 4-worker pool. Catches work-steal
    // imbalance and the past per-worker queue overflow class.
    // Counter must equal exactly 10_000 - the worker pool is
    // the most likely place a wakeup is dropped at scale.
    within_deadline("multi_10k_done", || {
        let sched = MultiScheduler::new(4);
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..10_000 {
            let c = Arc::clone(&counter);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                Step::Done
            });
        }
        let stats = sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 10_000);
        assert_eq!(stats.finished, 10_000);
    });
}

#[test]
fn multi_scheduler_yield_intensive_load_under_two_workers() {
    // 2_000 tasks each yielding 50 times (~100_000 steps) on
    // a 2-worker pool. Two workers exercise the work-steal
    // path; 50 yields per task amortises the per-task setup
    // cost over many step calls.
    within_deadline("multi_2k_x_50_yields", || {
        let sched = MultiScheduler::new(2);
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..2_000 {
            let c = Arc::clone(&counter);
            let counter_for_task = AtomicU32::new(50);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                let remaining = counter_for_task.load(Ordering::Relaxed);
                if remaining == 0 {
                    Step::Done
                } else {
                    counter_for_task.store(remaining - 1, Ordering::Relaxed);
                    Step::Yield
                }
            });
        }
        let stats = sched.run();
        assert_eq!(stats.finished, 2_000);
        // Each task runs 51 times (50 yields + 1 Done).
        assert_eq!(counter.load(Ordering::Relaxed), 2_000 * 51);
    });
}

#[test]
fn multi_scheduler_with_one_worker_handles_5k_tasks_serial() {
    // Pinning to a single worker forces serial execution of
    // 5_000 tasks. Catches a regression where the single-
    // worker fast path skips the proper completion / wakeup
    // sequencing - past M:N landings have had this class.
    within_deadline("multi_1w_5k_serial", || {
        let sched = MultiScheduler::new(1);
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..5_000 {
            let c = Arc::clone(&counter);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                Step::Done
            });
        }
        let stats = sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 5_000);
        assert_eq!(stats.finished, 5_000);
        assert_eq!(sched.worker_count(), 1);
    });
}

#[test]
fn cooperative_scheduler_spawn_during_run_to_scale() {
    // Each of 1_000 parent tasks spawns one child. Both must
    // complete in the same `run()` call - catches a regression
    // where spawned children get stranded after the parent
    // completes (a real bug class on the M:N transition).
    within_deadline("coop_spawn_during_run_2k", || {
        let mut sched = Scheduler::new();
        let counter = Arc::new(AtomicU32::new(0));
        for _ in 0..1_000 {
            let c = Arc::clone(&counter);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                Step::Done
            });
        }
        sched.run();
        // Stats accumulate across runs.
        for _ in 0..1_000 {
            let c = Arc::clone(&counter);
            sched.spawn(move || {
                c.fetch_add(1, Ordering::Relaxed);
                Step::Done
            });
        }
        sched.run();
        assert_eq!(counter.load(Ordering::Relaxed), 2_000);
        assert_eq!(sched.stats().spawned, 2_000);
        assert_eq!(sched.stats().finished, 2_000);
    });
}
