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

use std::ffi::{CStr, CString};
use std::io::{BufRead, Read};
use std::os::raw::c_char;

use super::*;
use crate::c_abi::errors::gos_rt_error_new;
use crate::c_abi::vec::gos_rt_result_new;

// ---------------------------------------------------------------
// Streams - io::stdout / io::stderr / io::stdin
// ---------------------------------------------------------------
//
// Each stream is an opaque handle returned by the corresponding
// constructor. Internally it's a `*GosStream` whose `fd` field
// is 0 (stdin), 1 (stdout), or 2 (stderr). The same three
// pointers are returned on every call - they live in static
// rodata, so `io::stdout()` is effectively a no-op that returns
// an already-interned handle.
//
// Write methods (`write_byte`, `write`, `write_str`, `flush`)
// route every stdout-fd call through the thread-local 64 KiB
// line-buffer; stderr writes go direct-to-syscall (it's error
// output, we want it unbuffered). Read methods read from stdin:
// `read_line(&mut String)` appends into the caller's buffer slot
// and returns `Result<i64, errors::Error>`; `read_to_string`
// allocates a fresh String through the GC arena and returns it.

#[repr(C)]
pub struct GosStream {
    pub fd: i32,
}

static STREAM_STDIN: GosStream = GosStream { fd: 0 };
static STREAM_STDOUT: GosStream = GosStream { fd: 1 };
static STREAM_STDERR: GosStream = GosStream { fd: 2 };

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stdin() -> *const GosStream {
    ffi_entry!(std::ptr::null(), { std::ptr::addr_of!(STREAM_STDIN) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stdout() -> *const GosStream {
    ffi_entry!(std::ptr::null(), { std::ptr::addr_of!(STREAM_STDOUT) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stderr() -> *const GosStream {
    ffi_entry!(std::ptr::null(), { std::ptr::addr_of!(STREAM_STDERR) })
}

/// `io::Copy(dst, src)` - drains `src` (the stdin stream) to EOF,
/// writing every byte to `dst`, and returns the byte count. Mirrors
/// Go's `io.Copy`. Only stdin -> stdout/stderr is wired today; any
/// other source fd is a no-op returning 0, matching the interpreter
/// builtin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_copy(dst: *const GosStream, src: *const GosStream) -> i64 {
    ffi_entry!(0, {
        let dst_fd = unsafe { stream_fd(dst) };
        let src_fd = unsafe { stream_fd(src) };
        if src_fd != 0 {
            return 0;
        }
        unsafe { gos_rt_flush_stdout() };
        let stdin = std::io::stdin();
        let mut buf = String::new();
        let n = match stdin.lock().read_to_string(&mut buf) {
            Ok(n) => n as i64,
            Err(_) => return 0,
        };
        unsafe { write_fd(dst_fd, buf.as_bytes()) };
        n
    })
}

/// `io::ReadAll(reader)` - drains `reader` (the stdin stream) to EOF
/// and returns the accumulated bytes as a freshly-allocated
/// GC-arena string. Mirrors Go's `io.ReadAll`. Non-stdin readers
/// return an empty string, matching the interpreter builtin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_read_all(reader: *const GosStream) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let fd = unsafe { stream_fd(reader) };
        if fd != 0 {
            return alloc_cstring(b"");
        }
        unsafe { gos_rt_flush_stdout() };
        let stdin = std::io::stdin();
        let mut buf = String::new();
        match stdin.lock().read_to_string(&mut buf) {
            Ok(_) => alloc_cstring(buf.as_bytes()),
            Err(_) => alloc_cstring(b""),
        }
    })
}

unsafe fn stream_fd(s: *const GosStream) -> i32 {
    if s.is_null() {
        return 1;
    }
    unsafe { (*s).fd }
}

unsafe fn write_fd(fd: i32, bytes: &[u8]) {
    if fd == 1 {
        unsafe { write_stdout(bytes) };
    } else {
        // Unbuffered direct write - fine for stderr and for any
        // user-opened fd once we add `open`. stdout is the only
        // buffered sink today.
        raw_write_fd(fd, bytes);
    }
}

fn raw_write_fd(fd: i32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    use std::io::Write;
    // Today the runtime only routes fds 1 and 2; fd 0 is read-only.
    // Other fds will land here once `open()` is wired - at that
    // point this dispatch grows. Going through `std::io` keeps the
    // call cross-platform (no `extern "C" fn write` symbol on
    // Windows MSVC).
    match fd {
        1 => {
            let stdout = std::io::stdout();
            let _ = stdout.lock().write_all(bytes);
        }
        2 => {
            let stderr = std::io::stderr();
            let _ = stderr.lock().write_all(bytes);
        }
        _ => {}
    }
}

/// Writes a single raw byte to `stream`. `b` is truncated to
/// its low 8 bits.
///
/// Hot path for fasta-style character-at-a-time output. The
/// stdout fast path inlines the buffer-append operation: load
/// `len`, check capacity, store byte at `bytes[len]`, bump
/// `len`. Only when the buffer is full do we drop into the
/// (large) flush helper. Stderr and other fds go straight to
/// `write(2)` since they're rare.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_write_byte(stream: *const GosStream, b: i64) {
    ffi_entry!((), {
        let fd = unsafe { stream_fd(stream) };
        if fd == 1 {
            let _guard = StdoutGuard::acquire();
            let bytes_ptr = GOS_RT_STDOUT_BYTES.0.get();
            let len_ptr = GOS_RT_STDOUT_LEN.0.get();
            let len = unsafe { *len_ptr };
            if len < STDOUT_BUF_SIZE {
                unsafe {
                    *(*bytes_ptr).as_mut_ptr().add(len) = b as u8;
                    *len_ptr = len + 1;
                }
                return;
            }
            // Buffer full - flush and stash the new byte.
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
                *(*bytes_ptr).as_mut_ptr() = b as u8;
                *len_ptr = 1;
            }
            return;
        }
        let byte = [(b & 0xff) as u8];
        raw_write_fd(fd, &byte);
    });
}

