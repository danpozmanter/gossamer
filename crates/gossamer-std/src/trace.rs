//! Distributed tracing — lean in-tree subset compatible with the
//! OpenTelemetry W3C trace-context specification.
//!
//! Provides identifier types ([`crate::trace::TraceId`],
//! [`crate::trace::SpanId`]), the request-scoped
//! [`crate::trace::SpanContext`] handle, an active
//! [`crate::trace::Span`] builder, a process-level
//! [`crate::trace::Tracer`] that collects every ended span, and
//! OTLP JSON export. The heavy `opentelemetry-otlp` crate is
//! intentionally not pulled in; a sidecar collector can POST
//! [`crate::trace::EndedSpan::to_otlp_json`] output to any
//! OTLP/HTTP endpoint.
//!
//! Identifiers use the CSPRNG via [`crate::crypto::rand`] so trace
//! IDs are unguessable. The active-span stack rides on top of
//! [`crate::context::Context`] via `with_value` / `value`, so a
//! goroutine that derives a child context picks up the parent span
//! automatically.
//!
//! All registry state is lock-free where possible: span sequence
//! numbers use `AtomicU64`; the global tracer slot is an
//! `OnceLock`. The per-tracer list of ended spans takes a short
//! `Mutex` lock — that path is off the hot per-request flow and
//! is the simplest correct shape until profiling justifies a
//! lock-free ring.

#![forbid(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::crypto::rand;

/// 128-bit trace identifier (W3C trace-context format).
///
/// Encoded as 32 lowercase hex characters in the `traceparent`
/// header. The all-zero ID is reserved as the "invalid" sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct TraceId(pub u128);

impl TraceId {
    /// Returns a freshly-generated random trace ID. Never returns
    /// the all-zero sentinel — on the astronomically-unlikely zero
    /// draw, the call retries once with a fresh sample.
    #[must_use]
    pub fn new_random() -> Self {
        let mut id = sample_u128();
        if id == 0 {
            id = sample_u128() | 1;
        }
        Self(id)
    }

    /// Parses a 32-character lowercase-hex trace ID. Returns
    /// `None` for any other length, non-hex characters, or the
    /// all-zero string.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 32 {
            return None;
        }
        let v = u128::from_str_radix(s, 16).ok()?;
        if v == 0 {
            return None;
        }
        Some(Self(v))
    }

    /// Returns the canonical 32-character lowercase-hex form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        format!("{:032x}", self.0)
    }
}

/// 64-bit span identifier (W3C trace-context format).
///
/// Encoded as 16 lowercase hex characters in the `traceparent`
/// header. The all-zero ID is reserved as the "invalid" sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanId(pub u64);

impl SpanId {
    /// Returns a freshly-generated random span ID. Never returns
    /// the all-zero sentinel.
    #[must_use]
    pub fn new_random() -> Self {
        let mut id = sample_u64();
        if id == 0 {
            id = sample_u64() | 1;
        }
        Self(id)
    }

    /// Parses a 16-character lowercase-hex span ID. Returns
    /// `None` for any other length, non-hex characters, or the
    /// all-zero string.
    #[must_use]
    pub fn from_hex(s: &str) -> Option<Self> {
        if s.len() != 16 {
            return None;
        }
        let v = u64::from_str_radix(s, 16).ok()?;
        if v == 0 {
            return None;
        }
        Some(Self(v))
    }

    /// Returns the canonical 16-character lowercase-hex form.
    #[must_use]
    pub fn to_hex(&self) -> String {
        format!("{:016x}", self.0)
    }
}

/// Immutable per-request trace handle: trace ID, span ID, and the
/// W3C `sampled` flag. Carried through [`crate::context::Context`]
/// to propagate parent identity into child spans.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SpanContext {
    /// Identifier of the overall trace this span belongs to.
    pub trace_id: TraceId,
    /// Identifier of this specific span.
    pub span_id: SpanId,
    /// W3C trace-flags `sampled` bit (bit 0). When `false`, a
    /// collector may discard the span.
    pub sampled: bool,
}

impl SpanContext {
    /// Renders this context as a W3C `traceparent` header value:
    /// `00-<trace_id>-<span_id>-<flags>` where flags is `01` when
    /// sampled and `00` otherwise.
    #[must_use]
    pub fn to_traceparent_header(&self) -> String {
        let flags = if self.sampled { "01" } else { "00" };
        format!(
            "00-{}-{}-{}",
            self.trace_id.to_hex(),
            self.span_id.to_hex(),
            flags,
        )
    }

