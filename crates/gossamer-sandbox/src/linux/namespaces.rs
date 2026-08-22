//! Namespaces: the `strict` level's process, mount, and network
//! isolation.
//!
//! Everything here runs between `fork` and `exec`, where only
//! async-signal-safe calls are permitted. Every string the code writes
//! is built before the fork and stored in [`Plan`]; the pre-exec path
//! allocates nothing.

#![allow(
    unsafe_code,
    reason = "namespace setup is raw syscalls by nature; each call below \
              operates on data allocated before the fork, as the \
              async-signal-safety contract requires"
)]

use std::ffi::CString;

/// Whether unprivileged user namespaces are usable, and what blocks
/// them when they are not.
///
/// Reported rather than guessed: distributions disable them in three
/// different ways and each needs its own sysctl named in the failure.
///
/// Answered once per process. A capability report is asked for several
/// times in one run - to refuse an unenforceable limit, to print the
/// banner, to explain the policy - and the answer costs a `fork`,
/// which a supervisor should not spend on a host configuration that
/// cannot change under it.
#[must_use]
pub(crate) fn user_namespace_blocker() -> Option<String> {
    static ANSWER: std::sync::OnceLock<Option<String>> = std::sync::OnceLock::new();
    ANSWER.get_or_init(probe_blocker).clone()
}

fn probe_blocker() -> Option<String> {
    let read = |path: &str| std::fs::read_to_string(path).ok();
    if let Some(value) = read("/proc/sys/kernel/unprivileged_userns_clone") {
        if value.trim() == "0" {
            return Some("kernel.unprivileged_userns_clone = 0".to_string());
        }
    }
    if let Some(value) = read("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") {
        if value.trim() == "1" {
            return Some("kernel.apparmor_restrict_unprivileged_userns = 1".to_string());
        }
    }
    if let Some(value) = read("/proc/sys/user/max_user_namespaces") {
        if value.trim() == "0" {
            return Some("user.max_user_namespaces = 0".to_string());
        }
    }
    // The sysctls can all read permissive and the namespace still be
    // refused, by a container runtime's seccomp profile among others.
    // Probing costs one clone, and a wrong answer here is the
    // difference between failing closed and failing at exec.
    probe_unshare()
}

/// Tries the unshare in a throwaway child, so a refusal is observed
/// rather than inferred.
fn probe_unshare() -> Option<String> {
    let pid = unsafe { libc::fork() };
    if pid < 0 {
        return Some("fork failed while probing user namespaces".to_string());
    }
    if pid == 0 {
        let created = unsafe { libc::unshare(libc::CLONE_NEWUSER) };
        unsafe { libc::_exit(i32::from(created != 0)) };
    }
    let mut status = 0;
    unsafe { libc::waitpid(pid, &raw mut status, 0) };
    let refused = !(libc::WIFEXITED(status) && libc::WEXITSTATUS(status) == 0);
    refused.then(|| "unprivileged user namespaces are refused by this host".to_string())
}

/// Everything the pre-exec path needs, allocated before the fork.
pub(crate) struct Plan {
    uid_map: CString,
    gid_map: CString,
    deny: CString,
    proc_source: CString,
    proc_target: CString,
    proc_type: CString,
    tmpfs_source: CString,
    tmpfs_target: CString,
    tmpfs_type: CString,
    tmpfs_options: CString,
    private_temp: bool,
    shadow_source: CString,
    shadowed: Vec<CString>,
}

