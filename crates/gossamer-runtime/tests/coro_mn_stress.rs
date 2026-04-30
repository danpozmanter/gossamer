//! Stress test for the coroutine-backed M:N scheduler.
//!
//! Spawns thousands of goroutines that each park on a netpoller
//! timer, then unblocks them all. The test fails if the scheduler
//! cannot drain them on a small worker pool — which it could not
//! when goroutines were OS-thread-bound (the previous design).

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use gossamer_runtime::sched_global;

const GOROUTINES: usize = 10_000;

#[test]
fn ten_thousand_goroutines_drain_on_a_small_worker_pool() {
    // Constrain the pool so we know goroutines aren't getting one
    // worker each. The default scheduler size is num_cpus, which on
    // a CI runner with 64 cores would mask the M:N requirement.
    // Setting GOSSAMER_MAX_PROCS=4 caps the pool. The first
    // `scheduler()` call after this set picks it up.
    // SAFETY: tests run serially within a process; no other
    // goroutine has been spawned yet because we haven't called
    // `scheduler()` from any prior point in this test.
    unsafe { std::env::set_var("GOSSAMER_MAX_PROCS", "4") };
    let counter: Arc<AtomicUsize> = Arc::new(AtomicUsize::new(0));
    let start = Instant::now();
    for _ in 0..GOROUTINES {
        let counter = Arc::clone(&counter);
        sched_global::spawn(Box::new(move || {
            sched_global::sleep_until(Instant::now() + Duration::from_millis(50));
            counter.fetch_add(1, Ordering::Relaxed);
        }));
    }
    // Wait for completion. The deadline is loose: 10k goroutines
    // each parked ~50ms with a 4-worker pool should complete in a
    // few hundred milliseconds; we allow up to 30 seconds before
    // declaring the scheduler stuck.
    let deadline = start + Duration::from_secs(30);
    while counter.load(Ordering::Relaxed) < GOROUTINES {
        assert!(
            Instant::now() <= deadline,
            "scheduler drained only {} of {GOROUTINES} goroutines in 30s",
            counter.load(Ordering::Relaxed),
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let elapsed = start.elapsed();
    eprintln!(
        "drained {GOROUTINES} goroutines in {:.3}s",
        elapsed.as_secs_f64()
    );
}
