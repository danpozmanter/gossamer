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

pub(crate) fn install_encoding_binary(globals: &mut Vec<(&'static str, Value)>) {
    install_module_pub(
        "encoding::binary",
        &[
            ("get_u8", builtin_bin_get_u8),
            ("put_u8", builtin_bin_put_u8),
            ("put_uvarint", builtin_bin_put_uvarint),
            ("put_varint", builtin_bin_put_varint),
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
            ("get_u16_be_at", builtin_bin_get_u16_be_at),
            ("get_u16_le_at", builtin_bin_get_u16_le_at),
            ("get_u32_be_at", builtin_bin_get_u32_be_at),
            ("get_u32_le_at", builtin_bin_get_u32_le_at),
            ("get_u64_be_at", builtin_bin_get_u64_be_at),
            ("get_u64_le_at", builtin_bin_get_u64_le_at),
            ("put_u16_be_at", builtin_bin_put_u16_be_at),
            ("put_u16_le_at", builtin_bin_put_u16_le_at),
            ("put_u32_be_at", builtin_bin_put_u32_be_at),
            ("put_u32_le_at", builtin_bin_put_u32_le_at),
            ("put_u64_be_at", builtin_bin_put_u64_be_at),
            ("put_u64_le_at", builtin_bin_put_u64_le_at),
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
}

pub(crate) fn builtin_bin_get_u8(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.is_empty() {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u8(&bytes),
    ))))
}
pub(crate) fn builtin_bin_put_u8(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u8;
    let mut buf = [0u8; 1];
    gossamer_std::encoding::binary::put_u8(&mut buf, v);
    Ok(Value::Array(Arc::new(vec![Value::Int(i64::from(buf[0]))])))
}
pub(crate) fn builtin_bin_put_uvarint(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 10];
    let n = gossamer_std::encoding::binary::put_uvarint(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf[..n].iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_varint(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0);
    let mut buf = [0u8; 10];
    let n = gossamer_std::encoding::binary::put_varint(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf[..n].iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}

pub(crate) fn bytes_from_value(v: &Value) -> Vec<u8> {
    v.bytes_or_empty()
}

/// Byte length of a buffer value, without building the buffer. `None` for a
/// shape whose length only the general conversion can answer.
fn byte_len_of(v: &Value) -> Option<usize> {
    match v {
        Value::ByteArray(arr) => Some(arr.len()),
        Value::InlineByteArray(arr) => Some(arr.len()),
        Value::ByteVec(arr) => Some(arr.len()),
        Value::String(s) => Some(s.as_str().len()),
        Value::IntArray(arr) => Some(arr.len()),
        Value::Array(arr) => Some(arr.len()),
        _ => None,
    }
}

/// Copies `width` bytes from `start` out of a buffer value. `None` when an
/// element is not a byte, leaving the general conversion to decide.
fn read_byte_window(v: &Value, start: usize, width: usize) -> Option<Vec<u8>> {
    match v {
        Value::ByteArray(arr) => arr.get(start..start + width).map(<[u8]>::to_vec),
        Value::InlineByteArray(arr) => arr.get(start..start + width).map(<[u8]>::to_vec),
        Value::ByteVec(arr) => arr.get(start..start + width).map(<[u8]>::to_vec),
        Value::String(s) => s
            .as_str()
            .as_bytes()
            .get(start..start + width)
            .map(<[u8]>::to_vec),
        Value::IntArray(arr) => arr
            .get(start..start + width)?
            .iter()
            .map(|&n| u8::try_from(n).ok())
            .collect(),
        Value::Array(arr) => arr
            .get(start..start + width)?
            .iter()
            .map(|elem| match elem {
                Value::Int(n) => u8::try_from(*n).ok(),
                Value::Uint(n) => u8::try_from(*n).ok(),
                _ => None,
            })
            .collect(),
        _ => None,
    }
}

/// Reads the `width`-byte window at `offset`, or the diagnostic when it
/// is not entirely inside the buffer.
fn window_at(args: &[Value], width: usize) -> Result<Vec<u8>, Value> {
    let source = args.first().unwrap_or(&Value::Unit);
    let offset = args.get(1).and_then(value_to_int).unwrap_or(0);
    // Reading a few bytes must not cost the buffer's length: take the window
    // straight out of the value when its shape allows, and fall back to the
    // general conversion only for a buffer whose elements are not plain bytes.
    if let Some(len) = byte_len_of(source) {
        if offset < 0 {
            return Err(err_variant(
                "binary: offset must be non-negative".to_string(),
            ));
        }
        let start = offset as usize;
        let Some(end) = start.checked_add(width) else {
            return Err(err_variant(
                "binary: offset overflows the buffer".to_string(),
            ));
        };
        if end > len {
            return Err(err_variant(
                "binary: read past the end of the buffer".to_string(),
            ));
        }
        if let Some(window) = read_byte_window(source, start, width) {
            return Ok(window);
        }
    }
    let bytes = bytes_from_value(source);
    if offset < 0 {
        return Err(err_variant(
            "binary: offset must be non-negative".to_string(),
        ));
    }
    let start = offset as usize;
    let Some(end) = start.checked_add(width) else {
        return Err(err_variant(
            "binary: offset overflows the buffer".to_string(),
        ));
    };
    if end > bytes.len() {
        return Err(err_variant(
            "binary: read past the end of the buffer".to_string(),
        ));
    }
    Ok(bytes[start..end].to_vec())
}

/// Writes `bytes` at `offset` through the caller's own buffer, which the
/// VM hands over as a write-back cell for a `&mut` argument.
fn write_at(args: &[Value], offset_index: usize, bytes: &[u8]) -> Result<(), Value> {
    let offset = args.get(offset_index).and_then(value_to_int).unwrap_or(0);
    if offset < 0 {
        return Err(err_variant(
            "binary: offset must be non-negative".to_string(),
        ));
    }
    let Some(Value::MutCell(cell)) = args.first() else {
        return Err(err_variant(
            "binary: destination must be a `&mut` byte buffer".to_string(),
        ));
    };
    let mut guard = cell.lock();
    let mut buf = bytes_from_value(&guard);
    let start = offset as usize;
    let Some(end) = start.checked_add(bytes.len()) else {
        return Err(err_variant(
            "binary: offset overflows the buffer".to_string(),
        ));
    };
    if end > buf.len() {
        return Err(err_variant(
            "binary: write past the end of the buffer".to_string(),
        ));
    }
    buf[start..end].copy_from_slice(bytes);
    *guard = Value::ByteVec(Arc::new(buf));
    Ok(())
}

macro_rules! bin_get_at {
    ($name:ident, $ty:ty, $from:ident, $n:expr, $doc:literal) => {
        #[doc = $doc]
        pub(crate) fn $name(args: &[Value]) -> RuntimeResult<Value> {
            let window = match window_at(args, $n) {
                Ok(window) => window,
                Err(v) => return Ok(v),
            };
            let mut arr = [0u8; $n];
            arr.copy_from_slice(&window);
            Ok(ok_variant(Value::Int(<$ty>::$from(arr) as i64)))
        }
    };
}

bin_get_at!(
    builtin_bin_get_u16_be_at,
    u16,
    from_be_bytes,
    2,
    "`binary::get_u16_be_at(bytes, offset) -> Result<u16, Error>`."
);
bin_get_at!(
    builtin_bin_get_u16_le_at,
    u16,
    from_le_bytes,
    2,
    "`binary::get_u16_le_at(bytes, offset) -> Result<u16, Error>`."
);
bin_get_at!(
    builtin_bin_get_u32_be_at,
    u32,
    from_be_bytes,
    4,
    "`binary::get_u32_be_at(bytes, offset) -> Result<u32, Error>`."
);
bin_get_at!(
    builtin_bin_get_u32_le_at,
    u32,
    from_le_bytes,
    4,
    "`binary::get_u32_le_at(bytes, offset) -> Result<u32, Error>`."
);
bin_get_at!(
    builtin_bin_get_u64_be_at,
    u64,
    from_be_bytes,
    8,
    "`binary::get_u64_be_at(bytes, offset) -> Result<u64, Error>`."
);
bin_get_at!(
    builtin_bin_get_u64_le_at,
    u64,
    from_le_bytes,
    8,
    "`binary::get_u64_le_at(bytes, offset) -> Result<u64, Error>`."
);

macro_rules! bin_put_at {
    ($name:ident, $ty:ty, $to:ident, $doc:literal) => {
        #[doc = $doc]
        pub(crate) fn $name(args: &[Value]) -> RuntimeResult<Value> {
            let value = args.get(2).and_then(value_to_int).unwrap_or(0) as $ty;
            match write_at(args, 1, &value.$to()) {
                Ok(()) => Ok(ok_variant(Value::Unit)),
                Err(v) => Ok(v),
            }
        }
    };
}

bin_put_at!(
    builtin_bin_put_u16_be_at,
    u16,
    to_be_bytes,
    "`binary::put_u16_be_at(buf, offset, value) -> Result<(), Error>`."
);
bin_put_at!(
    builtin_bin_put_u16_le_at,
    u16,
    to_le_bytes,
    "`binary::put_u16_le_at(buf, offset, value) -> Result<(), Error>`."
);
bin_put_at!(
    builtin_bin_put_u32_be_at,
    u32,
    to_be_bytes,
    "`binary::put_u32_be_at(buf, offset, value) -> Result<(), Error>`."
);
bin_put_at!(
    builtin_bin_put_u32_le_at,
    u32,
    to_le_bytes,
    "`binary::put_u32_le_at(buf, offset, value) -> Result<(), Error>`."
);
bin_put_at!(
    builtin_bin_put_u64_be_at,
    u64,
    to_be_bytes,
    "`binary::put_u64_be_at(buf, offset, value) -> Result<(), Error>`."
);
bin_put_at!(
    builtin_bin_put_u64_le_at,
    u64,
    to_le_bytes,
    "`binary::put_u64_le_at(buf, offset, value) -> Result<(), Error>`."
);

pub(crate) fn builtin_bin_put_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u16;
    let mut buf = [0u8; 2];
    gossamer_std::encoding::binary::put_u16_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u32;
    let mut buf = [0u8; 4];
    gossamer_std::encoding::binary::put_u32_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_be(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_put_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let v = args.get(1).and_then(value_to_int).unwrap_or(0) as u64;
    let mut buf = [0u8; 8];
    gossamer_std::encoding::binary::put_u64_le(&mut buf, v);
    Ok(Value::Array(Arc::new(
        buf.iter().map(|&b| Value::Int(i64::from(b))).collect(),
    )))
}
pub(crate) fn builtin_bin_get_u16_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_be(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u16_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 2 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u16_le(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u32_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_be(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u32_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 4 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(i64::from(
        gossamer_std::encoding::binary::get_u32_le(&bytes),
    ))))
}
pub(crate) fn builtin_bin_get_u64_be(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_be(&bytes) as i64,
    )))
}
pub(crate) fn builtin_bin_get_u64_le(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    if bytes.len() < 8 {
        return Ok(err_variant("binary: buffer too short".to_string()));
    }
    Ok(ok_variant(Value::Int(
        gossamer_std::encoding::binary::get_u64_le(&bytes) as i64,
    )))
}
pub(crate) fn builtin_bin_uvarint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::uvarint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::from(vec![
            Value::Int(v as i64),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}
pub(crate) fn builtin_bin_varint(args: &[Value]) -> RuntimeResult<Value> {
    let bytes = bytes_from_value(args.first().unwrap_or(&Value::Unit));
    match gossamer_std::encoding::binary::varint(&bytes) {
        Ok((v, n)) => Ok(ok_variant(Value::Tuple(Arc::from(vec![
            Value::Int(v),
            Value::Int(n as i64),
        ])))),
        Err(e) => Ok(err_variant(format!("{e}"))),
    }
}

// ----------------------------------------------------------------------
// encoding::csv