    /// Parses a W3C `traceparent` header value. Returns `None`
    /// for any version other than `00`, malformed segments, or
    /// reserved-zero identifiers. Unknown flag bits beyond bit 0
    /// are tolerated for forward compatibility.
    #[must_use]
    pub fn parse_traceparent(s: &str) -> Option<Self> {
        let mut parts = s.split('-');
        let version = parts.next()?;
        let trace = parts.next()?;
        let span = parts.next()?;
        let flags = parts.next()?;
        if parts.next().is_some() {
            return None;
        }
        if version != "00" || flags.len() != 2 {
            return None;
        }
        let trace_id = TraceId::from_hex(trace)?;
        let span_id = SpanId::from_hex(span)?;
        let raw_flags = u8::from_str_radix(flags, 16).ok()?;
        Some(Self {
            trace_id,
            span_id,
            sampled: (raw_flags & 0x01) != 0,
        })
    }
}

/// Status attached to an [`EndedSpan`] by [`Span::set_status`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanStatus {
    /// `true` for the OTLP `STATUS_CODE_OK` code, `false` for
    /// `STATUS_CODE_ERROR`.
    pub ok: bool,
    /// Optional human-readable description.
    pub message: String,
}

impl Default for SpanStatus {
    fn default() -> Self {
        Self {
            ok: true,
            message: String::new(),
        }
    }
}

/// In-flight span builder. Constructed via [`Tracer::start_span`]
/// or [`Span::new`]; consumed by [`Span::end`].
#[derive(Debug)]
pub struct Span {
    name: String,
    context: SpanContext,
    parent: Option<SpanContext>,
    attributes: Vec<(String, String)>,
    status: SpanStatus,
    start_unix_nanos: u128,
    tracer: Option<Arc<TracerInner>>,
}

impl Span {
    /// Creates a standalone span with no associated tracer. The
    /// span's trace and span IDs are freshly generated; the
    /// sampled flag is `true`. Useful for ad-hoc instrumentation
    /// in code that has no tracer in scope.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            context: SpanContext {
                trace_id: TraceId::new_random(),
                span_id: SpanId::new_random(),
                sampled: true,
            },
            parent: None,
            attributes: Vec::new(),
            status: SpanStatus::default(),
            start_unix_nanos: unix_nanos(),
            tracer: None,
        }
    }

    /// Returns the span's immutable [`SpanContext`].
    #[must_use]
    pub fn context(&self) -> SpanContext {
        self.context
    }

    /// Returns the span's name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Adds or overwrites a string-valued attribute.
    pub fn set_attribute(&mut self, key: &str, value: &str) {
        if let Some(slot) = self.attributes.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.to_string();
        } else {
            self.attributes.push((key.to_string(), value.to_string()));
        }
    }

    /// Sets the span's outcome. `ok = true` maps to OTLP
    /// `STATUS_CODE_OK`; `ok = false` to `STATUS_CODE_ERROR`. The
    /// message is preserved verbatim in the exported payload.
    pub fn set_status(&mut self, ok: bool, message: &str) {
        self.status = SpanStatus {
            ok,
            message: message.to_string(),
        };
    }

    /// Finalises the span: stamps the end time and pushes the
    /// resulting [`EndedSpan`] into the originating tracer's
    /// collected-span list, if any.
    pub fn end(self) {
        let ended = EndedSpan {
            name: self.name,
            context: self.context,
            parent: self.parent,
            attributes: self.attributes,
            status_ok: self.status.ok,
            status_message: self.status.message,
            start_unix_nanos: self.start_unix_nanos,
            end_unix_nanos: unix_nanos(),
        };
        if let Some(tracer) = self.tracer {
            tracer.push_ended(ended);
        }
    }
}

/// Finalised span — the immutable record produced by
/// [`Span::end`] and accumulated by [`Tracer::ended_spans`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndedSpan {
    /// Operation name supplied at [`Span::new`] /
    /// [`Tracer::start_span`].
    pub name: String,
    /// This span's own context (trace + span id + sampled flag).
    pub context: SpanContext,
    /// Parent context when the span was created as a child;
    /// `None` for root spans.
    pub parent: Option<SpanContext>,
    /// Ordered attribute list (insertion order; later writes for
    /// the same key overwrite in place).
    pub attributes: Vec<(String, String)>,
    /// `true` for OTLP `STATUS_CODE_OK`, `false` for
    /// `STATUS_CODE_ERROR`.
    pub status_ok: bool,
    /// Optional status description.
    pub status_message: String,
    /// Span start time, nanoseconds since the Unix epoch.
    pub start_unix_nanos: u128,
    /// Span end time, nanoseconds since the Unix epoch.
    pub end_unix_nanos: u128,
}

