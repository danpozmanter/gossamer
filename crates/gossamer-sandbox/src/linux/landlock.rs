//! Landlock: per-path filesystem rights, inherited by every
//! descendant, installable without privilege.
//!
//! Reached through the raw syscalls rather than a wrapper crate. The
//! ABI is three syscalls and two structs, and a security boundary is
//! better owned than depended upon; the part worth care is the ABI
//! negotiation below, which is written out explicitly rather than
//! hidden behind a version constant.

#![allow(
    unsafe_code,
    reason = "Landlock has no libc wrapper; every call below passes a \
              stack-owned attribute struct with its size, which is the \
              documented syscall contract"
)]

use std::ffi::CString;
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};

use crate::policy::{Access, Network, PathRule};

// Syscall numbers. Stable across every architecture Landlock exists
// on, and identical in the kernel's asm-generic table.
const SYS_LANDLOCK_CREATE_RULESET: libc::c_long = 444;
const SYS_LANDLOCK_ADD_RULE: libc::c_long = 445;
const SYS_LANDLOCK_RESTRICT_SELF: libc::c_long = 446;

/// Asks `landlock_create_ruleset` for the ABI version instead of a
/// ruleset.
const LANDLOCK_CREATE_RULESET_VERSION: u32 = 1 << 0;

/// Rule type: rights beneath a directory or on a file.
const LANDLOCK_RULE_PATH_BENEATH: libc::c_int = 1;

// Filesystem access rights, by the ABI that introduced each.
const FS_EXECUTE: u64 = 1 << 0;
const FS_WRITE_FILE: u64 = 1 << 1;
const FS_READ_FILE: u64 = 1 << 2;
const FS_READ_DIR: u64 = 1 << 3;
const FS_REMOVE_DIR: u64 = 1 << 4;
const FS_REMOVE_FILE: u64 = 1 << 5;
const FS_MAKE_CHAR: u64 = 1 << 6;
const FS_MAKE_DIR: u64 = 1 << 7;
const FS_MAKE_REG: u64 = 1 << 8;
const FS_MAKE_SOCK: u64 = 1 << 9;
const FS_MAKE_FIFO: u64 = 1 << 10;
const FS_MAKE_BLOCK: u64 = 1 << 11;
const FS_MAKE_SYM: u64 = 1 << 12;
/// ABI 2.
const FS_REFER: u64 = 1 << 13;
/// ABI 3.
const FS_TRUNCATE: u64 = 1 << 14;
/// ABI 5.
const FS_IOCTL_DEV: u64 = 1 << 15;

/// ABI 4: TCP restriction, which covers `bind` and `connect` and
/// nothing else - no UDP, no unix sockets, no raw. Network denial is
/// the network namespace; these are a second layer inside it.
const NET_BIND_TCP: u64 = 1 << 0;
const NET_CONNECT_TCP: u64 = 1 << 1;

#[repr(C)]
struct RulesetAttr {
    handled_access_fs: u64,
    handled_access_net: u64,
    scoped: u64,
}

#[repr(C)]
struct PathBeneathAttr {
    allowed_access: u64,
    parent_fd: i32,
}

/// The Landlock ABI this kernel reports, or `None` when Landlock is
/// absent or disabled.
#[must_use]
pub(crate) fn abi_version() -> Option<u32> {
    let version = unsafe {
        libc::syscall(
            SYS_LANDLOCK_CREATE_RULESET,
            std::ptr::null::<RulesetAttr>(),
            0usize,
            LANDLOCK_CREATE_RULESET_VERSION,
        )
    };
    u32::try_from(version).ok().filter(|value| *value > 0)
}

