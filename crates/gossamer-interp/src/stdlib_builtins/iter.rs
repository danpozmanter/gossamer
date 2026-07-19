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

use std::cell::{Cell, RefCell};
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
use crate::value::{
    MapKey, NativeCall, NativeDispatch, RuntimeError, RuntimeResult, Value, dense_map,
    dense_map_with_capacity,
};

/// Entry point invoked from `builtins::install`.
use super::*;

thread_local! {
    static LAZY_ITERATORS_ENABLED: RefCell<bool> = const { RefCell::new(false) };
    static NEXT_LAZY_ITER_ID: RefCell<i64> = const { RefCell::new(1) };
    static LAZY_ITER_STATES: RefCell<StdHashMap<i64, LazyIterState>> = RefCell::new(StdHashMap::new());
    static LAZY_VEC_BORROW_COUNT: Cell<usize> = const { Cell::new(0) };
    static LAZY_VEC_BORROWERS: RefCell<StdHashMap<usize, usize>> = RefCell::new(StdHashMap::new());
    static LAZY_VEC_GENERATIONS: RefCell<StdHashMap<usize, u64>> = RefCell::new(StdHashMap::new());
    static LAZY_VEC_REPLACEMENTS: RefCell<StdHashMap<usize, StdHashMap<usize, Value>>> = RefCell::new(StdHashMap::new());
}

#[derive(Debug)]
enum LazyIterState {
    Array {
        items: Arc<Vec<Value>>,
        source_id: usize,
        generation: u64,
        index: usize,
    },
    IntArray {
        items: Arc<Vec<i64>>,
        source_id: usize,
        generation: u64,
        index: usize,
    },
    FloatVec {
        items: Arc<Vec<f64>>,
        source_id: usize,
        generation: u64,
        index: usize,
    },
    Range {
        current: i64,
        end: i64,
        inclusive: bool,
        start_open: bool,
        end_open: bool,
        finished: bool,
    },
    Once {
        item: Option<Value>,
    },
    Repeat {
        item: Value,
        remaining: usize,
    },
    Take {
        upstream: Value,
        remaining: usize,
    },
    Skip {
        upstream: Value,
        remaining: usize,
    },
    Enumerate {
        upstream: Value,
        index: i64,
    },
    Chain {
        first: Value,
        second: Value,
        in_second: bool,
    },
    Zip {
        left: Value,
        right: Value,
    },
    Map {
        f: Value,
        upstream: Value,
    },
    Filter {
        p: Value,
        upstream: Value,
    },
    FilterMap {
        f: Value,
        upstream: Value,
    },
    FlatMap {
        f: Value,
        upstream: Value,
        current: Option<Value>,
    },
    Scan {
        acc: Value,
        f: Value,
        upstream: Value,
    },
    TakeWhile {
        p: Value,
        upstream: Value,
        done: bool,
    },
    SkipWhile {
        p: Value,
        upstream: Value,
        skipping: bool,
    },
}

pub fn set_lazy_iterators_enabled(enabled: bool) {
    let stale = LAZY_ITER_STATES.with(|states| {
        states
            .borrow_mut()
            .drain()
            .map(|(_, state)| state)
            .collect::<Vec<_>>()
    });
    for state in stale {
        discard_lazy_state(state);
    }
    LAZY_VEC_BORROW_COUNT.with(|count| count.set(0));
    LAZY_VEC_BORROWERS.with(|borrowers| borrowers.borrow_mut().clear());
    LAZY_VEC_GENERATIONS.with(|generations| generations.borrow_mut().clear());
    LAZY_VEC_REPLACEMENTS.with(|replacements| replacements.borrow_mut().clear());
    LAZY_ITERATORS_ENABLED.with(|cell| *cell.borrow_mut() = enabled);
}

fn lazy_iterators_enabled() -> bool {
    LAZY_ITERATORS_ENABLED.with(|cell| *cell.borrow())
}

fn new_lazy_iter(state: LazyIterState) -> Value {
    let id = NEXT_LAZY_ITER_ID.with(|next| {
        let mut next = next.borrow_mut();
        let id = *next;
        *next = next.saturating_add(1);
        id
    });
    LAZY_ITER_STATES.with(|states| {
        states.borrow_mut().insert(id, state);
    });
    Value::LazyIter(id)
}

/// Creates a lazy integer range while preserving which bounds were omitted so
/// diagnostics and the REPL can render the source-level shape without pulling
/// from the iterator.
pub(crate) fn new_range_iter(
    start: i64,
    end: i64,
    inclusive: bool,
    start_open: bool,
    end_open: bool,
) -> Value {
    new_lazy_iter(LazyIterState::Range {
        current: start,
        end,
        inclusive,
        start_open,
        end_open,
        finished: false,
    })
}

/// Returns a source-like representation without consuming the iterator.
pub(crate) fn lazy_iter_repr(id: i64) -> Option<String> {
    LAZY_ITER_STATES.with(|states| {
        let states = states.borrow();
        match states.get(&id)? {
            LazyIterState::Range {
                current,
                end,
                inclusive,
                start_open,
                end_open,
                ..
            } => {
                let start = if *start_open {
                    String::new()
                } else {
                    current.to_string()
                };
                let op = if *inclusive && !*end_open {
                    "..="
                } else {
                    ".."
                };
                let finish = if *end_open {
                    String::new()
                } else {
                    end.to_string()
                };
                Some(format!("{start}{op}{finish}"))
            }
            _ => None,
        }
    })
}

/// Returns open/closed range metadata without consuming the lazy range.
pub(crate) fn lazy_range_bounds(id: i64) -> Option<(i64, i64, bool, bool, bool)> {
    LAZY_ITER_STATES.with(|states| {
        let states = states.borrow();
        let LazyIterState::Range {
            current,
            end,
            inclusive,
            start_open,
            end_open,
            ..
        } = states.get(&id)?
        else {
            return None;
        };
        Some((*current, *end, *inclusive, *start_open, *end_open))
    })
}

