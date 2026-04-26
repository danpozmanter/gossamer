//! C-ABI runtime surface linked into every native Gossamer program.
//! Every symbol in this module is exported under the `gos_rt_*`
//! prefix so the Cranelift codegen can call them by name. All
//! `extern "C"` functions run in unsafe context — the compiler emits
//! raw pointers and trusts the contract described next to each
//! symbol. Failure modes are documented per symbol; they never
//! panic across the FFI boundary.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::missing_panics_doc)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::cast_possible_truncation)]
#![allow(clippy::cast_possible_wrap)]
#![allow(clippy::cast_sign_loss)]
#![allow(clippy::cast_precision_loss)]
#![allow(clippy::must_use_candidate)]
// FFI signatures must match the Cranelift / LLVM call sites
// exactly. The remaining pedantic lints either flag patterns we
// deliberately keep (similar `argc`/`argv` parameter names match
// the Unix convention; `cast_lossless` would make the
// hot-path runtime arithmetic harder to read; nested `unsafe
// extern` blocks are a localisation choice for fns we only
// reference once) or are already worked around in the source
// (`Vec::from_raw_parts(p, n, n)` reconstructs an exact
// allocation we hand-built). Allow them at file scope rather
// than dotting per-call-site annotations across 2k lines.
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::match_same_arms)]

use std::ffi::CStr;
use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

// ---------------------------------------------------------------
// Process-wide argv view
// ---------------------------------------------------------------
//
// `os::args()` is supposed to behave like a `Vec<String>`:
// `.len()` is the user-arg count and `args[i]` is the i-th user
// arg as a String. We map that to the flat codegen's stride-8
// indexing by returning `argv + 1` (the pointer just past
// `argv[0]`), so a Place projection with stride 8 reads the
// successive `char*` entries directly. `gos_rt_arr_len` detects
// this exact pointer and returns `argc - 1` rather than reading
// garbage through it.

static ARGS_PTR: AtomicUsize = AtomicUsize::new(0);
static ARGS_LEN: AtomicI64 = AtomicI64::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_args(argc: c_int, argv: *const *const c_char) {
    if argc > 1 && !argv.is_null() {
        // SAFETY: libc guarantees argv[0..argc] is valid when
        // argc > 0. `argv + 1` therefore addresses `argc - 1`
        // strings.
        let user_argv = unsafe { argv.add(1) };
        ARGS_PTR.store(user_argv as usize, Ordering::SeqCst);
        ARGS_LEN.store(i64::from(argc - 1), Ordering::SeqCst);
    } else {
        ARGS_PTR.store(0, Ordering::SeqCst);
        ARGS_LEN.store(0, Ordering::SeqCst);
    }
}

/// Returns the pointer to the first user-passed argument. A
/// Place projection with stride 8 reads successive strings
/// through it; `.len()` routes to `gos_rt_arr_len` which
/// short-circuits on this exact pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_args() -> *const c_char {
    ARGS_PTR.load(Ordering::SeqCst) as *const c_char
}

// ---------------------------------------------------------------
// Array/Vec/Generic len — first i64 of the passed buffer is len
// ---------------------------------------------------------------

/// Reads the leading i64 of a len-prefixed pointer.
///
/// Special cases:
/// - NULL returns 0.
/// - The exact pointer returned by `gos_rt_os_args` returns
///   `argc - 1` (the args-list length) instead of whatever the
///   first argv entry happens to look like when dereferenced.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_arr_len(p: *const i64) -> i64 {
    if p.is_null() {
        return 0;
    }
    if (p as usize) == ARGS_PTR.load(Ordering::SeqCst) && p as usize != 0 {
        return ARGS_LEN.load(Ordering::SeqCst);
    }
    // SAFETY: callers guarantee the pointer is a len-prefixed
    // buffer, the args sentinel, or NULL.
    unsafe { *p }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_len(p: *const i64) -> i64 {
    unsafe { gos_rt_arr_len(p) }
}

// ---------------------------------------------------------------
// String runtime
// ---------------------------------------------------------------
// Strings are represented as owning `CString`-shaped pointers
// allocated by Rust's `String::into_boxed_str`/`into_raw`. The
// pointer passed across the FFI is the first byte of the UTF-8
// payload; it is nul-terminated so C code can `%s`-print it. We
// track length separately by scanning for the nul byte in the C
// ABI; users that want O(1) length should use the GosStr header
// helpers (future). For L2 the single-owner story is enough.

unsafe fn c_str_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    unsafe { CStr::from_ptr(s).to_bytes().len() }
}