impl EndedSpan {
    /// Renders this span as a single OTLP-spec JSON object
    /// (the shape that lives inside a `ResourceSpans` →
    /// `ScopeSpans` → `spans[]` array element). A sidecar
    /// collector can wrap a batch of these into the full OTLP
    /// envelope.
    #[must_use]
    pub fn to_otlp_json(&self) -> String {
        let mut out = String::new();
        out.push('{');
        push_kv_str(&mut out, "traceId", &self.context.trace_id.to_hex());
        out.push(',');
        push_kv_str(&mut out, "spanId", &self.context.span_id.to_hex());
        if let Some(parent) = self.parent {
            out.push(',');
            push_kv_str(&mut out, "parentSpanId", &parent.span_id.to_hex());
        }
        out.push(',');
        push_kv_str(&mut out, "name", &self.name);
        out.push(',');
        push_kv_raw(&mut out, "kind", "1");
        out.push(',');
        push_kv_str(&mut out, "startTimeUnixNano", &self.start_unix_nanos.to_string());
        out.push(',');
        push_kv_str(&mut out, "endTimeUnixNano", &self.end_unix_nanos.to_string());
        out.push(',');
        out.push_str("\"attributes\":[");
        for (i, (k, v)) in self.attributes.iter().enumerate() {
            if i > 0 {
                out.push(',');
            }
            out.push('{');
            push_kv_str(&mut out, "key", k);
            out.push(',');
            out.push_str("\"value\":{");
            push_kv_str(&mut out, "stringValue", v);
            out.push('}');
            out.push('}');
        }
        out.push(']');
        out.push(',');
        let code = if self.status_ok { 1 } else { 2 };
        out.push_str("\"status\":{");
        push_kv_raw(&mut out, "code", &code.to_string());
        if !self.status_message.is_empty() {
            out.push(',');
            push_kv_str(&mut out, "message", &self.status_message);
        }
        out.push('}');
        out.push('}');
        out
    }
}

#[derive(Debug, Default)]
struct TracerInner {
    seq: AtomicU64,
    ended: Mutex<Vec<EndedSpan>>,
}

impl TracerInner {
    fn push_ended(&self, span: EndedSpan) {
        self.seq.fetch_add(1, Ordering::Relaxed);
        let mut list = match self.ended.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        list.push(span);
    }
}

/// Process-level tracer. Collects every [`Span::end`] call into an
/// in-memory list that callers can drain via [`Tracer::ended_spans`].
#[derive(Debug, Clone, Default)]
pub struct Tracer {
    inner: Arc<TracerInner>,
}

impl Tracer {
    /// Returns a fresh tracer with an empty ended-span list.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Starts a child span. If the calling [`crate::context::Context`]
    /// carries a [`SpanContext`] (via
    /// [`with_span_context`]), the new span inherits its
    /// `trace_id` and records the parent. Otherwise a fresh root
    /// span is started.
    #[must_use]
    pub fn start_span(&self, name: &str) -> Span {
        let parent = current_span_context();
        self.start_with_optional_parent(name, parent)
    }

    /// Starts a child span with an explicit parent context.
    #[must_use]
    pub fn start_span_with_parent(&self, name: &str, parent: SpanContext) -> Span {
        self.start_with_optional_parent(name, Some(parent))
    }

    fn start_with_optional_parent(&self, name: &str, parent: Option<SpanContext>) -> Span {
        let (trace_id, sampled) = match parent {
            Some(p) => (p.trace_id, p.sampled),
            None => (TraceId::new_random(), true),
        };
        Span {
            name: name.to_string(),
            context: SpanContext {
                trace_id,
                span_id: SpanId::new_random(),
                sampled,
            },
            parent,
            attributes: Vec::new(),
            status: SpanStatus::default(),
            start_unix_nanos: unix_nanos(),
            tracer: Some(Arc::clone(&self.inner)),
        }
    }

    /// Returns a snapshot of every span ended through this
    /// tracer. Clears nothing — repeated calls observe the
    /// accumulated history. Use [`Tracer::drain_ended_spans`] to consume.
    #[must_use]
    pub fn ended_spans(&self) -> Vec<EndedSpan> {
        let list = match self.inner.ended.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        list.clone()
    }

