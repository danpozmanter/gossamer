//! Dedicated thread pool for blocking system calls.
//!
//! File-system reads, writes, opens, and similar syscalls are
//! synchronous in the host kernel: there is no portable
//! non-blocking API for them. To keep these calls from monopolising
//! a scheduler P slot, we route them through a fixed-size pool of
//! OS threads here. The scheduler treats a pool-bound call as
//! "parked"; the calling goroutine resumes when the worker thread
//! delivers the result via a one-shot channel.
//!
//! Sizing follows Go's default `GOMAXPROCS` heuristic: at least
//! `4` threads; up to `2 * num_cpus` for I/O-heavy workloads. Pool
//! size is fixed for the program's lifetime.

#![forbid(unsafe_code)]

use std::sync::OnceLock;
use std::sync::mpsc::{Sender, channel};
use std::thread;

type Job = Box<dyn FnOnce() + Send + 'static>;

struct Pool {
    submit: Sender<Job>,
    size: usize,
}

static POOL: OnceLock<Pool> = OnceLock::new();

fn pool() -> &'static Pool {
    POOL.get_or_init(|| {
        let cpus = std::thread::available_parallelism().map_or(4, std::num::NonZero::get);
        let size = (cpus * 2).max(4);
        let (tx, rx) = channel::<Job>();
        let rx = std::sync::Arc::new(parking_lot::Mutex::new(rx));
        for index in 0..size {
            let rx = std::sync::Arc::clone(&rx);
            thread::Builder::new()
                .name(format!("gos-blocking-{index}"))
                .spawn(move || worker_loop(rx))
                .expect("spawn blocking pool worker");
        }
        Pool { submit: tx, size }
    })
}

fn worker_loop(rx: std::sync::Arc<parking_lot::Mutex<std::sync::mpsc::Receiver<Job>>>) {
    loop {
        let job = {
            let guard = rx.lock();
            guard.recv()
        };
        let Ok(job) = job else { return };
        // Run the job; panics inside the job propagate inside this
        // worker thread but do not poison the pool — we simply
        // recover and accept the next job.
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
    }
}

/// Runs `f` on the blocking thread pool and waits for the result.
/// The calling thread blocks on a one-shot channel; in the
/// goroutine model this is equivalent to a park.
pub fn run<R: Send + 'static>(f: impl FnOnce() -> R + Send + 'static) -> R {
    let (tx, rx) = std::sync::mpsc::channel::<R>();
    let job: Job = Box::new(move || {
        let result = f();
        // Send may fail if the caller dropped its receiver; either
        // way the result is no longer needed.
        let _ = tx.send(result);
    });
    pool()
        .submit
        .send(job)
        .expect("blocking pool sender disconnected");
    rx.recv().expect("blocking pool result channel closed")
}

/// Number of worker threads in the pool. Mostly for diagnostics.
#[must_use]
pub fn pool_size() -> usize {
    pool().size
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_returns_the_jobs_value() {
        let n = run(|| 1 + 2);
        assert_eq!(n, 3);
    }

    #[test]
    fn pool_size_is_at_least_four() {
        assert!(pool_size() >= 4);
    }

    #[test]
    fn pool_survives_a_panicking_job() {
        // First job panics; second one still completes.
        let _ = std::panic::catch_unwind(|| {
            run(|| panic!("job panic"));
        });
        let r = run(|| 7);
        assert_eq!(r, 7);
    }
}