fn discard_lazy_value(value: &Value) {
    let Value::LazyIter(id) = value else {
        return;
    };
    let state = LAZY_ITER_STATES.with(|states| states.borrow_mut().remove(id));
    if let Some(state) = state {
        discard_lazy_state(state);
    }
}

fn discard_lazy_state(state: LazyIterState) {
    match state {
        LazyIterState::Take { upstream, .. }
        | LazyIterState::Skip { upstream, .. }
        | LazyIterState::Enumerate { upstream, .. }
        | LazyIterState::Map { upstream, .. }
        | LazyIterState::Filter { upstream, .. }
        | LazyIterState::FilterMap { upstream, .. }
        | LazyIterState::Scan { upstream, .. }
        | LazyIterState::TakeWhile { upstream, .. }
        | LazyIterState::SkipWhile { upstream, .. } => discard_lazy_value(&upstream),
        LazyIterState::Chain { first, second, .. } => {
            discard_lazy_value(&first);
            discard_lazy_value(&second);
        }
        LazyIterState::Zip { left, right } => {
            discard_lazy_value(&left);
            discard_lazy_value(&right);
        }
        LazyIterState::FlatMap {
            upstream, current, ..
        } => {
            discard_lazy_value(&upstream);
            if let Some(current) = current {
                discard_lazy_value(&current);
            }
        }
        LazyIterState::Array { source_id, .. }
        | LazyIterState::IntArray { source_id, .. }
        | LazyIterState::FloatVec { source_id, .. } => release_lazy_vec_source(source_id),
        LazyIterState::Range { .. } | LazyIterState::Once { .. } | LazyIterState::Repeat { .. } => {
        }
    }
}

struct LazyStateGuard(Option<LazyIterState>);

impl Drop for LazyStateGuard {
    fn drop(&mut self) {
        if let Some(state) = self.0.take() {
            discard_lazy_state(state);
        }
    }
}

fn lazy_source(value: &Value) -> Value {
    match value {
        Value::LazyIter(_) => value.clone(),
        Value::Array(items) => {
            let source_id = Arc::as_ptr(items) as usize;
            retain_lazy_vec_source(source_id);
            new_lazy_iter(LazyIterState::Array {
                items: Arc::clone(items),
                source_id,
                generation: lazy_vec_generation(source_id),
                index: 0,
            })
        }
        Value::IntArray(items) => {
            let source_id = Arc::as_ptr(items) as usize;
            retain_lazy_vec_source(source_id);
            new_lazy_iter(LazyIterState::IntArray {
                items: Arc::clone(items),
                source_id,
                generation: lazy_vec_generation(source_id),
                index: 0,
            })
        }
        Value::FloatVec(items) => {
            let source_id = Arc::as_ptr(items) as usize;
            retain_lazy_vec_source(source_id);
            new_lazy_iter(LazyIterState::FloatVec {
                items: Arc::clone(items),
                source_id,
                generation: lazy_vec_generation(source_id),
                index: 0,
            })
        }
        other => new_lazy_iter(LazyIterState::Array {
            items: Arc::new(collect_array(other)),
            source_id: 0,
            generation: 0,
            index: 0,
        }),
    }
}

fn retain_lazy_vec_source(source_id: usize) {
    if source_id == 0 {
        return;
    }
    LAZY_VEC_BORROW_COUNT.with(|count| count.set(count.get().saturating_add(1)));
    LAZY_VEC_BORROWERS.with(|borrowers| {
        let mut borrowers = borrowers.borrow_mut();
        let count = borrowers.entry(source_id).or_insert(0);
        *count = count.saturating_add(1);
    });
}

fn release_lazy_vec_source(source_id: usize) {
    if source_id == 0 {
        return;
    }
    let released_last = LAZY_VEC_BORROWERS.with(|borrowers| {
        let mut borrowers = borrowers.borrow_mut();
        let Some(count) = borrowers.get_mut(&source_id) else {
            return false;
        };
        *count -= 1;
        if *count == 0 {
            borrowers.remove(&source_id);
            true
        } else {
            false
        }
    });
    LAZY_VEC_BORROW_COUNT.with(|count| count.set(count.get().saturating_sub(1)));
    if released_last {
        LAZY_VEC_GENERATIONS.with(|generations| {
            generations.borrow_mut().remove(&source_id);
        });
        LAZY_VEC_REPLACEMENTS.with(|replacements| {
            replacements.borrow_mut().remove(&source_id);
        });
    }
}

fn has_lazy_vec_borrowers() -> bool {
    LAZY_VEC_BORROW_COUNT.with(|count| count.get() != 0)
}

fn lazy_vec_source_is_borrowed(source_id: usize) -> bool {
    LAZY_VEC_BORROWERS.with(|borrowers| borrowers.borrow().contains_key(&source_id))
}

fn lazy_vec_generation(source_id: usize) -> u64 {
    LAZY_VEC_GENERATIONS
        .with(|generations| generations.borrow().get(&source_id).copied().unwrap_or(0))
}

/// Record a structural mutation before the VM applies copy-on-write to a Vec.
/// Lazy sources retain the original Arc identity, so recording first lets the
/// next pull reject the mutation even when the binding receives a new Arc.
pub(crate) fn note_vec_structural_mutation(value: &Value) {
    if !has_lazy_vec_borrowers() {
        return;
    }
    let source_id = match value {
        Value::Array(items) => Arc::as_ptr(items) as usize,
        Value::IntArray(items) => Arc::as_ptr(items) as usize,
        Value::FloatVec(items) => Arc::as_ptr(items) as usize,
        _ => return,
    };
    if !lazy_vec_source_is_borrowed(source_id) {
        return;
    }
    LAZY_VEC_GENERATIONS.with(|generations| {
        let mut generations = generations.borrow_mut();
        let generation = generations.entry(source_id).or_insert(0);
        *generation = generation.wrapping_add(1);
    });
}