/// Filesystem rights an `abi` understands. Requesting a right the
/// kernel does not know makes the whole ruleset fail, so the set is
/// derived from what the host reports rather than from what this build
/// was compiled against.
fn handled_fs(abi: u32) -> u64 {
    let mut rights = FS_EXECUTE
        | FS_WRITE_FILE
        | FS_READ_FILE
        | FS_READ_DIR
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_CHAR
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_MAKE_SOCK
        | FS_MAKE_FIFO
        | FS_MAKE_BLOCK
        | FS_MAKE_SYM;
    if abi >= 2 {
        rights |= FS_REFER;
    }
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    if abi >= 5 {
        rights |= FS_IOCTL_DEV;
    }
    rights
}

/// Rights that only mean something on a directory.
///
/// `landlock_add_rule` refuses a rule that names a regular file and
/// asks for one of these, so a grant on a file is masked down to what
/// a file can carry.
const DIRECTORY_ONLY: u64 = FS_READ_DIR
    | FS_REMOVE_DIR
    | FS_REMOVE_FILE
    | FS_MAKE_CHAR
    | FS_MAKE_DIR
    | FS_MAKE_REG
    | FS_MAKE_SOCK
    | FS_MAKE_FIFO
    | FS_MAKE_BLOCK
    | FS_MAKE_SYM
    | FS_REFER;

/// Rights a read-only grant confers.
fn read_rights(abi: u32) -> u64 {
    let mut rights = FS_EXECUTE | FS_READ_FILE | FS_READ_DIR;
    if abi >= 5 {
        rights |= FS_IOCTL_DEV;
    }
    rights
}

/// Rights a read-write grant confers.
fn write_rights(abi: u32) -> u64 {
    let mut rights = read_rights(abi)
        | FS_WRITE_FILE
        | FS_REMOVE_DIR
        | FS_REMOVE_FILE
        | FS_MAKE_CHAR
        | FS_MAKE_DIR
        | FS_MAKE_REG
        | FS_MAKE_SOCK
        | FS_MAKE_FIFO
        | FS_MAKE_BLOCK
        | FS_MAKE_SYM;
    if abi >= 2 {
        rights |= FS_REFER;
    }
    if abi >= 3 {
        rights |= FS_TRUNCATE;
    }
    rights
}

/// One grant, resolved to a C string and a rights mask before the
/// pre-exec window opens.
///
/// Everything the pre-exec code touches is precomputed here: between
/// `fork` and `exec` only async-signal-safe calls are permitted, and
/// building a `CString` allocates.
pub(crate) struct Grant {
    path: CString,
    rights: u64,
}

/// A ruleset compiled from the policy, ready to install.
pub(crate) struct Ruleset {
    grants: Vec<Grant>,
    handled_fs: u64,
    handled_net: u64,
}

impl Ruleset {
    /// Compiles `rules` against the ABI the host reports.
    ///
    /// Landlock is allow-list only: it has no deny rule, so a denial
    /// is enforced by the absence of a grant. The policy's rule
    /// ordering already resolved which paths are granted, so a denied
    /// path simply contributes nothing here.
    /// `network` is `None` when another mechanism holds the policy's
    /// network setting, in which case this ruleset handles no network
    /// access at all.
    pub(crate) fn compile(abi: u32, rules: &[PathRule], network: Option<Network>) -> Self {
        let denials: Vec<&Path> = rules
            .iter()
            .filter(|rule| rule.access == Access::Deny)
            .map(|rule| rule.path.as_path())
            .collect();
        let mut grants = Vec::new();
        for rule in rules {
            let rights = match rule.access {
                Access::ReadOnly => read_rights(abi),
                Access::ReadWrite => write_rights(abi),
                Access::Deny => continue,
            };
            for path in expand_around_denials(&rule.path, &denials) {
                let Ok(encoded) = CString::new(path.as_os_str().as_encoded_bytes()) else {
                    continue;
                };
                // Whether the path is a directory is settled here
                // rather than in the pre-exec window, which may not
                // allocate and may not call `stat`.
                let rights = if path.is_dir() {
                    rights
                } else {
                    rights & !DIRECTORY_ONLY
                };
                grants.push(Grant {
                    path: encoded,
                    rights,
                });
            }
        }
        let handled_net = if abi >= 4 {
            match network {
                Some(Network::None) => NET_BIND_TCP | NET_CONNECT_TCP,
                // No allow-rule is added for the handled access, so
                // every bind is refused while connect stays outside the
                // ruleset entirely.
                Some(Network::Client) => NET_BIND_TCP,
                // Landlock's net layer matches on port, never on
                // address, so it cannot tell a loopback bind from any
                // other one. Where a network namespace already holds
                // the setting, handling TCP here would only subtract
                // the child's own loopback.
                Some(Network::Open) | None => 0,
            }
        } else {
            0
        };
        Self {
            grants,
            handled_fs: handled_fs(abi),
            handled_net,
        }
    }

