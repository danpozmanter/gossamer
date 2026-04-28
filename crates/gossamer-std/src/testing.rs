//! Runtime support for `std::testing` — assertions and sub-test
//! harness helpers exposed alongside the `gos test` runner.
//! Prefer writing assertions in the direct form:
//! ```gos
//! testing::check_eq(&got, &want, "message describing what is being checked")
//! ```
//! The `gos test` runner inspects the assertion tally at the end of
//! each `#[test]` function, so a failed `check*` call causes the
//! test to fail even when its `Result<(), Error>` is not propagated
//! via `?` or `.expect()`. Reserve `?` / `.expect()` for the case
//! where a later assertion depends on the earlier one succeeding.

#![forbid(unsafe_code)]

use crate::errors::Error;

/// Asserts `cond`, returning an `Err` on failure with the supplied
/// message.
pub fn check(cond: bool, message: &str) -> Result<(), Error> {
    if cond {
        Ok(())
    } else {
        Err(Error::new(format!("assertion failed: {message}")))
    }
}

/// Asserts equality of `left` and `right`, producing a diff-style
/// failure message when they differ.
pub fn check_eq<T: std::fmt::Debug + PartialEq>(
    left: &T,
    right: &T,
    message: &str,
) -> Result<(), Error> {
    if left == right {
        Ok(())
    } else {
        Err(Error::new(format!(
            "{message}: left={left:?}, right={right:?}"
        )))
    }
}

/// Asserts `result` is `Ok`, returning the wrapped value.
pub fn check_ok<T, E: std::fmt::Debug>(result: Result<T, E>, message: &str) -> Result<T, Error> {
    result.map_err(|err| Error::new(format!("{message}: {err:?}")))
}

/// Boxed test body: a `FnOnce` that runs the test and returns its
/// outcome. `Send + 'static` so the parallel runner can move cases
/// onto worker threads.
pub type TestBody = Box<dyn FnOnce() -> Result<(), Error> + Send + 'static>;

/// One named test case as supplied to [`Runner::run_parallel`].
pub type TestCase = (String, TestBody);

/// One sub-test result.
#[derive(Debug, Clone)]
pub struct TestResult {
    /// Short human name.
    pub name: String,
    /// `true` when the body returned `Ok`.
    pub ok: bool,
    /// Captured error message when `ok == false`.
    pub error: Option<String>,
}

/// Minimal test-harness runner. Collects per-subtest results and
/// renders a summary.
pub struct Runner {
    results: Vec<TestResult>,
}

impl Runner {
    /// Empty runner.
    #[must_use]
    pub fn new() -> Self {
        Self {
            results: Vec::new(),
        }
    }

    /// Runs `body` as a sub-test tagged `name`.
    pub fn run<F>(&mut self, name: impl Into<String>, body: F)
    where
        F: FnOnce() -> Result<(), Error>,
    {
        let name = name.into();
        match body() {
            Ok(()) => self.results.push(TestResult {
                name,
                ok: true,
                error: None,
            }),
            Err(err) => self.results.push(TestResult {
                name,
                ok: false,
                error: Some(err.message().to_string()),
            }),
        }
    }

    /// Count of passes.
    #[must_use]
    pub fn passes(&self) -> usize {
        self.results.iter().filter(|r| r.ok).count()
    }

    /// Count of failures.
    #[must_use]
    pub fn failures(&self) -> usize {
        self.results.iter().filter(|r| !r.ok).count()
    }

    /// Borrowed view of every recorded result.
    #[must_use]
    pub fn results(&self) -> &[TestResult] {
        &self.results
    }

    /// Returns a plain-text summary. `"PASS: N  FAIL: M"`, followed by
    /// one line per failing test.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = format!("PASS: {}  FAIL: {}", self.passes(), self.failures());
        for result in &self.results {
            if !result.ok {
                out.push_str("\n  - ");
                out.push_str(&result.name);
                if let Some(err) = &result.error {
                    out.push_str(": ");
                    out.push_str(err);
                }
            }
        }
        out
    }

    /// Runs every subtest in `cases` across `worker_count` OS threads
    /// in parallel, mirroring Go's `t.Run(name, ...) + t.Parallel()`
    /// idiom. Each subtest body runs to completion on its assigned
    /// worker; results are aggregated in subtest-name order so the
    /// final summary is deterministic.
    pub fn run_parallel<F>(&mut self, worker_count: usize, cases: Vec<(String, F)>)
    where
        F: FnOnce() -> Result<(), Error> + Send + 'static,
    {
        use std::sync::Arc;
        use std::sync::Mutex as StdMutex;
        if worker_count <= 1 || cases.len() <= 1 {
            for (name, body) in cases {
                self.run(name, body);
            }
            return;
        }
        let queue = Arc::new(StdMutex::new(
            cases
                .into_iter()
                .enumerate()
                .map(|(idx, (name, body))| (idx, name, body))
                .collect::<Vec<_>>(),
        ));
        let results: Arc<StdMutex<Vec<(usize, TestResult)>>> = Arc::new(StdMutex::new(Vec::new()));
        let mut handles = Vec::with_capacity(worker_count.min(queue.lock().expect("lock").len()));
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            handles.push(std::thread::spawn(move || {
                loop {
                    let next = {
                        let mut q = queue.lock().expect("queue lock");
                        q.pop()
                    };
                    let Some((idx, name, body)) = next else {
                        return;
                    };
                    let outcome = body();
                    let result = match outcome {
                        Ok(()) => TestResult {
                            name,
                            ok: true,
                            error: None,
                        },
                        Err(err) => TestResult {
                            name,
                            ok: false,
                            error: Some(err.message().to_string()),
                        },
                    };
                    results.lock().expect("results lock").push((idx, result));
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        let mut collected = Arc::try_unwrap(results)
            .expect("arc unwrap")
            .into_inner()
            .expect("lock");
        collected.sort_by_key(|(idx, _)| *idx);
        for (_, r) in collected {
            self.results.push(r);
        }
    }
}

impl Default for Runner {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn check_passes_on_true_condition() {
        assert!(check(true, "x").is_ok());
        let err = check(false, "x").unwrap_err();
        assert!(err.message().contains("assertion failed: x"));
    }

    #[test]
    fn check_eq_renders_diff_on_mismatch() {
        let err = check_eq(&1, &2, "ints").unwrap_err();
        assert!(err.message().contains("ints: left=1, right=2"));
    }

    #[test]
    fn runner_counts_pass_and_fail() {
        let mut runner = Runner::new();
        runner.run("ok", || Ok(()));
        runner.run("fail", || Err(Error::new("nope")));
        runner.run("another-ok", || Ok(()));
        assert_eq!(runner.passes(), 2);
        assert_eq!(runner.failures(), 1);
        let summary = runner.summary();
        assert!(summary.contains("PASS: 2  FAIL: 1"));
        assert!(summary.contains("- fail: nope"));
    }

    #[test]
    fn run_parallel_preserves_input_order() {
        let mut runner = Runner::new();
        let cases: Vec<TestCase> = vec![
            ("a".to_string(), Box::new(|| Ok(()))),
            ("b".to_string(), Box::new(|| Err(Error::new("boom")))),
            ("c".to_string(), Box::new(|| Ok(()))),
        ];
        runner.run_parallel(4, cases);
        assert_eq!(runner.results().len(), 3);
        assert_eq!(runner.results()[0].name, "a");
        assert_eq!(runner.results()[1].name, "b");
        assert_eq!(runner.results()[2].name, "c");
        assert!(!runner.results()[1].ok);
    }
}