/// Publish a non-structural element replacement to outstanding borrowed
/// sources before the VM's copy-on-write store changes the binding's Arc.
pub(crate) fn note_vec_element_replacement(value: &Value, index: i64, replacement: &Value) {
    if index < 0 || !has_lazy_vec_borrowers() {
        return;
    }
    let index = index as usize;
    let source_id = match value {
        Value::Array(items) if index < items.len() => Arc::as_ptr(items) as usize,
        Value::IntArray(items) if index < items.len() => Arc::as_ptr(items) as usize,
        Value::FloatVec(items) if index < items.len() => Arc::as_ptr(items) as usize,
        _ => return,
    };
    if !lazy_vec_source_is_borrowed(source_id) {
        return;
    }
    LAZY_VEC_REPLACEMENTS.with(|replacements| {
        replacements
            .borrow_mut()
            .entry(source_id)
            .or_default()
            .insert(index, replacement.clone());
    });
}

fn lazy_vec_replacement(source_id: usize, index: usize) -> Option<Value> {
    LAZY_VEC_REPLACEMENTS.with(|replacements| {
        replacements
            .borrow()
            .get(&source_id)
            .and_then(|source| source.get(&index))
            .cloned()
    })
}

fn lazy_next(
    source: &Value,
    dispatch: &mut Option<&mut dyn NativeDispatch>,
) -> RuntimeResult<Option<Value>> {
    let Value::LazyIter(id) = source else {
        let mut xs = collect_array(source).into_iter();
        return Ok(xs.next());
    };
    let Some(state) = LAZY_ITER_STATES.with(|states| states.borrow_mut().remove(id)) else {
        return Ok(None);
    };
    let mut guard = LazyStateGuard(Some(state));
    let result = match guard.0.as_mut().expect("lazy iterator state guard") {
        LazyIterState::Array {
            items,
            source_id,
            generation,
            index,
        } => {
            if *source_id != 0 && lazy_vec_generation(*source_id) != *generation {
                return Err(crate::value::RuntimeError::Panic(
                    "borrowed Vec source was structurally mutated during iteration".to_string(),
                ));
            }
            let out =
                lazy_vec_replacement(*source_id, *index).or_else(|| items.get(*index).cloned());
            *index = index.saturating_add(usize::from(out.is_some()));
            Ok(out)
        }
        LazyIterState::IntArray {
            items,
            source_id,
            generation,
            index,
        } => {
            if lazy_vec_generation(*source_id) != *generation {
                return Err(crate::value::RuntimeError::Panic(
                    "borrowed Vec source was structurally mutated during iteration".to_string(),
                ));
            }
            let out = lazy_vec_replacement(*source_id, *index)
                .or_else(|| items.get(*index).copied().map(Value::Int));
            *index = index.saturating_add(usize::from(out.is_some()));
            Ok(out)
        }
        LazyIterState::FloatVec {
            items,
            source_id,
            generation,
            index,
        } => {
            if lazy_vec_generation(*source_id) != *generation {
                return Err(crate::value::RuntimeError::Panic(
                    "borrowed Vec source was structurally mutated during iteration".to_string(),
                ));
            }
            let out = lazy_vec_replacement(*source_id, *index)
                .or_else(|| items.get(*index).copied().map(Value::Float));
            *index = index.saturating_add(usize::from(out.is_some()));
            Ok(out)
        }
        LazyIterState::Range {
            current,
            end,
            inclusive,
            end_open,
            finished,
            ..
        } => {
            if *end_open {
                if cfg!(debug_assertions) && *current == i64::MAX {
                    Err(crate::value::RuntimeError::Panic(
                        "attempt to add with overflow in open integer range".to_string(),
                    ))
                } else {
                    let out = *current;
                    *current = current.wrapping_add(1);
                    Ok(Some(Value::Int(out)))
                }
            } else {
                let done = *finished
                    || if *inclusive {
                        *current > *end
                    } else {
                        *current >= *end
                    };
                if done {
                    Ok(None)
                } else {
                    let out = *current;
                    if *inclusive && out == *end {
                        *finished = true;
                    } else {
                        *current = current.saturating_add(1);
                    }
                    Ok(Some(Value::Int(out)))
                }
            }
        }
        LazyIterState::Once { item } => Ok(item.take()),
        LazyIterState::Repeat { item, remaining } => {
            if *remaining == 0 {
                Ok(None)
            } else {
                *remaining -= 1;
                Ok(Some(item.clone()))
            }
        }
        LazyIterState::Take {
            upstream,
            remaining,
        } => {
            if *remaining == 0 {
                Ok(None)
            } else {
                *remaining -= 1;
                lazy_next(upstream, dispatch)
            }
        }
        LazyIterState::Skip {
            upstream,
            remaining,
        } => {
            let mut exhausted = false;
            while *remaining > 0 {
                if lazy_next(upstream, dispatch)?.is_none() {
                    *remaining = 0;
                    exhausted = true;
                    break;
                }
                *remaining -= 1;
            }
            if exhausted {
                Ok(None)
            } else {
                lazy_next(upstream, dispatch)
            }
        }
        LazyIterState::Enumerate { upstream, index } => {
            if let Some(value) = lazy_next(upstream, dispatch)? {
                let pair = Value::Tuple(Arc::from(vec![Value::Int(*index), value]));
                *index = index.saturating_add(1);
                Ok(Some(pair))
            } else {
                Ok(None)
            }
        }
        LazyIterState::Chain {
            first,
            second,
            in_second,
        } => {
            if !*in_second && let Some(value) = lazy_next(first, dispatch)? {
                Ok(Some(value))
            } else {
                *in_second = true;
                lazy_next(second, dispatch)
            }
        }
        LazyIterState::Zip { left, right } => {
            if let Some(a) = lazy_next(left, dispatch)? {
                if let Some(b) = lazy_next(right, dispatch)? {
                    Ok(Some(Value::Tuple(Arc::from(vec![a, b]))))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        LazyIterState::Map { f, upstream } => {
            if let Some(value) = lazy_next(upstream, dispatch)? {
                if let Some(d) = dispatch.as_deref_mut() {
                    d.call_value(f, vec![value]).map(Some)
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        LazyIterState::Filter { p, upstream } => loop {
            let Some(value) = lazy_next(upstream, dispatch)? else {
                break Ok(None);
            };
            let Some(d) = dispatch.as_deref_mut() else {
                break Ok(None);
            };
            if matches!(d.call_value(p, vec![value.clone()])?, Value::Bool(true)) {
                break Ok(Some(value));
            }
        },
        LazyIterState::FilterMap { f, upstream } => loop {
            let Some(value) = lazy_next(upstream, dispatch)? else {
                break Ok(None);
            };
            let Some(d) = dispatch.as_deref_mut() else {
                break Ok(None);
            };
            if let Some(mapped) = some_payload(&d.call_value(f, vec![value])?) {
                break Ok(Some(mapped));
            }
        },
        LazyIterState::FlatMap {
            f,
            upstream,
            current,
        } => loop {
            if let Some(active) = current.as_ref()
                && let Some(value) = lazy_next(active, dispatch)?
            {
                break Ok(Some(value));
            }
            let Some(value) = lazy_next(upstream, dispatch)? else {
                break Ok(None);
            };
            let Some(d) = dispatch.as_deref_mut() else {
                break Ok(None);
            };
            *current = Some(lazy_source(&d.call_value(f, vec![value])?));
        },
        LazyIterState::Scan { acc, f, upstream } => {
            if let Some(value) = lazy_next(upstream, dispatch)? {
                if let Some(d) = dispatch.as_deref_mut() {
                    *acc = d.call_value(f, vec![acc.clone(), value])?;
                    Ok(Some(acc.clone()))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        LazyIterState::TakeWhile { p, upstream, done } => {
            if *done {
                Ok(None)
            } else if let Some(value) = lazy_next(upstream, dispatch)? {
                if let Some(d) = dispatch.as_deref_mut() {
                    if matches!(d.call_value(p, vec![value.clone()])?, Value::Bool(true)) {
                        Ok(Some(value))
                    } else {
                        *done = true;
                        Ok(None)
                    }
                } else {
                    Ok(None)
                }
            } else {
                *done = true;
                Ok(None)
            }
        }
        LazyIterState::SkipWhile {
            p,
            upstream,
            skipping,
        } => loop {
            let Some(value) = lazy_next(upstream, dispatch)? else {
                break Ok(None);
            };
            if !*skipping {
                break Ok(Some(value));
            }
            let Some(d) = dispatch.as_deref_mut() else {
                break Ok(None);
            };
            if !matches!(d.call_value(p, vec![value.clone()])?, Value::Bool(true)) {
                *skipping = false;
                break Ok(Some(value));
            }
        },
    };
    if matches!(result, Ok(Some(_))) {
        let state = guard.0.take().expect("live lazy iterator state");
        LAZY_ITER_STATES.with(|states| {
            states.borrow_mut().insert(*id, state);
        });
    }
    result
}

pub(crate) fn drain_lazy_iter(value: &Value) -> Option<Vec<Value>> {
    drain_lazy_iter_result(value).ok().flatten()
}

fn drain_lazy_iter_result(value: &Value) -> RuntimeResult<Option<Vec<Value>>> {
    if !matches!(value, Value::LazyIter(_)) {
        return Ok(None);
    }
    let mut out = Vec::new();
    let mut dispatch = None;
    while let Some(value) = lazy_next(value, &mut dispatch)? {
        out.push(value);
    }
    Ok(Some(out))
}

fn drain_iter_with_dispatch(
    value: &Value,
    dispatch: &mut dyn NativeDispatch,
) -> RuntimeResult<Vec<Value>> {
    let mut out = Vec::new();
    let mut dispatch = Some(dispatch);
    while let Some(value) = lazy_next(value, &mut dispatch)? {
        out.push(value);
    }
    Ok(out)
}

pub(crate) fn install_iter(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified `iter::*` names to avoid shadowing built-in
    // method dispatch (Option::map, Result::filter, Vec::any, etc.).
    //
    // Argument order is DATA-LAST throughout, matching SPEC §4.6 so
    // `xs |> iter::map(f)` desugars to `iter::map(f, xs)` and threads.
    let static_entries: &[(&str, BuiltinFnPub)] = &[
        ("empty", builtin_iter_empty),
        ("once", builtin_iter_once),
        ("take", builtin_iter_take),
        ("skip", builtin_iter_skip),
        ("step_by", builtin_iter_step_by),
        ("zip", builtin_iter_zip),
        ("enumerate", builtin_iter_enumerate),
        ("chain", builtin_iter_chain),
        ("flatten", builtin_iter_flatten),
        ("rev", builtin_iter_reversed),
        ("dedup", builtin_iter_dedup),
        ("range", builtin_iter_range),
        ("range_inclusive", builtin_iter_range_inclusive),
        ("repeat", builtin_iter_repeat),
        ("unzip", builtin_iter_unzip),
        ("windows", builtin_iter_windows),
        ("pairwise", builtin_iter_pairwise),
        ("chunks", builtin_iter_chunks),
    ];
    for (short, call) in static_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    // Explicit compatibility spellings for code migrated before the lazy
    // edition changes these names to return iterator state. Keep these aliases
    // on the same entry points as their eager counterparts.
    let eager_static_aliases: &[(&str, BuiltinFnPub)] = &[
        ("eager_chain", builtin_iter_eager_chain),
        ("eager_collect", builtin_iter_collect),
        ("eager_count", builtin_iter_count),
        ("eager_enumerate", builtin_iter_eager_enumerate),
        ("eager_range", builtin_iter_eager_range),
        ("eager_range_inclusive", builtin_iter_eager_range_inclusive),
        ("eager_skip", builtin_iter_eager_skip),
        ("eager_sum", builtin_iter_sum),
        ("eager_take", builtin_iter_eager_take),
        ("eager_zip", builtin_iter_eager_zip),
    ];
    for (short, call) in eager_static_aliases {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    // Closure-taking functions - must be `native` to access the interpreter.
    let native_entries: &[(&str, NativeCall)] = &[
        ("collect", native_iter_collect),
        ("count", native_iter_count),
        ("sum", native_iter_sum),
        ("product", native_iter_product),
        ("min", native_iter_min),
        ("max", native_iter_max),
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
        ("chunk_by", native_iter_chunk_by),
        ("count_by", native_iter_count_by),
    ];
    for (short, call) in native_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }

    let eager_native_aliases: &[(&str, NativeCall)] = &[
        ("eager_all", native_iter_all),
        ("eager_any", native_iter_any),
        ("eager_filter", native_iter_eager_filter),
        ("eager_find", native_iter_find),
        ("eager_fold", native_iter_fold),
        ("eager_map", native_iter_eager_map),
    ];
    for (short, call) in eager_native_aliases {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }

    // Receiver-first method forms on a `Vec` receiver (`xs.take(n)`,
    // `xs.step_by(s)`), registered under the `Vec::` key ONLY:
    // a bare-name registration would shadow the scalar prelude
    // (`min(3, 7)` / `max(a, b)`) with the sequence reducers.
    let vec_builtin_entries: &[(&str, BuiltinFnPub)] = &[
        ("take", builtin_vec_take_method),
        ("skip", builtin_vec_skip_method),
        ("enumerate", builtin_vec_enumerate_method),
        ("chain", builtin_vec_chain_method),
        ("zip", builtin_vec_zip_method),
        ("flatten", builtin_vec_flatten_method),
        ("rev", builtin_vec_rev_method),
        ("dedup", builtin_vec_dedup_method),
        ("windows", builtin_vec_windows_method),
        ("pairwise", builtin_vec_pairwise_method),
        ("chunks", builtin_vec_chunks_method),
        ("step_by", builtin_vec_step_by_method),
        ("collect", builtin_iter_collect),
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

    // Lazy iterator method calls use receiver-first syntax, while the
    // `iter::*` free functions are data-last. Register every supported
    // method form here so a lazy range cannot fall through to a missing
    // bare-name lookup or an eager Vec-only implementation.
    for (short, call) in [
        ("map", native_vec_map_method as NativeCall),
        ("filter", native_vec_filter_method as NativeCall),
        ("fold", native_vec_fold_method as NativeCall),
        ("for_each", native_vec_for_each_method as NativeCall),
        ("any", native_vec_any_method as NativeCall),
        ("all", native_vec_all_method as NativeCall),
        ("find", native_vec_find_method as NativeCall),
        ("position", native_vec_position_method as NativeCall),
        ("max_by_key", native_vec_max_by_key_method as NativeCall),
        ("min_by_key", native_vec_min_by_key_method as NativeCall),
        ("collect", native_vec_collect_method as NativeCall),
        ("count", native_vec_count_method as NativeCall),
        ("sum", native_vec_sum_method as NativeCall),
        ("product", native_vec_product_method as NativeCall),
        ("min", native_vec_min_method as NativeCall),
        ("max", native_vec_max_method as NativeCall),
    ] {
        let qualified: &'static str = Box::leak(format!("Iterator::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, call)));
    }
    for (short, call) in [
        ("take", builtin_iterator_take_method as BuiltinFnPub),
        ("skip", builtin_iterator_skip_method as BuiltinFnPub),
        (
            "enumerate",
            builtin_iterator_enumerate_method as BuiltinFnPub,
        ),
        ("chain", builtin_iterator_chain_method as BuiltinFnPub),
        ("zip", builtin_iterator_zip_method as BuiltinFnPub),
        ("flatten", builtin_iterator_flatten_method as BuiltinFnPub),
        ("rev", builtin_iterator_rev_method as BuiltinFnPub),
        ("dedup", builtin_iterator_dedup_method as BuiltinFnPub),
        ("windows", builtin_iterator_windows_method as BuiltinFnPub),
        ("pairwise", builtin_iterator_pairwise_method as BuiltinFnPub),
        ("chunks", builtin_iterator_chunks_method as BuiltinFnPub),
    ] {
        let qualified: &'static str = Box::leak(format!("Iterator::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, call)));
    }
}

/// Rotates a method call's `(receiver, rest…)` argument list into the
/// data-last `(rest…, receiver)` shape the iter natives consume.
fn rotate_receiver_last(args: &[Value]) -> Vec<Value> {
    let mut v: Vec<Value> = args.get(1..).unwrap_or(&[]).to_vec();
    v.push(args.first().cloned().unwrap_or(Value::Unit));
    v
}

fn non_negative_count(args: &[Value], index: usize, name: &str) -> RuntimeResult<usize> {
    let Some(raw) = args.get(index).and_then(value_to_int) else {
        return Err(RuntimeError::Type(format!("{name}: count must be i64")));
    };
    if raw < 0 {
        return Err(RuntimeError::Type(format!(
            "{name}: count must be non-negative"
        )));
    }
    usize::try_from(raw).map_err(|_| RuntimeError::Arithmetic(format!("{name}: count too large")))
}

fn positive_count(args: &[Value], index: usize, name: &str) -> RuntimeResult<usize> {
    let raw = non_negative_count(args, index, name)?;
    if raw == 0 {
        return Err(RuntimeError::Type(format!(
            "{name}: count must be positive"
        )));
    }
    Ok(raw)
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
vec_method_form!(native_vec_collect_method, native_iter_collect);
vec_method_form!(native_vec_sum_method, native_iter_sum);
vec_method_form!(native_vec_product_method, native_iter_product);
vec_method_form!(native_vec_min_method, native_iter_min);
vec_method_form!(native_vec_max_method, native_iter_max);

/// `xs.count()` is the element count; `xs.count(f)` counts the
/// elements the predicate accepts.
pub(crate) fn native_vec_count_method(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if args.len() <= 1 {
        return native_iter_count(dispatch, args);
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
    let n = non_negative_count(args, 1, "Vec::take")?;
    Ok(Value::Array(Arc::new(iter_std::take(n, &xs))))
}

/// `xs.skip(n)` - method form of `iter::skip` (receiver-first).
pub(crate) fn builtin_vec_skip_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_skip(&rotate_receiver_last(args))
}

/// `xs.enumerate()` - method form of `iter::enumerate`.
pub(crate) fn builtin_vec_enumerate_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_enumerate(args)
}

/// `xs.chain(other)` - method form of `iter::chain`.
pub(crate) fn builtin_vec_chain_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_chain(args)
}

/// `xs.zip(other)` - method form of `iter::zip`.
pub(crate) fn builtin_vec_zip_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_zip(args)
}

/// `xs.flatten()` - method form of `iter::flatten`.
pub(crate) fn builtin_vec_flatten_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_flatten(args)
}

/// `xs.rev()` - method form of `iter::rev`.
pub(crate) fn builtin_vec_rev_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_reversed(args)
}

/// `xs.dedup()` - method form of `iter::dedup`.
pub(crate) fn builtin_vec_dedup_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_dedup(args)
}

/// `xs.windows(n)` - method form of `iter::windows`.
pub(crate) fn builtin_vec_windows_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_windows(&rotate_receiver_last(args))
}

/// `xs.pairwise()` - method form of `iter::pairwise`.
pub(crate) fn builtin_vec_pairwise_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_pairwise(args)
}

/// `xs.chunks(n)` - method form of `iter::chunks`.
pub(crate) fn builtin_vec_chunks_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_chunks(&rotate_receiver_last(args))
}

/// `iter.take(n)` with the receiver rotated into the data-last free form.
pub(crate) fn builtin_iterator_take_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_take(&rotate_receiver_last(args))
}

/// `iter.skip(n)` with the receiver rotated into the data-last free form.
pub(crate) fn builtin_iterator_skip_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_skip(&rotate_receiver_last(args))
}