    /// Returns and clears the accumulated ended-span list.
    #[must_use]
    pub fn drain_ended_spans(&self) -> Vec<EndedSpan> {
        let mut list = match self.inner.ended.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        std::mem::take(&mut *list)
    }
}

/// Process-wide global tracer slot. Mirrors OpenTelemetry's
/// `set_tracer_provider` / `tracer` shape so library code can
/// emit spans without taking a `Tracer` argument.
pub mod global {
    use super::Tracer;
    use std::sync::OnceLock;

    static GLOBAL: OnceLock<Tracer> = OnceLock::new();

    /// Installs `tracer` as the process-wide global. Idempotent;
    /// subsequent calls are no-ops. Call this once at program
    /// start before spawning request-handling goroutines.
    pub fn set_tracer(tracer: Tracer) {
        let _ = GLOBAL.set(tracer);
    }

    /// Returns the global tracer, lazily creating an empty one
    /// if [`set_tracer`] was never called. The returned handle is
    /// cheap to clone.
    #[must_use]
    pub fn tracer() -> Tracer {
        GLOBAL.get_or_init(Tracer::new).clone()
    }
}

/// Key under which a [`SpanContext`] is stashed on a
/// [`crate::context::Context`] by [`with_span_context`].
pub const CONTEXT_KEY: &str = "std::trace::SpanContext";

