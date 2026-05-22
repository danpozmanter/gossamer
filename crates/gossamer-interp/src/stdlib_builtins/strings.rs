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

pub(crate) fn install_strings(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "strings",
        &[
            ("split", builtin_strings_split),
            ("splitn", builtin_strings_splitn),
            ("split_whitespace", builtin_strings_split_ws),
            ("trim", builtin_strings_trim),
            ("trim_start", builtin_strings_trim_start),
            ("trim_end", builtin_strings_trim_end),
            ("contains", builtin_strings_contains),
            ("find", builtin_strings_find),
            ("rfind", builtin_strings_rfind),
            ("split_once", builtin_strings_split_once),
            ("rsplit_once", builtin_strings_rsplit_once),
            ("count", builtin_strings_count),
            ("strip_chars", builtin_strings_strip_chars),
            ("lstrip_chars", builtin_strings_lstrip_chars),
            ("rstrip_chars", builtin_strings_rstrip_chars),
            ("zfill", builtin_strings_zfill),
            ("center", builtin_strings_center),
            ("slice", builtin_strings_slice),
            ("replace", builtin_strings_replace),
            ("replacen", builtin_strings_replacen),
            ("to_lower", builtin_strings_to_lower),
            ("to_upper", builtin_strings_to_upper),
            ("starts_with", builtin_strings_starts_with),
            ("ends_with", builtin_strings_ends_with),
            ("repeat", builtin_strings_repeat),
            ("lines", builtin_strings_lines),
            ("join", builtin_strings_join),
            ("strip_prefix", builtin_strings_strip_prefix),
            ("strip_suffix", builtin_strings_strip_suffix),
            ("pad_left", builtin_strings_pad_left),
            ("pad_right", builtin_strings_pad_right),
            ("contains_rune", builtin_strings_contains_rune),
            ("contains_any", builtin_strings_contains_any),
            ("index_rune", builtin_strings_index_rune),
            ("index_any", builtin_strings_index_any),
            ("last_index_any", builtin_strings_last_index_any),
            ("fields", builtin_strings_fields),
            ("equal_fold", builtin_strings_equal_fold),
            ("trim_matches", builtin_strings_trim_matches),
            ("to_title", builtin_strings_to_title),
        ],
        globals,
    );
}

pub(crate) fn builtin_strings_split(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let sep = args.get(1).and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split(text, sep)))
}

pub(crate) fn builtin_strings_splitn(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let n = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |x| usize::try_from(x.max(0)).unwrap_or(0));
    let sep = args.get(2).and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::splitn(text, n, sep)))
}

pub(crate) fn builtin_strings_split_ws(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split_whitespace(text)))
}

pub(crate) fn builtin_strings_trim(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim(text).into()))
}

pub(crate) fn builtin_strings_trim_start(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim_start(text).into()))
}

pub(crate) fn builtin_strings_trim_end(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim_end(text).into()))
}

pub(crate) fn builtin_strings_contains(args: &[Value]) -> RuntimeResult<Value> {
    // Delegate to the unified, array-aware `contains` so the
    // method-dispatch seeding (which seeds the bare `contains` key
    // from this entry) handles both `String::contains(substr)` and
    // `Vec::contains(&elem)`. Coercing a Vec receiver to "" here made
    // `"".contains("") == true`, so `nums.contains(&99)` wrongly
    // returned true.
    crate::builtins::builtin_contains(args)
}