/// `iter.enumerate()` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_enumerate_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_enumerate(args)
}

/// `iter.chain(other)` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_chain_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_chain(args)
}

/// `iter.zip(other)` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_zip_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_zip(args)
}

/// `iter.flatten()` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_flatten_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_flatten(args)
}

/// `iter.rev()` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_rev_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_reversed(args)
}

/// `iter.dedup()` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_dedup_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_dedup(args)
}

/// `iter.windows(n)` with the receiver rotated into the data-last free form.
pub(crate) fn builtin_iterator_windows_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_windows(&rotate_receiver_last(args))
}

/// `iter.pairwise()` with the receiver in the data-first free form.
pub(crate) fn builtin_iterator_pairwise_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_pairwise(args)
}

/// `iter.chunks(n)` with the receiver rotated into the data-last free form.
pub(crate) fn builtin_iterator_chunks_method(args: &[Value]) -> RuntimeResult<Value> {
    builtin_iter_chunks(&rotate_receiver_last(args))
}

/// `xs.step_by(step)` - every `step`-th element starting at index 0;
/// a step below 1 is treated as 1 (total, tier-identical).
pub(crate) fn builtin_vec_step_by_method(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let step = positive_count(args, 1, "Vec::step_by")?;
    let out: Vec<Value> = xs.iter().step_by(step).cloned().collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_collect(args: &[Value]) -> RuntimeResult<Value> {
    Ok(match args.first().unwrap_or(&Value::Unit) {
        Value::LazyIter(_) => Value::Array(Arc::new(
            drain_lazy_iter_result(args.first().unwrap_or(&Value::Unit))?.unwrap_or_default(),
        )),
        Value::Array(arr) => Value::Array(arr.clone()),
        Value::IntArray(arr) => {
            let out = arr.iter().copied().map(Value::Int).collect();
            Value::Array(Arc::new(out))
        }
        Value::FloatVec(arr) => {
            let out = arr.iter().copied().map(Value::Float).collect();
            Value::Array(Arc::new(out))
        }
        other => Value::Array(Arc::new(collect_array(other))),
    })
}

pub(crate) fn builtin_iter_once(args: &[Value]) -> RuntimeResult<Value> {
    if lazy_iterators_enabled() {
        return Ok(new_lazy_iter(LazyIterState::Once {
            item: Some(args.first().cloned().unwrap_or(Value::Unit)),
        }));
    }
    Ok(Value::Array(Arc::new(vec![
        args.first().cloned().unwrap_or(Value::Unit),
    ])))
}

pub(crate) fn builtin_iter_empty(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Array(Arc::new(Vec::new())))
}

