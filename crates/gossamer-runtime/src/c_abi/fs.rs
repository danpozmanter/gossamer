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

use std::collections::HashMap;
use std::ffi::CStr;
use std::io::{Read, Write};
use std::os::raw::c_char;
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use super::*;
use parking_lot::Mutex;

#[derive(Default)]
#[allow(clippy::struct_excessive_bools)]
struct GosOpenOptions {
    read: bool,
    write: bool,
    append: bool,
    truncate: bool,
    create: bool,
    create_new: bool,
}

static FILE_HANDLES: Mutex<Option<HashMap<i64, Arc<Mutex<std::fs::File>>>>> = Mutex::new(None);
static OPEN_OPTIONS_HANDLES: Mutex<Option<HashMap<i64, Arc<Mutex<GosOpenOptions>>>>> =
    Mutex::new(None);
static NEXT_FS_HANDLE: AtomicI64 = AtomicI64::new(1);
static NEXT_TEMP_RESOURCE: AtomicI64 = AtomicI64::new(1);

fn next_fs_handle() -> i64 {
    NEXT_FS_HANDLE.fetch_add(1, Ordering::Relaxed)
}

fn insert_file(file: std::fs::File) -> i64 {
    let h = next_fs_handle();
    FILE_HANDLES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(h, Arc::new(Mutex::new(file)));
    h
}

fn file_clone(h: i64) -> Option<Arc<Mutex<std::fs::File>>> {
    FILE_HANDLES
        .lock()
        .as_ref()
        .and_then(|m| m.get(&h).cloned())
}

/// Take an independent OS handle before queueing a blocking operation. This
/// keeps both the registry lock and the per-handle mutex out of the blocking
/// pool closure, so another goroutine can close or use the original handle.
fn duplicate_file(h: i64) -> std::io::Result<Option<std::fs::File>> {
    file_clone(h)
        .map(|file| file.lock().try_clone())
        .transpose()
}

fn insert_open_options(opts: GosOpenOptions) -> i64 {
    let h = next_fs_handle();
    OPEN_OPTIONS_HANDLES
        .lock()
        .get_or_insert_with(HashMap::new)
        .insert(h, Arc::new(Mutex::new(opts)));
    h
}

fn open_options_clone(h: i64) -> Option<Arc<Mutex<GosOpenOptions>>> {
    OPEN_OPTIONS_HANDLES
        .lock()
        .as_ref()
        .and_then(|m| m.get(&h).cloned())
}

fn apply_open_options(opts: &GosOpenOptions) -> std::fs::OpenOptions {
    let mut out = std::fs::OpenOptions::new();
    out.read(opts.read)
        .write(opts.write)
        .append(opts.append)
        .truncate(opts.truncate)
        .create(opts.create)
        .create_new(opts.create_new);
    out
}

fn fs_err(msg: &str) -> i128 {
    let cs = std::ffi::CString::new(msg).unwrap_or_default();
    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
    unsafe { gos_rt_result_new(1, err as i64) }
}

fn fs_io_err(err: &std::io::Error, context: &str) -> i128 {
    let msg = classify_io_error(err, context);
    fs_err(&msg)
}

fn temp_prefix(prefix: *const c_char, operation: &str) -> Result<String, i128> {
    if prefix.is_null() {
        return Err(fs_err(&format!("{operation}: null prefix")));
    }
    let prefix = unsafe { CStr::from_ptr(prefix).to_string_lossy().into_owned() };
    if prefix.contains(['/', '\\', '\0']) || matches!(prefix.as_str(), "." | "..") {
        return Err(fs_err(
            "temporary-resource prefix must be a single path component",
        ));
    }
    Ok(prefix)
}

fn temp_resource_path(prefix: &str) -> std::path::PathBuf {
    let n = NEXT_TEMP_RESOURCE.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    std::env::temp_dir().join(format!(
        "gossamer-{prefix}-{:x}-{nanos:x}-{n}",
        std::process::id()
    ))
}

