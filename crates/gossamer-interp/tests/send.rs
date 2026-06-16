//! Compile-time assertion that [`gossamer_interp::Value`] implements
//! [`Send`], so the scheduler can dispatch goroutines that carry
//! values across a real worker pool.

use std::thread;

use gossamer_interp::Value;

#[test]
fn value_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Value>();
}

#[test]
fn values_can_travel_across_threads() {
    let value = Value::Int(42);
    let handle = thread::spawn(move || match value {
        Value::Int(n) => n,
        _ => 0,
    });
    assert_eq!(handle.join().unwrap(), 42);
}