pub(crate) fn builtin_iter_step_by(args: &[Value]) -> RuntimeResult<Value> {
    let step = positive_count(args, 0, "iter::step_by")?;
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let out: Vec<Value> = xs.iter().step_by(step).cloned().collect();
    Ok(Value::Array(Arc::new(out)))
}

pub(crate) fn builtin_iter_count(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(xs) = drain_lazy_iter_result(args.first().unwrap_or(&Value::Unit))? {
        return Ok(Value::Int(xs.len() as i64));
    }
    let n = match args.first() {
        Some(Value::Array(arr)) => arr.len(),
        Some(Value::IntArray(arr)) => arr.len(),
        Some(Value::FloatVec(arr)) => arr.len(),
        _ => 0,
    };
    Ok(Value::Int(n as i64))
}

pub(crate) fn builtin_iter_take(args: &[Value]) -> RuntimeResult<Value> {
    let n = non_negative_count(args, 0, "iter::take")?;
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Take {
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            remaining: n,
        }));
    }
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let taken = iter_std::take(n, &xs);
    Ok(Value::Array(Arc::new(taken)))
}

fn builtin_iter_eager_take(args: &[Value]) -> RuntimeResult<Value> {
    let n = non_negative_count(args, 0, "iter::eager_take")?;
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Array(Arc::new(iter_std::take(n, &xs))))
}

