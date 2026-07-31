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
use std::time::{Duration, Instant};

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
//     legacy Var-typed callers) and the `flag` parser's sentinel
//     path; `gos_rt_arr_len` detects that pointer and
//     short-circuits to `argc - 1`. These consumers only read the
//     bytes through `CStr`, never run RC accounting on them.
//   - an owned `*mut GosVec` of `vec_elem_kind::STRING`, returned
//     by `gos_rt_os_args`, holding a gos-tagged, refcounted copy
//     of each user arg. The copies are essential: `args[i]` is a
//     `String`, so the compiled tier emits RC retain/release on it
//     for `.clone()` and end-of-scope drops, and those dispatch on
//     an RC header read at a negative offset from the pointer. A
//     raw libc `argv` pointer has no such header - the retain would
//     corrupt the contiguous argv block and a release would free a
//     libc-interior pointer - so the vec must own real gos strings.

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
        let startup_started = Instant::now();
        // Configure the allocator before copying argv into Gossamer-owned
        // strings. Linux also has an earlier constructor for THP policy, but
        // macOS and Windows reach this C entry first; doing it here gives all
        // native targets the same allocation policy before runtime-owned
        // allocations begin. `runtime_init` is process-idempotent.
        runtime_init();
        startup_trace("runtime_init", startup_started.elapsed());
        // Capture argv[0] as the program name whenever argv has any
        // entries - previously this only happened when argc > 1, so
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
            let len = argc - 1;
            // `ARGS_PTR` / `ARGS_LEN` keep the raw `argv + 1` view for the
            // read-only consumers that still walk it directly: the
            // `gos_rt_arr_len` length short-circuit and the `flag` parser's
            // sentinel path. Those only `CStr`-read the bytes; they never run
            // RC accounting on them, so the raw libc pointers are safe there.
            ARGS_PTR.store(user_argv as usize, Ordering::SeqCst);
            ARGS_LEN.store(i64::from(len), Ordering::SeqCst);
            // Expose `os::args()` as an owned `Vec<String>`: copy each arg
            // into a gos-tagged, refcounted string. libc's `argv` strings are
            // untagged and packed contiguously, so handing them to the RC
            // retain/release dispatch that `args[i].clone()` and end-of-scope
            // drops emit would read and write a fabricated RC header at a
            // negative offset from the pointer - that offset lands inside the
            // adjacent argument and corrupts it, and the release that drives
            // the bogus count to zero frees a libc-interior pointer. Owning
            // tagged copies makes every RC op land on a real header; the vec
            // holds the base reference for the process lifetime (I4: string
            // boundaries own their values), mirroring the `argv[0]` copy above.
            let vec = unsafe { gos_rt_vec_new_typed(8, vec_elem_kind::STRING) };
            if !vec.is_null() {
                for i in 0..len {
                    // SAFETY: `user_argv[0..len]` is valid (see above).
                    let p = unsafe { *user_argv.add(i as usize) };
                    let bytes = if p.is_null() {
                        &b""[..]
                    } else {
                        unsafe { CStr::from_ptr(p).to_bytes() }
                    };
                    let cs = alloc_cstring(bytes) as i64;
                    unsafe { gos_rt_vec_push(vec, std::ptr::addr_of!(cs).cast::<u8>()) };
                }
            }
            ARGS_VEC.store(vec as usize, Ordering::SeqCst);
        } else {
            ARGS_PTR.store(0, Ordering::SeqCst);
            ARGS_LEN.store(0, Ordering::SeqCst);
            // Even when there are no user args, expose a valid empty
            // `Vec<String>` so callers iterating `for a in env::args()` see
            // len=0 instead of dereferencing a null header, and so the
            // element kind matches the populated branch.
            let vec = unsafe { gos_rt_vec_new_typed(8, vec_elem_kind::STRING) };
            ARGS_VEC.store(vec as usize, Ordering::SeqCst);
        }
        startup_trace("arguments", startup_started.elapsed());
        // Initialise the Rust runtime's per-process state. The
        // Cranelift-emitted `main` shim is a plain
        // `extern "C" fn main(int, **char) -> int`, so libc's
        // `__libc_start_main` calls it directly - bypassing the
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
        // `runtime_init` ran before argv copying so allocator setup is not
        // delayed past the first runtime allocation.
    });
}

