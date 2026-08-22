//! cgroup v2: the memory and process-count limits, and the only
//! mechanism on Linux that reaches a descendant which left its process
//! group.
//!
//! Setting a limit that silently does nothing is worse than reporting
//! no limits at all, so the enforcement report answers from what this
//! process can actually write, and [`crate::Sandbox::new`] refuses a
//! limit the report says is unenforceable.

use std::ffi::CString;
use std::path::{Path, PathBuf};

use crate::level::Enforcement;
use crate::policy::Resources;

/// Controllers a policy's limits need in the delegated subtree.
const REQUIRED_CONTROLLERS: [&str; 2] = ["memory", "pids"];

/// Name of the leaf the supervisor moves itself into.
///
/// cgroup v2 refuses to enable a controller for the children of a
/// cgroup that holds processes of its own, so the delegated cgroup has
/// to be emptied before it can carry limits. Emptying it means moving
/// the supervisor down one level, which is what every tool that sets
/// limits under a delegated subtree does.
const SUPERVISOR_LEAF: &str = "gossamer-sandbox.supervisor";

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
    // The supervisor may already have moved itself into its leaf, in
    // which case the delegated cgroup is the level above it.
    let root = if root.file_name().is_some_and(|name| name == SUPERVISOR_LEAF) {
        root.parent()?.to_path_buf()
    } else {
        root
    };
    // Delegation is exactly the ability to write the controls; a
    // readable-but-not-writable directory is the undelegated case.
    let can_write = std::fs::OpenOptions::new()
        .append(true)
        .open(root.join("cgroup.subtree_control"))
        .is_ok();
    can_write.then_some(root)
}

/// Whether `root` offers every controller a limit needs.
fn has_controllers(root: &Path) -> bool {
    let Ok(available) = std::fs::read_to_string(root.join("cgroup.controllers")) else {
        return false;
    };
    let available: Vec<&str> = available.split_whitespace().collect();
    REQUIRED_CONTROLLERS
        .iter()
        .all(|needed| available.contains(needed))
}

/// Whether a cgroup this process can write limits into is reachable.
#[must_use]
pub(crate) fn available() -> bool {
    delegated_cgroup().is_some_and(|root| has_controllers(&root))
}

/// Whether every limit in `resources` can be enforced here.
///
/// `max_cpu_time` and `max_file_size` ride `setrlimit`, which needs no
/// delegation. Memory and process counts need a cgroup, and a host
/// without one gets `Partial` naming what it cannot do rather than a
/// limit nobody set.
#[must_use]
pub(crate) fn enforcement(resources: &Resources) -> Enforcement {
    let needs_cgroup = resources.max_memory.is_some() || resources.max_processes.is_some();
    if !needs_cgroup && !is_probe(resources) {
        return Enforcement::Full;
    }
    if available() {
        Enforcement::Full
    } else {
        Enforcement::Partial(
            "no delegated cgroup v2 with the memory and pids controllers: --max-memory-mb \
             and --max-processes are refused here, and a descendant that calls setsid \
             outlives the process-group kill"
                .to_string(),
        )
    }
}