// ---------------------------------------------------------------
// fs / path helpers - read_to_string, write, create_dir_all,
// path::join. Mirror Rust std::fs minus the typed Error.
// Filesystem syscalls are synchronous in every host kernel. Core
// reads, writes, and handle operations run through `run_blocking`,
// which parks a compiled goroutine while an OS thread performs the
// syscall. Pointer decoding and Gossamer heap allocation stay on the
// calling goroutine.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_read_to_string(path: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if path.is_null() {
            return alloc_cstring(b"");
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        match crate::sched_global::run_blocking("fs-read-string", move || {
            std::fs::read_to_string(p)
        }) {
            Ok(Ok(text)) => alloc_cstring(text.as_bytes()),
            Ok(Err(_)) | Err(_) => alloc_cstring(b""),
        }
    })
}

/// `fs::read_to_string(path) -> Result<String, errors::Error>`. Result-shaped
/// counterpart of the bare-string `gos_rt_fs_read_to_string` (which returns ""
/// on failure and cannot express an error); the compiled tiers route
/// `fs::read_to_string` here so a missing / unreadable path propagates `Err`
/// identically to the VM, mirroring how `fs::read` (`gos_rt_fs_read_bytes_result`)
/// already builds its error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_read_to_string_result(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("read_to_string: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-read-string", move || {
            std::fs::read_to_string(p)
        }) {
            Ok(Ok(text)) => {
                let s = alloc_cstring(text.as_bytes());
                unsafe { gos_rt_result_new(0, s as i64) }
            }
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_write(path: *const c_char, contents: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() || contents.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let c = unsafe { CStr::from_ptr(contents).to_bytes().to_vec() };
        i64::from(
            crate::sched_global::run_blocking("fs-write", move || std::fs::write(p, c))
                .is_ok_and(|result| result.is_ok()),
        )
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_create_dir_all(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        i64::from(
            crate::sched_global::run_blocking("fs-mkdir-all", move || std::fs::create_dir_all(p))
                .is_ok_and(|result| result.is_ok()),
        )
    })
}

/// `os::remove_file(path) -> Result<(), IoError>`. Returns a bool
/// in the compiled tier (the same shape every other os/fs mutator
/// uses). Without an explicit binding the call falls through to a
/// non-existent symbol and corrupts the destination slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_remove_file(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_str() }.unwrap_or("");
        i64::from(
            crate::sched_global::run_blocking("fs-remove-file", move || std::fs::remove_file(p))
                .is_ok_and(|result| result.is_ok()),
        )
    })
}

/// Mirrors `gossamer_std::io::IoError::from_std` classification so the
/// native fs error text matches the interp tier byte-for-byte
/// (`not found: {path}` / `permission denied: {path}` / `io: {path}: {err}`).
/// `gossamer-runtime` cannot depend on `gossamer-std` (the dependency
/// points the other way), so the three-arm mapping is replicated here;
/// the cross-tier fixture suite pins the parity.
fn classify_io_error(err: &std::io::Error, context: &str) -> String {
    use std::io::ErrorKind;
    match err.kind() {
        ErrorKind::NotFound => format!("not found: {context}"),
        ErrorKind::PermissionDenied => format!("permission denied: {context}"),
        _ => format!("io: {context}: {err}"),
    }
}