/// Emits opt-in startup phase timings for native binaries. This makes
/// cross-platform startup regressions diagnosable without affecting normal
/// program output or depending on a platform-specific profiler.
fn startup_trace(phase: &str, elapsed: Duration) {
    if std::env::var_os("GOS_STARTUP_TRACE").is_some() {
        eprintln!(
            "gos-startup: phase={phase} elapsed_us={}",
            elapsed.as_micros()
        );
    }
}

/// Configures mimalloc options that need to be process-wide. Delegates to the
/// single implementation in the crate root; the option indices and rationale
/// live there.
fn configure_allocator() {
    crate::init_process_allocator();
}

// Runs the allocator configuration from `.init_array`, before `main`
// and therefore before the process's first heap allocation. The
// `allow_thp` option only takes effect if set before mimalloc maps its
// first arena (the `MADV_HUGEPAGE` advice is applied at arena mmap
// time), and that arena is created by the very first allocation -
// argv capture above in compiled programs, Rust's pre-main runtime in
// the `gos` binary - which precedes every `runtime_init` call site.
// THP is Linux-only, so the constructor is too; other platforms keep
// the `runtime_init`/CLI-main call, where the remaining allocator knobs
// are safe to apply later. Lives in this module (not lib.rs) so the
// archive member is always pulled in: every compiled binary references
// `gos_rt_set_args`. Excluded from this crate's own test binary, whose
// option-index guard test must read the pristine mimalloc defaults before
// anything sets them.
#[cfg(all(target_os = "linux", not(tsan), not(test)))]
#[used]
#[unsafe(link_section = ".init_array")]
static CONFIGURE_ALLOCATOR_CTOR: extern "C" fn() = {
    extern "C" fn configure_allocator_early() {
        crate::init_process_allocator();
    }
    configure_allocator_early
};

#[cfg(unix)]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        configure_allocator();
        // Install the main-thread stack-overflow guard. A `gos build`
        // binary's `@main` is a bare `extern "C" fn` called straight from
        // libc, so rustc's `lang_start` guard is bypassed; without this a
        // deeply recursive program faults on the guard page as a raw
        // SIGSEGV (exit 139) instead of a diagnosed "stack overflow"
        // message. Idempotent (per-thread + process-wide `Once` inside).
        crate::stack_guard::install_stack_guard();
        // SIGPIPE → SIG_IGN. Mirrors what `std::rt::lang_start`'s
        // `sys::unix::init` does. Without this, a write to a
        // closed peer (very common under heavy keep-alive load)
        // terminates the process. Skipped under Miri, which cannot call the
        // `signal` foreign function; SIGPIPE delivery is moot in the
        // interpreter anyway.
        #[cfg(all(unix, not(miri)))]
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

#[cfg(all(not(unix), not(target_arch = "wasm32")))]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        configure_allocator();
        // Install the main-thread stack-overflow guard (Windows: a vectored
        // exception handler). See the unix arm for why the AOT `@main`
        // entry needs this that rustc's `lang_start` would otherwise provide.
        crate::stack_guard::install_stack_guard();
        let handle = std::thread::Builder::new()
            .name("gos-rt-init".to_string())
            .spawn(|| {})
            .expect("spawn rt init thread");
        let _ = handle.join();
    });
}

// wasm32-unknown-unknown has no threads, so the thread-machinery
// pre-warm above is both impossible and unnecessary: the runtime is
// single-threaded under the cooperative scheduler. Configure the
// allocator and stop.
#[cfg(target_arch = "wasm32")]
fn runtime_init() {
    use std::sync::Once;
    static ONCE: Once = Once::new();
    ONCE.call_once(configure_allocator);
}

/// Returns a `*mut GosVec` view of the user arguments. The
/// header's `ptr` is `argv + 1` and `len`/`cap` are `argc - 1`,
/// so `args.len()` dispatches through `gos_rt_arr_len` (reading
/// `len` at offset 0) and `args[i]` reads the i-th `*const c_char`
/// through the GosVec `ptr` field - same shape as any other
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
/// knows the script path (e.g. `gos examples/cat.gos`). The
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
/// binaries; the script path for `gos`).
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
struct DirInfoData {
    name: String,
    path: String,
    is_file: bool,
    is_dir: bool,
    is_symlink: bool,
    size: i64,
    modified_ms: i64,
}

