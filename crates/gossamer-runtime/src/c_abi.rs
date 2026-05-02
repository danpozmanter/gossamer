//! C-ABI runtime surface linked into every native Gossamer program.
//! Every symbol in this module is exported under the `gos_rt_*`
//! prefix so the Cranelift codegen can call them by name. All
//! `extern "C"` functions run in unsafe context — the compiler emits
//! raw pointers and trusts the contract described next to each
//! symbol. Failure modes are documented per symbol; they never
//! panic across the FFI boundary.

#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
// FFI signatures must match the Cranelift / LLVM call sites
// exactly. Keep these allows at file scope rather than dotting
// per-call-site annotations across the C-ABI surface:
// `similar_names` covers `argc`/`argv` Unix convention;
// `many_single_char_names` covers `p`/`n`/`k` in tight memory
// helpers; `items_after_statements` permits inner helper fns
// alongside the call site they document; `same_length_and_capacity`
// fires on `Vec::from_raw_parts(p, n, n)` reconstructing exact
// allocations; `cast_lossless` would force `i64::from(x)` shapes
// that obscure hot-path arithmetic; `doc_markdown` would force
// backticks around every C-ABI symbol name in summary lines.
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
// Pointer casts in this file all reinterpret memory the runtime
// allocates through `gos_rt_gc_alloc`, which is 8-byte aligned, or
// `Vec`-backed buffers (whose alignment matches the elem type). The
// linter cannot see the upstream alignment guarantee and would fire
// on every cast; concentrating the allow at file scope keeps the
// individual sites readable.
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
// Mutable statics back the C-ABI / LLVM-inlined surface (`STDOUT_BUF`,
// `STDOUT_LEN`, etc. — see `stdout_buffer_globals.md`). The lowerer
// emits load/store directly against these symbols, so they have to
// remain `static mut`; the lint flags every read but the contract
// is documented at each declaration.
#![allow(static_mut_refs)]

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
    // Initialise the Rust runtime's per-process state. The
    // Cranelift-emitted `main` shim is a plain
    // `extern "C" fn main(int, **char) -> int`, so libc's
    // `__libc_start_main` calls it directly — bypassing the
    // `std::rt::lang_start` wrapper that rustc generates around
    // a Rust `fn main()`. Without that wrapper several pieces of
    // standard-library state are left in their lazy-init defaults:
    //
    //   - `SIGPIPE` keeps its default `SIG_DFL` action, so the
    //     first `write_all` to a half-closed peer terminates the
    //     entire process with no diagnostic.
    //   - The main-thread stack guard is never installed, so
    //     stack overflow on the main thread silently corrupts
    //     adjacent mappings instead of trapping on a guard
    //     page.
    //   - `std::thread::Thread`'s name table for the main thread
    //     is empty, which `panic` printing relies on.
    //
    // Spawning and joining a no-op `std::thread` here forces the
    // first-use lazy initialisation paths (`thread::Builder`,
    // `Thread::new`, the parking primitives) to run during a
    // single-threaded prologue rather than during a concurrent
    // burst, which is the exact pattern that triggered the
    // "double free or corruption (out)" / "munmap_chunk(): invalid
    // pointer" abort under HTTP keep-alive load. We additionally
    // ignore SIGPIPE so writes to closed connections surface as
    // `EPIPE` instead of process-wide termination.
    runtime_init();
}

#[cfg(unix)]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        // SIGPIPE → SIG_IGN. Mirrors what `std::rt::lang_start`'s
        // `sys::unix::init` does. Without this, a write to a
        // closed peer (very common under heavy keep-alive load)
        // terminates the process.
        unsafe {
            libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        }
        // Pre-warm Rust's thread machinery. The first
        // `std::thread::spawn` call lazily initialises the
        // thread name table, parking primitives, and platform
        // backend. Doing it once here, single-threaded, removes a
        // race that under the HTTP server's accept-and-spawn-
        // burst pattern triggered glibc heap corruption when many
        // threads exited before their TLS destructors had been
        // assigned slot indices.
        let handle = std::thread::Builder::new()
            .name("gos-rt-init".to_string())
            .spawn(|| {})
            .expect("spawn rt init thread");
        let _ = handle.join();
    });
}

#[cfg(not(unix))]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        let handle = std::thread::Builder::new()
            .name("gos-rt-init".to_string())
            .spawn(|| {})
            .expect("spawn rt init thread");
        let _ = handle.join();
    });
}

/// Returns the pointer to the first user-passed argument. A
/// Place projection with stride 8 reads successive strings
/// through it; `.len()` routes to `gos_rt_arr_len` which
/// short-circuits on this exact pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_args() -> *const c_char {
    ARGS_PTR.load(Ordering::SeqCst) as *const c_char
}

/// `os::env(name) -> Option<String>`. Compiled tier returns a
/// `*mut GosResult` shaped as Option (disc 0 = Some, 1 = None).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_env(name: *const c_char) -> *mut GosResult {
    if name.is_null() {
        return unsafe { gos_rt_result_new(1, 0) };
    }
    let key = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
    match std::env::var(&key) {
        Ok(value) => {
            let cs = alloc_cstring(value.as_bytes());
            unsafe { gos_rt_result_new(0, cs as i64) }
        }
        Err(_) => unsafe { gos_rt_result_new(1, 0) },
    }
}

/// `os::cwd() -> Result<String, errors::Error>`. Compiled tier
/// returns a `*mut GosResult` (disc 0 = Ok, 1 = Err).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_cwd() -> *mut GosResult {
    match std::env::current_dir() {
        Ok(path) => {
            let cs = alloc_cstring(path.to_string_lossy().as_bytes());
            unsafe { gos_rt_result_new(0, cs as i64) }
        }
        Err(e) => {
            let msg = format!("cwd: {e}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            unsafe { gos_rt_result_new(1, err as i64) }
        }
    }
}

/// `fs::list_dir(path) -> Result<[DirInfo], errors::Error>`.
/// Each `DirInfo` is a 7-slot heap aggregate matching the
/// interpreter's struct field order:
/// `[name: *c_char, path: *c_char, is_file: i64, is_dir: i64,
/// is_symlink: i64, size: i64, modified_ms: i64]`. Field
/// indices match the MIR projections emitted for
/// `entry.<field>` access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_list_dir(path: *const c_char) -> *mut GosResult {
    let p = if path.is_null() {
        ".".to_string()
    } else {
        unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
    };
    let mut entries: Vec<std::fs::DirEntry> = match std::fs::read_dir(&p) {
        Ok(it) => it.flatten().collect(),
        Err(e) => {
            let msg = format!("list_dir: {e}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
    };
    entries.sort_by_key(std::fs::DirEntry::file_name);
    let out = unsafe { gos_rt_vec_new(8) };
    for entry in entries {
        let Ok(meta) = entry.metadata() else { continue };
        let Ok(ft) = entry.file_type() else { continue };
        let name_str = entry.file_name().to_string_lossy().into_owned();
        let path_str = entry.path().to_string_lossy().into_owned();
        let name_cs = alloc_cstring(name_str.as_bytes()) as i64;
        let path_cs = alloc_cstring(path_str.as_bytes()) as i64;
        let is_file = i64::from(ft.is_file());
        let is_dir = i64::from(ft.is_dir());
        let is_symlink = i64::from(ft.is_symlink());
        let size = i64::try_from(meta.len()).unwrap_or(0);
        let modified_ms = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0);
        // 7 fields * 8 bytes = 56 bytes.
        let layout = std::alloc::Layout::from_size_align(56, 8).unwrap();
        let blob = unsafe { std::alloc::alloc(layout) as *mut i64 };
        if blob.is_null() {
            continue;
        }
        unsafe {
            *blob.add(0) = name_cs;
            *blob.add(1) = path_cs;
            *blob.add(2) = is_file;
            *blob.add(3) = is_dir;
            *blob.add(4) = is_symlink;
            *blob.add(5) = size;
            *blob.add(6) = modified_ms;
        }
        let entry_val = blob as i64;
        unsafe {
            gos_rt_vec_push(out, std::ptr::addr_of!(entry_val).cast::<u8>());
        }
    }
    unsafe { gos_rt_result_new(0, out as i64) }
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
pub unsafe extern "C" fn gos_rt_str_is_empty(s: *const c_char) -> bool {
    unsafe { gos_rt_str_len(s) == 0 }
}

/// Generic length-zero check used by `is_empty` for any
/// receiver whose length is reachable through `gos_rt_len`
/// (Vec / array / slice / hashmap …).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_len_is_zero(p: *const i64) -> bool {
    unsafe { gos_rt_len(p) == 0 }
}

/// Clones a `*mut GosVec` element-by-element. Used by
/// `xs.to_vec()` so the result is independent of the source —
/// without this, the previous identity lowering aliased the
/// source buffer and mutations like `out.swap(i, j)` clobbered
/// the caller's input. Header + data are arena-allocated; the
/// next `gos_rt_gc_reset` reclaims them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_clone(src: *const GosVec) -> *mut GosVec {
    if src.is_null() {
        return unsafe { gos_rt_vec_new(8) };
    }
    let s = unsafe { &*src };
    let header = unsafe { gos_rt_gc_alloc(std::mem::size_of::<GosVec>() as u64) };
    if header.is_null() {
        return unsafe { gos_rt_vec_new(s.elem_bytes) };
    }
    let bytes = (s.len as usize) * (s.elem_bytes as usize);
    let data = if bytes == 0 {
        std::ptr::null_mut::<u8>()
    } else {
        let p = unsafe { gos_rt_gc_alloc(bytes as u64) };
        if !p.is_null() && !s.ptr.is_null() {
            unsafe {
                std::ptr::copy_nonoverlapping(s.ptr, p, bytes);
            }
        }
        p
    };
    let v = header.cast::<GosVec>();
    unsafe {
        std::ptr::write(
            v,
            GosVec {
                len: s.len,
                cap: s.len,
                elem_bytes: s.elem_bytes,
                ptr: data,
            },
        );
    }
    v
}

/// Materialises `s.as_bytes()` as a real `GosVec<u8>` so callees
/// receiving `&[u8]` can call `.len()` / `.iter()` / index it
/// the same way they would any other slice. The previous
/// identity lowering returned the raw c-string ptr — `.len()`
/// on it read the first 8 content bytes as a Vec length prefix,
/// and `.iter()` walked off into garbage. Backing buffer +
/// header are arena-allocated; the next `gos_rt_gc_reset`
/// reclaims them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_as_bytes(s: *const c_char) -> *mut GosVec {
    let len = if s.is_null() {
        0
    } else {
        unsafe { CStr::from_ptr(s).to_bytes().len() }
    };
    // The returned Vec is consumed by `bytes[i]` indexing in
    // user code, which the codegen lowers via the Vec/Slice
    // dispatch (`gos_rt_vec_get_i64`) — every slot is i64-shaped.
    // Materialise each byte as a zero-extended i64 so the load
    // returns the byte's value rather than 8 packed buffer
    // bytes. Use `gos_rt_vec_with_capacity` so the resulting
    // header is `Box::from_raw`-compatible — the auto-emitted
    // `gos_rt_vec_free` at scope-end relies on that
    // provenance.
    let v = unsafe { gos_rt_vec_with_capacity(8, len as i64) };
    if v.is_null() {
        return v;
    }
    if len > 0 && !s.is_null() {
        unsafe {
            let src = s.cast::<u8>();
            let header = &mut *v;
            let dst = header.ptr.cast::<i64>();
            for i in 0..len {
                *dst.add(i) = i64::from(*src.add(i));
            }
            header.len = len as i64;
        }
    }
    v
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
    // Cheap empty-checks that only touch the first byte. The full
    // `CStr::from_ptr(a).to_bytes()` form calls `strlen`, which on
    // a growing `s = s + c` accumulator is O(strlen(s)) per
    // iteration — turning the seq-build loop into a multi-second
    // strlen-dominated walk even after the arena O(N²) fix. The
    // fast path (extend-in-place) doesn't need `a`'s length at
    // all; `try_extend_last_cstring` reads it from
    // `arena.last_len`.
    let a_empty = a.is_null() || unsafe { *a.cast::<u8>() } == 0;
    let b_empty = b.is_null() || unsafe { *b.cast::<u8>() } == 0;
    // Fast path: if `a` is the most recent arena allocation,
    // extend it in place. Only `b` needs an actual length (it's
    // typically tiny — a literal, a single-char fragment, or a
    // numeric digit).
    if !a_empty && !b_empty {
        let b_bytes = unsafe { CStr::from_ptr(b).to_bytes() };
        let extended = try_extend_last_cstring(a, b_bytes);
        if !extended.is_null() {
            return extended;
        }
    }
    // Slow path: pay the strlen on both strings.
    let a_bytes: &[u8] = if a_empty {
        &[]
    } else {
        unsafe { CStr::from_ptr(a).to_bytes() }
    };
    let b_bytes: &[u8] = if b_empty {
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

/// `s == t` for string operands. Compares byte-for-byte. NULL
/// pointers compare equal to empty strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_eq(a: *const c_char, b: *const c_char) -> bool {
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
    a == b
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

/// Splits `s` on every occurrence of `sep` and returns a fresh
/// `*mut GosVec` of c-string pointers. Empty `sep` yields a
/// single-element vec containing the whole string (mirrors Rust's
/// `split` for the empty separator). Each split slice gets its
/// own heap-allocated nul-terminated copy so the caller can
/// hold them past the underlying string's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_split(s: *const c_char, sep: *const c_char) -> *mut GosVec {
    let s = if s.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
    };
    let sep = if sep.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(sep).to_str().unwrap_or("") }
    };
    let parts: Vec<*mut c_char> = if sep.is_empty() {
        vec![alloc_cstring(s.as_bytes())]
    } else {
        s.split(sep).map(|p| alloc_cstring(p.as_bytes())).collect()
    };
    let vec = unsafe { gos_rt_vec_with_capacity(8, parts.len() as i64) };
    for p in &parts {
        let pv = *p as i64;
        unsafe {
            gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>());
        }
    }
    vec
}

/// Splits `s` on `\n` and returns a fresh `*mut GosVec` of
/// c-string pointers, one per line. Trailing empty lines
/// (from `"a\nb\n"`) are dropped to mirror Rust's `lines()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_lines(s: *const c_char) -> *mut GosVec {
    let s = if s.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
    };
    let parts: Vec<*mut c_char> = s.lines().map(|l| alloc_cstring(l.as_bytes())).collect();
    let vec = unsafe { gos_rt_vec_with_capacity(8, parts.len() as i64) };
    for p in &parts {
        let pv = *p as i64;
        unsafe {
            gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>());
        }
    }
    vec
}

/// Returns `s` repeated `n` times. Rust's `String::repeat`
/// semantics: `n=0` returns the empty string, `n=1` returns a
/// fresh copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_repeat(s: *const c_char, n: i64) -> *mut c_char {
    let s = if s.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
    };
    let n = if n < 0 { 0 } else { n as usize };
    alloc_cstring(s.repeat(n).as_bytes())
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

/// `text.parse::<i64>()` returning a `Result<i64, errors::Error>`.
/// Err payload is a `*mut GosError` so user code can call
/// `e.message()` directly without `map_err`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_i64_result(s: *const c_char) -> *mut GosResult {
    if s.is_null() {
        let cs = std::ffi::CString::new("parse: null input").unwrap();
        let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
        return unsafe { gos_rt_result_new(1, err as i64) };
    }
    let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
    if let Ok(n) = text.parse::<i64>() {
        unsafe { gos_rt_result_new(0, n) }
    } else {
        let msg = format!(
            "unexpected byte 0x{:x} at 1:1",
            text.as_bytes().first().copied().unwrap_or(0)
        );
        let cs = std::ffi::CString::new(msg).unwrap_or_default();
        let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
        unsafe { gos_rt_result_new(1, err as i64) }
    }
}

/// `result.map_err(closure)`. If Err, calls closure and rebuilds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_map_err(
    result: *mut GosResult,
    closure: *const u8,
) -> *mut GosResult {
    if result.is_null() {
        return result;
    }
    let res = unsafe { &*result };
    if res.disc != 1 || closure.is_null() {
        return result;
    }
    // SAFETY: `closure` is a heap blob whose first word is the
    // lifted function's address (codegen invariant).
    let fn_addr = unsafe { *closure.cast::<i64>() };
    if fn_addr == 0 {
        return result;
    }
    let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr) };
    let new_payload = f(closure as i64, res.payload);
    unsafe { gos_rt_result_new(1, new_payload) }
}

/// `result.map(closure)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_map(
    result: *mut GosResult,
    closure: *const u8,
) -> *mut GosResult {
    if result.is_null() {
        return result;
    }
    let res = unsafe { &*result };
    if res.disc != 0 || closure.is_null() {
        return result;
    }
    let fn_addr = unsafe { *closure.cast::<i64>() };
    if fn_addr == 0 {
        return result;
    }
    let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr) };
    let new_payload = f(closure as i64, res.payload);
    unsafe { gos_rt_result_new(0, new_payload) }
}

/// `*cell` for `flag::Set::string` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_str(cell: *const *const c_char) -> *const c_char {
    if cell.is_null() {
        return std::ptr::null();
    }
    unsafe { *cell }
}

/// `*cell` for `flag::Set::uint` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_i64(cell: *const i64) -> i64 {
    if cell.is_null() {
        return 0;
    }
    unsafe { *cell }
}

/// `*cell` for `flag::Set::bool` cells, widened to i64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_bool(cell: *const bool) -> i64 {
    if cell.is_null() {
        return 0;
    }
    i64::from(unsafe { *cell })
}

/// `time::Duration::from_secs(n)` lowering — returns `n * 1000` as
/// the i64-millisecond Duration the compiled tier carries.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_secs(secs: i64) -> i64 {
    secs.saturating_mul(1_000)
}

// `flag::parse([decls])` declarative parser — takes an array of
// `FlagDecl`-shaped blobs and returns a `FlagMap` handle.
// Layout per blob: `[name_cs, short_char, kind_tag, int_val,
// str_cs]` (5 * 8 = 40 bytes). `kind_tag` is 0=Int, 1=Str, 2=Bool.
// Mirrors the interpreter's `builtin_flag_parse`.

#[derive(Debug, Clone)]
struct GosFlagMapEntry {
    name: String,
    short: Option<char>,
    kind: FlagKind,
    str_val: Option<Vec<u8>>,
    int_val: i64,
}

pub struct GosFlagMap {
    entries: Vec<GosFlagMapEntry>,
    positional: Vec<String>,
}

unsafe impl Send for GosFlagMap {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_parse(decls: *mut GosVec) -> *mut GosFlagMap {
    let mut entries: Vec<GosFlagMapEntry> = Vec::new();
    if !decls.is_null() {
        let len = unsafe { gos_rt_vec_len(decls) };
        for i in 0..len {
            let raw = unsafe { gos_rt_vec_get_i64(decls, i) };
            if raw == 0 {
                continue;
            }
            let blob = raw as *const i64;
            let name_cs = unsafe { *blob.add(0) } as *const c_char;
            let short_raw = unsafe { *blob.add(1) };
            let kind_tag = unsafe { *blob.add(2) };
            let int_val = unsafe { *blob.add(3) };
            let str_cs = unsafe { *blob.add(4) } as *const c_char;
            let name = if name_cs.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(name_cs).to_string_lossy().into_owned() }
            };
            let short = u32::try_from(short_raw).ok().and_then(char::from_u32);
            let kind = match kind_tag {
                0 => FlagKind::Int,
                1 => FlagKind::String,
                2 => FlagKind::Bool,
                _ => FlagKind::String,
            };
            let str_val = if matches!(kind, FlagKind::String) && !str_cs.is_null() {
                Some(unsafe { CStr::from_ptr(str_cs).to_bytes().to_vec() })
            } else {
                None
            };
            entries.push(GosFlagMapEntry {
                name,
                short,
                kind,
                str_val,
                int_val,
            });
        }
    }
    let positional = parse_argv_flag_values(
        &mut entries,
        ARGS_PTR.load(Ordering::SeqCst),
        ARGS_LEN.load(Ordering::SeqCst),
    );
    Box::into_raw(Box::new(GosFlagMap {
        entries,
        positional,
    }))
}

/// Parse `argv`/`argc` into positional strings, applying flag values
/// to `entries` in place.
fn parse_argv_flag_values(entries: &mut [GosFlagMapEntry], argv: usize, argc: i64) -> Vec<String> {
    let argv = argv as *const *const c_char;
    let mut idx: i64 = 0;
    let mut positional: Vec<String> = Vec::new();
    while idx < argc {
        let p = unsafe { *argv.offset(idx as isize) };
        if p.is_null() {
            idx += 1;
            continue;
        }
        let arg = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
        if arg == "--" {
            idx += 1;
            while idx < argc {
                let q = unsafe { *argv.offset(idx as isize) };
                if !q.is_null() {
                    let s = unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() };
                    positional.push(s);
                }
                idx += 1;
            }
            break;
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, explicit) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if let Some(entry) = entries.iter_mut().find(|e| e.name == name) {
                let value = if let Some(v) = explicit {
                    v
                } else if matches!(entry.kind, FlagKind::Bool) {
                    "true".to_string()
                } else if idx + 1 < argc {
                    idx += 1;
                    let q = unsafe { *argv.offset(idx as isize) };
                    if q.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() }
                    }
                } else {
                    String::new()
                };
                apply_decl_value(entry, &value);
                idx += 1;
                continue;
            }
            positional.push(arg);
            idx += 1;
            continue;
        }
        if let Some(rest) = arg.strip_prefix('-')
            && !rest.is_empty()
        {
            let mut chars = rest.chars();
            let first = chars.next().unwrap();
            let remainder: String = chars.collect();
            if let Some(entry) = entries.iter_mut().find(|e| e.short == Some(first)) {
                let value = if !remainder.is_empty() {
                    remainder
                } else if matches!(entry.kind, FlagKind::Bool) {
                    "true".to_string()
                } else if idx + 1 < argc {
                    idx += 1;
                    let q = unsafe { *argv.offset(idx as isize) };
                    if q.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() }
                    }
                } else {
                    String::new()
                };
                apply_decl_value(entry, &value);
                idx += 1;
                continue;
            }
        }
        positional.push(arg);
        idx += 1;
    }
    positional
}