/// `fs::File::open(path) -> Result<File, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_open(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            return fs_err("File::open: null path");
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-open", move || std::fs::File::open(p)) {
            Ok(Ok(file)) => unsafe { gos_rt_result_new(0, insert_file(file)) },
            Ok(Err(e)) => fs_io_err(&e, &context),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::create(path) -> Result<File, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_create(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            return fs_err("File::create: null path");
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-create", move || std::fs::File::create(p)) {
            Ok(Ok(file)) => unsafe { gos_rt_result_new(0, insert_file(file)) },
            Ok(Err(e)) => fs_io_err(&e, &context),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::temp_dir(prefix) -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_temp_dir(prefix: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let prefix = match temp_prefix(prefix, "fs::temp_dir") {
            Ok(prefix) => prefix,
            Err(error) => return error,
        };
        let path = temp_resource_path(&prefix);
        let context = path.to_string_lossy().into_owned();
        match crate::sched_global::run_blocking("fs-temp-dir", move || std::fs::create_dir(&path)) {
            Ok(Ok(())) => {
                let path = alloc_cstring(context.as_bytes());
                unsafe { gos_rt_result_new(0, path as i64) }
            }
            Ok(Err(error)) => fs_io_err(&error, &context),
            Err(error) => fs_err(&error),
        }
    })
}

/// `fs::temp_file(prefix) -> Result<(File, String), Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_temp_file(prefix: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let prefix = match temp_prefix(prefix, "fs::temp_file") {
            Ok(prefix) => prefix,
            Err(error) => return error,
        };
        let path = temp_resource_path(&prefix);
        let context = path.to_string_lossy().into_owned();
        match crate::sched_global::run_blocking("fs-temp-file", move || {
            std::fs::OpenOptions::new()
                .create_new(true)
                .read(true)
                .write(true)
                .open(path)
        }) {
            Ok(Ok(file)) => {
                let pair = unsafe { gos_rt_gc_alloc(16) }.cast::<i64>();
                if pair.is_null() {
                    return fs_err("fs::temp_file: allocation failed");
                }
                unsafe {
                    *pair = insert_file(file);
                    *pair.add(1) = alloc_cstring(context.as_bytes()) as i64;
                    gos_rt_result_new(0, pair as i64)
                }
            }
            Ok(Err(error)) => fs_io_err(&error, &context),
            Err(error) => fs_err(&error),
        }
    })
}

/// `fs::OpenOptions::new() -> OpenOptions`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_fs_open_options_new() -> i64 {
    insert_open_options(GosOpenOptions::default())
}

macro_rules! open_option_setter {
    ($name:ident, $field:ident) => {
        #[unsafe(no_mangle)]
        pub unsafe extern "C" fn $name(h: i64, enabled: i32) -> i64 {
            ffi_entry!(h, {
                if let Some(opts) = open_options_clone(h) {
                    opts.lock().$field = enabled != 0;
                }
                h
            })
        }
    };
}

open_option_setter!(gos_rt_fs_open_options_read, read);
open_option_setter!(gos_rt_fs_open_options_write, write);
open_option_setter!(gos_rt_fs_open_options_append, append);
open_option_setter!(gos_rt_fs_open_options_truncate, truncate);
open_option_setter!(gos_rt_fs_open_options_create, create);
open_option_setter!(gos_rt_fs_open_options_create_new, create_new);

