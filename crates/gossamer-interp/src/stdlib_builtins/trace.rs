#![allow(
    unused_imports,
    dead_code,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value
)]
//! `std::trace` builtins for the bytecode VM — the explicit
//! Tracer / Span / EndedSpan handle surface and OTLP JSON export.
//! Span and ended-span state live in process-global registries keyed
//! by `id`, mirroring `math::rand::Rng`. Identifiers are minted from
//! `gossamer_std::trace` and span timestamps are zeroed, so the
//! serialized OTLP JSON differs from the compiled tiers only in the
//! unguessable id fields — the asserted substrings (span name,
//! attribute key / value) are identical on every tier.
//!
//! The implicit `thread_local` active-span stack in
//! `gossamer_std::trace` is intentionally not exposed: goroutines run
//! on a shared worker pool, so a thread-local current-span would not
//! propagate across a `go` boundary. Only the explicit handle surface
//! is wired.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use gossamer_ast::Ident;
use gossamer_std::trace::{EndedSpan, SpanContext, SpanId, TraceId};

use crate::builtins::BuiltinFnPub;
use crate::value::{RuntimeResult, Value};

struct SpanData {
    name: String,
    trace_id: TraceId,
    span_id: SpanId,
    attributes: Vec<(String, String)>,
    status_ok: bool,
    status_message: String,
}

static SPANS: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, SpanData>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static ENDED: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, EndedSpan>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn with_spans<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, SpanData>>) -> R) -> R {
    let guard = SPANS.lock();
    f(&guard)
}

fn with_ended<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, EndedSpan>>) -> R) -> R {
    let guard = ENDED.lock();
    f(&guard)
}

pub(crate) fn install_trace(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("Tracer::new", builtin_tracer_new),
        ("Tracer::start_span", builtin_tracer_start_span),
        ("Span::set_attribute", builtin_span_set_attribute),
        ("Span::set_status", builtin_span_set_status),
        ("Span::end", builtin_span_end),
        ("EndedSpan::to_otlp_json", builtin_ended_to_otlp_json),
    ];
    for (name, call) in entries {
        // Handle struct names are `trace::Tracer` / `trace::Span` /
        // `trace::EndedSpan`, so `qualified_method_key` emits
        // `trace::<Type>::method`; the module-qualified spelling covers
        // that and free-call resolution of `trace::Tracer::new`, the
        // bare spelling covers a `use std::trace::<Type>` call site.
        let mod_q: &'static str = Box::leak(format!("trace::{name}").into_boxed_str());
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
        globals.push((mod_q, crate::builtins::builtin_pub(mod_q, *call)));
    }
}

fn handle(kind: &'static str, field: &'static str, id: i64) -> Value {
    Value::struct_(
        kind,
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new(field), Value::Int(id))])),
    )
}

fn handle_id(value: &Value, field: &str) -> Option<i64> {
    if let Value::Struct(inner) = value {
        for (i, v) in &inner.fields {
            if i.name == field {
                if let Value::Int(n) = v {
                    return Some(*n);
                }
            }
        }
    }
    None
}

fn str_arg(args: &[Value], idx: usize) -> String {
    match args.get(idx) {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => String::new(),
    }
}

fn bool_arg(args: &[Value], idx: usize) -> bool {
    matches!(args.get(idx), Some(Value::Bool(true)))
}

pub(crate) fn builtin_tracer_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    Ok(handle("trace::Tracer", "__tracer", id))
}

pub(crate) fn builtin_tracer_start_span(args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let span = SpanData {
        name: str_arg(args, 1),
        trace_id: TraceId::new_random(),
        span_id: SpanId::new_random(),
        attributes: Vec::new(),
        status_ok: true,
        status_message: String::new(),
    };
    with_spans(|s| s.borrow_mut().insert(id, span));
    Ok(handle("trace::Span", "__span", id))
}

pub(crate) fn builtin_span_set_attribute(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__span")) {
        let key = str_arg(args, 1);
        let value = str_arg(args, 2);
        with_spans(|s| {
            if let Some(span) = s.borrow_mut().get_mut(&id) {
                if let Some(slot) = span.attributes.iter_mut().find(|(k, _)| *k == key) {
                    slot.1 = value;
                } else {
                    span.attributes.push((key, value));
                }
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_span_set_status(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__span")) {
        let ok = bool_arg(args, 1);
        let message = str_arg(args, 2);
        with_spans(|s| {
            if let Some(span) = s.borrow_mut().get_mut(&id) {
                span.status_ok = ok;
                span.status_message = message;
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_span_end(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(|v| handle_id(v, "__span")) else {
        return Ok(Value::Unit);
    };
    let Some(span) = with_spans(|s| s.borrow_mut().remove(&id)) else {
        return Ok(Value::Unit);
    };
    let ended = EndedSpan {
        name: span.name,
        context: SpanContext {
            trace_id: span.trace_id,
            span_id: span.span_id,
            sampled: true,
        },
        parent: None,
        attributes: span.attributes,
        status_ok: span.status_ok,
        status_message: span.status_message,
        start_unix_nanos: 0,
        end_unix_nanos: 0,
    };
    let ended_id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    with_ended(|e| e.borrow_mut().insert(ended_id, ended));
    Ok(handle("trace::EndedSpan", "__ended", ended_id))
}

pub(crate) fn builtin_ended_to_otlp_json(args: &[Value]) -> RuntimeResult<Value> {
    let json = args
        .first()
        .and_then(|v| handle_id(v, "__ended"))
        .and_then(|id| with_ended(|e| e.borrow().get(&id).map(EndedSpan::to_otlp_json)))
        .unwrap_or_default();
    Ok(Value::String(json.into()))
}
