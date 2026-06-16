#![allow(
    unused_imports,
    dead_code,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_possible_truncation,
    clippy::cast_precision_loss,
    clippy::missing_errors_doc,
    clippy::unnecessary_wraps,
    clippy::needless_pass_by_value
)]
//! `std::metrics` builtins for the bytecode VM — Prometheus-compatible
//! Counter / Gauge / Histogram and a rendering Registry. Metric and
//! registry state live in process-global registries keyed by `id`, so
//! `&self` mutating methods reach through the registry instead of the
//! VM's receiver write-back (mirrors `math::rand::Rng`). The metric
//! primitives and the text-exposition rendering are
//! `gossamer_std::metrics`, so the Prometheus text matches the
//! compiled tiers byte-for-byte.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::sync::Arc;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicI64, Ordering};

use gossamer_ast::Ident;
use gossamer_std::metrics::{Counter, Gauge, Histogram, Metric, Registry};

use crate::builtins::{BuiltinFnPub, value_to_int};
use crate::value::{RuntimeResult, Value};

static METRICS: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, Metric>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static REGISTRIES: LazyLock<parking_lot::ReentrantMutex<RefCell<StdHashMap<i64, Registry>>>> =
    LazyLock::new(|| parking_lot::ReentrantMutex::new(RefCell::new(StdHashMap::new())));
static NEXT_ID: AtomicI64 = AtomicI64::new(1);

fn with_metrics<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, Metric>>) -> R) -> R {
    let guard = METRICS.lock();
    f(&guard)
}

fn with_registries<R>(f: impl FnOnce(&RefCell<StdHashMap<i64, Registry>>) -> R) -> R {
    let guard = REGISTRIES.lock();
    f(&guard)
}

pub(crate) fn install_metrics(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("Counter::new", builtin_counter_new),
        ("Counter::inc", builtin_counter_inc),
        ("Counter::value", builtin_counter_value),
        ("Gauge::new", builtin_gauge_new),
        ("Gauge::set", builtin_gauge_set),
        ("Gauge::inc", builtin_gauge_inc),
        ("Gauge::dec", builtin_gauge_dec),
        ("Gauge::value", builtin_gauge_value),
        ("Histogram::new", builtin_histogram_new),
        ("Histogram::observe", builtin_histogram_observe),
        ("Histogram::sum", builtin_histogram_sum),
        ("Histogram::count", builtin_histogram_count),
        ("Registry::new", builtin_registry_new),
        ("Registry::register", builtin_registry_register),
        ("Registry::render", builtin_registry_render),
        ("serve_metrics", builtin_serve_metrics),
    ];
    for (name, call) in entries {
        // The handle struct names are `metrics::Counter` /
        // `metrics::Gauge` / `metrics::Histogram` / `metrics::Registry`,
        // so `qualified_method_key` emits `metrics::<Type>::method`; the
        // module-qualified spelling covers that and free-call
        // resolution of `metrics::<Type>::new`, the bare spelling covers
        // a `use std::metrics::<Type>` call site.
        let mod_q: &'static str = Box::leak(format!("metrics::{name}").into_boxed_str());
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
        globals.push((mod_q, crate::builtins::builtin_pub(mod_q, *call)));
    }
}

fn metric_handle(kind: &'static str, id: i64) -> Value {
    Value::struct_(
        kind,
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__metric"), Value::Int(id))])),
    )
}