/// `fs::OpenOptions::open(path) -> Result<File, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_open_options_open(h: i64, path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            return fs_err("OpenOptions::open: null path");
        }
        let Some(opts) = open_options_clone(h) else {
            return fs_err("OpenOptions::open: stale handle");
        };
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let open = apply_open_options(&opts.lock());
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-open-options", move || open.open(p)) {
            Ok(Ok(file)) => unsafe { gos_rt_result_new(0, insert_file(file)) },
            Ok(Err(e)) => fs_io_err(&e, &context),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::read(max) -> Result<Vec<u8>, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_read(h: i64, max: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(mut file) = (match duplicate_file(h) {
            Ok(file) => file,
            Err(e) => return fs_io_err(&e, "File::read"),
        }) else {
            return fs_err("File::read: stale handle");
        };
        let cap = max.clamp(1, 1 << 24) as usize;
        match crate::sched_global::run_blocking("fs-file-read", move || {
            let mut buf = vec![0u8; cap];
            file.read(&mut buf).map(|n| {
                buf.truncate(n);
                buf
            })
        }) {
            Ok(Ok(buf)) => unsafe {
                gos_rt_result_new(0, super::encoding::bytes_to_gosvec(&buf) as i64)
            },
            Ok(Err(e)) => fs_io_err(&e, "File::read"),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::read_to_string() -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_read_to_string(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(mut file) = (match duplicate_file(h) {
            Ok(file) => file,
            Err(e) => return fs_io_err(&e, "File::read_to_string"),
        }) else {
            return fs_err("File::read_to_string: stale handle");
        };
        match crate::sched_global::run_blocking("fs-file-read-string", move || {
            let mut text = String::new();
            file.read_to_string(&mut text).map(|_| text)
        }) {
            Ok(Ok(text)) => unsafe { gos_rt_result_new(0, alloc_cstring(text.as_bytes()) as i64) },
            Ok(Err(e)) => fs_io_err(&e, "File::read_to_string"),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::write(data: Vec<u8>) -> Result<i64, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_write(
    h: i64,
    data: *const crate::c_abi::vec::GosVec,
) -> i128 {
    ffi_entry!(0i128, {
        let Some(mut file) = (match duplicate_file(h) {
            Ok(file) => file,
            Err(e) => return fs_io_err(&e, "File::write"),
        }) else {
            return fs_err("File::write: stale handle");
        };
        let bytes = unsafe { super::encoding::gosvec_u8(data) };
        let len = bytes.len();
        match crate::sched_global::run_blocking("fs-file-write", move || file.write_all(&bytes)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, len as i64) },
            Ok(Err(e)) => fs_io_err(&e, "File::write"),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::flush() -> Result<(), Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_flush(h: i64) -> i128 {
    ffi_entry!(0i128, {
        let Some(mut file) = (match duplicate_file(h) {
            Ok(file) => file,
            Err(e) => return fs_io_err(&e, "File::flush"),
        }) else {
            return fs_err("File::flush: stale handle");
        };
        match crate::sched_global::run_blocking("fs-file-flush", move || file.flush()) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => fs_io_err(&e, "File::flush"),
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::File::close()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_file_close(h: i64) {
    ffi_entry!((), {
        if let Some(files) = FILE_HANDLES.lock().as_mut() {
            files.remove(&h);
        }
    });
}

/// `os::write_file(path, contents) -> Result<(), IoError>` - Result
/// shape. Used when the call site chains `.map_err(...)` (askq's
/// `save_history`); the bool-returning `gos_rt_fs_write` would
/// trip cranelift's verifier when `.map_err` then tried to feed
/// the i8 result into a `*mut GosResult` slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_write_file_result(
    path: *const c_char,
    contents: *const c_char,
) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() || contents.is_null() {
            let cs = std::ffi::CString::new("write_file: null arg").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let c = unsafe { CStr::from_ptr(contents).to_bytes().to_vec() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-write-file", move || std::fs::write(p, c)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::write_file(path, bytes: &[u8]) -> Result<(), IoError>` - `Vec<u8>`
/// payload variant. The MIR dispatcher picks this helper over the
/// c-string-shaped one when the contents argument types as `Vec<u8>`
/// or `&[u8]`, so binary writes (e.g. saving a downloaded image)
/// preserve every byte including embedded NULs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_write_file_bytes_result(
    path: *const c_char,
    contents: *const crate::c_abi::vec::GosVec,
) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() || contents.is_null() {
            let cs = std::ffi::CString::new("write_file: null arg").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let v = unsafe { &*contents };
        // Width-aware payload extraction: an i64-stride Vec carrying
        // u8 values needs the low byte of each slot, not a flat
        // memcpy. `elem_bytes == 1` is the proper Vec<u8> case.
        let bytes_buf: Vec<u8>;
        let bytes: &[u8] = if v.ptr.is_null() || v.len == 0 {
            &[]
        } else if v.elem_bytes == 1 {
            unsafe { std::slice::from_raw_parts(v.ptr.as_ptr(), v.len as usize) }
        } else if v.elem_bytes == 8 {
            let count = v.len as usize;
            bytes_buf = (0..count)
                .map(|i| {
                    let slot_ptr = unsafe { v.ptr.add(i * 8) };
                    unsafe { *slot_ptr }
                })
                .collect();
            &bytes_buf
        } else {
            unsafe {
                std::slice::from_raw_parts(v.ptr.as_ptr(), v.len as usize * v.elem_bytes as usize)
            }
        };
        let context = p.clone();
        let bytes = bytes.to_vec();
        match crate::sched_global::run_blocking("fs-write-bytes", move || std::fs::write(p, bytes))
        {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::read_file(path) -> Result<Vec<u8>, IoError>` - bytes-shaped
/// read counterpart to `gos_rt_fs_read_to_string`. The Ok payload
/// is a `*mut GosVec` with elem_bytes=1 holding the raw file
/// content (preserves embedded NULs / non-UTF-8 bytes). Callers
/// who want a String should use `os::read_file_to_string` /
/// `fs::read_to_string` instead.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_read_bytes_result(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("read_file: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-read-bytes", move || std::fs::read(p)) {
            Ok(Ok(bytes)) => {
                let len_i64 = bytes.len() as i64;
                let v = unsafe { crate::c_abi::vec::gos_rt_vec_with_capacity(1, len_i64) };
                if !bytes.is_empty() {
                    let vref = unsafe { &mut *v };
                    if !vref.ptr.is_null() {
                        unsafe {
                            std::ptr::copy_nonoverlapping(
                                bytes.as_ptr(),
                                vref.ptr.as_ptr(),
                                bytes.len(),
                            );
                        }
                        vref.len = len_i64;
                    }
                }
                unsafe { gos_rt_result_new(0, v as i64) }
            }
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::mkdir_all(path) -> Result<(), IoError>` - Result shape, for
/// `.map_err(...)` chains.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_mkdir_all_result(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("mkdir_all: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-mkdir-all", move || std::fs::create_dir_all(p))
        {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::remove_all(path) -> Result<(), IoError>` - removes a directory tree or file.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_remove_dir_all_result(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("remove_all: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-remove-dir-all", move || {
            std::fs::remove_dir_all(p)
        }) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::create_dir(path) -> Result<(), Error>` - non-recursive
/// directory creation; fails when a parent component is missing.
/// Use `fs::create_dir_all` for the recursive form. Matches the
/// interp's `fs::create_dir` builtin (`std::fs::create_dir`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_create_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("create_dir: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-create-dir", move || std::fs::create_dir(p)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `fs::remove_dir(path) -> Result<(), Error>` - removes a single
/// empty directory; fails if it is non-empty. Use `fs::remove_dir_all`
/// / `fs::remove_all` for a recursive tree removal. Matches the
/// interp's `fs::remove_dir` / `os::remove_dir` builtin
/// (`std::fs::remove_dir`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_remove_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("remove_dir: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-remove-dir", move || std::fs::remove_dir(p)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::remove_file(path) -> Result<(), IoError>` - Result shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_remove_file_result(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("remove_file: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-remove-file", move || std::fs::remove_file(p)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::rename(from, to)` / `fs::rename(from, to)` -> Result<(), Error>.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_rename(from: *const c_char, to: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if from.is_null() || to.is_null() {
            let cs = std::ffi::CString::new("rename: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let f = unsafe { CStr::from_ptr(from).to_string_lossy().into_owned() };
        let t = unsafe { CStr::from_ptr(to).to_string_lossy().into_owned() };
        let context = f.clone();
        match crate::sched_global::run_blocking("fs-rename", move || std::fs::rename(f, t)) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `env::set_current_dir(path) -> Result<(), Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_env_set_current_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let p = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        let context = p.clone();
        match crate::sched_global::run_blocking("env-set-current-dir", move || {
            std::env::set_current_dir(p)
        }) {
            Ok(Ok(())) => unsafe { gos_rt_result_new(0, 0) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::arch() -> String` - target CPU architecture (e.g. `"x86_64"`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_os_arch() -> *const c_char {
    alloc_cstring(std::env::consts::ARCH.as_bytes()).cast_const()
}

/// `os::family() -> String` - target OS family (e.g. `"unix"`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_os_family() -> *const c_char {
    alloc_cstring(std::env::consts::FAMILY.as_bytes()).cast_const()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_join(a: *const c_char, b: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let a = if a.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(a).to_str() }.unwrap_or("")
        };
        let b = if b.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(b).to_str() }.unwrap_or("")
        };
        alloc_cstring(path_join(a, b).as_bytes())
    })
}

/// Final path component helper for `path::file_name`.
/// Inlined here so the runtime
/// crate stays free of a dep on `gossamer-std`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_base(p: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let basename: &str = match s.rfind('/') {
            None => s,
            Some(idx) => &s[idx + 1..],
        };
        alloc_cstring(basename.as_bytes())
    })
}

/// Parent directory helper for `path::parent`.
/// Returns `"."` when no separator is present.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_dir(p: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let dirname: &str = match s.rfind('/') {
            None => ".",
            Some(0) => "/",
            Some(idx) => &s[..idx],
        };
        alloc_cstring(dirname.as_bytes())
    })
}