    /// Installs the ruleset on the calling process, where it is
    /// inherited by every descendant and can never be relaxed.
    ///
    /// Async-signal-safe: `open`, `syscall`, and `close` only, over
    /// data allocated before the fork.
    pub(crate) fn restrict_self(&self) -> Result<(), i32> {
        let attr = RulesetAttr {
            handled_access_fs: self.handled_fs,
            handled_access_net: self.handled_net,
            scoped: 0,
        };
        let ruleset_fd = unsafe {
            libc::syscall(
                SYS_LANDLOCK_CREATE_RULESET,
                std::ptr::from_ref(&attr),
                std::mem::size_of::<RulesetAttr>(),
                0u32,
            )
        };
        let ruleset_fd = RawFd::try_from(ruleset_fd).map_err(|_| -1)?;
        if ruleset_fd < 0 {
            return Err(errno());
        }
        for grant in &self.grants {
            let parent_fd = unsafe {
                libc::open(
                    grant.path.as_ptr(),
                    libc::O_PATH | libc::O_CLOEXEC | libc::O_NOFOLLOW,
                )
            };
            if parent_fd < 0 {
                // A grant whose path vanished between compile and
                // exec is not fatal: the absent path is unreachable,
                // which is the outcome the grant would have permitted
                // anyway.
                continue;
            }
            let beneath = PathBeneathAttr {
                allowed_access: grant.rights & self.handled_fs,
                parent_fd,
            };
            let added = unsafe {
                libc::syscall(
                    SYS_LANDLOCK_ADD_RULE,
                    ruleset_fd,
                    LANDLOCK_RULE_PATH_BENEATH,
                    std::ptr::from_ref(&beneath),
                    0u32,
                )
            };
            unsafe { libc::close(parent_fd) };
            if added != 0 {
                let error = errno();
                unsafe { libc::close(ruleset_fd) };
                return Err(error);
            }
        }
        let restricted = unsafe { libc::syscall(SYS_LANDLOCK_RESTRICT_SELF, ruleset_fd, 0u32) };
        let error = errno();
        unsafe { libc::close(ruleset_fd) };
        if restricted == 0 { Ok(()) } else { Err(error) }
    }
}

/// The paths to grant so that `granted` is reachable but nothing
/// under `denials` is.
///
/// Landlock is allow-list only: `landlock_add_rule` grants rights and
/// there is no deny rule, so a denial inside a grant cannot be
/// expressed directly. It is expressed by granting the granted
/// directory's other children instead, recursively down to each
/// denial's parent. That is fail-closed, and it is why the shipped
/// profiles are written as explicit allow-lists: this expansion
/// snapshots the directory, so a child created after the sandbox
/// starts is not granted here while it would be allowed on the
/// backends that have a real deny rule.
fn expand_around_denials(granted: &Path, denials: &[&Path]) -> Vec<PathBuf> {
    let inside: Vec<&Path> = denials
        .iter()
        .copied()
        .filter(|denial| denial.starts_with(granted) && *denial != granted)
        .collect();
    if inside.is_empty() {
        return vec![granted.to_path_buf()];
    }
    let Ok(entries) = std::fs::read_dir(granted) else {
        // A grant that cannot be listed cannot be expanded, and
        // granting it whole would let the denial through, so it
        // contributes nothing.
        return Vec::new();
    };
    let mut expanded = Vec::new();
    for entry in entries.filter_map(Result::ok) {
        let child = entry.path();
        if inside.iter().any(|denial| *denial == child) {
            continue;
        }
        if inside.iter().any(|denial| denial.starts_with(&child)) {
            expanded.extend(expand_around_denials(&child, denials));
        } else {
            expanded.push(child);
        }
    }
    expanded
}

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(-1)
}