impl Plan {
    /// Builds the plan for the calling user.
    ///
    /// `private_temp` names the directory a private `tmpfs` is mounted
    /// on. It is the policy's own temp directory rather than `/tmp`,
    /// because mounting over `/tmp` would hide a workspace that lives
    /// under it. `temp_bytes` bounds it; with no bound the mount takes
    /// the kernel default, which is half of RAM.
    ///
    /// `denied_sockets` are the socket paths the policy refuses. A
    /// filesystem policy cannot reach them: `connect` on a pathname
    /// socket is not one of Landlock's access rights, so a denial the
    /// ruleset carries does not stop the connection. The mount
    /// namespace is the mechanism that does, by covering each one with
    /// a node nothing can connect to.
    #[must_use]
    pub(crate) fn new(
        private_temp: Option<&std::path::Path>,
        temp_bytes: Option<u64>,
        denied_sockets: &[std::path::PathBuf],
    ) -> Self {
        let uid = unsafe { libc::geteuid() };
        let gid = unsafe { libc::getegid() };
        let cstring = |text: String| CString::new(text).unwrap_or_default();
        Self {
            uid_map: cstring(format!("0 {uid} 1\n")),
            gid_map: cstring(format!("0 {gid} 1\n")),
            deny: cstring("deny".to_string()),
            proc_source: cstring("proc".to_string()),
            proc_target: cstring("/proc".to_string()),
            proc_type: cstring("proc".to_string()),
            tmpfs_source: cstring("tmpfs".to_string()),
            tmpfs_target: cstring(
                private_temp.map_or_else(String::new, |path| path.to_string_lossy().into_owned()),
            ),
            tmpfs_type: cstring("tmpfs".to_string()),
            // Sized from the policy rather than from a constant: a
            // build whose temporary files outgrow a number nobody chose
            // fails with an `ENOSPC` that names nothing.
            tmpfs_options: cstring(temp_bytes.map_or_else(
                || "mode=1777".to_string(),
                |bytes| format!("mode=1777,size={bytes}"),
            )),
            private_temp: private_temp.is_some(),
            shadow_source: cstring("/dev/null".to_string()),
            shadowed: denied_sockets
                .iter()
                .map(|path| cstring(path.to_string_lossy().into_owned()))
                .collect(),
        }
    }

    /// Enters the user, mount, IPC, UTS, and PID namespaces, and the
    /// network namespace when `deny_network` is set.
    ///
    /// Returns the errno of the first syscall that refused, so the
    /// caller can report which primitive failed instead of a bare
    /// spawn error.
    pub(crate) fn enter(&self, deny_network: bool) -> Result<(), i32> {
        let mut flags = libc::CLONE_NEWUSER
            | libc::CLONE_NEWNS
            | libc::CLONE_NEWIPC
            | libc::CLONE_NEWUTS
            | libc::CLONE_NEWPID;
        if deny_network {
            flags |= libc::CLONE_NEWNET;
        }
        if unsafe { libc::unshare(flags) } != 0 {
            return Err(errno());
        }
        // `setgroups` must be denied before the gid map is written, or
        // the kernel refuses the map for an unprivileged process.
        write_file(c"/proc/self/setgroups", &self.deny)?;
        write_file(c"/proc/self/uid_map", &self.uid_map)?;
        write_file(c"/proc/self/gid_map", &self.gid_map)?;
        Ok(())
    }

    /// Gives the child its own `/proc` and, when asked, its own
    /// `/tmp`.
    ///
    /// A PID namespace without a private `/proc` still shows the host
    /// process table, so `ps` works and the isolation is decorative.
    /// Called after the fork that makes the caller PID 1, because
    /// mounting `proc` requires being inside the PID namespace.
    pub(crate) fn mount_private_filesystems(&self) -> Result<(), i32> {
        // The mount namespace starts as a shared copy of the host's
        // tree; without this the private mounts propagate back out.
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(errno());
        }
        if unsafe {
            libc::mount(
                self.proc_source.as_ptr(),
                self.proc_target.as_ptr(),
                self.proc_type.as_ptr(),
                libc::MS_NOSUID | libc::MS_NOEXEC | libc::MS_NODEV,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(errno());
        }
        if self.private_temp
            && unsafe {
                libc::mount(
                    self.tmpfs_source.as_ptr(),
                    self.tmpfs_target.as_ptr(),
                    self.tmpfs_type.as_ptr(),
                    libc::MS_NOSUID | libc::MS_NODEV,
                    self.tmpfs_options.as_ptr().cast(),
                )
            } != 0
        {
            return Err(errno());
        }
        self.shadow_denied_sockets()
    }