pub(crate) fn builtin_strings_find(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::find(text, needle) {
        Some(idx) => Ok(some_variant(Value::Int(idx as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_rfind(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::rfind(text, needle) {
        Some(idx) => Ok(some_variant(Value::Int(idx as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_split_once(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let sep = args.get(1).and_then(as_str).unwrap_or("");
    match text.split_once(sep) {
        Some((head, tail)) => Ok(some_variant(Value::Tuple(std::sync::Arc::new(vec![
            Value::String(head.into()),
            Value::String(tail.into()),
        ])))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_rsplit_once(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let sep = args.get(1).and_then(as_str).unwrap_or("");
    match text.rsplit_once(sep) {
        Some((head, tail)) => Ok(some_variant(Value::Tuple(std::sync::Arc::new(vec![
            Value::String(head.into()),
            Value::String(tail.into()),
        ])))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_count(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    if needle.is_empty() {
        return Ok(Value::Int(0));
    }
    Ok(Value::Int(text.matches(needle).count() as i64))
}

pub(crate) fn builtin_strings_strip_chars(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let cutset = args.get(1).and_then(as_str).unwrap_or("");
    if cutset.is_empty() {
        return Ok(Value::String(text.into()));
    }
    let pat: Vec<char> = cutset.chars().collect();
    Ok(Value::String(text.trim_matches(pat.as_slice()).into()))
}

pub(crate) fn builtin_strings_lstrip_chars(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let cutset = args.get(1).and_then(as_str).unwrap_or("");
    if cutset.is_empty() {
        return Ok(Value::String(text.into()));
    }
    let pat: Vec<char> = cutset.chars().collect();
    Ok(Value::String(
        text.trim_start_matches(pat.as_slice()).into(),
    ))
}

pub(crate) fn builtin_strings_rstrip_chars(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let cutset = args.get(1).and_then(as_str).unwrap_or("");
    if cutset.is_empty() {
        return Ok(Value::String(text.into()));
    }
    let pat: Vec<char> = cutset.chars().collect();
    Ok(Value::String(text.trim_end_matches(pat.as_slice()).into()))
}

pub(crate) fn builtin_strings_zfill(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let width = args.get(1).and_then(value_to_int).unwrap_or(0);
    let cur = text.chars().count() as i64;
    if width <= 0 || cur >= width {
        return Ok(Value::String(text.into()));
    }
    let mut out = String::new();
    for _ in 0..(width - cur) {
        out.push('0');
    }
    out.push_str(text);
    Ok(Value::String(out.into()))
}

pub(crate) fn builtin_strings_center(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let width = args.get(1).and_then(value_to_int).unwrap_or(0);
    let pad = match args.get(2) {
        Some(Value::Char(c)) if *c != '\0' => *c,
        Some(other) => char::from_u32(value_to_int(other).unwrap_or(32) as u32).unwrap_or(' '),
        None => ' ',
    };
    let cur = text.chars().count() as i64;
    if width <= 0 || cur >= width {
        return Ok(Value::String(text.into()));
    }
    let total = (width - cur) as usize;
    let left = total / 2;
    let right = total - left;
    let mut out = String::new();
    for _ in 0..left {
        out.push(pad);
    }
    out.push_str(text);
    for _ in 0..right {
        out.push(pad);
    }
    Ok(Value::String(out.into()))
}

pub(crate) fn builtin_strings_slice(args: &[Value]) -> RuntimeResult<Value> {
    // Delegate to the unified String/Vec slicer so the seeded bare
    // `slice` key handles both `"text".slice(a, b)` and
    // `vec.slice(a, b)`. A String-only impl here coerced a Vec
    // receiver to "" and reported length 0.
    crate::builtins::builtin_str_or_vec_slice(args)
}

pub(crate) fn builtin_strings_replace(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let from = args.get(1).and_then(as_str).unwrap_or("");
    let to = args.get(2).and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::replace(text, from, to).into()))
}

pub(crate) fn builtin_strings_replacen(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let from = args.get(1).and_then(as_str).unwrap_or("");
    let to = args.get(2).and_then(as_str).unwrap_or("");
    let n = args
        .get(3)
        .and_then(value_to_int)
        .map_or(0, |v| usize::try_from(v.max(0)).unwrap_or(0));
    Ok(Value::String(
        strings_std::replacen(text, from, to, n).into(),
    ))
}

pub(crate) fn builtin_strings_to_lower(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_lowercase(text).into()))
}

pub(crate) fn builtin_strings_to_upper(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_uppercase(text).into()))
}

pub(crate) fn builtin_strings_starts_with(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let prefix = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::starts_with(text, prefix)))
}

pub(crate) fn builtin_strings_ends_with(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let suffix = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::ends_with(text, suffix)))
}

pub(crate) fn builtin_strings_repeat(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let count = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |v| usize::try_from(v.max(0)).unwrap_or(0));
    Ok(Value::String(strings_std::repeat(text, count).into()))
}

pub(crate) fn builtin_strings_lines(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::lines(text)))
}

pub(crate) fn builtin_strings_join(args: &[Value]) -> RuntimeResult<Value> {
    // Two argument shapes: (parts, sep) or (sep, parts).
    let first = args.first();
    let second = args.get(1);
    let (raw_parts, sep_opt): (Option<&Value>, Option<&str>) = match (first, second) {
        (Some(Value::Array(a)), Some(Value::String(s))) => {
            (Some(&Value::Array(a.clone())), Some(s.as_str()))
        }
        (Some(Value::String(s)), Some(Value::Array(_a))) => (second, Some(s.as_str())),
        (Some(Value::Array(_a)), _) => (first, Some("")),
        _ => return Ok(Value::String(String::new().into())),
    };
    let sep_owned = sep_opt.unwrap_or("").to_string();
    let parts: Vec<String> = match raw_parts {
        Some(Value::Array(arr)) => arr
            .iter()
            .map(|v| match v {
                Value::String(s) => s.as_str().to_string(),
                other => format!("{other}"),
            })
            .collect(),
        _ => Vec::new(),
    };
    Ok(Value::String(strings_std::join(&parts, &sep_owned).into()))
}

pub(crate) fn builtin_strings_strip_prefix(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let prefix = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::strip_prefix(text, prefix) {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_strip_suffix(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let suffix = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::strip_suffix(text, suffix) {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_pad_left(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let width = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |v| usize::try_from(v.max(0)).unwrap_or(0));
    let pad_char = args
        .get(2)
        .and_then(|v| match v {
            Value::Char(c) => Some(*c),
            Value::String(s) => s.as_str().chars().next(),
            _ => None,
        })
        .unwrap_or(' ');
    Ok(Value::String(
        strings_std::pad_left(text, width, pad_char).into(),
    ))
}

pub(crate) fn builtin_strings_pad_right(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let width = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |v| usize::try_from(v.max(0)).unwrap_or(0));
    let pad_char = args
        .get(2)
        .and_then(|v| match v {
            Value::Char(c) => Some(*c),
            Value::String(s) => s.as_str().chars().next(),
            _ => None,
        })
        .unwrap_or(' ');
    Ok(Value::String(
        strings_std::pad_right(text, width, pad_char).into(),
    ))
}

pub(crate) fn builtin_strings_contains_rune(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let r = match args.get(1) {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        _ => '\0',
    };
    Ok(Value::Bool(strings_std::contains_rune(text, r)))
}

pub(crate) fn builtin_strings_contains_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::contains_any(text, chars)))
}

pub(crate) fn builtin_strings_index_rune(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let r = match args.get(1) {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        _ => '\0',
    };
    match strings_std::index_rune(text, r) {
        Some(i) => Ok(some_variant(Value::Int(i as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_index_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::index_any(text, chars) {
        Some(i) => Ok(some_variant(Value::Int(i as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_last_index_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::last_index_any(text, chars) {
        Some(i) => Ok(some_variant(Value::Int(i as i64))),
        None => Ok(none_variant()),
    }
}

pub(crate) fn builtin_strings_fields(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::fields(text)))
}

pub(crate) fn builtin_strings_equal_fold(args: &[Value]) -> RuntimeResult<Value> {
    let a = args.first().and_then(as_str).unwrap_or("");
    let b = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::equal_fold(a, b)))
}

pub(crate) fn builtin_strings_trim_matches(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let cutset = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::String(
        strings_std::trim_matches(text, cutset).into(),
    ))
}

pub(crate) fn builtin_strings_to_title(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_title(text).into()))
}

// ----------------------------------------------------------------------
// strconv
