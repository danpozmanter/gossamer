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

pub(crate) fn install_bufio_extras(globals: &mut Vec<(&'static str, Value)>) {
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

pub(crate) fn builtin_bufio_read_to_string(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "bufio::read_to_string", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    let read_path = path.clone();
    crate::comptime_gate::guard_read("bufio::read_to_string", &read_path)?;
    match gossamer_runtime::sched_global::run_blocking("bufio-read-to-string", move || {
        let file = std::fs::File::open(&read_path)?;
        let mut reader = bufio_std::Reader::new(file);
        let mut out = String::new();
        let mut chunk = [0u8; 4096];
        loop {
            let n = IoRead::read(&mut reader, &mut chunk)?;
            if n == 0 {
                break;
            }
            out.push_str(&String::from_utf8_lossy(&chunk[..n]));
        }
        Ok::<_, std::io::Error>(out)
    }) {
        Ok(Ok(out)) => Ok(ok_variant(Value::String(out.into()))),
        Ok(Err(e)) => Ok(err_variant(format!("read {path}: {e}"))),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_bufio_read_lines_of(args: &[Value]) -> RuntimeResult<Value> {
    let path = match arg_str_at(args, 0, "bufio::read_lines_of", "path") {
        Ok(s) => s,
        Err(v) => return Ok(v),
    };
    crate::comptime_gate::guard_read("bufio::read_lines_of", &path)?;
    match gossamer_runtime::sched_global::run_blocking("bufio-read-lines", move || {
        std::fs::read_to_string(path)
    }) {
        Ok(Ok(text)) => Ok(ok_variant(string_array(strings_std::lines(&text)))),
        Ok(Err(e)) => Ok(err_variant(e.to_string())),
        Err(e) => Ok(err_variant(e)),
    }
}

pub(crate) fn builtin_bufio_split_whitespace(args: &[Value]) -> RuntimeResult<Value> {
    let text = args.first().and_then(as_str).unwrap_or("");
    Ok(string_array(strings_std::split_whitespace(text)))
}

#[cfg(test)]
mod bufio_blocking_tests {
    use super::*;

    fn ok_payload(value: Value) -> Value {
        match value {
            Value::Variant(inner) if inner.name.as_str() == "Ok" => inner
                .fields
                .first()
                .cloned()
                .expect("Ok must carry a payload"),
            other => panic!("expected Ok result, found {other:?}"),
        }
    }

    #[test]
    fn file_read_builtins_preserve_text_and_lines() {
        let path = gossamer_runtime::platform::temp_dir().join(format!(
            "gossamer-bufio-{}.txt",
            gossamer_runtime::platform::process_id()
        ));
        std::fs::write(&path, "first\nsecond\n").expect("write fixture");
        let arg = Value::String(path.to_string_lossy().into_owned().into());

        assert!(matches!(
            ok_payload(builtin_bufio_read_to_string(std::slice::from_ref(&arg)).expect("read")),
            Value::String(ref text) if text.as_str() == "first\nsecond\n"
        ));
        assert!(matches!(
            ok_payload(builtin_bufio_read_lines_of(&[arg]).expect("lines")),
            Value::Array(ref lines) if lines.len() == 2
        ));
        std::fs::remove_file(path).expect("remove fixture");
    }
}

// ----------------------------------------------------------------------
// time extras
