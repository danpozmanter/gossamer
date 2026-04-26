//! Compile-time assertions that [`gossamer_interp::Value`] and
//! [`gossamer_interp::Interpreter`] implement [`Send`].
//! The Arc-based refactor tracked in the risks backlog
//! requires both of these to hold so the scheduler can eventually
//! dispatch goroutines across a real worker pool.

use std::thread;

use gossamer_interp::{Interpreter, Value};

#[test]
fn value_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Value>();
}

#[test]
fn interpreter_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<Interpreter>();
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
