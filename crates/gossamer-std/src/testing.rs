//! Runtime support for `std::testing` - assertions and sub-test
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

static ENV_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
static CWD_STATE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Restores a process environment variable when dropped.
///
/// The guard serializes environment and working-directory mutations made
/// through the testing helpers so parallel tests cannot observe partial state.
pub struct ScopedEnv {
    name: String,
    previous: Option<String>,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedEnv {
    /// Sets `name` for the lifetime of the returned guard.
    pub fn set(name: &str, value: &str) -> Result<Self, crate::io::IoError> {
        let guard = ENV_STATE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = crate::env::var(name);
        crate::env::set_var(name, value)?;
        Ok(Self {
            name: name.to_string(),
            previous,
            _guard: guard,
        })
    }
}

impl Drop for ScopedEnv {
    fn drop(&mut self) {
        if let Some(value) = &self.previous {
            let _ = crate::env::set_var(&self.name, value);
        } else {
            crate::env::unset_var(&self.name);
        }
    }
}

/// Restores the process working directory when dropped.
pub struct ScopedCwd {
    previous: std::path::PathBuf,
    _guard: std::sync::MutexGuard<'static, ()>,
}

impl ScopedCwd {
    /// Changes the working directory for the lifetime of the returned guard.
    pub fn set(path: impl AsRef<std::path::Path>) -> std::io::Result<Self> {
        let guard = CWD_STATE_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let previous = std::env::current_dir()?;
        std::env::set_current_dir(path)?;
        Ok(Self {
            previous,
            _guard: guard,
        })
    }
}

impl Drop for ScopedCwd {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

/// Polls `condition` until it succeeds or the deadline expires.
#[must_use]
pub fn poll_until(
    timeout: std::time::Duration,
    interval: std::time::Duration,
    mut condition: impl FnMut() -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    loop {
        if condition() {
            return true;
        }
        let now = std::time::Instant::now();
        if now >= deadline {
            return false;
        }
        std::thread::sleep(interval.min(deadline.saturating_duration_since(now)));
    }
}

/// Loopback HTTP server for integration tests.
///
/// `TestServer` binds `127.0.0.1:0` before its worker starts, so every
/// instance receives an isolated OS-assigned port without a bind-twice race.
/// The worker is stopped and joined by [`TestServer::shutdown`] or [`Drop`].
/// It is a Rust-hosted test helper while the public Gossamer `httptest`
/// surface is being wired through the frontend and native tiers.
#[cfg(not(target_arch = "wasm32"))]
pub struct TestServer {
    address: std::net::SocketAddr,
    shutdown: std::sync::Arc<std::sync::atomic::AtomicBool>,
    worker: Option<std::thread::JoinHandle<std::io::Result<()>>>,
}

#[cfg(not(target_arch = "wasm32"))]
impl TestServer {
    /// Starts a loopback server that dispatches every request to `handler`.
    ///
    /// The server is ready to accept connections when this returns. Call
    /// [`Self::url`] or [`Self::url_for`] when constructing client requests.
    pub fn start<H>(handler: H) -> std::io::Result<Self>
    where
        H: FnMut(crate::http::Request) -> crate::http::Response + Send + 'static,
    {
        Self::start_with_config(crate::http::server::Config::default(), handler)
    }

    /// Starts a loopback server with an explicit HTTP-server configuration.
    ///
    /// The supplied configuration's shutdown flag is retained by this helper,
    /// so callers should not share it with another server.
    pub fn start_with_config<H>(
        config: crate::http::server::Config,
        handler: H,
    ) -> std::io::Result<Self>
    where
        H: FnMut(crate::http::Request) -> crate::http::Response + Send + 'static,
    {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let shutdown = std::sync::Arc::clone(&config.shutdown);
        let worker = std::thread::Builder::new()
            .name("gossamer-test-http-server".to_string())
            .spawn(move || crate::http::server::run(listener, &config, handler))?;
        Ok(Self {
            address,
            shutdown,
            worker: Some(worker),
        })
    }

    /// Socket address assigned to this server.
    #[must_use]
    pub const fn addr(&self) -> std::net::SocketAddr {
        self.address
    }

    /// Base HTTP URL for this server, without a trailing slash.
    #[must_use]
    pub fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    /// Resolves `path` against [`Self::url`].
    ///
    /// A leading slash is optional. An empty path resolves to `/`.
    #[must_use]
    pub fn url_for(&self, path: &str) -> String {
        format!("{}/{}", self.url(), path.trim_start_matches('/'))
    }