/// Whether this call is the capability report asking what the host can
/// do, rather than a policy asking whether its own limits will hold.
///
/// The report has no limits to ask about, so it must answer for the
/// mechanism instead of trivially answering `full`.
fn is_probe(resources: &Resources) -> bool {
    resources == &Resources::default()
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
    pub(crate) fn create(resources: &Resources) -> Result<Option<Self>, String> {
        let Some(root) = delegated_cgroup().filter(|root| has_controllers(root)) else {
            return Ok(None);
        };
        move_supervisor_out_of(&root)?;
        enable_controllers(&root)?;

        let directory = root.join(format!(
            "gossamer-sandbox.{}.{}",
            std::process::id(),
            serial()
        ));
        std::fs::create_dir_all(&directory)
            .map_err(|error| format!("creating {}: {error}", directory.display()))?;

        if let Some(bytes) = resources.max_memory {
            write_control(&directory, "memory.max", &bytes.to_string())?;
            // Without this the kernel swaps rather than refusing, and a
            // memory limit that turns into disk traffic is not a limit.
            write_control(&directory, "memory.swap.max", "0")?;
        }
        if let Some(count) = resources.max_processes {
            write_control(&directory, "pids.max", &count.to_string())?;
        }

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

/// Moves this process into its own leaf so `root` holds no processes.
///
/// The postcondition is about `root`, not about any single member: the
/// list is a snapshot of a directory other processes share, and a pid
/// in it may exit or be moved by a concurrent supervisor before the
/// write reaches it. `ESRCH` reports exactly that, which is the state
/// the move was asking for, so it counts as done.
fn move_supervisor_out_of(root: &Path) -> Result<(), String> {
    let leaf = root.join(SUPERVISOR_LEAF);
    if !leaf.is_dir() {
        std::fs::create_dir_all(&leaf)
            .map_err(|error| format!("creating {}: {error}", leaf.display()))?;
    }
    let members =
        std::fs::read_to_string(root.join("cgroup.procs")).unwrap_or_else(|_| String::new());
    for pid in members.lines().filter(|line| !line.trim().is_empty()) {
        place(&leaf, pid.trim())?;
    }
    Ok(())
}

/// Writes one pid into `leaf`, treating a process that is no longer
/// there as a move that no longer needs doing.
fn place(leaf: &Path, pid: &str) -> Result<(), String> {
    match std::fs::write(leaf.join("cgroup.procs"), pid) {
        Ok(()) => Ok(()),
        Err(error) if error.raw_os_error() == Some(libc::ESRCH) => Ok(()),
        Err(error) => Err(format!("moving {pid} into {}: {error}", leaf.display())),
    }
}

/// Turns on the controllers a limit needs for `root`'s children.
fn enable_controllers(root: &Path) -> Result<(), String> {
    let enabled = std::fs::read_to_string(root.join("cgroup.subtree_control")).unwrap_or_default();
    let enabled: Vec<&str> = enabled.split_whitespace().collect();
    for controller in REQUIRED_CONTROLLERS {
        if enabled.contains(&controller) {
            continue;
        }
        std::fs::write(
            root.join("cgroup.subtree_control"),
            format!("+{controller}"),
        )
        .map_err(|error| {
            format!(
                "enabling the {controller} controller in {}: {error}",
                root.display()
            )
        })?;
    }
    Ok(())
}

fn write_control(directory: &Path, name: &str, value: &str) -> Result<(), String> {
    std::fs::write(directory.join(name), value).map_err(|error| {
        format!(
            "writing {} = {value}: {error}",
            directory.join(name).display()
        )
    })
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

    #[test]
    fn a_policy_asking_for_no_limits_needs_no_delegation() {
        let resources = Resources {
            max_file_size: Some(1 << 20),
            ..Resources::default()
        };
        assert_eq!(enforcement(&resources), Enforcement::Full);
    }

    #[test]
    fn a_memory_limit_reports_what_the_host_can_do() {
        let resources = Resources {
            max_memory: Some(1 << 30),
            ..Resources::default()
        };
        // Either answer is correct depending on the host; what must
        // never happen is a limit reported as enforced while nothing
        // is written.
        match enforcement(&resources) {
            Enforcement::Full => assert!(available()),
            Enforcement::Partial(reason) => {
                assert!(!available());
                assert!(reason.contains("delegated cgroup"), "{reason}");
            }
            Enforcement::None => panic!("a memory limit is never silently unreported"),
        }
    }

    /// The delegated cgroup is shared with every other process in the
    /// slice, so a pid read from it may be gone by the time the move
    /// reaches it. That is the state the move wanted, and a run must
    /// not fail because a neighbour exited first.
    #[test]
    fn a_member_that_is_already_gone_is_not_a_failure() {
        let Some(root) = delegated_cgroup().filter(|root| has_controllers(root)) else {
            return;
        };
        let leaf = root.join(SUPERVISOR_LEAF);
        if std::fs::create_dir_all(&leaf).is_err() {
            return;
        }
        // `pid_max` is an exclusive bound, so this pid can never name a
        // live process and the write answers ESRCH.
        let unassignable = std::fs::read_to_string("/proc/sys/kernel/pid_max").map_or_else(
            |_| 4_194_304,
            |text| text.trim().parse().unwrap_or(4_194_304),
        );
        assert_eq!(place(&leaf, &unassignable.to_string()), Ok(()));
    }

    /// The capability report asks with no limits set, and must still
    /// answer for the mechanism rather than trivially reporting `full`.
    #[test]
    fn the_capability_probe_answers_for_the_mechanism() {
        let reported = enforcement(&Resources::default());
        if available() {
            assert_eq!(reported, Enforcement::Full);
        } else {
            assert!(matches!(reported, Enforcement::Partial(_)), "{reported}");
        }
    }
}