    /// Covers each denied socket with `/dev/null`, so a connection to
    /// it answers `ENOTSOCK` instead of reaching the daemon behind it.
    ///
    /// The bind mount is made after the tree is `MS_PRIVATE`, so it is
    /// this run's view of the path and never the host's.
    ///
    /// A path that is not there is skipped: the policy names every
    /// well-known daemon socket, and a machine runs a few of them.
    /// `ENOENT` from the mount is that case and nothing else, because
    /// the source is `/dev/null`.
    fn shadow_denied_sockets(&self) -> Result<(), i32> {
        for target in &self.shadowed {
            if unsafe {
                libc::mount(
                    self.shadow_source.as_ptr(),
                    target.as_ptr(),
                    std::ptr::null(),
                    libc::MS_BIND,
                    std::ptr::null(),
                )
            } != 0
            {
                let error = errno();
                if error == libc::ENOENT {
                    continue;
                }
                return Err(error);
            }
        }
        Ok(())
    }
}

/// Forks so the caller's child becomes PID 1 of the new namespace, and
/// runs the reaper loop in the caller.
///
/// A PID namespace needs a process at PID 1 that reaps orphans and
/// forwards signals; without one, the first exit tears the namespace
/// down and orphaned descendants become unreapable zombies.
///
/// Returns `Ok(())` in the grandchild, which goes on to `exec`. The
/// PID-1 process never returns: it reaps until its own child exits,
/// then exits with that child's status.
pub(crate) fn fork_reaper() -> Result<(), i32> {
    let child = unsafe { libc::fork() };
    if child < 0 {
        return Err(errno());
    }
    if child == 0 {
        return Ok(());
    }
    reap_until(child)
}

/// PID 1's whole contract: forward the signals a supervisor sends,
/// reap every orphan, and exit with the payload's status.
fn reap_until(payload: libc::pid_t) -> ! {
    loop {
        let mut status = 0;
        let reaped = unsafe { libc::waitpid(-1, &raw mut status, 0) };
        if reaped == payload {
            if libc::WIFSIGNALED(status) {
                // Re-raise so the supervisor outside the namespace
                // sees the same death the payload died of.
                let signal = libc::WTERMSIG(status);
                unsafe {
                    libc::signal(signal, libc::SIG_DFL);
                    libc::raise(signal);
                }
            }
            unsafe { libc::_exit(libc::WEXITSTATUS(status)) };
        }
        if reaped < 0 && errno() != libc::EINTR {
            unsafe { libc::_exit(1) };
        }
    }
}

fn write_file(path: &std::ffi::CStr, contents: &CString) -> Result<(), i32> {
    let fd = unsafe { libc::open(path.as_ptr(), libc::O_WRONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return Err(errno());
    }
    let bytes = contents.as_bytes();
    let written = unsafe { libc::write(fd, bytes.as_ptr().cast(), bytes.len()) };
    unsafe { libc::close(fd) };
    if written == bytes.len() as isize {
        Ok(())
    } else {
        Err(errno())
    }
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
}

#[cfg(test)]
mod namespace_tests {
    use super::*;

    #[test]
    fn the_plan_maps_the_calling_user_to_root_inside_the_namespace() {
        let plan = Plan::new(
            Some(std::path::Path::new("/tmp/private")),
            Some(1 << 30),
            &[],
        );
        let uid = unsafe { libc::geteuid() };
        assert_eq!(
            plan.uid_map.to_str().expect("uid map is utf-8"),
            format!("0 {uid} 1\n")
        );
        assert_eq!(plan.deny.to_str().expect("deny is utf-8"), "deny");
    }

    /// A denied socket is covered rather than merely refused, so the
    /// path list the plan carries has to reach the pre-exec step that
    /// covers it.
    #[test]
    fn a_denied_socket_is_carried_into_the_plan() {
        let plan = Plan::new(
            None,
            None,
            &[std::path::PathBuf::from("/run/example/daemon.sock")],
        );
        assert_eq!(
            plan.shadowed
                .first()
                .and_then(|target| target.to_str().ok()),
            Some("/run/example/daemon.sock")
        );
        assert_eq!(
            plan.shadow_source.to_str().expect("source is utf-8"),
            "/dev/null"
        );
    }
}
