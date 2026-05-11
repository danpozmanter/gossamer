//! Wires up Gossamer-callable builtins for stdlib modules whose
//! Rust-side implementation already exists but had no user-facing
//! exposure. Each `install_*` helper is invoked from
//! `builtins::install` so user code that writes
//! `strings::join`, `strconv::parse_i64`, `net::TcpStream::connect`,
//! `time::Instant::now`, etc. resolves to a real callable.
//!
//! All builtins return a `Result`-shaped variant (`Ok` / `Err`) on
//! fallible operations so callers can chain `?` without wrapping.

#![allow(clippy::unnecessary_wraps)]

use std::cell::RefCell;
use std::collections::HashMap as StdHashMap;
use std::io::Read as IoRead;
use std::sync::Arc;

use gossamer_ast::Ident;

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
pub(crate) fn install(globals: &mut Vec<(&'static str, Value)>) {
    install_strings(globals);
    install_strconv(globals);
    install_path(globals);
    install_utf8(globals);
    install_os_extras(globals);
    install_fs_extras(globals);
    install_bufio_extras(globals);
    install_time_extras(globals);
    install_net(globals);
    install_set(globals);
    install_sync_extras(globals);
    install_math(globals);
    install_math_bits(globals);
    install_unicode(globals);
    install_encoding_binary(globals);
    install_encoding_csv(globals);
    install_encoding_pem(globals);
    install_utf16(globals);
    install_iter(globals);
    install_crypto(globals);
    install_encoding_yaml(globals);
    install_compress(globals);
    install_hash_fnv(globals);
    install_archive_zip(globals);
    install_archive_tar(globals);
    install_sync_atomic_u64(globals);
    install_sync_barrier(globals);
    install_crypto_breadth(globals);
    install_hash_crc32_adler32(globals);
    install_json_builtins(globals);
    install_time_completeness(globals);
    install_net_ip(globals);
    install_thread(globals);
    install_html(globals);
    install_encoding_base64_hex(globals);
    install_encoding_base32(globals);
    install_encoding_ascii85(globals);
    install_encoding_xml(globals);
    install_crypto_insecure(globals);
    install_compress_bzip2(globals);
    install_math_big(globals);
}

// ----------------------------------------------------------------------
// Helpers

fn arg_str_at(args: &[Value], idx: usize, fn_name: &str, label: &str) -> Result<String, Value> {
    match args.get(idx) {
        Some(Value::String(s)) => Ok(s.as_str().to_string()),
        _ => Err(err_variant(format!("{fn_name}: expected string {label}"))),
    }
}

fn string_array(values: Vec<String>) -> Value {
    Value::Array(Arc::new(
        values
            .into_iter()
            .map(|s| Value::String(s.into()))
            .collect(),
    ))
}

// ----------------------------------------------------------------------
// strings

fn install_strings(globals: &mut Vec<(&'static str, Value)>) {
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

fn builtin_strings_split(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let sep = args.get(1).and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split(text, sep)))
}

fn builtin_strings_splitn(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let n = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |x| usize::try_from(x.max(0)).unwrap_or(0));
    let sep = args.get(2).and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::splitn(text, n, sep)))
}

fn builtin_strings_split_ws(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split_whitespace(text)))
}

fn builtin_strings_trim(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim(text).into()))
}

fn builtin_strings_trim_start(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim_start(text).into()))
}

fn builtin_strings_trim_end(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::trim_end(text).into()))
}

fn builtin_strings_contains(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::contains(text, needle)))
}

fn builtin_strings_find(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::find(text, needle) {
        Some(idx) => Ok(some_variant(Value::Int(idx as i64))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_rfind(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let needle = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::rfind(text, needle) {
        Some(idx) => Ok(some_variant(Value::Int(idx as i64))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_replace(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let from = args.get(1).and_then(as_str).unwrap_or("");
    let to = args.get(2).and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::replace(text, from, to).into()))
}

fn builtin_strings_replacen(args: &[Value]) -> RuntimeResult<Value> {
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

fn builtin_strings_to_lower(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_lowercase(text).into()))
}

fn builtin_strings_to_upper(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_uppercase(text).into()))
}

fn builtin_strings_starts_with(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let prefix = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::starts_with(text, prefix)))
}

fn builtin_strings_ends_with(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let suffix = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::ends_with(text, suffix)))
}

fn builtin_strings_repeat(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let count = args
        .get(1)
        .and_then(value_to_int)
        .map_or(0, |v| usize::try_from(v.max(0)).unwrap_or(0));
    Ok(Value::String(strings_std::repeat(text, count).into()))
}

fn builtin_strings_lines(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::lines(text)))
}

fn builtin_strings_join(args: &[Value]) -> RuntimeResult<Value> {
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

fn builtin_strings_strip_prefix(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let prefix = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::strip_prefix(text, prefix) {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_strip_suffix(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let suffix = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::strip_suffix(text, suffix) {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_pad_left(args: &[Value]) -> RuntimeResult<Value> {
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

fn builtin_strings_pad_right(args: &[Value]) -> RuntimeResult<Value> {
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

fn builtin_strings_contains_rune(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let r = match args.get(1) {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        _ => '\0',
    };
    Ok(Value::Bool(strings_std::contains_rune(text, r)))
}

fn builtin_strings_contains_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::contains_any(text, chars)))
}

fn builtin_strings_index_rune(args: &[Value]) -> RuntimeResult<Value> {
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

fn builtin_strings_index_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::index_any(text, chars) {
        Some(i) => Ok(some_variant(Value::Int(i as i64))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_last_index_any(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let chars = args.get(1).and_then(as_str).unwrap_or("");
    match strings_std::last_index_any(text, chars) {
        Some(i) => Ok(some_variant(Value::Int(i as i64))),
        None => Ok(none_variant()),
    }
}

fn builtin_strings_fields(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::fields(text)))
}

fn builtin_strings_equal_fold(args: &[Value]) -> RuntimeResult<Value> {
    let a = args.first().and_then(as_str).unwrap_or("");
    let b = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::Bool(strings_std::equal_fold(a, b)))
}

fn builtin_strings_trim_matches(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    let cutset = args.get(1).and_then(as_str).unwrap_or("");
    Ok(Value::String(
        strings_std::trim_matches(text, cutset).into(),
    ))
}

fn builtin_strings_to_title(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(strings_std::to_title(text).into()))
}

// ----------------------------------------------------------------------
// strconv

fn install_strconv(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "strconv",
        &[
            ("parse_int", builtin_strconv_parse_i64),
            ("parse_i64", builtin_strconv_parse_i64),
            ("parse_u64", builtin_strconv_parse_u64),
            ("parse_float", builtin_strconv_parse_f64),
            ("parse_f64", builtin_strconv_parse_f64),
            ("parse_bool", builtin_strconv_parse_bool),
            ("format_int", builtin_strconv_format_i64),
            ("format_i64", builtin_strconv_format_i64),
            ("format_float", builtin_strconv_format_f64),
            ("format_f64", builtin_strconv_format_f64),
            ("itoa", builtin_strconv_format_i64),
            ("atoi", builtin_strconv_parse_i64),
        ],
        globals,
    );
}

fn builtin_strconv_parse_i64(args: &[Value]) -> RuntimeResult<Value> {
    let text = match arg_str_at(args, 0, "strconv::parse_int", "argument") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match strconv_std::parse_i64(&text) {
        Ok(n) => Ok(ok_variant(Value::Int(n))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_strconv_parse_u64(args: &[Value]) -> RuntimeResult<Value> {
    let text = match arg_str_at(args, 0, "strconv::parse_u64", "argument") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match strconv_std::parse_u64(&text) {
        Ok(n) => Ok(ok_variant(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_strconv_parse_f64(args: &[Value]) -> RuntimeResult<Value> {
    let text = match arg_str_at(args, 0, "strconv::parse_float", "argument") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match strconv_std::parse_f64(&text) {
        Ok(n) => Ok(ok_variant(Value::Float(n))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_strconv_parse_bool(args: &[Value]) -> RuntimeResult<Value> {
    let text = match arg_str_at(args, 0, "strconv::parse_bool", "argument") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match strconv_std::parse_bool(&text) {
        Ok(b) => Ok(ok_variant(Value::Bool(b))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_strconv_format_i64(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::String(strconv_std::format_i64(n).into()))
}

fn builtin_strconv_format_f64(args: &[Value]) -> RuntimeResult<Value> {
    let f = match args.first() {
        Some(Value::Float(f)) => *f,
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    };
    Ok(Value::String(strconv_std::format_f64(f).into()))
}

// ----------------------------------------------------------------------
// path

fn install_path(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "path",
        &[
            ("parent", builtin_path_parent),
            ("file_name", builtin_path_file_name),
            ("stem", builtin_path_stem),
            ("ext", builtin_path_ext),
            ("extension", builtin_path_ext),
            ("is_absolute", builtin_path_is_absolute),
            ("normalize", builtin_path_normalize),
        ],
        globals,
    );
}

fn builtin_path_parent(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    let dir = path_std::dir(path);
    if dir.is_empty() {
        Ok(none_variant())
    } else {
        Ok(some_variant(Value::String(dir.into())))
    }
}

fn builtin_path_file_name(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    let base = path_std::base(path);
    if base.is_empty() {
        Ok(none_variant())
    } else {
        Ok(some_variant(Value::String(base.into())))
    }
}

fn builtin_path_stem(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    let base = path_std::base(path);
    if base.is_empty() {
        return Ok(none_variant());
    }
    let stem = match base.rfind('.') {
        Some(idx) if idx > 0 => base[..idx].to_string(),
        _ => base,
    };
    Ok(some_variant(Value::String(stem.into())))
}

fn builtin_path_ext(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    let ext = path_std::ext(path);
    if ext.is_empty() {
        Ok(none_variant())
    } else {
        Ok(some_variant(Value::String(ext.into())))
    }
}

fn builtin_path_is_absolute(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(path_std::is_absolute(path)))
}

fn builtin_path_normalize(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::String(path_std::clean(path).into()))
}

// ----------------------------------------------------------------------
// utf8

fn install_utf8(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "utf8",
        &[
            ("count_runes", builtin_utf8_count_runes),
            ("rune_count", builtin_utf8_count_runes),
            ("rune_count_in_string", builtin_utf8_rune_count_in_string),
            ("rune_len", builtin_utf8_rune_len),
            ("is_valid", builtin_utf8_is_valid),
            ("valid_string", builtin_utf8_valid_string),
            ("valid_rune", builtin_utf8_valid_rune),
            ("full_rune", builtin_utf8_full_rune),
            ("rune_start", builtin_utf8_rune_start),
            ("decode_rune", builtin_utf8_decode_rune),
            ("decode_rune_in_string", builtin_utf8_decode_rune_in_string),
            ("decode_last_rune", builtin_utf8_decode_last_rune),
            (
                "decode_last_rune_in_string",
                builtin_utf8_decode_last_rune_in_string,
            ),
            ("append_rune", builtin_utf8_append_rune),
        ],
        globals,
    );
}

fn builtin_utf8_count_runes(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Int(utf8_std::rune_count(text.as_bytes()) as i64))
}

fn builtin_utf8_rune_len(args: &[Value]) -> RuntimeResult<Value> {
    let c = match args.first() {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => match s.as_str().chars().next() {
            Some(c) => c,
            None => return Ok(Value::Int(0)),
        },
        _ => return Ok(Value::Int(0)),
    };
    Ok(Value::Int(c.len_utf8() as i64))
}

fn builtin_utf8_is_valid(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::String(_)) => Ok(Value::Bool(true)),
        Some(Value::Array(arr)) => {
            let bytes: Vec<u8> = arr
                .iter()
                .filter_map(|v| match v {
                    Value::Int(n) => u8::try_from(*n).ok(),
                    _ => None,
                })
                .collect();
            Ok(Value::Bool(std::str::from_utf8(&bytes).is_ok()))
        }
        _ => Ok(Value::Bool(false)),
    }
}

fn builtin_utf8_valid_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(utf8_std::valid_string(s)))
}

fn builtin_utf8_valid_rune(args: &[Value]) -> RuntimeResult<Value> {
    let r = match args.first() {
        Some(Value::Int(n)) => *n as u32,
        Some(Value::Char(c)) => *c as u32,
        _ => return Ok(Value::Bool(false)),
    };
    Ok(Value::Bool(utf8_std::valid_rune(r)))
}

fn builtin_utf8_rune_count_in_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Int(utf8_std::rune_count_in_string(s) as i64))
}

fn builtin_utf8_full_rune(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_utf8_arg(args.first());
    Ok(Value::Bool(utf8_std::full_rune(&bytes)))
}

fn builtin_utf8_rune_start(args: &[Value]) -> RuntimeResult<Value> {
    let b = match args.first() {
        Some(Value::Int(n)) => *n as u8,
        _ => return Ok(Value::Bool(false)),
    };
    Ok(Value::Bool(utf8_std::rune_start(b)))
}

fn bytes_from_utf8_arg(v: Option<&Value>) -> Vec<u8> {
    match v {
        Some(Value::String(s)) => s.as_str().as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|e| match e {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

fn decode_rune_result(ch: char, n: usize) -> Value {
    Value::Tuple(Arc::new(vec![Value::Char(ch), Value::Int(n as i64)]))
}

fn builtin_utf8_decode_rune(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_utf8_arg(args.first());
    let (ch, n) = utf8_std::decode_rune(&bytes);
    Ok(decode_rune_result(ch, n))
}

fn builtin_utf8_decode_rune_in_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    let (ch, n) = utf8_std::decode_rune_in_string(s);
    Ok(decode_rune_result(ch, n))
}

fn builtin_utf8_decode_last_rune(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_utf8_arg(args.first());
    let (ch, n) = utf8_std::decode_last_rune(&bytes);
    Ok(decode_rune_result(ch, n))
}

fn builtin_utf8_decode_last_rune_in_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    let (ch, n) = utf8_std::decode_last_rune_in_string(s);
    Ok(decode_rune_result(ch, n))
}

fn builtin_utf8_append_rune(args: &[Value]) -> RuntimeResult<Value> {
    let buf = bytes_from_utf8_arg(args.first());
    let r = match args.get(1) {
        Some(Value::Char(c)) => *c,
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        _ => '\0',
    };
    let result = utf8_std::append_rune(buf, r);
    Ok(Value::Array(Arc::new(
        result
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    )))
}

// ----------------------------------------------------------------------
// os extras

fn install_os_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "os",
        &[
            ("set_env", builtin_os_set_env),
            ("unset_env", builtin_os_unset_env),
            ("is_file", builtin_os_is_file),
            ("is_dir", builtin_os_is_dir),
            ("is_symlink", builtin_os_is_symlink),
            ("file_size", builtin_os_file_size),
            ("home", builtin_os_home),
            ("temp_dir", builtin_os_temp_dir),
            ("set_cwd", builtin_os_set_cwd),
            ("canonicalize", builtin_os_canonicalize),
            ("remove_dir", builtin_os_remove_dir),
            ("remove_dir_all", builtin_os_remove_dir_all),
            ("copy", builtin_os_copy),
        ],
        globals,
    );
}

fn builtin_os_set_env(args: &[Value]) -> RuntimeResult<Value> {
    let name = match arg_str_at(args, 0, "os::set_env", "name") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let value = args.get(1).and_then(as_str).unwrap_or("").to_string();
    match os_std::set_env(&name, &value) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_unset_env(args: &[Value]) -> RuntimeResult<Value> {
    let name = args.first().and_then(as_str).unwrap_or("");
    os_std::unset_env(name);
    Ok(Value::Unit)
}

fn builtin_os_is_file(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_file(path)))
}

fn builtin_os_is_dir(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_dir(path)))
}

fn builtin_os_is_symlink(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(os_std::is_symlink(path)))
}

fn builtin_os_file_size(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Int(
        i64::try_from(os_std::file_size(path)).unwrap_or(i64::MAX),
    ))
}

fn builtin_os_home(_args: &[Value]) -> RuntimeResult<Value> {
    match os_std::home() {
        Some(s) => Ok(some_variant(Value::String(s.into()))),
        None => Ok(none_variant()),
    }
}

fn builtin_os_temp_dir(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::String(os_std::temp_dir().into()))
}

