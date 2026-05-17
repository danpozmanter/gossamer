//! Runtime support for `std::http::health` — operational health,
//! readiness, and liveness endpoints.
//!
//! A `Health` builder collects named [`Probe`]s; each probe runs a
//! check and returns `Ok` (healthy) or `Err(message)` (degraded).
//! The `handler()` method returns a closure that any router can
//! mount at `/health` / `/readiness` / `/liveness`.
//!
//! Conventional split:
//!
//! - `/liveness` — process up and not deadlocked. Returns 200 with
//!   an empty body unless the process is wedged. Kubernetes uses
//!   this to decide when to restart.
//! - `/readiness` — process able to serve traffic. Returns 200 when
//!   all downstream probes pass; 503 otherwise. Kubernetes uses
//!   this to gate inclusion in the load balancer.
//! - `/health` — combined view; typically mirrors readiness for
//!   convenience.
//!
//! Probes that take more than ~1 s should run on a background
//! goroutine and cache their result; this module deliberately runs
//! probes synchronously on the request path. Wrap long-running
//! checks in a `Probe` that reads a cached value updated by a
//! separate timer.

#![forbid(unsafe_code)]

use std::sync::Arc;
use std::time::Duration;

use crate::http::{Headers, Request, Response, StatusCode};
use crate::http_router::Params;

/// A single health check.
///
/// Implementations should be cheap (microseconds, not milliseconds)
/// and never panic. The returned `Result` is rendered into the
/// JSON response: `Ok(())` becomes `"status": "ok"`; `Err(msg)`
/// becomes `"status": "fail", "message": msg`.
pub trait Probe: Send + Sync + 'static {
    /// Runs the check and returns its outcome.
    fn check(&self) -> Result<(), String>;
}

impl<F> Probe for F
where
    F: Fn() -> Result<(), String> + Send + Sync + 'static,
{
    fn check(&self) -> Result<(), String> {
        self()
    }
}

struct Entry {
    name: String,
    probe: Arc<dyn Probe>,
    timeout: Duration,
}

/// Health-check registry. Clone to share across goroutines.
#[derive(Clone, Default)]
pub struct Health {
    inner: Arc<parking_lot::RwLock<Vec<Entry>>>,
}

impl Health {
    /// Empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers a named probe with a one-second default timeout.
    pub fn probe(&self, name: impl Into<String>, probe: impl Probe) -> &Self {
        self.probe_with_timeout(name, Duration::from_secs(1), probe)
    }

    /// Registers a probe with an explicit deadline. Probes that
    /// take longer than `timeout` are reported as failed with a
    /// timeout message — useful for downstream calls.
    pub fn probe_with_timeout(
        &self,
        name: impl Into<String>,
        timeout: Duration,
        probe: impl Probe,
    ) -> &Self {
        let mut guard = self.inner.write();
        guard.push(Entry {
            name: name.into(),
            probe: Arc::new(probe),
            timeout,
        });
        self
    }

    /// Returns true if every probe passes. Implementations that
    /// need readiness vs liveness distinctions should keep two
    /// `Health` instances.
    #[must_use]
    pub fn all_ok(&self) -> bool {
        let guard = self.inner.read();
        for entry in guard.iter() {
            if run_with_timeout(&*entry.probe, entry.timeout).is_err() {
                return false;
            }
        }
        true
    }