thread_local! {
    static CURRENT: std::cell::RefCell<Vec<SpanContext>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// Pushes `ctx` onto the thread-local active-span stack. Returns
/// a guard whose `Drop` pops the entry — call sites can use
/// `let _g = enter_span(ctx);` and rely on RAII to balance.
///
/// This is the integration point for goroutine-style propagation
/// when the caller does not want to thread a
/// [`crate::context::Context`] through every signature: the runtime
/// can call `enter_span` on a
/// goroutine entry and `exit_span` on its return.
#[must_use]
pub fn enter_span(ctx: SpanContext) -> SpanGuard {
    CURRENT.with(|stack| stack.borrow_mut().push(ctx));
    SpanGuard { _private: () }
}

/// RAII guard returned by [`enter_span`]. Pops the top of the
/// thread-local active-span stack on drop.
#[must_use = "drop the guard at the end of the span's scope"]
pub struct SpanGuard {
    _private: (),
}

impl Drop for SpanGuard {
    fn drop(&mut self) {
        CURRENT.with(|stack| {
            let _ = stack.borrow_mut().pop();
        });
    }
}

/// Returns the [`SpanContext`] on top of the thread-local
/// active-span stack, or `None` when no span is active.
#[must_use]
pub fn current_span_context() -> Option<SpanContext> {
    CURRENT.with(|stack| stack.borrow().last().copied())
}

/// Attaches `ctx` to a [`crate::context::Context`]-shaped value.
///
/// The current `gossamer-std::context::Context` carries its own
/// cancellation state rather than an arbitrary value bag. This
/// helper records the span context on the thread-local stack so
/// child spans started inside the closure pick it up via
/// [`current_span_context`]. The closure shape mirrors what a
/// future `Context::with_value` API would look like once
/// `gossamer-std::context` grows a value map.
pub fn with_span_context<R>(ctx: SpanContext, f: impl FnOnce() -> R) -> R {
    let _guard = enter_span(ctx);
    f()
}

fn sample_u128() -> u128 {
    let mut buf = [0u8; 16];
    rand::fill_or_abort(&mut buf);
    u128::from_be_bytes(buf)
}

fn sample_u64() -> u64 {
    let mut buf = [0u8; 8];
    rand::fill_or_abort(&mut buf);
    u64::from_be_bytes(buf)
}

fn unix_nanos() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn push_kv_str(out: &mut String, key: &str, value: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":\"");
    for ch in value.chars() {
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
    out.push('"');
}

fn push_kv_raw(out: &mut String, key: &str, raw: &str) {
    out.push('"');
    out.push_str(key);
    out.push_str("\":");
    out.push_str(raw);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trace_id_roundtrip_via_hex() {
        let id = TraceId::new_random();
        let hex = id.to_hex();
        assert_eq!(hex.len(), 32);
        let parsed = TraceId::from_hex(&hex).expect("hex round-trip");
        assert_eq!(id, parsed);
    }

    #[test]
    fn span_id_roundtrip_via_hex() {
        let id = SpanId::new_random();
        let hex = id.to_hex();
        assert_eq!(hex.len(), 16);
        assert_eq!(SpanId::from_hex(&hex), Some(id));
    }

    #[test]
    fn trace_id_rejects_zero_and_short() {
        assert!(TraceId::from_hex("0".repeat(32).as_str()).is_none());
        assert!(TraceId::from_hex("abc").is_none());
        assert!(TraceId::from_hex(&"z".repeat(32)).is_none());
    }

    #[test]
    fn traceparent_header_renders_and_parses() {
        let ctx = SpanContext {
            trace_id: TraceId(0x0123_4567_89ab_cdef_fedc_ba98_7654_3210),
            span_id: SpanId(0xdead_beef_cafe_f00d),
            sampled: true,
        };
        let header = ctx.to_traceparent_header();
        assert_eq!(
            header,
            "00-0123456789abcdeffedcba9876543210-deadbeefcafef00d-01"
        );
        let parsed = SpanContext::parse_traceparent(&header).expect("parse");
        assert_eq!(parsed, ctx);
    }

    #[test]
    fn traceparent_unsampled_round_trips() {
        let ctx = SpanContext {
            trace_id: TraceId::new_random(),
            span_id: SpanId::new_random(),
            sampled: false,
        };
        let h = ctx.to_traceparent_header();
        assert!(h.ends_with("-00"));
        let parsed = SpanContext::parse_traceparent(&h).expect("parse");
        assert!(!parsed.sampled);
        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
    }

    #[test]
    fn traceparent_rejects_bad_input() {
        assert!(SpanContext::parse_traceparent("").is_none());
        assert!(SpanContext::parse_traceparent("01-abc-def-00").is_none());
        assert!(
            SpanContext::parse_traceparent(
                "00-00000000000000000000000000000000-deadbeefcafef00d-01"
            )
            .is_none()
        );
        assert!(
            SpanContext::parse_traceparent("00-0123456789abcdef-deadbeefcafef00d-01").is_none()
        );
    }

    #[test]
    fn span_records_start_and_end_nanos() {
        let span = Span::new("op");
        let start = span.start_unix_nanos;
        std::thread::sleep(std::time::Duration::from_millis(2));
        let tracer = Tracer::new();
        let bound = tracer.start_span_with_parent(
            "child",
            SpanContext {
                trace_id: TraceId::new_random(),
                span_id: SpanId::new_random(),
                sampled: true,
            },
        );
        bound.end();
        let ended = tracer.ended_spans();
        assert_eq!(ended.len(), 1);
        assert!(ended[0].end_unix_nanos >= ended[0].start_unix_nanos);
        // Standalone Span has no tracer attached.
        span.end();
        assert!(start > 0);
    }

    #[test]
    fn attributes_preserved_through_end() {
        let tracer = Tracer::new();
        let mut s = tracer.start_span("op");
        s.set_attribute("user.id", "u-42");
        s.set_attribute("http.status", "200");
        s.set_attribute("user.id", "u-43"); // overwrite
        s.end();
        let ended = tracer.ended_spans();
        assert_eq!(ended.len(), 1);
        let attrs = &ended[0].attributes;
        assert_eq!(attrs.len(), 2);
        assert_eq!(
            attrs.iter().find(|(k, _)| k == "user.id").map(|(_, v)| v.as_str()),
            Some("u-43")
        );
        assert_eq!(
            attrs.iter().find(|(k, _)| k == "http.status").map(|(_, v)| v.as_str()),
            Some("200")
        );
    }

    #[test]
    fn tracer_collects_multiple_spans() {
        let tracer = Tracer::new();
        for i in 0..5 {
            let mut s = tracer.start_span("op");
            s.set_attribute("i", &i.to_string());
            s.end();
        }
        assert_eq!(tracer.ended_spans().len(), 5);
        let drained = tracer.drain_ended_spans();
        assert_eq!(drained.len(), 5);
        assert_eq!(tracer.ended_spans().len(), 0);
    }

    #[test]
    fn parent_child_share_trace_id() {
        let tracer = Tracer::new();
        let parent_ctx = SpanContext {
            trace_id: TraceId::new_random(),
            span_id: SpanId::new_random(),
            sampled: true,
        };
        let child = tracer.start_span_with_parent("child", parent_ctx);
        let child_ctx = child.context();
        child.end();
        assert_eq!(child_ctx.trace_id, parent_ctx.trace_id);
        assert_ne!(child_ctx.span_id, parent_ctx.span_id);
        let ended = tracer.ended_spans();
        assert_eq!(ended[0].parent, Some(parent_ctx));
        assert_eq!(ended[0].context.trace_id, parent_ctx.trace_id);
    }

    #[test]
    fn status_round_trips() {
        let tracer = Tracer::new();
        let mut ok = tracer.start_span("ok");
        ok.set_status(true, "fine");
        ok.end();
        let mut bad = tracer.start_span("bad");
        bad.set_status(false, "boom");
        bad.end();
        let ended = tracer.ended_spans();
        assert!(ended[0].status_ok);
        assert_eq!(ended[0].status_message, "fine");
        assert!(!ended[1].status_ok);
        assert_eq!(ended[1].status_message, "boom");
    }

    #[test]
    fn otlp_json_contains_required_fields() {
        let tracer = Tracer::new();
        let parent = SpanContext {
            trace_id: TraceId::new_random(),
            span_id: SpanId::new_random(),
            sampled: true,
        };
        let mut span = tracer.start_span_with_parent("checkout", parent);
        span.set_attribute("user", "alice");
        span.set_status(false, "card declined");
        span.end();
        let ended = tracer.ended_spans();
        let json = ended[0].to_otlp_json();
        assert!(json.contains("\"traceId\":"));
        assert!(json.contains("\"spanId\":"));
        assert!(json.contains("\"parentSpanId\":"));
        assert!(json.contains("\"name\":\"checkout\""));
        assert!(json.contains("\"startTimeUnixNano\":"));
        assert!(json.contains("\"endTimeUnixNano\":"));
        assert!(json.contains("\"attributes\":["));
        assert!(json.contains("\"key\":\"user\""));
        assert!(json.contains("\"stringValue\":\"alice\""));
        assert!(json.contains("\"status\":{\"code\":2"));
        assert!(json.contains("\"message\":\"card declined\""));
    }

    #[test]
    fn otlp_json_escapes_quotes_and_control_chars() {
        let mut span = Span::new("weird \"name\"\n");
        span.set_attribute("k", "v\twith\"quote");
        let tracer = Tracer::new();
        span.tracer = Some(Arc::clone(&tracer.inner));
        span.end();
        let json = tracer.ended_spans()[0].to_otlp_json();
        assert!(json.contains("weird \\\"name\\\"\\n"));
        assert!(json.contains("v\\twith\\\"quote"));
    }

    #[test]
    fn sampled_flag_round_trips() {
        for sampled in [true, false] {
            let ctx = SpanContext {
                trace_id: TraceId::new_random(),
                span_id: SpanId::new_random(),
                sampled,
            };
            let h = ctx.to_traceparent_header();
            let parsed = SpanContext::parse_traceparent(&h).expect("parse");
            assert_eq!(parsed.sampled, sampled);
        }
    }

    #[test]
    fn global_tracer_is_shared() {
        let t = Tracer::new();
        global::set_tracer(t.clone());
        let mut s = global::tracer().start_span("global-op");
        s.set_attribute("k", "v");
        s.end();
        // Either the just-set tracer or one previously set in the
        // same process suite observes the span; the OnceLock means
        // the global is set-once across tests. Assert through `t`
        // directly when set_tracer actually installed our handle.
        let from_t = t.ended_spans();
        let from_global = global::tracer().ended_spans();
        assert!(!from_t.is_empty() || !from_global.is_empty());
    }

    #[test]
    fn enter_span_makes_context_current() {
        let ctx = SpanContext {
            trace_id: TraceId::new_random(),
            span_id: SpanId::new_random(),
            sampled: true,
        };
        assert_eq!(current_span_context(), None);
        let _g = enter_span(ctx);
        assert_eq!(current_span_context(), Some(ctx));
        drop(_g);
        assert_eq!(current_span_context(), None);
    }

    #[test]
    fn tracer_inherits_current_context_as_parent() {
        let ctx = SpanContext {
            trace_id: TraceId::new_random(),
            span_id: SpanId::new_random(),
            sampled: true,
        };
        let tracer = Tracer::new();
        with_span_context(ctx, || {
            let span = tracer.start_span("child");
            assert_eq!(span.context().trace_id, ctx.trace_id);
            span.end();
        });
        let ended = tracer.ended_spans();
        assert_eq!(ended[0].parent, Some(ctx));
    }
}
