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

use std::ffi::CStr;
use std::os::raw::c_char;

use super::*;

// ---------------------------------------------------------------
// BTreeMap - sorted-key map with String keys + i64 values.
// Mirrors the `gos_rt_map_*` shape but iterates in key order.
// ---------------------------------------------------------------

pub struct GosBtMap {
    inner: std::collections::BTreeMap<String, i64>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_new() -> *mut GosBtMap {
    ffi_entry!(std::ptr::null_mut(), {
        Box::into_raw(Box::new(GosBtMap {
            inner: std::collections::BTreeMap::new(),
        }))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_insert(m: *mut GosBtMap, key: *const c_char, value: i64) {
    ffi_entry!((), {
        if m.is_null() || key.is_null() {
            return;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let m = unsafe { &mut *m };
        m.inner.insert(k, value);
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_get_or(
    m: *const GosBtMap,
    key: *const c_char,
    def: i64,
) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() || key.is_null() {
            return def;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let m = unsafe { &*m };
        m.inner.get(&k).copied().unwrap_or(def)
    })
}

/// `BTreeMap::get(k) -> Option<i64>` packed as `*mut GosResult`
/// (disc=0 Some(v), disc=1 None). Mirrors `gos_rt_map_get_i64_opt`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_get(m: *const GosBtMap, key: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if m.is_null() || key.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let m = unsafe { &*m };
        match m.inner.get(&k) {
            Some(v) => unsafe { gos_rt_result_new(0, *v) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `BTreeMap::contains(k) -> bool`. Mirrors `gos_rt_map_contains_key_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_contains(m: *const GosBtMap, key: *const c_char) -> i32 {
    ffi_entry!(0, {
        if m.is_null() || key.is_null() {
            return 0;
        }
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        let m = unsafe { &*m };
        i32::from(m.inner.contains_key(&k))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_len(m: *const GosBtMap) -> i64 {
    ffi_entry!(-1, {
        if m.is_null() {
            return 0;
        }
        unsafe { (*m).inner.len() as i64 }
    })
}

/// Returns a fresh `*mut GosVec` of the BTreeMap's keys (in sort
/// order, since BTreeMap iterates ordered). Used by the
/// `for (k, v) in m.iter()` lowering - the codegen iterates the
/// keys vec by index and re-fetches the value via
/// `gos_rt_btmap_get_or` so each binding gets a real value, not
/// the ranked Vec header garbage the previous (missing) iter
/// dispatch printed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_keys(m: *const GosBtMap) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // STRING-typed: the snapshot owns its key strings, so
        // `gos_rt_vec_free` reclaims them even on early `break`.
        let v = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        if m.is_null() {
            return v;
        }
        let m = unsafe { &*m };
        for k in m.inner.keys() {
            let cstr = alloc_cstring(k.as_bytes());
            let ptr_val = cstr as i64;
            unsafe {
                gos_rt_vec_push(v, std::ptr::addr_of!(ptr_val).cast::<u8>());
            }
        }
        v
    })
}

/// Renders an i64-elem `Vec` as `[v0, v1, …]`. Returns a fresh
/// String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_i64(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 4);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let n = unsafe { (p as *const i64).read_unaligned() };
            out.push_str(&format!("{n}"));
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders an `f64`-elem `Vec` as `[v0, v1, …]`. Returns a fresh
/// String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_f64(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 6);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let n = unsafe { (p as *const f64).read_unaligned() };
            out.push_str(&format!("{n}"));
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a `bool`-elem `Vec` as `[true, false, …]`. Returns a
/// fresh String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_bool(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 6);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let b = unsafe { *p } != 0;
            out.push_str(if b { "true" } else { "false" });
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a `String`-elem `Vec` as `[s0, s1, …]`. Each element
/// in the Vec is a NUL-terminated `*const c_char`; we read it as
/// an 8-byte word and dereference. Returns a fresh String
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_string(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 8);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let s_ptr = unsafe {
                std::ptr::with_exposed_provenance::<c_char>((p as *const usize).read_unaligned())
            };
            if !s_ptr.is_null() {
                let cs = unsafe { std::ffi::CStr::from_ptr(s_ptr) };
                out.push_str(&cs.to_string_lossy());
            }
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a `Vec<Vec<i64>>` as `[[a, b], [c], …]`. Each
/// element is a `*mut GosVec` (8-byte slot); we recursively
/// stringify each inner `Vec<i64>`. Returns a fresh String
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_vec_i64(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 8);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let inner_ptr = unsafe {
                std::ptr::with_exposed_provenance::<GosVec>((p as *const usize).read_unaligned())
            };
            if inner_ptr.is_null() {
                out.push_str("[]");
            } else {
                let rendered = unsafe { gos_rt_vec_format_i64(inner_ptr) };
                if rendered.is_null() {
                    out.push_str("[]");
                } else {
                    let cs = unsafe { std::ffi::CStr::from_ptr(rendered) };
                    out.push_str(&cs.to_string_lossy());
                }
            }
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a `Vec<Vec<String>>` as `[[s0, s1], [s2], …]`. Each
/// element is a `*mut GosVec` (8-byte slot); we recursively
/// stringify each inner `Vec<String>`. Returns a fresh String
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_vec_string(v: *const GosVec) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"[]");
        }
        let vec = unsafe { &*v };
        let mut out = String::with_capacity(2 + (vec.len as usize) * 8);
        out.push('[');
        for i in 0..vec.len {
            if i > 0 {
                out.push_str(", ");
            }
            let p = unsafe { vec.ptr.add((i as usize) * (vec.elem_bytes as usize)) };
            let inner_ptr = unsafe {
                std::ptr::with_exposed_provenance::<GosVec>((p as *const usize).read_unaligned())
            };
            if inner_ptr.is_null() {
                out.push_str("[]");
            } else {
                let rendered = unsafe { gos_rt_vec_format_string(inner_ptr) };
                if rendered.is_null() {
                    out.push_str("[]");
                } else {
                    let cs = unsafe { std::ffi::CStr::from_ptr(rendered) };
                    out.push_str(&cs.to_string_lossy());
                }
            }
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[i64; N]` raw buffer as `[v0, v1, …]`. Used by
/// the print/format dispatch for fixed-size array literals
/// (`let xs = [a, b, c]`) whose storage is a flat heap blob, not a
/// `GosVec` with a header. Each element occupies one i64 slot
/// regardless of platform pointer width.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_i64(p: *const i64, len: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || len <= 0 {
            return alloc_cstring(b"[]");
        }
        let len_usize = len.max(0) as usize;
        let mut out = String::with_capacity(2 + len_usize * 4);
        out.push('[');
        for i in 0..len_usize {
            if i > 0 {
                out.push_str(", ");
            }
            let n = unsafe { p.add(i).read_unaligned() };
            out.push_str(&format!("{n}"));
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[f64; N]` raw buffer. Layout: each element is
/// stored at an 8-byte stride; we read the raw word as f64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_f64(p: *const f64, len: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || len <= 0 {
            return alloc_cstring(b"[]");
        }
        let len_usize = len.max(0) as usize;
        let mut out = String::with_capacity(2 + len_usize * 6);
        out.push('[');
        for i in 0..len_usize {
            if i > 0 {
                out.push_str(", ");
            }
            let n = unsafe { p.add(i).read_unaligned() };
            out.push_str(&format!("{n}"));
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[bool; N]` raw buffer. Each element is one
/// 8-byte slot; the low byte is the bool.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_bool(p: *const i64, len: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || len <= 0 {
            return alloc_cstring(b"[]");
        }
        let len_usize = len.max(0) as usize;
        let mut out = String::with_capacity(2 + len_usize * 6);
        out.push('[');
        for i in 0..len_usize {
            if i > 0 {
                out.push_str(", ");
            }
            let raw = unsafe { p.add(i).read_unaligned() };
            out.push_str(if raw & 1 != 0 { "true" } else { "false" });
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[String; N]` raw buffer. Each element is a
/// pointer to a NUL-terminated c-string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_string(
    p: *const *const c_char,
    len: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || len <= 0 {
            return alloc_cstring(b"[]");
        }
        let len_usize = len.max(0) as usize;
        let mut out = String::with_capacity(2 + len_usize * 8);
        out.push('[');
        for i in 0..len_usize {
            if i > 0 {
                out.push_str(", ");
            }
            let s_ptr = unsafe { p.add(i).read_unaligned() };
            if !s_ptr.is_null() {
                let cs = unsafe { std::ffi::CStr::from_ptr(s_ptr) };
                out.push_str(&cs.to_string_lossy());
            }
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[[i64; M]; N]` raw buffer as `[[..], [..], …]`.
/// The nested repeat/literal layout is `N * M` contiguous 8-byte
/// slots (inner arrays inline, no per-row header), so the row at
/// index `i` starts at slot `i * inner`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_arr_i64(
    p: *const i64,
    outer: i64,
    inner: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || outer <= 0 || inner <= 0 {
            return alloc_cstring(b"[]");
        }
        let (outer, inner) = (outer as usize, inner as usize);
        let mut out = String::with_capacity(2 + outer * (2 + inner * 4));
        out.push('[');
        for i in 0..outer {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('[');
            for j in 0..inner {
                if j > 0 {
                    out.push_str(", ");
                }
                let n = unsafe { p.add(i * inner + j).read_unaligned() };
                out.push_str(&format!("{n}"));
            }
            out.push(']');
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[[f64; M]; N]` raw buffer; same layout contract
/// as the i64 variant, reading each slot as an f64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_arr_f64(
    p: *const f64,
    outer: i64,
    inner: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || outer <= 0 || inner <= 0 {
            return alloc_cstring(b"[]");
        }
        let (outer, inner) = (outer as usize, inner as usize);
        let mut out = String::with_capacity(2 + outer * (2 + inner * 6));
        out.push('[');
        for i in 0..outer {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('[');
            for j in 0..inner {
                if j > 0 {
                    out.push_str(", ");
                }
                let n = unsafe { p.add(i * inner + j).read_unaligned() };
                out.push_str(&format!("{n}"));
            }
            out.push(']');
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// Renders a flat `[[bool; M]; N]` raw buffer; same layout contract
/// as the i64 variant, each slot's low bit is the bool.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_format_arr_bool(
    p: *const i64,
    outer: i64,
    inner: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if p.is_null() || outer <= 0 || inner <= 0 {
            return alloc_cstring(b"[]");
        }
        let (outer, inner) = (outer as usize, inner as usize);
        let mut out = String::with_capacity(2 + outer * (2 + inner * 7));
        out.push('[');
        for i in 0..outer {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('[');
            for j in 0..inner {
                if j > 0 {
                    out.push_str(", ");
                }
                let raw = unsafe { p.add(i * inner + j).read_unaligned() };
                out.push_str(if raw & 1 != 0 { "true" } else { "false" });
            }
            out.push(']');
        }
        out.push(']');
        alloc_cstring(out.as_bytes())
    })
}

/// `os::set_env(name, value) -> Result<(), errors::Error>`.
///
/// Mutates the calling process's environment so subsequently
/// spawned children inherit the new value. Routes through
/// `safe_env::set_env`, which serializes the POSIX `setenv`
/// against the rest of the runtime so concurrent goroutines
/// can't race on the env block.
///
/// MIR-side dispatch routes `os::set_env(...)` here so the
/// compiled tier matches the VM's behaviour. Without this binding
/// `os::set_env` lowered to a generic call against a non-existent
/// symbol - the compiled tier silently no-op'd, and downstream
/// `os::env(name)` returned the old value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_set_env(name: *const c_char, value: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if name.is_null() {
            let cs = std::ffi::CString::new("os::set_env: name is null").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let name_str = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        let value_str = if value.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
        };
        crate::safe_env::set_env(&name_str, &value_str);
        unsafe { gos_rt_result_new(0, 0) }
    })
}

/// `os::unset_env(name)` - companion to `gos_rt_os_set_env`.
/// Returns unit; failures (e.g. name with `=`) are silently
/// dropped to match the VM's lenient behaviour.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_unset_env(name: *const c_char) {
    ffi_entry!((), {
        if name.is_null() {
            return;
        }
        let name_str = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
        crate::safe_env::unset_env(&name_str);
    });
}

/// `exec::spawn(prog, args) -> Result<i64, errors::Error>`.
///
/// Non-blocking sibling of `exec::run`: launches `prog` with
/// `args` in the background, redirects stdin/stdout/stderr to
/// `/dev/null` so the child detaches from the calling tty, and
/// returns the child PID immediately. Wait/kill is the caller's
/// responsibility (see `gos_rt_exec_kill`). Used by long-running
/// daemon launches (e.g. an LLM-server program a tool spawns
/// before issuing HTTP requests against it).
///
/// Ok payload is the PID as `i64`; Err payload is a `*mut
/// GosError`. The Result aggregate matches the `Result<i64,
/// errors::Error>` shape MIR pins via the sentinel-DefId Adt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_exec_spawn(prog: *const c_char, args: *mut GosVec) -> i128 {
    ffi_entry!(0i128, {
        let prog_str = if prog.is_null() {
            let cs = std::ffi::CString::new("exec::spawn: program is null").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        } else {
            unsafe { CStr::from_ptr(prog).to_string_lossy().into_owned() }
        };
        let mut cmd_args: Vec<String> = Vec::new();
        if !args.is_null() {
            let v = unsafe { &*args };
            let elem_bytes = v.elem_bytes as usize;
            if elem_bytes != 0 && !v.ptr.is_null() {
                for i in 0..v.len {
                    let slot = unsafe { v.ptr.add((i as usize) * elem_bytes) };
                    let cstr_ptr = unsafe {
                        std::ptr::with_exposed_provenance::<c_char>(
                            (slot as *const usize).read_unaligned(),
                        )
                    };
                    if cstr_ptr.is_null() {
                        cmd_args.push(String::new());
                        continue;
                    }
                    let arg_str =
                        unsafe { CStr::from_ptr(cstr_ptr).to_string_lossy().into_owned() };
                    cmd_args.push(arg_str);
                }
            }
        }
        let mut command = std::process::Command::new(&prog_str);
        command.args(&cmd_args);
        command.stdin(std::process::Stdio::null());
        command.stdout(std::process::Stdio::null());
        command.stderr(std::process::Stdio::null());
        match command.spawn() {
            Ok(child) => {
                let pid = i64::from(child.id());
                // Detach: forget the Child handle so its Drop doesn't
                // wait. The user shells the kill via `gos_rt_exec_kill`
                // (or leaves the daemon running for the parent's
                // lifetime).
                std::mem::forget(child);
                unsafe { gos_rt_result_new(0, pid) }
            }
            Err(e) => {
                let msg = format!("exec::spawn({prog_str}): {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}

/// Sends SIGTERM (Unix) / TerminateProcess (Windows) to the PID
/// returned by `gos_rt_exec_spawn`. Companion to
/// `gos_rt_exec_spawn` for stop_server-style teardown paths.
/// Returns `true` on success, `false` if the kill syscall failed
/// (e.g. the process already exited, EPERM).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_exec_kill(pid: i64) -> i64 {
    ffi_entry!(-1, {
        if pid <= 0 {
            return 0;
        }
        #[cfg(unix)]
        {
            // SAFETY: libc::kill is safe to call with any pid /
            // signal; the kernel returns EINVAL / EPERM on failure
            // rather than crashing the caller.
            let rc = unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) };
            i64::from(rc == 0)
        }
        #[cfg(windows)]
        {
            // SAFETY: Win32 OpenProcess/TerminateProcess/CloseHandle.
            // CloseHandle is always called to prevent a handle leak.
            unsafe extern "system" {
                fn OpenProcess(desired_access: u32, inherit_handle: i32, process_id: u32) -> isize;
                fn TerminateProcess(process: isize, exit_code: u32) -> i32;
                fn CloseHandle(object: isize) -> i32;
            }
            const PROCESS_TERMINATE: u32 = 0x0001;
            let handle = unsafe { OpenProcess(PROCESS_TERMINATE, 0, pid as u32) };
            if handle == 0 {
                return 0;
            }
            let ok = unsafe { TerminateProcess(handle, 1) };
            unsafe { CloseHandle(handle) };
            i64::from(ok != 0)
        }
        #[cfg(not(any(unix, windows)))]
        {
            let _ = pid;
            0
        }
    })
}
