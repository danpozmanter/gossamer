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

pub(crate) fn install_archive_zip(globals: &mut Vec<(&'static str, Value)>) {
    for (short, call) in [
        ("zip::read", builtin_archive_zip_read as BuiltinFnPub),
        ("zip::write", builtin_archive_zip_write),
    ] {
        let q: &'static str = Box::leak(format!("archive::{short}").into_boxed_str());
        globals.push((q, crate::builtins::builtin_pub(q, call)));
    }
}

pub(crate) fn zip_entry_to_value(entry: gossamer_std::archive::zip::ZipEntry) -> Value {
    Value::struct_(
        "archive::ZipEntry",
        Arc::new(vec![
            (Ident::new("name"), Value::String(entry.name.into())),
            (Ident::new("data"), bytes_to_array(entry.data)),
            (Ident::new("is_dir"), Value::Bool(entry.is_dir)),
        ]),
    )
}

pub(crate) fn builtin_archive_zip_read(args: &[Value]) -> RuntimeResult<Value> {
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

pub(crate) fn builtin_archive_zip_write(args: &[Value]) -> RuntimeResult<Value> {
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
