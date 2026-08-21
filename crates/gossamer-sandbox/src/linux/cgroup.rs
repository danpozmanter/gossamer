//! cgroup v2 limits, and the delegation check that decides whether
//! they are worth setting.
//!
//! Setting a limit that silently does nothing is worse than reporting
//! no limits at all, so the enforcement report answers from what this
//! process can actually write.

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
    let writable = root.join("cgroup.subtree_control");
    // Delegation is exactly the ability to write the controls; a
    // readable-but-not-writable directory is the undelegated case.
    let can_write = std::fs::OpenOptions::new()
        .append(true)
        .open(&writable)
        .is_ok();
    can_write.then_some(root)
}

/// Whether any resource limit in `resources` can be enforced here.
#[must_use]
pub(crate) fn enforcement(resources: &crate::policy::Resources) -> crate::level::Enforcement {
    let asks_for_nothing = resources.max_memory.is_none()
        && resources.max_processes.is_none()
        && resources.max_cpu_time.is_none()
        && resources.max_file_size.is_none();
    // A timeout is enforced by the supervisor, not by a cgroup, so it
    // is honored whatever the delegation looks like.
    if asks_for_nothing {
        return crate::level::Enforcement::Full;
    }
    // `max_cpu_time` and `max_file_size` ride `setrlimit`, which needs
    // no delegation; memory and process counts need a cgroup.
    let needs_cgroup = resources.max_memory.is_some() || resources.max_processes.is_some();
    if !needs_cgroup {
        return crate::level::Enforcement::Full;
    }
    if delegated_cgroup().is_some() {
        crate::level::Enforcement::Full
    } else {
        crate::level::Enforcement::Partial(
            "no delegated cgroup v2: memory and process-count limits are not enforceable \
             without one, so they are not set"
                .to_string(),
        )
    }
}

#[cfg(test)]
mod cgroup_tests {
    use super::*;
    use crate::level::Enforcement;
    use crate::policy::Resources;

    #[test]
    fn a_policy_asking_for_no_limits_is_fully_enforced() {
        assert_eq!(enforcement(&Resources::default()), Enforcement::Full);
    }

    #[test]
    fn rlimit_backed_limits_need_no_delegation() {
        let resources = Resources {
            max_file_size: Some(1 << 20),
            ..Resources::default()
        };
        assert_eq!(enforcement(&resources), Enforcement::Full);
    }

    #[test]
    fn a_memory_limit_reports_honestly_when_no_cgroup_is_delegated() {
        let resources = Resources {
            max_memory: Some(1 << 30),
            ..Resources::default()
        };
        let reported = enforcement(&resources);
        // Either answer is correct depending on the host; what must
        // never happen is a limit reported as enforced while nothing
        // is written.
        match reported {
            Enforcement::Full => assert!(delegated_cgroup().is_some()),
            Enforcement::Partial(reason) => {
                assert!(delegated_cgroup().is_none());
                assert!(reason.contains("delegated cgroup"), "{reason}");
            }
            Enforcement::None => panic!("a memory limit is never silently unreported"),
        }
    }
}