pub(crate) fn builtin_iter_skip(args: &[Value]) -> RuntimeResult<Value> {
    let n = non_negative_count(args, 0, "iter::skip")?;
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Skip {
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            remaining: n,
        }));
    }
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let rest = iter_std::skip(n, &xs);
    Ok(Value::Array(Arc::new(rest)))
}

fn builtin_iter_eager_skip(args: &[Value]) -> RuntimeResult<Value> {
    let n = non_negative_count(args, 0, "iter::eager_skip")?;
    let xs = collect_array(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Array(Arc::new(iter_std::skip(n, &xs))))
}

pub(crate) fn builtin_iter_zip(args: &[Value]) -> RuntimeResult<Value> {
    if lazy_iterators_enabled()
        || matches!(args.first(), Some(Value::LazyIter(_)))
        || matches!(args.get(1), Some(Value::LazyIter(_)))
    {
        return Ok(new_lazy_iter(LazyIterState::Zip {
            left: lazy_source(args.first().unwrap_or(&Value::Unit)),
            right: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
        }));
    }
    let a = collect_array(args.first().unwrap_or(&Value::Unit));
    let b = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let zipped: Vec<Value> = a
        .into_iter()
        .zip(b)
        .map(|(x, y)| Value::Tuple(Arc::from(vec![x, y])))
        .collect();
    Ok(Value::Array(Arc::new(zipped)))
}

