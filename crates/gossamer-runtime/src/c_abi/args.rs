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
use std::os::raw::{c_char, c_int};
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use super::*;

// ---------------------------------------------------------------
// Process-wide argv view
// ---------------------------------------------------------------
//
// `os::args()` is supposed to behave like a `Vec<String>`:
// `.len()` is the user-arg count and `args[i]` is the i-th user
// arg as a String. We expose two views:
//
//   - the raw `argv + 1` pointer, stored in `ARGS_PTR`, used by
//     the flat-codegen Place projection with stride 8 (the
//     legacy Var-typed callers); `gos_rt_arr_len` detects that
//     pointer and short-circuits to `argc - 1`.
//   - a real `*mut GosVec` whose backing `ptr` is `argv + 1` and
//     whose `len`/`cap` are `argc - 1`, returned by
//     `gos_rt_os_args` itself. Pinning `os::args()` to
//     `Vec<String>` at MIR lowering then makes `args[i].len()`
//     dispatch through `gos_rt_str_len` instead of falling into
//     the generic `gos_rt_len` (which reads a Vec header out of
//     a `*const c_char` pointer and crashes when the leading
//     bytes don't form a valid length).

pub static ARGS_PTR: AtomicUsize = AtomicUsize::new(0);
pub static ARGS_LEN: AtomicI64 = AtomicI64::new(0);
static ARGS_VEC: AtomicUsize = AtomicUsize::new(0);
// Pointer to the program name string. Set from argv[0] in
// `gos_rt_set_args`, or overridden via `gos_rt_set_program_name`.
// Lifetime: either the OS-owned argv[0] (process-lifetime), or a
// leaked CString allocated by `gos_rt_set_program_name`.
static PROGRAM_NAME_PTR: AtomicUsize = AtomicUsize::new(0);

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_args(argc: c_int, argv: *const *const c_char) {
    ffi_entry!((), {
        // Capture argv[0] as the program name whenever argv has any
        // entries — previously this only happened when argc > 1, so
        // a binary run with no user args had `env::program_name()`
        // return null and stringify to an empty string.
        if argc >= 1 && !argv.is_null() {
            // SAFETY: libc guarantees argv[0..argc] is valid when
            // argc >= 1. The pointer at `*argv` is the program name.
            let name_ptr = unsafe { *argv };
            if !name_ptr.is_null() {
                // Copy argv[0] into a gos-allocated (tagged, refcounted) string.
                // argv is libc-owned and untagged; passing it raw to the RC
                // retain/release dispatch would mis-read its byte-before-pointer
                // as an RC header. The stored copy holds the base reference for
                // the process lifetime (I4: string boundaries own their values).
                let bytes = unsafe { CStr::from_ptr(name_ptr).to_bytes() };
                let owned = alloc_cstring(bytes);
                PROGRAM_NAME_PTR.store(owned as usize, Ordering::SeqCst);
            }
        }
        if argc > 1 && !argv.is_null() {
            // SAFETY: libc guarantees argv[0..argc] is valid when
            // argc > 0. `argv + 1` therefore addresses `argc - 1`
            // strings.

            let user_argv = unsafe { argv.add(1) };
            let len = i64::from(argc - 1);
            ARGS_PTR.store(user_argv as usize, Ordering::SeqCst);
            ARGS_LEN.store(len, Ordering::SeqCst);
            // Expose `os::args()` as a real `*mut GosVec` so the
            // compiled tier can index it with the standard
            // `header.ptr + i * elem_bytes` shape and dispatch
            // `args[i].len()` to `gos_rt_str_len` once `os::args`'s
            // return type is pinned to `Vec<String>`.
            //
            // `cap = 0` marks the data buffer as borrowed (libc owns
            // `argv`), so `gos_rt_vec_free` skips its dealloc arm
            // when GC sweep walks across this header — `len` alone is
            // enough for `args.len()` (read at offset 0) and indexing
            // (which only touches `ptr` and `elem_bytes`).
            let vec = Box::into_raw(Box::new(GosVec {
                len,
                cap: 0,
                elem_bytes: 8,
                elem_kind: vec_elem_kind::PRIMITIVE,
                _reserved: [0; 3],
                ptr: SyncRawPtr::new(user_argv as *mut u8),
            }));
            ARGS_VEC.store(vec as usize, Ordering::SeqCst);
        } else {
            ARGS_PTR.store(0, Ordering::SeqCst);
            ARGS_LEN.store(0, Ordering::SeqCst);
            // Even when there are no user args, expose a valid
            // empty `GosVec` so callers iterating `for a in
            // env::args()` see len=0 instead of dereferencing a
            // null header. The previous null sentinel segfaulted on
            // the iterator's `header.ptr + 0 * elem_bytes` walk.
            let vec = Box::into_raw(Box::new(GosVec {
                len: 0,
                cap: 0,
                elem_bytes: 8,
                elem_kind: vec_elem_kind::PRIMITIVE,
                _reserved: [0; 3],
                ptr: SyncRawPtr::new(std::ptr::null_mut()),
            }));
            ARGS_VEC.store(vec as usize, Ordering::SeqCst);
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
    });
}

/// Returns freed pages to the OS promptly by setting mimalloc's purge
/// delay to zero. mimalloc's default (1000 ms in v3) defers the
/// `madvise` purge to batch it; on a phase-structured program — build a
/// large map, drop it, build the next — every dropped phase's pages stay
/// resident until process exit, so peak RSS becomes the SUM of all
/// phases instead of the largest live set (measured: k-nucleotide
/// `--release` 52.6 MB -> 28.8 MB, wall-clock unchanged). Delegates to
/// the single implementation in the crate root; the option index and
/// rationale live there.
fn configure_allocator() {
    crate::init_process_allocator();
}

#[cfg(unix)]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        configure_allocator();
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
        configure_allocator();
        let handle = std::thread::Builder::new()
            .name("gos-rt-init".to_string())
            .spawn(|| {})
            .expect("spawn rt init thread");
        let _ = handle.join();
    });
}