/// `path::split(p) -> (String, String)` - splits into a
/// `(directory, file)` pair. Returns a heap `*mut StrPair` (two
/// c-string slots); the MIR types the return as `(String, String)`
/// so a destructure reads slot 0 (dir) and slot 1 (file). The
/// directory carries no trailing separator unless the path is `/`,
/// matching `gossamer_std::path::split`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_split(p: *const c_char) -> *mut i64 {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let (dir, file): (&str, &str) = match s.rfind('/') {
            None => ("", s),
            Some(0) => ("/", &s[1..]),
            Some(idx) => (&s[..idx], &s[idx + 1..]),
        };
        #[repr(C)]
        struct StrPair {
            a: i64,
            b: i64,
        }
        Box::into_raw(Box::new(StrPair {
            a: alloc_cstring(dir.as_bytes()) as i64,
            b: alloc_cstring(file.as_bytes()) as i64,
        }))
        .cast()
    })
}

/// `path::extension(p) -> Option<String>` - extension with the leading
/// dot wrapped in `Some`, or `None` if absent / the dot is at the
/// very start of the file name. Mirrors the interp / stdlib
/// `path::extension` Option-returning shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_ext(p: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let basename: &str = match s.rfind('/') {
            None => s,
            Some(idx) => &s[idx + 1..],
        };
        let ext_str: &str = match basename.rfind('.') {
            None | Some(0) => "",
            Some(idx) => &basename[idx..],
        };
        if ext_str.is_empty() {
            unsafe { gos_rt_result_new(1, 0) }
        } else {
            let cstr = alloc_cstring(ext_str.as_bytes()) as i64;
            unsafe { gos_rt_result_new(0, cstr) }
        }
    })
}

