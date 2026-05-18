//! Safe-Rust wrapper around the classic Unix `fork` + `setsid` +
//! second-`fork` daemonization sequence. The unsafe is contained
//! here so `gossamer-std` (which carries `#![forbid(unsafe_code)]`)
//! can call into it through ordinary safe Rust.
//!
//! ## Soundness contract
//!
//! `fork` after threads have started is fundamentally unsafe — only
//! async-signal-safe operations may run in the child until the next
//! `exec*`. `daemonize` must therefore be invoked **before any
//! goroutine or thread spawn**. The wrapper does the minimum needed
//! to detach from the controlling terminal:
//!
//! 1. `fork` — parent exits, child continues.
//! 2. `setsid` — child becomes session + process-group leader.
//! 3. Second `fork` — grandchild can never re-acquire a controlling
//!    terminal even if it later opens one.
//! 4. `chdir("/")` — release the inherited working directory so the
//!    daemon does not pin a mount point.
//! 5. Redirect stdin / stdout / stderr to `/dev/null` so the
//!    detached child does not inherit terminal file descriptors.
//!
//! On non-Unix targets `daemonize` returns
//! `Err(std::io::ErrorKind::Unsupported)`.

#![allow(unsafe_code)]

use std::io;

/// Detaches the current process from its controlling terminal.
/// Returns `Ok(())` in the grandchild (the daemon). The parent and
/// intermediate child do not return: they call `_exit(0)`.
///
/// Must be called before any goroutine or worker thread is spawned.
#[cfg(unix)]
pub fn daemonize() -> io::Result<()> {
    use std::ffi::CString;

    // First fork: detach from the invoking shell. The parent
    // _exit()s so the shell sees the launch return immediately.
    // SAFETY: fork is unsafe in a multi-threaded program; the
    // documented contract requires the caller to invoke this
    // before any thread spawn.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid > 0 {
        // SAFETY: _exit is async-signal-safe and the parent has
        // nothing further to clean up at the runtime level.
        unsafe { libc::_exit(0) };
    }

    // Become session + process-group leader so we are no longer
    // associated with the parent's controlling terminal.
    // SAFETY: setsid takes no args and only mutates kernel-side
    // state for the current process.
    if unsafe { libc::setsid() } < 0 {
        return Err(io::Error::last_os_error());
    }

    // Second fork: ensure we cannot re-acquire a controlling
    // terminal even if we later open one.
    // SAFETY: same contract as the first fork above.
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Err(io::Error::last_os_error());
    }
    if pid > 0 {
        // SAFETY: see above.
        unsafe { libc::_exit(0) };
    }

    // Release the inherited working directory so the daemon does
    // not pin a mount point.
    let root = CString::new("/").expect("'/' is valid C string");
    // SAFETY: chdir takes a NUL-terminated path; the CString above
    // provides one whose lifetime outlasts the call.
    if unsafe { libc::chdir(root.as_ptr()) } < 0 {
        return Err(io::Error::last_os_error());
    }

    // Redirect stdio to /dev/null. Close the inherited fds first
    // so dup2's target slot is free; ignore EBADF if a slot is
    // already closed.
    redirect_stdio_to_devnull()?;
    Ok(())
}

#[cfg(unix)]
fn redirect_stdio_to_devnull() -> io::Result<()> {
    use std::ffi::CString;

    let devnull = CString::new("/dev/null").expect("'/dev/null' is valid C string");
    // SAFETY: open with O_RDWR + a NUL-terminated path; the
    // CString lifetime outlasts the syscall.
    let fd = unsafe { libc::open(devnull.as_ptr(), libc::O_RDWR) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    for target in [libc::STDIN_FILENO, libc::STDOUT_FILENO, libc::STDERR_FILENO] {
        // SAFETY: dup2 atomically replaces `target` with a
        // duplicate of `fd`; both are valid open file descriptors
        // at this point.
        if unsafe { libc::dup2(fd, target) } < 0 {
            let err = io::Error::last_os_error();
            // SAFETY: close on the source fd; failure is ignored
            // because the daemonization is already half-done.
            unsafe { libc::close(fd) };
            return Err(err);
        }
    }
    if fd > libc::STDERR_FILENO {
        // SAFETY: source fd is no longer needed after the dup2s.
        unsafe { libc::close(fd) };
    }
    Ok(())
}

/// Non-Unix stub: daemonization is a POSIX concept. Returns
/// `Unsupported`.
#[cfg(not(unix))]
pub fn daemonize() -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "daemonize is only supported on Unix targets",
    ))
}