/// Returns a `*mut GosVec` view of the user arguments. The
/// header's `ptr` is `argv + 1` and `len`/`cap` are `argc - 1`,
/// so `args.len()` dispatches through `gos_rt_arr_len` (reading
/// `len` at offset 0) and `args[i]` reads the i-th `*const c_char`
/// through the GosVec `ptr` field — same shape as any other
/// `Vec<String>`. `gos_rt_arr_len`'s legacy `argv + 1` sentinel
/// short-circuit is retained for callers that still hold the raw
/// pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_args() -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        ARGS_VEC.load(Ordering::SeqCst) as *mut GosVec
    })
}

/// Overrides the program name returned by `os::program_name()`.
/// The interpreter calls this via `gos_rt_set_program_name` when it
/// knows the script path (e.g. `gos run examples/cat.gos`). The
/// provided string is copied into a leaked `CString` so the pointer
/// is process-lifetime safe.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_set_program_name(name: *const c_char) {
    ffi_entry!((), {
        if name.is_null() {
            return;
        }
        // SAFETY: caller guarantees `name` is a valid NUL-terminated string.
        let bytes = unsafe { CStr::from_ptr(name).to_bytes() };
        let owned = alloc_cstring(bytes);
        PROGRAM_NAME_PTR.store(owned as usize, Ordering::SeqCst);
    });
}

/// Returns the program name as a `*const c_char` (`argv[0]` for native
/// binaries; the script path for `gos run`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_program_name() -> *const c_char {
    ffi_entry!(std::ptr::null(), {
        PROGRAM_NAME_PTR.load(Ordering::SeqCst) as *const c_char
    })
}

/// `env::temp_dir() -> String`. Returns the platform temp directory:
/// `/tmp` on Linux, `$TMPDIR` on macOS, `%TEMP%`/`%USERPROFILE%\AppData\Local\Temp`
/// on Windows. Mirrors Rust's `std::env::temp_dir`; the returned
/// pointer is GC-managed and lives for the process lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_env_temp_dir() -> *const c_char {
    ffi_entry!(std::ptr::null(), {
        let path = std::env::temp_dir();
        let bytes = path.to_string_lossy();
        alloc_cstring(bytes.as_bytes()).cast_const()
    })
}