#[cfg(test)]
mod landlock_tests {
    use super::*;

    #[test]
    fn requested_rights_never_exceed_what_the_abi_handles() {
        for abi in 1..=8 {
            assert_eq!(read_rights(abi) & !handled_fs(abi), 0);
            assert_eq!(write_rights(abi) & !handled_fs(abi), 0);
        }
    }

    #[test]
    fn refer_and_truncate_appear_only_at_the_abi_that_introduced_them() {
        assert_eq!(handled_fs(1) & FS_REFER, 0);
        assert_ne!(handled_fs(2) & FS_REFER, 0);
        assert_eq!(handled_fs(2) & FS_TRUNCATE, 0);
        assert_ne!(handled_fs(3) & FS_TRUNCATE, 0);
        assert_eq!(handled_fs(4) & FS_IOCTL_DEV, 0);
        assert_ne!(handled_fs(5) & FS_IOCTL_DEV, 0);
    }

    #[test]
    fn a_grant_on_a_regular_file_drops_the_directory_only_rights() {
        let file = std::env::temp_dir().join("gos-sandbox-landlock-file-grant");
        std::fs::write(&file, b"x").expect("write fixture");
        let rules = vec![PathRule {
            path: file,
            access: Access::ReadWrite,
        }];
        let compiled = Ruleset::compile(8, &rules, Some(Network::Open));
        assert_eq!(compiled.grants[0].rights & DIRECTORY_ONLY, 0);
        assert_ne!(compiled.grants[0].rights & FS_WRITE_FILE, 0);
    }

    #[test]
    fn a_denial_inside_a_grant_expands_to_the_directorys_other_children() {
        let root = std::env::temp_dir().join("gos-sandbox-expand-denials");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("keep")).expect("create fixture");
        std::fs::write(root.join("secret"), b"token").expect("create fixture");
        let root = root.canonicalize().expect("canonicalize");
        let secret = root.join("secret");

        let expanded = expand_around_denials(&root, &[secret.as_path()]);
        assert!(expanded.contains(&root.join("keep")));
        assert!(!expanded.contains(&secret));
        assert!(!expanded.contains(&root));
    }

    #[test]
    fn a_grant_with_no_denial_inside_it_is_left_whole() {
        let root = std::env::temp_dir();
        let elsewhere = PathBuf::from("/etc/shadow");
        assert_eq!(
            expand_around_denials(&root, &[elsewhere.as_path()]),
            vec![root]
        );
    }

    #[test]
    fn a_denied_rule_contributes_no_grant() {
        let rules = vec![PathRule {
            path: std::path::PathBuf::from("/etc"),
            access: Access::Deny,
        }];
        assert!(
            Ruleset::compile(4, &rules, Some(Network::None))
                .grants
                .is_empty()
        );
    }

    #[test]
    fn the_tcp_layer_is_requested_only_from_the_abi_that_has_it() {
        assert_eq!(Ruleset::compile(3, &[], Some(Network::None)).handled_net, 0);
        assert_ne!(Ruleset::compile(4, &[], Some(Network::None)).handled_net, 0);
        assert_eq!(
            Ruleset::compile(4, &[], Some(Network::Client)).handled_net,
            NET_BIND_TCP,
            "client handles bind only, so connect stays outside the ruleset"
        );
        assert_eq!(Ruleset::compile(4, &[], Some(Network::Open)).handled_net, 0);
    }
}