fn registry_handle(id: i64) -> Value {
    Value::struct_(
        "metrics::Registry",
        Arc::unwrap_or_clone(Arc::new(vec![(Ident::new("__registry"), Value::Int(id))])),
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

fn f64_arg(args: &[Value], idx: usize) -> f64 {
    match args.get(idx) {
        Some(Value::Float(x)) => *x,
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    }
}

fn buckets_arg(value: Option<&Value>) -> Vec<f64> {
    let Some(v) = value else {
        return Vec::new();
    };
    match v {
        Value::FloatVec(items) => items.iter().copied().collect(),
        Value::IntArray(items) => items.iter().map(|n| *n as f64).collect(),
        Value::Array(items) => items.iter().filter_map(elem_f64).collect(),
        rx @ Value::FloatArray(_) => match rx.float_array_to_value_array() {
            Value::Array(items) => items.iter().filter_map(elem_f64).collect(),
            _ => Vec::new(),
        },
        _ => Vec::new(),
    }
}

fn elem_f64(v: &Value) -> Option<f64> {
    match v {
        Value::Float(x) => Some(*x),
        Value::Int(n) => Some(*n as f64),
        _ => None,
    }
}

pub(crate) fn builtin_counter_new(args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let metric = Metric::Counter(Counter::new(&str_arg(args, 0), &str_arg(args, 1)));
    with_metrics(|m| m.borrow_mut().insert(id, metric));
    Ok(metric_handle("metrics::Counter", id))
}

pub(crate) fn builtin_counter_inc(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__metric")) {
        with_metrics(|m| {
            if let Some(Metric::Counter(c)) = m.borrow().get(&id) {
                c.inc();
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_counter_value(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(|v| handle_id(v, "__metric"))
        .and_then(|id| {
            with_metrics(|m| match m.borrow().get(&id) {
                Some(Metric::Counter(c)) => Some(c.value()),
                _ => None,
            })
        })
        .unwrap_or(0);
    Ok(Value::Int(v as i64))
}

pub(crate) fn builtin_gauge_new(args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let metric = Metric::Gauge(Gauge::new(&str_arg(args, 0), &str_arg(args, 1)));
    with_metrics(|m| m.borrow_mut().insert(id, metric));
    Ok(metric_handle("metrics::Gauge", id))
}

pub(crate) fn builtin_gauge_set(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__metric")) {
        let v = f64_arg(args, 1);
        with_metrics(|m| {
            if let Some(Metric::Gauge(g)) = m.borrow().get(&id) {
                g.set(v);
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_gauge_inc(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__metric")) {
        with_metrics(|m| {
            if let Some(Metric::Gauge(g)) = m.borrow().get(&id) {
                g.add(1.0);
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_gauge_dec(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__metric")) {
        with_metrics(|m| {
            if let Some(Metric::Gauge(g)) = m.borrow().get(&id) {
                g.sub(1.0);
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_gauge_value(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(|v| handle_id(v, "__metric"))
        .and_then(|id| {
            with_metrics(|m| match m.borrow().get(&id) {
                Some(Metric::Gauge(g)) => Some(g.value()),
                _ => None,
            })
        })
        .unwrap_or(0.0);
    Ok(Value::Float(v))
}

pub(crate) fn builtin_histogram_new(args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let buckets = buckets_arg(args.get(2));
    let metric = Metric::Histogram(Histogram::new(
        &str_arg(args, 0),
        &str_arg(args, 1),
        &buckets,
    ));
    with_metrics(|m| m.borrow_mut().insert(id, metric));
    Ok(metric_handle("metrics::Histogram", id))
}

pub(crate) fn builtin_histogram_observe(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(|v| handle_id(v, "__metric")) {
        let v = f64_arg(args, 1);
        with_metrics(|m| {
            if let Some(Metric::Histogram(h)) = m.borrow().get(&id) {
                h.observe(v);
            }
        });
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_histogram_sum(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(|v| handle_id(v, "__metric"))
        .and_then(|id| {
            with_metrics(|m| match m.borrow().get(&id) {
                Some(Metric::Histogram(h)) => Some(h.sum()),
                _ => None,
            })
        })
        .unwrap_or(0.0);
    Ok(Value::Float(v))
}

pub(crate) fn builtin_histogram_count(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(|v| handle_id(v, "__metric"))
        .and_then(|id| {
            with_metrics(|m| match m.borrow().get(&id) {
                Some(Metric::Histogram(h)) => Some(h.count()),
                _ => None,
            })
        })
        .unwrap_or(0);
    Ok(Value::Int(v as i64))
}

pub(crate) fn builtin_registry_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    with_registries(|r| r.borrow_mut().insert(id, Registry::new()));
    Ok(registry_handle(id))
}

pub(crate) fn builtin_registry_register(args: &[Value]) -> RuntimeResult<Value> {
    let reg_id = args.first().and_then(|v| handle_id(v, "__registry"));
    let metric_id = args.get(1).and_then(|v| handle_id(v, "__metric"));
    if let (Some(rid), Some(mid)) = (reg_id, metric_id) {
        let metric = with_metrics(|m| m.borrow().get(&mid).cloned());
        if let Some(metric) = metric {
            with_registries(|r| {
                if let Some(reg) = r.borrow().get(&rid) {
                    reg.register(metric);
                }
            });
        }
    }
    Ok(Value::Unit)
}

pub(crate) fn builtin_registry_render(args: &[Value]) -> RuntimeResult<Value> {
    let text = args
        .first()
        .and_then(|v| handle_id(v, "__registry"))
        .and_then(|id| with_registries(|r| r.borrow().get(&id).map(Registry::expose)))
        .unwrap_or_default();
    Ok(Value::String(text.into()))
}

/// `metrics::serve_metrics(addr, registry) -> Result<(), errors::Error>`
/// — serves the registry on `/metrics` over the std http server. Blocks
/// the calling goroutine until shutdown; the compiled tier serves over
/// the runtime's own server via `gos_rt_metrics_serve`.
pub(crate) fn builtin_serve_metrics(args: &[Value]) -> RuntimeResult<Value> {
    let addr = str_arg(args, 0);
    let registry = args
        .get(1)
        .and_then(|v| handle_id(v, "__registry"))
        .and_then(|id| with_registries(|r| r.borrow().get(&id).cloned()));
    let Some(registry) = registry else {
        return Ok(crate::builtins::err_variant(
            "serve_metrics: unknown registry handle",
        ));
    };
    match gossamer_std::metrics::serve_metrics(&addr, registry) {
        Ok(()) => Ok(Value::variant("Ok", vec![Value::Unit])),
        Err(e) => Ok(crate::builtins::err_variant(format!("{e}"))),
    }
}