/// `env::home_dir() -> Option<String>`. Returns `Some(path)` when
/// the user has a home directory; `None` otherwise. The Result
/// payload's disc-0/disc-1 convention mirrors `gos_rt_os_env` so
/// `if let Some(h) = env::home_dir()` works the same way.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_env_home_dir() -> i128 {
    ffi_entry!(0i128, {
        #[allow(deprecated)]
        match std::env::home_dir() {
            Some(path) => {
                let bytes = path.to_string_lossy();
                let cs = alloc_cstring(bytes.as_bytes());
                unsafe { gos_rt_result_new(0, cs as i64) }
            }
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `os::env(name) -> Option<String>`. Compiled tier returns a
/// `*mut GosResult` shaped as Option (disc 0 = Some, 1 = None).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_env(name: *const c_char) -> i128 {
    ffi_entry!(0i128, {
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
    })
}

/// `os::cwd() -> Result<String, errors::Error>`. Compiled tier
/// returns a `*mut GosResult` (disc 0 = Ok, 1 = Err).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_cwd() -> i128 {
    ffi_entry!(0i128, {
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
    })
}

/// `fs::list_dir(path) -> Result<[DirInfo], errors::Error>`.
/// Each `DirInfo` is a 7-slot heap aggregate matching the
/// interpreter's struct field order:
/// `[name: *c_char, path: *c_char, is_file: i64, is_dir: i64,
/// is_symlink: i64, size: i64, modified_ms: i64]`. Field
/// indices match the MIR projections emitted for
/// `entry.<field>` access.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_list_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
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
            let entry_path = entry.path();
            // Use std::fs::metadata (opens a handle) rather than entry.metadata()
            // (reads from FindFile cache on Windows). The latter returns 0 for
            // directory sizes on Windows because WIN32_FIND_DATA stores nFileSize=0
            // for directories; the former calls GetFileInformationByHandle and
            // returns the real NTFS directory-index allocation, matching what the
            // interpreter gets via the same syscall path.
            let Ok(meta) = std::fs::metadata(&entry_path) else {
                continue;
            };
            let Ok(ft) = entry.file_type() else { continue };
            let name_str = entry.file_name().to_string_lossy().into_owned();
            let path_str = entry_path.to_string_lossy().into_owned();
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
            // 7 fields * 8 bytes = 56 bytes. Route through the
            // tracing collector so the blob participates in
            // mark/sweep instead of leaking after every list_dir.
            let blob = super::gc::gos_rt_gc_alloc(56) as *mut i64;
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
    })
}

/// `fs::walk_dir(root) -> Result<[DirInfo], errors::Error>`.
/// Recursive descendant walk. Same DirInfo shape as `fs::list_dir`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_walk_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let root = if path.is_null() {
            ".".to_string()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        let out = unsafe { gos_rt_vec_new(8) };
        let mut stack: Vec<std::path::PathBuf> = vec![std::path::PathBuf::from(&root)];
        while let Some(dir) = stack.pop() {
            let Ok(read) = std::fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<std::fs::DirEntry> = read.flatten().collect();
            entries.sort_by_key(std::fs::DirEntry::file_name);
            for entry in entries {
                let path_buf = entry.path();
                // Same reason as in gos_rt_fs_list_dir: use std::fs::metadata
                // rather than entry.metadata() so directory sizes agree with
                // the interpreter on Windows.
                let Ok(meta) = std::fs::metadata(&path_buf) else {
                    continue;
                };
                let Ok(ft) = entry.file_type() else { continue };
                let name_str = entry.file_name().to_string_lossy().into_owned();
                let path_str = path_buf.to_string_lossy().into_owned();
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
                // 7 fields * 8 bytes = 56 bytes. Route through the
                // tracing collector — symmetric with list_dir.
                let blob = super::gc::gos_rt_gc_alloc(56) as *mut i64;
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
                if is_dir == 1 && is_symlink == 0 {
                    stack.push(path_buf);
                }
            }
        }
        unsafe { gos_rt_result_new(0, out as i64) }
    })
}

/// `os::exists(path) -> bool`. Returns 1 when `path` names an
/// existing filesystem entry, 0 otherwise. Returns i64 so that
/// Cranelift / LLVM callers receive a full-width integer rather
/// than an i8 that gets garbage in the upper bits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_exists(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        i64::from(std::path::Path::new(&p).exists())
    })
}