#[cfg(any(unix, windows))]
const ENCODED_PATH_PREFIX: &str = "@gossamer-path:x";
const ESCAPED_PATH_PREFIX: &str = "@gossamer-path:u";

pub(super) fn encode_os_path(path: &std::path::Path) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let bytes = path.as_os_str().as_bytes();
        if let Ok(text) = std::str::from_utf8(bytes) {
            return escape_path_prefix(text);
        }
        let mut encoded = String::with_capacity(ENCODED_PATH_PREFIX.len() + bytes.len() * 2);
        encoded.push_str(ENCODED_PATH_PREFIX);
        for byte in bytes {
            use std::fmt::Write;
            write!(encoded, "{byte:02X}").expect("writing to String cannot fail");
        }
        encoded
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        if let Some(text) = path.to_str() {
            return escape_path_prefix(text);
        }
        let mut encoded = String::from(ENCODED_PATH_PREFIX);
        for unit in path.as_os_str().encode_wide() {
            use std::fmt::Write;
            write!(encoded, "{unit:04X}").expect("writing to String cannot fail");
        }
        encoded
    }
    #[cfg(not(any(unix, windows)))]
    {
        escape_path_prefix(&path.to_string_lossy())
    }
}

pub(super) fn decode_os_path(path: &str) -> std::path::PathBuf {
    if let Some(text) = path.strip_prefix(ESCAPED_PATH_PREFIX) {
        return std::path::PathBuf::from(format!("@gossamer-path:{text}"));
    }
    #[cfg(not(any(unix, windows)))]
    {
        return std::path::PathBuf::from(path);
    }
    #[cfg(any(unix, windows))]
    let Some(hex) = path.strip_prefix(ENCODED_PATH_PREFIX) else {
        return std::path::PathBuf::from(path);
    };
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStringExt;
        if !hex.len().is_multiple_of(2) {
            return std::path::PathBuf::from(path);
        }
        let Some(bytes) = (0..hex.len())
            .step_by(2)
            .map(|start| u8::from_str_radix(&hex[start..start + 2], 16).ok())
            .collect::<Option<Vec<_>>>()
        else {
            return std::path::PathBuf::from(path);
        };
        std::path::PathBuf::from(std::ffi::OsString::from_vec(bytes))
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStringExt;
        if !hex.len().is_multiple_of(4) {
            return std::path::PathBuf::from(path);
        }
        let Some(units) = (0..hex.len())
            .step_by(4)
            .map(|start| u16::from_str_radix(&hex[start..start + 4], 16).ok())
            .collect::<Option<Vec<_>>>()
        else {
            return std::path::PathBuf::from(path);
        };
        std::path::PathBuf::from(std::ffi::OsString::from_wide(&units))
    }
}

fn escape_path_prefix(path: &str) -> String {
    path.strip_prefix("@gossamer-path:").map_or_else(
        || path.to_string(),
        |rest| format!("{ESCAPED_PATH_PREFIX}{rest}"),
    )
}

fn dir_info(entry: std::fs::DirEntry) -> Option<DirInfoData> {
    let entry_path = entry.path();
    // Use std::fs::metadata (opens a handle) rather than entry.metadata()
    // (reads from FindFile cache on Windows). The latter returns 0 for
    // directory sizes on Windows because WIN32_FIND_DATA stores nFileSize=0
    // for directories; the former calls GetFileInformationByHandle and
    // returns the real NTFS directory-index allocation, matching what the
    // interpreter gets via the same syscall path.
    let meta = std::fs::metadata(&entry_path).ok()?;
    let ft = entry.file_type().ok()?;
    Some(DirInfoData {
        name: encode_os_path(std::path::Path::new(&entry.file_name())),
        path: encode_os_path(&entry_path),
        is_file: ft.is_file(),
        is_dir: ft.is_dir(),
        is_symlink: ft.is_symlink(),
        size: i64::try_from(meta.len()).unwrap_or(0),
        modified_ms: meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .and_then(|d| i64::try_from(d.as_millis()).ok())
            .unwrap_or(0),
    })
}

fn list_dir_data(path: &str) -> Result<Vec<DirInfoData>, std::io::Error> {
    let mut entries: Vec<std::fs::DirEntry> =
        std::fs::read_dir(decode_os_path(path))?.flatten().collect();
    entries.sort_by_key(std::fs::DirEntry::file_name);
    Ok(entries.into_iter().filter_map(dir_info).collect())
}