fn builtin_os_set_cwd(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::set_cwd", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::set_cwd(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_canonicalize(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::canonicalize", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::canonicalize(&path) {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_remove_dir(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::remove_dir", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::remove_dir(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_remove_dir_all(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "os::remove_dir_all", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::remove_dir_all(&path) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_os_copy(args: &[Value]) -> RuntimeResult<Value> {
    let src = match arg_str_at(args, 0, "os::copy", "source path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let dst = match arg_str_at(args, 1, "os::copy", "destination path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match os_std::copy(&src, &dst) {
        Ok(n) => Ok(ok_variant(Value::Int(i64::try_from(n).unwrap_or(i64::MAX)))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// fs extras

fn install_fs_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "fs",
        &[
            ("is_file", builtin_os_is_file),
            ("is_dir", builtin_os_is_dir),
            ("is_symlink", builtin_os_is_symlink),
            ("file_size", builtin_os_file_size),
            ("metadata", builtin_fs_metadata),
            ("copy", builtin_os_copy),
            ("canonicalize", builtin_os_canonicalize),
        ],
        globals,
    );
}

fn builtin_fs_metadata(args: &[Value]) -> RuntimeResult<Value> {
    let path = args.first().and_then(as_str).unwrap_or("");
    match std::fs::metadata(path) {
        Ok(meta) => {
            let fields = vec![
                (
                    Ident::new("size"),
                    Value::Int(i64::try_from(meta.len()).unwrap_or(i64::MAX)),
                ),
                (Ident::new("is_file"), Value::Bool(meta.is_file())),
                (Ident::new("is_dir"), Value::Bool(meta.is_dir())),
                (
                    Ident::new("is_symlink"),
                    Value::Bool(meta.file_type().is_symlink()),
                ),
                (
                    Ident::new("readonly"),
                    Value::Bool(meta.permissions().readonly()),
                ),
                (
                    Ident::new("modified_unix_ms"),
                    Value::Int(
                        meta.modified()
                            .ok()
                            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX)),
                    ),
                ),
            ];
            Ok(ok_variant(Value::struct_("fs::Metadata", Arc::new(fields))))
        }
        Err(e) => Ok(err_variant(format!("metadata: {e}"))),
    }
}

// ----------------------------------------------------------------------
// bufio extras

fn install_bufio_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "bufio",
        &[
            ("read_to_string", builtin_bufio_read_to_string),
            ("read_lines_of", builtin_bufio_read_lines_of),
            ("split_whitespace", builtin_bufio_split_whitespace),
        ],
        globals,
    );
}

fn builtin_bufio_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "bufio::read_to_string", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match std::fs::File::open(&path) {
        Ok(file) => {
            let mut reader = bufio_std::Reader::new(file);
            let mut out = String::new();
            let mut chunk = [0u8; 4096];
            loop {
                match IoRead::read(&mut reader, &mut chunk) {
                    Ok(0) => break,
                    Ok(n) => out.push_str(&String::from_utf8_lossy(&chunk[..n])),
                    Err(e) => return Ok(err_variant(format!("read: {e}"))),
                }
            }
            Ok(ok_variant(Value::String(out.into())))
        }
        Err(e) => Ok(err_variant(format!("open {path}: {e}"))),
    }
}

fn builtin_bufio_read_lines_of(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "bufio::read_lines_of", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(ok_variant(string_array(strings_std::lines(&text)))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_bufio_split_whitespace(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split_whitespace(text)))
}

// ----------------------------------------------------------------------
// time extras

fn install_time_extras(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "time",
        &[
            ("now_nanos", builtin_time_now_nanos),
            ("monotonic_ms", builtin_time_monotonic_ms),
            ("monotonic_nanos", builtin_time_monotonic_nanos),
            ("since_ms", builtin_time_since_ms),
            ("Instant::now", builtin_time_instant_now),
            ("Instant::elapsed_ms", builtin_time_instant_elapsed_ms),
            ("Duration::from_millis", builtin_time_duration_from_millis),
            ("Duration::from_secs", builtin_time_duration_from_secs),
            ("Duration::from_micros", builtin_time_duration_from_micros),
            ("Duration::as_millis", builtin_time_duration_as_millis),
            ("Duration::as_secs", builtin_time_duration_as_secs),
            ("Duration::as_micros", builtin_time_duration_as_micros),
        ],
        globals,
    );
    globals.push((
        "Instant::now",
        crate::builtins::builtin_pub("Instant::now", builtin_time_instant_now),
    ));
    globals.push((
        "elapsed_ms",
        crate::builtins::builtin_pub("elapsed_ms", builtin_time_instant_elapsed_ms),
    ));
}

fn builtin_time_now_nanos(_args: &[Value]) -> RuntimeResult<Value> {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    Ok(Value::Int(i64::try_from(nanos).unwrap_or(i64::MAX)))
}

thread_local! {
    static MONOTONIC_BASE: std::cell::OnceCell<std::time::Instant> = const { std::cell::OnceCell::new() };
}

fn monotonic_base() -> std::time::Instant {
    MONOTONIC_BASE.with(|cell| *cell.get_or_init(std::time::Instant::now))
}

fn builtin_time_monotonic_ms(_args: &[Value]) -> RuntimeResult<Value> {
    let dur = monotonic_base().elapsed();
    Ok(Value::Int(
        i64::try_from(dur.as_millis()).unwrap_or(i64::MAX),
    ))
}

fn builtin_time_monotonic_nanos(_args: &[Value]) -> RuntimeResult<Value> {
    let dur = monotonic_base().elapsed();
    Ok(Value::Int(
        i64::try_from(dur.as_nanos()).unwrap_or(i64::MAX),
    ))
}

fn builtin_time_since_ms(args: &[Value]) -> RuntimeResult<Value> {
    let start = args.first().and_then(value_to_int).unwrap_or(0);
    let now = i64::try_from(monotonic_base().elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(Value::Int(now.saturating_sub(start)))
}

fn builtin_time_instant_now(_args: &[Value]) -> RuntimeResult<Value> {
    let ms = i64::try_from(monotonic_base().elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(Value::struct_(
        "time::Instant",
        Arc::new(vec![(Ident::new("__ms"), Value::Int(ms))]),
    ))
}

fn builtin_time_instant_elapsed_ms(args: &[Value]) -> RuntimeResult<Value> {
    let start_ms = match args.first() {
        Some(Value::Struct(s)) => s
            .fields
            .iter()
            .find_map(|(i, v)| {
                if i.name == "__ms" {
                    if let Value::Int(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or(0),
        Some(Value::Int(n)) => *n,
        _ => 0,
    };
    let now = i64::try_from(monotonic_base().elapsed().as_millis()).unwrap_or(i64::MAX);
    Ok(Value::Int(now.saturating_sub(start_ms)))
}

fn builtin_time_duration_from_millis(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(args.first().and_then(value_to_int).unwrap_or(0)))
}

fn builtin_time_duration_from_secs(args: &[Value]) -> RuntimeResult<Value> {
    let secs = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(secs.saturating_mul(1000)))
}

fn builtin_time_duration_from_micros(args: &[Value]) -> RuntimeResult<Value> {
    let us = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(us / 1000))
}

fn builtin_time_duration_as_millis(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(args.first().and_then(value_to_int).unwrap_or(0)))
}

fn builtin_time_duration_as_secs(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        args.first().and_then(value_to_int).unwrap_or(0) / 1000,
    ))
}

fn builtin_time_duration_as_micros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        args.first()
            .and_then(value_to_int)
            .unwrap_or(0)
            .saturating_mul(1000),
    ))
}

// ----------------------------------------------------------------------
// net (TCP listener / stream + UDP socket + DNS)
//
// Sockets are referred to from Gossamer code via opaque handle values
// (`net::TcpStream` / `net::TcpListener` / `net::UdpSocket` structs
// holding a __handle: i64). The Rust-side socket lives in a per-thread
// registry keyed by handle id.

thread_local! {
    static NEXT_NET_ID: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    static TCP_STREAM_REGISTRY: RefCell<StdHashMap<i64, net_std::TcpStream>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    static TCP_LISTENER_REGISTRY: RefCell<StdHashMap<i64, net_std::TcpListener>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    static UDP_REGISTRY: RefCell<StdHashMap<i64, net_std::UdpSocket>> =
        RefCell::new(StdHashMap::new());
}

fn next_net_id() -> i64 {
    NEXT_NET_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

fn handle_struct(name: &'static str, id: i64) -> Value {
    Value::struct_(
        name,
        Arc::new(vec![(Ident::new("__handle"), Value::Int(id))]),
    )
}

fn handle_id(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        for (ident, v) in inner.fields.iter() {
            if ident.name == "__handle" {
                if let Value::Int(n) = v {
                    return Some(*n);
                }
            }
        }
    }
    None
}

fn install_net(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "net",
        &[
            ("resolve", builtin_net_resolve),
            ("lookup", builtin_net_resolve),
        ],
        globals,
    );
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("TcpListener::bind", builtin_tcp_listener_bind),
        ("TcpListener::accept", builtin_tcp_listener_accept),
        ("TcpListener::local_addr", builtin_tcp_listener_local_addr),
        ("TcpListener::close", builtin_tcp_listener_close),
        ("TcpStream::connect", builtin_tcp_stream_connect),
        ("TcpStream::read", builtin_tcp_stream_read),
        (
            "TcpStream::read_to_string",
            builtin_tcp_stream_read_to_string,
        ),
        ("TcpStream::write", builtin_tcp_stream_write),
        ("TcpStream::write_all", builtin_tcp_stream_write),
        ("TcpStream::close", builtin_tcp_stream_close),
        ("UdpSocket::bind", builtin_udp_bind),
        ("UdpSocket::send_to", builtin_udp_send_to),
        ("UdpSocket::recv_from", builtin_udp_recv_from),
        ("UdpSocket::local_addr", builtin_udp_local_addr),
        ("UdpSocket::close", builtin_udp_close),
    ];
    for (short, call) in entries {
        let qualified: &'static str = Box::leak(format!("net::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*short, crate::builtins::builtin_pub(short, *call)));
    }
    // Bare-name dispatch for method-call shape (`stream.read(n)`).
    for (short, call) in &[
        ("recv_from", builtin_udp_recv_from as BuiltinFnPub),
        ("send_to", builtin_udp_send_to as BuiltinFnPub),
        ("accept", builtin_tcp_listener_accept as BuiltinFnPub),
        (
            "local_addr",
            builtin_tcp_listener_local_addr as BuiltinFnPub,
        ),
    ] {
        globals.push((*short, crate::builtins::builtin_pub(short, *call)));
    }
}

fn builtin_net_resolve(args: &[Value]) -> RuntimeResult<Value> {
    let host = args.first().and_then(as_str).unwrap_or("");
    let needle = if host.contains(':') {
        host.to_string()
    } else {
        format!("{host}:0")
    };
    match net_std::resolve(&needle) {
        Ok(addrs) => {
            let values: Vec<Value> = addrs
                .into_iter()
                .map(|a| Value::String(a.ip().to_string().into()))
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(values))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_tcp_listener_bind(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "TcpListener::bind", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::TcpListener::bind(&addr) {
        Ok(listener) => {
            let id = next_net_id();
            TCP_LISTENER_REGISTRY.with(|r| {
                r.borrow_mut().insert(id, listener);
            });
            Ok(ok_variant(handle_struct("net::TcpListener", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_tcp_listener_accept(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpListener::accept: missing handle"));
    };
    let res = TCP_LISTENER_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(listener) = reg.get_mut(&id) else {
            return Err("TcpListener::accept: stale handle".to_string());
        };
        listener.accept().map_err(|e| e.to_string())
    });
    match res {
        Ok((stream, addr)) => {
            let sid = next_net_id();
            TCP_STREAM_REGISTRY.with(|r| {
                r.borrow_mut().insert(sid, stream);
            });
            let pair = Value::Tuple(Arc::new(vec![
                handle_struct("net::TcpStream", sid),
                Value::String(addr.to_string().into()),
            ]));
            Ok(ok_variant(pair))
        }
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_tcp_listener_local_addr(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpListener::local_addr: missing handle"));
    };
    let res = TCP_LISTENER_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|l| l.local_addr().map_err(|e| e.to_string()))
    });
    match res {
        Some(Ok(addr)) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Some(Err(e)) => Ok(err_variant(e)),
        None => Ok(err_variant("TcpListener::local_addr: stale handle")),
    }
}

fn builtin_tcp_listener_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        TCP_LISTENER_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

fn builtin_tcp_stream_connect(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "TcpStream::connect", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::TcpStream::connect(&addr) {
        Ok(stream) => {
            let id = next_net_id();
            TCP_STREAM_REGISTRY.with(|r| {
                r.borrow_mut().insert(id, stream);
            });
            Ok(ok_variant(handle_struct("net::TcpStream", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_tcp_stream_read(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::read: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(4096)
        .clamp(1, 1 << 24);
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::read: stale handle".to_string());
        };
        let mut buf = vec![0u8; max as usize];
        match stream.read(&mut buf) {
            Ok(n) => {
                buf.truncate(n);
                Ok(buf)
            }
            Err(e) => Err(e.to_string()),
        }
    });
    match res {
        Ok(bytes) => Ok(ok_variant(Value::Array(Arc::new(
            bytes
                .into_iter()
                .map(|b| Value::Int(i64::from(b)))
                .collect(),
        )))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_tcp_stream_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::read_to_string: missing handle"));
    };
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::read_to_string: stale handle".to_string());
        };
        let mut out = Vec::new();
        let mut chunk = [0u8; 4096];
        loop {
            match stream.read(&mut chunk) {
                Ok(0) => break,
                Ok(n) => out.extend_from_slice(&chunk[..n]),
                Err(e) => return Err(e.to_string()),
            }
        }
        Ok(String::from_utf8_lossy(&out).into_owned())
    });
    match res {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_tcp_stream_write(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("TcpStream::write: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "TcpStream::write: expected string or byte array",
            ));
        }
    };
    let res = TCP_STREAM_REGISTRY.with(|r| {
        let mut reg = r.borrow_mut();
        let Some(stream) = reg.get_mut(&id) else {
            return Err("TcpStream::write: stale handle".to_string());
        };
        stream.write_all(&bytes).map_err(|e| e.to_string())
    });
    match res {
        Ok(()) => Ok(ok_variant(Value::Int(bytes.len() as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_tcp_stream_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        TCP_STREAM_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

fn builtin_udp_bind(args: &[Value]) -> RuntimeResult<Value> {
    let addr = match arg_str_at(args, 0, "UdpSocket::bind", "address") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    match net_std::UdpSocket::bind(&addr) {
        Ok(sock) => {
            let id = next_net_id();
            UDP_REGISTRY.with(|r| {
                r.borrow_mut().insert(id, sock);
            });
            Ok(ok_variant(handle_struct("net::UdpSocket", id)))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_udp_send_to(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::send_to: missing handle"));
    };
    let bytes: Vec<u8> = match args.get(1) {
        Some(Value::String(s)) => s.as_bytes().to_vec(),
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => {
            return Ok(err_variant(
                "UdpSocket::send_to: expected string or byte array",
            ));
        }
    };
    let addr = args.get(2).and_then(as_str).unwrap_or("").to_string();
    let res = UDP_REGISTRY.with(|r| {
        let reg = r.borrow();
        match reg.get(&id) {
            Some(sock) => sock.send_to(&bytes, &addr).map_err(|e| e.to_string()),
            None => Err("UdpSocket::send_to: stale handle".to_string()),
        }
    });
    match res {
        Ok(n) => Ok(ok_variant(Value::Int(n as i64))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_udp_recv_from(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::recv_from: missing handle"));
    };
    let max = args
        .get(1)
        .and_then(value_to_int)
        .unwrap_or(1500)
        .clamp(1, 1 << 16);
    let res = UDP_REGISTRY.with(|r| {
        let reg = r.borrow();
        let Some(sock) = reg.get(&id) else {
            return Err("UdpSocket::recv_from: stale handle".to_string());
        };
        let mut buf = vec![0u8; max as usize];
        match sock.recv_from(&mut buf) {
            Ok((n, addr)) => {
                buf.truncate(n);
                Ok((buf, addr))
            }
            Err(e) => Err(e.to_string()),
        }
    });
    match res {
        Ok((bytes, addr)) => {
            let bytes_v = Value::Array(Arc::new(
                bytes
                    .into_iter()
                    .map(|b| Value::Int(i64::from(b)))
                    .collect(),
            ));
            Ok(ok_variant(Value::Tuple(Arc::new(vec![
                bytes_v,
                Value::String(addr.to_string().into()),
            ]))))
        }
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_udp_local_addr(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(handle_id) else {
        return Ok(err_variant("UdpSocket::local_addr: missing handle"));
    };
    let res = UDP_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|s| s.local_addr().map_err(|e| e.to_string()))
    });
    match res {
        Some(Ok(addr)) => Ok(ok_variant(Value::String(addr.to_string().into()))),
        Some(Err(e)) => Ok(err_variant(e)),
        None => Ok(err_variant("UdpSocket::local_addr: stale handle")),
    }
}

fn builtin_udp_close(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(handle_id) {
        UDP_REGISTRY.with(|r| {
            r.borrow_mut().remove(&id);
        });
    }
    Ok(Value::Unit)
}

// ----------------------------------------------------------------------
// HashSet (real set, distinct from HashMap)

thread_local! {
    static NEXT_SET_HANDLE: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    static SET_REGISTRY: RefCell<StdHashMap<i64, std::collections::HashSet<MapKey>>> =
        RefCell::new(StdHashMap::new());
}

fn install_set(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("HashSet::new", builtin_set_new),
        ("HashSet::insert", builtin_set_insert),
        ("HashSet::remove", builtin_set_remove),
        ("HashSet::contains", builtin_set_contains),
        ("HashSet::len", builtin_set_len),
        ("HashSet::is_empty", builtin_set_is_empty),
        ("HashSet::clear", builtin_set_clear),
        ("HashSet::to_vec", builtin_set_to_vec),
        ("HashSet::iter", builtin_set_to_vec),
        ("collections::HashSet::new", builtin_set_new),
    ];
    for (name, call) in entries {
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn next_set_handle() -> i64 {
    NEXT_SET_HANDLE.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

fn set_handle(id: i64) -> Value {
    Value::struct_(
        "HashSet",
        Arc::new(vec![(Ident::new("__set"), Value::Int(id))]),
    )
}

fn set_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "HashSet" {
            for (i, v) in inner.fields.iter() {
                if i.name == "__set" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn builtin_set_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = next_set_handle();
    SET_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, std::collections::HashSet::new());
    });
    Ok(set_handle(id))
}

fn builtin_set_insert(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args.first().cloned().unwrap_or(Value::Unit);
    let Some(id) = set_id_of(&handle) else {
        return Ok(handle);
    };
    let Some(value) = args.get(1) else {
        return Ok(handle);
    };
    let key = MapKey::from_value(value);
    SET_REGISTRY.with(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            let _ = s.insert(key);
        }
    });
    // Return the handle so the VM writeback-move is idempotent.
    Ok(handle)
}

fn builtin_set_remove(args: &[Value]) -> RuntimeResult<Value> {
    let handle = args.first().cloned().unwrap_or(Value::Unit);
    let Some(id) = set_id_of(&handle) else {
        return Ok(handle);
    };
    let Some(value) = args.get(1) else {
        return Ok(handle);
    };
    let key = MapKey::from_value(value);
    SET_REGISTRY.with(|r| {
        if let Some(s) = r.borrow_mut().get_mut(&id) {
            let _ = s.remove(&key);
        }
    });
    // Return the handle so the VM writeback-move is idempotent.
    Ok(handle)
}

fn builtin_set_contains(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Bool(false));
    };
    let Some(value) = args.get(1) else {
        return Ok(Value::Bool(false));
    };
    let key = MapKey::from_value(value);
    let has = SET_REGISTRY.with(|r| r.borrow().get(&id).is_some_and(|s| s.contains(&key)));
    Ok(Value::Bool(has))
}

fn builtin_set_len(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Int(0));
    };
    let n = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map_or(0, std::collections::HashSet::len)
    });
    Ok(Value::Int(n as i64))
}

fn builtin_set_is_empty(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Bool(true));
    };
    let empty = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .is_none_or(std::collections::HashSet::is_empty)
    });
    Ok(Value::Bool(empty))
}