/// Writes every byte of the passed C-string through `stream`.
/// `stream.write(s)` and `stream.write_str(s)` both land here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_write_str(stream: *const GosStream, s: *const c_char) {
    ffi_entry!((), {
        let fd = unsafe { stream_fd(stream) };
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        unsafe { write_fd(fd, bytes) };
    });
}

/// Writes the low byte of each `i64` slot in `arr[..len]` to
/// `stream`. Used by user code to build a small line buffer
/// (e.g. fasta's 60-char line) as `[i64; N]` and emit it in
/// one bulk call instead of paying per-byte FFI overhead.
///
/// The flat-slot array layout means a Gossamer `[u8; 60]` /
/// `[i64; 60]` is stored as `[60 x i64]`; this routine reads
/// each i64 and writes its low 8 bits. Batches the whole
/// block into a single `write_stdout` (or syscall) call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_write_byte_array(
    stream: *const GosStream,
    arr: *const i64,
    len: i64,
) {
    ffi_entry!((), {
        if arr.is_null() || len <= 0 {
            return;
        }
        let len = len as usize;
        let fd = unsafe { stream_fd(stream) };
        if fd == 1 {
            // Stdout fast path. We always check capacity ONCE
            // up front and (if it fits) do a tight pack that the
            // optimiser is happy to vectorise - no per-iteration
            // bounds branch. The slow path (block doesn't fit
            // remaining capacity) flushes and retries; for the
            // small-block case (fasta's 61-byte lines) the buffer
            // is rarely full, so the fast path runs every line.
            let guard = StdoutGuard::acquire();
            let bytes_ptr = GOS_RT_STDOUT_BYTES.0.get();
            let len_ptr = GOS_RT_STDOUT_LEN.0.get();
            let cur = unsafe { *len_ptr };
            if cur + len <= STDOUT_BUF_SIZE {
                unsafe {
                    let dst = (*bytes_ptr).as_mut_ptr().add(cur);
                    for i in 0..len {
                        *dst.add(i) = (*arr.add(i)) as u8;
                    }
                    *len_ptr = cur + len;
                }
                return;
            }
            // Slow path: block doesn't fit. Flush and either pack
            // an oversized payload directly, or recurse so the
            // first arm fires with an empty buffer. The recursion
            // case has to drop the guard first - `STDOUT_LOCK` is
            // a non-recursive `RawMutex`, so re-entering on the
            // same OS thread would deadlock.
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                *len_ptr = 0;
                if len > STDOUT_BUF_SIZE {
                    let mut tmp = Vec::<u8>::with_capacity(len);
                    for i in 0..len {
                        tmp.push((*arr.add(i)) as u8);
                    }
                    raw_write_stdout(&tmp);
                } else {
                    drop(guard);
                    gos_rt_stream_write_byte_array(stream, arr, len as i64);
                    return;
                }
            }
            return;
        }
        // Other fds: pack into a stack buffer and issue one syscall.
        let mut buf = [0u8; 4096];
        let mut cur = 0usize;
        for i in 0..len {
            if cur >= buf.len() {
                raw_write_fd(fd, &buf[..cur]);
                cur = 0;
            }
            buf[cur] = unsafe { (*arr.add(i)) as u8 };
            cur += 1;
        }
        if cur > 0 {
            raw_write_fd(fd, &buf[..cur]);
        }
    });
}