/// `path::parent(p) -> Option<String>` - drops the trailing
/// component. Returns None when `p` has no parent (root or
/// single-component path). Mirrors `gossamer_std::path::parent`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_parent(p: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let trimmed = s.trim_end_matches('/');
        match trimmed.rfind('/') {
            None => unsafe { gos_rt_result_new(1, 0) },
            Some(0) => {
                let cstr = alloc_cstring(b"/") as i64;
                unsafe { gos_rt_result_new(0, cstr) }
            }
            Some(idx) => {
                let cstr = alloc_cstring(&trimmed.as_bytes()[..idx]) as i64;
                unsafe { gos_rt_result_new(0, cstr) }
            }
        }
    })
}

/// `path::file_stem(p) -> Option<String>` - basename minus the
/// extension. Returns None when the basename is empty.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_stem(p: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let basename = match s.rfind('/') {
            None => s,
            Some(idx) => &s[idx + 1..],
        };
        if basename.is_empty() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let stem = match basename.rfind('.') {
            None | Some(0) => basename,
            Some(idx) => &basename[..idx],
        };
        let cstr = alloc_cstring(stem.as_bytes()) as i64;
        unsafe { gos_rt_result_new(0, cstr) }
    })
}

/// `path::file_name(p) -> Option<String>` - last component.
/// Returns None for empty paths or trailing-slash directories.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_file_name(p: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let s = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        let basename = match s.rfind('/') {
            None => s,
            Some(idx) => &s[idx + 1..],
        };
        if basename.is_empty() {
            unsafe { gos_rt_result_new(1, 0) }
        } else {
            let cstr = alloc_cstring(basename.as_bytes()) as i64;
            unsafe { gos_rt_result_new(0, cstr) }
        }
    })
}

