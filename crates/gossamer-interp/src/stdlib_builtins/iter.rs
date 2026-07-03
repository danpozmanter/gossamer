#![allow(
    unused_imports,
    dead_code,
    unreachable_pub,
    missing_docs,
    clippy::wildcard_imports,
    clippy::too_many_lines,
    clippy::too_many_arguments,
    clippy::similar_names,
    clippy::many_single_char_names,
    clippy::items_after_statements,
    clippy::cast_lossless,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::doc_markdown,
    clippy::option_if_let_else,
    clippy::match_same_arms,
    clippy::if_not_else,
    clippy::single_match_else,
    clippy::needless_pass_by_value,
    clippy::manual_let_else,
    clippy::redundant_else,
    clippy::collapsible_if,
    clippy::collapsible_else_if,
    clippy::map_unwrap_or,
    clippy::struct_excessive_bools,
    clippy::module_name_repetitions,
    clippy::unnecessary_wraps,
    clippy::large_enum_variant,
    clippy::if_same_then_else,
    clippy::single_match,
    clippy::useless_conversion,
    clippy::needless_borrows_for_generic_args,
    clippy::let_and_return,
    unsafe_op_in_unsafe_fn,
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::undocumented_unsafe_blocks,
    clippy::needless_collect,
    clippy::elidable_lifetime_names,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::must_use_candidate,
    clippy::missing_const_for_fn,
    clippy::needless_range_loop,
    clippy::cognitive_complexity,
    clippy::unused_io_amount,
    clippy::ptr_arg,
    clippy::ptr_as_ptr,
    clippy::redundant_closure,
    clippy::redundant_closure_for_method_calls,
    clippy::semicolon_if_nothing_returned,
    clippy::single_call_fn,
    clippy::unused_self,
    clippy::range_plus_one,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::cast_ptr_alignment,
    clippy::manual_assert,
    clippy::manual_string_new,
    clippy::match_bool,
    clippy::nonminimal_bool,
    clippy::redundant_pattern_matching,
    clippy::useless_let_if_seq
)]
//! Wires up Gossamer-callable builtins for stdlib modules whose
//! Rust-side implementation already exists but had no user-facing
//! exposure. Each `install_*` helper is invoked from
//! `builtins::install` so user code that writes
//! `strings::join`, `strconv::parse_i64`, `net::TcpStream::connect`,
//! `time::Instant::now`, etc. resolves to a real callable.
//!
//! All builtins return a `Result`-shaped variant (`Ok` / `Err`) on
//! fallible operations so callers can chain `?` without wrapping.

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::io::Read as IoRead;
use std::sync::Arc;

use gossamer_ast::Ident;
use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

use crate::value::SmolStr;

use gossamer_std::bufio as bufio_std;
use gossamer_std::math as math_std;
#[cfg(not(target_arch = "wasm32"))]
use gossamer_std::net as net_std;
use gossamer_std::os as os_std;
use gossamer_std::path as path_std;
use gossamer_std::strconv as strconv_std;
use gossamer_std::strings as strings_std;
use gossamer_std::unicode as unicode_std;
use gossamer_std::utf8 as utf8_std;

use gossamer_std::iter as iter_std;
use gossamer_std::utf16 as utf16_std;

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, install_module_pub, none_variant, ok_variant, some_variant,
    value_to_int,
};
use crate::value::{MapKey, NativeCall, NativeDispatch, RuntimeResult, Value};

/// Entry point invoked from `builtins::install`.
use super::*;