/// Flushes the buffered writer (only matters for the stdout
/// stream today).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_flush(stream: *const GosStream) {
    ffi_entry!((), {
        let fd = unsafe { stream_fd(stream) };
        if fd == 1 {
            unsafe { gos_rt_flush_stdout() };
        }
    });
}

fn stream_read_line_err(message: &str) -> i128 {
    let msg = CString::new(message).unwrap_or_else(|_| c"read_line failed".to_owned());
    let err = unsafe { gos_rt_error_new(msg.as_ptr()) };
    gos_rt_result_new(1, err as i64)
}

/// Reads one line from `stream` (expected to be stdin), appends the raw line
/// to the caller's `String` slot, and returns `Ok(bytes_read)`. The buffer
/// keeps the newline; callers can use `trim()` when they want prompt input.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_read_line(
    stream: *const GosStream,
    buf_slot: *mut *mut c_char,
) -> i128 {
    ffi_entry!(stream_read_line_err("read_line: runtime panic"), {
        if buf_slot.is_null() {
            return stream_read_line_err("read_line: expected &mut String buffer");
        }
        let fd = unsafe { stream_fd(stream) };
        if fd != 0 {
            return gos_rt_result_new(0, 0);
        }
        unsafe { gos_rt_flush_stdout() };
        let stdin = std::io::stdin();
        let mut line = String::new();
        match stdin.lock().read_line(&mut line) {
            Ok(n) => {
                let current = unsafe { *buf_slot };
                let mut out = if current.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(current) }
                        .to_string_lossy()
                        .into_owned()
                };
                out.push_str(&line);
                let updated = alloc_cstring(out.as_bytes());
                unsafe {
                    *buf_slot = updated;
                }
                gos_rt_result_new(0, n as i64)
            }
            Err(e) => stream_read_line_err(&format!("read_line: {e}")),
        }
    })
}

/// Reads every remaining byte from `stream` (expected to be
/// stdin) into a freshly-allocated GC-arena string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_read_to_string(stream: *const GosStream) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let fd = unsafe { stream_fd(stream) };
        if fd != 0 {
            return alloc_cstring(b"");
        }
        unsafe { gos_rt_flush_stdout() };
        let stdin = std::io::stdin();
        let mut buf = String::new();
        match stdin.lock().read_to_string(&mut buf) {
            Ok(_) => alloc_cstring(buf.as_bytes()),
            Err(_) => alloc_cstring(b""),
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_println() {
    ffi_entry!((), {
        unsafe { write_stdout(b"\n") };
        // Flush on every newline - matches Rust's LineWriter<StdoutRaw> contract
        // so that `println!` output appears immediately, as it does in Go and
        // on the JVM. Programs that need high-throughput output should use
        // stream write methods directly rather than `println!`.
        unsafe { gos_rt_flush_stdout() };
    });
}