fn builtin_iter_eager_zip(args: &[Value]) -> RuntimeResult<Value> {
    let a = collect_array(args.first().unwrap_or(&Value::Unit));
    let b = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let zipped = a
        .into_iter()
        .zip(b)
        .map(|(x, y)| Value::Tuple(Arc::from(vec![x, y])))
        .collect();
    Ok(Value::Array(Arc::new(zipped)))
}

pub(crate) fn builtin_iter_enumerate(args: &[Value]) -> RuntimeResult<Value> {
    if lazy_iterators_enabled() || matches!(args.first(), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Enumerate {
            upstream: lazy_source(args.first().unwrap_or(&Value::Unit)),
            index: 0,
        }));
    }
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let enumerated: Vec<Value> = xs
        .into_iter()
        .enumerate()
        .map(|(i, x)| Value::Tuple(Arc::from(vec![Value::Int(i as i64), x])))
        .collect();
    Ok(Value::Array(Arc::new(enumerated)))
}

fn builtin_iter_eager_enumerate(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let enumerated = xs
        .into_iter()
        .enumerate()
        .map(|(i, x)| Value::Tuple(Arc::from(vec![Value::Int(i as i64), x])))
        .collect();
    Ok(Value::Array(Arc::new(enumerated)))
}

pub(crate) fn builtin_iter_chain(args: &[Value]) -> RuntimeResult<Value> {
    if lazy_iterators_enabled()
        || matches!(args.first(), Some(Value::LazyIter(_)))
        || matches!(args.get(1), Some(Value::LazyIter(_)))
    {
        return Ok(new_lazy_iter(LazyIterState::Chain {
            first: lazy_source(args.first().unwrap_or(&Value::Unit)),
            second: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            in_second: false,
        }));
    }
    let mut result = collect_array(args.first().unwrap_or(&Value::Unit));
    result.extend(collect_array(args.get(1).unwrap_or(&Value::Unit)));
    Ok(Value::Array(Arc::new(result)))
}

fn builtin_iter_eager_chain(args: &[Value]) -> RuntimeResult<Value> {
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
    if lazy_iterators_enabled() {
        return Ok(new_lazy_iter(LazyIterState::Array {
            items: Arc::new(xs),
            source_id: 0,
            generation: 0,
            index: 0,
        }));
    }
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

pub(crate) fn native_iter_collect(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        return Ok(Value::Array(Arc::new(drain_iter_with_dispatch(
            args.first().unwrap_or(&Value::Unit),
            dispatch,
        )?)));
    }
    builtin_iter_collect(args)
}

pub(crate) fn native_iter_count(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        let n = drain_iter_with_dispatch(args.first().unwrap_or(&Value::Unit), dispatch)?.len();
        return Ok(Value::Int(n as i64));
    }
    builtin_iter_count(args)
}

pub(crate) fn native_iter_sum(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        let xs = drain_iter_with_dispatch(args.first().unwrap_or(&Value::Unit), dispatch)?;
        return builtin_iter_sum(&[Value::Array(Arc::new(xs))]);
    }
    builtin_iter_sum(args)
}

pub(crate) fn native_iter_product(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        let xs = drain_iter_with_dispatch(args.first().unwrap_or(&Value::Unit), dispatch)?;
        return builtin_iter_product(&[Value::Array(Arc::new(xs))]);
    }
    builtin_iter_product(args)
}

pub(crate) fn native_iter_min(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        let xs = drain_iter_with_dispatch(args.first().unwrap_or(&Value::Unit), dispatch)?;
        return builtin_iter_min(&[Value::Array(Arc::new(xs))]);
    }
    builtin_iter_min(args)
}

pub(crate) fn native_iter_max(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    if let Some(Value::LazyIter(_)) = args.first() {
        let xs = drain_iter_with_dispatch(args.first().unwrap_or(&Value::Unit), dispatch)?;
        return builtin_iter_max(&[Value::Array(Arc::new(xs))]);
    }
    builtin_iter_max(args)
}

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
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Map {
            f,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
        }));
    }
    native_iter_eager_map(dispatch, args)
}

fn native_iter_eager_map(
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
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Filter {
            p,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
        }));
    }
    native_iter_eager_filter(dispatch, args)
}