    /// Stops the server and joins its worker thread.
    ///
    /// This is idempotent. A panic in the server worker is reported as an I/O
    /// error instead of being silently discarded by a test.
    pub fn shutdown(&mut self) -> std::io::Result<()> {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);
        let Some(worker) = self.worker.take() else {
            return Ok(());
        };
        match worker.join() {
            Ok(result) => result,
            Err(_) => Err(std::io::Error::other("HTTP test-server worker panicked")),
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
impl Drop for TestServer {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

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

/// Waits until the global goroutine scheduler is idle, or until
/// `timeout` elapses. This is intended for concurrency tests that need a
/// bounded quiescence point without hard-coded sleeps.
#[must_use]
pub fn wait_for_scheduler_idle(timeout: std::time::Duration) -> bool {
    let deadline = std::time::Instant::now() + timeout;
    let scheduler = gossamer_runtime::sched_global::scheduler();
    loop {
        let stats = scheduler.stats();
        if scheduler.live_goroutines() == 0 && stats.spawned == stats.finished {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Marker handed to `#[bench]` functions by the `gos bench` harness.
///
/// The bench fn signature is `fn name(b: &mut Bencher)`; the harness
/// calibrates iteration counts itself and forwards `iter_count` so a
/// body that wraps its work in `b.iter(|| ...)` runs the inner
/// closure exactly that many times. `Bencher` is a thin Rust-side
/// shim today - the harness does the heavy lifting (timing + alloc
/// delta + reporting) outside the user's bench fn, so existing
/// zero-argument `#[bench]` fns keep working unchanged.
#[derive(Debug, Default)]
pub struct Bencher {
    iter_count: u64,
}

impl Bencher {
    /// Constructs a [`Bencher`] requesting `iter_count` inner-loop
    /// iterations. The CLI bench harness picks this value via its
    /// auto-tuning step (start at 100, double until the wall-clock
    /// total exceeds the calibration window) before invoking the
    /// user's bench fn.
    #[must_use]
    pub fn new(iter_count: u64) -> Self {
        Self { iter_count }
    }

    /// Calibrated iteration count selected by the harness for the
    /// current bench fn. Use this when implementing a bench fn that
    /// wants to size its own inner loop.
    #[must_use]
    pub fn iter_count(&self) -> u64 {
        self.iter_count
    }

    /// Runs `f` `iter_count` times and returns the wall-clock
    /// duration of the inner loop. The bench harness divides the
    /// returned duration by `iter_count` to compute `ns/op`.
    /// Named `iter` (not `iter_for`) to match the conventional
    /// benchmark-harness vocabulary (`b.iter(|| ...)`); this is the
    /// loop driver, not an iterator producer.
    #[allow(
        clippy::iter_not_returning_iterator,
        reason = "bench-harness convention"
    )]
    pub fn iter<F: FnMut()>(&mut self, mut f: F) -> std::time::Duration {
        let started = std::time::Instant::now();
        for _ in 0..self.iter_count {
            f();
        }
        started.elapsed()
    }
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
        use parking_lot::Mutex as StdMutex;
        use std::sync::Arc;
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
        let mut handles = Vec::with_capacity(worker_count.min(queue.lock().len()));
        for _ in 0..worker_count {
            let queue = Arc::clone(&queue);
            let results = Arc::clone(&results);
            handles.push(std::thread::spawn(move || {
                loop {
                    let next = {
                        let mut q = queue.lock();
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
                    results.lock().push((idx, result));
                }
            }));
        }
        for h in handles {
            let _ = h.join();
        }
        let mut collected = Arc::try_unwrap(results).expect("arc unwrap").into_inner();
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
    fn wait_for_scheduler_idle_returns_true_when_already_idle() {
        assert!(wait_for_scheduler_idle(std::time::Duration::from_millis(
            10
        )));
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

    #[cfg(not(target_arch = "wasm32"))]
    #[test]
    fn test_server_round_trips_requests_and_joins_cleanly() {
        let mut server = TestServer::start(|request| {
            assert_eq!(request.path, "/status");
            assert_eq!(request.query, "full=1");
            crate::http::Response::text(crate::http::StatusCode::OK, "ready")
        })
        .expect("start test server");

        assert!(server.url().starts_with("http://127.0.0.1:"));
        assert_eq!(server.url_for(""), format!("{}/", server.url()));
        assert_eq!(server.url_for("status"), format!("{}/status", server.url()));

        let response =
            crate::http::get(&server.url_for("/status?full=1"), &[]).expect("request test server");
        assert_eq!(response.status, crate::http::StatusCode::OK);
        assert_eq!(response.body, b"ready");

        server.shutdown().expect("join test server");
        server.shutdown().expect("repeated shutdown is harmless");
    }
}