fn builtin_set_clear(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(id) = args.first().and_then(set_id_of) {
        SET_REGISTRY.with(|r| {
            if let Some(set) = r.borrow_mut().get_mut(&id) {
                set.clear();
            }
        });
    }
    // Return the receiver so the VM writeback preserves the struct handle.
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

fn builtin_set_to_vec(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(set_id_of) else {
        return Ok(Value::Array(Arc::new(Vec::new())));
    };
    let values: Vec<Value> = SET_REGISTRY.with(|r| {
        r.borrow()
            .get(&id)
            .map(|s| s.iter().map(MapKey::to_value).collect())
            .unwrap_or_default()
    });
    Ok(Value::Array(Arc::new(values)))
}

// ----------------------------------------------------------------------
// sync extras (atomics + mutex + once)

use std::sync::atomic::{AtomicBool as StdAtomicBool, AtomicI64 as StdAtomicI64, Ordering};

thread_local! {
    static NEXT_ATOMIC_ID: RefCell<i64> = const { RefCell::new(1) };
    #[allow(clippy::missing_const_for_thread_local)]
    static ATOMIC_I64_REGISTRY: RefCell<StdHashMap<i64, Arc<StdAtomicI64>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    static ATOMIC_BOOL_REGISTRY: RefCell<StdHashMap<i64, Arc<StdAtomicBool>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    static MUTEX_REGISTRY: RefCell<StdHashMap<i64, Arc<parking_lot::Mutex<Value>>>> =
        RefCell::new(StdHashMap::new());
    #[allow(clippy::missing_const_for_thread_local)]
    static ONCE_REGISTRY: RefCell<StdHashMap<i64, Arc<parking_lot::Once>>> =
        RefCell::new(StdHashMap::new());
}

fn next_atomic_id() -> i64 {
    NEXT_ATOMIC_ID.with(|c| {
        let mut v = c.borrow_mut();
        let id = *v;
        *v += 1;
        id
    })
}

fn atomic_handle(name: &'static str, id: i64) -> Value {
    Value::struct_(
        name,
        Arc::new(vec![(Ident::new("__atomic"), Value::Int(id))]),
    )
}

fn atomic_id_of(value: &Value, expected: &str) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == expected {
            for (i, v) in inner.fields.iter() {
                if i.name == "__atomic" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn install_sync_extras(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("AtomicI64::new", builtin_atomic_i64_new),
        ("AtomicI64::load", builtin_atomic_i64_load),
        ("AtomicI64::store", builtin_atomic_i64_store),
        ("AtomicI64::fetch_add", builtin_atomic_i64_fetch_add),
        ("AtomicI64::fetch_sub", builtin_atomic_i64_fetch_sub),
        ("AtomicI64::compare_and_swap", builtin_atomic_i64_cas),
        ("AtomicI32::new", builtin_atomic_i64_new),
        ("AtomicI32::load", builtin_atomic_i64_load),
        ("AtomicI32::store", builtin_atomic_i64_store),
        ("AtomicI32::fetch_add", builtin_atomic_i64_fetch_add),
        ("AtomicBool::new", builtin_atomic_bool_new),
        ("AtomicBool::load", builtin_atomic_bool_load),
        ("AtomicBool::store", builtin_atomic_bool_store),
        ("AtomicBool::compare_and_swap", builtin_atomic_bool_cas),
        ("Mutex::new", builtin_mutex_new),
        ("Mutex::lock", builtin_mutex_lock),
        ("Mutex::unlock", builtin_mutex_unlock),
        ("Mutex::store", builtin_mutex_store),
        ("Once::new", builtin_once_new),
        ("Once::call", builtin_once_call),
    ];
    for (name, call) in entries {
        let qualified: &'static str = Box::leak(format!("sync::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn builtin_atomic_i64_new(args: &[Value]) -> RuntimeResult<Value> {
    let init = args.first().and_then(value_to_int).unwrap_or(0);
    let id = next_atomic_id();
    ATOMIC_I64_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, Arc::new(StdAtomicI64::new(init)));
    });
    Ok(atomic_handle("sync::AtomicI64", id))
}

fn with_atomic_i64<R>(value: &Value, f: impl FnOnce(&Arc<StdAtomicI64>) -> R) -> Option<R> {
    let id = atomic_id_of(value, "sync::AtomicI64")?;
    ATOMIC_I64_REGISTRY.with(|r| r.borrow().get(&id).map(f))
}

fn builtin_atomic_i64_load(args: &[Value]) -> RuntimeResult<Value> {
    let n = args
        .first()
        .and_then(|v| with_atomic_i64(v, |a| a.load(Ordering::SeqCst)))
        .unwrap_or(0);
    Ok(Value::Int(n))
}

fn builtin_atomic_i64_store(args: &[Value]) -> RuntimeResult<Value> {
    let val = args.get(1).and_then(value_to_int).unwrap_or(0);
    if let Some(handle) = args.first() {
        let _ = with_atomic_i64(handle, |a| a.store(val, Ordering::SeqCst));
    }
    Ok(Value::Unit)
}

fn builtin_atomic_i64_fetch_add(args: &[Value]) -> RuntimeResult<Value> {
    let delta = args.get(1).and_then(value_to_int).unwrap_or(0);
    let prev = args
        .first()
        .and_then(|v| with_atomic_i64(v, |a| a.fetch_add(delta, Ordering::SeqCst)))
        .unwrap_or(0);
    Ok(Value::Int(prev))
}

fn builtin_atomic_i64_fetch_sub(args: &[Value]) -> RuntimeResult<Value> {
    let delta = args.get(1).and_then(value_to_int).unwrap_or(0);
    let prev = args
        .first()
        .and_then(|v| with_atomic_i64(v, |a| a.fetch_sub(delta, Ordering::SeqCst)))
        .unwrap_or(0);
    Ok(Value::Int(prev))
}

fn builtin_atomic_i64_cas(args: &[Value]) -> RuntimeResult<Value> {
    let current = args.get(1).and_then(value_to_int).unwrap_or(0);
    let new = args.get(2).and_then(value_to_int).unwrap_or(0);
    let ok = args
        .first()
        .and_then(|v| {
            with_atomic_i64(v, |a| {
                a.compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            })
        })
        .unwrap_or(false);
    Ok(Value::Bool(ok))
}

fn builtin_atomic_bool_new(args: &[Value]) -> RuntimeResult<Value> {
    let init = matches!(args.first(), Some(Value::Bool(true)));
    let id = next_atomic_id();
    ATOMIC_BOOL_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(StdAtomicBool::new(init)));
    });
    Ok(atomic_handle("sync::AtomicBool", id))
}

fn with_atomic_bool<R>(value: &Value, f: impl FnOnce(&Arc<StdAtomicBool>) -> R) -> Option<R> {
    let id = atomic_id_of(value, "sync::AtomicBool")?;
    ATOMIC_BOOL_REGISTRY.with(|r| r.borrow().get(&id).map(f))
}

fn builtin_atomic_bool_load(args: &[Value]) -> RuntimeResult<Value> {
    let v = args
        .first()
        .and_then(|v| with_atomic_bool(v, |a| a.load(Ordering::SeqCst)))
        .unwrap_or(false);
    Ok(Value::Bool(v))
}

fn builtin_atomic_bool_store(args: &[Value]) -> RuntimeResult<Value> {
    let val = matches!(args.get(1), Some(Value::Bool(true)));
    if let Some(handle) = args.first() {
        let _ = with_atomic_bool(handle, |a| a.store(val, Ordering::SeqCst));
    }
    Ok(Value::Unit)
}

fn builtin_atomic_bool_cas(args: &[Value]) -> RuntimeResult<Value> {
    let current = matches!(args.get(1), Some(Value::Bool(true)));
    let new = matches!(args.get(2), Some(Value::Bool(true)));
    let ok = args
        .first()
        .and_then(|v| {
            with_atomic_bool(v, |a| {
                a.compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            })
        })
        .unwrap_or(false);
    Ok(Value::Bool(ok))
}

fn builtin_mutex_new(args: &[Value]) -> RuntimeResult<Value> {
    let init = args.first().cloned().unwrap_or(Value::Unit);
    let id = next_atomic_id();
    MUTEX_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(parking_lot::Mutex::new(init)));
    });
    Ok(Value::struct_(
        "sync::Mutex",
        Arc::new(vec![(Ident::new("__mutex"), Value::Int(id))]),
    ))
}

fn mutex_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        if inner.name == "sync::Mutex" {
            for (i, v) in inner.fields.iter() {
                if i.name == "__mutex" {
                    if let Value::Int(n) = v {
                        return Some(*n);
                    }
                }
            }
        }
    }
    None
}

fn builtin_mutex_lock(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(mutex_id_of) else {
        return Ok(Value::Unit);
    };
    let arc = MUTEX_REGISTRY.with(|r| r.borrow().get(&id).cloned());
    match arc {
        Some(m) => {
            let guard = m.lock();
            Ok(guard.clone())
        }
        None => Ok(Value::Unit),
    }
}

fn builtin_mutex_unlock(_args: &[Value]) -> RuntimeResult<Value> {
    // The VM's `lock()` acquires and releases atomically (the
    // parking_lot guard is dropped before the builtin returns), so
    // the lock is never held across Gossamer code. `unlock()` is
    // therefore a no-op in the interpreted tier.
    Ok(Value::Unit)
}

fn builtin_mutex_store(args: &[Value]) -> RuntimeResult<Value> {
    let Some(id) = args.first().and_then(mutex_id_of) else {
        return Ok(Value::Unit);
    };
    let new_val = args.get(1).cloned().unwrap_or(Value::Unit);
    let arc = MUTEX_REGISTRY.with(|r| r.borrow().get(&id).cloned());
    if let Some(m) = arc {
        *m.lock() = new_val;
    }
    Ok(Value::Unit)
}

fn builtin_once_new(_args: &[Value]) -> RuntimeResult<Value> {
    let id = next_atomic_id();
    ONCE_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(parking_lot::Once::new()));
    });
    Ok(Value::struct_(
        "sync::Once",
        Arc::new(vec![(Ident::new("__once"), Value::Int(id))]),
    ))
}

