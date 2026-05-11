//! Runtime support for `std::thread` — OS thread primitives.
//!
//! Gossamer programs should prefer goroutines (`go fn()`) for most
//! concurrency. OS threads are provided for interop with C libraries,
//! CPU-bound work that needs OS-level parallelism, and rare cases where
//! goroutine scheduling is not sufficient.

#![forbid(unsafe_code)]

use std::thread::JoinHandle;
use std::time::Duration;

use crate::errors::Error;

/// Spawns an OS thread that runs `f` and returns a `JoinHandle`.
pub fn spawn<F, T>(f: F) -> JoinHandle<T>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    std::thread::spawn(f)
}

/// Sleeps the current OS thread for `millis` milliseconds.
pub fn sleep_ms(millis: u64) {
    std::thread::sleep(Duration::from_millis(millis));
}

/// Yields the current OS thread's timeslice to the scheduler.
pub fn yield_now() {
    std::thread::yield_now();
}

/// Returns the number of logical CPUs available.
#[must_use]
pub fn num_cpus() -> usize {
    std::thread::available_parallelism().map_or(1, std::num::NonZero::get)
}

/// Blocks the current thread until the provided `handle` finishes.
/// Returns an error if the joined thread panicked.
pub fn join<T: std::fmt::Debug>(handle: JoinHandle<T>) -> Result<T, Error> {
    handle
        .join()
        .map_err(|_| Error::new("thread::join: thread panicked"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[test]
    fn spawn_and_join() {
        let counter = Arc::new(Mutex::new(0i32));
        let c2 = Arc::clone(&counter);
        let h = spawn(move || {
            *c2.lock().unwrap() += 1;
        });
        join(h).expect("thread should not panic");
        assert_eq!(*counter.lock().unwrap(), 1);
    }

    #[test]
    fn num_cpus_positive() {
        assert!(num_cpus() >= 1);
    }

    #[test]
    fn sleep_ms_short() {
        let start = std::time::Instant::now();
        sleep_ms(5);
        assert!(start.elapsed().as_millis() >= 4);
    }
}