fn apply_decl_value(entry: &mut GosFlagMapEntry, raw: &str) {
    match entry.kind {
        FlagKind::Int | FlagKind::Uint | FlagKind::Duration => {
            entry.int_val = raw.parse::<i64>().unwrap_or(entry.int_val);
        }
        FlagKind::Float => {
            entry.int_val = raw.parse::<f64>().unwrap_or(0.0).to_bits() as i64;
        }
        FlagKind::Bool => {
            entry.int_val = i64::from(matches!(raw, "true" | "1" | "yes" | "on"));
        }
        FlagKind::String | FlagKind::StringList => {
            entry.str_val = Some(raw.as_bytes().to_vec());
        }
    }
}

/// `FlagMap::get(map, key) -> Option<i64-or-string>`. Returns
/// `Result<int_or_str_ptr, ()>` (Result-as-Option in the
/// compiled tier) carrying either the i64 slot for numeric /
/// bool flags or the c-string pointer for string flags.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_map_get(
    map: *const GosFlagMap,
    key: *const c_char,
) -> *mut GosResult {
    if map.is_null() || key.is_null() {
        return unsafe { gos_rt_result_new(1, 0) };
    }
    let m = unsafe { &*map };
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    if let Some(entry) = m.entries.iter().find(|e| e.name == k) {
        let payload = match entry.kind {
            FlagKind::String | FlagKind::StringList => {
                let bytes = entry.str_val.as_deref().unwrap_or(&[]);
                alloc_cstring(bytes) as i64
            }
            _ => entry.int_val,
        };
        return unsafe { gos_rt_result_new(0, payload) };
    }
    // Suppress unused-field warning on positional (kept for
    // future surface — `flag::parse(...)?.positional`).
    let _ = &m.positional;
    unsafe { gos_rt_result_new(1, 0) }
}

/// `time::format_rfc3339(unix_ms) -> Result<String, errors::Error>`.
/// Renders a UTC RFC 3339 timestamp from a unix-milliseconds
/// instant. Mirrors the interpreter builtin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_format_rfc3339(unix_ms: i64) -> *mut GosResult {
    let secs = unix_ms.div_euclid(1_000);
    let nanos = (unix_ms.rem_euclid(1_000) * 1_000_000) as u32;
    let _ = nanos;
    let mut y: i64 = 1970;
    let mut remain = secs.div_euclid(86_400);
    let is_leap = |yr: i64| (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0;
    let dy = |yr: i64| if is_leap(yr) { 366 } else { 365 };
    if remain < 0 {
        while remain < 0 {
            y -= 1;
            remain += dy(y);
        }
    } else {
        while remain >= dy(y) {
            remain -= dy(y);
            y += 1;
        }
    }
    let dim = |m: i64, yr: i64| -> i64 {
        match m {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if is_leap(yr) {
                    29
                } else {
                    28
                }
            }
            _ => 30,
        }
    };
    let mut m = 1_i64;
    while remain >= dim(m, y) {
        remain -= dim(m, y);
        m += 1;
    }
    let day = remain + 1;
    let s = secs.rem_euclid(86_400);
    let h = s / 3600;
    let mi = (s % 3600) / 60;
    let se = s % 60;
    let s_str = format!("{y:04}-{m:02}-{day:02}T{h:02}:{mi:02}:{se:02}Z");
    let cs = alloc_cstring(s_str.as_bytes());
    unsafe { gos_rt_result_new(0, cs as i64) }
}

/// `time::Duration::from_millis(n)` lowering — Duration is already
/// stored as i64 ms in the compiled tier, so this is the identity.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_millis(ms: i64) -> i64 {
    ms
}

/// `*cell` for `flag::Set::float` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_f64(cell: *const f64) -> f64 {
    if cell.is_null() {
        return 0.0;
    }
    unsafe { *cell }
}

/// `*cell` for `flag::Set::string_list` cells. The cell stores a
/// `*mut GosVec` that the runtime owns; reads return a borrow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_vec(cell: *const *mut GosVec) -> *mut GosVec {
    if cell.is_null() {
        return std::ptr::null_mut();
    }
    unsafe { *cell }
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

/// Stringifies an *unsigned* 64-bit integer. Distinct from
/// `gos_rt_i64_to_str` so values `>= 2^63` print as their true
/// magnitude rather than a leading-`-` two's-complement view.
/// Used by the cranelift + LLVM lowerers when the source TyKind
/// resolves to `u8/u16/u32/u64/u128/usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_u64_to_str(n: u64) -> *mut c_char {
    alloc_cstring(n.to_string().as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_to_str(x: f64) -> *mut c_char {
    alloc_cstring(format!("{x}").as_bytes())
}

/// Stringifies an `f64` with `prec` fractional digits — the runtime
/// side of `format!("{:.N}", x)`. Routes through the Rust standard
/// library's float formatter so rounding matches the interpreter's
/// `{:.N}` Display output bit-for-bit. Negative `prec` is clamped to
/// zero; very large `prec` is clamped to a sane upper bound to keep
/// the allocation bounded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_prec_to_str(x: f64, prec: i64) -> *mut c_char {
    let prec = prec.clamp(0, 64) as usize;
    alloc_cstring(format!("{x:.prec$}").as_bytes())
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

// Process-global 64 KiB stdout buffer. The buffer's lifetime is
// the whole process, but every entry into the inline byte-write
// fast path takes the buffer mutex (`STDOUT_LOCK` below) so two
// goroutines on different OS threads cannot race on
// `GOS_RT_STDOUT_LEN`. The previous design (no lock) tore the
// length under any multi-thread output and is the C3 finding in
// `~/dev/contexts/lang/adversarial_analysis.md`.
//
// Performance: parking_lot's uncontended acquire/release is ~10 ns
// total. The LLVM lowerer takes the lock once per inline write
// region (a single byte, or a contiguous range — the array
// writer in `lower_stream_write_byte_array_inline` packs up to
// 65 K bytes per acquire). For fasta's 60-byte lines that is
// ~4 M acquires across 250 MB of output → ~40 ms of total mutex
// overhead, lost in the noise against the ~2 s of I/O.
//
// `STDOUT_LOCK` is exposed to the codegen via
// `gos_rt_stdout_acquire` / `gos_rt_stdout_release`. The codegen
// pairs them around any inline access; the runtime helpers
// (`gos_rt_print_*`) acquire it via the safe `lock()` path.
/// Hot-path stdout buffer capacity. Codegen inlines a buffer
/// length check against this value, so it must stay in sync
/// with `GOS_RT_STDOUT_BYTES`'s length declared in
/// `gossamer-codegen-llvm::emit` (see the `@GOS_RT_STDOUT_BYTES`
/// extern there) and with the inline `icmp ... 8192` checks
/// emitted by `lower::Lowerer`.
///
/// Sized for the line-buffered shape of `println!` / `print!`:
/// 8 KiB holds ~100 lines of typical output between flushes.
/// Programs that emit one giant block per flush (rare in practice)
/// take additional spills through `gos_rt_flush_stdout` — still
/// correct, just more syscalls. The previous 64 KiB cost 56 KiB
/// of BSS in every Gossamer binary for what is almost always wasted
/// slack.
pub const STDOUT_BUF_SIZE: usize = 8 * 1024;

/// Process-global mutex protecting [`GOS_RT_STDOUT_BYTES`] and
/// [`GOS_RT_STDOUT_LEN`]. Held for the duration of any inline
/// byte-write region (codegen-emitted) or any
/// `gos_rt_print_*` / `gos_rt_println` runtime helper. The
/// underlying lock is non-recursive; reentrant nesting on the
/// same OS thread routes through the per-thread depth counter
/// below so `gos_rt_println("foo")` (which acquires inside the
/// helper) can be called from inside an inline write region
/// (which already acquired) without deadlocking.
static STDOUT_LOCK: parking_lot::RawMutex = {
    use parking_lot::lock_api::RawMutex;
    parking_lot::RawMutex::INIT
};

thread_local! {
    /// Reentrancy counter for [`STDOUT_LOCK`] on the current
    /// thread. Bumped on each `acquire`, dropped on each
    /// `release`. The mutex is taken on the 0→1 transition and
    /// released on the 1→0 transition; intermediate transitions
    /// are no-ops at the lock layer. This makes
    /// `gos_rt_stdout_acquire` / `_release` recursion-safe even
    /// though `parking_lot::RawMutex` itself is not.
    static STDOUT_LOCK_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

/// Internal entry point: increments the per-thread reentrancy
/// counter, taking the mutex on the outermost acquire. Called by
/// every code path that touches the stdout buffer.
fn stdout_lock_acquire() {
    STDOUT_LOCK_DEPTH.with(|depth| {
        let n = depth.get();
        if n == 0 {
            use parking_lot::lock_api::RawMutex;
            STDOUT_LOCK.lock();
        }
        depth.set(n + 1);
    });
}

/// Internal entry point: decrements the per-thread reentrancy
/// counter, releasing the mutex when the counter returns to zero.
/// Calling this without a matching `stdout_lock_acquire` is a
/// programming error; debug builds assert.
fn stdout_lock_release() {
    STDOUT_LOCK_DEPTH.with(|depth| {
        let n = depth.get();
        debug_assert!(n > 0, "stdout_lock_release without acquire");
        if n == 1 {
            use parking_lot::lock_api::RawMutex;
            // SAFETY: invariant — `stdout_lock_acquire` ran on
            // the same thread when `n` was 0, taking the lock.
            unsafe { STDOUT_LOCK.unlock() };
        }
        depth.set(n.saturating_sub(1));
    });
}

/// Process-global stdout buffer storage. The LLVM backend
/// emits inline fast-path code that loads
/// `GOS_RT_STDOUT_LEN`, stores the new byte at offset
/// `bytes[len]`, and bumps the length — bypassing the FFI
/// call and saving the per-call overhead that dominates
/// character-at-a-time output (fasta hot loop). Access from any
/// thread requires the `STDOUT_LOCK` mutex be held.
#[unsafe(no_mangle)]
pub static mut GOS_RT_STDOUT_BYTES: [u8; STDOUT_BUF_SIZE] = [0; STDOUT_BUF_SIZE];

/// Current write offset in `GOS_RT_STDOUT_BYTES`. The inline
/// fast path reads this, stores the byte, and writes it back.
/// Access from any thread requires the `STDOUT_LOCK` mutex be
/// held.
#[unsafe(no_mangle)]
pub static mut GOS_RT_STDOUT_LEN: usize = 0;

/// Acquires the process-wide stdout buffer lock. Codegen wraps
/// every inline byte-write region in matched
/// [`gos_rt_stdout_acquire`] / [`gos_rt_stdout_release`] calls so
/// concurrent goroutines on different OS threads serialise their
/// writes against the buffer. Re-entry on the same thread is
/// supported via the per-thread `STDOUT_LOCK_DEPTH` counter so
/// the runtime FFI helpers (`gos_rt_print_*`, `gos_rt_println`,
/// `gos_rt_flush_stdout`) remain safe to call from inside an
/// outer acquire.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stdout_acquire() {
    stdout_lock_acquire();
}

/// Releases the process-wide stdout buffer lock acquired by a
/// matching [`gos_rt_stdout_acquire`]. Calling this without a
/// prior acquire is a programming error; the codegen always
/// emits matched pairs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_stdout_release() {
    stdout_lock_release();
}

/// Convenience RAII guard: acquires `STDOUT_LOCK` for the duration
/// of the current scope. Reentrant via the per-thread depth
/// counter so a runtime helper that holds a guard can call
/// another runtime helper that also acquires.
struct StdoutGuard;

impl StdoutGuard {
    fn acquire() -> Self {
        stdout_lock_acquire();
        Self
    }
}

impl Drop for StdoutGuard {
    fn drop(&mut self) {
        stdout_lock_release();
    }
}

fn raw_write_stdout(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    use std::io::Write;
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    let _ = handle.write_all(bytes);
}

/// Inner mechanic shared by `write_stdout` and any internal
/// caller that already holds `STDOUT_LOCK`. Splitting the lock
/// acquisition from the buffer manipulation lets us avoid
/// re-entering the (non-recursive) `RawMutex` from helpers that
/// already entered through the safe guard.
unsafe fn write_stdout_locked(bytes: &[u8]) {
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
        raw_write_stdout(bytes);
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

unsafe fn write_stdout(bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    let _guard = StdoutGuard::acquire();
    unsafe { write_stdout_locked(bytes) };
}

/// Flushes the process-global stdout buffer. Called on every
/// `println`-family intrinsic and on process exit via
/// `gos_rt_flush_stdout`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flush_stdout() {
    let _guard = StdoutGuard::acquire();
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

/// Prints an unsigned 64-bit integer through the buffered
/// stdout path. Distinct from `gos_rt_print_i64` so values
/// `>= 2^63` print without a leading `-` (the sign-extension
/// bug a single shared printer would have).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_print_u64(n: u64) {
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

/// Direct stderr writer used by `eprint`/`eprintln` lowering.
/// Bypasses the stdout buffer. Flushes stdout first so prior
/// `println` output isn't reordered with diagnostic output —
/// matches the language semantics where stderr appears in the
/// expected place relative to stdout.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_eprint_str(s: *const c_char) {
    let bytes = if s.is_null() {
        b"" as &[u8]
    } else {
        unsafe { CStr::from_ptr(s).to_bytes() }
    };
    unsafe { gos_rt_flush_stdout() };
    use std::io::Write;
    let stderr = std::io::stderr();
    let _ = stderr.lock().write_all(bytes);
}

/// `eprint_str` followed by a newline. Mirrors `gos_rt_println`
/// for the stderr path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_eprintln() {
    use std::io::Write;
    let stderr = std::io::stderr();
    let _ = stderr.lock().write_all(b"\n");
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
        raw_write_fd(fd, bytes);
    }
}

fn raw_write_fd(fd: i32, bytes: &[u8]) {
    if bytes.is_empty() {
        return;
    }
    use std::io::Write;
    // Today the runtime only routes fds 1 and 2; fd 0 is read-only.
    // Other fds will land here once `open()` is wired — at that
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
    let fd = unsafe { stream_fd(stream) };
    if fd == 1 {
        let _guard = StdoutGuard::acquire();
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
    raw_write_fd(fd, &byte);
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
        let guard = StdoutGuard::acquire();
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
        // Slow path: block doesn't fit. Flush and either pack
        // an oversized payload directly, or recurse so the
        // first arm fires with an empty buffer. The recursion
        // case has to drop the guard first — `STDOUT_LOCK` is
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
pub unsafe extern "C" fn gos_rt_println() {
    unsafe { write_stdout(b"\n") };
    // Line-flush so interactive output appears promptly.
    // Batched programs (fasta et al.) fill the buffer and flush
    // in 64 KiB chunks, which is dramatically cheaper than per-
    // write syscalls.
    let _guard = StdoutGuard::acquire();
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

/// Builds a fresh `*mut GosVec` from a stack/heap array. Copies
/// `len * elem_bytes` bytes from `data` into a freshly-allocated
/// data buffer; `Box::into_raw`s the resulting GosVec header.
///
/// Used at the binding-call boundary to convert a Gossamer
/// `[T; N]` array literal (or similarly-shaped value) into the
/// `*mut GosVec` shape the binding's C-ABI thunk expects for a
/// `Vec<T>` parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_from_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    let len = len.max(0);
    let n = (len as usize) * (elem_bytes as usize);
    let buf_ptr = if n == 0 || data.is_null() {
        std::ptr::null_mut()
    } else {
        let mut buf: Vec<u8> = vec![0u8; n];
        unsafe {
            std::ptr::copy_nonoverlapping(data, buf.as_mut_ptr(), n);
        }
        let p = buf.as_mut_ptr();
        std::mem::forget(buf);
        p
    };
    Box::into_raw(Box::new(GosVec {
        len,
        cap: len,
        elem_bytes,
        ptr: buf_ptr,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_len(v: *const GosVec) -> i64 {
    if v.is_null() {
        return 0;
    }
    unsafe { (*v).len }
}

/// Typed-i64 wrapper around [`gos_rt_vec_push`]. Spills the value
/// to a stack slot and forwards its address so the byte-erased
/// push helper can `memcpy` it into the vec's storage. Used by the
/// dynamic-count `[value; n]` lowering — passing an i64 directly
/// to the byte-erased helper would otherwise need a per-call-site
/// stack slot in cranelift.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_i64(v: *mut GosVec, value: i64) {
    let bytes = value.to_ne_bytes();
    unsafe { gos_rt_vec_push(v, bytes.as_ptr()) };
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

// ---------------------------------------------------------------
// Tagged-union encoding for `Result<T, E>` and `Option<T>`. The
// previous "happy-path" encoding stored just the payload value
// in the Result slot — meaning `Err(_)` and `None` had no
// distinguishing bit at runtime, so `match res { Ok(v) => …,
// Err(e) => … }` always took the Ok arm. A 2-slot heap struct
// (`disc`, `payload`) makes the Err / None case representable
// and lets pattern dispatch read the real discriminant.
//
// Convention: `disc == 0` = Ok / Some, `disc == 1` = Err / None.
// ---------------------------------------------------------------

#[repr(C)]
pub struct GosResult {
    pub disc: i64,
    pub payload: i64,
}

unsafe impl Send for GosResult {}
unsafe impl Sync for GosResult {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_new(disc: i64, payload: i64) -> *mut GosResult {
    // Arena-allocate so per-request results don't leak. The web
    // server's bench loop produces 100k+ Result handles per
    // second — Box::into_raw + leak grows RSS without bound.
    // `gos_rt_gc_reset` at the end of each request reclaims the
    // arena bytes; the GosHttpResponse referenced by `payload`
    // is still Box-allocated and freed via `drop_handler_result`.
    // The arena now 8-byte aligns allocations of size >= 8 bytes
    // (see `gos_rt_gc_alloc`), so casting the returned `*mut u8`
    // to `*mut GosResult` (16 B, align 8) is safe. The clippy
    // alignment lint can't see the runtime invariant from the
    // bare cast site.
    let p = unsafe { gos_rt_gc_alloc(std::mem::size_of::<GosResult>() as u64) }.cast::<GosResult>();
    if p.is_null() {
        return Box::into_raw(Box::new(GosResult { disc, payload }));
    }
    unsafe {
        std::ptr::write(p, GosResult { disc, payload });
    }
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_disc(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 1;
    }
    unsafe { (*p).disc }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_dbg(p: i64) -> i64 {
    eprintln!("[rt] dbg called with raw i64 = {p:#x}");
    p
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_payload(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 0;
    }
    unsafe { (*p).payload }
}

/// `result.unwrap()` / `option.unwrap()`. Returns the wrapped
/// payload on the happy path; panics on Err / None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_unwrap(p: *const GosResult) -> i64 {
    if p.is_null() {
        let cs = std::ffi::CString::new("called `Result::unwrap()` on an `Err` value").unwrap();
        unsafe { gos_rt_panic(cs.as_ptr()) };
        return 0;
    }
    let r = unsafe { &*p };
    if r.disc != 0 {
        let cs = std::ffi::CString::new("called `Result::unwrap()` on an `Err` value").unwrap();
        unsafe { gos_rt_panic(cs.as_ptr()) };
        return 0;
    }
    r.payload
}

/// `result.unwrap_or(default)` / `option.unwrap_or(default)`.
/// Returns the payload on the happy path, else the supplied
/// default. Both inputs flow through as raw 64-bit slots so the
/// helper works for any inner type that fits in a single word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_unwrap_or(p: *const GosResult, default: i64) -> i64 {
    if p.is_null() {
        return default;
    }
    let r = unsafe { &*p };
    if r.disc == 0 { r.payload } else { default }
}

/// `result.ok()` / `option.ok()`. Returns the payload on Ok/Some,
/// else 0. Mirrors the conventional "missing returns the zero
/// value of the wrapped type" semantics used elsewhere in the
/// compiled tier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_ok(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 0;
    }
    let r = unsafe { &*p };
    if r.disc == 0 { r.payload } else { 0 }
}

/// `result.err()`. Returns the error payload on Err, else 0.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_err(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 0;
    }
    let r = unsafe { &*p };
    if r.disc == 1 { r.payload } else { 0 }
}

/// `result.is_ok()` / `option.is_some()`. Returns 1 on Ok/Some,
/// 0 on Err/None or null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_is_ok(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 0;
    }
    let r = unsafe { &*p };
    i64::from(r.disc == 0)
}

/// `result.is_err()` / `option.is_none()`. Returns 1 on Err/None
/// or null, 0 on Ok/Some.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_is_err(p: *const GosResult) -> i64 {
    if p.is_null() {
        return 1;
    }
    let r = unsafe { &*p };
    i64::from(r.disc != 0)
}

/// Maps a `gos_main` return value to a process exit code.
/// Treats a heap-shaped pointer as a `*mut GosResult` and reads
/// its `disc`; falls back to the raw value (truncated) for
/// non-pointer returns. Also blocks until every outstanding
/// goroutine has settled so their stdout reaches the user
/// before the process exits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_main_exit_code(raw: i64) -> i32 {
    // Wait for every goroutine spawned via `go expr` to finish.
    // Without this the M:N pool keeps workers alive on a Condvar
    // while the main thread races straight to `_exit`, dropping
    // unflushed stdout and any worker output that hadn't yet
    // reached the underlying file descriptor.
    // Wait for outstanding goroutines so their stdout reaches
    // the user before the process exits. The M:N pool's worker
    // threads boot lazily on first `spawn`, so a fast main
    // (`go expr; return`) can race the worker start-up. Two
    // guards: (1) seed wait so the worker pool has time to
    // dequeue the first task, and (2) settle wait so a
    // task that just decremented `live` has time to actually
    // emit its stdout before the next sample.
    let sched = crate::sched_global::scheduler();
    let start = std::time::Instant::now();
    let deadline = start + std::time::Duration::from_secs(5);
    let mut consecutive_settled = 0_u32;
    let mut iters = 0_u64;
    while std::time::Instant::now() < deadline {
        let live = sched.live_goroutines();
        let stats = sched.stats();
        let settled =
            live == 0 && stats.spawned == stats.finished && start.elapsed().as_millis() >= 100;
        if settled {
            consecutive_settled += 1;
            if consecutive_settled >= 5 {
                break;
            }
        } else {
            consecutive_settled = 0;
        }
        iters += 1;
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let _ = iters;
    // Flush any buffered stdout that workers wrote so it
    // reaches the user before the process exits.
    unsafe { gos_rt_flush_stdout() };
    if raw == 0 {
        return 0;
    }
    let p = raw as usize;
    let looks_like_heap = p > 0x10000 && p.trailing_zeros() >= 3;
    if !looks_like_heap {
        return raw as i32;
    }
    let disc = unsafe { (*(raw as *const GosResult)).disc };
    disc as i32
}

