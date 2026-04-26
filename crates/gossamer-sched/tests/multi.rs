//! Tests for the multi-threaded scheduler and the poller abstraction.

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use gossamer_sched::{Gid, Interest, MockPoller, MultiScheduler, PollSource, Poller, Step};

struct AtomicIncr {
    counter: Arc<AtomicU32>,
    remaining: u32,
}

impl gossamer_sched::Task for AtomicIncr {
    fn step(&mut self) -> Step {
        self.counter.fetch_add(1, Ordering::Relaxed);
        if self.remaining == 0 {
            Step::Done
        } else {
            self.remaining -= 1;
            Step::Yield
        }
    }
}

#[test]
fn multi_scheduler_runs_all_tasks_across_workers() {
    let sched = MultiScheduler::new(4);
    let counter = Arc::new(AtomicU32::new(0));
    for _ in 0..50 {
        let c = Arc::clone(&counter);
        sched.spawn(move || {
            c.fetch_add(1, Ordering::Relaxed);
            Step::Done
        });
    }
    let stats = sched.run();
    assert_eq!(counter.load(Ordering::Relaxed), 50);
    assert_eq!(stats.finished, 50);
}

#[test]
fn multi_scheduler_respects_yield_and_eventually_drains() {
    let sched = MultiScheduler::new(2);
    let counter = Arc::new(AtomicU32::new(0));
    for _ in 0..8 {
        sched.spawn(AtomicIncr {
            counter: Arc::clone(&counter),
            remaining: 3,
        });
    }
    let stats = sched.run();
    // 4 steps per task (remaining=3 yields + final Done) × 8 tasks.
    assert_eq!(stats.finished, 8);
    assert!(stats.steps >= 8 * 4);
    assert_eq!(u64::from(counter.load(Ordering::Relaxed)), stats.steps);
}

#[test]
fn multi_scheduler_with_single_worker_still_completes() {
    let sched = MultiScheduler::new(1);
    let counter = Arc::new(AtomicU32::new(0));
    for _ in 0..10 {
        let c = Arc::clone(&counter);
        sched.spawn(move || {
            c.fetch_add(1, Ordering::Relaxed);
            Step::Done
        });
    }
    let stats = sched.run();
    assert_eq!(counter.load(Ordering::Relaxed), 10);
    assert_eq!(stats.finished, 10);
    assert_eq!(sched.worker_count(), 1);
}

#[test]
fn mock_poller_delivers_registered_readiness() {
    let mut poller = MockPoller::new();
    let source = PollSource(1);
    poller.register(source, Interest::Readable, Gid(42));
    assert!(poller.drain().is_empty());
    poller.fire(source, Interest::Readable);
    let events = poller.drain();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].gid, Gid(42));
    assert_eq!(events[0].source, source);
    assert_eq!(events[0].interest, Interest::Readable);
}

#[test]
fn mock_poller_ignores_deregistered_sources() {
    let mut poller = MockPoller::new();
    let source = PollSource(5);
    poller.register(source, Interest::Writable, Gid(7));
    poller.deregister(source, Interest::Writable);
    poller.fire(source, Interest::Writable);
    assert!(poller.drain().is_empty());
}
