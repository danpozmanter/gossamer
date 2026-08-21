#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

use std::fmt::Write as _;
use std::os::raw::c_char;

use super::GosVec;

// ---------------------------------------------------------------
// slog - structured JSON-line logger on stderr.
//
// Emits one `{"level":"L","msg":"...","k":"v",...}` record per call
// so the compiled tier is byte-identical to the bytecode VM's
// `slog_emit` (gossamer-interp). The trailing key/value fields
// arrive as a `GosVec<String>` of already-display-rendered
// c-strings (the MIR slog lowering stringifies every field arg),
// paired key-then-value. Escaping mirrors the VM's `json_escape_str`.
// ---------------------------------------------------------------

/// Read the c-string at slot `i` of a `GosVec<String>` (each slot
/// holds a `*const c_char` packed as i64), or `""` if absent/null.
unsafe fn vec_str_at(vec: &GosVec, i: usize) -> &str {
    let p = unsafe { vec.ptr.add(i * (vec.elem_bytes as usize)) };
    let elem_ptr = unsafe { (p as *const i64).read_unaligned() } as *const c_char;
    unsafe { crate::c_abi::gos_str_arg_text(elem_ptr) }
}

/// JSON-escape a string the same way the VM's `json_escape_str` does.
fn json_escape_str(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if (ch as u32) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", ch as u32);
            }
            ch => out.push(ch),
        }
    }
    out
}

/// Writes one JSON-line record at `level` to stderr with `fields` as its
/// trailing key/value pairs. The record shape every tier reports a server
/// fault in, so a log line reads identically under `gos run` and a native
/// build.
#[cfg(not(target_arch = "wasm32"))]
pub(crate) fn emit_json_line(level: &str, msg: &str, fields: &[(&str, &str)]) {
    let mut line = String::with_capacity(64 + msg.len());
    line.push('{');
    let _ = write!(line, "\"level\":\"{level}\"");
    let _ = write!(line, ",\"msg\":\"{}\"", json_escape_str(msg));
    for (key, value) in fields {
        let _ = write!(
            line,
            ",\"{}\":\"{}\"",
            json_escape_str(key),
            json_escape_str(value),
        );
    }
    line.push('}');
    line.push('\n');
    eprint!("{line}");
}

unsafe fn slog_emit(level: &str, msg: *const c_char, fields: *const GosVec) {
    let m = if msg.is_null() {
        String::new()
    } else {
        unsafe { crate::c_abi::gos_str_arg_string(msg) }
    };
    let mut line = String::with_capacity(64 + m.len());
    line.push('{');
    let _ = write!(line, "\"level\":\"{level}\"");
    let _ = write!(line, ",\"msg\":\"{}\"", json_escape_str(&m));
    if !fields.is_null() {
        let vec = unsafe { &*fields };
        let pairs = (vec.len.max(0) as usize) / 2;
        for i in 0..pairs {
            let key = unsafe { vec_str_at(vec, 2 * i) };
            let value = unsafe { vec_str_at(vec, 2 * i + 1) };
            let _ = write!(
                line,
                ",\"{}\":\"{}\"",
                json_escape_str(key),
                json_escape_str(value),
            );
        }
    }
    line.push('}');
    line.push('\n');
    eprint!("{line}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_info(msg: *const c_char, fields: *const GosVec) {
    ffi_entry!((), {
        unsafe { slog_emit("INFO", msg, fields) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_warn(msg: *const c_char, fields: *const GosVec) {
    ffi_entry!((), {
        unsafe { slog_emit("WARN", msg, fields) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_error(msg: *const c_char, fields: *const GosVec) {
    ffi_entry!((), {
        unsafe { slog_emit("ERROR", msg, fields) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_debug(msg: *const c_char, fields: *const GosVec) {
    ffi_entry!((), {
        unsafe { slog_emit("DEBUG", msg, fields) };
    });
}
