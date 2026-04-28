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
//!
//! Defense #3: the submit side is a `crossbeam_channel::bounded`
//! channel sized at `4 * pool_size`. If the queue saturates,
//! `run` blocks on `submit` instead of growing an unbounded backlog
//! that would silently turn into RAM. This puts a hard cap on the
//! amount of in-flight blocking work — runaway producers see
//! backpressure.

#![forbid(unsafe_code)]

use std::sync::OnceLock;
use std::thread;

use crossbeam_channel::{Receiver, Sender, bounded};

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
        // Backpressure capacity: enough headroom that a brief burst
        // does not stall callers, but small enough that a runaway
        // producer cannot accumulate megabytes of pending closures.
        let (tx, rx) = bounded::<Job>(size * 4);
        for index in 0..size {
            let rx: Receiver<Job> = rx.clone();
            thread::Builder::new()
                .name(format!("gos-blocking-{index}"))
                .spawn(move || worker_loop(rx))
                .expect("spawn blocking pool worker");
        }
        Pool { submit: tx, size }
    })
}

fn worker_loop(rx: Receiver<Job>) {
    while let Ok(job) = rx.recv() {
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

    #[test]
    fn many_jobs_complete_under_backpressure() {
        // Submit more jobs than the channel capacity to exercise
        // the bounded-channel backpressure path.
        let n = pool_size() * 16;
        let mut handles = Vec::with_capacity(n);
        for i in 0..n {
            handles.push(std::thread::spawn(move || run(move || i * 2)));
        }
        let mut total = 0i64;
        for h in handles {
            total += h.join().unwrap() as i64;
        }
        let expected: i64 = (0..n as i64).map(|i| i * 2).sum();
        assert_eq!(total, expected);
    }
}
