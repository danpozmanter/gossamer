//!: sync, time, and panic integration tests.

use std::sync::Arc;
use std::thread;

use gossamer_std::panic::{catch_unwind, quiet_panics};
use gossamer_std::sync::{
    AtomicBool, AtomicI64, AtomicU64, Barrier, Mutex, Once, RwLock, WaitGroup,
};
use gossamer_std::time::{Duration, Instant, SystemTime, now, sleep};

#[test]
fn mutex_serialises_cross_thread_updates() {
    let counter = Arc::new(Mutex::new(0_i64));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let c = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            for _ in 0..100 {
                c.with(|n| *n += 1);
            }
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    counter.with(|n| assert_eq!(*n, 800));
}

#[test]
fn rwlock_allows_multiple_readers() {
    let data = Arc::new(RwLock::new(vec![1, 2, 3, 4]));
    let handles: Vec<_> = (0..4)
        .map(|_| {
            let d = Arc::clone(&data);
            thread::spawn(move || d.with_read(|v| v.iter().sum::<i32>()))
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 10);
    }
    data.with_write(|v| v.push(5));
    data.with_read(|v| assert_eq!(v.len(), 5));
}

#[test]
fn once_runs_initialiser_exactly_once() {
    let once = Arc::new(Once::new());
    let counter = Arc::new(AtomicI64::new(0));
    let mut handles = Vec::new();
    for _ in 0..10 {
        let o = Arc::clone(&once);
        let c = Arc::clone(&counter);
        handles.push(thread::spawn(move || {
            o.call_once(|| {
                c.fetch_add(1);
            });
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(counter.load(), 1);
}

#[test]
fn atomic_primitives_expose_load_store_and_add() {
    let i = AtomicI64::new(5);
    i.store(10);
    assert_eq!(i.load(), 10);
    assert_eq!(i.fetch_add(3), 10);
    assert_eq!(i.load(), 13);

    let u = AtomicU64::new(0);
    u.fetch_add(4);
    assert_eq!(u.load(), 4);

    let b = AtomicBool::new(false);
    b.store(true);
    assert!(b.load());
    assert!(b.compare_and_swap(true, false));
    assert!(!b.load());
    assert!(!b.compare_and_swap(true, true));
}

#[test]
fn wait_group_counts_in_flight_work() {
    let wg = Arc::new(WaitGroup::new());
    wg.add(3);
    assert_eq!(wg.pending(), 3);
    let mut handles = Vec::new();
    for _ in 0..3 {
        let g = Arc::clone(&wg);
        handles.push(thread::spawn(move || {
            thread::sleep(std::time::Duration::from_millis(5));
            g.done();
        }));
    }
    wg.wait();
    for h in handles {
        h.join().unwrap();
    }
    assert_eq!(wg.pending(), 0);
}

#[test]
fn barrier_synchronises_participants() {
    let barrier = Arc::new(Barrier::new(3));
    let handles: Vec<_> = (0..3)
        .map(|_| {
            let b = Arc::clone(&barrier);
            thread::spawn(move || {
                b.wait();
                42_u32
            })
        })
        .collect();
    for h in handles {
        assert_eq!(h.join().unwrap(), 42);
    }
}

#[test]
fn instant_elapsed_is_non_negative() {
    let start = now();
    sleep(Duration::from_millis(2));
    let elapsed = start.elapsed();
    assert!(elapsed.as_millis() >= 1, "elapsed={elapsed:?}");
}

#[test]
fn duration_constructors_and_accessors_agree() {
    assert_eq!(Duration::from_secs(2).as_millis(), 2_000);
    assert_eq!(Duration::from_millis(5_000).as_secs(), 5);
    assert_eq!(Duration::from_micros(1_000).as_millis(), 1);
    assert_eq!(Duration::ZERO.as_secs(), 0);
}

#[test]
fn instant_ordering_and_duration_since() {
    let t0 = Instant::now();
    sleep(Duration::from_millis(1));
    let t1 = Instant::now();
    assert!(t1 > t0);
    assert!(t1.duration_since(t0) >= Duration::ZERO);
    assert_eq!(t0.duration_since(t1), Duration::ZERO);
}

#[test]
fn system_time_unix_millis_is_positive() {
    let millis = SystemTime::now().unix_millis();
    assert!(millis > 1_600_000_000_000);
}

#[test]
fn catch_unwind_captures_string_panic() {
    let result = quiet_panics(|| catch_unwind(|| std::panic::panic_any("boom".to_string())));
    let err = result.expect_err("panic should propagate");
    assert_eq!(err.message, "boom");
}

#[test]
fn catch_unwind_passes_through_normal_values() {
    let result = catch_unwind(|| 1 + 2);
    assert_eq!(result.unwrap(), 3);
}