    /// Runs every probe and returns a rendered JSON status report.
    ///
    /// Shape:
    ///
    /// ```json
    /// {
    ///   "status": "ok",
    ///   "checks": {
    ///     "db": {"status": "ok"},
    ///     "redis": {"status": "fail", "message": "connection refused"}
    ///   }
    /// }
    /// ```
    #[must_use]
    pub fn snapshot(&self) -> (bool, String) {
        let guard = self.inner.read();
        let mut ok = true;
        let mut checks = String::from("{");
        for (i, entry) in guard.iter().enumerate() {
            if i > 0 {
                checks.push(',');
            }
            let res = run_with_timeout(&*entry.probe, entry.timeout);
            checks.push('"');
            push_json_escaped(&mut checks, &entry.name);
            checks.push_str("\":");
            match res {
                Ok(()) => {
                    checks.push_str(r#"{"status":"ok"}"#);
                }
                Err(msg) => {
                    ok = false;
                    checks.push_str(r#"{"status":"fail","message":""#);
                    push_json_escaped(&mut checks, &msg);
                    checks.push_str(r#""}"#);
                }
            }
        }
        checks.push('}');
        let status = if ok { "ok" } else { "fail" };
        let body = format!(r#"{{"status":"{status}","checks":{checks}}}"#);
        (ok, body)
    }

    /// Returns a handler suitable for `/health` / `/readiness`.
    /// Returns 200 + JSON body when every probe passes, 503 + JSON
    /// when any probe fails. HEAD requests omit the body.
    pub fn handler(
        &self,
    ) -> impl Fn(&Request, &Params) -> Response + Send + Sync + 'static + Clone {
        let this = self.clone();
        move |req: &Request, _p: &Params| -> Response {
            let (ok, body) = this.snapshot();
            let status = if ok { StatusCode::OK } else { StatusCode(503) };
            let mut headers = Headers::new();
            headers.insert("content-type", "application/json; charset=utf-8");
            headers.insert("cache-control", "no-store");
            let body = if matches!(req.method, crate::http::Method::Head) {
                Vec::new()
            } else {
                body.into_bytes()
            };
            Response {
                status,
                headers,
                body,
            }
        }
    }

    /// Convenience: a handler that always returns 200. Use for
    /// liveness when "process is up" is the only signal needed.
    pub fn liveness_handler()
    -> impl Fn(&Request, &Params) -> Response + Send + Sync + 'static + Clone {
        |_req: &Request, _p: &Params| -> Response {
            let mut headers = Headers::new();
            headers.insert("content-type", "application/json; charset=utf-8");
            headers.insert("cache-control", "no-store");
            Response {
                status: StatusCode::OK,
                headers,
                body: br#"{"status":"ok"}"#.to_vec(),
            }
        }
    }
}

fn run_with_timeout(probe: &dyn Probe, timeout: Duration) -> Result<(), String> {
    // The probe runs synchronously; timeout is an upper bound on
    // wall-clock spent inside it. We measure after the call so that
    // a slow probe still completes (and we can report it). The
    // timeout is advisory — for hard cancellation, the probe must
    // honour its own deadline.
    let start = std::time::Instant::now();
    let result = probe.check();
    let elapsed = start.elapsed();
    if elapsed > timeout {
        return Err(format!(
            "probe exceeded timeout ({}ms > {}ms)",
            elapsed.as_millis(),
            timeout.as_millis()
        ));
    }
    result
}

fn push_json_escaped(out: &mut String, s: &str) {
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
}

/// Predefined: a probe that always passes. Useful as a placeholder.
#[must_use]
pub fn always_ok() -> impl Probe {
    || -> Result<(), String> { Ok(()) }
}

/// Predefined: a probe that always fails with `message`. Useful for
/// testing readiness-gated rollouts.
pub fn always_fail(message: impl Into<String>) -> impl Probe {
    let m = message.into();
    move || -> Result<(), String> { Err(m.clone()) }
}

/// Probe that performs a TCP connect with a deadline. Useful for
/// "is the database port reachable" checks. Does not authenticate;
/// for that, ship a probe that runs a real query.
pub fn tcp_probe(addr: impl Into<String>, deadline: Duration) -> impl Probe {
    let addr = addr.into();
    move || -> Result<(), String> {
        std::net::TcpStream::connect_timeout(
            &addr
                .parse()
                .map_err(|e: std::net::AddrParseError| e.to_string())?,
            deadline,
        )
        .map(drop)
        .map_err(|e| e.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::Method;

    fn req(method: Method) -> Request {
        Request {
            method,
            path: "/health".into(),
            query: String::new(),
            headers: Headers::new(),
            body: Vec::new(),
            context: crate::context::Context::background(),
            trailers: None,
        }
    }

    #[test]
    fn empty_health_is_ok() {
        let h = Health::new();
        assert!(h.all_ok());
        let (ok, body) = h.snapshot();
        assert!(ok);
        assert_eq!(body, r#"{"status":"ok","checks":{}}"#);
    }

    #[test]
    fn single_passing_probe() {
        let h = Health::new();
        h.probe("db", always_ok());
        assert!(h.all_ok());
        let (ok, body) = h.snapshot();
        assert!(ok);
        assert!(body.contains(r#""db":{"status":"ok"}"#));
    }

    #[test]
    fn single_failing_probe() {
        let h = Health::new();
        h.probe("db", always_fail("connection refused"));
        assert!(!h.all_ok());
        let (ok, body) = h.snapshot();
        assert!(!ok);
        assert!(body.contains(r#""db":{"status":"fail","message":"connection refused"}"#));
        assert!(body.starts_with(r#"{"status":"fail""#));
    }

    #[test]
    fn mixed_probes() {
        let h = Health::new();
        h.probe("db", always_ok());
        h.probe("redis", always_fail("ECONNREFUSED"));
        h.probe("kafka", always_ok());
        assert!(!h.all_ok());
        let (_, body) = h.snapshot();
        // db ok, redis fail, kafka ok
        assert!(body.contains(r#""db":{"status":"ok"}"#));
        assert!(body.contains(r#""redis":{"status":"fail""#));
        assert!(body.contains(r#""kafka":{"status":"ok"}"#));
    }

    #[test]
    fn handler_returns_200_when_healthy() {
        let h = Health::new();
        h.probe("db", always_ok());
        let handler = h.handler();
        let resp = handler(&req(Method::Get), &Params::default());
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(
            resp.headers.get("content-type").unwrap_or(""),
            "application/json; charset=utf-8"
        );
        assert!(!resp.body.is_empty());
    }

    #[test]
    fn handler_returns_503_when_degraded() {
        let h = Health::new();
        h.probe("db", always_fail("down"));
        let handler = h.handler();
        let resp = handler(&req(Method::Get), &Params::default());
        assert_eq!(resp.status, StatusCode(503));
    }

    #[test]
    fn head_request_returns_empty_body() {
        let h = Health::new();
        h.probe("db", always_ok());
        let handler = h.handler();
        let resp = handler(&req(Method::Head), &Params::default());
        assert_eq!(resp.status, StatusCode::OK);
        assert!(resp.body.is_empty());
    }

    #[test]
    fn liveness_handler_always_ok() {
        let handler = Health::liveness_handler();
        let resp = handler(&req(Method::Get), &Params::default());
        assert_eq!(resp.status, StatusCode::OK);
        assert_eq!(resp.body, br#"{"status":"ok"}"#.to_vec());
    }

    #[test]
    fn json_escapes_message_with_quotes() {
        let h = Health::new();
        h.probe("svc", always_fail(r#"got "weird" \data"#));
        let (_, body) = h.snapshot();
        assert!(body.contains(r#"got \"weird\" \\data"#));
    }

    #[test]
    fn probe_via_closure() {
        let h = Health::new();
        h.probe("custom", || -> Result<(), String> { Ok(()) });
        assert!(h.all_ok());
    }

    #[test]
    fn clone_shares_probes() {
        let h = Health::new();
        let h2 = h.clone();
        h.probe("db", always_ok());
        // h2 sees the probe registered through h.
        assert!(h2.all_ok());
        let (ok, body) = h2.snapshot();
        assert!(ok);
        assert!(body.contains(r#""db""#));
    }

    #[test]
    fn timeout_marks_probe_failed() {
        let h = Health::new();
        h.probe_with_timeout(
            "slow",
            Duration::from_millis(1),
            || -> Result<(), String> {
                std::thread::sleep(Duration::from_millis(20));
                Ok(())
            },
        );
        let (ok, body) = h.snapshot();
        assert!(!ok);
        assert!(body.contains("exceeded timeout"));
    }
}