fn alloc_cstring(s: &[u8]) -> *mut c_char {
    // Pick the first NUL (if any) so we never copy past it.
    let nul = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    let len = nul;
    // SAFETY: `gos_rt_gc_alloc` returns a pointer into a
    // thread-local arena sized for `len + 1` bytes; we write the
    // payload + trailing NUL and return the arena pointer. Freed
    // en bloc by `gos_rt_gc_reset`.
    unsafe {
        let raw = gos_rt_gc_alloc((len + 1) as u64);
        if raw.is_null() {
            // Arena exhausted (shouldn't happen under the current
            // bump allocator). Fall back to a leaky Box.
            let mut v = s[..len].to_vec();
            v.push(0);
            return Box::into_raw(v.into_boxed_slice()).cast::<c_char>();
        }
        std::ptr::copy_nonoverlapping(s.as_ptr(), raw, len);
        *raw.add(len) = 0;
        raw.cast::<c_char>()
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_len(s: *const c_char) -> i64 {
    unsafe { c_str_len(s) as i64 }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_byte_at(s: *const c_char, i: i64) -> i64 {
    if s.is_null() || i < 0 {
        return 0;
    }
    // Strings are null-terminated and treated as immutable
    // bytes. The previous implementation called
    // `CStr::from_ptr(s).to_bytes()` which walks the string with
    // `strlen` on every access — fasta-style hot loops doing
    // `s[idx % len]` paid O(strlen) per byte. The user's loop is
    // expected to keep `idx` in range (e.g. `% alu_len` against
    // a precomputed `alu_len = alu.len()`); reading past the
    // null terminator returns zero, which is what callers expect
    // anyway.
    let byte = unsafe { *s.cast::<u8>().add(i as usize) };
    i64::from(byte)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_concat(a: *const c_char, b: *const c_char) -> *mut c_char {
    let a_bytes: &[u8] = if a.is_null() {
        &[]
    } else {
        unsafe { CStr::from_ptr(a).to_bytes() }
    };
    let b_bytes: &[u8] = if b.is_null() {
        &[]
    } else {
        unsafe { CStr::from_ptr(b).to_bytes() }
    };
    let mut out = Vec::with_capacity(a_bytes.len() + b_bytes.len());
    out.extend_from_slice(a_bytes);
    out.extend_from_slice(b_bytes);
    alloc_cstring(&out)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_trim(s: *const c_char) -> *mut c_char {
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    let st = std::str::from_utf8(bytes).unwrap_or("");
    alloc_cstring(st.trim().as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_upper(s: *const c_char) -> *mut c_char {
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    let st = std::str::from_utf8(bytes).unwrap_or("");
    alloc_cstring(st.to_uppercase().as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_lower(s: *const c_char) -> *mut c_char {
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    let st = std::str::from_utf8(bytes).unwrap_or("");
    alloc_cstring(st.to_lowercase().as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_contains(s: *const c_char, needle: *const c_char) -> i32 {
    if s.is_null() || needle.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(s).to_bytes() };
    let n = unsafe { CStr::from_ptr(needle).to_bytes() };
    if n.is_empty() {
        return 1;
    }
    if s.len() < n.len() {
        return 0;
    }
    for i in 0..=(s.len() - n.len()) {
        if &s[i..i + n.len()] == n {
            return 1;
        }
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_starts_with(s: *const c_char, prefix: *const c_char) -> i32 {
    if s.is_null() || prefix.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(s).to_bytes() };
    let p = unsafe { CStr::from_ptr(prefix).to_bytes() };
    i32::from(s.starts_with(p))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_ends_with(s: *const c_char, suffix: *const c_char) -> i32 {
    if s.is_null() || suffix.is_null() {
        return 0;
    }
    let s = unsafe { CStr::from_ptr(s).to_bytes() };
    let suf = unsafe { CStr::from_ptr(suffix).to_bytes() };
    i32::from(s.ends_with(suf))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_find(s: *const c_char, needle: *const c_char) -> i64 {
    if s.is_null() || needle.is_null() {
        return -1;
    }
    let s = unsafe { CStr::from_ptr(s).to_bytes() };
    let n = unsafe { CStr::from_ptr(needle).to_bytes() };
    if n.is_empty() {
        return 0;
    }
    if s.len() < n.len() {
        return -1;
    }
    for i in 0..=(s.len() - n.len()) {
        if &s[i..i + n.len()] == n {
            return i as i64;
        }
    }
    -1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_replace(
    s: *const c_char,
    from: *const c_char,
    to: *const c_char,
) -> *mut c_char {
    let s = if s.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
    };
    let f = if from.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(from).to_str().unwrap_or("") }
    };
    let t = if to.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(to).to_str().unwrap_or("") }
    };
    alloc_cstring(s.replace(f, t).as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_i64(s: *const c_char, ok_out: *mut i32) -> i64 {
    if s.is_null() {
        if !ok_out.is_null() {
            unsafe { *ok_out = 0 };
        }
        return 0;
    }
    let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
    if let Ok(n) = text.parse::<i64>() {
        if !ok_out.is_null() {
            unsafe { *ok_out = 1 };
        }
        n
    } else {
        if !ok_out.is_null() {
            unsafe { *ok_out = 0 };
        }
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_f64(s: *const c_char, ok_out: *mut i32) -> f64 {
    if s.is_null() {
        if !ok_out.is_null() {
            unsafe { *ok_out = 0 };
        }
        return 0.0;
    }
    let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
    if let Ok(x) = text.parse::<f64>() {
        if !ok_out.is_null() {
            unsafe { *ok_out = 1 };
        }
        x
    } else {
        if !ok_out.is_null() {
            unsafe { *ok_out = 0 };
        }
        0.0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_i64_to_str(n: i64) -> *mut c_char {
    alloc_cstring(n.to_string().as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_to_str(x: f64) -> *mut c_char {
    alloc_cstring(format!("{x}").as_bytes())
}

/// Stringifies a bool (passed as i32: nonzero = true). Used by
/// codegen to assemble multi-arg panic / format-style messages.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bool_to_str(b: i32) -> *mut c_char {
    alloc_cstring(if b == 0 { b"false" } else { b"true" })
}

/// Stringifies a char (passed as i32 Unicode scalar) into a freshly
/// heap-allocated UTF-8 c-string. Invalid scalars (surrogates,
/// > U+10FFFF) render as `\u{FFFD}` (REPLACEMENT CHARACTER).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_char_to_str(c: i32) -> *mut c_char {
    let scalar = u32::try_from(c)
        .ok()
        .and_then(char::from_u32)
        .unwrap_or('\u{FFFD}');
    let mut buf = [0u8; 4];
    let s = scalar.encode_utf8(&mut buf);
    alloc_cstring(s.as_bytes())
}

// ---------------------------------------------------------------
// Print helpers (variadic-printf workaround — Cranelift 0.123
// has no variadic-call ABI support, so every formatted print
// routes through a fixed-signature wrapper.)
// ---------------------------------------------------------------

// Process-global 64 KiB stdout buffer. The previous design
// used a thread-local `RefCell<Vec<u8>>`, but every byte write
// then paid for: (a) the TLS slot lookup, (b) `RefCell`
// borrow-tracking, and (c) `Vec` capacity-check + push. For
// character-at-a-time output (fasta's hot loop emits ~50M
// bytes one at a time), that overhead dominated the wall
// clock. The reference Rust / Go programs reach byte-write
// speed by inlining `BufWriter`/`bufio.Writer`, which we can't
// do across an FFI boundary; the next best thing is a
// monomorphic process-global buffer and unsynchronised
// pointer arithmetic in the byte-write fast path. Multi-
// threaded programs are still safe because the bytecode VM /
// scheduler do not concurrently call into this buffer (each
// goroutine's writes are serialised through a single thread
// at the boundary today; full TLS-per-thread remains a
// follow-up if anyone hits contention).
/// Hot-path stdout buffer capacity. Codegen inlines a buffer
/// length check against this value, so it must stay in sync
/// with `GOS_RT_STDOUT_BYTES`'s length below.
pub const STDOUT_BUF_SIZE: usize = 64 * 1024;

/// Process-global stdout buffer storage. The LLVM backend
/// emits inline fast-path code that loads
/// `GOS_RT_STDOUT_LEN`, stores the new byte at offset
/// `bytes[len]`, and bumps the length — bypassing the FFI
/// call and saving the per-call overhead that dominates
/// character-at-a-time output (fasta hot loop).
#[unsafe(no_mangle)]
pub static mut GOS_RT_STDOUT_BYTES: [u8; STDOUT_BUF_SIZE] = [0; STDOUT_BUF_SIZE];

/// Current write offset in `GOS_RT_STDOUT_BYTES`. The inline
/// fast path reads this, stores the byte, and writes it back.
#[unsafe(no_mangle)]
pub static mut GOS_RT_STDOUT_LEN: usize = 0;

unsafe fn raw_write_stdout(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    unsafe extern "C" {
        fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    }
    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe { write(1, bytes.as_ptr().add(off), bytes.len() - off) };
        if n <= 0 {
            return;
        }
        off += n as usize;
    }
}

#[allow(static_mut_refs)]
unsafe fn write_stdout(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let len = unsafe { *len_ptr };
    // Flush and bypass the buffer entirely for chunks that
    // don't fit — a single large chunk costs one syscall
    // either way.
    if bytes.len() >= STDOUT_BUF_SIZE {
        if len > 0 {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
                *len_ptr = 0;
            }
        }
        unsafe { raw_write_stdout(bytes) };
        return;
    }
    if len + bytes.len() > STDOUT_BUF_SIZE {
        unsafe {
            raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*bytes_ptr).as_mut_ptr(), bytes.len());
            *len_ptr = bytes.len();
        }
    } else {
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                (*bytes_ptr).as_mut_ptr().add(len),
                bytes.len(),
            );
            *len_ptr = len + bytes.len();
        }
    }
}

/// Flushes the process-global stdout buffer. Called on every
/// `println`-family intrinsic and on process exit via
/// `gos_rt_flush_stdout`.
#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_flush_stdout() {
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let len = unsafe { *len_ptr };
    if len > 0 {
        unsafe {
            raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
            *len_ptr = 0;
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_str(s: *const c_char) {
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    unsafe { write_stdout(bytes) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_i64(n: i64) {
    // Format on the stack — avoid the per-call heap allocation
    // that `n.to_string()` would incur.
    let mut buf = itoa::Buffer::new();
    let text = buf.format(n);
    unsafe { write_stdout(text.as_bytes()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_f64(x: f64) {
    // Match the interpreter's `{}` Display output.
    let text = format!("{x}");
    unsafe { write_stdout(text.as_bytes()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_bool(b: i32) {
    unsafe { write_stdout(if b != 0 { b"true" } else { b"false" }) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_char(c: i32) {
    if let Some(ch) = char::from_u32(c as u32) {
        let mut buf = [0u8; 4];
        let s = ch.encode_utf8(&mut buf);
        unsafe { write_stdout(s.as_bytes()) };
    }
}

// ---------------------------------------------------------------
// Streams — io::stdout / io::stderr / io::stdin
// ---------------------------------------------------------------
//
// Each stream is an opaque handle returned by the corresponding
// constructor. Internally it's a `*GosStream` whose `fd` field
// is 0 (stdin), 1 (stdout), or 2 (stderr). The same three
// pointers are returned on every call — they live in static
// rodata, so `io::stdout()` is effectively a no-op that returns
// an already-interned handle.
//
// Write methods (`write_byte`, `write`, `write_str`, `flush`)
// route every stdout-fd call through the thread-local 64 KiB
// line-buffer; stderr writes go direct-to-syscall (it's error
// output, we want it unbuffered). Read methods (`read_line`,
// `read_to_string`) read from libc `fgets` / stdin; they
// allocate a fresh String through the GC arena and return it.

#[repr(C)]
pub struct GosStream {
    pub fd: i32,
}

unsafe impl Send for GosStream {}
unsafe impl Sync for GosStream {}

static STREAM_STDIN: GosStream = GosStream { fd: 0 };
static STREAM_STDOUT: GosStream = GosStream { fd: 1 };
static STREAM_STDERR: GosStream = GosStream { fd: 2 };

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stdin() -> *const GosStream {
    std::ptr::addr_of!(STREAM_STDIN)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stdout() -> *const GosStream {
    std::ptr::addr_of!(STREAM_STDOUT)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_io_stderr() -> *const GosStream {
    std::ptr::addr_of!(STREAM_STDERR)
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
        // Unbuffered direct write — fine for stderr and for any
        // user-opened fd once we add `open`. stdout is the only
        // buffered sink today.
        unsafe { raw_write_fd(fd, bytes) };
    }
}

unsafe fn raw_write_fd(fd: i32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    unsafe extern "C" {
        fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    }
    let mut off = 0usize;
    while off < bytes.len() {
        let n = unsafe { write(fd, bytes.as_ptr().add(off), bytes.len() - off) };
        if n <= 0 {
            return;
        }
        off += n as usize;
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
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_stream_write_byte(stream: *const GosStream, b: i64) {
    let fd = unsafe { stream_fd(stream) };
    if fd == 1 {
        let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
        let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
        let len = unsafe { *len_ptr };
        if len < STDOUT_BUF_SIZE {
            unsafe {
                *(*bytes_ptr).as_mut_ptr().add(len) = b as u8;
                *len_ptr = len + 1;
            }
            return;
        }
        // Buffer full — flush and stash the new byte.
        unsafe {
            raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
            *(*bytes_ptr).as_mut_ptr() = b as u8;
            *len_ptr = 1;
        }
        return;
    }
    let byte = [(b & 0xff) as u8];
    unsafe { raw_write_fd(fd, &byte) };
}

/// Writes every byte of the passed C-string through `stream`.
/// `stream.write(s)` and `stream.write_str(s)` both land here.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_write_str(stream: *const GosStream, s: *const c_char) {
    let fd = unsafe { stream_fd(stream) };
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    unsafe { write_fd(fd, bytes) };
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
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_stream_write_byte_array(
    stream: *const GosStream,
    arr: *const i64,
    len: i64,
) {
    if arr.is_null() || len <= 0 {
        return;
    }
    let len = len as usize;
    let fd = unsafe { stream_fd(stream) };
    if fd == 1 {
        // Stdout fast path. We always check capacity ONCE
        // up front and (if it fits) do a tight pack that the
        // optimiser is happy to vectorise — no per-iteration
        // bounds branch. The slow path (block doesn't fit
        // remaining capacity) flushes and retries; for the
        // small-block case (fasta's 61-byte lines) the buffer
        // is rarely full, so the fast path runs every line.
        let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
        let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
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
        // Slow path: block doesn't fit. Flush and recurse with
        // a now-empty buffer.
        unsafe {
            raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            *len_ptr = 0;
            // Block bigger than the buffer? Pack into a heap
            // vec and write it directly.
            if len > STDOUT_BUF_SIZE {
                let mut tmp = Vec::<u8>::with_capacity(len);
                for i in 0..len {
                    tmp.push((*arr.add(i)) as u8);
                }
                raw_write_stdout(&tmp);
            } else {
                // Recurse; now the buffer is empty so the
                // first arm fires.
                gos_rt_stream_write_byte_array(stream, arr, len as i64);
            }
        }
        return;
    }
    // Other fds: pack into a stack buffer and issue one syscall.
    let mut buf = [0u8; 4096];
    let mut cur = 0usize;
    for i in 0..len {
        if cur >= buf.len() {
            unsafe { raw_write_fd(fd, &buf[..cur]) };
            cur = 0;
        }
        buf[cur] = unsafe { (*arr.add(i)) as u8 };
        cur += 1;
    }
    if cur > 0 {
        unsafe { raw_write_fd(fd, &buf[..cur]) };
    }
}

/// Flushes the buffered writer (only matters for the stdout
/// stream today).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_flush(stream: *const GosStream) {
    let fd = unsafe { stream_fd(stream) };
    if fd == 1 {
        unsafe { gos_rt_flush_stdout() };
    }
}

/// Reads one line from `stream` (expected to be stdin). Strips
/// the trailing `\n` if present. Returns the GC-arena-owned
/// C-string pointer; an empty string on EOF or any read error.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_read_line(stream: *const GosStream) -> *mut c_char {
    let fd = unsafe { stream_fd(stream) };
    if fd != 0 {
        return alloc_cstring(b"");
    }
    unsafe { gos_rt_flush_stdout() };
    let stdin = std::io::stdin();
    let mut line = String::new();
    match stdin.lock().read_line(&mut line) {
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            alloc_cstring(line.as_bytes())
        }
        Err(_) => alloc_cstring(b""),
    }
}

/// Reads every remaining byte from `stream` (expected to be
/// stdin) into a freshly-allocated GC-arena string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stream_read_to_string(stream: *const GosStream) -> *mut c_char {
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
}

#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_println() {
    unsafe { write_stdout(b"\n") };
    // Line-flush so interactive output appears promptly.
    // Batched programs (fasta et al.) fill the buffer and flush
    // in 64 KiB chunks, which is dramatically cheaper than per-
    // write syscalls.
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let len = unsafe { *len_ptr };
    if len >= STDOUT_BUF_SIZE / 2 {
        unsafe {
            raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), len));
            *len_ptr = 0;
        }
    }
}

// ---------------------------------------------------------------
// Vec runtime — a `{ elem_bytes, len, cap, ptr }` struct
// ---------------------------------------------------------------

#[repr(C)]
pub struct GosVec {
    pub len: i64,
    pub cap: i64,
    pub elem_bytes: u32,
    pub ptr: *mut u8,
}

unsafe impl Send for GosVec {}
unsafe impl Sync for GosVec {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_new(elem_bytes: u32) -> *mut GosVec {
    Box::into_raw(Box::new(GosVec {
        len: 0,
        cap: 0,
        elem_bytes,
        ptr: std::ptr::null_mut(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity(elem_bytes: u32, cap: i64) -> *mut GosVec {
    if cap <= 0 {
        return unsafe { gos_rt_vec_new(elem_bytes) };
    }
    let bytes = (cap as usize) * (elem_bytes as usize);
    // Zero-initialised so the backing storage is always valid to
    // read (clippy::uninit_vec). The interpreter never observes a
    // slot before it's been explicitly written via push/insert,
    // but zeroing is cheap and removes the UB risk.
    let mut buf: Vec<u8> = vec![0u8; bytes];
    let ptr = buf.as_mut_ptr();
    std::mem::forget(buf);
    Box::into_raw(Box::new(GosVec {
        len: 0,
        cap,
        elem_bytes,
        ptr,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_len(v: *const GosVec) -> i64 {
    if v.is_null() {
        return 0;
    }
    unsafe { (*v).len }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push(v: *mut GosVec, elem: *const u8) {
    if v.is_null() || elem.is_null() {
        return;
    }
    let vec = unsafe { &mut *v };
    if vec.len == vec.cap {
        // Grow geometrically (cap -> max(4, cap*2)).
        let new_cap = if vec.cap == 0 { 4 } else { vec.cap * 2 };
        let old_bytes = (vec.cap as usize) * (vec.elem_bytes as usize);
        let new_bytes = (new_cap as usize) * (vec.elem_bytes as usize);
        // Zero-initialised — see `gos_rt_vec_with_capacity`.
        let mut buf: Vec<u8> = vec![0u8; new_bytes];
        if !vec.ptr.is_null() && old_bytes > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(vec.ptr, buf.as_mut_ptr(), old_bytes);
                // drop old allocation
                Vec::from_raw_parts(vec.ptr, old_bytes, old_bytes);
            }
        }
        vec.ptr = buf.as_mut_ptr();
        vec.cap = new_cap;
        std::mem::forget(buf);
    }
    let dst = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
    unsafe {
        std::ptr::copy_nonoverlapping(elem, dst, vec.elem_bytes as usize);
    }
    vec.len += 1;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_get_ptr(v: *const GosVec, idx: i64) -> *mut u8 {
    if v.is_null() {
        return std::ptr::null_mut();
    }
    let vec = unsafe { &*v };
    if idx < 0 || idx >= vec.len {
        return std::ptr::null_mut();
    }
    unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) }
}

/// Removes the last element of `v` and writes its bytes to
/// `out`. Returns 1 on success, 0 if the vec was empty. `out`
/// must be sized for `v.elem_bytes`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_pop(v: *mut GosVec, out: *mut u8) -> i32 {
    if v.is_null() || out.is_null() {
        return 0;
    }
    let vec = unsafe { &mut *v };
    if vec.len <= 0 {
        return 0;
    }
    vec.len -= 1;
    let src = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
    unsafe {
        std::ptr::copy_nonoverlapping(src, out, vec.elem_bytes as usize);
    }
    1
}

// ---------------------------------------------------------------
// HashMap runtime — byte-erased keys + values over std's HashMap
// ---------------------------------------------------------------

use std::collections::HashMap as StdHashMap;

/// Layout-sensitive: the first 8 bytes hold the current element
/// count so the generic `gos_rt_arr_len` returns the right value
/// without needing a HashMap-specific dispatch. Kept in sync with
/// `inner.len()` on every insert/remove.
#[repr(C)]
pub struct GosMap {
    len_cache: i64,
    key_bytes: u32,
    val_bytes: u32,
    inner: StdMutex<StdHashMap<Vec<u8>, Vec<u8>>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new(key_bytes: u32, val_bytes: u32) -> *mut GosMap {
    Box::into_raw(Box::new(GosMap {
        len_cache: 0,
        key_bytes,
        val_bytes,
        inner: StdMutex::new(StdHashMap::new()),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_len(m: *const GosMap) -> i64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).len_cache }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert(m: *mut GosMap, key: *const u8, val: *const u8) {
    if m.is_null() || key.is_null() || val.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let kb = map.key_bytes as usize;
    let vb = map.val_bytes as usize;
    let k = unsafe { std::slice::from_raw_parts(key, kb) }.to_vec();
    let v = unsafe { std::slice::from_raw_parts(val, vb) }.to_vec();
    let prior = map.inner.lock().unwrap().insert(k, v);
    if prior.is_none() {
        map.len_cache += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get(m: *const GosMap, key: *const u8, val_out: *mut u8) -> i32 {
    if m.is_null() || key.is_null() || val_out.is_null() {
        return 0;
    }
    let map = unsafe { &*m };
    let kb = map.key_bytes as usize;
    let vb = map.val_bytes as usize;
    let k = unsafe { std::slice::from_raw_parts(key, kb) };
    let inner = map.inner.lock().unwrap();
    if let Some(v) = inner.get(k) {
        unsafe {
            std::ptr::copy_nonoverlapping(v.as_ptr(), val_out, vb);
        }
        1
    } else {
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove(m: *mut GosMap, key: *const u8) -> i32 {
    if m.is_null() || key.is_null() {
        return 0;
    }
    let map = unsafe { &mut *m };
    let kb = map.key_bytes as usize;
    let k = unsafe { std::slice::from_raw_parts(key, kb) };
    if map.inner.lock().unwrap().remove(k).is_some() {
        map.len_cache -= 1;
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------
// Channel runtime — bounded MPMC via parking_lot Mutex<VecDeque>
// ---------------------------------------------------------------

use std::collections::VecDeque;
use std::sync::Condvar as StdCondvar;
use std::sync::Mutex as StdMutex;

pub struct GosChan {
    pub elem_bytes: u32,
    pub cap: i64, // 0 = unbounded
    pub closed: StdMutex<bool>,
    pub buf: StdMutex<VecDeque<Vec<u8>>>,
    pub not_empty: StdCondvar,
    pub not_full: StdCondvar,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_new(elem_bytes: u32, cap: i64) -> *mut GosChan {
    Box::into_raw(Box::new(GosChan {
        elem_bytes,
        cap,
        closed: StdMutex::new(false),
        buf: StdMutex::new(VecDeque::new()),
        not_empty: StdCondvar::new(),
        not_full: StdCondvar::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_send(c: *mut GosChan, val: *const u8) {
    if c.is_null() || val.is_null() {
        return;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    let mut data = vec![0u8; bytes_len];
    unsafe {
        std::ptr::copy_nonoverlapping(val, data.as_mut_ptr(), bytes_len);
    }
    let mut guard = chan.buf.lock().unwrap();
    while chan.cap > 0 && guard.len() as i64 >= chan.cap {
        guard = chan.not_full.wait(guard).unwrap();
    }
    guard.push_back(data);
    drop(guard);
    chan.not_empty.notify_one();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_send(c: *mut GosChan, val: *const u8) -> i32 {
    if c.is_null() || val.is_null() {
        return 0;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    let mut data = vec![0u8; bytes_len];
    unsafe {
        std::ptr::copy_nonoverlapping(val, data.as_mut_ptr(), bytes_len);
    }
    let mut guard = chan.buf.lock().unwrap();
    if chan.cap > 0 && guard.len() as i64 >= chan.cap {
        return 0;
    }
    guard.push_back(data);
    drop(guard);
    chan.not_empty.notify_one();
    1
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_recv(c: *mut GosChan, out: *mut u8) -> i32 {
    if c.is_null() || out.is_null() {
        return 0;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    let mut guard = chan.buf.lock().unwrap();
    loop {
        if let Some(item) = guard.pop_front() {
            unsafe {
                std::ptr::copy_nonoverlapping(item.as_ptr(), out, bytes_len);
            }
            drop(guard);
            chan.not_full.notify_one();
            return 1;
        }
        if *chan.closed.lock().unwrap() {
            return 0;
        }
        guard = chan.not_empty.wait(guard).unwrap();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_recv(c: *mut GosChan, out: *mut u8) -> i32 {
    if c.is_null() || out.is_null() {
        return 0;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    let mut guard = chan.buf.lock().unwrap();
    if let Some(item) = guard.pop_front() {
        unsafe {
            std::ptr::copy_nonoverlapping(item.as_ptr(), out, bytes_len);
        }
        drop(guard);
        chan.not_full.notify_one();
        return 1;
    }
    0
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_close(c: *mut GosChan) {
    if c.is_null() {
        return;
    }
    let chan = unsafe { &*c };
    *chan.closed.lock().unwrap() = true;
    chan.not_empty.notify_all();
    chan.not_full.notify_all();
}

// ---------------------------------------------------------------
// Scheduler — L2.5 stub: one OS thread per `go`
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn(
    func: Option<unsafe extern "C" fn(*mut u8)>,
    env: *mut u8,
) {
    let Some(f) = func else { return };
    // Box the raw env pointer as a usize so it crosses the thread
    // boundary as Send. Each thread handles its own lifecycle;
    // real scheduler work happens in L2.5 proper.
    let env_addr = env as usize;
    std::thread::spawn(move || {
        let env = env_addr as *mut u8;
        unsafe { f(env) };
    });
}

/// Spawns a new thread that calls a zero-argument function. Used
/// by `go task()` in the codegen when the call has no arguments.
/// The function pointer is stored as a usize to keep the runtime
/// helper signature stable across different target function
/// signatures.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_0(fn_addr: usize) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        // SAFETY: the caller promises `fn_addr` is the address of
        // an `extern "C" fn() -> i64` — the SysV-ABI convention
        // native codegen emits for every Gossamer function.
        type Fn0 = unsafe extern "C" fn() -> i64;
        let f: Fn0 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f() };
    });
}

/// Spawns a thread that calls a one-argument function with a
/// single i64 payload. All Gossamer scalar types fit in an i64
/// slot; floats are passed by bitcast in the caller.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_1(fn_addr: usize, arg0: i64) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn1 = unsafe extern "C" fn(i64) -> i64;
        let f: Fn1 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0) };
    });
}

/// Two-arg version. Enough for `go task(a, b)` where both args
/// fit in an i64 register.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_2(fn_addr: usize, arg0: i64, arg1: i64) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn2 = unsafe extern "C" fn(i64, i64) -> i64;
        let f: Fn2 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1) };
    });
}

/// Three-arg version. Required for fan-out patterns where a
/// worker takes a shared buffer pointer, an index / chunk
/// argument, and a `WaitGroup` to signal completion.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_3(fn_addr: usize, arg0: i64, arg1: i64, arg2: i64) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
        let f: Fn3 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2) };
    });
}

/// Four-arg version. Same intent as the 3-arg form;
/// covers the common fasta worker shape (buf, start, count,
/// wg).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_4(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
        let f: Fn4 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3) };
    });
}

/// Five-arg version. Used by fasta_mt's IUB worker
/// (buf, off, count, start_state, wg).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_5(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
    arg4: i64,
) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn5 = unsafe extern "C" fn(i64, i64, i64, i64, i64) -> i64;
        let f: Fn5 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4) };
    });
}

/// Six-arg version, headroom for future fan-out shapes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_6(
    fn_addr: usize,
    arg0: i64,
    arg1: i64,
    arg2: i64,
    arg3: i64,
    arg4: i64,
    arg5: i64,
) {
    if fn_addr == 0 {
        return;
    }
    std::thread::spawn(move || {
        type Fn6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64;
        let f: Fn6 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4, arg5) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_yield() {
    std::thread::yield_now();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sleep_ns(ns: i64) {
    if ns <= 0 {
        return;
    }
    std::thread::sleep(std::time::Duration::from_nanos(ns as u64));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_now_ns() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as i64)
}

// ---------------------------------------------------------------
// GC — bump allocator with safepoint reset
// ---------------------------------------------------------------
//
// Thread-local 4 MB arena. `gos_rt_gc_alloc(size)` bumps a pointer;
// when the arena fills, a new one is allocated and the old one
// leaks (bounded by the process). `gos_rt_gc_reset()` discards
// every arena on the current thread — call at well-defined
// safepoints (end of main, between benchmark iterations, etc.).
// A real tri-color GC replaces this without changing the ABI.

const ARENA_BYTES: usize = 4 * 1024 * 1024;

struct Arena {
    buf: Vec<u8>,
    used: usize,
}

thread_local! {
    static ARENAS: std::cell::RefCell<Vec<Arena>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gc_alloc(size: u64) -> *mut u8 {
    if size == 0 {
        return std::ptr::null_mut();
    }
    let size = size as usize;
    ARENAS.with(|cell| {
        let mut arenas = cell.borrow_mut();
        if arenas.last().is_none_or(|a| a.used + size > a.buf.len()) {
            let cap = size.max(ARENA_BYTES);
            // Zero-initialised arena. Allocations are bumped out
            // of `buf` and the caller writes before reading, but
            // zeroing avoids reading-before-write UB if anyone
            // peeks at the raw arena memory.
            let buf = vec![0u8; cap];
            arenas.push(Arena { buf, used: 0 });
        }
        let arena = arenas.last_mut().unwrap();
        let ptr = unsafe { arena.buf.as_mut_ptr().add(arena.used) };
        arena.used += size;
        ptr
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gc_reset() {
    ARENAS.with(|cell| {
        cell.borrow_mut().clear();
    });
}

// ---------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------
//
// Starts a minimal blocking TCP listener on `addr`. For every
// incoming connection it spawns an OS thread that reads the HTTP
// request (ignored beyond parsing the request line), then writes
// a static `200 OK\r\nContent-Length: 2\r\n\r\nok` response and
// closes. Native programs that call `http::serve(addr, handler)`
// land here: the handler is ignored today because cross-FFI
// dispatch into Gossamer code isn't wired yet, but the listener
// keeps the process alive and accepts connections — enough to
// measure end-to-end request handling.
//
// Returns `!` (never returns); the call blocks forever on
// `listener.incoming()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_serve(addr: *const c_char) -> ! {
    let addr_s = if addr.is_null() {
        "0.0.0.0:8080".to_string()
    } else {
        unsafe { CStr::from_ptr(addr).to_string_lossy().into_owned() }
    };
    let listener = match TcpListener::bind(&addr_s) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("gos_rt_http_serve: bind {addr_s} failed: {e}");
            std::process::exit(1);
        }
    };
    for stream in listener.incoming().flatten() {
        std::thread::spawn(move || handle_http_conn(stream));
    }
    std::process::exit(0);
}

fn handle_http_conn(mut stream: TcpStream) {
    // Loop to support HTTP keep-alive: most benchmark harnesses
    // reuse one TCP connection across every request. Break when
    // the peer closes the socket or we get a malformed request.
    let mut buf = [0u8; 8192];
    loop {
        // Read just enough to cover request line + headers. For
        // this minimal server we discard the body entirely.
        let n = match stream.read(&mut buf) {
            Ok(0) => return,
            Ok(n) => n,
            Err(_) => return,
        };
        let _ = n;
        let resp = b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
        if stream.write_all(resp).is_err() {
            return;
        }
    }
}

// ---------------------------------------------------------------
// Panic
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_panic(msg: *const c_char) {
    let text = if msg.is_null() {
        "panic".to_string()
    } else {
        unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
    };
    // Match the unified diagnostic-code prefix the VM /
    // tree-walker use so both execution modes tag panics with
    // `error[GX0005]` — keeps user-visible stderr identical
    // whether `gos run` took the native path or fell back.
    eprintln!("error[GX0005]: panic: {text}");
    std::process::abort();
}

// ---------------------------------------------------------------
// Exit
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_exit(code: i32) -> ! {
    std::process::exit(code);
}

// ---------------------------------------------------------------
// Time (seconds since UNIX epoch as f64 — interpreter parity)
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_now() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

// ---------------------------------------------------------------
// Math helpers
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_sqrt(x: f64) -> f64 {
    x.sqrt()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_pow(x: f64, y: f64) -> f64 {
    x.powf(y)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_sin(x: f64) -> f64 {
    x.sin()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_cos(x: f64) -> f64 {
    x.cos()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_log(x: f64) -> f64 {
    x.ln()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_exp(x: f64) -> f64 {
    x.exp()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_abs(x: f64) -> f64 {
    x.abs()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_floor(x: f64) -> f64 {
    x.floor()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_ceil(x: f64) -> f64 {
    x.ceil()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as i64)
}

// ---------------------------------------------------------------
// Mutex<T> primitive
// ---------------------------------------------------------------
//
// Naked synchronisation primitive — no payload, no RAII guard,
// the user follows lock/unlock discipline. Backed by
// `parking_lot::Mutex<()>` so contention uses futexes on
// Linux. The pointer is heap-allocated and shared by every
// goroutine that captures it.

pub struct GosMutex {
    inner: parking_lot::Mutex<()>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_new() -> *mut GosMutex {
    Box::into_raw(Box::new(GosMutex {
        inner: parking_lot::Mutex::new(()),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_lock(m: *mut GosMutex) {
    if m.is_null() {
        return;
    }
    let m = unsafe { &*m };
    // Forget the guard — the user calls unlock explicitly.
    let guard = m.inner.lock();
    std::mem::forget(guard);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_unlock(m: *mut GosMutex) {
    if m.is_null() {
        return;
    }
    // SAFETY: matched with the `forget` in lock — the lock is
    // held and we now release it. Releasing an unlocked mutex
    // is undefined; the user's discipline (one lock per
    // unlock) is required.
    let m = unsafe { &*m };
    unsafe { m.inner.force_unlock() };
}

// ---------------------------------------------------------------
// WaitGroup primitive
// ---------------------------------------------------------------
//
// Mirrors `sync.WaitGroup` in Go. `add(n)` bumps a counter,
// `done()` decrements, `wait()` blocks until the counter hits
// zero. Implemented as `(parking_lot::Mutex<i64>, parking_lot
// ::Condvar)`.

pub struct GosWaitGroup {
    counter: parking_lot::Mutex<i64>,
    cv: parking_lot::Condvar,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_new() -> *mut GosWaitGroup {
    Box::into_raw(Box::new(GosWaitGroup {
        counter: parking_lot::Mutex::new(0),
        cv: parking_lot::Condvar::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_add(wg: *mut GosWaitGroup, n: i64) {
    if wg.is_null() {
        return;
    }
    let wg = unsafe { &*wg };
    let mut c = wg.counter.lock();
    *c += n;
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_done(wg: *mut GosWaitGroup) {
    if wg.is_null() {
        return;
    }
    let wg = unsafe { &*wg };
    let mut c = wg.counter.lock();
    *c -= 1;
    if *c <= 0 {
        wg.cv.notify_all();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_wait(wg: *mut GosWaitGroup) {
    if wg.is_null() {
        return;
    }
    let wg = unsafe { &*wg };
    let mut c = wg.counter.lock();
    while *c > 0 {
        wg.cv.wait(&mut c);
    }
}

// ---------------------------------------------------------------
// Heap [i64] primitive
// ---------------------------------------------------------------
//
// Small-but-essential heap-backed array shared by reference
// across goroutines. Same memory model as Go's `make([]int64,
// n)`; the user holds the pointer as an i64 and passes it
// through `go expr` / channels. Indexing goes through the
// runtime so the language doesn't have to grow `&mut [T]`
// semantics for fan-out workloads.

#[repr(C)]
pub struct GosI64Vec {
    /// Length in elements.
    pub len: i64,
    /// Heap-allocated backing storage. `len * 8` bytes,
    /// 8-byte-aligned.
    pub data: *mut i64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_i64_new(len: i64) -> *mut GosI64Vec {
    if len < 0 {
        return std::ptr::null_mut();
    }
    let n = len as usize;
    let mut v: Vec<i64> = vec![0i64; n];
    let data = v.as_mut_ptr();
    std::mem::forget(v);
    Box::into_raw(Box::new(GosI64Vec { len, data }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_i64_free(v: *mut GosI64Vec) {
    if v.is_null() {
        return;
    }
    let v = unsafe { Box::from_raw(v) };
    if !v.data.is_null() {
        let n = v.len as usize;
        unsafe {
            let _ = Vec::from_raw_parts(v.data, n, n);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_i64_get(v: *const GosI64Vec, idx: i64) -> i64 {
    if v.is_null() || idx < 0 {
        return 0;
    }
    let v = unsafe { &*v };
    if idx >= v.len || v.data.is_null() {
        return 0;
    }
    unsafe { *v.data.add(idx as usize) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_i64_set(v: *mut GosI64Vec, idx: i64, val: i64) {
    if v.is_null() || idx < 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if idx >= v_ref.len || v_ref.data.is_null() {
        return;
    }
    unsafe { *v_ref.data.add(idx as usize) = val };
}

/// Length accessor for the heap vec — separate from
/// `gos_rt_arr_len` so the codegen can route by symbol.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_i64_len(v: *const GosI64Vec) -> i64 {
    if v.is_null() {
        return 0;
    }
    unsafe { (*v).len }
}

/// Bulk write `v[start..start+count]` to stdout, emitting a
/// newline after every `line_width` bytes. Used by fasta-style
/// programs that fill a worker buffer then need to flush it
/// with line breaks. Single FFI call instead of one per
/// line.
#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_heap_i64_write_lines_to_stdout(
    v: *const GosI64Vec,
    start: i64,
    count: i64,
    line_width: i64,
) {
    if v.is_null() || start < 0 || count <= 0 || line_width <= 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if v_ref.data.is_null() {
        return;
    }
    let end = start.saturating_add(count);
    if end > v_ref.len {
        return;
    }
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let mut cur = unsafe { *len_ptr };
    let mut col: i64 = 0;
    let mut idx = start as usize;
    let end = (start + count) as usize;
    while idx < end {
        // Need at least 1 byte; if buffer full, flush.
        if cur >= STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
        let avail = STDOUT_BUF_SIZE - cur;
        // Plan a packed run that fits in the remaining
        // buffer space and doesn't cross the next newline.
        let chars_to_eol = (line_width - col) as usize;
        let chars_left = end - idx;
        let take = std::cmp::min(chars_to_eol, std::cmp::min(chars_left, avail));
        unsafe {
            for i in 0..take {
                *(*bytes_ptr).as_mut_ptr().add(cur + i) = *v_ref.data.add(idx + i) as u8;
            }
        }
        cur += take;
        idx += take;
        col += take as i64;
        if col >= line_width {
            // Append newline if room (otherwise flush first).
            if cur >= STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
            unsafe {
                *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
            }
            cur += 1;
            col = 0;
        }
    }
    // Trailing newline if we ended mid-line (matches the
    // bench-game fasta convention: the last line is short
    // but still terminated with '\n').
    if col > 0 {
        if cur >= STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
        unsafe {
            *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
        }
        cur += 1;
    }
    unsafe { *len_ptr = cur };
}

/// Bulk-write the low byte of every i64 slot in
/// `v[start..start+count]` to stdout. Used by the
/// multi-threaded fasta variant: each worker fills a slice
/// of a shared heap vec; main writes ranges out in order
/// without per-byte FFI cost.
#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_heap_i64_write_bytes_to_stdout(
    v: *const GosI64Vec,
    start: i64,
    count: i64,
) {
    if v.is_null() || start < 0 || count <= 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if v_ref.data.is_null() {
        return;
    }
    let end = start.saturating_add(count);
    if end > v_ref.len {
        return;
    }
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let mut cur = unsafe { *len_ptr };
    let n = count as usize;
    let mut idx = start as usize;
    let mut written = 0usize;
    while written < n {
        let avail = STDOUT_BUF_SIZE - cur;
        let take = std::cmp::min(avail, n - written);
        unsafe {
            for i in 0..take {
                *(*bytes_ptr).as_mut_ptr().add(cur + i) = *v_ref.data.add(idx + i) as u8;
            }
        }
        cur += take;
        idx += take;
        written += take;
        if cur == STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
    }
    unsafe { *len_ptr = cur };
}

// ---------------------------------------------------------------
// Heap [u8] primitive (`U8Vec`)
// ---------------------------------------------------------------
//
// Mirrors `GosI64Vec` but stores one byte per element. The
// motivating use case is fasta-style scratch buffers where each
// element is a single ASCII character — using `i64` storage
// blew memory up by 8x with no upside since the workers only
// ever write 0..=255.

#[repr(C)]
pub struct GosU8Vec {
    /// Length in elements (= bytes).
    pub len: i64,
    /// Heap-allocated backing storage. `len` bytes, 1-byte
    /// aligned.
    pub data: *mut u8,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_new(len: i64) -> *mut GosU8Vec {
    if len < 0 {
        return std::ptr::null_mut();
    }
    let n = len as usize;
    let mut v: Vec<u8> = vec![0u8; n];
    let data = v.as_mut_ptr();
    std::mem::forget(v);
    Box::into_raw(Box::new(GosU8Vec { len, data }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_free(v: *mut GosU8Vec) {
    if v.is_null() {
        return;
    }
    let v = unsafe { Box::from_raw(v) };
    if !v.data.is_null() {
        let n = v.len as usize;
        unsafe {
            let _ = Vec::from_raw_parts(v.data, n, n);
        }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_get(v: *const GosU8Vec, idx: i64) -> i64 {
    if v.is_null() || idx < 0 {
        return 0;
    }
    let v = unsafe { &*v };
    if idx >= v.len || v.data.is_null() {
        return 0;
    }
    unsafe { i64::from(*v.data.add(idx as usize)) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_set(v: *mut GosU8Vec, idx: i64, val: i64) {
    if v.is_null() || idx < 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if idx >= v_ref.len || v_ref.data.is_null() {
        return;
    }
    // Truncate to a byte; callers pass `i64`-typed source values
    // that always live in `0..=255` for this use case.
    unsafe { *v_ref.data.add(idx as usize) = val as u8 };
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_len(v: *const GosU8Vec) -> i64 {
    if v.is_null() {
        return 0;
    }
    unsafe { (*v).len }
}

/// Bulk write `v[start..start+count]` to stdout, emitting a
/// newline after every `line_width` bytes. Single FFI call so
/// fasta-shape programs don't pay one `gos_rt_print_*` per byte.
#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_heap_u8_write_lines_to_stdout(
    v: *const GosU8Vec,
    start: i64,
    count: i64,
    line_width: i64,
) {
    if v.is_null() || start < 0 || count <= 0 || line_width <= 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if v_ref.data.is_null() {
        return;
    }
    let end = start.saturating_add(count);
    if end > v_ref.len {
        return;
    }
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let mut cur = unsafe { *len_ptr };
    let mut col: i64 = 0;
    let mut idx = start as usize;
    let end = (start + count) as usize;
    while idx < end {
        if cur >= STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
        let avail = STDOUT_BUF_SIZE - cur;
        let chars_to_eol = (line_width - col) as usize;
        let chars_left = end - idx;
        let take = std::cmp::min(chars_to_eol, std::cmp::min(chars_left, avail));
        // u8 → u8 plain memcpy.
        unsafe {
            std::ptr::copy_nonoverlapping(
                v_ref.data.add(idx),
                (*bytes_ptr).as_mut_ptr().add(cur),
                take,
            );
        }
        cur += take;
        idx += take;
        col += take as i64;
        if col >= line_width {
            if cur >= STDOUT_BUF_SIZE {
                unsafe {
                    raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
                }
                cur = 0;
            }
            unsafe {
                *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
            }
            cur += 1;
            col = 0;
        }
    }
    if col > 0 {
        if cur >= STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
        unsafe {
            *(*bytes_ptr).as_mut_ptr().add(cur) = b'\n';
        }
        cur += 1;
    }
    unsafe { *len_ptr = cur };
}

/// Bulk-write the bytes of `v[start..start+count]` to stdout,
/// no line breaks. Used by the phased fasta variant where one
/// "phase" fills the buffer with whole 60-byte lines (newlines
/// already in the buffer) and then dumps it.
#[unsafe(no_mangle)]
#[allow(static_mut_refs)]
pub unsafe extern "C" fn gos_rt_heap_u8_write_bytes_to_stdout(
    v: *const GosU8Vec,
    start: i64,
    count: i64,
) {
    if v.is_null() || start < 0 || count <= 0 {
        return;
    }
    let v_ref = unsafe { &*v };
    if v_ref.data.is_null() {
        return;
    }
    let end = start.saturating_add(count);
    if end > v_ref.len {
        return;
    }
    let bytes_ptr = &raw mut GOS_RT_STDOUT_BYTES;
    let len_ptr = &raw mut GOS_RT_STDOUT_LEN;
    let mut cur = unsafe { *len_ptr };
    let n = count as usize;
    let mut idx = start as usize;
    let mut written = 0usize;
    while written < n {
        if cur >= STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
        let avail = STDOUT_BUF_SIZE - cur;
        let take = std::cmp::min(avail, n - written);
        unsafe {
            std::ptr::copy_nonoverlapping(
                v_ref.data.add(idx),
                (*bytes_ptr).as_mut_ptr().add(cur),
                take,
            );
        }
        cur += take;
        idx += take;
        written += take;
        if cur == STDOUT_BUF_SIZE {
            unsafe {
                raw_write_stdout(std::slice::from_raw_parts((*bytes_ptr).as_ptr(), cur));
            }
            cur = 0;
        }
    }
    unsafe { *len_ptr = cur };
}

// ---------------------------------------------------------------
// Atomic<i64> primitive
// ---------------------------------------------------------------
//
// Heap-allocated `AtomicI64`. Used for shared work-counters
// (e.g. handing out chunk indices to workers) and for
// once-style flags. Mirrors Go's `atomic.Int64`.

pub struct GosAtomicI64 {
    inner: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_new(initial: i64) -> *mut GosAtomicI64 {
    Box::into_raw(Box::new(GosAtomicI64 {
        inner: AtomicI64::new(initial),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load(a: *const GosAtomicI64) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.load(Ordering::SeqCst)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store(a: *mut GosAtomicI64, val: i64) {
    if a.is_null() {
        return;
    }
    let a = unsafe { &*a };
    a.inner.store(val, Ordering::SeqCst);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_fetch_add(a: *mut GosAtomicI64, delta: i64) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.fetch_add(delta, Ordering::SeqCst)
}

// ---------------------------------------------------------------
// LCG jump-ahead helper
// ---------------------------------------------------------------
//
// fasta-style benchmarks use a Lehmer / Park-Miller LCG of the
// form `state' = (state * IA + IC) mod IM`. Multi-threaded
// fasta needs each worker to start at a different point in the
// stream so the streams interleave correctly. This helper
// computes `LCG^n(state)` in O(log n) time using fast modular
// exponentiation.

/// Compute `LCG^n(state)` where the LCG is
/// `s' = (s * ia + ic) mod im`. Returns the state after `n`
/// applications. `n` is clamped to non-negative.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_lcg_jump(state: i64, ia: i64, ic: i64, im: i64, n: i64) -> i64 {
    if n <= 0 || im <= 0 {
        return state;
    }
    // Apply the recurrence n times via doubling on the
    // affine transform `s -> a*s + b mod m`.
    //
    // Composition: (a1 * (a2 * s + b2) + b1) = a1*a2*s + a1*b2 + b1.
    // So composing two transforms (a, b) is (a1*a2, a1*b2 + b1).
    // Doubling: (a, b) -> (a*a, a*b + b).
    let mut a = ia.rem_euclid(im);
    let mut b = ic.rem_euclid(im);
    let mut result_a: i64 = 1; // identity affine: 1*s + 0
    let mut result_b: i64 = 0;
    let m = im;
    let mut k = n;
    while k > 0 {
        if k & 1 == 1 {
            // result <- a * result_a, a * result_b + b
            // i.e. composition: (result_a, result_b) ∘ (a, b)
            // applied as `(result_a, result_b) := compose((a, b), (result_a, result_b))`
            let new_a = mul_mod(a, result_a, m);
            let new_b = (mul_mod(a, result_b, m) + b).rem_euclid(m);
            result_a = new_a;
            result_b = new_b;
        }
        // Double the (a, b) transform.
        let next_a = mul_mod(a, a, m);
        let next_b = (mul_mod(a, b, m) + b).rem_euclid(m);
        a = next_a;
        b = next_b;
        k >>= 1;
    }
    (mul_mod(result_a, state.rem_euclid(m), m) + result_b).rem_euclid(m)
}

/// `(a * b) mod m` without i128 overflow on i64-sized
/// operands. fasta's IM is 139968, well within i32 range, so
/// this is fine on x86_64; the i128 widening keeps it correct
/// for any callers that pick larger moduli.
fn mul_mod(a: i64, b: i64, m: i64) -> i64 {
    let prod = (a as i128) * (b as i128);
    (prod.rem_euclid(m as i128)) as i64
}

// ----- Fn-trait coercion trampolines -----
//
// When a bare `fn item` (or the address of a non-capturing lifted
// closure) is coerced to `Fn(args) -> ret`, MIR allocates a
// 16-byte env blob `[trampoline_addr, real_fn_addr]` and stores
// `gos_rt_fn_tramp_<arity>` at offset 0. The closure-call dispatch
// in the cranelift codegen then invokes that trampoline as
// `f(env, args…)`; the trampoline reads the real fn from `env+8`
// and forwards the args, dropping the env. Capturing closures
// don't need this — their env already carries the lifted body's
// (env, args) signature at offset 0.
//
// Arities 0..=8 cover every higher-order shape the stdlib uses
// today (most are arity ≤ 3); add more if a real call site needs
// it.

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_0(env: *const u8) -> i64 {
    // SAFETY: `env` was constructed by the MIR coercion site as a
    // 16-byte blob whose word at offset 8 is the real fn ptr.
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn() -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
    real_fn()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_1(env: *const u8, a0: i64) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64) -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_2(env: *const u8, a0: i64, a1: i64) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64) -> i64 = unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_3(env: *const u8, a0: i64, a1: i64, a2: i64) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_4(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2, a3)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_5(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2, a3, a4)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_6(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2, a3, a4, a5)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_7(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2, a3, a4, a5, a6)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fn_tramp_8(
    env: *const u8,
    a0: i64,
    a1: i64,
    a2: i64,
    a3: i64,
    a4: i64,
    a5: i64,
    a6: i64,
    a7: i64,
) -> i64 {
    let real_fn_addr = unsafe { core::ptr::read_unaligned(env.add(8).cast::<usize>()) };
    let real_fn: extern "C" fn(i64, i64, i64, i64, i64, i64, i64, i64) -> i64 =
        unsafe { core::mem::transmute(real_fn_addr) };
    real_fn(a0, a1, a2, a3, a4, a5, a6, a7)
}