fn builtin_once_call(args: &[Value]) -> RuntimeResult<Value> {
    let id = match args.first() {
        Some(Value::Struct(inner)) if inner.name == "sync::Once" => inner
            .fields
            .iter()
            .find_map(|(i, v)| {
                if i.name == "__once" {
                    if let Value::Int(n) = v {
                        Some(*n)
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
            .unwrap_or(0),
        _ => return Ok(Value::Bool(false)),
    };
    let mut ran = false;
    let arc = ONCE_REGISTRY.with(|r| r.borrow().get(&id).cloned());
    if let Some(once) = arc {
        once.call_once(|| {
            ran = true;
        });
    }
    Ok(Value::Bool(ran))
}

// ----------------------------------------------------------------------
// math

fn install_math(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "math",
        &[
            ("abs", builtin_math_abs),
            ("sqrt", builtin_math_sqrt),
            ("cbrt", builtin_math_cbrt),
            ("floor", builtin_math_floor),
            ("ceil", builtin_math_ceil),
            ("round", builtin_math_round),
            ("trunc", builtin_math_trunc),
            ("sin", builtin_math_sin),
            ("cos", builtin_math_cos),
            ("tan", builtin_math_tan),
            ("asin", builtin_math_asin),
            ("acos", builtin_math_acos),
            ("atan", builtin_math_atan),
            ("atan2", builtin_math_atan2),
            ("sinh", builtin_math_sinh),
            ("cosh", builtin_math_cosh),
            ("tanh", builtin_math_tanh),
            ("exp", builtin_math_exp),
            ("exp2", builtin_math_exp2),
            ("ln", builtin_math_ln),
            ("log2", builtin_math_log2),
            ("log10", builtin_math_log10),
            ("log", builtin_math_log),
            ("pow", builtin_math_pow),
            ("hypot", builtin_math_hypot),
            ("min", builtin_math_min),
            ("max", builtin_math_max),
            ("min_f64", builtin_math_min_f64),
            ("max_f64", builtin_math_max_f64),
            ("min_i64", builtin_math_min_i64),
            ("max_i64", builtin_math_max_i64),
            ("abs_i64", builtin_math_abs_i64),
            ("fmod", builtin_math_fmod),
            ("mod_float", builtin_math_fmod),
            ("is_nan", builtin_math_is_nan),
            ("is_inf", builtin_math_is_inf),
            ("nan", builtin_math_nan),
            ("inf", builtin_math_inf),
            ("copysign", builtin_math_copysign),
            ("dim", builtin_math_dim),
        ],
        globals,
    );
    // Expose constants as float globals.
    for (name, val) in [
        ("math::PI", math_std::PI),
        ("math::E", math_std::E),
        ("math::SQRT_2", math_std::SQRT_2),
        ("math::LN_2", math_std::LN_2),
        ("math::LN_10", math_std::LN_10),
        ("math::LOG2_E", math_std::LOG2_E),
        ("math::LOG10_E", math_std::LOG10_E),
        ("math::PHI", math_std::PHI),
        ("math::MAX_F64", math_std::MAX_F64),
        ("math::MIN_POSITIVE_F64", math_std::MIN_POSITIVE_F64),
        ("math::INF", math_std::INF),
        ("math::NEG_INF", math_std::NEG_INF),
    ] {
        let leaked: &'static str = Box::leak(name.to_string().into_boxed_str());
        globals.push((leaked, Value::Float(val)));
    }
}

fn arg_f64(args: &[Value], idx: usize) -> f64 {
    match args.get(idx) {
        Some(Value::Float(f)) => *f,
        Some(Value::Int(n)) => *n as f64,
        _ => 0.0,
    }
}

fn builtin_math_abs(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::abs(arg_f64(args, 0))))
}
fn builtin_math_sqrt(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::sqrt(arg_f64(args, 0))))
}
fn builtin_math_cbrt(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::cbrt(arg_f64(args, 0))))
}
fn builtin_math_floor(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::floor(arg_f64(args, 0))))
}
fn builtin_math_ceil(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::ceil(arg_f64(args, 0))))
}
fn builtin_math_round(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::round(arg_f64(args, 0))))
}
fn builtin_math_trunc(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::trunc(arg_f64(args, 0))))
}
fn builtin_math_sin(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::sin(arg_f64(args, 0))))
}
fn builtin_math_cos(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::cos(arg_f64(args, 0))))
}
fn builtin_math_tan(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::tan(arg_f64(args, 0))))
}
fn builtin_math_asin(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::asin(arg_f64(args, 0))))
}
fn builtin_math_acos(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::acos(arg_f64(args, 0))))
}
fn builtin_math_atan(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::atan(arg_f64(args, 0))))
}
fn builtin_math_atan2(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::atan2(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_sinh(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::sinh(arg_f64(args, 0))))
}
fn builtin_math_cosh(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::cosh(arg_f64(args, 0))))
}
fn builtin_math_tanh(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::tanh(arg_f64(args, 0))))
}
fn builtin_math_exp(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::exp(arg_f64(args, 0))))
}
fn builtin_math_exp2(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::exp2(arg_f64(args, 0))))
}
fn builtin_math_ln(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::ln(arg_f64(args, 0))))
}
fn builtin_math_log2(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::log2(arg_f64(args, 0))))
}
fn builtin_math_log10(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::log10(arg_f64(args, 0))))
}
fn builtin_math_log(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::log(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_pow(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::pow(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_hypot(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::hypot(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_min(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Float(_)) = args.first() {
        return Ok(Value::Float(math_std::min_f64(
            arg_f64(args, 0),
            arg_f64(args, 1),
        )));
    }
    let x = args.first().and_then(value_to_int).unwrap_or(0);
    let y = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::min_i64(x, y)))
}
fn builtin_math_max(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(Value::Float(_)) = args.first() {
        return Ok(Value::Float(math_std::max_f64(
            arg_f64(args, 0),
            arg_f64(args, 1),
        )));
    }
    let x = args.first().and_then(value_to_int).unwrap_or(0);
    let y = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::max_i64(x, y)))
}
fn builtin_math_min_f64(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::min_f64(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_max_f64(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::max_f64(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_min_i64(args: &[Value]) -> RuntimeResult<Value> {
    let x = args.first().and_then(value_to_int).unwrap_or(0);
    let y = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::min_i64(x, y)))
}
fn builtin_math_max_i64(args: &[Value]) -> RuntimeResult<Value> {
    let x = args.first().and_then(value_to_int).unwrap_or(0);
    let y = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::max_i64(x, y)))
}
fn builtin_math_abs_i64(args: &[Value]) -> RuntimeResult<Value> {
    let x = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::abs_i64(x)))
}
fn builtin_math_fmod(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::fmod(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_is_nan(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(math_std::is_nan(arg_f64(args, 0))))
}
fn builtin_math_is_inf(args: &[Value]) -> RuntimeResult<Value> {
    let x = arg_f64(args, 0);
    let sign = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Bool(math_std::is_inf(x, sign)))
}
fn builtin_math_nan(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::nan()))
}
fn builtin_math_inf(args: &[Value]) -> RuntimeResult<Value> {
    let sign = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::Float(math_std::inf(sign)))
}
fn builtin_math_copysign(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::copysign(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}
fn builtin_math_dim(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Float(math_std::dim(
        arg_f64(args, 0),
        arg_f64(args, 1),
    )))
}

// ----------------------------------------------------------------------
// math::bits

fn arg_u64(args: &[Value], idx: usize) -> u64 {
    match args.get(idx) {
        Some(Value::Int(n)) => *n as u64,
        _ => 0,
    }
}

fn install_math_bits(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified names; bare `len`, `add`, `sub`, `mul`, `div`
    // would shadow built-in array/string methods and arithmetic operators.
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("count_ones", builtin_bits_count_ones),
        ("count_zeros", builtin_bits_count_zeros),
        ("leading_zeros", builtin_bits_leading_zeros),
        ("trailing_zeros", builtin_bits_trailing_zeros),
        ("rotate_left", builtin_bits_rotate_left),
        ("rotate_right", builtin_bits_rotate_right),
        ("reverse_bits", builtin_bits_reverse_bits),
        ("reverse_bytes", builtin_bits_reverse_bytes),
        ("len", builtin_bits_len),
        ("add", builtin_bits_add),
        ("sub", builtin_bits_sub),
        ("mul", builtin_bits_mul),
        ("div", builtin_bits_div),
    ];
    for (short, call) in entries {
        let qualified: &'static str = Box::leak(format!("math::bits::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }
}

fn builtin_bits_count_ones(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::count_ones(arg_u64(
        args, 0,
    )))))
}
fn builtin_bits_count_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::count_zeros(arg_u64(
        args, 0,
    )))))
}
fn builtin_bits_leading_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::leading_zeros(
        arg_u64(args, 0),
    ))))
}
fn builtin_bits_trailing_zeros(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::trailing_zeros(
        arg_u64(args, 0),
    ))))
}
fn builtin_bits_rotate_left(args: &[Value]) -> RuntimeResult<Value> {
    let x = arg_u64(args, 0);
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::bits::rotate_left(x, n) as i64))
}
fn builtin_bits_rotate_right(args: &[Value]) -> RuntimeResult<Value> {
    let x = arg_u64(args, 0);
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    Ok(Value::Int(math_std::bits::rotate_right(x, n) as i64))
}
fn builtin_bits_reverse_bits(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        math_std::bits::reverse_bits(arg_u64(args, 0)) as i64
    ))
}
fn builtin_bits_reverse_bytes(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(
        math_std::bits::reverse_bytes(arg_u64(args, 0)) as i64
    ))
}
fn builtin_bits_len(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(i64::from(math_std::bits::len(arg_u64(args, 0)))))
}
fn builtin_bits_add(args: &[Value]) -> RuntimeResult<Value> {
    let (sum, carry) = math_std::bits::add(arg_u64(args, 0), arg_u64(args, 1), arg_u64(args, 2));
    Ok(Value::Tuple(Arc::new(vec![
        Value::Int(sum as i64),
        Value::Int(carry as i64),
    ])))
}
fn builtin_bits_sub(args: &[Value]) -> RuntimeResult<Value> {
    let (diff, borrow) = math_std::bits::sub(arg_u64(args, 0), arg_u64(args, 1), arg_u64(args, 2));
    Ok(Value::Tuple(Arc::new(vec![
        Value::Int(diff as i64),
        Value::Int(borrow as i64),
    ])))
}
fn builtin_bits_mul(args: &[Value]) -> RuntimeResult<Value> {
    let (hi, lo) = math_std::bits::mul(arg_u64(args, 0), arg_u64(args, 1));
    Ok(Value::Tuple(Arc::new(vec![
        Value::Int(hi as i64),
        Value::Int(lo as i64),
    ])))
}
fn builtin_bits_div(args: &[Value]) -> RuntimeResult<Value> {
    let y = arg_u64(args, 2);
    if y == 0 {
        return Ok(err_variant("math::bits::div: division by zero".to_string()));
    }
    let (q, r) = math_std::bits::div(arg_u64(args, 0), arg_u64(args, 1), y);
    Ok(Value::Tuple(Arc::new(vec![
        Value::Int(q as i64),
        Value::Int(r as i64),
    ])))
}

// ----------------------------------------------------------------------
// unicode

fn arg_char(args: &[Value], idx: usize) -> char {
    match args.get(idx) {
        Some(Value::Char(c)) => *c,
        Some(Value::String(s)) => s.as_str().chars().next().unwrap_or('\0'),
        Some(Value::Int(n)) => char::from_u32(*n as u32).unwrap_or('\0'),
        _ => '\0',
    }
}

fn install_unicode(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "unicode",
        &[
            ("is_letter", builtin_unicode_is_letter),
            ("is_digit", builtin_unicode_is_digit),
            ("is_number", builtin_unicode_is_number),
            ("is_space", builtin_unicode_is_space),
            ("is_upper", builtin_unicode_is_upper),
            ("is_lower", builtin_unicode_is_lower),
            ("is_title", builtin_unicode_is_title),
            ("is_punct", builtin_unicode_is_punct),
            ("is_symbol", builtin_unicode_is_symbol),
            ("is_mark", builtin_unicode_is_mark),
            ("is_print", builtin_unicode_is_print),
            ("is_graphic", builtin_unicode_is_graphic),
            ("is_control", builtin_unicode_is_control),
            ("to_upper", builtin_unicode_to_upper),
            ("to_lower", builtin_unicode_to_lower),
            ("to_title", builtin_unicode_to_title),
            ("simple_fold", builtin_unicode_simple_fold),
        ],
        globals,
    );
}

fn builtin_unicode_is_letter(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_letter(arg_char(args, 0))))
}
fn builtin_unicode_is_digit(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_digit(arg_char(args, 0))))
}
fn builtin_unicode_is_number(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_number(arg_char(args, 0))))
}
fn builtin_unicode_is_space(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_space(arg_char(args, 0))))
}
fn builtin_unicode_is_upper(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_upper(arg_char(args, 0))))
}
fn builtin_unicode_is_lower(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_lower(arg_char(args, 0))))
}
fn builtin_unicode_is_title(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_title(arg_char(args, 0))))
}
fn builtin_unicode_is_punct(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_punct(arg_char(args, 0))))
}
fn builtin_unicode_is_symbol(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_symbol(arg_char(args, 0))))
}
fn builtin_unicode_is_mark(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_mark(arg_char(args, 0))))
}
fn builtin_unicode_is_print(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_print(arg_char(args, 0))))
}
fn builtin_unicode_is_graphic(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_graphic(arg_char(args, 0))))
}
fn builtin_unicode_is_control(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(unicode_std::is_control(arg_char(args, 0))))
}
fn builtin_unicode_to_upper(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Char(unicode_std::to_upper(arg_char(args, 0))))
}
fn builtin_unicode_to_lower(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Char(unicode_std::to_lower(arg_char(args, 0))))
}
fn builtin_unicode_to_title(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Char(unicode_std::to_title(arg_char(args, 0))))
}
fn builtin_unicode_simple_fold(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Char(unicode_std::simple_fold(arg_char(args, 0))))
}

// ----------------------------------------------------------------------
// encoding::binary (extended)

fn install_encoding_binary(globals: &mut Vec<(&'static str, Value)>) {
    use gossamer_std::encoding::binary as bin;

    install_module_pub(
        "encoding::binary",
        &[
            ("put_u16_be", builtin_bin_put_u16_be),
            ("put_u32_be", builtin_bin_put_u32_be),
            ("get_u16_be", builtin_bin_get_u16_be),
            ("get_u16_le", builtin_bin_get_u16_le),
            ("get_u32_be", builtin_bin_get_u32_be),
            ("get_u32_le", builtin_bin_get_u32_le),
            ("put_u16_le", builtin_bin_put_u16_le),
            ("put_u32_le", builtin_bin_put_u32_le),
            ("get_u64_be", builtin_bin_get_u64_be),
            ("put_u64_be", builtin_bin_put_u64_be),
            ("get_u64_le", builtin_bin_get_u64_le),
            ("put_u64_le", builtin_bin_put_u64_le),
            ("uvarint", builtin_bin_uvarint),
            ("varint", builtin_bin_varint),
        ],
        globals,
    );

    // Register bare names too for backward compat.
    for (name, f) in &[
        ("put_u16_be", builtin_bin_put_u16_be as BuiltinFnPub),
        ("put_u32_be", builtin_bin_put_u32_be as BuiltinFnPub),
    ] {
        globals.push((*name, crate::builtins::builtin_pub(name, *f)));
    }

    // Suppress unused warning — the module `bin` is only used for its
    // associated functions, which we call through the function pointers below.
    let _ = bin::get_u8;
}

fn bytes_from_value(v: &Value) -> Vec<u8> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|elem| match elem {
                Value::Int(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        // Fast-path for typed integer arrays produced by literal [n, ...] with i64 elements.
        Value::IntArray(arr) => arr.iter().filter_map(|&n| u8::try_from(n).ok()).collect(),
        Value::String(s) => s.as_str().as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

fn builtin_bin_put_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_put_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_put_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_put_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_put_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_put_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
fn builtin_bin_get_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_be(&bytes),
    ))))
}
fn builtin_bin_get_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_le(&bytes),
    ))))
}
fn builtin_bin_get_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_be(&bytes),
    ))))
}
fn builtin_bin_get_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_le(&bytes),
    ))))
}
fn builtin_bin_get_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_be(&bytes) as i64,
    )))
}
fn builtin_bin_get_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_le(&bytes) as i64,
    )))
}
fn builtin_bin_uvarint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::uvarint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            Value::Int(v as i64),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}
fn builtin_bin_varint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::varint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            Value::Int(v),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::csv

fn install_encoding_csv(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "encoding::csv",
        &[
            ("read", builtin_csv_read),
            ("parse_line", builtin_csv_parse_line),
            ("write", builtin_csv_write),
        ],
        globals,
    );
}