// ---------------------------------------------------------------
// Sets — `HashSet<String>` (the most common shape) backed by
// `std::collections::HashSet<String>`. Stored on the heap; the
// pointer is the value seen by user code. Element type is
// erased at the FFI: only String keys are wired today, matching
// the common case in `examples/data_structures.gos`.
// ---------------------------------------------------------------

pub struct GosSet {
    inner: std::collections::HashSet<String>,
}

unsafe impl Send for GosSet {}
unsafe impl Sync for GosSet {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_new() -> *mut GosSet {
    Box::into_raw(Box::new(GosSet {
        inner: std::collections::HashSet::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_insert(s: *mut GosSet, key: *const c_char) -> bool {
    if s.is_null() || key.is_null() {
        return false;
    }
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let s = unsafe { &mut *s };
    s.inner.insert(k)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_contains(s: *const GosSet, key: *const c_char) -> bool {
    if s.is_null() || key.is_null() {
        return false;
    }
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let s = unsafe { &*s };
    s.inner.contains(&k)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_remove(s: *mut GosSet, key: *const c_char) -> bool {
    if s.is_null() || key.is_null() {
        return false;
    }
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let s = unsafe { &mut *s };
    s.inner.remove(&k)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_len(s: *const GosSet) -> i64 {
    if s.is_null() {
        return 0;
    }
    unsafe { (*s).inner.len() as i64 }
}

// ---------------------------------------------------------------
// BTreeMap — sorted-key map with String keys + i64 values.
// Mirrors the `gos_rt_map_*` shape but iterates in key order.
// ---------------------------------------------------------------

pub struct GosBtMap {
    inner: std::collections::BTreeMap<String, i64>,
}

unsafe impl Send for GosBtMap {}
unsafe impl Sync for GosBtMap {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_new() -> *mut GosBtMap {
    Box::into_raw(Box::new(GosBtMap {
        inner: std::collections::BTreeMap::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_insert(m: *mut GosBtMap, key: *const c_char, value: i64) {
    if m.is_null() || key.is_null() {
        return;
    }
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let m = unsafe { &mut *m };
    m.inner.insert(k, value);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_get_or(
    m: *const GosBtMap,
    key: *const c_char,
    def: i64,
) -> i64 {
    if m.is_null() || key.is_null() {
        return def;
    }
    let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
    let m = unsafe { &*m };
    m.inner.get(&k).copied().unwrap_or(def)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_len(m: *const GosBtMap) -> i64 {
    if m.is_null() {
        return 0;
    }
    unsafe { (*m).inner.len() as i64 }
}

/// Returns a fresh `*mut GosVec` of the BTreeMap's keys (in sort
/// order, since BTreeMap iterates ordered). Used by the
/// `for (k, v) in m.iter()` lowering — the codegen iterates the
/// keys vec by index and re-fetches the value via
/// `gos_rt_btmap_get_or` so each binding gets a real value, not
/// the ranked Vec header garbage the previous (missing) iter
/// dispatch printed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_keys(m: *const GosBtMap) -> *mut GosVec {
    let v = unsafe { gos_rt_vec_new(8) };
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
}

/// Renders an i64-elem `Vec` as `[v0, v1, …]`. Returns a fresh
/// String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_i64(v: *const GosVec) -> *mut c_char {
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
}

/// Renders an `f64`-elem `Vec` as `[v0, v1, …]`. Returns a fresh
/// String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_f64(v: *const GosVec) -> *mut c_char {
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
}

/// Renders a `bool`-elem `Vec` as `[true, false, …]`. Returns a
/// fresh String pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_bool(v: *const GosVec) -> *mut c_char {
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
}

/// Renders a `String`-elem `Vec` as `[s0, s1, …]`. Each element
/// in the Vec is a NUL-terminated `*const c_char`; we read it as
/// an 8-byte word and dereference. Returns a fresh String
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_string(v: *const GosVec) -> *mut c_char {
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
        let s_ptr = unsafe { (p as *const *const c_char).read_unaligned() };
        if s_ptr.is_null() {
            out.push_str("\"\"");
        } else {
            let cs = unsafe { std::ffi::CStr::from_ptr(s_ptr) };
            let payload = cs.to_string_lossy();
            out.push('"');
            out.push_str(&payload);
            out.push('"');
        }
    }
    out.push(']');
    alloc_cstring(out.as_bytes())
}

/// Renders a `Vec<Vec<i64>>` as `[[a, b], [c], …]`. Each
/// element is a `*mut GosVec` (8-byte slot); we recursively
/// stringify each inner `Vec<i64>`. Returns a fresh String
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_format_vec_i64(v: *const GosVec) -> *mut c_char {
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
        let inner_ptr = unsafe { (p as *const *const GosVec).read_unaligned() };
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
}

/// Reads an `i64`-shaped element from a `Vec` (or any
/// 8-byte-elem `GosVec`) by index. Returns `0` when the receiver
/// is null or `idx` is out of range. Used by the MIR-side Vec
/// indexing path so `xs[0]` reads the data buffer instead of the
/// `GosVec` header's `len` field.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_get_i64(v: *const GosVec, idx: i64) -> i64 {
    if v.is_null() {
        return 0;
    }
    let vec = unsafe { &*v };
    if idx < 0 || idx >= vec.len {
        return 0;
    }
    let p = unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) };
    unsafe { (p as *const i64).read_unaligned() }
}

/// Writes an `i64`-shaped element to a `Vec` at `idx`. No-op for
/// null receivers or out-of-range indices (so a stale `xs[i] = v`
/// after a shrink doesn't trash unrelated memory).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_set_i64(v: *mut GosVec, idx: i64, value: i64) {
    if v.is_null() {
        return;
    }
    let vec = unsafe { &mut *v };
    if idx < 0 || idx >= vec.len {
        return;
    }
    let p = unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) };
    unsafe { p.cast::<i64>().write_unaligned(value) };
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
/// `vec[lo..hi]` — copies the subrange `[lo, hi)` of `v`'s
/// elements into a fresh `GosVec` and returns a pointer to it.
/// Out-of-range bounds are clamped. Element bytes are copied
/// directly (the i64-erased ABI matches the rest of the Vec
/// surface).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_slice(v: *const GosVec, lo: i64, hi: i64) -> *mut GosVec {
    if v.is_null() {
        return unsafe { gos_rt_vec_new(8) };
    }
    let src = unsafe { &*v };
    let elem_bytes = src.elem_bytes;
    let len = src.len;
    let lo = lo.max(0).min(len);
    let hi = hi.max(lo).min(len);
    let count = hi - lo;
    let out = unsafe { gos_rt_vec_with_capacity(elem_bytes, count) };
    if !out.is_null() && count > 0 {
        for i in 0..count {
            unsafe {
                let src_ptr = src.ptr.add(((lo + i) as usize) * (elem_bytes as usize));
                gos_rt_vec_push(out, src_ptr);
            }
        }
    }
    out
}

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
// HashMap runtime — typed-storage variants over rustc-hash's
// FxHashMap. Auto-promotes Empty → I64I64 / StrI64 / StrStr /
// Bytes on first typed call. The i64-keyed/i64-valued shape
// (counter / scoreboard hot paths) avoids per-op `Vec<u8>`
// allocation and uses FxHash directly on the
// 8-byte key.
// ---------------------------------------------------------------

use rustc_hash::FxHashMap;

/// Layout-sensitive: the first 8 bytes hold the current element
/// count so the generic `gos_rt_arr_len` returns the right value
/// without needing a HashMap-specific dispatch.
#[repr(C)]
pub struct GosMap {
    len_cache: i64,
    storage: parking_lot::Mutex<MapStorage>,
}

enum MapStorage {
    Empty,
    I64I64(FxHashMap<i64, i64>),
    /// String-keyed maps store keys as `Box<[u8]>` (16 B header)
    /// rather than `Vec<u8>` (24 B header) — for the k-mer-counter
    /// hot shape (HashMap<String, i64> with millions of short
    /// keys), the saved 8 B per entry compounds visibly: ~8 MB
    /// off a 1 M-entry table. Same applies to `StrStr` keys and
    /// the `Bytes` byte-erased fallback.
    StrI64(FxHashMap<Box<[u8]>, i64>),
    StrStr(FxHashMap<Box<[u8]>, Box<[u8]>>),
    I64Str(FxHashMap<i64, Box<[u8]>>),
    Bytes(FxHashMap<Box<[u8]>, Box<[u8]>>),
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new(_key_bytes: u32, _val_bytes: u32) -> *mut GosMap {
    Box::into_raw(Box::new(GosMap {
        len_cache: 0,
        storage: parking_lot::Mutex::new(MapStorage::Empty),
    }))
}

/// Pre-sized constructor: avoids the doubling chain (~22 reallocs
/// for ~5M inserts) when the caller has an upper bound. Picks the
/// initial typed shape from the byte sizes — both 8 → I64I64,
/// otherwise the byte-erased generic shape that promotes lazily.
/// Pre-sizing avoids the doubling chain on counter-style hot
/// loops where the caller knows the total entry count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_new_with_capacity(
    key_bytes: u32,
    val_bytes: u32,
    cap: i64,
) -> *mut GosMap {
    let cap = if cap < 0 { 0 } else { cap as usize };
    let storage = if key_bytes == 8 && val_bytes == 8 {
        MapStorage::I64I64(FxHashMap::with_capacity_and_hasher(
            cap,
            rustc_hash::FxBuildHasher,
        ))
    } else {
        MapStorage::Empty
    };
    Box::into_raw(Box::new(GosMap {
        len_cache: 0,
        storage: parking_lot::Mutex::new(storage),
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
    let k = unsafe { std::slice::from_raw_parts(key, 8) }.to_vec();
    let v = unsafe { std::slice::from_raw_parts(val, 8) }.to_vec();
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::Bytes(FxHashMap::default());
    }
    let MapStorage::Bytes(inner) = &mut *storage else {
        return;
    };
    if inner
        .insert(k.into_boxed_slice(), v.into_boxed_slice())
        .is_none()
    {
        map.len_cache += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get(m: *const GosMap, key: *const u8, val_out: *mut u8) -> i32 {
    if m.is_null() || key.is_null() || val_out.is_null() {
        return 0;
    }
    let map = unsafe { &*m };
    let k = unsafe { std::slice::from_raw_parts(key, 8) };
    let storage = map.storage.lock();
    let MapStorage::Bytes(inner) = &*storage else {
        return 0;
    };
    if let Some(v) = inner.get(k) {
        unsafe {
            std::ptr::copy_nonoverlapping(v.as_ptr(), val_out, v.len());
        }
        1
    } else {
        0
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_i64(m: *const GosMap, key: i64, default: i64) -> i64 {
    if m.is_null() {
        return default;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::I64I64(inner) => inner.get(&key).copied().unwrap_or(default),
        _ => default,
    }
}

/// `get_or` for string-keyed, i64-valued maps. Mirrors
/// `gos_rt_map_get_or_i64` but hashes the key via the same UTF-8
/// byte slice the `_str_i64` insert path uses, so an `insert(k, v)`
/// followed by `get_or(k, d)` round-trips.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_str_i64(
    m: *const GosMap,
    key: *const c_char,
    default: i64,
) -> i64 {
    if m.is_null() || key.is_null() {
        return default;
    }
    let map = unsafe { &*m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::StrI64(inner) => inner.get(key_bytes).copied().unwrap_or(default),
        _ => default,
    }
}

/// `get_or` for string-keyed, string-valued maps. Returns a fresh
/// GC-allocated `*mut c_char` for the stored value, or a copy of
/// `default`'s bytes when the key is absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_str_str(
    m: *const GosMap,
    key: *const c_char,
    default: *const c_char,
) -> *mut c_char {
    let default_bytes: &[u8] = if default.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(default) }.to_bytes()
    };
    if m.is_null() || key.is_null() {
        return alloc_cstring(default_bytes);
    }
    let map = unsafe { &*m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let storage = map.storage.lock();
    let MapStorage::StrStr(inner) = &*storage else {
        return alloc_cstring(default_bytes);
    };
    match inner.get(key_bytes) {
        Some(v) => alloc_cstring(v),
        None => alloc_cstring(default_bytes),
    }
}

/// `get_or` for i64-keyed, string-valued maps.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_or_i64_str(
    m: *const GosMap,
    key: i64,
    default: *const c_char,
) -> *mut c_char {
    let default_bytes: &[u8] = if default.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(default) }.to_bytes()
    };
    if m.is_null() {
        return alloc_cstring(default_bytes);
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    let MapStorage::I64Str(inner) = &*storage else {
        return alloc_cstring(default_bytes);
    };
    match inner.get(&key) {
        Some(v) => alloc_cstring(v),
        None => alloc_cstring(default_bytes),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_i64_i64(m: *mut GosMap, key: i64, val: i64) {
    if m.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::I64I64(FxHashMap::default());
    }
    let MapStorage::I64I64(inner) = &mut *storage else {
        return;
    };
    if inner.insert(key, val).is_none() {
        map.len_cache += 1;
    }
}

/// Fused increment: `m[k] = m.get_or(k, 0) + by`. Single lock,
/// single hash, single bucket walk. Replaces the
/// `m.insert(k, m.get_or(k, 0) + 1)` pattern that costs 2× the
/// hash work on hot counter loops.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_i64(m: *mut GosMap, key: i64, by: i64) -> i64 {
    if m.is_null() {
        return 0;
    }
    let map = unsafe { &mut *m };
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::I64I64(FxHashMap::default());
    }
    let MapStorage::I64I64(inner) = &mut *storage else {
        return 0;
    };
    let entry = inner.entry(key).or_insert_with(|| {
        map.len_cache += 1;
        0
    });
    *entry += by;
    *entry
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_i64(m: *const GosMap, key: i64) -> i64 {
    if m.is_null() {
        return 0;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::I64I64(inner) => inner.get(&key).copied().unwrap_or(0),
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_contains_key_i64(m: *const GosMap, key: i64) -> bool {
    if m.is_null() {
        return false;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::I64I64(inner) => inner.contains_key(&key),
        MapStorage::I64Str(inner) => inner.contains_key(&key),
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove_i64(m: *mut GosMap, key: i64) -> bool {
    if m.is_null() {
        return false;
    }
    let map = unsafe { &mut *m };
    let mut storage = map.storage.lock();
    let removed = match &mut *storage {
        MapStorage::I64I64(inner) => inner.remove(&key).is_some(),
        MapStorage::I64Str(inner) => inner.remove(&key).is_some(),
        _ => false,
    };
    if removed {
        map.len_cache -= 1;
    }
    removed
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_str_i64(m: *mut GosMap, key: *const c_char, val: i64) {
    if m.is_null() || key.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes().to_vec();
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::StrI64(FxHashMap::default());
    }
    let MapStorage::StrI64(inner) = &mut *storage else {
        return;
    };
    if inner.insert(key_bytes.into_boxed_slice(), val).is_none() {
        map.len_cache += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_str_i64(m: *const GosMap, key: *const c_char) -> i64 {
    if m.is_null() || key.is_null() {
        return 0;
    }
    let map = unsafe { &*m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::StrI64(inner) => inner.get(key_bytes).copied().unwrap_or(0),
        _ => 0,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_str_str(
    m: *mut GosMap,
    key: *const c_char,
    val: *const c_char,
) {
    if m.is_null() || key.is_null() || val.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes().to_vec();
    let val_bytes = unsafe { CStr::from_ptr(val) }.to_bytes().to_vec();
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::StrStr(FxHashMap::default());
    }
    let MapStorage::StrStr(inner) = &mut *storage else {
        return;
    };
    if inner
        .insert(key_bytes.into_boxed_slice(), val_bytes.into_boxed_slice())
        .is_none()
    {
        map.len_cache += 1;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_str_str(
    m: *const GosMap,
    key: *const c_char,
) -> *mut c_char {
    if m.is_null() || key.is_null() {
        return empty_cstring();
    }
    let map = unsafe { &*m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let storage = map.storage.lock();
    let MapStorage::StrStr(inner) = &*storage else {
        return empty_cstring();
    };
    match inner.get(key_bytes) {
        Some(v) => {
            let mut buf: Vec<u8> = v.to_vec();
            buf.push(0);
            let boxed = buf.into_boxed_slice();
            Box::leak(boxed).as_mut_ptr().cast::<c_char>()
        }
        None => empty_cstring(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_contains_key_str(m: *const GosMap, key: *const c_char) -> bool {
    if m.is_null() || key.is_null() {
        return false;
    }
    let map = unsafe { &*m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::StrI64(inner) => inner.contains_key(key_bytes),
        MapStorage::StrStr(inner) => inner.contains_key(key_bytes),
        _ => false,
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove_str(m: *mut GosMap, key: *const c_char) -> bool {
    if m.is_null() || key.is_null() {
        return false;
    }
    let map = unsafe { &mut *m };
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let mut storage = map.storage.lock();
    let removed = match &mut *storage {
        MapStorage::StrI64(inner) => inner.remove(key_bytes).is_some(),
        MapStorage::StrStr(inner) => inner.remove(key_bytes).is_some(),
        _ => false,
    };
    if removed {
        map.len_cache -= 1;
    }
    removed
}

/// `m.inc_at(seq, start, len, by)` for `HashMap<String, i64>` —
/// the zero-allocation analogue of
/// `m.insert(k, m.get_or(k, 0) + by)` where `k = seq[start..start+len]`.
///
/// Mirrors `*m.entry(&seq[i..i+k]).or_insert(0) += by`: the
/// slice is borrowed (zero-copy), the hash table is consulted
/// exactly once, and a `Vec<u8>` is allocated only on the first
/// occurrence of each unique key. Halves the hash work per
/// iteration vs the get_or + insert pair, and avoids any
/// per-iteration scratch allocation for the key.
///
/// Returns the new value at `seq[start..start+len]` (or `by` if
/// the entry is fresh).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_at_str_i64(
    m: *mut GosMap,
    seq: *const c_char,
    start: i64,
    len: i64,
    by: i64,
) -> i64 {
    if m.is_null() || seq.is_null() || len <= 0 || start < 0 {
        return 0;
    }
    let map = unsafe { &mut *m };
    let key_slice: &[u8] =
        unsafe { std::slice::from_raw_parts(seq.cast::<u8>().add(start as usize), len as usize) };
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::StrI64(FxHashMap::default());
    }
    let MapStorage::StrI64(inner) = &mut *storage else {
        return 0;
    };
    // Lookup is by `&[u8]` — `Vec<u8>: Borrow<[u8]>` lets the
    // hashbrown table hash the slice without first allocating an
    // owned key. Only the first occurrence of each unique k-mer
    // pays the `to_vec()` cost.
    if let Some(v) = inner.get_mut(key_slice) {
        *v += by;
        return *v;
    }
    inner.insert(key_slice.to_vec().into_boxed_slice(), by);
    map.len_cache += 1;
    by
}

/// `m.inc(key, by)` for `HashMap<String, i64>` — adds `by`
/// (default 1 in user code) to the value at `key`, inserting
/// the entry if absent. Halves the lock + hash work compared to
/// `m.insert(k, m.get_or(k, 0) + by)` and avoids the
/// double-borrow that pattern triggers in compiled mode.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_inc_str_i64(
    m: *mut GosMap,
    key: *const c_char,
    by: i64,
) -> i64 {
    if m.is_null() || key.is_null() {
        return 0;
    }
    let key_bytes = unsafe { CStr::from_ptr(key) }.to_bytes();
    let map = unsafe { &mut *m };
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::StrI64(FxHashMap::default());
    }
    let MapStorage::StrI64(inner) = &mut *storage else {
        return 0;
    };
    if let Some(v) = inner.get_mut(key_bytes) {
        *v += by;
        return *v;
    }
    inner.insert(key_bytes.to_vec().into_boxed_slice(), by);
    map.len_cache += 1;
    by
}

/// `m.insert(k: i64, v: String)` — `HashMap<i64, String>` insert.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_insert_i64_str(m: *mut GosMap, key: i64, val: *const c_char) {
    if m.is_null() || val.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let val_bytes = unsafe { CStr::from_ptr(val) }.to_bytes().to_vec();
    let mut storage = map.storage.lock();
    if matches!(*storage, MapStorage::Empty) {
        *storage = MapStorage::I64Str(FxHashMap::default());
    }
    let MapStorage::I64Str(inner) = &mut *storage else {
        return;
    };
    if inner.insert(key, val_bytes.into_boxed_slice()).is_none() {
        map.len_cache += 1;
    }
}

/// `m.get(k: i64) -> String` — returns an empty string when absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_get_i64_str(m: *const GosMap, key: i64) -> *mut c_char {
    if m.is_null() {
        return empty_cstring();
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    let MapStorage::I64Str(inner) = &*storage else {
        return empty_cstring();
    };
    match inner.get(&key) {
        Some(v) => alloc_cstring(v),
        None => empty_cstring(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_clear(m: *mut GosMap) {
    if m.is_null() {
        return;
    }
    let map = unsafe { &mut *m };
    let mut storage = map.storage.lock();
    *storage = MapStorage::Empty;
    map.len_cache = 0;
}

/// Drops a `HashMap` allocated by [`gos_rt_map_new`] /
/// [`gos_rt_map_new_with_capacity`]. The MIR's drop-insertion pass
/// emits a call to this at every function return for any local
/// that owns a freshly-constructed map and isn't moved into the
/// return slot. Idempotent on null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_free(m: *mut GosMap) {
    if m.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(m) });
}

/// Drops a `Vec` allocated by [`gos_rt_vec_new`] /
/// [`gos_rt_vec_with_capacity`]. Frees both the `GosVec` header
/// and its backing element buffer. Idempotent on null.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_free(v: *mut GosVec) {
    if v.is_null() {
        return;
    }
    let boxed = unsafe { Box::from_raw(v) };
    if !boxed.ptr.is_null() && boxed.cap > 0 {
        let bytes = (boxed.cap as usize) * (boxed.elem_bytes as usize);
        unsafe {
            let _ = Vec::from_raw_parts(boxed.ptr, bytes, bytes);
        }
    }
    drop(boxed);
}

/// Drops a `HashSet` allocated by [`gos_rt_set_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_free(s: *mut GosSet) {
    if s.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(s) });
}

/// Drops a `BTreeMap` allocated by [`gos_rt_btmap_new`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_btmap_free(m: *mut GosBtMap) {
    if m.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(m) });
}

/// Snapshots the i64 keys of an i64-keyed `HashMap` into a fresh
/// `GosVec<i64>` for the for-loop lowerer to drive with the
/// regular `gos_rt_vec_*` helpers. Iteration order matches the
/// underlying `FxHashMap`'s order — undefined-but-stable per
/// process. Returns an empty vec for any other storage shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_keys_i64(m: *const GosMap) -> *mut GosVec {
    let out = unsafe { gos_rt_vec_new(8) };
    if m.is_null() {
        return out;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    let push_key = |k: &i64| {
        let bytes = k.to_ne_bytes();
        unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
    };
    match &*storage {
        MapStorage::I64I64(inner) => inner.keys().for_each(push_key),
        MapStorage::I64Str(inner) => inner.keys().for_each(push_key),
        _ => {}
    }
    out
}

/// Snapshots the i64 values of an i64-valued `HashMap` into a
/// fresh `GosVec<i64>`. Pairs with `gos_rt_map_keys_i64` for
/// `for v in m.values()` lowering. Empty vec for non-i64-valued
/// storage shapes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_i64(m: *const GosMap) -> *mut GosVec {
    let out = unsafe { gos_rt_vec_new(8) };
    if m.is_null() {
        return out;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    match &*storage {
        MapStorage::I64I64(inner) => {
            for v in inner.values() {
                let bytes = v.to_ne_bytes();
                unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
            }
        }
        MapStorage::StrI64(inner) => {
            for v in inner.values() {
                let bytes = v.to_ne_bytes();
                unsafe { gos_rt_vec_push(out, bytes.as_ptr()) };
            }
        }
        _ => {}
    }
    out
}

/// Snapshots the string keys of a string-keyed `HashMap` into a
/// fresh `GosVec<*mut c_char>`. Each key is freshly allocated in
/// the GC arena so the slot value is the same `*mut c_char`
/// representation Gossamer's `String` type uses elsewhere.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_keys_str(m: *const GosMap) -> *mut GosVec {
    let out = unsafe { gos_rt_vec_new(8) };
    if m.is_null() {
        return out;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    let push_key = |k: &[u8]| {
        let cstr = alloc_cstring(k);
        let slot = (cstr as usize as i64).to_ne_bytes();
        unsafe { gos_rt_vec_push(out, slot.as_ptr()) };
    };
    match &*storage {
        MapStorage::StrI64(inner) => {
            for k in inner.keys() {
                push_key(k);
            }
        }
        MapStorage::StrStr(inner) => {
            for k in inner.keys() {
                push_key(k);
            }
        }
        _ => {}
    }
    out
}

/// Snapshots the string values of a string-valued `HashMap` into
/// a fresh `GosVec<*mut c_char>`. Mirrors `gos_rt_map_keys_str`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_values_str(m: *const GosMap) -> *mut GosVec {
    let out = unsafe { gos_rt_vec_new(8) };
    if m.is_null() {
        return out;
    }
    let map = unsafe { &*m };
    let storage = map.storage.lock();
    let push_val = |v: &[u8]| {
        let cstr = alloc_cstring(v);
        let slot = (cstr as usize as i64).to_ne_bytes();
        unsafe { gos_rt_vec_push(out, slot.as_ptr()) };
    };
    match &*storage {
        MapStorage::StrStr(inner) => inner.values().for_each(|v| push_val(v)),
        MapStorage::I64Str(inner) => inner.values().for_each(|v| push_val(v)),
        _ => {}
    }
    out
}

fn empty_cstring() -> *mut c_char {
    let buf: Box<[u8]> = vec![0u8].into_boxed_slice();
    Box::leak(buf).as_mut_ptr().cast::<c_char>()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_map_remove(m: *mut GosMap, key: *const u8) -> i32 {
    if m.is_null() || key.is_null() {
        return 0;
    }
    let map = unsafe { &mut *m };
    let k = unsafe { std::slice::from_raw_parts(key, 8) };
    let mut storage = map.storage.lock();
    let removed = match &mut *storage {
        MapStorage::Bytes(inner) => inner.remove(k).is_some(),
        _ => false,
    };
    if removed {
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

/// Channel payload storage. The 8-byte specialisation matches the
/// most common shape (every i64-class scalar plus pointer-sized
/// values fit) and avoids the per-message `Vec<u8>` allocation that
/// the byte-erased path needs. The codegen always knows
/// `elem_bytes` at the `gos_rt_chan_new` site, so the dispatch is
/// a one-time check at construction.
enum ChanStorage {
    /// 8-byte inline payloads. A 1M-message run with cap=100
    /// holds at most 100 * 8 = 800 B of payload here, vs ~3.2 MB
    /// of `Vec<u8>` headers + 8 B allocations under `Bytes`.
    I64(VecDeque<i64>),
    /// Erased byte storage for any other element size.
    Bytes(VecDeque<Vec<u8>>),
}

pub struct GosChan {
    pub elem_bytes: u32,
    pub cap: i64, // 0 = unbounded
    pub closed: StdMutex<bool>,
    buf: StdMutex<ChanStorage>,
    pub not_empty: StdCondvar,
    pub not_full: StdCondvar,
    /// Gids of goroutines parked on a recv (channel was empty). The
    /// next sender pops one and unparks it. Empty when no
    /// goroutines are waiting, in which case the OS-thread
    /// `not_empty` Condvar is the only waker path.
    parked_recv: parking_lot::Mutex<std::collections::VecDeque<crate::sched::Gid>>,
    /// Gids of goroutines parked on a send (buffer full). The next
    /// receiver pops one and unparks it.
    parked_send: parking_lot::Mutex<std::collections::VecDeque<crate::sched::Gid>>,
    /// Goroutine id of the most recent sender. Read by recv to
    /// record a happens-before edge into the race detector. `-1`
    /// means "no sender yet observed".
    pub last_sender: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_new(elem_bytes: u32, cap: i64) -> *mut GosChan {
    let buf = if elem_bytes == 8 {
        ChanStorage::I64(VecDeque::new())
    } else {
        ChanStorage::Bytes(VecDeque::new())
    };
    Box::into_raw(Box::new(GosChan {
        elem_bytes,
        cap,
        closed: StdMutex::new(false),
        buf: StdMutex::new(buf),
        not_empty: StdCondvar::new(),
        not_full: StdCondvar::new(),
        parked_recv: parking_lot::Mutex::new(std::collections::VecDeque::new()),
        parked_send: parking_lot::Mutex::new(std::collections::VecDeque::new()),
        last_sender: AtomicI64::new(-1),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_send(c: *mut GosChan, val: *const u8) {
    if c.is_null() || val.is_null() {
        return;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    loop {
        let mut guard = chan.buf.lock().unwrap();
        if chan.cap <= 0 || (storage_len(&guard) as i64) < chan.cap {
            push_back(&mut guard, val, bytes_len);
            drop(guard);
            chan.last_sender
                .store(i64::from(crate::race::current_gid()), Ordering::Release);
            wake_one_recv(chan);
            return;
        }
        // Buffer full. Goroutines park; OS threads block.
        if gossamer_coro::in_goroutine() {
            drop(guard);
            crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                chan.parked_send.lock().push_back(parker.gid);
            });
            // Cleanup: remove our gid from parked_send if still
            // present (e.g. a parallel close fired with pre_unpark
            // before any matching receive).
            if let Some(gid) = crate::sched_global::current_gid() {
                chan.parked_send.lock().retain(|g| *g != gid);
            }
        } else {
            // Non-goroutine fallback: condvar-block the OS thread.
            // The lock guard is consumed by `wait` and re-acquired
            // on wakeup; we discard it explicitly via `drop` so
            // clippy doesn't flag a let-underscore-lock pattern.
            drop(chan.not_full.wait(guard).unwrap());
        }
    }
}

fn wake_one_recv(chan: &GosChan) {
    if let Some(gid) = chan.parked_recv.lock().pop_front() {
        crate::sched_global::scheduler().unpark(gid);
    }
    chan.not_empty.notify_one();
}

fn wake_one_send(chan: &GosChan) {
    if let Some(gid) = chan.parked_send.lock().pop_front() {
        crate::sched_global::scheduler().unpark(gid);
    }
    chan.not_full.notify_one();
}

fn wake_all(chan: &GosChan) {
    let recvs: Vec<_> = chan.parked_recv.lock().drain(..).collect();
    let sends: Vec<_> = chan.parked_send.lock().drain(..).collect();
    let sched = crate::sched_global::scheduler();
    for gid in recvs.into_iter().chain(sends) {
        sched.unpark(gid);
    }
    chan.not_empty.notify_all();
    chan.not_full.notify_all();
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_try_send(c: *mut GosChan, val: *const u8) -> i32 {
    if c.is_null() || val.is_null() {
        return 0;
    }
    let chan = unsafe { &*c };
    let bytes_len = chan.elem_bytes as usize;
    let mut guard = chan.buf.lock().unwrap();
    if chan.cap > 0 && storage_len(&guard) as i64 >= chan.cap {
        return 0;
    }
    push_back(&mut guard, val, bytes_len);
    drop(guard);
    chan.last_sender
        .store(i64::from(crate::race::current_gid()), Ordering::Release);
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
    loop {
        let mut guard = chan.buf.lock().unwrap();
        if pop_front(&mut guard, out, bytes_len) {
            drop(guard);
            record_chan_handoff(chan);
            wake_one_send(chan);
            return 1;
        }
        if *chan.closed.lock().unwrap() {
            return 0;
        }
        // Empty channel. Goroutines park; OS threads block.
        if gossamer_coro::in_goroutine() {
            drop(guard);
            crate::sched_global::park(crate::sched::ParkReason::Chan, |parker| {
                chan.parked_recv.lock().push_back(parker.gid);
            });
            if let Some(gid) = crate::sched_global::current_gid() {
                chan.parked_recv.lock().retain(|g| *g != gid);
            }
        } else {
            drop(chan.not_empty.wait(guard).unwrap());
        }
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
    if pop_front(&mut guard, out, bytes_len) {
        drop(guard);
        record_chan_handoff(chan);
        chan.not_full.notify_one();
        return 1;
    }
    0
}

fn storage_len(storage: &ChanStorage) -> usize {
    match storage {
        ChanStorage::I64(d) => d.len(),
        ChanStorage::Bytes(d) => d.len(),
    }
}

fn push_back(storage: &mut ChanStorage, val: *const u8, bytes_len: usize) {
    match storage {
        ChanStorage::I64(deque) => {
            // Read 8 bytes from `val` into an i64 in a way that
            // doesn't assume natural alignment of the source.
            let mut tmp = [0u8; 8];
            unsafe {
                std::ptr::copy_nonoverlapping(val, tmp.as_mut_ptr(), 8);
            }
            deque.push_back(i64::from_ne_bytes(tmp));
        }
        ChanStorage::Bytes(deque) => {
            let mut data = vec![0u8; bytes_len];
            unsafe {
                std::ptr::copy_nonoverlapping(val, data.as_mut_ptr(), bytes_len);
            }
            deque.push_back(data);
        }
    }
}

fn pop_front(storage: &mut ChanStorage, out: *mut u8, bytes_len: usize) -> bool {
    match storage {
        ChanStorage::I64(deque) => deque.pop_front().is_some_and(|n| {
            let bytes = n.to_ne_bytes();
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), out, 8);
            }
            true
        }),
        ChanStorage::Bytes(deque) => deque.pop_front().is_some_and(|item| {
            unsafe {
                std::ptr::copy_nonoverlapping(item.as_ptr(), out, bytes_len);
            }
            true
        }),
    }
}

/// Records the sender->receiver synchronisation edge into the race
/// detector. Called immediately after a successful recv. No-op
/// when the race detector is disabled.
fn record_chan_handoff(chan: &GosChan) {
    let from = chan.last_sender.load(Ordering::Acquire);
    if from < 0 {
        return;
    }
    let to = crate::race::current_gid();
    crate::race::record_sync(u32::try_from(from).unwrap_or(0), to);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_close(c: *mut GosChan) {
    if c.is_null() {
        return;
    }
    let chan = unsafe { &*c };
    *chan.closed.lock().unwrap() = true;
    wake_all(chan);
}

/// Drops a channel created with `gos_rt_chan_new`.
/// Closes the channel first so any thread parked on `not_empty` /
/// `not_full` wakes with `RecvResult::Closed` / `SendResult::Closed`
/// before the underlying storage is reclaimed. Calling this on a
/// channel that other threads are still using is a logic error;
/// the codegen emits the call at the channel's last live use.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_chan_drop(c: *mut GosChan) {
    if c.is_null() {
        return;
    }
    // Close + notify before reclamation so parked threads observe
    // the closed flag rather than racing the Box drop. The Drop
    // impl on `GosChan` repeats the close+notify, harmlessly,
    // because callers may also drop a `Box<GosChan>` directly in
    // tests without going through this entry point.
    unsafe {
        gos_rt_chan_close(c);
        drop(Box::from_raw(c));
    }
}

impl Drop for GosChan {
    fn drop(&mut self) {
        if let Ok(mut closed) = self.closed.lock() {
            *closed = true;
        }
        self.not_empty.notify_all();
        self.not_full.notify_all();
    }
}

// ---------------------------------------------------------------
// Scheduler — every `go fn(args)` lands on the M:N pool
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn(
    func: Option<unsafe extern "C" fn(*mut u8)>,
    env: *mut u8,
) {
    let Some(f) = func else { return };
    let env_addr = env as usize;
    spawn_task(Box::new(move || {
        let env = env_addr as *mut u8;
        unsafe { f(env) };
    }));
}

fn spawn_task(task: Box<dyn FnOnce() + Send + 'static>) {
    crate::sched_global::spawn(task);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_0(fn_addr: usize) {
    if fn_addr == 0 {
        return;
    }
    spawn_task(Box::new(move || {
        // SAFETY: the caller promises `fn_addr` is the address of
        // an `extern "C" fn() -> i64` — the SysV-ABI convention
        // native codegen emits for every Gossamer function.
        type Fn0 = unsafe extern "C" fn() -> i64;
        let f: Fn0 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f() };
    }));
}

/// Spawns a goroutine on the work-stealing scheduler (or, if no
/// scheduler is installed, an OS thread) that calls a one-argument
/// function with a single i64 payload. All Gossamer scalar types
/// fit in an i64 slot; floats are passed by bitcast.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_1(fn_addr: usize, arg0: i64) {
    if fn_addr == 0 {
        return;
    }
    spawn_task(Box::new(move || {
        type Fn1 = unsafe extern "C" fn(i64) -> i64;
        let f: Fn1 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0) };
    }));
}

/// Two-arg version.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_2(fn_addr: usize, arg0: i64, arg1: i64) {
    if fn_addr == 0 {
        return;
    }
    spawn_task(Box::new(move || {
        type Fn2 = unsafe extern "C" fn(i64, i64) -> i64;
        let f: Fn2 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1) };
    }));
}

/// Three-arg version. Required for fan-out patterns (buf, idx, wg).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_spawn_call_3(fn_addr: usize, arg0: i64, arg1: i64, arg2: i64) {
    if fn_addr == 0 {
        return;
    }
    spawn_task(Box::new(move || {
        type Fn3 = unsafe extern "C" fn(i64, i64, i64) -> i64;
        let f: Fn3 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2) };
    }));
}

/// Four-arg version. Common fasta worker shape (buf, start, count, wg).
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
    spawn_task(Box::new(move || {
        type Fn4 = unsafe extern "C" fn(i64, i64, i64, i64) -> i64;
        let f: Fn4 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3) };
    }));
}

/// Five-arg version. Used by fasta_mt's IUB worker.
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
    spawn_task(Box::new(move || {
        type Fn5 = unsafe extern "C" fn(i64, i64, i64, i64, i64) -> i64;
        let f: Fn5 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4) };
    }));
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
    spawn_task(Box::new(move || {
        type Fn6 = unsafe extern "C" fn(i64, i64, i64, i64, i64, i64) -> i64;
        let f: Fn6 = unsafe { std::mem::transmute(fn_addr) };
        let _ = unsafe { f(arg0, arg1, arg2, arg3, arg4, arg5) };
    }));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_go_yield() {
    // Real coroutine yield — suspend this goroutine and let the
    // worker M run another. The scheduler immediately re-enqueues
    // the suspended goroutine because we don't set the
    // pending-park flag, so this is a "give up the slice"
    // primitive (Go's `runtime.Gosched`). Falls back to an OS
    // yield if called outside a goroutine context.
    if gossamer_coro::in_goroutine() {
        gossamer_coro::suspend();
    } else {
        std::thread::yield_now();
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sleep_ns(ns: i64) {
    if ns <= 0 {
        return;
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_nanos(ns as u64);
    // Park on the netpoller's timer wheel so a sleeping goroutine
    // does not consume a worker slot for the full duration. The
    // worker thread is still parked on a Condvar, but the
    // scheduler's pool grows transparently if multiple goroutines
    // sleep concurrently.
    crate::sched_global::sleep_until(deadline);
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
// Thread-local arena. `gos_rt_gc_alloc(size)` bumps a pointer;
// when the arena fills, a new one is allocated and the old one
// stays alive until `gos_rt_gc_reset()` discards every arena on
// the current thread. Call reset at well-defined safepoints
// (end of main, between benchmark iterations, etc.).
//
// Arena buffers are capped at `MAX_ARENA_CAP` so the geometric
// growth path (2× per fresh arena) plateaus instead of running
// away. Without the cap, after K arenas total capacity was
// `ARENA_BYTES * (2^K - 1)`; with the cap it's at most
// `MAX_ARENA_CAP * K`. For long-running format-heavy programs
// this turns "exponential blowup of slack space at the tail of
// each arena" into "linear in the number of arenas needed".
//
// `gos_rt_arena_save() -> u64` / `gos_rt_arena_restore(saved)`
// expose a checkpoint/rewind primitive so codegen can wrap
// scope-bounded allocations (e.g. ephemeral format!() output that
// is consumed before the surrounding function returns) without
// permanently leaking the slack. The semantics are "undo every
// allocation made since the matching save"; callers must
// guarantee no live pointer into the saved range escapes the
// scope, since restore makes those pointers dangling.
//
// A real tri-color GC replaces this without changing the ABI.

const ARENA_BYTES: usize = 4 * 1024 * 1024;
const MAX_ARENA_CAP: usize = 16 * 1024 * 1024;

struct Arena {
    buf: Vec<u8>,
    used: usize,
    /// Start offset (within `buf`) of the most recent allocation
    /// returned by `gos_rt_gc_alloc`. Used by
    /// [`try_extend_last_cstring`] to grow `s = s + c`-style
    /// concatenation chains in place instead of leaking the
    /// previous slot.
    last_start: usize,
    last_len: usize,
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
    // Align requests >= 8 bytes to 8-byte boundaries so structs
    // and aggregates (GosResult, GosTuple, …) come out aligned.
    // Smaller requests (cstrings, single bytes) keep byte-packing
    // because strings only require 1-byte alignment and tight
    // packing matters for `try_extend_last_cstring`.
    let want_align = size >= 8;
    ARENAS.with(|cell| {
        let mut arenas = cell.borrow_mut();
        // Align the bump cursor *before* the capacity check so a
        // padded allocation that crosses the buffer end correctly
        // triggers a fresh arena.
        if want_align && let Some(a) = arenas.last_mut() {
            let pad = a.used.wrapping_neg() & 7;
            let padded = a.used.saturating_add(pad);
            if padded <= a.buf.len() {
                a.used = padded;
            }
        }
        if arenas.last().is_none_or(|a| a.used + size > a.buf.len()) {
            // The 2× headroom on `size` keeps `try_extend_last_cstring`
            // amortised: an exact-fit arena would force every
            // subsequent extension to roll over into a new arena, and
            // each rollover copies the current buffer, reintroducing
            // the O(N²) blowup the in-place extension was meant to
            // fix. The 2× on `prev_cap` keeps that amortisation
            // working when several large strings share the same
            // arena. Both are bounded by `MAX_ARENA_CAP` so the
            // doubling can't run away — beyond the cap each new
            // arena is `MAX_ARENA_CAP` bytes, so total slack across
            // K arenas is at most `K * MAX_ARENA_CAP` instead of
            // `ARENA_BYTES * (2^K - 1)`.
            let prev_cap = arenas.last().map_or(0, |a| a.buf.len());
            let cap = size
                .saturating_mul(2)
                .max(ARENA_BYTES)
                .max(prev_cap.saturating_mul(2))
                .min(MAX_ARENA_CAP.max(size));
            // Zero-initialised arena. Allocations are bumped out
            // of `buf` and the caller writes before reading, but
            // zeroing avoids reading-before-write UB if anyone
            // peeks at the raw arena memory.
            let buf = vec![0u8; cap];
            arenas.push(Arena {
                buf,
                used: 0,
                last_start: 0,
                last_len: 0,
            });
        }
        let arena = arenas.last_mut().unwrap();
        let ptr = unsafe { arena.buf.as_mut_ptr().add(arena.used) };
        arena.last_start = arena.used;
        arena.last_len = size;
        arena.used += size;
        ptr
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gc_reset() {
    // Reset the bump cursors in place rather than dropping the
    // arena Vecs. Dropping freed the underlying `Vec<u8>` storage,
    // which then had to be re-malloced on the next allocation —
    // 100k+ malloc/free pairs per second under HTTP keep-alive
    // load, hammering glibc's allocator lock. Keeping the arenas
    // alive across resets means the steady-state path is purely
    // bump-pointer arithmetic with zero allocator traffic.
    ARENAS.with(|cell| {
        let mut arenas = cell.borrow_mut();
        // If we accumulated multiple arenas (a single request
        // overflowed `ARENA_BYTES`), keep only the largest one
        // for reuse. Capacity is preserved; subsequent requests
        // reuse it without rolling over again.
        if arenas.len() > 1 {
            let mut keep_idx = 0usize;
            for (i, a) in arenas.iter().enumerate() {
                if a.buf.len() > arenas[keep_idx].buf.len() {
                    keep_idx = i;
                }
            }
            let keep = arenas.swap_remove(keep_idx);
            arenas.clear();
            arenas.push(keep);
        }
        if let Some(a) = arenas.last_mut() {
            a.used = 0;
            a.last_start = 0;
            a.last_len = 0;
        }
    });
}

/// Records the current arena watermark on the calling thread.
///
/// The returned `u64` packs `(arena_index << 32) | byte_used` so
/// `gos_rt_arena_restore` can identify both the arena and the
/// position within it. Calls outside any active arena return zero,
/// which `restore` interprets as "rewind everything" (equivalent
/// to `gos_rt_gc_reset`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_arena_save() -> u64 {
    ARENAS.with(|cell| {
        let arenas = cell.borrow();
        if arenas.is_empty() {
            return 0;
        }
        let idx = arenas.len() - 1;
        let used = arenas[idx].used;
        ((idx as u64) << 32) | (used as u64)
    })
}

/// Rewinds the calling thread's arena to the watermark `saved`
/// returned by an earlier `gos_rt_arena_save` call.
///
/// All arenas allocated after `saved` are dropped, and the active
/// arena's `used` is rolled back to the saved offset. Pointers
/// returned by `gos_rt_gc_alloc` between save and restore become
/// dangling; the caller is responsible for ensuring no live value
/// references them. This is the rewind half of the scope-bounded
/// allocation pattern; codegen-side wiring is a separate concern.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_arena_restore(saved: u64) {
    ARENAS.with(|cell| {
        let mut arenas = cell.borrow_mut();
        if saved == 0 {
            arenas.clear();
            return;
        }
        let idx = (saved >> 32) as usize;
        let used = (saved & 0xFFFF_FFFF) as usize;
        if idx >= arenas.len() {
            return;
        }
        arenas.truncate(idx + 1);
        if let Some(arena) = arenas.last_mut() {
            // If the saved offset is past the current high-water
            // mark (shouldn't happen for valid saves), clamp to
            // avoid producing garbage `used`.
            arena.used = arena.used.min(used).max(used.min(arena.buf.len()));
            // Reset the in-place extension cursor so the next
            // alloc isn't mistaken for an extension of an
            // already-rewound allocation.
            arena.last_start = arena.used;
            arena.last_len = 0;
        }
    });
}

/// If `a_ptr` points to the most recent NUL-terminated arena
/// allocation in the current thread's active arena and the arena
/// has room for `extra` additional bytes plus the relocated NUL,
/// extends that allocation in place by appending `extra` bytes
/// and returns `a_ptr`. Otherwise returns `null` so the caller
/// falls back to a fresh allocation.
///
/// The returned C-string still has a single trailing NUL; the
/// previous NUL is overwritten by the first byte of `extra` and a
/// new NUL is written one past the last appended byte.
fn try_extend_last_cstring(a_ptr: *const c_char, extra: &[u8]) -> *mut c_char {
    if a_ptr.is_null() {
        return std::ptr::null_mut();
    }
    ARENAS.with(|cell| {
        let mut arenas = cell.borrow_mut();
        let Some(arena) = arenas.last_mut() else {
            return std::ptr::null_mut();
        };
        // Allocations always include a trailing NUL inside their
        // recorded length. The most recent allocation occupies
        // `[last_start, last_start + last_len) == [last_start, used)`.
        let last_ptr = unsafe { arena.buf.as_mut_ptr().add(arena.last_start) };
        if last_ptr.cast::<c_char>() != a_ptr.cast_mut() {
            return std::ptr::null_mut();
        }
        if arena.last_len == 0 {
            return std::ptr::null_mut();
        }
        let payload_len = arena.last_len - 1; // bytes excluding the trailing NUL
        let need = arena.used + extra.len();
        if need > arena.buf.len() {
            return std::ptr::null_mut();
        }
        // Overwrite the existing NUL with the first byte of
        // `extra`, append the rest, then write a fresh NUL.
        unsafe {
            let nul_offset = arena.last_start + payload_len;
            let dst = arena.buf.as_mut_ptr().add(nul_offset);
            std::ptr::copy_nonoverlapping(extra.as_ptr(), dst, extra.len());
            *dst.add(extra.len()) = 0;
        }
        arena.used += extra.len();
        arena.last_len += extra.len();
        last_ptr.cast::<c_char>()
    })
}

// ---------------------------------------------------------------
// HTTP server
// ---------------------------------------------------------------
//
// Blocking TCP listener with one OS thread per accepted
// connection. Per connection we keep a `ConnScratch` reused
// across keep-alive requests so the steady state allocates
// nothing on the parse / response paths beyond what the user's
// handler does inside the gossamer arena (which is reset
// between requests). Phase 2 of the http_optimizations plan
// swaps `parse_request_into` for httparse and adds
// BufReader/BufWriter; today the parser is a naive CRLF split
// that's enough for HTTP/1.1 keep-alive bench traffic.

const STATIC_OK_RESPONSE: &[u8] =
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: 2\r\nConnection: keep-alive\r\n\r\nok";
const RESPONSE_500_BYTES: &[u8] =
    b"HTTP/1.1 500 Internal Server Error\r\nContent-Length: 0\r\nConnection: keep-alive\r\n\r\n";
const RESPONSE_400_BYTES: &[u8] =
    b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";

/// Per-connection mutable scratch. Reused across keep-alive
/// requests so steady state allocates only inside the gossamer
/// arena, which is reset between requests.
struct ConnScratch {
    /// Filled in place by `parse_request_into` and handed to
    /// the user handler as `*mut GosHttpRequest`. Lives for
    /// the entire connection.
    request: GosHttpRequest,
    /// Bytes written to the wire. Truncated, never freed,
    /// across requests.
    response_buf: Vec<u8>,
}

impl ConnScratch {
    fn new() -> Self {
        Self {
            request: GosHttpRequest {
                method: String::with_capacity(8),
                url: String::with_capacity(64),
                headers: Vec::with_capacity(16),
                body: Vec::with_capacity(0),
            },
            response_buf: Vec::with_capacity(512),
        }
    }
}

/// Starts an HTTP listener and dispatches each request to
/// `handler_fn(handler_env, request)`. Returns 200/payload from
/// the handler's `Ok(Response)`, 500 from `Err`, and a static
/// `200 OK\r\n\r\nok` when `handler_fn` is null (legacy stub).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_serve(
    addr: *const c_char,
    handler_env: *mut u8,
    handler_fn: i64,
) -> ! {
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
    let env_addr = handler_env as usize;
    let fn_addr = handler_fn as usize;
    // Per-connection goroutine on the M:N work-stealing pool.
    // Each accepted socket is dispatched via
    // `crate::sched_global::spawn`, so the connection lifetime is
    // owned by a scheduler-managed worker rather than a fresh OS
    // thread. The pool grows automatically when goroutines park
    // on socket reads (`MultiScheduler::enter_blocking_syscall`),
    // so the keep-alive throughput characteristics of the
    // previous thread-per-conn design hold under load.
    //
    // Read/write helpers (`conn_read_nonblocking` /
    // `conn_write_nonblocking`) drive non-blocking I/O against
    // the global netpoller: when the kernel buffer is empty (or
    // full), the helper registers interest with `OsPoller`,
    // calls `enter_blocking_syscall` to keep the pool warm, and
    // parks on a Condvar that the netpoller signals when the
    // socket is ready. This matches Go's `netpoll` shape: the
    // worker thread is parked while the socket waits, but the
    // scheduler always has at least one M ready to run other
    // goroutines.
    let read_timeout = std::time::Duration::from_secs(30);
    loop {
        let Ok((stream, _addr)) = listener.accept() else {
            break;
        };
        let _ = stream.set_nodelay(true);
        let _ = stream.set_read_timeout(Some(read_timeout));
        std::thread::Builder::new()
            .name("gos-http-conn".to_string())
            .spawn(move || {
                handle_http_conn(stream, env_addr, fn_addr);
            })
            .ok();
    }
    std::process::exit(0);
}

type HandlerFn = unsafe extern "C" fn(env: *mut u8, req: *mut GosHttpRequest) -> *mut GosResult;

fn handle_http_conn(mut stream: TcpStream, env_addr: usize, fn_addr: usize) {
    let mut scratch = ConnScratch::new();
    let mut accum: Vec<u8> = Vec::with_capacity(8192);
    let mut buf: Vec<u8> = vec![0u8; 8192];
    loop {
        let header_end = find_header_end(&accum);
        if header_end.is_none() {
            match stream.read(&mut buf) {
                Ok(0) => return,
                Ok(n) => {
                    accum.extend_from_slice(&buf[..n]);
                    continue;
                }
                Err(_) => return,
            }
        }
        let req_end = header_end.unwrap();
        // `raw` is the request's header bytes (inclusive of the
        // trailing `\r\n\r\n`). Anything past it is the next
        // request — keep it in `accum` for the next iteration.
        let raw = &accum[..req_end];

        scratch.response_buf.clear();

        if fn_addr == 0 {
            // Legacy stub path: ignore the request, send static
            // 200/ok. No arena allocation happens here.
            scratch.response_buf.extend_from_slice(STATIC_OK_RESPONSE);
        } else {
            // Reset the request scratch in place. Field
            // capacities persist; we only push back into them.
            scratch.request.method.clear();
            scratch.request.url.clear();
            scratch.request.headers.clear();
            scratch.request.body.clear();

            if !parse_request_into(raw, &mut scratch.request) {
                // Malformed request: send 400 and close. Keeping
                // the connection open after an unparseable request
                // is unsafe — we don't know how many bytes the
                // bogus request claimed, so the next request would
                // be misaligned. The connection will be reopened
                // by the client.
                let _ = stream.write_all(RESPONSE_400_BYTES);
                return;
            }

            // SAFETY: `fn_addr` came from `gos_fn_addr("T::serve")`
            // at the user's `http::serve(addr, app)` call site;
            // env_addr is the `&app` pointer passed alongside.
            let handler: HandlerFn = unsafe { std::mem::transmute(fn_addr) };
            let env_ptr = env_addr as *mut u8;
            let req_ptr: *mut GosHttpRequest = &raw mut scratch.request;
            let result_ptr = unsafe { handler(env_ptr, req_ptr) };
            if !extract_response_into(result_ptr, &mut scratch.response_buf) {
                scratch.response_buf.extend_from_slice(RESPONSE_500_BYTES);
            }
            unsafe { drop_handler_result(result_ptr) };

            // Reset the per-thread gossamer arena. The handler
            // may have allocated strings/vecs into it (e.g.
            // `format!` output backing the response body, json
            // encoding output); without this the arena grows
            // unboundedly across requests on a long-lived
            // connection.
            unsafe { gos_rt_gc_reset() };
        }

        if stream.write_all(&scratch.response_buf).is_err() {
            return;
        }
        // Drop the consumed request from the accumulator while
        // preserving any pipelined remainder. `drain` shifts the
        // tail into place; capacity is retained.
        accum.drain(..req_end);
    }
}

/// Connection wrapper that bridges a non-blocking [`TcpStream`] to
/// the global netpoller. Reads and writes that would block register
/// interest with [`crate::sched_global`] and park the calling
/// goroutine on a Condvar; the netpoller wakes the waker when the
/// kernel reports readiness.
#[allow(dead_code)]
struct HttpConn {
    stream: TcpStream,
    mio_stream: mio::net::TcpStream,
    last_source: Option<crate::sched::PollSource>,
}

#[allow(dead_code)]
impl HttpConn {
    fn wrap(stream: TcpStream) -> Option<Self> {
        let cloned = stream.try_clone().ok()?;
        Some(Self {
            mio_stream: mio::net::TcpStream::from_std(cloned),
            stream,
            last_source: None,
        })
    }

    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        loop {
            match std::io::Read::read(&mut self.stream, buf) {
                Ok(n) => return Ok(n),
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(crate::sched::Interest::Readable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
    }

    fn write_all(&mut self, mut buf: &[u8]) -> std::io::Result<()> {
        while !buf.is_empty() {
            match std::io::Write::write(&mut self.stream, buf) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "wrote zero bytes",
                    ));
                }
                Ok(n) => buf = &buf[n..],
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    self.wait(crate::sched::Interest::Writable)?;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }

    fn wait(&mut self, interest: crate::sched::Interest) -> std::io::Result<()> {
        // Goroutine-aware wait: park the calling coroutine on the
        // netpoller's readiness signal. The worker thread is freed
        // to run other goroutines while we wait. When called from
        // a non-goroutine OS thread (e.g. tooling code), the helper
        // falls back to a brief OS-thread sleep.
        crate::sched_global::wait_io(&mut self.mio_stream, interest)
    }
}

impl Drop for HttpConn {
    fn drop(&mut self) {
        if let Some(source) = self.last_source.take() {
            // Best-effort deregistration; the netpoller's `by_source`
            // map will leak the slot otherwise.
            let _ = crate::sched_global::with_poller(|p| {
                p.deregister_io(
                    &mut self.mio_stream,
                    source,
                    crate::sched::Interest::Readable,
                )
            });
        }
    }
}

/// Returns the index *one past* the trailing `\r\n\r\n` of the
/// first complete header section in `buf`, or `None` when the
/// buffer doesn't yet contain a full request header.
fn find_header_end(buf: &[u8]) -> Option<usize> {
    if buf.len() < 4 {
        return None;
    }
    let needle = b"\r\n\r\n";
    buf.windows(4).position(|w| w == needle).map(|p| p + 4)
}

/// Drops the `GosHttpResponse` referenced by the handler's
/// `Result` so each request doesn't leak. Three cases:
///
/// 1. The response was constructed via `gos_rt_http_response_text_new`
///    / `_json_new` — the new fast path returns a pointer to a
///    per-thread reusable buffer (no Box). We just clear it for
///    the next request; do NOT call `Box::from_raw`.
/// 2. The response was constructed by some other path that did
///    Box-allocate (e.g. `gos_rt_http_request_send` from the
///    client side, never reachable from a server handler today).
/// 3. `result` is null or carries `Err` — nothing to drop.
unsafe fn drop_handler_result(result: *mut GosResult) {
    if result.is_null() {
        return;
    }
    let r = unsafe { &*result };
    if r.disc != 0 {
        return;
    }
    let response_ptr = r.payload as *mut GosHttpResponse;
    if response_ptr.is_null() {
        return;
    }
    if is_thread_local_response(response_ptr) {
        // Per-thread buffer: don't free, just reset for the next
        // request. The arena reset at the end of `handle_http_conn`
        // reclaims any cstrings the response pointed at.
        unsafe {
            (*response_ptr).status = 0;
            (*response_ptr).body = std::ptr::null_mut();
            (*response_ptr).headers.clear();
        }
        return;
    }
    drop(unsafe { Box::from_raw(response_ptr) });
}

thread_local! {
    /// Reusable response buffer for the server's per-request
    /// constructors (`gos_rt_http_response_text_new` /
    /// `_json_new`). Eliminates the per-request `Box::into_raw` /
    /// `Box::from_raw` malloc/free pair that was the dominant
    /// per-request cost — at conc=100 the system allocator's lock
    /// became the bottleneck. The buffer is owned by the worker
    /// thread; the caller writes status/body/headers and returns
    /// the buffer's address. `drop_handler_result` recognises the
    /// pointer (by `is_thread_local_response`) and skips the free.
    static RESPONSE_BUF: std::cell::UnsafeCell<GosHttpResponse> = const {
        std::cell::UnsafeCell::new(GosHttpResponse {
            status: 0,
            body: std::ptr::null_mut(),
            headers: Vec::new(),
        })
    };
}

fn thread_local_response_ptr() -> *mut GosHttpResponse {
    RESPONSE_BUF.with(std::cell::UnsafeCell::get)
}

fn is_thread_local_response(p: *mut GosHttpResponse) -> bool {
    p == thread_local_response_ptr()
}

/// Parses `raw` into `request` in place. Returns false on
/// malformed input. Headers and body are parsed lazily — we only
/// extract the request line (method + path) here, since the
/// bench handler and most simple endpoints never read headers.
/// `request.header(name)` materialises the header list on
/// demand from the saved raw buffer (`request.raw_buf`).
fn parse_request_into(raw: &[u8], request: &mut GosHttpRequest) -> bool {
    let Ok(text) = std::str::from_utf8(raw) else {
        return false;
    };
    let Some(request_line_end) = text.find("\r\n") else {
        return false;
    };
    let request_line = &text[..request_line_end];
    let mut parts = request_line.split_whitespace();
    let Some(method) = parts.next() else {
        return false;
    };
    let Some(url) = parts.next() else {
        return false;
    };
    request.method.push_str(method);
    request.url.push_str(url);
    // Stash the raw bytes so `request.header(name)` can lazily
    // scan them on demand. Reuses the existing `body` Vec as the
    // raw buffer (the bench paths never actually push to body
    // and `clear()` retains capacity, so this is a cheap copy
    // that amortises across requests).
    request.body.extend_from_slice(raw);
    true
}

/// Writes `result`'s response payload (status + headers +
/// body) into `out` as raw HTTP/1.1 bytes. Returns false if
/// `result` doesn't carry a valid OK response.
fn extract_response_into(result: *mut GosResult, out: &mut Vec<u8>) -> bool {
    if result.is_null() {
        return false;
    }
    let r = unsafe { &*result };
    if r.disc != 0 {
        return false;
    }
    let response_ptr = r.payload as *const GosHttpResponse;
    if response_ptr.is_null() {
        return false;
    }
    let response = unsafe { &*response_ptr };
    let body_bytes: &[u8] = if response.body.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(response.body).to_bytes() }
    };
    out.extend_from_slice(b"HTTP/1.1 ");
    let mut buf = itoa::Buffer::new();
    out.extend_from_slice(buf.format(response.status).as_bytes());
    out.extend_from_slice(b" ");
    out.extend_from_slice(status_reason(response.status).as_bytes());
    out.extend_from_slice(b"\r\n");
    let mut has_content_length = false;
    let mut has_content_type = false;
    for (k, v) in &response.headers {
        if k.eq_ignore_ascii_case("content-length") {
            has_content_length = true;
        }
        if k.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        out.extend_from_slice(k.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(v.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    if !has_content_type {
        out.extend_from_slice(b"Content-Type: application/json\r\n");
    }
    if !has_content_length {
        out.extend_from_slice(b"Content-Length: ");
        out.extend_from_slice(buf.format(body_bytes.len()).as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"Connection: keep-alive\r\n\r\n");
    out.extend_from_slice(body_bytes);
    true
}

/// Maps a status code to its canonical reason phrase.
/// Falls back to `"OK"` for unknown codes — caller is
/// expected to use a sensible status; this is best-effort.
const fn status_reason(status: i64) -> &'static str {
    match status {
        100 => "Continue",
        101 => "Switching Protocols",
        200 => "OK",
        201 => "Created",
        202 => "Accepted",
        204 => "No Content",
        301 => "Moved Permanently",
        302 => "Found",
        304 => "Not Modified",
        307 => "Temporary Redirect",
        308 => "Permanent Redirect",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        408 => "Request Timeout",
        409 => "Conflict",
        413 => "Payload Too Large",
        414 => "URI Too Long",
        429 => "Too Many Requests",
        500 => "Internal Server Error",
        501 => "Not Implemented",
        502 => "Bad Gateway",
        503 => "Service Unavailable",
        504 => "Gateway Timeout",
        _ => "OK",
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
    /// Goroutine id of the most recent unlocker. Read by the next
    /// lock acquirer to record a happens-before edge into the race
    /// detector. `-1` means "never been locked".
    last_unlocker: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_mutex_new() -> *mut GosMutex {
    Box::into_raw(Box::new(GosMutex {
        inner: parking_lot::Mutex::new(()),
        last_unlocker: AtomicI64::new(-1),
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
    let from = m.last_unlocker.load(Ordering::Acquire);
    if from >= 0 {
        crate::race::record_sync(u32::try_from(from).unwrap_or(0), crate::race::current_gid());
    }
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
    m.last_unlocker
        .store(i64::from(crate::race::current_gid()), Ordering::Release);
    unsafe { m.inner.force_unlock() };
}

// ---------------------------------------------------------------
// WaitGroup primitive
// ---------------------------------------------------------------
//
// Mirrors `sync.WaitGroup` in Go. `add(n)` bumps a counter,
// `done()` decrements, `wait()` blocks until the counter hits
// zero. Implemented as `(parking_lot::Mutex<i64>, parking_lot
// ::Condvar)` plus a sticky error flag so misuse never panics
// while the lock is held.

pub struct GosWaitGroup {
    counter: parking_lot::Mutex<i64>,
    cv: parking_lot::Condvar,
    /// Sticky misuse marker. Bit 0 set on underflow (done called
    /// more than add granted), bit 1 set on overflow (counter would
    /// pass `i64::MAX`). Surfaced via `gos_rt_wg_error` so callers
    /// can fail loudly without taking a panic path while the
    /// counter mutex is held.
    error: AtomicI64,
    /// Goroutine id of the most recent caller of `done`. Used by
    /// `wait` to record a happens-before edge so the race detector
    /// observes that the waiter sees everything the done-callers
    /// did before signalling.
    last_done: AtomicI64,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_new() -> *mut GosWaitGroup {
    Box::into_raw(Box::new(GosWaitGroup {
        counter: parking_lot::Mutex::new(0),
        cv: parking_lot::Condvar::new(),
        error: AtomicI64::new(0),
        last_done: AtomicI64::new(-1),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_add(wg: *mut GosWaitGroup, n: i64) -> i64 {
    if wg.is_null() {
        return -1;
    }
    let wg = unsafe { &*wg };
    let mut c = wg.counter.lock();
    if let Some(v) = c.checked_add(n) {
        *c = v;
        if v < 0 {
            wg.error.fetch_or(1, Ordering::Relaxed);
        }
        if v <= 0 {
            wg.cv.notify_all();
        }
        v
    } else {
        wg.error.fetch_or(2, Ordering::Relaxed);
        -1
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_done(wg: *mut GosWaitGroup) -> i64 {
    if wg.is_null() {
        return -1;
    }
    let wg = unsafe { &*wg };
    let mut c = wg.counter.lock();
    *c -= 1;
    let value = *c;
    if value < 0 {
        wg.error.fetch_or(1, Ordering::Relaxed);
    }
    if value <= 0 {
        wg.cv.notify_all();
    }
    drop(c);
    wg.last_done
        .store(i64::from(crate::race::current_gid()), Ordering::Release);
    value
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
    drop(c);
    let from = wg.last_done.load(Ordering::Acquire);
    if from >= 0 {
        crate::race::record_sync(u32::try_from(from).unwrap_or(0), crate::race::current_gid());
    }
}

/// Returns the sticky misuse bitmask: 0 = ok, 1 = underflow seen,
/// 2 = overflow seen, 3 = both. Reading does not clear the flag;
/// `gos_rt_wg_error_clear` resets it.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_error(wg: *const GosWaitGroup) -> i64 {
    if wg.is_null() {
        return 0;
    }
    let wg = unsafe { &*wg };
    wg.error.load(Ordering::Relaxed)
}

/// Clears the sticky misuse bitmask. Returns the value observed
/// before the clear so callers can act on whatever was queued.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_wg_error_clear(wg: *mut GosWaitGroup) -> i64 {
    if wg.is_null() {
        return 0;
    }
    let wg = unsafe { &*wg };
    wg.error.swap(0, Ordering::Relaxed)
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
//
// **Concurrency contract.** `GosI64Vec` and `GosU8Vec` are
// **single-owner**: the backing buffer is allocated and freed by
// one goroutine, and concurrent mutation across goroutines is
// undefined behaviour. `gos_rt_arr_push`-style operations resize
// by reallocating `data`, so two goroutines that both observe
// `len == cap` and both reallocate corrupt the heap. For
// cross-goroutine sharing use the `GosSyncI64Vec` / `GosSyncU8Vec`
// types defined below — same conceptual shape, every operation
// guarded by an internal `parking_lot` mutex.

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
    let _guard = StdoutGuard::acquire();
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
    let _guard = StdoutGuard::acquire();
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

/// Materialises the first `len` bytes of a `U8Vec` into a fresh
/// immutable `String` (NUL-terminated arena allocation). The
/// canonical "freeze the build buffer" step at the end of an
/// incremental construction loop — equivalent to F#'s
/// `StringBuilder.ToString()` or Rust's
/// `String::from_utf8(vec).unwrap()`.
///
/// `len` is a separate argument because callers typically
/// pre-allocate a capacity-sized `U8Vec` and write fewer bytes
/// than the buffer's nominal length. Returns the empty string
/// when `v` is null, `len` is non-positive, or `len` exceeds the
/// buffer's nominal length.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_heap_u8_to_string(v: *const GosU8Vec, len: i64) -> *mut c_char {
    if v.is_null() || len <= 0 {
        return alloc_cstring(b"");
    }
    let v_ref = unsafe { &*v };
    if v_ref.data.is_null() {
        return alloc_cstring(b"");
    }
    let cap = v_ref.len.max(0) as usize;
    let take = (len as usize).min(cap);
    let bytes: &[u8] = unsafe { std::slice::from_raw_parts(v_ref.data, take) };
    alloc_cstring(bytes)
}

/// Bulk write `v[start..start+count]` to stdout, emitting a
/// newline after every `line_width` bytes. Single FFI call so
/// fasta-shape programs don't pay one `gos_rt_print_*` per byte.
#[unsafe(no_mangle)]
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
    let _guard = StdoutGuard::acquire();
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
    let _guard = StdoutGuard::acquire();
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
// SyncI64Vec / SyncU8Vec — cross-goroutine-safe vec wrappers
// ---------------------------------------------------------------
//
// Same conceptual shape as `GosI64Vec` / `GosU8Vec` but with the
// backing storage owned by a `parking_lot::Mutex<Vec<_>>`. Every
// operation takes the mutex briefly so concurrent push/get/set
// across goroutines is safe. Use this whenever the same `vec`
// value is captured into a `go` closure or placed on a channel.

pub struct GosSyncI64Vec {
    inner: parking_lot::Mutex<Vec<i64>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_new(len: i64) -> *mut GosSyncI64Vec {
    let n = if len < 0 { 0 } else { len as usize };
    Box::into_raw(Box::new(GosSyncI64Vec {
        inner: parking_lot::Mutex::new(vec![0i64; n]),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_drop(v: *mut GosSyncI64Vec) {
    if v.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(v) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_len(v: *const GosSyncI64Vec) -> i64 {
    if v.is_null() {
        return 0;
    }
    let v = unsafe { &*v };
    i64::try_from(v.inner.lock().len()).unwrap_or(i64::MAX)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_get(v: *const GosSyncI64Vec, idx: i64) -> i64 {
    if v.is_null() || idx < 0 {
        return 0;
    }
    let v = unsafe { &*v };
    let g = v.inner.lock();
    g.get(idx as usize).copied().unwrap_or(0)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_set(v: *mut GosSyncI64Vec, idx: i64, val: i64) {
    if v.is_null() || idx < 0 {
        return;
    }
    let v = unsafe { &*v };
    let mut g = v.inner.lock();
    if let Some(slot) = g.get_mut(idx as usize) {
        *slot = val;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_push(v: *mut GosSyncI64Vec, val: i64) {
    if v.is_null() {
        return;
    }
    let v = unsafe { &*v };
    v.inner.lock().push(val);
}

/// Atomic increment: `vec[idx] += delta`, returns the new value.
/// Used by fan-out workers that share a counter slot without
/// needing a separate AtomicI64 per slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_i64_add(v: *mut GosSyncI64Vec, idx: i64, delta: i64) -> i64 {
    if v.is_null() || idx < 0 {
        return 0;
    }
    let v = unsafe { &*v };
    let mut g = v.inner.lock();
    if let Some(slot) = g.get_mut(idx as usize) {
        *slot = slot.wrapping_add(delta);
        *slot
    } else {
        0
    }
}

pub struct GosSyncU8Vec {
    inner: parking_lot::Mutex<Vec<u8>>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_new(len: i64) -> *mut GosSyncU8Vec {
    let n = if len < 0 { 0 } else { len as usize };
    Box::into_raw(Box::new(GosSyncU8Vec {
        inner: parking_lot::Mutex::new(vec![0u8; n]),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_drop(v: *mut GosSyncU8Vec) {
    if v.is_null() {
        return;
    }
    drop(unsafe { Box::from_raw(v) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_len(v: *const GosSyncU8Vec) -> i64 {
    if v.is_null() {
        return 0;
    }
    let v = unsafe { &*v };
    i64::try_from(v.inner.lock().len()).unwrap_or(i64::MAX)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_get(v: *const GosSyncU8Vec, idx: i64) -> i64 {
    if v.is_null() || idx < 0 {
        return 0;
    }
    let v = unsafe { &*v };
    let g = v.inner.lock();
    g.get(idx as usize).copied().map_or(0, i64::from)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_set(v: *mut GosSyncU8Vec, idx: i64, val: i64) {
    if v.is_null() || idx < 0 {
        return;
    }
    let v = unsafe { &*v };
    let mut g = v.inner.lock();
    if let Some(slot) = g.get_mut(idx as usize) {
        *slot = val as u8;
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_sync_u8_push(v: *mut GosSyncU8Vec, val: i64) {
    if v.is_null() {
        return;
    }
    let v = unsafe { &*v };
    v.inner.lock().push(val as u8);
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

/// Acquire-ordered load. Cheaper than the SeqCst variant on
/// architectures with relaxed memory models (ARM64, RISC-V); on
/// x86 it lowers to the same instruction. Pair with the `_release`
/// store at the producer side for the standard release/acquire
/// pattern (`Mutex`-like handoff, lock-free queue head, etc.).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load_acquire(a: *const GosAtomicI64) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.load(Ordering::Acquire)
}

/// Release-ordered store, paired with `_load_acquire`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store_release(a: *mut GosAtomicI64, val: i64) {
    if a.is_null() {
        return;
    }
    let a = unsafe { &*a };
    a.inner.store(val, Ordering::Release);
}

/// Relaxed load — no synchronisation, only atomicity. Useful for
/// progress counters, generation tokens, and other observable-
/// from-anywhere values where ordering is enforced separately.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_load_relaxed(a: *const GosAtomicI64) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.load(Ordering::Relaxed)
}

/// Relaxed store, paired with `_load_relaxed`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_store_relaxed(a: *mut GosAtomicI64, val: i64) {
    if a.is_null() {
        return;
    }
    let a = unsafe { &*a };
    a.inner.store(val, Ordering::Relaxed);
}

/// AcqRel-ordered fetch_add. Use when both producer and consumer
/// observe the modification (CAS loops, ticket counters).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_fetch_add_acqrel(
    a: *mut GosAtomicI64,
    delta: i64,
) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.fetch_add(delta, Ordering::AcqRel)
}

/// Compare-and-swap with SeqCst semantics. Returns `1` when the
/// swap happened, `0` when the observed value did not match
/// `expected`. Used to implement spin-locks and lock-free
/// data structures from compiled code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_cas(
    a: *mut GosAtomicI64,
    expected: i64,
    new: i64,
) -> i32 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    match a
        .inner
        .compare_exchange(expected, new, Ordering::SeqCst, Ordering::SeqCst)
    {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Acquire-on-success / Acquire-on-failure CAS. Cheaper than the
/// SeqCst variant on relaxed-memory hosts.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_cas_acq_rel(
    a: *mut GosAtomicI64,
    expected: i64,
    new: i64,
) -> i32 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    match a
        .inner
        .compare_exchange(expected, new, Ordering::AcqRel, Ordering::Acquire)
    {
        Ok(_) => 1,
        Err(_) => 0,
    }
}

/// Atomic exchange — returns the previous value.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_atomic_i64_swap(a: *mut GosAtomicI64, val: i64) -> i64 {
    if a.is_null() {
        return 0;
    }
    let a = unsafe { &*a };
    a.inner.swap(val, Ordering::AcqRel)
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

// ---------------------------------------------------------------
// JSON runtime — wraps `serde_json::Value` behind a heap pointer
// so user code can do `json::parse(s)`, `value.field`, and
// `value.as_i64()` from compiled Gossamer. The MIR lowerer
// rewrites field access on a `json::Value` receiver into a
// `gos_rt_json_get(value, "field")` call before the cranelift
// backend sees it.
// ---------------------------------------------------------------

/// Heap-allocated JSON node. The compiled tier shuttles raw
/// `*mut GosJson` pointers through normal i64 slots; the runtime
/// owns every node exclusively (each helper that "returns" a value
/// boxes a fresh node). Lifetime tied to the next
/// `gos_rt_gc_reset` only for the cstring helpers — JSON nodes are
/// `Box`-leaked on purpose so they survive arena resets.
#[derive(Debug, Clone)]
pub struct GosJson {
    inner: serde_json::Value,
}

impl GosJson {
    fn into_raw(value: serde_json::Value) -> *mut GosJson {
        Box::into_raw(Box::new(GosJson { inner: value }))
    }

    fn null_ptr() -> *mut GosJson {
        Self::into_raw(serde_json::Value::Null)
    }
}

unsafe fn json_borrow<'a>(p: *const GosJson) -> Option<&'a serde_json::Value> {
    if p.is_null() {
        None
    } else {
        Some(unsafe { &(*p).inner })
    }
}

/// `json::parse(text) -> Result<json::Value, String>` runtime
/// entry point. Returns a real `GosResult` so `match` and `?`
/// work across function boundaries in compiled code.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_parse(text: *const c_char) -> *mut GosResult {
    let bytes: &[u8] = if text.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(text).to_bytes() }
    };
    match std::str::from_utf8(bytes).map(serde_json::from_str::<serde_json::Value>) {
        Ok(Ok(v)) => {
            let ptr = GosJson::into_raw(v);
            unsafe { gos_rt_result_new(0, ptr as i64) }
        }
        Ok(Err(e)) => {
            let msg = format!("{e}");
            let cs = alloc_cstring(msg.as_bytes());
            unsafe { gos_rt_result_new(1, cs as i64) }
        }
        Err(_) => unsafe { gos_rt_result_new(1, alloc_cstring(b"invalid UTF-8") as i64) },
    }
}

/// `json::render(value) -> String`. Always returns a non-null
/// C-string (empty on null input) into the GC arena.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_render(j: *const GosJson) -> *mut c_char {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return alloc_cstring(b"");
    };
    let s = serde_json::to_string(v).unwrap_or_default();
    alloc_cstring(s.as_bytes())
}

/// `value.get(key) -> json::Value`. Returns a fresh `GosJson*`
/// holding the field's value, or a JSON-null node when the
/// receiver is not an object or the field is missing. Nested
/// chains (`root.latency.low_ms`) work because each call returns
/// a real handle the next call can dereference.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_get(j: *const GosJson, key: *const c_char) -> *mut GosJson {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return GosJson::null_ptr();
    };
    let key_bytes: &[u8] = if key.is_null() {
        b""
    } else {
        unsafe { CStr::from_ptr(key).to_bytes() }
    };
    let Ok(key_str) = std::str::from_utf8(key_bytes) else {
        return GosJson::null_ptr();
    };
    match v.get(key_str) {
        Some(child) => GosJson::into_raw(child.clone()),
        None => GosJson::null_ptr(),
    }
}

/// `value.at(idx) -> json::Value`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_at(j: *const GosJson, idx: i64) -> *mut GosJson {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return GosJson::null_ptr();
    };
    if idx < 0 {
        return GosJson::null_ptr();
    }
    match v.get(idx as usize) {
        Some(child) => GosJson::into_raw(child.clone()),
        None => GosJson::null_ptr(),
    }
}

/// `value.len() -> i64` for arrays and objects; 0 elsewhere.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_len(j: *const GosJson) -> i64 {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return 0;
    };
    match v {
        serde_json::Value::Array(a) => a.len() as i64,
        serde_json::Value::Object(o) => o.len() as i64,
        serde_json::Value::String(s) => s.len() as i64,
        _ => 0,
    }
}

/// `value.is_null() -> bool` (returns 1/0 i32, the codegen ABI).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_is_null(j: *const GosJson) -> i32 {
    match unsafe { json_borrow(j) } {
        Some(serde_json::Value::Null) | None => 1,
        Some(_) => 0,
    }
}

/// `value.as_i64() -> i64`. JSON numbers convert; everything else
/// returns 0 (matches the interpreter's `unwrap_or(0)` shape).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_as_i64(j: *const GosJson) -> i64 {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return 0;
    };
    match v {
        serde_json::Value::Number(n) => n
            .as_i64()
            .unwrap_or_else(|| n.as_f64().unwrap_or(0.0) as i64),
        serde_json::Value::Bool(b) => i64::from(*b),
        serde_json::Value::String(s) => s.parse::<i64>().unwrap_or(0),
        _ => 0,
    }
}

/// `value.as_f64() -> f64`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_as_f64(j: *const GosJson) -> f64 {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return 0.0;
    };
    match v {
        serde_json::Value::Number(n) => n.as_f64().unwrap_or(0.0),
        serde_json::Value::Bool(true) => 1.0,
        serde_json::Value::Bool(false) => 0.0,
        serde_json::Value::String(s) => s.parse::<f64>().unwrap_or(0.0),
        _ => 0.0,
    }
}

/// `value.as_str() -> String`. Strings round-trip; non-string
/// values render through serde_json::to_string so users can still
/// log them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_as_str(j: *const GosJson) -> *mut c_char {
    let Some(v) = (unsafe { json_borrow(j) }) else {
        return alloc_cstring(b"");
    };
    match v {
        serde_json::Value::String(s) => alloc_cstring(s.as_bytes()),
        other => {
            let rendered = serde_json::to_string(other).unwrap_or_default();
            alloc_cstring(rendered.as_bytes())
        }
    }
}

/// `value.as_bool() -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_as_bool(j: *const GosJson) -> i32 {
    match unsafe { json_borrow(j) } {
        Some(serde_json::Value::Bool(true)) => 1,
        Some(serde_json::Value::Number(n)) if n.as_f64().unwrap_or(0.0) != 0.0 => 1,
        Some(serde_json::Value::String(s)) if !s.is_empty() => 1,
        _ => 0,
    }
}

/// Identity helper for `json::as_array` / similar type
/// assertions — the runtime doesn't keep separate array vs
/// object handles, so the as_* coercions just thread the
/// receiver through unchanged. Lets MIR lowering route these
/// names without special-casing them at the call site.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_identity(j: *mut GosJson) -> *mut GosJson {
    j
}

/// `json::Value::String(s)` constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_string(s: *const c_char) -> *mut GosJson {
    let text = if s.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(s).to_string_lossy().into_owned() }
    };
    GosJson::into_raw(serde_json::Value::String(text))
}

/// `json::Value::Int(n)` constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_int(n: i64) -> *mut GosJson {
    GosJson::into_raw(serde_json::Value::Number(n.into()))
}

/// `json::Value::Bool(b)` constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_bool(b: i32) -> *mut GosJson {
    GosJson::into_raw(serde_json::Value::Bool(b != 0))
}

/// `json::Value::Null` constructor.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_null() -> *mut GosJson {
    GosJson::null_ptr()
}

/// `json::Value::Array(vec)` constructor. Takes a `*mut GosVec` of
/// `*mut GosJson` element pointers and rebuilds a real
/// `serde_json::Value::Array`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_array(vec: *const GosVec) -> *mut GosJson {
    let mut out: Vec<serde_json::Value> = Vec::new();
    if !vec.is_null() {
        let header = unsafe { &*vec };
        let len = usize::try_from(header.len.max(0)).unwrap_or(0);
        if !header.ptr.is_null() && len > 0 {
            let elems =
                unsafe { std::slice::from_raw_parts(header.ptr.cast::<*const GosJson>(), len) };
            for elem in elems {
                if let Some(v) = unsafe { json_borrow(*elem) } {
                    out.push(v.clone());
                } else {
                    out.push(serde_json::Value::Null);
                }
            }
        }
    }
    GosJson::into_raw(serde_json::Value::Array(out))
}

/// `json::Value::object(n, pairs_ptr)` — fan-out constructor
/// that takes the pair count and a flat `[k0, v0, k1, v1, …]`
/// arena buffer. Lets the MIR lowerer materialise an array
/// literal of `(String, json::Value)` pairs into a 16-B-strided
/// buffer without going through `gos_rt_vec_push` (which
/// truncates at 8 bytes today). The legacy
/// `gos_rt_json_value_object(*mut GosVec)` survives for runner
/// builds that still pass a real `GosVec`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_object_n(n: i64, pairs: *const i64) -> *mut GosJson {
    let mut out = serde_json::Map::new();
    let n = usize::try_from(n.max(0)).unwrap_or(0);
    if !pairs.is_null() && n > 0 {
        let slice = unsafe { std::slice::from_raw_parts(pairs, n * 2) };
        for chunk in slice.chunks_exact(2) {
            let key_ptr = chunk[0] as *const c_char;
            let val_ptr = chunk[1] as *mut GosJson;
            let key = if key_ptr.is_null() {
                String::new()
            } else {
                unsafe { CStr::from_ptr(key_ptr).to_string_lossy().into_owned() }
            };
            let v = if let Some(v) = unsafe { json_borrow(val_ptr) } {
                v.clone()
            } else {
                serde_json::Value::Null
            };
            out.insert(key, v);
        }
    }
    GosJson::into_raw(serde_json::Value::Object(out))
}

/// `json::Value::object([(k, v), ...])` constructor. Takes a
/// `*mut GosVec` of `(String, *mut GosJson)` tuple pointers.
/// Used by the runner-build path; the compiled tier prefers
/// `gos_rt_json_value_object_n` to dodge `*mut GosVec` plumbing
/// for the array-literal-of-pairs shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_json_value_object(vec: *const GosVec) -> *mut GosJson {
    let mut out = serde_json::Map::new();
    if !vec.is_null() {
        let header = unsafe { &*vec };
        let raw_len = usize::try_from(header.len.max(0)).unwrap_or(0);
        let elem_bytes = header.elem_bytes as usize;
        // The compiled tier passes raw stack-arrays where the
        // call site expected a `*mut GosVec`; in that case the
        // first 8 bytes the runtime reads as `header.len` are
        // actually the first key's c_char pointer (huge value),
        // and following the bogus length crashes on the next
        // strlen. Bail early when the header doesn't look like
        // a GosVec we built (`elem_bytes` is one of the small
        // shapes we hand out, the length is plausible).
        let header_looks_valid = matches!(elem_bytes, 8 | 16 | 24) && raw_len <= 16 * 1024 * 1024;
        if header_looks_valid && !header.ptr.is_null() && raw_len > 0 {
            // Tuples in the compiled tier currently get pushed as
            // flat 8-byte slots — `[("k", v), ("k2", v2)]` lands
            // as `len = 4` of i64 slots, not `len = 2` of 16-byte
            // pairs. Detect this by `elem_bytes`: if it's 8, treat
            // `len` as half the tuple count and stride 8; if it's
            // 16, treat `len` as the tuple count and stride 16.
            let tuple_count = if elem_bytes == 16 {
                raw_len
            } else {
                raw_len / 2
            };
            let pairs =
                unsafe { std::slice::from_raw_parts(header.ptr.cast::<[i64; 2]>(), tuple_count) };
            for pair in pairs {
                let key_ptr = pair[0] as *const c_char;
                let val_ptr = pair[1] as *mut GosJson;
                let key = if key_ptr.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(key_ptr).to_string_lossy().into_owned() }
                };
                let v = if let Some(v) = unsafe { json_borrow(val_ptr) } {
                    v.clone()
                } else {
                    serde_json::Value::Null
                };
                out.insert(key, v);
            }
        }
    }
    GosJson::into_raw(serde_json::Value::Object(out))
}