/// `path::normalize(p) -> String`. Lexical
/// cleanup mirroring `gossamer_std::path::normalize` (no I/O); the
/// runtime crate stays free of a dep on `gossamer-std`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_clean(p: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let path = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        alloc_cstring(path_clean(path).as_bytes())
    })
}

/// `path::is_absolute(p) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_is_absolute(p: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let path = if p.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(p).to_str() }.unwrap_or("")
        };
        i32::from(path.starts_with('/'))
    })
}

/// `path::starts_with(p, prefix) -> bool` - path-aware prefix test.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_has_prefix(p: *const c_char, prefix: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let path = if p.is_null() {
            String::new()
        } else {
            path_clean(unsafe { CStr::from_ptr(p).to_str() }.unwrap_or(""))
        };
        let prefix = if prefix.is_null() {
            String::new()
        } else {
            path_clean(unsafe { CStr::from_ptr(prefix).to_str() }.unwrap_or(""))
        };
        if path == prefix {
            return 1;
        }
        let matched = if prefix.ends_with('/') {
            path.starts_with(&prefix)
        } else {
            let mut candidate = prefix.clone();
            candidate.push('/');
            path.starts_with(&candidate)
        };
        i32::from(matched)
    })
}

/// `os::copy(src, dst) / fs::copy(src, dst) -> Result<i64, Error>`
/// - copies the file contents and returns the byte count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_copy(src: *const c_char, dst: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let src = if src.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(src).to_string_lossy().into_owned() }
        };
        let dst = if dst.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(dst).to_string_lossy().into_owned() }
        };
        let context = format!("{src} -> {dst}");
        match crate::sched_global::run_blocking("fs-copy", move || std::fs::copy(src, dst)) {
            Ok(Ok(n)) => unsafe { gos_rt_result_new(0, i64::try_from(n).unwrap_or(i64::MAX)) },
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// `os::canonicalize(p) / fs::canonicalize(p) -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_canonicalize(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let p = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        let context = p.clone();
        match crate::sched_global::run_blocking("fs-canonicalize", move || std::fs::canonicalize(p))
        {
            Ok(Ok(abs)) => {
                let s = abs.to_string_lossy().into_owned();
                let ptr = alloc_cstring(s.as_bytes()) as i64;
                unsafe { gos_rt_result_new(0, ptr) }
            }
            Ok(Err(e)) => {
                let msg = classify_io_error(&e, &context);
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => fs_err(&e),
        }
    })
}

/// Builds a `Result::Ok(*mut GosVec)` carrying owned strings.
/// STRING-typed: the vec owns each element, so `gos_rt_vec_free`
/// deep-frees them.
fn ok_str_vec(parts: &[String]) -> i128 {
    let vec = unsafe {
        crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
            8,
            parts.len() as i64,
            crate::c_abi::vec::vec_elem_kind::STRING,
        )
    };
    for p in parts {
        let pv = alloc_cstring(p.as_bytes()) as i64;
        unsafe { gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>()) };
    }
    unsafe { gos_rt_result_new(0, vec as i64) }
}