fn builtin_csv_read(args: &[Value]) -> RuntimeResult<Value> {
    let input = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::encoding::csv::read(input) {
        Ok(records) => {
            let rows: Vec<Value> = records
                .into_iter()
                .map(|row| {
                    Value::Array(Arc::new(
                        row.into_iter().map(|f| Value::String(f.into())).collect(),
                    ))
                })
                .collect();
            Ok(ok_variant(Value::Array(Arc::new(rows))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_csv_parse_line(args: &[Value]) -> RuntimeResult<Value> {
    let line = args.first().and_then(as_str).unwrap_or("");
    let fields = gossamer_std::encoding::csv::parse_line(line);
    Ok(Value::Array(Arc::new(
        fields
            .into_iter()
            .map(|f| Value::String(f.into()))
            .collect(),
    )))
}

fn builtin_csv_write(args: &[Value]) -> RuntimeResult<Value> {
    let records = match args.first() {
        Some(Value::Array(outer)) => outer
            .iter()
            .filter_map(|row| match row {
                Value::Array(inner) => Some(
                    inner
                        .iter()
                        .map(|v| match v {
                            Value::String(s) => s.as_str().to_string(),
                            other => format!("{other}"),
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    Ok(Value::String(
        gossamer_std::encoding::csv::write(&records).into(),
    ))
}

// ----------------------------------------------------------------------
// encoding::pem

fn install_encoding_pem(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "encoding::pem",
        &[
            ("encode", builtin_pem_encode),
            ("decode", builtin_pem_decode),
            ("decode_all", builtin_pem_decode_all),
        ],
        globals,
    );
}

fn pem_block_to_value(block: gossamer_std::encoding::pem::Block) -> Value {
    let bytes_val = Value::Array(Arc::new(
        block
            .bytes
            .into_iter()
            .map(|b| Value::Int(i64::from(b)))
            .collect(),
    ));
    Value::struct_(
        "encoding::pem::Block",
        Arc::new(vec![
            (
                Ident::new("block_type"),
                Value::String(block.block_type.into()),
            ),
            (Ident::new("bytes"), bytes_val),
        ]),
    )
}

fn pem_block_from_value(v: &Value) -> gossamer_std::encoding::pem::Block {
    let (mut block_type, mut bytes) = (String::new(), Vec::new());
    if let Value::Struct(s) = v {
        for (k, val) in s.fields.iter() {
            match k.name.as_str() {
                "block_type" => {
                    if let Value::String(s) = val {
                        block_type = s.as_str().to_string();
                    }
                }
                "bytes" => {
                    if let Value::Array(arr) = val {
                        bytes = arr
                            .iter()
                            .filter_map(|e| match e {
                                Value::Int(n) => u8::try_from(*n).ok(),
                                _ => None,
                            })
                            .collect();
                    }
                }
                _ => {}
            }
        }
    }
    gossamer_std::encoding::pem::Block { block_type, bytes }
}

fn builtin_pem_encode(args: &[Value]) -> RuntimeResult<Value> {
    let block = pem_block_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::pem::encode(&block).into(),
    ))
}

fn builtin_pem_decode(args: &[Value]) -> RuntimeResult<Value> {
    let input = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::encoding::pem::decode(input) {
        Ok((block, _rest)) => Ok(ok_variant(pem_block_to_value(block))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_pem_decode_all(args: &[Value]) -> RuntimeResult<Value> {
    let input = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::encoding::pem::decode_all(input) {
        Ok(blocks) => {
            let values: Vec<Value> = blocks.into_iter().map(pem_block_to_value).collect();
            Ok(ok_variant(Value::Array(Arc::new(values))))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// utf16

fn install_utf16(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified `utf16::*` names to avoid shadowing built-in
    // method dispatch (e.g. `rune_len` conflicts with utf8::rune_len).
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("is_surrogate", builtin_utf16_is_surrogate),
        ("rune_len", builtin_utf16_rune_len),
        ("decode_surrogate_pair", builtin_utf16_decode_surrogate_pair),
        ("encode_string", builtin_utf16_encode_string),
        ("decode_to_string", builtin_utf16_decode_to_string),
    ];
    for (short, call) in entries {
        let qualified: &'static str = Box::leak(format!("utf16::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }
}

fn builtin_utf16_is_surrogate(args: &[Value]) -> RuntimeResult<Value> {
    let r = args.first().and_then(value_to_int).unwrap_or(0);
    Ok(Value::Bool(utf16_std::is_surrogate(r as u16)))
}

fn builtin_utf16_rune_len(args: &[Value]) -> RuntimeResult<Value> {
    let ch = arg_char(args, 0);
    Ok(Value::Int(utf16_std::rune_len(ch) as i64))
}

fn builtin_utf16_decode_surrogate_pair(args: &[Value]) -> RuntimeResult<Value> {
    let high = args.first().and_then(value_to_int).unwrap_or(0) as u16;
    let low = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    match utf16_std::decode_surrogate_pair(high, low) {
        Some(ch) => Ok(some_variant(Value::Char(ch))),
        None => Ok(none_variant()),
    }
}

fn builtin_utf16_encode_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    let units = utf16_std::encode_string(s);
    Ok(Value::Array(Arc::new(
        units
            .into_iter()
            .map(|u| Value::Int(i64::from(u)))
            .collect(),
    )))
}

fn builtin_utf16_decode_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let units: Vec<u16> = match args.first() {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| value_to_int(v).map(|n| n as u16))
            .collect(),
        _ => Vec::new(),
    };
    Ok(Value::String(utf16_std::decode_to_string(&units).into()))
}

// ----------------------------------------------------------------------
// iter

/// Helper to extract elements from a `Value::Array` / `Value::IntArray`.
fn collect_array(v: &Value) -> Vec<Value> {
    match v {
        Value::Array(arr) => arr.as_ref().clone(),
        Value::IntArray(arr) => arr.iter().map(|&n| Value::Int(n)).collect(),
        Value::FloatVec(arr) => arr.iter().map(|&f| Value::Float(f)).collect(),
        _ => Vec::new(),
    }
}

fn install_iter(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified `iter::*` names to avoid shadowing built-in
    // method dispatch (Option::map, Result::filter, Vec::any, etc.).
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
    ];
    for (short, call) in static_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
    }

    // Closure-taking functions — must be `native` to access the interpreter.
    let native_entries: &[(&str, NativeCall)] = &[
        ("map", native_iter_map),
        ("filter", native_iter_filter),
        ("fold", native_iter_fold),
        ("any", native_iter_any),
        ("all", native_iter_all),
        ("flat_map", native_iter_flat_map),
    ];
    for (short, call) in native_entries {
        let qualified: &'static str = Box::leak(format!("iter::{short}").into_boxed_str());
        globals.push((qualified, Value::native(qualified, *call)));
    }
}

fn builtin_iter_count(args: &[Value]) -> RuntimeResult<Value> {
    let n = match args.first() {
        Some(Value::Array(arr)) => arr.len(),
        Some(Value::IntArray(arr)) => arr.len(),
        Some(Value::FloatVec(arr)) => arr.len(),
        _ => 0,
    };
    Ok(Value::Int(n as i64))
}

fn builtin_iter_take(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let taken = iter_std::take(&xs, n);
    Ok(Value::Array(Arc::new(taken)))
}

fn builtin_iter_skip(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let n = args.get(1).and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    let rest = iter_std::skip(&xs, n);
    Ok(Value::Array(Arc::new(rest)))
}

fn builtin_iter_zip(args: &[Value]) -> RuntimeResult<Value> {
    let a = collect_array(args.first().unwrap_or(&Value::Unit));
    let b = collect_array(args.get(1).unwrap_or(&Value::Unit));
    let zipped: Vec<Value> = a
        .into_iter()
        .zip(b)
        .map(|(x, y)| Value::Tuple(Arc::new(vec![x, y])))
        .collect();
    Ok(Value::Array(Arc::new(zipped)))
}

fn builtin_iter_enumerate(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let enumerated: Vec<Value> = xs
        .into_iter()
        .enumerate()
        .map(|(i, x)| Value::Tuple(Arc::new(vec![Value::Int(i as i64), x])))
        .collect();
    Ok(Value::Array(Arc::new(enumerated)))
}

fn builtin_iter_chain(args: &[Value]) -> RuntimeResult<Value> {
    let mut result = collect_array(args.first().unwrap_or(&Value::Unit));
    result.extend(collect_array(args.get(1).unwrap_or(&Value::Unit)));
    Ok(Value::Array(Arc::new(result)))
}

fn builtin_iter_flatten(args: &[Value]) -> RuntimeResult<Value> {
    let outer = collect_array(args.first().unwrap_or(&Value::Unit));
    let flat: Vec<Value> = outer.into_iter().flat_map(|v| collect_array(&v)).collect();
    Ok(Value::Array(Arc::new(flat)))
}

fn builtin_iter_reversed(args: &[Value]) -> RuntimeResult<Value> {
    let mut xs = collect_array(args.first().unwrap_or(&Value::Unit));
    xs.reverse();
    Ok(Value::Array(Arc::new(xs)))
}

fn builtin_iter_dedup(args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut out: Vec<Value> = Vec::new();
    for x in xs {
        if out.last().is_none_or(|last| !values_equal(last, &x)) {
            out.push(x);
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

fn values_equal(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x.as_str() == y.as_str(),
        _ => false,
    }
}

fn builtin_iter_sum(args: &[Value]) -> RuntimeResult<Value> {
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

fn native_iter_map(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut out = Vec::with_capacity(xs.len());
    for x in xs {
        out.push(dispatch.call_value(&f, vec![x])?);
    }
    Ok(Value::Array(Arc::new(out)))
}

fn native_iter_filter(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut out = Vec::new();
    for x in xs {
        if let Value::Bool(true) = dispatch.call_value(&f, vec![x.clone()])? {
            out.push(x);
        }
    }
    Ok(Value::Array(Arc::new(out)))
}

fn native_iter_fold(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let mut acc = args.get(1).cloned().unwrap_or(Value::Unit);
    let f = args.get(2).cloned().unwrap_or(Value::Unit);
    for x in xs {
        acc = dispatch.call_value(&f, vec![acc, x])?;
    }
    Ok(acc)
}

fn native_iter_any(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    for x in xs {
        if matches!(dispatch.call_value(&f, vec![x])?, Value::Bool(true)) {
            return Ok(Value::Bool(true));
        }
    }
    Ok(Value::Bool(false))
}

fn native_iter_all(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    for x in xs {
        if !matches!(dispatch.call_value(&f, vec![x])?, Value::Bool(true)) {
            return Ok(Value::Bool(false));
        }
    }
    Ok(Value::Bool(true))
}

fn native_iter_flat_map(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let xs = collect_array(args.first().unwrap_or(&Value::Unit));
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut out = Vec::new();
    for x in xs {
        let result = dispatch.call_value(&f, vec![x])?;
        out.extend(collect_array(&result));
    }
    Ok(Value::Array(Arc::new(out)))
}

// ----------------------------------------------------------------------
// crypto (sha256, hmac, rand — always enabled in this crate)

fn install_crypto(globals: &mut Vec<(&'static str, Value)>) {
    // crypto::sha256
    for (short, call) in [
        ("digest", builtin_crypto_sha256_digest as BuiltinFnPub),
        ("hex", builtin_crypto_sha256_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::sha256::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // crypto::hmac
    {
        let q = "crypto::hmac::sha256_mac";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_hmac_sha256_mac),
        ));
    }
    // crypto::subtle
    {
        let q = "crypto::subtle::constant_time_eq";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_subtle_ct_eq),
        ));
    }
    // crypto::rand
    {
        let q = "crypto::rand::bytes";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_rand_bytes),
        ));
    }
}

fn bytes_to_value_array(b: &[u8]) -> Value {
    Value::Array(Arc::new(
        b.iter().map(|&x| Value::Int(i64::from(x))).collect(),
    ))
}

fn value_to_bytes(v: &Value) -> Vec<u8> {
    match v {
        Value::Array(arr) => arr
            .iter()
            .filter_map(|x| value_to_int(x).map(|n| n as u8))
            .collect(),
        Value::String(s) => s.as_str().as_bytes().to_vec(),
        _ => Vec::new(),
    }
}

fn builtin_crypto_sha256_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::sha256::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

fn builtin_crypto_sha256_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::sha256::hex(&input).into(),
    ))
}

fn builtin_crypto_hmac_sha256_mac(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let mac = gossamer_std::crypto::hmac::sha256_mac(&key, &msg);
    Ok(bytes_to_value_array(&mac))
}

fn builtin_crypto_subtle_ct_eq(args: &[Value]) -> RuntimeResult<Value> {
    let a = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let b = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Bool(gossamer_std::crypto::subtle::constant_time_eq(
        &a, &b,
    )))
}

fn builtin_crypto_rand_bytes(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    match gossamer_std::crypto::rand::bytes(n) {
        Ok(b) => Ok(ok_variant(bytes_to_value_array(&b))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::yaml (always enabled in this crate)

fn install_encoding_yaml(globals: &mut Vec<(&'static str, Value)>) {
    // Register only qualified names — bare `parse` and `encode` shadow
    // built-in string/value method dispatch.
    for (short, call) in [
        ("parse", builtin_yaml_parse as BuiltinFnPub),
        ("parse_all", builtin_yaml_parse_all),
        ("encode", builtin_yaml_encode),
    ] {
        let q: &'static str = Box::leak(format!("encoding::yaml::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn yaml_value_to_gossamer(v: gossamer_std::encoding::yaml::Value) -> Value {
    use gossamer_std::encoding::yaml::Value as YV;
    match v {
        YV::Null => Value::Unit,
        YV::Bool(b) => Value::Bool(b),
        YV::Int(n) => Value::Int(n),
        YV::Float(f) => Value::Float(f),
        YV::String(s) => Value::String(s.into()),
        YV::Seq(seq) => Value::Array(Arc::new(
            seq.into_iter().map(yaml_value_to_gossamer).collect(),
        )),
        YV::Map(pairs) => {
            let mut hmap = rustc_hash::FxHashMap::default();
            for (k, v) in pairs {
                let key = match k {
                    YV::String(s) => MapKey::Str(s.into()),
                    YV::Int(n) => MapKey::Int(n),
                    YV::Bool(b) => MapKey::Bool(b),
                    _ => MapKey::NonHashable,
                };
                hmap.insert(key, yaml_value_to_gossamer(v));
            }
            Value::Map(Arc::new(parking_lot::Mutex::new(hmap)))
        }
    }
}

fn gossamer_value_to_yaml(v: &Value) -> gossamer_std::encoding::yaml::Value {
    use gossamer_std::encoding::yaml::Value as YV;
    match v {
        Value::Unit => YV::Null,
        Value::Bool(b) => YV::Bool(*b),
        Value::Int(n) => YV::Int(*n),
        Value::Float(f) => YV::Float(*f),
        Value::String(s) => YV::String(s.as_str().to_string()),
        Value::Array(arr) => YV::Seq(arr.iter().map(gossamer_value_to_yaml).collect()),
        Value::Map(map) => {
            let guard = map.lock();
            let pairs: Vec<_> = guard
                .iter()
                .map(|(k, v)| {
                    let yk = match k {
                        MapKey::Str(s) => YV::String(s.to_string()),
                        MapKey::Int(n) => YV::Int(*n),
                        MapKey::Bool(b) => YV::Bool(*b),
                        _ => YV::Null,
                    };
                    (yk, gossamer_value_to_yaml(v))
                })
                .collect();
            YV::Map(pairs)
        }
        _ => YV::Null,
    }
}

fn builtin_yaml_parse(args: &[Value]) -> RuntimeResult<Value> {
    let src = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::encoding::yaml::parse(src) {
        Ok(v) => Ok(ok_variant(yaml_value_to_gossamer(v))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_yaml_parse_all(args: &[Value]) -> RuntimeResult<Value> {
    let src = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::encoding::yaml::parse_all(src) {
        Ok(vs) => {
            let arr = Value::Array(Arc::new(
                vs.into_iter().map(yaml_value_to_gossamer).collect(),
            ));
            Ok(ok_variant(arr))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_yaml_encode(args: &[Value]) -> RuntimeResult<Value> {
    let yv = gossamer_value_to_yaml(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::yaml::encode(&yv) {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// compress (gzip / flate / zlib)

fn bytes_to_array(v: Vec<u8>) -> Value {
    Value::Array(Arc::new(
        v.into_iter().map(|b| Value::Int(i64::from(b))).collect(),
    ))
}

fn install_compress(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("gzip::encode", builtin_compress_gzip_encode as BuiltinFnPub),
        ("gzip::decode", builtin_compress_gzip_decode),
        ("flate::compress", builtin_compress_flate_compress),
        ("flate::decompress", builtin_compress_flate_decompress),
        ("zlib::compress", builtin_compress_zlib_compress),
        ("zlib::decompress", builtin_compress_zlib_decompress),
    ] {
        let q: &'static str = Box::leak(format!("compress::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_compress_gzip_encode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    let lvl = gossamer_std::compress::gzip::Level::new(level.clamp(0, 9))
        .unwrap_or(gossamer_std::compress::gzip::Level::DEFAULT);
    match gossamer_std::compress::gzip::encode(&input, lvl) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_compress_gzip_decode(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::gzip::decode(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_compress_flate_compress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    match gossamer_std::compress::flate::compress(&input, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_compress_flate_decompress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::flate::decompress(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_compress_zlib_compress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    match gossamer_std::compress::zlib::compress(&input, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_compress_zlib_decompress(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::zlib::decompress(&input) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// hash::fnv

fn install_hash_fnv(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("fnv::hash64", builtin_hash_fnv_hash64 as BuiltinFnPub),
        ("fnv::hash32", builtin_hash_fnv_hash32),
        ("fnv::hash_string", builtin_hash_fnv_hash_string),
    ] {
        let q: &'static str = Box::leak(format!("hash::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_hash_fnv_hash64(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::Int(gossamer_std::hash::fnv::hash64(&input) as i64))
}

fn builtin_hash_fnv_hash32(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::Int(i64::from(gossamer_std::hash::fnv::hash32(
        &input,
    ))))
}

fn builtin_hash_fnv_hash_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Int(gossamer_std::hash::fnv::hash_string(s) as i64))
}

// ----------------------------------------------------------------------
// archive::zip

fn install_archive_zip(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("zip::read", builtin_archive_zip_read as BuiltinFnPub),
        ("zip::write", builtin_archive_zip_write),
    ] {
        let q: &'static str = Box::leak(format!("archive::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn zip_entry_to_value(entry: gossamer_std::archive::zip::ZipEntry) -> Value {
    Value::struct_(
        "archive::ZipEntry",
        Arc::new(vec![
            (Ident::new("name"), Value::String(entry.name.into())),
            (Ident::new("data"), bytes_to_array(entry.data)),
            (Ident::new("is_dir"), Value::Bool(entry.is_dir)),
        ]),
    )
}

fn builtin_archive_zip_read(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::archive::zip::read(&input) {
        Ok(entries) => {
            let arr = Value::Array(Arc::new(
                entries.into_iter().map(zip_entry_to_value).collect(),
            ));
            Ok(ok_variant(arr))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_archive_zip_write(args: &[Value]) -> RuntimeResult<Value> {
    // Expects an array of (name, data) tuples.
    let pairs = match args.first() {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Tuple(t) => {
                    let name = match t.first()? {
                        Value::String(s) => s.as_str().to_string(),
                        _ => return None,
                    };
                    let data = bytes_from_value(t.get(1)?);
                    Some((name, data))
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let refs: Vec<(&str, &[u8])> = pairs
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    match gossamer_std::archive::zip::write(&refs) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// archive::tar

fn install_archive_tar(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("tar::read", builtin_archive_tar_read as BuiltinFnPub),
        ("tar::write", builtin_archive_tar_write),
    ] {
        let q: &'static str = Box::leak(format!("archive::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn tar_entry_to_value(entry: gossamer_std::archive::tar::TarEntry) -> Value {
    Value::struct_(
        "archive::TarEntry",
        Arc::new(vec![
            (Ident::new("name"), Value::String(entry.name.into())),
            (Ident::new("data"), bytes_to_array(entry.data)),
            (Ident::new("is_dir"), Value::Bool(entry.is_dir)),
        ]),
    )
}

fn builtin_archive_tar_read(args: &[Value]) -> RuntimeResult<Value> {
    let input = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::archive::tar::read(&input) {
        Ok(entries) => {
            let arr = Value::Array(Arc::new(
                entries.into_iter().map(tar_entry_to_value).collect(),
            ));
            Ok(ok_variant(arr))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_archive_tar_write(args: &[Value]) -> RuntimeResult<Value> {
    let pairs = match args.first() {
        Some(Value::Array(arr)) => arr
            .iter()
            .filter_map(|v| match v {
                Value::Tuple(t) => {
                    let name = match t.first()? {
                        Value::String(s) => s.as_str().to_string(),
                        _ => return None,
                    };
                    let data = bytes_from_value(t.get(1)?);
                    Some((name, data))
                }
                _ => None,
            })
            .collect::<Vec<_>>(),
        _ => Vec::new(),
    };
    let refs: Vec<(&str, &[u8])> = pairs
        .iter()
        .map(|(n, d)| (n.as_str(), d.as_slice()))
        .collect();
    match gossamer_std::archive::tar::write(&refs) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// sync::AtomicU64

use std::sync::atomic::AtomicU64 as StdAtomicU64;

thread_local! {
    static ATOMIC_U64_REGISTRY: std::cell::RefCell<std::collections::HashMap<i64, Arc<StdAtomicU64>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
    static BARRIER_REGISTRY: std::cell::RefCell<std::collections::HashMap<i64, Arc<gossamer_std::sync::Barrier>>> =
        std::cell::RefCell::new(std::collections::HashMap::new());
}

fn install_sync_atomic_u64(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("AtomicU64::new", builtin_atomic_u64_new),
        ("AtomicU64::load", builtin_atomic_u64_load),
        ("AtomicU64::store", builtin_atomic_u64_store),
        ("AtomicU64::fetch_add", builtin_atomic_u64_fetch_add),
        ("AtomicU64::compare_and_swap", builtin_atomic_u64_cas),
    ];
    for (name, call) in entries {
        let qualified: &'static str = Box::leak(format!("sync::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn atomic_u64_handle(id: i64) -> Value {
    atomic_handle("sync::AtomicU64", id)
}

fn atomic_u64_id_of(value: &Value) -> Option<i64> {
    atomic_id_of(value, "sync::AtomicU64")
}

fn with_atomic_u64<R>(value: &Value, f: impl FnOnce(&Arc<StdAtomicU64>) -> R) -> Option<R> {
    let id = atomic_u64_id_of(value)?;
    ATOMIC_U64_REGISTRY.with(|r| r.borrow().get(&id).map(f))
}

fn builtin_atomic_u64_new(args: &[Value]) -> RuntimeResult<Value> {
    let init = args.first().and_then(value_to_int).unwrap_or(0) as u64;
    let id = next_atomic_id();
    ATOMIC_U64_REGISTRY.with(|r| {
        r.borrow_mut().insert(id, Arc::new(StdAtomicU64::new(init)));
    });
    Ok(atomic_u64_handle(id))
}

fn builtin_atomic_u64_load(args: &[Value]) -> RuntimeResult<Value> {
    let n = args
        .first()
        .and_then(|v| with_atomic_u64(v, |a| a.load(Ordering::SeqCst)))
        .unwrap_or(0);
    Ok(Value::Int(n as i64))
}

fn builtin_atomic_u64_store(args: &[Value]) -> RuntimeResult<Value> {
    let val = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    if let Some(handle) = args.first() {
        let _ = with_atomic_u64(handle, |a| a.store(val, Ordering::SeqCst));
    }
    Ok(Value::Unit)
}

fn builtin_atomic_u64_fetch_add(args: &[Value]) -> RuntimeResult<Value> {
    let delta = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let prev = args
        .first()
        .and_then(|v| with_atomic_u64(v, |a| a.fetch_add(delta, Ordering::SeqCst)))
        .unwrap_or(0);
    Ok(Value::Int(prev as i64))
}

fn builtin_atomic_u64_cas(args: &[Value]) -> RuntimeResult<Value> {
    let current = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let new = args.get(2).and_then(value_to_int).unwrap_or(0) as u64;
    let ok = args
        .first()
        .and_then(|v| {
            with_atomic_u64(v, |a| {
                a.compare_exchange(current, new, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            })
        })
        .unwrap_or(false);
    Ok(Value::Bool(ok))
}

// ----------------------------------------------------------------------
// sync::Barrier

fn install_sync_barrier(globals: &mut Vec<(&'static str, Value)>) {
    let entries: &[(&str, BuiltinFnPub)] = &[
        ("Barrier::new", builtin_barrier_new),
        ("Barrier::wait", builtin_barrier_wait),
    ];
    for (name, call) in entries {
        let qualified: &'static str = Box::leak(format!("sync::{name}").into_boxed_str());
        globals.push((qualified, crate::builtins::builtin_pub(qualified, *call)));
        globals.push((*name, crate::builtins::builtin_pub(name, *call)));
    }
}

fn barrier_handle(id: i64) -> Value {
    Value::struct_(
        "sync::Barrier",
        Arc::new(vec![(Ident::new("__barrier"), Value::Int(id))]),
    )
}

fn barrier_id_of(value: &Value) -> Option<i64> {
    if let Value::Struct(inner) = value {
        for (ident, v) in inner.fields.iter() {
            if ident.name == "__barrier" {
                if let Value::Int(n) = v {
                    return Some(*n);
                }
            }
        }
    }
    None
}

fn with_barrier<R>(
    value: &Value,
    f: impl FnOnce(&Arc<gossamer_std::sync::Barrier>) -> R,
) -> Option<R> {
    let id = barrier_id_of(value)?;
    BARRIER_REGISTRY.with(|r| r.borrow().get(&id).map(f))
}

fn builtin_barrier_new(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(1) as usize;
    let id = next_atomic_id();
    BARRIER_REGISTRY.with(|r| {
        r.borrow_mut()
            .insert(id, Arc::new(gossamer_std::sync::Barrier::new(n)));
    });
    Ok(barrier_handle(id))
}

fn builtin_barrier_wait(args: &[Value]) -> RuntimeResult<Value> {
    if let Some(handle) = args.first() {
        let _ = with_barrier(handle, |b| b.wait());
    }
    Ok(Value::Unit)
}

// ----------------------------------------------------------------------
// crypto breadth (sha512, blake3, aead, ed25519, ecdsa, kdf, x509)

fn install_crypto_breadth(globals: &mut Vec<(&'static str, Value)>) {
    // sha512
    for (short, call) in [
        ("digest", builtin_crypto_sha512_digest as BuiltinFnPub),
        ("hex", builtin_crypto_sha512_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::sha512::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // blake3
    for (short, call) in [
        ("digest", builtin_crypto_blake3_digest as BuiltinFnPub),
        ("hex", builtin_crypto_blake3_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::blake3::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // aead
    for (short, call) in [
        ("aes_256_gcm_seal", builtin_crypto_aes_seal as BuiltinFnPub),
        ("aes_256_gcm_open", builtin_crypto_aes_open),
        ("chacha20_poly1305_seal", builtin_crypto_chacha_seal),
        ("chacha20_poly1305_open", builtin_crypto_chacha_open),
    ] {
        let q: &'static str = Box::leak(format!("crypto::aead::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // ed25519
    for (short, call) in [
        ("keypair", builtin_crypto_ed25519_keypair as BuiltinFnPub),
        ("sign", builtin_crypto_ed25519_sign),
        ("verify", builtin_crypto_ed25519_verify),
    ] {
        let q: &'static str = Box::leak(format!("crypto::ed25519::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // ecdsa
    for (short, call) in [
        (
            "keypair_pem",
            builtin_crypto_ecdsa_keypair_pem as BuiltinFnPub,
        ),
        ("sign_pem", builtin_crypto_ecdsa_sign_pem),
        ("verify_pem", builtin_crypto_ecdsa_verify_pem),
    ] {
        let q: &'static str = Box::leak(format!("crypto::ecdsa::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // kdf
    for (short, call) in [
        ("pbkdf2_sha256", builtin_crypto_kdf_pbkdf2 as BuiltinFnPub),
        ("argon2id_hash", builtin_crypto_kdf_argon2id_hash),
        ("argon2id_verify", builtin_crypto_kdf_argon2id_verify),
        ("scrypt_interactive", builtin_crypto_kdf_scrypt),
    ] {
        let q: &'static str = Box::leak(format!("crypto::kdf::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
    // x509
    {
        let q = "crypto::x509::parse_pem";
        globals.push((
            q,
            crate::builtins::builtin_pub(q, builtin_crypto_x509_parse_pem),
        ));
    }
}

fn builtin_crypto_sha512_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::sha512::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

fn builtin_crypto_sha512_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::sha512::hex(&input).into(),
    ))
}

fn builtin_crypto_blake3_digest(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::blake3::digest(&input);
    Ok(bytes_to_value_array(&digest))
}

fn builtin_crypto_blake3_hex(args: &[Value]) -> RuntimeResult<Value> {
    let input = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::blake3::hex(&input).into(),
    ))
}

fn builtin_crypto_aes_seal(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let pt = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::aes_256_gcm_seal(&key, &nonce, &pt, &aad) {
        Ok(ct) => Ok(ok_variant(bytes_to_value_array(&ct))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_aes_open(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let ct = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::aes_256_gcm_open(&key, &nonce, &ct, &aad) {
        Ok(pt) => Ok(ok_variant(bytes_to_value_array(&pt))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_chacha_seal(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let pt = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::chacha20_poly1305_seal(&key, &nonce, &pt, &aad) {
        Ok(ct) => Ok(ok_variant(bytes_to_value_array(&ct))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_chacha_open(args: &[Value]) -> RuntimeResult<Value> {
    let key = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let nonce = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let ct = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    let aad = value_to_bytes(args.get(3).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::aead::chacha20_poly1305_open(&key, &nonce, &ct, &aad) {
        Ok(pt) => Ok(ok_variant(bytes_to_value_array(&pt))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ed25519_keypair(_args: &[Value]) -> RuntimeResult<Value> {
    match gossamer_std::crypto::ed25519::keypair() {
        Ok((secret, public)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            bytes_to_value_array(&secret),
            bytes_to_value_array(&public),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ed25519_sign(args: &[Value]) -> RuntimeResult<Value> {
    let secret = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ed25519::sign(&secret, &msg) {
        Ok(sig) => Ok(ok_variant(bytes_to_value_array(&sig))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ed25519_verify(args: &[Value]) -> RuntimeResult<Value> {
    let public = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let sig = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ed25519::verify(&public, &msg, &sig) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ecdsa_keypair_pem(_args: &[Value]) -> RuntimeResult<Value> {
    match gossamer_std::crypto::ecdsa::keypair_pem() {
        Ok((secret, public)) => Ok(ok_variant(Value::Tuple(Arc::new(vec![
            Value::String(secret.into()),
            Value::String(public.into()),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ecdsa_sign_pem(args: &[Value]) -> RuntimeResult<Value> {
    let secret = args.first().and_then(as_str).unwrap_or("").to_string();
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ecdsa::sign_pem(&secret, &msg) {
        Ok(sig) => Ok(ok_variant(bytes_to_value_array(&sig))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_ecdsa_verify_pem(args: &[Value]) -> RuntimeResult<Value> {
    let public = args.first().and_then(as_str).unwrap_or("").to_string();
    let msg = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let sig = value_to_bytes(args.get(2).unwrap_or(&Value::Unit));
    match gossamer_std::crypto::ecdsa::verify_pem(&public, &msg, &sig) {
        Ok(()) => Ok(ok_variant(Value::Unit)),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_kdf_pbkdf2(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let salt = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let iterations = args.get(2).and_then(value_to_int).unwrap_or(100_000) as u32;
    let output = args.get(3).and_then(value_to_int).unwrap_or(32) as usize;
    let key = gossamer_std::crypto::kdf::pbkdf2_sha256(&password, &salt, iterations, output);
    Ok(bytes_to_value_array(&key))
}

fn builtin_crypto_kdf_argon2id_hash(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::kdf::argon2id_hash(&password) {
        Ok(phc) => Ok(ok_variant(Value::String(phc.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_kdf_argon2id_verify(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let phc = args.get(1).and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::crypto::kdf::argon2id_verify(&password, &phc) {
        Ok(ok) => Ok(ok_variant(Value::Bool(ok))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_kdf_scrypt(args: &[Value]) -> RuntimeResult<Value> {
    let password = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    let salt = value_to_bytes(args.get(1).unwrap_or(&Value::Unit));
    let output = args.get(2).and_then(value_to_int).unwrap_or(32) as usize;
    match gossamer_std::crypto::kdf::scrypt_interactive(&password, &salt, output) {
        Ok(key) => Ok(ok_variant(bytes_to_value_array(&key))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_crypto_x509_parse_pem(args: &[Value]) -> RuntimeResult<Value> {
    let pem = value_to_bytes(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::crypto::x509::parse_pem(&pem) {
        Ok(info) => {
            let san_v = Value::Array(Arc::new(
                info.san_dns
                    .into_iter()
                    .map(|s| Value::String(s.into()))
                    .collect(),
            ));
            let struct_v = Value::struct_(
                "crypto::x509::CertInfo",
                Arc::new(vec![
                    (Ident::new("subject"), Value::String(info.subject.into())),
                    (Ident::new("issuer"), Value::String(info.issuer.into())),
                    (Ident::new("serial"), bytes_to_value_array(&info.serial)),
                    (
                        Ident::new("not_before_unix"),
                        Value::Int(info.not_before_unix),
                    ),
                    (
                        Ident::new("not_after_unix"),
                        Value::Int(info.not_after_unix),
                    ),
                    (Ident::new("san_dns"), san_v),
                    (Ident::new("sha256"), bytes_to_value_array(&info.sha256)),
                ]),
            );
            Ok(ok_variant(struct_v))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// hash::crc32 and hash::adler32

fn install_hash_crc32_adler32(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        (
            "crc32::checksum",
            builtin_hash_crc32_checksum as BuiltinFnPub,
        ),
        ("crc32::checksum_string", builtin_hash_crc32_checksum_string),
        ("crc32::update", builtin_hash_crc32_update),
        ("adler32::checksum", builtin_hash_adler32_checksum),
        (
            "adler32::checksum_string",
            builtin_hash_adler32_checksum_string,
        ),
        ("adler32::update", builtin_hash_adler32_update),
    ] {
        let q: &'static str = Box::leak(format!("hash::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_hash_crc32_checksum(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::Int(i64::from(gossamer_std::hash::crc32::checksum(
        &data,
    ))))
}

fn builtin_hash_crc32_checksum_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::Int(i64::from(
        gossamer_std::hash::crc32::checksum_string(&s),
    )))
}

fn builtin_hash_crc32_update(args: &[Value]) -> RuntimeResult<Value> {
    let crc = args.first().and_then(value_to_int).unwrap_or(0) as u32;
    let data = bytes_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Int(i64::from(gossamer_std::hash::crc32::update(
        crc, &data,
    ))))
}

fn builtin_hash_adler32_checksum(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::Int(i64::from(
        gossamer_std::hash::adler32::checksum(&data),
    )))
}

fn builtin_hash_adler32_checksum_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::Int(i64::from(
        gossamer_std::hash::adler32::checksum_string(&s),
    )))
}

fn builtin_hash_adler32_update(args: &[Value]) -> RuntimeResult<Value> {
    let adler = args.first().and_then(value_to_int).unwrap_or(1) as u32;
    let data = bytes_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Int(i64::from(gossamer_std::hash::adler32::update(
        adler, &data,
    ))))
}

// ----------------------------------------------------------------------
// json builtins (parse / encode / encode_pretty / valid)

fn install_json_builtins(globals: &mut Vec<(&'static str, Value)>) {
    // Only register under encoding::json:: prefix to avoid shadowing the
    // existing json:: builtins in builtins.rs (which carry json::get / as_str
    // / the JsonValue ecosystem). Code that writes
    // `use std::encoding` and calls `encoding::json::parse` uses these.
    for (short, call) in [
        ("parse", builtin_json_std_parse as BuiltinFnPub),
        ("encode", builtin_json_std_encode),
        ("encode_pretty", builtin_json_std_encode_pretty),
        ("valid", builtin_json_std_valid),
    ] {
        let q: &'static str = Box::leak(format!("encoding::json::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn json_std_to_value(v: gossamer_std::json::Value) -> Value {
    use gossamer_std::json::Value as JV;
    match v {
        JV::Null => Value::Unit,
        JV::Bool(b) => Value::Bool(b),
        JV::Number(f) => {
            if f.fract() == 0.0 && f.abs() < 9_007_199_254_740_992.0 {
                Value::Int(f as i64)
            } else {
                Value::Float(f)
            }
        }
        JV::String(s) => Value::String(s.into()),
        JV::Array(arr) => Value::Array(Arc::new(arr.into_iter().map(json_std_to_value).collect())),
        JV::Object(map) => {
            let mut hmap = rustc_hash::FxHashMap::default();
            for (k, v) in map {
                hmap.insert(MapKey::Str(k.into()), json_std_to_value(v));
            }
            Value::Map(Arc::new(parking_lot::Mutex::new(hmap)))
        }
    }
}

fn value_to_json_std(v: &Value) -> gossamer_std::json::Value {
    use gossamer_std::json::Value as JV;
    match v {
        Value::Unit => JV::Null,
        Value::Bool(b) => JV::Bool(*b),
        Value::Int(n) => JV::Number(*n as f64),
        Value::Float(f) => JV::Number(*f),
        Value::String(s) => JV::String(s.as_str().to_string()),
        Value::Array(arr) => JV::Array(arr.iter().map(value_to_json_std).collect()),
        Value::Map(map) => {
            let guard = map.lock();
            let obj: std::collections::BTreeMap<String, JV> = guard
                .iter()
                .map(|(k, v)| {
                    let key = match k {
                        MapKey::Str(s) => s.to_string(),
                        MapKey::Int(n) => n.to_string(),
                        MapKey::Bool(b) => b.to_string(),
                        _ => "<key>".to_string(),
                    };
                    (key, value_to_json_std(v))
                })
                .collect();
            JV::Object(obj)
        }
        _ => JV::Null,
    }
}

fn builtin_json_std_parse(args: &[Value]) -> RuntimeResult<Value> {
    let src = args.first().and_then(as_str).unwrap_or("");
    match gossamer_std::json::parse(src) {
        Ok(v) => Ok(ok_variant(json_std_to_value(v))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_json_std_encode(args: &[Value]) -> RuntimeResult<Value> {
    let jv = value_to_json_std(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(gossamer_std::json::encode(&jv).into()))
}

fn builtin_json_std_encode_pretty(args: &[Value]) -> RuntimeResult<Value> {
    let jv = value_to_json_std(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(gossamer_std::json::encode_pretty(&jv).into()))
}

fn builtin_json_std_valid(args: &[Value]) -> RuntimeResult<Value> {
    let src = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::json::parse(src).is_ok()))
}

// ----------------------------------------------------------------------
// time completeness (sleep, now, unix_ms, format_rfc3339, parse_rfc3339)

fn install_time_completeness(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "time",
        &[
            ("sleep", builtin_time_sleep),
            ("now", builtin_time_now_unix_ms),
            ("unix_ms", builtin_time_now_unix_ms),
            ("format_rfc3339", builtin_time_format_rfc3339),
            ("parse_rfc3339", builtin_time_parse_rfc3339),
        ],
        globals,
    );
}

fn builtin_time_sleep(args: &[Value]) -> RuntimeResult<Value> {
    let ms = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    std::thread::sleep(std::time::Duration::from_millis(ms));
    Ok(Value::Unit)
}

fn builtin_time_now_unix_ms(_args: &[Value]) -> RuntimeResult<Value> {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_millis());
    Ok(Value::Int(i64::try_from(ms).unwrap_or(i64::MAX)))
}

fn builtin_time_format_rfc3339(args: &[Value]) -> RuntimeResult<Value> {
    let ms = args.first().and_then(value_to_int).unwrap_or(0);
    let st = gossamer_std::time::SystemTime::from_unix_millis(ms);
    match gossamer_std::time::format_rfc3339(st) {
        Ok(s) => Ok(ok_variant(Value::String(s.into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_time_parse_rfc3339(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::time::parse_rfc3339(&s) {
        Ok(st) => {
            let ms = st.unix_millis();
            Ok(ok_variant(Value::Int(
                i64::try_from(ms).unwrap_or(i64::MAX),
            )))
        }
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// net::ip builtins

fn install_net_ip(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("parse", builtin_net_ip_parse as BuiltinFnPub),
        ("is_valid", builtin_net_ip_is_valid),
        ("is_v4", builtin_net_ip_is_v4),
        ("is_v6", builtin_net_ip_is_v6),
        ("to_string", builtin_net_ip_to_string),
        ("is_loopback", builtin_net_ip_is_loopback),
        ("is_private", builtin_net_ip_is_private),
        ("is_multicast", builtin_net_ip_is_multicast),
        ("is_unspecified", builtin_net_ip_is_unspecified),
        ("octets", builtin_net_ip_octets),
    ] {
        let q: &'static str = Box::leak(format!("net::ip::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn ip_to_value(ip: gossamer_std::net::ip::Ip) -> Value {
    let (tag, octets): (&str, Vec<u8>) = match &ip {
        gossamer_std::net::ip::Ip::V4(_) => ("v4", ip.octets()),
        gossamer_std::net::ip::Ip::V6(_) => ("v6", ip.octets()),
    };
    Value::struct_(
        "net::ip::Ip",
        Arc::new(vec![
            (Ident::new("__tag"), Value::String(tag.into())),
            (Ident::new("__str"), Value::String(ip.to_string().into())),
            (Ident::new("__octets"), bytes_to_value_array(&octets)),
        ]),
    )
}

fn ip_from_value(v: &Value) -> Option<gossamer_std::net::ip::Ip> {
    let s = match v {
        Value::Struct(inner) if inner.name == "net::ip::Ip" => {
            inner.fields.iter().find_map(|(i, val)| {
                if i.name == "__str" {
                    if let Value::String(s) = val {
                        Some(s.as_str().to_string())
                    } else {
                        None
                    }
                } else {
                    None
                }
            })
        }
        Value::String(s) => Some(s.as_str().to_string()),
        _ => None,
    }?;
    gossamer_std::net::ip::parse(&s).ok()
}

fn builtin_net_ip_parse(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::net::ip::parse(&s) {
        Ok(ip) => Ok(ok_variant(ip_to_value(ip))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_net_ip_is_valid(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_valid(s)))
}

fn builtin_net_ip_is_v4(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_v4(s)))
}

fn builtin_net_ip_is_v6(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("");
    Ok(Value::Bool(gossamer_std::net::ip::is_v6(s)))
}

fn builtin_net_ip_to_string(args: &[Value]) -> RuntimeResult<Value> {
    match ip_from_value(args.first().unwrap_or(&Value::Unit)) {
        Some(ip) => Ok(Value::String(ip.to_string().into())),
        None => Ok(Value::String("".into())),
    }
}

fn builtin_net_ip_is_loopback(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_loopback()),
    ))
}

fn builtin_net_ip_is_private(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_private()),
    ))
}

fn builtin_net_ip_is_multicast(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_multicast()),
    ))
}

fn builtin_net_ip_is_unspecified(args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Bool(
        ip_from_value(args.first().unwrap_or(&Value::Unit)).is_some_and(|ip| ip.is_unspecified()),
    ))
}

fn builtin_net_ip_octets(args: &[Value]) -> RuntimeResult<Value> {
    match ip_from_value(args.first().unwrap_or(&Value::Unit)) {
        Some(ip) => Ok(bytes_to_value_array(&ip.octets())),
        None => Ok(Value::Array(Arc::new(vec![]))),
    }
}

// ----------------------------------------------------------------------
// thread builtins

fn install_thread(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "thread",
        &[
            ("sleep_ms", builtin_thread_sleep_ms),
            ("num_cpus", builtin_thread_num_cpus),
            ("yield_now", builtin_thread_yield_now),
        ],
        globals,
    );
}

fn builtin_thread_sleep_ms(args: &[Value]) -> RuntimeResult<Value> {
    let ms = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    gossamer_std::thread::sleep_ms(ms);
    Ok(Value::Unit)
}

fn builtin_thread_num_cpus(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Int(gossamer_std::thread::num_cpus() as i64))
}

fn builtin_thread_yield_now(_args: &[Value]) -> RuntimeResult<Value> {
    gossamer_std::thread::yield_now();
    Ok(Value::Unit)
}

// ----------------------------------------------------------------------
// html escape / unescape

fn install_html(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "html",
        &[
            ("escape", builtin_html_escape),
            ("unescape", builtin_html_unescape),
        ],
        globals,
    );
}

fn builtin_html_escape(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::String(gossamer_std::html::escape(&s).into()))
}

fn builtin_html_unescape(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::String(gossamer_std::html::unescape(&s).into()))
}

// ----------------------------------------------------------------------
// encoding::base64 and encoding::hex (qualified paths under `use std::encoding`)

fn install_encoding_base64_hex(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("base64::encode", builtin_enc_base64_encode as BuiltinFnPub),
        ("base64::decode", builtin_enc_base64_decode),
        ("hex::encode", builtin_enc_hex_encode),
        ("hex::decode", builtin_enc_hex_decode),
    ] {
        let q: &'static str = Box::leak(format!("encoding::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_enc_base64_encode(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::base64::encode(&data).into(),
    ))
}

fn builtin_enc_base64_decode(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::base64::decode(&s) {
        Ok(bytes) => Ok(ok_variant(bytes_to_array(bytes))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_enc_hex_encode(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::hex::encode(&data).into(),
    ))
}

fn builtin_enc_hex_decode(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::hex::decode(&s) {
        Ok(bytes) => Ok(ok_variant(bytes_to_array(bytes))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::base32

fn install_encoding_base32(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("encode", builtin_base32_encode as BuiltinFnPub),
        ("decode", builtin_base32_decode),
        ("encode_string", builtin_base32_encode_string),
        ("decode_string", builtin_base32_decode_string),
        ("encode_hex", builtin_base32_encode_hex),
        ("decode_hex", builtin_base32_decode_hex),
    ] {
        let q: &'static str = Box::leak(format!("encoding::base32::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_base32_encode(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::base32::encode(&data).into(),
    ))
}

fn builtin_base32_decode(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::base32::decode(&s) {
        Ok(bytes) => Ok(ok_variant(bytes_to_array(bytes))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_base32_encode_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::String(
        gossamer_std::encoding::base32::encode_string(&s).into(),
    ))
}

fn builtin_base32_decode_string(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::base32::decode_string(&s) {
        Ok(out) => Ok(ok_variant(Value::String(out.into()))),
        Err(e) => Ok(err_variant(e)),
    }
}

fn builtin_base32_encode_hex(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::base32::encode_hex(&data).into(),
    ))
}

fn builtin_base32_decode_hex(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::base32::decode_hex(&s) {
        Ok(bytes) => Ok(ok_variant(bytes_to_array(bytes))),
        Err(e) => Ok(err_variant(e)),
    }
}

// ----------------------------------------------------------------------
// encoding::ascii85

fn install_encoding_ascii85(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("encode", builtin_ascii85_encode as BuiltinFnPub),
        ("decode", builtin_ascii85_decode),
    ] {
        let q: &'static str = Box::leak(format!("encoding::ascii85::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_ascii85_encode(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::encoding::ascii85::encode(&data).into(),
    ))
}

fn builtin_ascii85_decode(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::ascii85::decode(&s) {
        Ok(bytes) => Ok(ok_variant(bytes_to_array(bytes))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::xml

fn install_encoding_xml(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("parse", builtin_xml_parse as BuiltinFnPub),
        ("encode", builtin_xml_encode),
        ("escape", builtin_xml_escape),
    ] {
        let q: &'static str = Box::leak(format!("encoding::xml::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn xml_node_to_value(node: &gossamer_std::encoding::xml::Node) -> Value {
    use gossamer_std::encoding::xml::Node;
    match node {
        Node::Text(s) => {
            let mut map = rustc_hash::FxHashMap::default();
            map.insert(
                MapKey::Str("__xml_type".into()),
                Value::String("text".into()),
            );
            map.insert(MapKey::Str("value".into()), Value::String(s.clone().into()));
            Value::Map(Arc::new(parking_lot::Mutex::new(map)))
        }
        Node::Element {
            name,
            attrs,
            children,
        } => {
            let mut map = rustc_hash::FxHashMap::default();
            map.insert(
                MapKey::Str("__xml_type".into()),
                Value::String("element".into()),
            );
            map.insert(
                MapKey::Str("name".into()),
                Value::String(name.clone().into()),
            );
            let mut attr_map = rustc_hash::FxHashMap::default();
            for (k, v) in attrs {
                attr_map.insert(
                    MapKey::Str(k.clone().into()),
                    Value::String(v.clone().into()),
                );
            }
            map.insert(
                MapKey::Str("attrs".into()),
                Value::Map(Arc::new(parking_lot::Mutex::new(attr_map))),
            );
            let child_vals: Vec<Value> = children.iter().map(xml_node_to_value).collect();
            map.insert(
                MapKey::Str("children".into()),
                Value::Array(Arc::new(child_vals)),
            );
            Value::Map(Arc::new(parking_lot::Mutex::new(map)))
        }
    }
}

fn value_to_xml_node(v: &Value) -> Option<gossamer_std::encoding::xml::Node> {
    use gossamer_std::encoding::xml::Node;
    let map = match v {
        Value::Map(m) => m.lock(),
        _ => return None,
    };
    let xml_type = match map.get(&MapKey::Str("__xml_type".into())) {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => return None,
    };
    if xml_type == "text" {
        let s = match map.get(&MapKey::Str("value".into())) {
            Some(Value::String(s)) => s.as_str().to_string(),
            _ => String::new(),
        };
        return Some(Node::Text(s));
    }
    if xml_type == "element" {
        let name = match map.get(&MapKey::Str("name".into())) {
            Some(Value::String(s)) => s.as_str().to_string(),
            _ => return None,
        };
        let attrs = match map.get(&MapKey::Str("attrs".into())) {
            Some(Value::Map(m)) => {
                let inner = m.lock();
                inner
                    .iter()
                    .filter_map(|(k, v)| {
                        let key = match k {
                            MapKey::Str(s) => s.to_string(),
                            _ => return None,
                        };
                        let val = match v {
                            Value::String(s) => s.as_str().to_string(),
                            _ => return None,
                        };
                        Some((key, val))
                    })
                    .collect()
            }
            _ => std::collections::BTreeMap::new(),
        };
        let children = match map.get(&MapKey::Str("children".into())) {
            Some(Value::Array(arr)) => arr.iter().filter_map(value_to_xml_node).collect(),
            _ => Vec::new(),
        };
        return Some(Node::Element {
            name,
            attrs,
            children,
        });
    }
    None
}

fn builtin_xml_parse(args: &[Value]) -> RuntimeResult<Value> {
    let src = args.first().and_then(as_str).unwrap_or("").to_string();
    match gossamer_std::encoding::xml::parse(&src) {
        Ok(node) => Ok(ok_variant(xml_node_to_value(&node))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_xml_encode(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    match value_to_xml_node(v) {
        Some(node) => Ok(Value::String(
            gossamer_std::encoding::xml::encode(&node).into(),
        )),
        None => Ok(Value::String("".into())),
    }
}

fn builtin_xml_escape(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("").to_string();
    Ok(Value::String(
        gossamer_std::encoding::xml::escape(&s).into(),
    ))
}

// ----------------------------------------------------------------------
// crypto::insecure (MD5, SHA-1)

fn install_crypto_insecure(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("md5", builtin_insecure_md5 as BuiltinFnPub),
        ("md5_hex", builtin_insecure_md5_hex),
        ("sha1", builtin_insecure_sha1),
        ("sha1_hex", builtin_insecure_sha1_hex),
    ] {
        let q: &'static str = Box::leak(format!("crypto::insecure::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_insecure_md5(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::insecure::md5(&data);
    Ok(bytes_to_array(digest.to_vec()))
}

fn builtin_insecure_md5_hex(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::insecure::md5_hex(&data).into(),
    ))
}

fn builtin_insecure_sha1(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let digest = gossamer_std::crypto::insecure::sha1(&data);
    Ok(bytes_to_array(digest.to_vec()))
}

fn builtin_insecure_sha1_hex(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    Ok(Value::String(
        gossamer_std::crypto::insecure::sha1_hex(&data).into(),
    ))
}

// ----------------------------------------------------------------------
// compress::bzip2

fn install_compress_bzip2(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("compress", builtin_bzip2_compress as BuiltinFnPub),
        ("decompress", builtin_bzip2_decompress),
    ] {
        let q: &'static str = Box::leak(format!("compress::bzip2::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn builtin_bzip2_compress(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    let level = args.get(1).and_then(value_to_int).unwrap_or(6) as u32;
    match gossamer_std::compress::bzip2::compress(&data, level) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_bzip2_decompress(args: &[Value]) -> RuntimeResult<Value> {
    let data = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::compress::bzip2::decompress(&data) {
        Ok(out) => Ok(ok_variant(bytes_to_array(out))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// math::big — arbitrary-precision integers (string representation in VM)

fn install_math_big(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        // signed Int
        ("int_from_str", builtin_big_int_from_str as BuiltinFnPub),
        ("int_from_i64", builtin_big_int_from_i64),
        ("int_to_str", builtin_big_int_to_str),
        ("int_to_hex", builtin_big_int_to_hex),
        ("int_to_i64", builtin_big_int_to_i64),
        ("int_is_zero", builtin_big_int_is_zero),
        ("int_is_positive", builtin_big_int_is_positive),
        ("int_is_negative", builtin_big_int_is_negative),
        ("int_add", builtin_big_int_add),
        ("int_sub", builtin_big_int_sub),
        ("int_mul", builtin_big_int_mul),
        ("int_div", builtin_big_int_div),
        ("int_rem", builtin_big_int_rem),
        ("int_pow", builtin_big_int_pow),
        ("int_abs", builtin_big_int_abs),
        ("int_neg", builtin_big_int_neg),
        ("int_gcd", builtin_big_int_gcd),
        ("int_lcm", builtin_big_int_lcm),
        ("int_cmp", builtin_big_int_cmp),
        // unsigned Uint
        ("uint_from_str", builtin_big_uint_from_str),
        ("uint_from_u64", builtin_big_uint_from_u64),
        ("uint_to_str", builtin_big_uint_to_str),
        ("uint_to_hex", builtin_big_uint_to_hex),
        ("uint_to_u64", builtin_big_uint_to_u64),
        ("uint_is_zero", builtin_big_uint_is_zero),
        ("uint_add", builtin_big_uint_add),
        ("uint_mul", builtin_big_uint_mul),
        ("uint_pow", builtin_big_uint_pow),
        ("uint_pow_mod", builtin_big_uint_pow_mod),
        ("uint_bit_len", builtin_big_uint_bit_len),
        // free functions
        ("factorial", builtin_big_factorial),
    ] {
        let q: &'static str = Box::leak(format!("math::big::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

fn big_int_from_value(v: &Value) -> gossamer_std::math::big::Int {
    let s = match v {
        Value::String(s) => s.as_str().to_string(),
        Value::Int(n) => n.to_string(),
        _ => "0".to_string(),
    };
    gossamer_std::math::big::Int::parse(&s)
        .unwrap_or_else(|_| gossamer_std::math::big::Int::from_i64(0))
}

fn big_uint_from_value(v: &Value) -> gossamer_std::math::big::Uint {
    let s = match v {
        Value::String(s) => s.as_str().to_string(),
        Value::Int(n) => n.abs().to_string(),
        _ => "0".to_string(),
    };
    gossamer_std::math::big::Uint::parse(&s)
        .unwrap_or_else(|_| gossamer_std::math::big::Uint::from_u64(0))
}

fn builtin_big_int_from_str(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("0").to_string();
    match gossamer_std::math::big::Int::parse(&s) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_big_int_from_i64(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0);
    let big = gossamer_std::math::big::Int::from_i64(n);
    Ok(Value::String(big.to_string().into()))
}

fn builtin_big_int_to_str(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_int_from_value(v).to_string().into()))
}

fn builtin_big_int_to_hex(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_int_from_value(v).to_hex().into()))
}

fn builtin_big_int_to_i64(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    match big_int_from_value(v).to_i64() {
        Some(n) => Ok(some_variant(Value::Int(n))),
        None => Ok(none_variant()),
    }
}

fn builtin_big_int_is_zero(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_zero()))
}

fn builtin_big_int_is_positive(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_positive()))
}

fn builtin_big_int_is_negative(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_int_from_value(v).is_negative()))
}

fn builtin_big_int_add(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.add(&b).to_string().into()))
}

fn builtin_big_int_sub(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.sub(&b).to_string().into()))
}

fn builtin_big_int_mul(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.mul(&b).to_string().into()))
}

fn builtin_big_int_div(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    match a.div(&b) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_big_int_rem(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    match a.rem(&b) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_big_int_pow(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = args.get(1).and_then(value_to_int).unwrap_or(0).max(0) as u32;
    Ok(Value::String(a.pow(exp).to_string().into()))
}

fn builtin_big_int_abs(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(
        big_int_from_value(v).abs().to_string().into(),
    ))
}

fn builtin_big_int_neg(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(
        big_int_from_value(v).neg().to_string().into(),
    ))
}

fn builtin_big_int_gcd(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.gcd(&b).to_string().into()))
}

fn builtin_big_int_lcm(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.lcm(&b).to_string().into()))
}

fn builtin_big_int_cmp(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_int_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_int_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::Int(a.compare(&b)))
}

fn builtin_big_uint_from_str(args: &[Value]) -> RuntimeResult<Value> {
    let s = args.first().and_then(as_str).unwrap_or("0").to_string();
    match gossamer_std::math::big::Uint::parse(&s) {
        Ok(n) => Ok(ok_variant(Value::String(n.to_string().into()))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

fn builtin_big_uint_from_u64(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    let big = gossamer_std::math::big::Uint::from_u64(n);
    Ok(Value::String(big.to_string().into()))
}

fn builtin_big_uint_to_str(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_uint_from_value(v).to_string().into()))
}

fn builtin_big_uint_to_hex(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::String(big_uint_from_value(v).to_hex().into()))
}

fn builtin_big_uint_to_u64(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    match big_uint_from_value(v).to_u64() {
        Some(n) => Ok(some_variant(Value::Int(n as i64))),
        None => Ok(none_variant()),
    }
}

fn builtin_big_uint_is_zero(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Bool(big_uint_from_value(v).is_zero()))
}

fn builtin_big_uint_add(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.add(&b).to_string().into()))
}

fn builtin_big_uint_mul(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let b = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    Ok(Value::String(a.mul(&b).to_string().into()))
}

fn builtin_big_uint_pow(args: &[Value]) -> RuntimeResult<Value> {
    let a = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = args.get(1).and_then(value_to_int).unwrap_or(0).max(0) as u32;
    Ok(Value::String(a.pow(exp).to_string().into()))
}

fn builtin_big_uint_pow_mod(args: &[Value]) -> RuntimeResult<Value> {
    let base = big_uint_from_value(args.first().unwrap_or(&Value::Unit));
    let exp = big_uint_from_value(args.get(1).unwrap_or(&Value::Unit));
    let modulus = big_uint_from_value(args.get(2).unwrap_or(&Value::Unit));
    Ok(Value::String(
        base.pow_mod(&exp, &modulus).to_string().into(),
    ))
}

fn builtin_big_uint_bit_len(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.first().unwrap_or(&Value::Unit);
    Ok(Value::Int(big_uint_from_value(v).bit_len() as i64))
}

fn builtin_big_factorial(args: &[Value]) -> RuntimeResult<Value> {
    let n = args.first().and_then(value_to_int).unwrap_or(0).max(0) as u64;
    Ok(Value::String(
        gossamer_std::math::big::factorial(n).to_string().into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(builtin: BuiltinFnPub, args: Vec<Value>) -> Value {
        builtin(&args).unwrap()
    }

    #[test]
    fn strings_join_inserts_separator() {
        let parts = Value::Array(Arc::new(vec![
            Value::String("a".into()),
            Value::String("b".into()),
            Value::String("c".into()),
        ]));
        let out = call(builtin_strings_join, vec![parts, Value::String(",".into())]);
        if let Value::String(s) = out {
            assert_eq!(s.as_str(), "a,b,c");
        } else {
            panic!("expected string, got {out:?}");
        }
    }

    #[test]
    fn strconv_parse_int_round_trip() {
        let parsed = call(builtin_strconv_parse_i64, vec![Value::String("42".into())]);
        if let Value::Variant(inner) = parsed {
            assert_eq!(inner.name, "Ok");
            assert!(matches!(inner.fields.first(), Some(Value::Int(42))));
        } else {
            panic!("expected Ok variant");
        }
        let formatted = call(builtin_strconv_format_i64, vec![Value::Int(-7)]);
        if let Value::String(s) = formatted {
            assert_eq!(s.as_str(), "-7");
        } else {
            panic!("expected string");
        }
    }

    #[test]
    fn set_supports_full_lifecycle() {
        let s = call(builtin_set_new, vec![]);
        assert!(matches!(s, Value::Struct(_)));
        // insert returns the handle (for VM writeback idempotency).
        let after_insert = call(builtin_set_insert, vec![s.clone(), Value::Int(1)]);
        assert!(matches!(after_insert, Value::Struct(_)));
        let _ = call(builtin_set_insert, vec![s.clone(), Value::Int(1)]);
        let n = call(builtin_set_len, vec![s.clone()]);
        assert!(matches!(n, Value::Int(1)));
        let has = call(builtin_set_contains, vec![s.clone(), Value::Int(1)]);
        assert!(matches!(has, Value::Bool(true)));
        // remove returns the handle (for VM writeback idempotency).
        let after_remove = call(builtin_set_remove, vec![s.clone(), Value::Int(1)]);
        assert!(matches!(after_remove, Value::Struct(_)));
        let empty = call(builtin_set_is_empty, vec![s]);
        assert!(matches!(empty, Value::Bool(true)));
    }

    #[test]
    fn tcp_listener_round_trip_via_loopback() {
        let listener = call(
            builtin_tcp_listener_bind,
            vec![Value::String("127.0.0.1:0".into())],
        );
        let listener_handle = match listener {
            Value::Variant(inner) if inner.name == "Ok" => inner.fields[0].clone(),
            other => panic!("bind failed: {other:?}"),
        };
        let addr = match call(
            builtin_tcp_listener_local_addr,
            vec![listener_handle.clone()],
        ) {
            Value::Variant(inner) if inner.name == "Ok" => match &inner.fields[0] {
                Value::String(s) => s.as_str().to_string(),
                other => panic!("expected addr string, got {other:?}"),
            },
            other => panic!("local_addr failed: {other:?}"),
        };
        let addr_clone = addr.clone();
        let join = std::thread::spawn(move || {
            let conn = call(
                builtin_tcp_stream_connect,
                vec![Value::String(addr_clone.into())],
            );
            let conn_handle = match conn {
                Value::Variant(inner) if inner.name == "Ok" => inner.fields[0].clone(),
                other => panic!("connect failed: {other:?}"),
            };
            call(
                builtin_tcp_stream_write,
                vec![conn_handle.clone(), Value::String("hello".into())],
            );
            call(builtin_tcp_stream_close, vec![conn_handle]);
        });
        let accepted = call(builtin_tcp_listener_accept, vec![listener_handle.clone()]);
        let stream_handle = match accepted {
            Value::Variant(inner) if inner.name == "Ok" => match &inner.fields[0] {
                Value::Tuple(parts) => parts[0].clone(),
                other => panic!("expected tuple, got {other:?}"),
            },
            other => panic!("accept failed: {other:?}"),
        };
        let read = call(
            builtin_tcp_stream_read_to_string,
            vec![stream_handle.clone()],
        );
        match read {
            Value::Variant(inner) if inner.name == "Ok" => match &inner.fields[0] {
                Value::String(s) => assert_eq!(s.as_str(), "hello"),
                other => panic!("expected string, got {other:?}"),
            },
            other => panic!("read failed: {other:?}"),
        }
        call(builtin_tcp_stream_close, vec![stream_handle]);
        call(builtin_tcp_listener_close, vec![listener_handle]);
        join.join().unwrap();
    }

    #[test]
    fn udp_round_trip_via_loopback() {
        let server = call(builtin_udp_bind, vec![Value::String("127.0.0.1:0".into())]);
        let server_handle = match server {
            Value::Variant(inner) if inner.name == "Ok" => inner.fields[0].clone(),
            other => panic!("bind failed: {other:?}"),
        };
        let addr = match call(builtin_udp_local_addr, vec![server_handle.clone()]) {
            Value::Variant(inner) if inner.name == "Ok" => match &inner.fields[0] {
                Value::String(s) => s.as_str().to_string(),
                _ => panic!("addr was not string"),
            },
            _ => panic!("local_addr failed"),
        };
        let client = call(builtin_udp_bind, vec![Value::String("127.0.0.1:0".into())]);
        let client_handle = match client {
            Value::Variant(inner) if inner.name == "Ok" => inner.fields[0].clone(),
            _ => panic!("client bind failed"),
        };
        call(
            builtin_udp_send_to,
            vec![
                client_handle.clone(),
                Value::String("ping".into()),
                Value::String(addr.into()),
            ],
        );
        let recv = call(
            builtin_udp_recv_from,
            vec![server_handle.clone(), Value::Int(64)],
        );
        match recv {
            Value::Variant(inner) if inner.name == "Ok" => match &inner.fields[0] {
                Value::Tuple(parts) => match &parts[0] {
                    Value::Array(bytes) => {
                        let payload: Vec<u8> = bytes
                            .iter()
                            .filter_map(|v| match v {
                                Value::Int(n) => u8::try_from(*n).ok(),
                                _ => None,
                            })
                            .collect();
                        assert_eq!(payload, b"ping");
                    }
                    _ => panic!("expected bytes array"),
                },
                _ => panic!("expected tuple"),
            },
            other => panic!("recv failed: {other:?}"),
        }
        call(builtin_udp_close, vec![server_handle]);
        call(builtin_udp_close, vec![client_handle]);
    }

    #[test]
    fn time_instant_returns_monotonic_handle() {
        let inst = call(builtin_time_instant_now, vec![]);
        std::thread::sleep(std::time::Duration::from_millis(2));
        let elapsed = call(builtin_time_instant_elapsed_ms, vec![inst]);
        match elapsed {
            Value::Int(n) => assert!(n >= 0),
            other => panic!("expected int, got {other:?}"),
        }
    }
}
