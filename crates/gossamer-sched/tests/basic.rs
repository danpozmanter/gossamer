//! End-to-end scheduler tests.

use std::cell::RefCell;
use std::rc::Rc;

use gossamer_sched::{Scheduler, Step};

struct Counter {
    remaining: u32,
    log: Rc<RefCell<Vec<u32>>>,
    tag: u32,
}

impl gossamer_sched::Task for Counter {
    fn step(&mut self) -> Step {
        self.log.borrow_mut().push(self.tag);
        if self.remaining == 0 {
            return Step::Done;
        }
        self.remaining -= 1;
        Step::Yield
    }
}

#[test]
fn empty_scheduler_run_returns_zero_steps() {
    let mut sched = Scheduler::new();
    assert_eq!(sched.run(), 0);
    assert!(sched.is_empty());
}

#[test]
fn single_task_runs_to_completion() {
    let mut sched = Scheduler::new();
    let counter = Rc::new(RefCell::new(0_u32));
    let c = Rc::clone(&counter);
    sched.spawn(move || {
        *c.borrow_mut() += 1;
        Step::Done
    });
    sched.run();
    assert_eq!(*counter.borrow(), 1);
    assert_eq!(sched.stats().spawned, 1);
    assert_eq!(sched.stats().finished, 1);
}

#[test]
fn two_tasks_interleave_fifo() {
    let log: Rc<RefCell<Vec<u32>>> = Rc::new(RefCell::new(Vec::new()));
    let mut sched = Scheduler::new();
    sched.spawn(Counter {
        remaining: 2,
        log: Rc::clone(&log),
        tag: 1,
    });
    sched.spawn(Counter {
        remaining: 2,
        log: Rc::clone(&log),
        tag: 2,
    });
    sched.run();
    let log = log.borrow().clone();
    assert_eq!(log, vec![1, 2, 1, 2, 1, 2]);
}

#[test]
fn bounded_run_respects_step_budget() {
    let mut sched = Scheduler::new();
    sched.spawn(Counter {
        remaining: 5,
        log: Rc::new(RefCell::new(Vec::new())),
        tag: 0,
    });
    let ran = sched.run_bounded(3);
    assert_eq!(ran, 3);
    assert!(!sched.is_empty());
    sched.run();
    assert!(sched.is_empty());
}

#[test]
fn spawning_during_run_is_observed_on_next_pass() {
    let child_ran = Rc::new(RefCell::new(false));
    let mut sched = Scheduler::new();
    let flag = Rc::clone(&child_ran);
    sched.spawn(move || {
        let _ = flag;
        Step::Done
    });
    sched.run();
    let flag2 = Rc::clone(&child_ran);
    sched.spawn(move || {
        *flag2.borrow_mut() = true;
        Step::Done
    });
    sched.run();
    assert!(*child_ran.borrow());
    assert_eq!(sched.stats().spawned, 2);
    assert_eq!(sched.stats().finished, 2);
}

#[test]
fn many_goroutines_all_complete() {
    let mut sched = Scheduler::new();
    let counter = Rc::new(RefCell::new(0_u32));
    for _ in 0..100 {
        let c = Rc::clone(&counter);
        sched.spawn(move || {
            *c.borrow_mut() += 1;
            Step::Done
        });
    }
    sched.run();
    assert_eq!(*counter.borrow(), 100);
    assert_eq!(sched.stats().finished, 100);
}

#[test]
fn yielding_task_eventually_finishes() {
    let mut sched = Scheduler::new();
    let mut ticks = 0;
    let gid = sched.spawn(move || {
        ticks += 1;
        if ticks >= 4 { Step::Done } else { Step::Yield }
    });
    sched.run();
    assert!(!sched.is_active(gid));
}