fn err_io(e: &std::io::Error) -> i128 {
    let cs = std::ffi::CString::new(format!("{e}")).unwrap_or_default();
    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
    unsafe { gos_rt_result_new(1, err as i64) }
}

/// `bufio::read_to_string(path) -> Result<String, Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bufio_read_to_string(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let p = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        match crate::sched_global::run_blocking("bufio-read-string", move || {
            std::fs::read_to_string(p)
        }) {
            Ok(Ok(text)) => unsafe { gos_rt_result_new(0, alloc_cstring(text.as_bytes()) as i64) },
            Ok(Err(e)) => err_io(&e),
            Err(e) => fs_err(&e),
        }
    })
}

/// `bufio::read_lines_of(path) -> Result<[String], Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bufio_read_lines_of(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let p = if path.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        match crate::sched_global::run_blocking("bufio-read-lines", move || {
            std::fs::read_to_string(p)
        }) {
            Ok(Ok(text)) => {
                let lines: Vec<String> = text.lines().map(str::to_string).collect();
                ok_str_vec(&lines)
            }
            Ok(Err(e)) => err_io(&e),
            Err(e) => fs_err(&e),
        }
    })
}

/// `net::resolve(host) / net::lookup(host) -> Result<[String], Error>`
/// - resolves a host (optionally `host:port`) to IP address strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_net_resolve(host: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        use std::net::ToSocketAddrs;
        let h = if host.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(host).to_str().unwrap_or("") }.to_string()
        };
        let needle = if h.contains(':') { h } else { format!("{h}:0") };
        match crate::sched_global::run_blocking("net-resolve", move || {
            needle
                .to_socket_addrs()
                .map(|addrs| addrs.map(|a| a.ip().to_string()).collect::<Vec<_>>())
        }) {
            Ok(Ok(ips)) => ok_str_vec(&ips),
            Ok(Err(e)) => err_io(&e),
            Err(e) => fs_err(&e),
        }
    })
}

/// Posix path join mirroring `gossamer_std::path::join`: collapses a
/// trailing separator on `base` and a leading separator on `segment`
/// to a single `/`, absorbs an absolute `segment`, and is independent
/// of the host separator so the compiled tier matches the VM on every
/// platform.
fn path_join(base: &str, segment: &str) -> String {
    if segment.starts_with('/') {
        return segment.to_string();
    }
    if base.is_empty() {
        return segment.to_string();
    }
    let mut out = base.trim_end_matches('/').to_string();
    out.push('/');
    out.push_str(segment.trim_start_matches('/'));
    out
}

/// Lexical path normalization shared by `gos_rt_path_clean` /
/// `gos_rt_path_has_prefix`. Mirrors `gossamer_std::path::normalize`.
fn path_clean(path: &str) -> String {
    if path.is_empty() {
        return ".".to_string();
    }
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for segment in path.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|s: &&str| *s != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let mut out = String::new();
    if absolute {
        out.push('/');
    }
    out.push_str(&parts.join("/"));
    if out.is_empty() { ".".to_string() } else { out }
}

// --- 0.7.0 scalar cmp helpers ---

/// Scalar `min(a, b)` for i64. Pair-shape used by the prelude
/// `min(a, b)` dispatch in compiled mode.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_min_i64(a: i64, b: i64) -> i64 {
    a.min(b)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_max_i64(a: i64, b: i64) -> i64 {
    a.max(b)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_clamp_i64(x: i64, lo: i64, hi: i64) -> i64 {
    x.clamp(lo, hi)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_min_f64(a: f64, b: f64) -> f64 {
    a.min(b)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_max_f64(a: f64, b: f64) -> f64 {
    a.max(b)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_clamp_f64(x: f64, lo: f64, hi: f64) -> f64 {
    x.clamp(lo, hi)
}