fn native_iter_eager_filter(
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
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::FilterMap {
            f,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
        }));
    }
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
    let source = args.get(2).unwrap_or(&Value::Unit);
    let values = if matches!(source, Value::LazyIter(_)) {
        drain_iter_with_dispatch(source, dispatch)?
    } else {
        collect_array(source)
    };
    for value in values {
        acc = dispatch.call_value(&f, vec![acc, value])?;
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
    if lazy_iterators_enabled() || matches!(args.get(2), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::Scan {
            acc,
            f,
            upstream: lazy_source(args.get(2).unwrap_or(&Value::Unit)),
        }));
    }
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
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::TakeWhile {
            p,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            done: false,
        }));
    }
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
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::SkipWhile {
            p,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            skipping: true,
        }));
    }
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

pub(crate) fn native_iter_chunk_by(
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
    let mut map = dense_map_with_capacity(groups.len());
    for (k, v) in groups {
        map.insert(k, Value::Array(Arc::new(v)));
    }
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
        let mut typed = dense_map_with_capacity(counts.len());
        for (k, v) in counts {
            if let MapKey::Int(n) = k {
                typed.insert(n, v);
            }
        }
        return Ok(Value::IntMap(Arc::new(parking_lot::Mutex::new(typed))));
    }
    let mut map = dense_map();
    for (k, v) in counts {
        map.insert(k, Value::Int(v));
    }
    Ok(Value::Map(Arc::new(parking_lot::Mutex::new(map))))
}

pub(crate) fn native_iter_flat_map(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let f = args.first().cloned().unwrap_or(Value::Unit);
    if lazy_iterators_enabled() || matches!(args.get(1), Some(Value::LazyIter(_))) {
        return Ok(new_lazy_iter(LazyIterState::FlatMap {
            f,
            upstream: lazy_source(args.get(1).unwrap_or(&Value::Unit)),
            current: None,
        }));
    }
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
    if lazy_iterators_enabled() {
        return Ok(new_range_iter(start, end, false, false, false));
    }
    Ok(Value::IntArray(Arc::new(iter_std::range(start, end))))
}

fn builtin_iter_eager_range(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let end = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::IntArray(Arc::new(iter_std::range(start, end))))
}

pub(crate) fn builtin_iter_range_inclusive(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let end = args.get(1).and_then(value_to_int).unwrap_or(0);
    if lazy_iterators_enabled() {
        return Ok(new_range_iter(start, end, true, false, false));
    }
    Ok(Value::IntArray(Arc::new(iter_std::range_inclusive(
        start, end,
    ))))
}

fn builtin_iter_eager_range_inclusive(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let end = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::IntArray(Arc::new(iter_std::range_inclusive(
        start, end,
    ))))
}

pub(crate) fn builtin_iter_repeat(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().cloned().unwrap_or(Value::Unit);
    let n = non_negative_count(args, 1, "iter::repeat")?;
    if lazy_iterators_enabled() {
        return Ok(new_lazy_iter(LazyIterState::Repeat {
            item: v,
            remaining: n,
        }));
    }
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

pub(crate) fn builtin_iter_windows(args: &[Value]) -> RuntimeResult<Value> {
    let n = positive_count(args, 0, "iter::windows")?;
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

pub(crate) fn builtin_iter_chunks(args: &[Value]) -> RuntimeResult<Value> {
    let n = positive_count(args, 0, "iter::chunks")?;
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
    crate::vm::value_ordering(a, b).unwrap_or(Ordering::Equal)
}

#[cfg(test)]
mod lazy_cleanup_tests {
    use super::*;

    #[test]
    fn dropping_unconsumed_vec_iterator_releases_its_source_once() {
        let items = Arc::new(vec![1i64, 2, 3]);
        let weak = Arc::downgrade(&items);
        let source = Value::IntArray(items);
        let iter = lazy_source(&source);

        drop(source);
        assert!(weak.upgrade().is_some());
        discard_lazy_value(&iter);
        drop(iter);
        assert!(weak.upgrade().is_none());
    }

    #[test]
    fn ordinary_vec_mutation_does_not_allocate_lazy_tracking_state() {
        set_lazy_iterators_enabled(true);
        let source = Value::IntArray(Arc::new(vec![1i64, 2, 3]));

        note_vec_element_replacement(&source, 1, &Value::Int(9));
        note_vec_structural_mutation(&source);

        assert!(!has_lazy_vec_borrowers());
        assert!(LAZY_VEC_BORROWERS.with(|borrowers| borrowers.borrow().is_empty()));
        assert!(LAZY_VEC_GENERATIONS.with(|generations| generations.borrow().is_empty()));
        assert!(LAZY_VEC_REPLACEMENTS.with(|replacements| replacements.borrow().is_empty()));
        set_lazy_iterators_enabled(false);
    }

    #[test]
    fn last_vec_iterator_reclaims_mutation_tracking_state() {
        set_lazy_iterators_enabled(true);
        let source = Value::IntArray(Arc::new(vec![1i64, 2, 3]));
        let iter = lazy_source(&source);

        note_vec_element_replacement(&source, 1, &Value::Int(9));
        assert!(matches!(
            lazy_vec_replacement(source_id(&source), 1),
            Some(Value::Int(9))
        ));

        discard_lazy_value(&iter);
        assert!(!has_lazy_vec_borrowers());
        assert!(LAZY_VEC_REPLACEMENTS.with(|replacements| replacements.borrow().is_empty()));
        set_lazy_iterators_enabled(false);
    }

    fn source_id(value: &Value) -> usize {
        match value {
            Value::IntArray(items) => Arc::as_ptr(items) as usize,
            _ => unreachable!(),
        }
    }
}

// ----------------------------------------------------------------------
// option - F#-style chaining surface for `Option<T>` (SPEC §10.4a).
// Data-last argument order. Methods are kept on `Option<T>` itself
// (Rust-style); these are the free-function siblings for use with
// `|>`.
