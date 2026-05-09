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
use gossamer_std::net as net_std;
use gossamer_std::os as os_std;
use gossamer_std::path as path_std;
use gossamer_std::strconv as strconv_std;
use gossamer_std::strings as strings_std;
use gossamer_std::utf8 as utf8_std;

use crate::builtins::{
    BuiltinFnPub, as_str, err_variant, install_module_pub, none_variant, ok_variant, some_variant,
    value_to_int,
};
use crate::value::{MapKey, RuntimeResult, Value};

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
            ("rune_len", builtin_utf8_rune_len),
            ("is_valid", builtin_utf8_is_valid),
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