fn walk_dir_data(root: &str) -> Result<Vec<DirInfoData>, std::io::Error> {
    let mut out = Vec::new();
    let mut stack = vec![decode_os_path(root)];
    while let Some(dir) = stack.pop() {
        let mut entries: Vec<std::fs::DirEntry> = std::fs::read_dir(&dir)?.flatten().collect();
        entries.sort_by_key(std::fs::DirEntry::file_name);
        for entry in entries {
            let child = entry.path();
            let Some(info) = dir_info(entry) else {
                continue;
            };
            if info.is_dir && !info.is_symlink {
                stack.push(child);
            }
            out.push(info);
        }
    }
    Ok(out)
}

fn dir_infos_result(entries: Vec<DirInfoData>) -> i128 {
    let out = unsafe { gos_rt_vec_new(8) };
    for entry in entries {
        let name_cs = alloc_cstring(entry.name.as_bytes()) as i64;
        let path_cs = alloc_cstring(entry.path.as_bytes()) as i64;
        // 7 fields * 8 bytes = 56 bytes. Route through the tracing collector
        // so the blob participates in mark/sweep instead of leaking.
        let blob = super::gc::gos_rt_gc_alloc(56) as *mut i64;
        if blob.is_null() {
            continue;
        }
        unsafe {
            *blob.add(0) = name_cs;
            *blob.add(1) = path_cs;
            *blob.add(2) = i64::from(entry.is_file);
            *blob.add(3) = i64::from(entry.is_dir);
            *blob.add(4) = i64::from(entry.is_symlink);
            *blob.add(5) = entry.size;
            *blob.add(6) = entry.modified_ms;
        }
        let entry_val = blob as i64;
        unsafe {
            gos_rt_vec_push(out, std::ptr::addr_of!(entry_val).cast::<u8>());
        }
    }
    unsafe { gos_rt_result_new(0, out as i64) }
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_list_dir(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let p = if path.is_null() {
            ".".to_string()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        match crate::sched_global::run_blocking("fs-list-dir", move || list_dir_data(&p)) {
            Ok(Ok(entries)) => dir_infos_result(entries),
            Ok(Err(e)) => {
                let msg = format!("list_dir: {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => {
                let cs = std::ffi::CString::new(e).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
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
        match crate::sched_global::run_blocking("fs-walk-dir", move || walk_dir_data(&root)) {
            Ok(Ok(entries)) => dir_infos_result(entries),
            Ok(Err(e)) => {
                let cs = std::ffi::CString::new(e.to_string()).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => {
                let cs = std::ffi::CString::new(e).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
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
/// VM's `fs::Metadata { … }` aggregate - the dominant call shape
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

/// `fs::metadata(path)` leaf -> Result<(size, is_file, is_dir,
/// is_symlink, readonly, modified_unix_ms), errors::Error>. The
/// injected Gossamer wrapper (`__gos_fs_metadata`) folds this 6-slot
/// tuple into a real `Metadata` struct, so `fs::metadata(p).size` /
/// `.is_file` lower natively on every tier. Field order and units
/// (millis since the Unix epoch) match the VM's `fs::Metadata`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fs_metadata_raw(path: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if path.is_null() {
            let cs = std::ffi::CString::new("fs::metadata: null path").unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let p = unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() };
        match std::fs::metadata(&p) {
            Ok(m) => {
                let modified = m
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
                let blob = crate::c_abi::gos_rt_gc_alloc(48) as *mut i64;
                if blob.is_null() {
                    let cs =
                        std::ffi::CString::new("fs::metadata: alloc failed").unwrap_or_default();
                    let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                    return unsafe { gos_rt_result_new(1, err as i64) };
                }
                unsafe {
                    *blob = i64::try_from(m.len()).unwrap_or(i64::MAX);
                    *blob.add(1) = i64::from(m.is_file());
                    *blob.add(2) = i64::from(m.is_dir());
                    *blob.add(3) = i64::from(m.file_type().is_symlink());
                    *blob.add(4) = i64::from(m.permissions().readonly());
                    *blob.add(5) = modified;
                }
                unsafe { gos_rt_result_new(0, blob as i64) }
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
/// dereferences it as a Result aggregate - the visible symptom is
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
        let display_prog = prog_str.clone();
        match crate::sched_global::run_blocking("exec-run", move || {
            let mut command = std::process::Command::new(&prog_str);
            command.args(&cmd_args);
            command.stdout(std::process::Stdio::piped());
            command.stderr(std::process::Stdio::piped());
            command.output()
        }) {
            Ok(Ok(out)) => {
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
            Ok(Err(e)) => {
                let msg = format!("exec::run({display_prog}): {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
            Err(e) => {
                let msg = format!("exec::run({display_prog}): {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}

#[cfg(test)]
mod args_tests {
    use super::*;
    use std::ffi::CStr;

    /// `os::args()` must hand back owned, gos-tagged strings rather than the
    /// raw, contiguously-packed libc `argv` pointers: `args[i]` is a `String`,
    /// so the compiled tier emits RC retain/release on it, and that dispatch
    /// reads an RC header at a negative offset from the pointer. A raw `argv`
    /// pointer has no such header, so the retain corrupts the adjacent argument
    /// and the release frees a libc-interior pointer.
    #[test]
    fn user_args_are_owned_gos_strings_safe_to_retain() {
        // Mimic libc's layout: a single block of NUL-terminated, contiguous
        // args. `argv[0]` is the program name; the rest are user args.
        let words = ["prog", "Qwen3.6-35B", "a", "b", "c", "d"];
        let mut block: Vec<u8> = Vec::new();
        let mut offsets: Vec<usize> = Vec::new();
        for w in &words {
            offsets.push(block.len());
            block.extend_from_slice(w.as_bytes());
            block.push(0);
        }
        let base = block.as_ptr();
        let argv: Vec<*const c_char> = offsets
            .iter()
            .map(|&o| unsafe { base.add(o) }.cast::<c_char>())
            .collect();
        unsafe { gos_rt_set_args(argv.len() as c_int, argv.as_ptr()) };

        let vec = unsafe { gos_rt_os_args() };
        assert!(!vec.is_null());
        let len = unsafe { gos_rt_vec_len(vec) };
        assert_eq!(len, 5, "argv[0] is the program name; 5 user args remain");

        // Every element is a gos-owned (tagged) string: RC dispatch reads a
        // real header instead of fabricating one from libc bytes.
        for i in 0..len {
            let p = unsafe { gos_rt_vec_get_i64(vec, i) } as *const c_char;
            assert!(
                unsafe { crate::c_abi::string::is_gos_string(p) },
                "arg {i} must be a gos-owned string, not a raw argv pointer"
            );
        }

        // Retaining and releasing every other arg must leave arg 0 byte-for-byte
        // intact - on the raw-pointer design the retain wrote an RC header into
        // the contiguous neighbour and mutated it.
        let arg0 = unsafe { gos_rt_vec_get_i64(vec, 0) } as *const c_char;
        let before = unsafe { CStr::from_ptr(arg0) }.to_bytes().to_vec();
        for i in 1..len {
            let p = unsafe { gos_rt_vec_get_i64(vec, i) } as *mut u8;
            unsafe { gos_rt_rc_retain(p) };
            unsafe { gos_rt_rc_release(p) };
        }
        let after = unsafe { CStr::from_ptr(arg0) }.to_bytes().to_vec();
        assert_eq!(before, after, "retaining neighbours must not mutate arg 0");
        assert_eq!(after, b"Qwen3.6-35B");
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn encoded_non_utf8_directory_path_can_be_listed_again() {
        use std::os::unix::ffi::OsStringExt;

        let root = std::env::temp_dir().join(format!(
            "gossamer-native-non-utf8-dir-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let child = root.join(std::ffi::OsString::from_vec(b"x\xa0y".to_vec()));
        std::fs::create_dir(&child).unwrap();
        std::fs::write(child.join("payload"), b"x").unwrap();

        let root_text = root.to_str().unwrap();
        let first = list_dir_data(root_text).unwrap();
        assert_eq!(first.len(), 1);
        assert!(first[0].path.starts_with(ENCODED_PATH_PREFIX));
        let nested = list_dir_data(&first[0].path).unwrap();
        assert_eq!(nested.len(), 1);

        std::fs::remove_dir_all(&root).unwrap();
    }
}
