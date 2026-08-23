//! cgroup v2: the only mechanism on Linux that reaches a descendant
//! which left its process group.
//!
//! The run's tree lives in a cgroup of its own, so `cgroup.kill` ends
//! every process in it whatever session or process group it moved
//! itself into. No controller is enabled, so a host that delegates a
//! cgroup at all can contain a run.

use std::ffi::CString;
use std::path::PathBuf;

/// The delegated cgroup this process may create children under, or
/// `None` when the host delegates none.
///
/// systemd's user slice delegates one; a bare login shell or a
/// container without delegation does not, and there the limits are
/// unenforceable without privilege.
#[must_use]
pub(crate) fn delegated_cgroup() -> Option<PathBuf> {
    let own = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    // cgroup v2 reports exactly one `0::<path>` line.
    let relative = own
        .lines()
        .find_map(|line| line.strip_prefix("0::"))?
        .trim()
        .to_string();
    let root = PathBuf::from("/sys/fs/cgroup").join(relative.trim_start_matches('/'));
    // Delegation is exactly the ability to write the controls; a
    // readable-but-not-writable directory is the undelegated case.
    let can_write = std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("cgroup.subtree_control"))
        .is_ok();
    can_write.then_some(root)
}

/// Whether a cgroup this process can create children under is
/// reachable.
#[must_use]
pub(crate) fn available() -> bool {
    delegated_cgroup().is_some()
}

/// A cgroup holding one run's process tree.
///
/// Two jobs: it carries the memory and process-count limits, and it is
/// the handle that reaches every descendant at once. A process cannot
/// leave the cgroup it was placed in without privilege, so `setsid`,
/// double-forking, and reparenting to `init` all stay inside it.
pub(crate) struct Cgroup {
    directory: PathBuf,
    /// The `cgroup.procs` path a child writes itself into, resolved to
    /// a C string before the fork because the pre-exec window may not
    /// allocate.
    join_path: CString,
}

impl Cgroup {
    /// Creates the cgroup for a run, applying whatever limits
    /// `resources` names.
    ///
    /// `Ok(None)` when the host delegates no usable cgroup: the caller
    /// has already refused any limit that needed one, so what remains
    /// is a run whose teardown falls back to the process group.
    pub(crate) fn create() -> Result<Option<Self>, String> {
        let Some(root) = delegated_cgroup() else {
            return Ok(None);
        };
        let directory = root.join(format!(
            "gossamer-sandbox.{}.{}",
            std::process::id(),
            serial()
        ));
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;

        let join = directory.join("cgroup.procs");
        let join_path = CString::new(join.as_os_str().as_encoded_bytes())
            .map_err(|error| format!("{}: {error}", join.display()))?;
        Ok(Some(Self {
            directory,
            join_path,
        }))
    }

    /// The `cgroup.procs` path the pre-exec code writes `0` into, so
    /// the child places itself before it execs anything.
    pub(crate) fn join_path(&self) -> &CString {
        &self.join_path
    }

    /// Kills every process in the cgroup, whatever session or process
    /// group it moved itself into.
    pub(crate) fn kill(&self) {
        if std::fs::write(self.directory.join("cgroup.kill"), "1").is_ok() {
            return;
        }
        // `cgroup.kill` arrived in 5.14. Older kernels need the same
        // thing done by hand, and the read is repeated because a
        // process killed mid-fork can leave a child behind.
        for _ in 0..8 {
            let Ok(members) = std::fs::read_to_string(self.directory.join("cgroup.procs")) else {
                return;
            };
            let pids: Vec<i32> = members
                .lines()
                .filter_map(|line| line.trim().parse().ok())
                .collect();
            if pids.is_empty() {
                return;
            }
            for pid in pids {
                #[allow(
                    unsafe_code,
                    reason = "kill has no safe wrapper; the pid comes from a cgroup this \
                              process created and owns"
                )]
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
}

impl Drop for Cgroup {
    fn drop(&mut self) {
        // A cgroup with a live member cannot be removed, and a member
        // that outlives the run is exactly what the kill is for.
        self.kill();
        let _ = std::fs::remove_dir(&self.directory);
    }
}

/// A cgroup name no other run in this process shares.
fn serial() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[cfg(test)]
mod cgroup_tests {
    use super::*;

    /// A run is contained by a cgroup of its own, which needs a
    /// delegated parent and nothing else: no controller is enabled, so
    /// a host that delegates a cgroup without the memory or pids
    /// controller still contains its runs.
    #[test]
    fn a_run_gets_a_cgroup_of_its_own_wherever_one_is_delegated() {
        let Some(group) = Cgroup::create().expect("create") else {
            assert!(!available(), "a host with a delegated cgroup creates one");
            return;
        };
        assert!(available());
        assert!(
            group.join_path().to_bytes().ends_with(b"cgroup.procs"),
            "the child joins through cgroup.procs"
        );
    }
}
