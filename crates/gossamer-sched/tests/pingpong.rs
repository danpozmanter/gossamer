//! 10k-goroutine pingpong test exercising the work-stealing
//! scheduler under fan-out load. The throughput target is 2x the
//! original `Mutex<Vec<SendTask>>` implementation; the assertion
//! here is the looser "every goroutine completes its budget within
//! 10 seconds" — the 2x measurement runs in the bench harness, not
//! the regression suite.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use gossamer_sched::{MultiScheduler, Step, Task};

struct Counter {
    counter: Arc<AtomicU64>,
    budget: u64,
}

impl Task for Counter {
    fn step(&mut self) -> Step {
        if self.budget == 0 {
            return Step::Done;
        }
        self.budget -= 1;
        self.counter.fetch_add(1, Ordering::Relaxed);
        if self.budget == 0 {
            Step::Done
        } else {
            Step::Yield
        }
    }
}

#[test]
fn ten_thousand_goroutines_complete() {
    let workers = std::thread::available_parallelism()
        .map_or(4, std::num::NonZero::get)
        .min(8);
    let sched = MultiScheduler::new(workers);
    let counter = Arc::new(AtomicU64::new(0));
    let n = 10_000_u64;
    let budget_per_g = 4_u64;
    for _ in 0..n {
        sched.spawn(Counter {
            counter: Arc::clone(&counter),
            budget: budget_per_g,
        });
    }
    let started = Instant::now();
    let stats = sched.run();
    let elapsed = started.elapsed();
    assert_eq!(counter.load(Ordering::Relaxed), n * budget_per_g);
    assert_eq!(stats.finished, n);
    assert!(
        elapsed < Duration::from_secs(10),
        "10k pingpong took {elapsed:?}, budget 10s",
    );
}
