//! Audit M9 (0.6.0): per-goroutine panic isolation.
//!
//! A panic inside a spawned goroutine must NOT abort the process.
//! The coroutine wrapper catches the unwind, eprintln's the
//! message, sets the panicked flag, and lets resume return
//! normally so the scheduler keeps running.

use gossamer_coro::{Goroutine, any_goroutine_panicked};

#[test]
fn goroutine_panic_does_not_kill_the_process() {
    // If isolation is broken, this test aborts the whole test
    // binary - which surfaces as a `cargo test` failure of every
    // test in the same process, not just this one. The fact that
    // we reach the assertion at all is the primary signal.
    let mut g = Goroutine::new(Box::new(|| panic!("intentional test panic")));
    let _done = g.resume();
    // The resume call returned cleanly - no panic propagated.
    assert!(
        any_goroutine_panicked(),
        "panicked-flag must be sticky after a panic in a goroutine"
    );
}

#[test]
fn sibling_goroutines_continue_running_after_a_panic() {
    let mut bad = Goroutine::new(Box::new(|| panic!("intentional sibling-panic test")));
    let _ = bad.resume();
    // After the bad goroutine has panicked, a fresh goroutine
    // still works end-to-end.
    let result_slot: std::sync::Arc<std::sync::atomic::AtomicBool> =
        std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let slot_clone = std::sync::Arc::clone(&result_slot);
    let mut good = Goroutine::new(Box::new(move || {
        slot_clone.store(true, std::sync::atomic::Ordering::Release);
    }));
    let done = good.resume();
    assert!(done, "well-behaved goroutine must resume cleanly");
    assert!(
        result_slot.load(std::sync::atomic::Ordering::Acquire),
        "sibling goroutine must have run after a peer panicked"
    );
}
