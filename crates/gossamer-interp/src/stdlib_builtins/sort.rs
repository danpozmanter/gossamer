#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::doc_markdown,
    clippy::needless_pass_by_value,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::single_call_fn,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap
)]
//! Gossamer-callable `std::sort` builtins: the explicit stable-order and
//! sorted-sequence search half of the sequence surface. `Vec`'s inherent
//! `sort` is unstable, so `sort::sort_stable` is the spelling that
//! guarantees equal elements keep their input order.

use std::sync::Arc;

use crate::builtins::{BuiltinFnPub, none_variant, some_variant};
use crate::stdlib_builtins::encoding_pem::collect_array;
use crate::value::{RuntimeResult, Value};

use super::*;

/// Entry point invoked from `stdlib_builtins::install`.
pub(crate) fn install_sort(globals: &mut Vec<(&'static str, Value)>) {
    for (name, call) in [
        ("sort::sort_stable", builtin_sort_stable as BuiltinFnPub),
        ("sort::binary_search", builtin_sort_binary_search),
        ("sort::partition_point", builtin_sort_partition_point),
    ] {
        globals.push((name, crate::builtins::builtin_pub(name, call)));
    }
}

/// Total order matching the compiled shims: integers compare by value,
/// everything else by its `String` rendering, so both tiers agree.
fn cmp_values(a: &Value, b: &Value) -> std::cmp::Ordering {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.total_cmp(y),
        (Value::String(x), Value::String(y)) => x.as_str().cmp(y.as_str()),
        _ => sort_key(a).cmp(&sort_key(b)),
    }
}

fn sort_key(v: &Value) -> String {
    match v {
        Value::String(s) => s.as_str().to_string(),
        Value::Int(n) => n.to_string(),
        other => format!("{other:?}"),
    }
}

fn elements(v: Option<&Value>) -> Vec<Value> {
    v.map(collect_array).unwrap_or_default()
}

/// Rebuild a sequence in the receiver's storage shape so a packed
/// `IntArray` / `FloatVec` input yields the same representation back.
fn rebuild_like(source: Option<&Value>, items: Vec<Value>) -> Value {
    match source {
        Some(Value::IntArray(_)) => Value::IntArray(Arc::new(
            items
                .iter()
                .map(|v| match v {
                    Value::Int(n) => *n,
                    _ => 0,
                })
                .collect(),
        )),
        Some(Value::FloatVec(_)) => Value::FloatVec(Arc::new(
            items
                .iter()
                .map(|v| match v {
                    Value::Float(f) => *f,
                    Value::Int(n) => *n as f64,
                    _ => 0.0,
                })
                .collect(),
        )),
        _ => Value::Array(Arc::new(items)),
    }
}

/// `sort::sort_stable(xs) -> [T]` - a fresh ascending sequence in which
/// equal elements keep their input order.
pub(crate) fn builtin_sort_stable(args: &[Value]) -> RuntimeResult<Value> {
    let mut items = elements(args.first());
    items.sort_by(cmp_values);
    Ok(rebuild_like(args.first(), items))
}

/// Index of the first element not ordered before `target` in a sorted
/// sequence.
fn lower_bound(items: &[Value], target: &Value) -> usize {
    let (mut lo, mut hi) = (0usize, items.len());
    while lo < hi {
        let mid = usize::midpoint(lo, hi);
        if cmp_values(&items[mid], target) == std::cmp::Ordering::Less {
            lo = mid + 1;
        } else {
            hi = mid;
        }
    }
    lo
}

/// `sort::binary_search(xs, target) -> Option<i64>` over a sorted
/// sequence.
pub(crate) fn builtin_sort_binary_search(args: &[Value]) -> RuntimeResult<Value> {
    let items = elements(args.first());
    let Some(target) = args.get(1) else {
        return Ok(none_variant());
    };
    let at = lower_bound(&items, target);
    if at < items.len() && cmp_values(&items[at], target) == std::cmp::Ordering::Equal {
        Ok(some_variant(Value::Int(at as i64)))
    } else {
        Ok(none_variant())
    }
}

/// `sort::partition_point(xs, pivot) -> i64` - the count of elements
/// strictly less than `pivot` in a sorted sequence, which is also the
/// insertion index that keeps it sorted.
pub(crate) fn builtin_sort_partition_point(args: &[Value]) -> RuntimeResult<Value> {
    let items = elements(args.first());
    let Some(pivot) = args.get(1) else {
        return Ok(Value::Int(0));
    };
    Ok(Value::Int(lower_bound(&items, pivot) as i64))
}