/// `os::is_file(path) -> bool` / `fs::is_file(path) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_is_file(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        i64::from(std::fs::metadata(&p).is_ok_and(|m| m.is_file()))
    })
}

/// `os::is_dir(path) -> bool` / `fs::is_dir(path) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_is_dir(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        i64::from(std::fs::metadata(&p).is_ok_and(|m| m.is_dir()))
    })
}

/// `fs::is_symlink(path) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_is_symlink(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        i64::from(std::fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_symlink()))
    })
}

/// `fs::file_size(path) -> i64`. Returns 0 when the path cannot be
/// stat'd; the interp's matching helper has the same shape.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_file_size(path: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if path.is_null() {
            return 0;
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        std::fs::metadata(&p).map_or(0, |m| i64::try_from(m.len()).unwrap_or(i64::MAX))
    })
}

/// `fs::metadata(path) -> Result<i64, errors::Error>`. The Ok
/// payload is the file size in bytes; richer struct accessors
/// (`is_file`, `is_dir`, `modified_unix_ms`, …) are exposed
/// through the existing `gos_rt_os_*` predicates on the same
/// path. This compiled-tier surface is a strict subset of the
/// VM's `fs::Metadata { … }` aggregate — the dominant call shape
/// (`if let Ok(_) = fs::metadata(p) { … }` to test stat-ability)
/// works end-to-end; the field-rich form will land alongside the
/// shared aggregate-binding pass that http::Request / sql::Row
/// also need.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_metadata(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("fs::metadata: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        match std::fs::metadata(&p) {
            Ok(m) => {
                let size = i64::try_from(m.len()).unwrap_or(i64::MAX);
                unsafe { gos_rt_result_new(0, size) }
            }
            Err(e) => {
                let msg = format!("fs::metadata({p}): {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}

/// `exec::run(prog, args) -> Result<Output, errors::Error>`.
///
/// Spawns `prog` with `args` (a `Vec<String>` whose backing storage
/// is a tight array of `*const c_char`), captures stdout/stderr,
/// and waits for completion.
///
/// Ok payload is a 3-slot heap aggregate matching the MIR field
/// order registered in `stdlib_struct_shapes`:
/// `[stdout: *c_char, stderr: *c_char, code: i64]`. Err payload is
/// a `*mut GosError` so callers can `.map_err(|e| errors::wrap(e,
/// ...))` without a hand-rolled string-to-error coercion.
///
/// Without this binding, the MIR layer would fall through to a
/// generic free-call dispatch that emits a call to a non-existent
/// symbol; cranelift / LLVM then either drop the call entirely or
/// stash a garbage pointer in the destination, and the caller
/// dereferences it as a Result aggregate — the visible symptom is
/// either a plain segfault or `memory allocation of <huge> bytes
/// failed` when the runtime treats the random pointer as a
/// length-prefixed buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_exec_run(prog: *const c_char, args: *mut GosVec) -> i128 {
    ffi_entry!(0i128, {
        let prog_str = if prog.is_null() {
            let cs = std::ffi::CString::new("exec::run: program is null").unwrap_or_default();
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
                    let cstr_ptr = unsafe { (slot as *const *const c_char).read_unaligned() };
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
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        match command.output() {
            Ok(out) => {
                let stdout_str = String::from_utf8_lossy(&out.stdout).into_owned();
                let stderr_str = String::from_utf8_lossy(&out.stderr).into_owned();
                let code = i64::from(out.status.code().unwrap_or(-1));
                let stdout_cs = alloc_cstring(stdout_str.as_bytes()) as i64;
                let stderr_cs = alloc_cstring(stderr_str.as_bytes()) as i64;
                // Output struct laid out as `[stdout: i64, stderr: i64,
                // code: i64]` (3 × 8 B). Box-allocated so the pointer
                // shares the global allocator domain with every other
                // helper-returned aggregate; previously the arena
                // backed this and an LLVM `arena_restore` could rewind
                // the watermark while the caller still held the blob.
                let blob = Box::into_raw(Box::new([stdout_cs, stderr_cs, code])).cast::<i64>();
                gos_rt_result_new(0, blob as i64)
            }
            Err(e) => {
                let msg = format!("exec::run({prog_str}): {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}