pub(crate) fn install_iter(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified `iter::*` names to avoid shadowing built-in
    // method dispatch (Option::map, Result::filter, Vec::any, etc.).
    //
    // Argument order is DATA-LAST throughout, matching SPEC §4.6 so
    // `xs |> iter::map(f)` desugars to `iter::map(f, xs)` and threads.
    let static_entries: &[(&str, BuiltinFnPub)] = &[
        ("count", builtin_iter_count),
        ("take", builtin_iter_take),
        ("skip", builtin_iter_skip),
        ("zip", builtin_iter_zip),
        ("enumerate", builtin_iter_enumerate),
        ("chain", builtin_iter_chain),
        ("flatten", builtin_iter_flatten),
        ("reversed", builtin_iter_reversed),
        ("dedup", builtin_iter_dedup),
        ("sum", builtin_iter_sum),
        ("product", builtin_iter_product),
        ("min", builtin_iter_min),
        ("max", builtin_iter_max),
        ("range", builtin_iter_range),
        ("range_inclusive", builtin_iter_range_inclusive),
        ("repeat", builtin_iter_repeat),
        ("unzip", builtin_iter_unzip),
        ("windowed", builtin_iter_windowed),
        ("pairwise", builtin_iter_pairwise),
        ("chunk_by_size", builtin_iter_chunk_by_size),
    ];
    for (short, call) in static_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    // Closure-taking functions - must be `native` to access the interpreter.
    let native_entries: &[(&str, NativeCall)] = &[
        ("for_each", native_iter_for_each),
        ("map", native_iter_map),
        ("filter", native_iter_filter),
        ("filter_map", native_iter_filter_map),
        ("flat_map", native_iter_flat_map),
        ("fold", native_iter_fold),
        ("reduce", native_iter_reduce),
        ("scan", native_iter_scan),
        ("sum_by", native_iter_sum_by),
        ("product_by", native_iter_product_by),
        ("any", native_iter_any),
        ("all", native_iter_all),
        ("find", native_iter_find),
        ("position", native_iter_position),
        ("find_map", native_iter_find_map),
        ("take_while", native_iter_take_while),
        ("skip_while", native_iter_skip_while),
        ("partition", native_iter_partition),
        ("sort_by", native_iter_sort_by),
        ("sort_by_key", native_iter_sort_by_key),
        ("min_by", native_iter_min_by),
        ("max_by", native_iter_max_by),
        ("min_by_key", native_iter_min_by_key),
        ("max_by_key", native_iter_max_by_key),
        ("group_by", native_iter_group_by),
        ("count_by", native_iter_count_by),
    ];
    for (short, call) in native_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }

    // Receiver-first method forms on a `Vec` receiver (`xs.take(n)`,
    // `xs.step_by(s)`), registered under the `Vec::` key ONLY:
    // a bare-name registration would shadow the scalar prelude
    // (`min(3, 7)` / `max(a, b)`) with the sequence reducers.
    let vec_builtin_entries: &[(&str, BuiltinFnPub)] = &[
        ("take", builtin_vec_take_method),
        ("step_by", builtin_vec_step_by_method),
        // Data-first single-argument reducers: the method call's
        // (receiver) argument list is already the free form's shape.
        ("sum", builtin_iter_sum),
        ("min", builtin_iter_min),
        ("max", builtin_iter_max),
    ];
    for (short, call) in vec_builtin_entries {
        let qualified: &'static str = Box::leak(format!("Vec::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    // Closure-taking combinators in method form: the receiver leads the
    // argument list, the natives are data-last - each wrapper rotates
    // the receiver to the back and delegates.
    let vec_native_entries: &[(&str, NativeCall)] = &[
        ("map", native_vec_map_method),
        ("filter", native_vec_filter_method),
        ("for_each", native_vec_for_each_method),
        ("any", native_vec_any_method),
        ("all", native_vec_all_method),
        ("find", native_vec_find_method),
        ("position", native_vec_position_method),
        ("max_by_key", native_vec_max_by_key_method),
        ("min_by_key", native_vec_min_by_key_method),
        ("fold", native_vec_fold_method),
        ("count", native_vec_count_method),
    ];
    for (short, call) in vec_native_entries {
        let qualified: &'static str = Box::leak(format!("Vec::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }
}

/// Rotates a method call's `(receiver, rest…)` argument list into the
/// data-last `(rest…, receiver)` shape the iter natives consume.
fn rotate_receiver_last(args: &[Value]) -> Vec<Value> {
    let mut v: Vec<Value> = args.get(1..).unwrap_or(&[]).to_vec();
    v.push(args.first().cloned().unwrap_or(Value::Unit));
    v
}

macro_rules! vec_method_form {
    ($name:ident, $delegate:ident) => {
        pub(crate) fn $name(
            dispatch: &mut dyn NativeDispatch,
            args: &[Value],
        ) -> RuntimeResult<Value> {
            $delegate(dispatch, &rotate_receiver_last(args))
        }
    };
}

vec_method_form!(native_vec_map_method, native_iter_map);
vec_method_form!(native_vec_filter_method, native_iter_filter);
vec_method_form!(native_vec_for_each_method, native_iter_for_each);
vec_method_form!(native_vec_any_method, native_iter_any);
vec_method_form!(native_vec_all_method, native_iter_all);
vec_method_form!(native_vec_find_method, native_iter_find);
vec_method_form!(native_vec_position_method, native_iter_position);
vec_method_form!(native_vec_max_by_key_method, native_iter_max_by_key);
vec_method_form!(native_vec_min_by_key_method, native_iter_min_by_key);
vec_method_form!(native_vec_fold_method, native_iter_fold);

/// `xs.count()` is the element count; `xs.count(f)` counts the
/// elements the predicate accepts.
pub(crate) fn native_vec_count_method(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if args.len() <= 1 {
        return builtin_iter_count(args);
    }
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut n = 0i64;
    for x in xs {
        if matches!(dispatch.call_value(&f, vec![x])?, Value::Bool(true)) {
            n += 1;
        }
    }
    Ok(Value::Int(n))
}

/// `xs.take(n)` - method form of `iter::take` (receiver-first).
pub(crate) fn builtin_vec_take_method(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    Ok(Value::Array(Arc::new(iter_std::take(n, &xs))))
}

/// `xs.step_by(step)` - every `step`-th element starting at index 0;
/// a step below 1 is treated as 1 (total, tier-identical).
pub(crate) fn builtin_vec_step_by_method(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let step = args.get(1).and_then(value_to_int).unwrap_or(1).max(1);
    let step = usize::try_from(step).unwrap_or(1);
    let out: Vec<Value> = xs.iter().step_by(step).cloned().collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_count(args: &[Value]) -> RuntimeResult<Value> {
    let n = match args.first() {
        Some(Value::Array(arr)) => arr.len(),
        Some(Value::IntArray(arr)) => arr.len(),
        Some(Value::FloatVec(arr)) => arr.len(),
        _ => 0,
    };
    Ok(Value::Int(n as i64))
}

pub(crate) fn builtin_iter_take(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let taken = iter_std::take(n, &xs);
    Ok(Value::Array(Arc::new(taken)))
}

pub(crate) fn builtin_iter_skip(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let rest = iter_std::skip(n, &xs);
    Ok(Value::Array(Arc::new(rest)))
}

pub(crate) fn builtin_iter_zip(args: &[Value]) -> RuntimeResult<Value> {
    let a = collect_array(args.first().unwrap_or(&Value::Unit));
    let b = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let zipped: Vec<Value> = a
        .into_iter()
        .zip(b)
        .map(|(x, y)| Value::Tuple(Arc::from(vec![x, y])))
        .collect();
    Ok(Value::Array(Arc::new(zipped)))
}

pub(crate) fn builtin_iter_enumerate(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let enumerated: Vec<Value> = xs
        .into_iter()
        .enumerate()
        .map(|(i, x)| Value::Tuple(Arc::from(vec![Value::Int(i as i64), x])))
        .collect();
    Ok(Value::Array(Arc::new(enumerated)))
}

pub(crate) fn builtin_iter_chain(args: &[Value]) -> RuntimeResult<Value> {
    let mut result = collect_array(args.first().unwrap_or(&Value::Unit));
    result.extend(collect_array(args.get(1).unwrap_or(&Value::Unit)));
    Ok(Value::Array(Arc::new(result)))
}

pub(crate) fn builtin_iter_flatten(args: &[Value]) -> RuntimeResult<Value> {
    let outer = collect_array(args.first().unwrap_or(&Value::Unit));
    let flat: Vec<Value> = outer.into_iter().flat_map(|v| collect_array(&v)).collect();
    Ok(Value::Array(Arc::new(flat)))
}

pub(crate) fn builtin_iter_reversed(args: &[Value]) -> RuntimeResult<Value> {
    let mut xs = collect_array(args.first().unwrap_or(&Value::Unit));
    xs.reverse();
    Ok(Value::Array(Arc::new(xs)))
}

pub(crate) fn builtin_iter_dedup(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut out: Vec<Value> = Vec::new();
    for x in xs {
        if out.last().is_none_or(|last| !values_equal(last, &x)) {
            out.push(x);
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x.as_str() == y.as_str(),
        _ => false,
    }
}

pub(crate) fn builtin_iter_sum(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::IntArray(arr)) => Ok(Value::Int(arr.iter().sum())),
        Some(Value::FloatVec(arr)) => Ok(Value::Float(arr.iter().sum())),
        Some(Value::Array(arr)) => {
            // Try i64 first, then f64.
            let mut int_sum: i64 = 0;
            let mut float_sum: f64 = 0.0;
            let mut is_float = false;
            for v in arr.iter() {
                match v {
                    Value::Int(n) => {
                        int_sum += n;
                        float_sum += *n as f64;
                    }
                    Value::Float(f) => {
                        is_float = true;
                        float_sum += f;
                    }
                    _ => {}
                }
            }
            if is_float {
                Ok(Value::Float(float_sum))
            } else {
                Ok(Value::Int(int_sum))
            }
        }
        _ => Ok(Value::Int(0)),
    }
}

// ------- closure-taking iter natives (DATA-LAST argument order) -------
//
// Each native reads its callable(s) from the head of `args` and the data
// from `args.last()`. This matches SPEC §4.6 so the pipe form
// `xs |> iter::f(g)` desugars to `iter::f(g, xs)` and threads.

pub(crate) fn native_iter_for_each(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for x in xs {
        dispatch.call_value(&f, vec![x])?;
    }
    Ok(Value::Unit)
}

pub(crate) fn native_iter_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        out.push(dispatch.call_value(&f, vec![x])?);
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_filter(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::new();
    for x in xs {
        if let Value::Bool(true) = dispatch.call_value(&p, vec![x.clone()])? {
            out.push(x);
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_filter_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::new();
    for x in xs {
        if let Some(v) = some_payload(&dispatch.call_value(&f, vec![x])?) {
            out.push(v);
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_fold(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    // Signature: fold(init, f, xs) - data still last.
    let mut acc = args.first().cloned().unwrap_or(Value::Unit);
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(2).unwrap_or(&Value::Unit));
    for x in xs {
        acc = dispatch.call_value(&f, vec![acc, x])?;
    }
    Ok(acc)
}

pub(crate) fn native_iter_reduce(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(first) = iter.next() else {
        return Ok(none_variant());
    };
    let mut acc = first;
    for x in iter {
        acc = dispatch.call_value(&f, vec![acc, x])?;
    }
    Ok(some_variant(acc))
}

pub(crate) fn native_iter_scan(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    // Signature: scan(init, f, xs).
    let mut acc = args.first().cloned().unwrap_or(Value::Unit);
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(2).unwrap_or(&Value::Unit));
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        acc = dispatch.call_value(&f, vec![acc.clone(), x])?;
        out.push(acc.clone());
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_sum_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut int_sum: i64 = 0;
    let mut float_sum: f64 = 0.0;
    let mut is_float = false;
    for x in xs {
        match dispatch.call_value(&f, vec![x])? {
            Value::Int(n) => {
                int_sum += n;
                float_sum += n as f64;
            }
            Value::Float(v) => {
                is_float = true;
                float_sum += v;
            }
            _ => {}
        }
    }
    Ok(if is_float {
        Value::Float(float_sum)
    } else {
        Value::Int(int_sum)
    })
}

pub(crate) fn native_iter_product_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut int_prod: i64 = 1;
    let mut float_prod: f64 = 1.0;
    let mut is_float = false;
    for x in xs {
        match dispatch.call_value(&f, vec![x])? {
            Value::Int(n) => {
                int_prod = int_prod.wrapping_mul(n);
                float_prod *= n as f64;
            }
            Value::Float(v) => {
                is_float = true;
                float_prod *= v;
            }
            _ => {}
        }
    }
    Ok(if is_float {
        Value::Float(float_prod)
    } else {
        Value::Int(int_prod)
    })
}

pub(crate) fn native_iter_any(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for x in xs {
        if matches!(dispatch.call_value(&p, vec![x])?, Value::Bool(true)) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

pub(crate) fn native_iter_all(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for x in xs {
        if !matches!(dispatch.call_value(&p, vec![x])?, Value::Bool(true)) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

pub(crate) fn native_iter_find(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for x in xs {
        if matches!(dispatch.call_value(&p, vec![x.clone()])?, Value::Bool(true)) {
            return Ok(some_variant(x));
        }
    }
    Ok(none_variant())
}

pub(crate) fn native_iter_position(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for (i, x) in xs.into_iter().enumerate() {
        if matches!(dispatch.call_value(&p, vec![x])?, Value::Bool(true)) {
            return Ok(some_variant(Value::Int(i as i64)));
        }
    }
    Ok(none_variant())
}

pub(crate) fn native_iter_find_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    for x in xs {
        let r = dispatch.call_value(&f, vec![x])?;
        if let Some(v) = some_payload(&r) {
            return Ok(some_variant(v));
        }
    }
    Ok(none_variant())
}

pub(crate) fn native_iter_take_while(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::new();
    for x in xs {
        if matches!(dispatch.call_value(&p, vec![x.clone()])?, Value::Bool(true)) {
            out.push(x);
        } else {
            break;
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_skip_while(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::new();
    let mut dropping = true;
    for x in xs {
        if dropping && matches!(dispatch.call_value(&p, vec![x.clone()])?, Value::Bool(true)) {
            continue;
        }
        dropping = false;
        out.push(x);
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_partition(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let p = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut yes = Vec::new();
    let mut no = Vec::new();
    for x in xs {
        if matches!(dispatch.call_value(&p, vec![x.clone()])?, Value::Bool(true)) {
            yes.push(x);
        } else {
            no.push(x);
        }
    }
    Ok(Value::Tuple(Arc::from(vec![
        Value::Array(Arc::new(yes)),
        Value::Array(Arc::new(no)),
    ])))
}

pub(crate) fn native_iter_sort_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let cmp = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = xs;
    let mut error: Option<crate::value::RuntimeError> = None;
    out.sort_by(|a, b| {
        if error.is_some() {
            return std::cmp::Ordering::Equal;
        }
        match dispatch.call_value(&cmp, vec![a.clone(), b.clone()]) {
            Ok(Value::Int(n)) => match n.signum() {
                -1 => std::cmp::Ordering::Less,
                1 => std::cmp::Ordering::Greater,
                _ => std::cmp::Ordering::Equal,
            },
            Ok(_) => std::cmp::Ordering::Equal,
            Err(e) => {
                error = Some(e);
                std::cmp::Ordering::Equal
            }
        }
    });
    if let Some(e) = error {
        return Err(e);
    }
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn native_iter_sort_by_key(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let key = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut keyed: Vec<(Value, Value)> = Vec::with_capacity(xs.len());
    for x in xs {
        let k = dispatch.call_value(&key, vec![x.clone()])?;
        keyed.push((k, x));
    }
    keyed.sort_by(|a, b| compare_values_total(&a.0, &b.0));
    Ok(Value::Array(Arc::new(
        keyed.into_iter().map(|(_, v)| v).collect(),
    )))
}

pub(crate) fn native_iter_min_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let cmp = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    for x in iter {
        let ord = dispatch.call_value(&cmp, vec![x.clone(), best.clone()])?;
        if let Value::Int(n) = ord
            && n < 0
        {
            best = x;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn native_iter_max_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let cmp = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    for x in iter {
        let ord = dispatch.call_value(&cmp, vec![x.clone(), best.clone()])?;
        if let Value::Int(n) = ord
            && n > 0
        {
            best = x;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn native_iter_min_by_key(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let key = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    let mut best_key = dispatch.call_value(&key, vec![best.clone()])?;
    for x in iter {
        let k = dispatch.call_value(&key, vec![x.clone()])?;
        if compare_values_total(&k, &best_key) == std::cmp::Ordering::Less {
            best = x;
            best_key = k;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn native_iter_max_by_key(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let key = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    let mut best_key = dispatch.call_value(&key, vec![best.clone()])?;
    for x in iter {
        let k = dispatch.call_value(&key, vec![x.clone()])?;
        if compare_values_total(&k, &best_key) == std::cmp::Ordering::Greater {
            best = x;
            best_key = k;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn native_iter_group_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let key = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut groups: rustc_hash::FxHashMap<MapKey, Vec<Value>> = rustc_hash::FxHashMap::default();
    for x in xs {
        let k = dispatch.call_value(&key, vec![x.clone()])?;
        groups.entry(MapKey::from_value(&k)).or_default().push(x);
    }
    let map: rustc_hash::FxHashMap<MapKey, Value> = groups
        .into_iter()
        .map(|(k, v)| (k, Value::Array(Arc::new(v))))
        .collect();
    Ok(Value::Map(Arc::new(parking_lot::Mutex::new(map))))
}

pub(crate) fn native_iter_count_by(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let key = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut counts: rustc_hash::FxHashMap<MapKey, i64> = rustc_hash::FxHashMap::default();
    let mut all_int_keys = true;
    for x in xs {
        let k = dispatch.call_value(&key, vec![x])?;
        all_int_keys &= matches!(k, Value::Int(_));
        *counts.entry(MapKey::from_value(&k)).or_insert(0) += 1;
    }
    // An i64-keyed count map must come back as the typed IntMap: the
    // bytecode compiler's fast path emits IntMapGetOr/IntMapInc for
    // `HashMap<i64, i64>`-typed receivers, and those ops hard-fail on
    // a generic Value::Map (the "receiver lost typed invariant" bug).
    if all_int_keys {
        let typed: rustc_hash::FxHashMap<i64, i64> = counts
            .into_iter()
            .filter_map(|(k, v)| match k {
                MapKey::Int(n) => Some((n, v)),
                _ => None,
            })
            .collect();
        return Ok(Value::IntMap(Arc::new(parking_lot::Mutex::new(typed))));
    }
    let map: rustc_hash::FxHashMap<MapKey, Value> = counts
        .into_iter()
        .map(|(k, v)| (k, Value::Int(v)))
        .collect();
    Ok(Value::Map(Arc::new(parking_lot::Mutex::new(map))))
}

pub(crate) fn native_iter_flat_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let mut out = Vec::new();
    for x in xs {
        let result = dispatch.call_value(&f, vec![x])?;
        out.extend(collect_array(&result));
    }
    Ok(Value::Array(Arc::new(out)))
}

// ------- non-closure iter builtins added in the F#-parity pass -------

pub(crate) fn builtin_iter_product(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::IntArray(arr)) => Ok(Value::Int(arr.iter().product())),
        Some(Value::FloatVec(arr)) => Ok(Value::Float(arr.iter().product())),
        Some(Value::Array(arr)) => {
            let mut int_prod: i64 = 1;
            let mut float_prod: f64 = 1.0;
            let mut is_float = false;
            for v in arr.iter() {
                match v {
                    Value::Int(n) => {
                        int_prod = int_prod.wrapping_mul(*n);
                        float_prod *= *n as f64;
                    }
                    Value::Float(f) => {
                        is_float = true;
                        float_prod *= f;
                    }
                    _ => {}
                }
            }
            Ok(if is_float {
                Value::Float(float_prod)
            } else {
                Value::Int(int_prod)
            })
        }
        _ => Ok(Value::Int(1)),
    }
}

/// Public alias so `crate::builtins::builtin_min_dispatch` can
/// fall through to the collection-shaped `min` when called with a
/// single Vec / Array argument.
pub(crate) fn iter_min(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_min(args)
}

/// Public alias so `crate::builtins::builtin_max_dispatch` can
/// fall through to the collection-shaped `max`.
pub(crate) fn iter_max(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_max(args)
}

pub(crate) fn builtin_iter_min(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    for x in iter {
        if compare_values_total(&x, &best) == std::cmp::Ordering::Less {
            best = x;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn builtin_iter_max(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut iter = xs.into_iter();
    let Some(mut best) = iter.next() else {
        return Ok(none_variant());
    };
    for x in iter {
        if compare_values_total(&x, &best) == std::cmp::Ordering::Greater {
            best = x;
        }
    }
    Ok(some_variant(best))
}

pub(crate) fn builtin_iter_range(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let end = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::IntArray(Arc::new(iter_std::range(start, end))))
}

pub(crate) fn builtin_iter_range_inclusive(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let end = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::IntArray(Arc::new(iter_std::range_inclusive(
        start, end,
    ))))
}

pub(crate) fn builtin_iter_repeat(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().cloned().unwrap_or(Value::Unit);
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let out: Vec<Value> = (0..n).map(|_| v.clone()).collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_unzip(args: &[Value]) -> RuntimeResult<Value> {
    let pairs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut a = Vec::with_capacity(pairs.len());
    let mut b = Vec::with_capacity(pairs.len());
    for p in pairs {
        if let Value::Tuple(t) = p
            && t.len() >= 2
        {
            a.push(t[0].clone());
            b.push(t[1].clone());
        }
    }
    Ok(Value::Tuple(Arc::from(vec![
        Value::Array(Arc::new(a)),
        Value::Array(Arc::new(b)),
    ])))
}

pub(crate) fn builtin_iter_windowed(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    if n == 0 || xs.len() < n {
        return Ok(Value::Array(Arc::new(Vec::new())));
    }
    let out: Vec<Value> = xs
        .windows(n)
        .map(|w| Value::Array(Arc::new(w.to_vec())))
        .collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_pairwise(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let out: Vec<Value> = xs
        .windows(2)
        .map(|w| Value::Tuple(Arc::from(vec![w[0].clone(), w[1].clone()])))
        .collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_chunk_by_size(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    if n == 0 {
        return Ok(Value::Array(Arc::new(Vec::new())));
    }
    let out: Vec<Value> = xs
        .chunks(n)
        .map(|c| Value::Array(Arc::new(c.to_vec())))
        .collect();
    Ok(Value::Array(Arc::new(out)))
}

// ------- support helpers for iter combinators -------

/// Extract the payload of a `Some(_)` variant, or `None` for `None`/non-variant.
pub(crate) fn some_payload(v: &Value) -> Option<Value> {
    if let Value::Variant(inner) = v
        && inner.name == "Some"
        && let Some(first) = inner.fields.first()
    {
        return Some(first.clone());
    }
    None
}

/// Total order over `Value`s for sort/min/max stability. Falls back to
/// `Equal` for cross-type comparisons rather than panicking.
pub(crate) fn compare_values_total(a: &Value, b: &Value) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => x.partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Int(x), Value::Float(y)) => (*x as f64).partial_cmp(y).unwrap_or(Ordering::Equal),
        (Value::Float(x), Value::Int(y)) => x.partial_cmp(&(*y as f64)).unwrap_or(Ordering::Equal),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Char(x), Value::Char(y)) => x.cmp(y),
        (Value::String(x), Value::String(y)) => x.as_str().cmp(y.as_str()),
        _ => Ordering::Equal,
    }
}

// ----------------------------------------------------------------------
// option - F#-style chaining surface for `Option<T>` (SPEC §10.4a).
// Data-last argument order. Methods are kept on `Option<T>` itself
// (Rust-style); these are the free-function siblings for use with
// `|>`.