// ---------------------------------------------------------------
// errors module — Gossamer's `Result<T, errors::Error>` plumbing.
// `Error` is an opaque heap struct: a leaked message string plus
// an optional cause pointer. The compiled tier represents an
// `errors::Error` value as `*mut GosError`; `Option<&Error>`
// (`e.cause()` return) is the same pointer with `null` for
// `None`.
// ---------------------------------------------------------------

#[repr(C)]
pub struct GosError {
    /// Heap-leaked, nul-terminated UTF-8 message.
    message: *mut c_char,
    /// Cause pointer. NULL when the error has no cause.
    cause: *mut GosError,
}

unsafe impl Send for GosError {}
unsafe impl Sync for GosError {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_new(msg: *const c_char) -> *mut GosError {
    let text = if msg.is_null() {
        Vec::new()
    } else {
        unsafe { CStr::from_ptr(msg).to_bytes().to_vec() }
    };
    let leaked = alloc_cstring(&text);
    Box::into_raw(Box::new(GosError {
        message: leaked,
        cause: std::ptr::null_mut(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_wrap(
    cause: *mut GosError,
    msg: *const c_char,
) -> *mut GosError {
    let text = if msg.is_null() {
        Vec::new()
    } else {
        unsafe { CStr::from_ptr(msg).to_bytes().to_vec() }
    };
    let leaked = alloc_cstring(&text);
    Box::into_raw(Box::new(GosError {
        message: leaked,
        cause,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_message(err: *const GosError) -> *mut c_char {
    if err.is_null() {
        return alloc_cstring(b"");
    }
    let m = unsafe { (*err).message };
    if m.is_null() {
        return alloc_cstring(b"");
    }
    // Re-leak a copy so the caller can hold the string past the
    // GosError's lifetime if it ever gets reclaimed.
    let bytes = unsafe { CStr::from_ptr(m).to_bytes().to_vec() };
    alloc_cstring(&bytes)
}

// ---------------------------------------------------------------
// Concat buffer — backing store for `__concat` / `format!`.
// Thread-local so `go { format!(...) }` calls don't trample
// each other.
// ---------------------------------------------------------------

thread_local! {
    static CONCAT_BUF: std::cell::RefCell<Vec<u8>> = std::cell::RefCell::new(Vec::with_capacity(256));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_init() {
    CONCAT_BUF.with(|b| {
        let mut buf = b.borrow_mut();
        buf.clear();
        // Bound the high-water mark: a one-time large `format!()`
        // result would otherwise pin the buffer's capacity at the
        // peak forever. 4 KiB is plenty for typical concat chains;
        // anything larger reallocates next time and shrinks again
        // here, returning the slack to the allocator.
        if buf.capacity() > 4096 {
            *buf = Vec::with_capacity(256);
        }
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_str(s: *const c_char) {
    if s.is_null() {
        return;
    }
    let bytes = unsafe { CStr::from_ptr(s).to_bytes() };
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(bytes));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_i64(n: i64) {
    let s = format!("{n}");
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(s.as_bytes()));
}

/// Appends an *unsigned* 64-bit integer to the concat buffer.
/// Used when the source TyKind is `u8/u16/u32/u64/u128/usize` so
/// values `>= 2^63` print as their true magnitude rather than the
/// sign-flipped two's-complement view a single `i64` printer would
/// produce.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_u64(n: u64) {
    let s = format!("{n}");
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(s.as_bytes()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_f64(x: f64) {
    let s = format!("{x}");
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(s.as_bytes()));
}

/// Appends `x` to the concat buffer with `prec` fractional digits.
/// Used by the `{:.N}` lowering when the surrounding `__concat`
/// pipeline can route the value directly without an intermediate
/// allocation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_f64_prec(x: f64, prec: i64) {
    let prec = prec.clamp(0, 64) as usize;
    let s = format!("{x:.prec$}");
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(s.as_bytes()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_bool(b: i32) {
    let s = if b != 0 { "true" } else { "false" };
    CONCAT_BUF.with(|buf| buf.borrow_mut().extend_from_slice(s.as_bytes()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_char(c: i32) {
    let ch = char::from_u32(c as u32).unwrap_or('\u{FFFD}');
    let s = ch.to_string();
    CONCAT_BUF.with(|b| b.borrow_mut().extend_from_slice(s.as_bytes()));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_concat_finish() -> *mut c_char {
    CONCAT_BUF.with(|b| {
        let buf = b.borrow();
        alloc_cstring(&buf)
    })
}

/// Returns the cause of `err` wrapped in an `Option<errors::Error>`
/// `GosResult` handle (`disc=0/Some` for non-null, `disc=1/None`
/// for null). Lets the match on `error.cause()` see a real
/// discriminant and terminate the cause-chain walk.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_cause(err: *const GosError) -> *mut GosResult {
    let cause = if err.is_null() {
        std::ptr::null_mut::<GosError>()
    } else {
        unsafe { (*err).cause }
    };
    let (disc, payload) = if cause.is_null() {
        (1, 0)
    } else {
        (0, cause as i64)
    };
    Box::into_raw(Box::new(GosResult { disc, payload }))
}

/// Walks the cause chain looking for a substring match.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_error_is(err: *const GosError, needle: *const c_char) -> bool {
    if err.is_null() || needle.is_null() {
        return false;
    }
    let Ok(needle) = (unsafe { CStr::from_ptr(needle).to_str() }) else {
        return false;
    };
    let mut cur = err;
    while !cur.is_null() {
        let m = unsafe { (*cur).message };
        if !m.is_null()
            && let Ok(text) = unsafe { CStr::from_ptr(m).to_str() }
            && text.contains(needle)
        {
            return true;
        }
        cur = unsafe { (*cur).cause };
    }
    false
}

// ---------------------------------------------------------------
// regex module — wraps the host `regex` crate with a c-ABI shim.
// Patterns compile lazily via `gos_rt_regex_compile`; matches /
// captures / replacements operate on `*const Regex` handles
// returned to user code as opaque `*mut GosRegex`.
// ---------------------------------------------------------------

#[repr(transparent)]
pub struct GosRegex {
    inner: regex::Regex,
}

unsafe impl Send for GosRegex {}
unsafe impl Sync for GosRegex {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_compile(pat: *const c_char) -> *mut GosRegex {
    if pat.is_null() {
        return std::ptr::null_mut();
    }
    let s = unsafe { CStr::from_ptr(pat).to_str() }.unwrap_or("");
    match regex::Regex::new(s) {
        Ok(re) => Box::into_raw(Box::new(GosRegex { inner: re })),
        Err(_) => std::ptr::null_mut(),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_is_match(re: *const GosRegex, text: *const c_char) -> bool {
    if re.is_null() || text.is_null() {
        return false;
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    unsafe { (*re).inner.is_match(s) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_find(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut c_char {
    if re.is_null() || text.is_null() {
        return alloc_cstring(b"");
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    match unsafe { (*re).inner.find(s) } {
        Some(m) => alloc_cstring(m.as_str().as_bytes()),
        None => alloc_cstring(b""),
    }
}

/// Finds every non-overlapping match of `re` in `text` and returns
/// a `Vec<String>` of the matched substrings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_find_all(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    // Each element is a 24-byte `(i64 start, i64 end, *c_char text)`
    // tuple. The previous 8-byte-per-element shape only stored the
    // matched text, leaving `hit.0` / `hit.1` reading garbage and
    // `hit.2` indexing past the end of the buffer (which the
    // example then printed as an empty string).
    let vec = unsafe { gos_rt_vec_new(24) };
    if re.is_null() || text.is_null() {
        return vec;
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    for m in unsafe { (*re).inner.find_iter(s) } {
        let cstr = alloc_cstring(m.as_str().as_bytes());
        #[repr(C)]
        struct Tup {
            start: i64,
            end: i64,
            text: i64,
        }
        let entry = Tup {
            start: m.start() as i64,
            end: m.end() as i64,
            text: cstr as i64,
        };
        unsafe {
            gos_rt_vec_push(vec, std::ptr::addr_of!(entry).cast::<u8>());
        }
    }
    vec
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_replace_all(
    re: *const GosRegex,
    text: *const c_char,
    repl: *const c_char,
) -> *mut c_char {
    if re.is_null() || text.is_null() {
        return alloc_cstring(b"");
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    let r = if repl.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(repl).to_str() }.unwrap_or("")
    };
    alloc_cstring(unsafe { (*re).inner.replace_all(s, r) }.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_split(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    let vec = unsafe { gos_rt_vec_new(8) };
    if re.is_null() || text.is_null() {
        return vec;
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    for piece in unsafe { (*re).inner.split(s) } {
        let cstr = alloc_cstring(piece.as_bytes());
        let ptr_val = cstr as i64;
        unsafe {
            gos_rt_vec_push(vec, std::ptr::addr_of!(ptr_val).cast::<u8>());
        }
    }
    vec
}

/// Returns `Vec<Vec<*c_char>>` — outer Vec has one entry per
/// match, inner Vec has one entry per group (group 0 = full
/// match, group 1+ = sub-captures). Missing groups are NULL
/// (which user code can pattern-match as `Option::None` because
/// the runtime treats null pointers as the zero discriminant).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_regex_captures_all(
    re: *const GosRegex,
    text: *const c_char,
) -> *mut GosVec {
    let outer = unsafe { gos_rt_vec_new(8) };
    if re.is_null() || text.is_null() {
        return outer;
    }
    let s = unsafe { CStr::from_ptr(text).to_str() }.unwrap_or("");
    for caps in unsafe { (*re).inner.captures_iter(s) } {
        let inner = unsafe { gos_rt_vec_new(8) };
        for i in 0..caps.len() {
            let ptr_val: i64 = match caps.get(i) {
                Some(m) => alloc_cstring(m.as_str().as_bytes()) as i64,
                None => 0,
            };
            unsafe {
                gos_rt_vec_push(inner, std::ptr::addr_of!(ptr_val).cast::<u8>());
            }
        }
        let inner_val = inner as i64;
        unsafe {
            gos_rt_vec_push(outer, std::ptr::addr_of!(inner_val).cast::<u8>());
        }
    }
    outer
}

// ---------------------------------------------------------------
// fs / path helpers — read_to_string, write, create_dir_all,
// path::join. Mirror Rust std::fs minus the typed Error.
// Filesystem syscalls are synchronous in every host kernel; the
// goroutine running these calls parks the OS worker for the
// kernel's duration. The scheduler's natural fan-out (one M per
// blocked goroutine, capped at `worker_count_cap`) keeps the
// run queue moving for callers that batch fs work in parallel.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_read_to_string(path: *const c_char) -> *mut c_char {
    if path.is_null() {
        return alloc_cstring(b"");
    }
    let p = unsafe { CStr::from_ptr(path).to_str() }.unwrap_or("");
    match std::fs::read_to_string(p) {
        Ok(text) => alloc_cstring(text.as_bytes()),
        Err(_) => alloc_cstring(b""),
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_write(path: *const c_char, contents: *const c_char) -> bool {
    if path.is_null() || contents.is_null() {
        return false;
    }
    let p = unsafe { CStr::from_ptr(path).to_str() }.unwrap_or("");
    let c = unsafe { CStr::from_ptr(contents).to_str() }.unwrap_or("");
    std::fs::write(p, c).is_ok()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_create_dir_all(path: *const c_char) -> bool {
    if path.is_null() {
        return false;
    }
    let p = unsafe { CStr::from_ptr(path).to_str() }.unwrap_or("");
    std::fs::create_dir_all(p).is_ok()
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_path_join(a: *const c_char, b: *const c_char) -> *mut c_char {
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
    let joined = std::path::Path::new(a).join(b);
    alloc_cstring(joined.to_string_lossy().as_bytes())
}

// ---------------------------------------------------------------
// bufio::Scanner — wraps a reader with a buffered line iterator.
// `Scanner::new(reader)` returns an opaque handle; `.scan()`
// advances to the next line and returns `true` when one was
// available; `.text()` returns the most recently scanned line.
// ---------------------------------------------------------------

pub struct GosScanner {
    lines: std::vec::IntoIter<String>,
    current: Option<String>,
}

unsafe impl Send for GosScanner {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bufio_scanner_new(
    stream: *mut std::ffi::c_void,
) -> *mut GosScanner {
    // Read the entire stream up front: cheap for the typical
    // CLI/file usage and avoids weaving a real Read trait
    // through the runtime.
    let text = if stream.is_null() {
        String::new()
    } else {
        // Re-use the stream-read-to-string helper: every stream
        // the runtime exposes is one of the io handles.
        let cstr = unsafe { gos_rt_stream_read_to_string(stream.cast::<GosStream>()) };
        if cstr.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(cstr).to_string_lossy().into_owned() }
        }
    };
    let lines: Vec<String> = text.lines().map(str::to_string).collect();
    Box::into_raw(Box::new(GosScanner {
        lines: lines.into_iter(),
        current: None,
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bufio_scanner_scan(s: *mut GosScanner) -> bool {
    if s.is_null() {
        return false;
    }
    let scanner = unsafe { &mut *s };
    if let Some(line) = scanner.lines.next() {
        scanner.current = Some(line);
        true
    } else {
        scanner.current = None;
        false
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bufio_scanner_text(s: *const GosScanner) -> *mut c_char {
    if s.is_null() {
        return alloc_cstring(b"");
    }
    let scanner = unsafe { &*s };
    match &scanner.current {
        Some(text) => alloc_cstring(text.as_bytes()),
        None => alloc_cstring(b""),
    }
}

// ---------------------------------------------------------------
// flag::Set — minimal CLI-flag parser. The compiled tier exposes
// a single mutable `*mut GosFlagSet` with `.string`, `.uint`,
// `.bool` registration and `.parse(args)`. Each registration
// returns a `*mut Cell<T>` so user code does `*name` to read
// the post-parse value.
// ---------------------------------------------------------------

pub struct GosFlagSet {
    name: String,
    specs: Vec<FlagSpec>,
    /// After `.parse()` runs, these hold the positional args left
    /// over. The handle returned via `gos_rt_flag_parse` is a
    /// `*mut GosVec` of c-string pointers.
    positional: Vec<String>,
}

struct FlagSpec {
    long_name: String,
    short: Option<char>,
    summary: String,
    kind: FlagKind,
    cell: *mut std::ffi::c_void,
}

#[derive(Debug, Clone)]
enum FlagKind {
    String,
    Int,
    Uint,
    Float,
    Bool,
    /// Duration cell stores `i64` milliseconds — same wire shape as
    /// `time::Duration` in the compiled tier.
    Duration,
    /// String-list cell stores `*mut GosVec` of c-string pointers.
    StringList,
}

unsafe impl Send for GosFlagSet {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_new(name: *const c_char) -> *mut GosFlagSet {
    let n = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
    };
    Box::into_raw(Box::new(GosFlagSet {
        name: n,
        specs: Vec::new(),
        positional: Vec::new(),
    }))
}

fn read_cstr(p: *const c_char) -> String {
    if p.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() }
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_string(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_v: *const c_char,
    help: *const c_char,
) -> *mut *mut c_char {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let dv = if default_v.is_null() {
        alloc_cstring(b"")
    } else {
        let bytes = unsafe { CStr::from_ptr(default_v).to_bytes().to_vec() };
        alloc_cstring(&bytes)
    };
    let cell = Box::into_raw(Box::new(dv));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::String,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_int(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_v: i64,
    help: *const c_char,
) -> *mut i64 {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let cell = Box::into_raw(Box::new(default_v));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::Int,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_uint(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_v: u64,
    help: *const c_char,
) -> *mut u64 {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let cell = Box::into_raw(Box::new(default_v));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::Uint,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_float(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_v: f64,
    help: *const c_char,
) -> *mut f64 {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let cell = Box::into_raw(Box::new(default_v));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::Float,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_bool(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_v: bool,
    help: *const c_char,
) -> *mut bool {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let cell = Box::into_raw(Box::new(default_v));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::Bool,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

/// Duration cell. `default_v` is interpreted as milliseconds (same
/// wire shape used by `time::Duration` in the compiled tier).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_duration(
    set: *mut GosFlagSet,
    name: *const c_char,
    default_ms: i64,
    help: *const c_char,
) -> *mut i64 {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let cell = Box::into_raw(Box::new(default_ms));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::Duration,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_string_list(
    set: *mut GosFlagSet,
    name: *const c_char,
    help: *const c_char,
) -> *mut *mut GosVec {
    if set.is_null() {
        return std::ptr::null_mut();
    }
    let n = read_cstr(name);
    let h = read_cstr(help);
    let backing = unsafe { gos_rt_vec_new(8) };
    let cell = Box::into_raw(Box::new(backing));
    let set = unsafe { &mut *set };
    set.specs.push(FlagSpec {
        long_name: n,
        short: None,
        summary: h,
        kind: FlagKind::StringList,
        cell: cell.cast::<std::ffi::c_void>(),
    });
    cell
}

/// Attaches a one-character short alias to the most recently
/// registered flag — mirrors `Set::short` in `gossamer-std`.
/// `letter` is passed as i64 to match how single-char literals
/// flow through the compiled-tier C ABI.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_short(set: *mut GosFlagSet, letter: i64) {
    if set.is_null() {
        return;
    }
    let set = unsafe { &mut *set };
    let Some(ch) = u32::try_from(letter).ok().and_then(char::from_u32) else {
        return;
    };
    if let Some(last) = set.specs.last_mut() {
        last.short = Some(ch);
    }
}

/// Returns the auto-generated usage string as a heap-allocated
/// c-string. Matches `gossamer-std::flag::Set::usage`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_usage(set: *const GosFlagSet) -> *mut c_char {
    if set.is_null() {
        return alloc_cstring(b"");
    }
    let set = unsafe { &*set };
    let bytes = render_flag_usage(set).into_bytes();
    alloc_cstring(&bytes)
}

fn render_flag_usage(set: &GosFlagSet) -> String {
    let program = if set.name.is_empty() {
        "program"
    } else {
        &set.name
    };
    let mut out = format!("usage: {program} [FLAGS] [POSITIONAL]\n\nflags:\n");
    for def in &set.specs {
        let label = match def.short {
            Some(ch) => format!("  -{ch}, --{}", def.long_name),
            None => format!("      --{}", def.long_name),
        };
        out.push_str(&format!("{label:<30} {}\n", def.summary));
    }
    out
}

fn parse_duration_text(text: &str) -> Option<i64> {
    let text = text.trim();
    if let Some(rest) = text.strip_suffix("ms") {
        return rest.parse::<i64>().ok();
    }
    if let Some(rest) = text.strip_suffix("us") {
        return rest.parse::<i64>().ok().map(|n| n / 1_000);
    }
    if let Some(rest) = text.strip_suffix("ns") {
        return rest.parse::<i64>().ok().map(|n| n / 1_000_000);
    }
    if let Some(rest) = text.strip_suffix("s") {
        return rest.parse::<i64>().ok().map(|n| n * 1_000);
    }
    if let Some(rest) = text.strip_suffix("m") {
        return rest.parse::<i64>().ok().map(|n| n * 60_000);
    }
    if let Some(rest) = text.strip_suffix("h") {
        return rest.parse::<i64>().ok().map(|n| n * 3_600_000);
    }
    text.parse::<i64>().ok().map(|n| n * 1_000)
}

fn parse_bool_text(text: &str) -> Option<bool> {
    match text {
        "true" | "1" | "yes" | "on" => Some(true),
        "false" | "0" | "no" | "off" => Some(false),
        _ => None,
    }
}

/// Resolves an explicit-or-following value for `spec` and writes
/// it into the spec's cell. Returns the number of argv tokens
/// consumed (1 for `--name=value`, `--bool`, `-v`; 2 for
/// `--name value`).
fn apply_flag_value(
    spec: &mut FlagSpec,
    explicit: Option<String>,
    get_arg_ptr: &dyn Fn(i64) -> *const c_char,
    idx: i64,
    argc: i64,
) -> i64 {
    // Bool with no explicit value is a "set true" form.
    if matches!(spec.kind, FlagKind::Bool) && explicit.is_none() {
        unsafe {
            *(spec.cell.cast::<bool>()) = true;
        }
        return 1;
    }
    let (raw, consumed) = if let Some(v) = explicit {
        (v, 1)
    } else {
        if idx + 1 >= argc {
            return 1;
        }
        let p = get_arg_ptr(idx + 1);
        if p.is_null() {
            return 1;
        }
        let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
        (s, 2)
    };
    match spec.kind {
        FlagKind::String => {
            let bytes = raw.as_bytes().to_vec();
            let leaked = alloc_cstring(&bytes);
            unsafe {
                *(spec.cell.cast::<*mut c_char>()) = leaked;
            }
        }
        FlagKind::Int => {
            if let Ok(n) = raw.parse::<i64>() {
                unsafe {
                    *(spec.cell.cast::<i64>()) = n;
                }
            }
        }
        FlagKind::Uint => {
            if let Ok(n) = raw.parse::<u64>() {
                unsafe {
                    *(spec.cell.cast::<u64>()) = n;
                }
            }
        }
        FlagKind::Float => {
            if let Ok(x) = raw.parse::<f64>() {
                unsafe {
                    *(spec.cell.cast::<f64>()) = x;
                }
            }
        }
        FlagKind::Bool => {
            if let Some(b) = parse_bool_text(&raw) {
                unsafe {
                    *(spec.cell.cast::<bool>()) = b;
                }
            }
        }
        FlagKind::Duration => {
            if let Some(ms) = parse_duration_text(&raw) {
                unsafe {
                    *(spec.cell.cast::<i64>()) = ms;
                }
            }
        }
        FlagKind::StringList => {
            let bytes = raw.as_bytes().to_vec();
            let cstr = alloc_cstring(&bytes);
            let ptr_val = cstr as i64;
            let backing = unsafe { *(spec.cell.cast::<*mut GosVec>()) };
            if !backing.is_null() {
                unsafe {
                    gos_rt_vec_push(backing, std::ptr::addr_of!(ptr_val).cast::<u8>());
                }
            }
        }
    }
    consumed
}

/// Parses GNU-style `--name value` and `--bool` flags out of
/// `args` (a `*mut GosVec` of c-string pointers from
/// `os::args()`), filling in each registered cell. Returns a
/// `*mut GosVec` of the leftover positional arguments.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_set_parse(
    set: *mut GosFlagSet,
    args: *const GosVec,
) -> *mut GosVec {
    if set.is_null() {
        return unsafe { gos_rt_vec_new(8) };
    }
    let set = unsafe { &mut *set };
    set.positional.clear();
    if args.is_null() {
        return unsafe { gos_rt_vec_new(8) };
    }
    // Two callers reach this function: the runner-build path
    // passes a real `*mut GosVec` of c-string pointers; the
    // compiled path passes the `os::args()` sentinel — a raw
    // `argv + 1` pointer with `argc - 1` length stashed in the
    // process-global ARGS_PTR / ARGS_LEN. Detect the sentinel by
    // pointer-equality and route to a separate iteration path
    // that walks `argv` directly. Without this branch the code
    // tries to read a GosVec header out of an argv pointer and
    // segfaults on the first positional arg.
    let sentinel_ptr = ARGS_PTR.load(Ordering::SeqCst);
    let is_sentinel = sentinel_ptr != 0 && (args as usize) == sentinel_ptr;
    let (argc, start_i, get_arg_ptr): (i64, i64, Box<dyn Fn(i64) -> *const c_char>) = if is_sentinel
    {
        let argv = sentinel_ptr as *const *const c_char;
        let len = ARGS_LEN.load(Ordering::SeqCst);
        let getter: Box<dyn Fn(i64) -> *const c_char> =
            Box::new(move |i: i64| unsafe { *argv.add(i as usize) });
        (len, 0, getter)
    } else {
        let v = args;
        let len = unsafe { gos_rt_vec_len(v) };
        let getter: Box<dyn Fn(i64) -> *const c_char> = Box::new(move |i: i64| unsafe {
            let p = gos_rt_vec_get_ptr(v, i);
            if p.is_null() {
                std::ptr::null()
            } else {
                p.cast::<*const c_char>().read_unaligned()
            }
        });
        (len, 1, getter) // GosVec form: skip argv[0]
    };
    let mut i = start_i;
    while i < argc {
        let arg_ptr = get_arg_ptr(i);
        let arg = if arg_ptr.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(arg_ptr).to_string_lossy().into_owned() }
        };
        if arg == "--" {
            i += 1;
            while i < argc {
                let p = get_arg_ptr(i);
                if !p.is_null() {
                    let s = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
                    set.positional.push(s);
                }
                i += 1;
            }
            break;
        }
        if arg == "--help" || arg == "-h" {
            print!("{}", render_flag_usage(set));
            std::process::exit(0);
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, explicit) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if let Some(spec) = set.specs.iter_mut().find(|s| s.long_name == name) {
                let consumed = apply_flag_value(spec, explicit, &get_arg_ptr, i, argc);
                i += consumed;
                continue;
            }
            set.positional.push(arg);
            i += 1;
            continue;
        }
        if let Some(rest) = arg.strip_prefix('-')
            && !rest.is_empty()
        {
            let mut chars = rest.chars();
            let first = chars.next().unwrap();
            let remainder: String = chars.collect();
            if let Some(spec) = set.specs.iter_mut().find(|s| s.short == Some(first)) {
                let explicit = if remainder.is_empty() {
                    None
                } else if let Some(stripped) = remainder.strip_prefix('=') {
                    Some(stripped.to_string())
                } else {
                    Some(remainder.clone())
                };
                let consumed = apply_flag_value(spec, explicit, &get_arg_ptr, i, argc);
                i += consumed;
                continue;
            }
        }
        set.positional.push(arg);
        i += 1;
    }
    let out = unsafe { gos_rt_vec_with_capacity(8, set.positional.len() as i64) };
    for s in &set.positional {
        let bytes = s.as_bytes();
        let cstr = alloc_cstring(bytes);
        let ptr_val = cstr as i64;
        unsafe {
            gos_rt_vec_push(out, std::ptr::addr_of!(ptr_val).cast::<u8>());
        }
    }
    out
}

// ---------------------------------------------------------------
// HTTP client — minimal Builder pattern returning Response with
// `status` (i64) + `body` (String). Backed by a small synchronous
// HTTP/1.1 implementation to avoid pulling a TLS stack into the
// runtime crate.
// ---------------------------------------------------------------

pub struct GosHttpClient {
    _placeholder: u8,
}

pub struct GosHttpRequest {
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

pub struct GosHttpResponse {
    pub status: i64,
    pub body: *mut c_char,
    pub headers: Vec<(String, String)>,
}

unsafe impl Send for GosHttpClient {}
unsafe impl Send for GosHttpRequest {}
unsafe impl Send for GosHttpResponse {}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_new() -> *mut GosHttpClient {
    Box::into_raw(Box::new(GosHttpClient { _placeholder: 0 }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_get(
    _client: *mut GosHttpClient,
    url: *const c_char,
) -> *mut GosHttpRequest {
    let url = if url.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
    };
    Box::into_raw(Box::new(GosHttpRequest {
        method: "GET".to_string(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_client_post(
    _client: *mut GosHttpClient,
    url: *const c_char,
) -> *mut GosHttpRequest {
    let url = if url.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(url).to_string_lossy().into_owned() }
    };
    Box::into_raw(Box::new(GosHttpRequest {
        method: "POST".to_string(),
        url,
        headers: Vec::new(),
        body: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_header(
    req: *mut GosHttpRequest,
    name: *const c_char,
    value: *const c_char,
) -> *mut GosHttpRequest {
    if req.is_null() {
        return req;
    }
    let n = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
    };
    let v = if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
    };
    unsafe { (*req).headers.push((n, v)) };
    req
}

/// Mutating header insert used by the chained `req.headers.insert`
/// lowering (return-by-receiver kept off so the call has no value).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_set_header(
    req: *mut GosHttpRequest,
    name: *const c_char,
    value: *const c_char,
) {
    if req.is_null() {
        return;
    }
    let n = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
    };
    let v = if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
    };
    let req = unsafe { &mut *req };
    req.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&n));
    req.headers.push((n, v));
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_get_header(
    req: *const GosHttpRequest,
    name: *const c_char,
) -> *mut c_char {
    if req.is_null() || name.is_null() {
        return alloc_cstring(b"");
    }
    let n = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
    let req = unsafe { &*req };
    let found = req
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&n))
        .map_or(String::new(), |(_, v)| v.clone());
    alloc_cstring(found.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_body(
    req: *mut GosHttpRequest,
    body: *const c_char,
) -> *mut GosHttpRequest {
    if req.is_null() {
        return req;
    }
    let b = if body.is_null() {
        Vec::new()
    } else {
        unsafe { CStr::from_ptr(body).to_bytes().to_vec() }
    };
    unsafe { (*req).body = b };
    req
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_send(
    req: *mut GosHttpRequest,
) -> *mut GosHttpResponse {
    if req.is_null() {
        return Box::into_raw(Box::new(GosHttpResponse {
            status: 0,
            body: alloc_cstring(b""),
            headers: Vec::new(),
        }));
    }
    let req = unsafe { Box::from_raw(req) };
    let (status, body_bytes) = http_request_ureq(&req).unwrap_or((0, Vec::new()));
    let body = alloc_cstring(&body_bytes);
    Box::into_raw(Box::new(GosHttpResponse {
        status,
        body,
        headers: Vec::new(),
    }))
}

fn http_request_ureq(req: &GosHttpRequest) -> Option<(i64, Vec<u8>)> {
    if req.method.eq_ignore_ascii_case("GET") && req.headers.is_empty() && req.body.is_empty() {
        return http_get_follow_redirects(&req.url).ok();
    }
    None
}

fn http_get_follow_redirects(url: &str) -> Result<(i64, Vec<u8>), String> {
    let mut current = url.to_string();
    for _ in 0..6 {
        let (status, body, location) = if current.starts_with("https://") {
            http_get_tls(&current)?
        } else {
            http_get_plain(&current)?
        };
        if !(300..=399).contains(&status) || location.is_empty() {
            return Ok((status, body));
        }
        current = absolute_redirect(&current, &location);
    }
    Err(format!("too many redirects fetching `{url}`"))
}

fn absolute_redirect(from: &str, location: &str) -> String {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.to_string();
    }
    let scheme_end = from.find("://").map_or(0, |i| i + 3);
    let host_end = from[scheme_end..]
        .find('/')
        .map_or(from.len(), |i| scheme_end + i);
    if location.starts_with('/') {
        format!("{}{}", &from[..host_end], location)
    } else {
        format!("{}/{}", &from[..host_end], location)
    }
}

fn http_get_tls(url: &str) -> Result<(i64, Vec<u8>, String), String> {
    use gossamer_pkg::transport::{HttpsTransport, Transport};

    let transport = HttpsTransport::new_mozilla_roots();
    let body = transport.get(url).map_err(|e| format!("{e}"))?;
    Ok((200, body, String::new()))
}

fn http_get_plain(url: &str) -> Result<(i64, Vec<u8>, String), String> {
    let (host, path) = parse_http_get_url(url).ok_or_else(|| format!("unsupported URL: {url}"))?;
    let (host_part, port) = match host.split_once(':') {
        Some((h, p)) => (h, p),
        None => (host.as_str(), "80"),
    };
    let port_num = port
        .parse::<u16>()
        .map_err(|_| format!("bad port in URL: {url}"))?;
    let mut stream = connect_host_port(host_part, port_num)
        .map_err(|e| format!("connect {host_part}:{port}: {e}"))?;
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(30)))
        .ok();
    let request = format!(
        "GET {path} HTTP/1.1\r\nHost: {host_part}\r\nUser-Agent: gos/{version}\r\nAccept: */*\r\nConnection: close\r\n\r\n",
        version = env!("CARGO_PKG_VERSION"),
    );
    std::io::Write::write_all(&mut stream, request.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut response = Vec::new();
    std::io::Read::read_to_end(&mut stream, &mut response).map_err(|e| format!("read: {e}"))?;
    let response_str = String::from_utf8_lossy(&response);
    let Some((header_block, body)) = response_str.split_once("\r\n\r\n") else {
        return Err("invalid HTTP response".to_string());
    };
    let status_line = header_block.lines().next().unwrap_or("");
    let status = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse::<i64>().ok())
        .unwrap_or(0);
    let mut location = String::new();
    for hline in header_block.lines().skip(1) {
        if let Some((name, value)) = hline.split_once(':')
            && name.trim().eq_ignore_ascii_case("location")
        {
            location = value.trim().to_string();
            break;
        }
    }
    Ok((status, body.as_bytes().to_vec(), location))
}

fn parse_http_get_url(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?;
    let (host, path) = match rest.split_once('/') {
        Some((h, p)) => (h.to_string(), format!("/{p}")),
        None => (rest.to_string(), "/".to_string()),
    };
    Some((host, path))
}

#[cfg(unix)]
fn connect_host_port(host: &str, port: u16) -> std::io::Result<std::net::TcpStream> {
    use std::mem::MaybeUninit;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

    let host_c = std::ffi::CString::new(host)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "host contains NUL"))?;
    let port_c = std::ffi::CString::new(port.to_string())
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "port contains NUL"))?;
    let hints = MaybeUninit::<libc::addrinfo>::zeroed();
    // SAFETY: zeroed `addrinfo` is a valid base to fill selected fields.
    let mut hints = unsafe { hints.assume_init() };
    hints.ai_family = libc::AF_UNSPEC;
    hints.ai_socktype = libc::SOCK_STREAM;
    let mut out: *mut libc::addrinfo = std::ptr::null_mut();
    // SAFETY: pointers stay valid for the call; `out` is written by libc.
    let rc = unsafe {
        libc::getaddrinfo(
            host_c.as_ptr(),
            port_c.as_ptr(),
            &raw const hints,
            &raw mut out,
        )
    };
    if rc != 0 {
        let msg = unsafe { CStr::from_ptr(libc::gai_strerror(rc)) }
            .to_string_lossy()
            .into_owned();
        return Err(std::io::Error::other(msg));
    }
    let mut cursor = out;
    let mut last_err = None;
    while !cursor.is_null() {
        // SAFETY: `cursor` comes from the valid `addrinfo` chain returned by libc.
        let ai = unsafe { &*cursor };
        let addr = match ai.ai_family {
            libc::AF_INET => {
                // SAFETY: ai_family says this is `sockaddr_in`.
                let sin = unsafe { &*(ai.ai_addr.cast::<libc::sockaddr_in>()) };
                let ip = Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                Some(SocketAddr::new(IpAddr::V4(ip), u16::from_be(sin.sin_port)))
            }
            libc::AF_INET6 => {
                // SAFETY: ai_family says this is `sockaddr_in6`.
                let sin6 = unsafe { &*(ai.ai_addr.cast::<libc::sockaddr_in6>()) };
                let ip = Ipv6Addr::from(sin6.sin6_addr.s6_addr);
                Some(SocketAddr::new(
                    IpAddr::V6(ip),
                    u16::from_be(sin6.sin6_port),
                ))
            }
            _ => None,
        };
        if let Some(addr) = addr {
            match std::net::TcpStream::connect(addr) {
                Ok(stream) => {
                    // SAFETY: `out` was allocated by libc on successful `getaddrinfo`.
                    unsafe { libc::freeaddrinfo(out) };
                    return Ok(stream);
                }
                Err(err) => last_err = Some(err),
            }
        }
        cursor = ai.ai_next;
    }
    // SAFETY: `out` was allocated by libc on successful `getaddrinfo`.
    unsafe { libc::freeaddrinfo(out) };
    Err(last_err.unwrap_or_else(|| std::io::Error::other("no socket addresses resolved")))
}

#[cfg(not(unix))]
fn connect_host_port(host: &str, port: u16) -> std::io::Result<std::net::TcpStream> {
    std::net::TcpStream::connect((host, port))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_query(req: *const GosHttpRequest) -> *mut c_char {
    if req.is_null() {
        return alloc_cstring(b"");
    }
    // Naive query extraction: everything after the first `?`
    // in the URL (without the leading `?`).
    let url = &unsafe { &*req }.url;
    if let Some(pos) = url.find('?') {
        alloc_cstring(&url.as_bytes()[pos + 1..])
    } else {
        alloc_cstring(b"")
    }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_body_str(req: *const GosHttpRequest) -> *mut c_char {
    if req.is_null() {
        return alloc_cstring(b"");
    }
    alloc_cstring(&unsafe { &*req }.body)
}

/// Returns the request's URL path (the part after the host).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_path(req: *const GosHttpRequest) -> *mut c_char {
    if req.is_null() {
        return alloc_cstring(b"");
    }
    let r = unsafe { &*req };
    let path = if let Some(rest) = r
        .url
        .strip_prefix("http://")
        .or_else(|| r.url.strip_prefix("https://"))
    {
        match rest.find('/') {
            Some(i) => &rest[i..],
            None => "/",
        }
    } else {
        r.url.as_str()
    };
    alloc_cstring(path.as_bytes())
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_request_method(req: *const GosHttpRequest) -> *mut c_char {
    if req.is_null() {
        return alloc_cstring(b"");
    }
    alloc_cstring(unsafe { &*req }.method.as_bytes())
}

/// Constructs a 200-style text response. Writes into the
/// per-thread response buffer (`RESPONSE_BUF`) — the previous
/// `Box::into_raw` per request was the dominant overhead at
/// conc=100. The body pointer is stored verbatim: it's already
/// valid arena/static memory (string literals live for the
/// program; `format!()` output lives until the next
/// `gos_rt_gc_reset`, which runs *after* the response is written
/// to the socket). Skipping the copy removes another two
/// allocations per request.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_text_new(
    status: i64,
    body: *const c_char,
) -> *mut GosHttpResponse {
    // Box-allocate per request rather than reusing a per-thread
    // buffer. The thread-local optimization saved a malloc/free
    // pair, but exposed a subtle aliasing hazard under concurrent
    // load: when many connection threads exit in rapid succession,
    // the TLS-owned `headers: Vec<(String, String)>` had its drop
    // path running concurrently with whatever code happened to be
    // using the response pointer. Switching to Box::into_raw +
    // Box::from_raw makes ownership explicit — `drop_handler_result`
    // is the unique reclaim site.
    Box::into_raw(Box::new(GosHttpResponse {
        status,
        body: body.cast_mut(),
        headers: Vec::new(),
    }))
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_json_new(
    status: i64,
    body: *const c_char,
) -> *mut GosHttpResponse {
    unsafe { gos_rt_http_response_text_new(status, body) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_status(resp: *const GosHttpResponse) -> i64 {
    if resp.is_null() {
        return 0;
    }
    unsafe { (*resp).status }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_body(resp: *const GosHttpResponse) -> *mut c_char {
    if resp.is_null() {
        return alloc_cstring(b"");
    }
    unsafe { (*resp).body }
}

/// Sets `Header: Value` on a response, replacing any prior value
/// with the same case-insensitive name. Used by the chained
/// `r.headers.insert(name, value)` lowering.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_set_header(
    resp: *mut GosHttpResponse,
    name: *const c_char,
    value: *const c_char,
) {
    if resp.is_null() {
        return;
    }
    let n = if name.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() }
    };
    let v = if value.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(value).to_string_lossy().into_owned() }
    };
    let resp = unsafe { &mut *resp };
    resp.headers.retain(|(k, _)| !k.eq_ignore_ascii_case(&n));
    resp.headers.push((n, v));
}

/// Reads `Header` value from a response, empty string when absent.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_http_response_get_header(
    resp: *const GosHttpResponse,
    name: *const c_char,
) -> *mut c_char {
    if resp.is_null() || name.is_null() {
        return alloc_cstring(b"");
    }
    let n = unsafe { CStr::from_ptr(name).to_string_lossy().into_owned() };
    let resp = unsafe { &*resp };
    let found = resp
        .headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(&n))
        .map_or(String::new(), |(_, v)| v.clone());
    alloc_cstring(found.as_bytes())
}

// ---------------------------------------------------------------
// testing module — minimal `check`, `check_eq`, `check_ok` that
// log to stderr. Real test discovery / reporting is done via the
// interpreter today; these stubs make the example compile.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_testing_check(cond: bool, msg: *const c_char) -> bool {
    if !cond {
        let m = if msg.is_null() {
            "check failed".to_string()
        } else {
            unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
        };
        eprintln!("test check failed: {m}");
    }
    cond
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_testing_check_eq_i64(a: i64, b: i64, msg: *const c_char) -> bool {
    let ok = a == b;
    if !ok {
        let m = if msg.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(msg).to_string_lossy().into_owned() }
        };
        eprintln!("test check_eq failed: {a} != {b} ({m})");
    }
    ok
}

// ---------------------------------------------------------------
// gzip module — encode / decode using `flate2`.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gzip_encode(data: *const c_char) -> *mut c_char {
    if data.is_null() {
        return alloc_cstring(b"");
    }
    let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
    use std::io::Write;
    let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    if enc.write_all(bytes).is_err() {
        return alloc_cstring(b"");
    }
    let buf = enc.finish().unwrap_or_default();
    alloc_cstring(&buf)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_gzip_decode(data: *const c_char) -> *mut c_char {
    if data.is_null() {
        return alloc_cstring(b"");
    }
    let bytes = unsafe { CStr::from_ptr(data).to_bytes() };
    use std::io::Read;
    let mut dec = flate2::read::GzDecoder::new(bytes);
    let mut out = Vec::new();
    if dec.read_to_end(&mut out).is_err() {
        return alloc_cstring(b"");
    }
    alloc_cstring(&out)
}

// ---------------------------------------------------------------
// slog — simple stderr logger.
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_info(msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    eprintln!("INFO: {m}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_warn(msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    eprintln!("WARN: {m}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_error(msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    eprintln!("ERROR: {m}");
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_slog_debug(msg: *const c_char) {
    if msg.is_null() {
        return;
    }
    let m = unsafe { CStr::from_ptr(msg).to_string_lossy() };
    eprintln!("DEBUG: {m}");
}

#[cfg(test)]
mod map_iter_tests {
    use super::*;

    #[test]
    fn map_keys_i64_snapshots_inserted_keys() {
        unsafe {
            let m = gos_rt_map_new(8, 8);
            gos_rt_map_insert_i64_i64(m, 1, 100);
            gos_rt_map_insert_i64_i64(m, 2, 200);
            gos_rt_map_insert_i64_i64(m, 3, 50);
            assert_eq!(gos_rt_map_len(m), 3);
            let v = gos_rt_map_keys_i64(m);
            assert_eq!(gos_rt_vec_len(v), 3);
            let mut keys: Vec<i64> = (0..gos_rt_vec_len(v))
                .map(|i| {
                    let p = gos_rt_vec_get_ptr(v, i);
                    (p as *const i64).read_unaligned()
                })
                .collect();
            keys.sort_unstable();
            assert_eq!(keys, vec![1, 2, 3]);
        }
    }
}
