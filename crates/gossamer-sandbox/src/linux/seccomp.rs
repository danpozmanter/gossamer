//! A seccomp filter that removes kernel surface a build never needs.
//!
//! This is not a filesystem or a network policy - Landlock and the
//! namespaces are - and it is not an allow-list of everything a
//! compiler calls, which would break a build the first time a
//! toolchain reached for a syscall nobody anticipated. It is a
//! deny-list of the syscall families that only exist to reach out of a
//! sandbox or to attack the kernel, each named with the reason it is
//! there.

#![allow(
    unsafe_code,
    reason = "seccomp is installed with a raw prctl over a BPF program \
              built before the fork; there is no safe wrapper"
)]

/// One instruction of a classic-BPF program, as `seccomp(2)` takes it.
#[repr(C)]
#[derive(Clone, Copy)]
struct SockFilter {
    code: u16,
    jt: u8,
    jf: u8,
    k: u32,
}

#[repr(C)]
struct SockFprog {
    len: u16,
    filter: *const SockFilter,
}

const BPF_LD: u16 = 0x00;
const BPF_JMP: u16 = 0x05;
const BPF_RET: u16 = 0x06;
const BPF_W: u16 = 0x00;
const BPF_ABS: u16 = 0x20;
const BPF_JEQ: u16 = 0x10;
const BPF_K: u16 = 0x00;

const SECCOMP_RET_KILL_PROCESS: u32 = 0x8000_0000;
const SECCOMP_RET_ALLOW: u32 = 0x7fff_0000;
const SECCOMP_RET_ERRNO: u32 = 0x0005_0000;

/// Byte offset of `nr` inside `struct seccomp_data`.
const OFFSET_NR: u32 = 0;
/// Byte offset of `arch` inside `struct seccomp_data`.
const OFFSET_ARCH: u32 = 4;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const AUDIT_ARCH: u32 = 0;

/// Syscalls the filter refuses, and why each is on the list.
///
/// The verdict is `EPERM` rather than a kill for everything a program
/// might legitimately probe and handle, and a kill for the families
/// that have no legitimate use inside a build.
fn denied_syscalls() -> Vec<(libc::c_long, u32)> {
    let kill = SECCOMP_RET_KILL_PROCESS;
    let deny = SECCOMP_RET_ERRNO | (libc::EPERM as u32);
    vec![
        // Debugging another process is how a sandboxed build reads a
        // sibling's memory, including one holding credentials.
        (libc::SYS_ptrace, kill),
        // Loading a kernel module is a direct escape.
        (libc::SYS_init_module, kill),
        (libc::SYS_finit_module, kill),
        (libc::SYS_delete_module, kill),
        // Rebooting or changing the machine's clock and hostname is
        // never part of compiling something.
        (libc::SYS_reboot, kill),
        (libc::SYS_settimeofday, kill),
        (libc::SYS_clock_settime, kill),
        (libc::SYS_clock_adjtime, kill),
        (libc::SYS_sethostname, deny),
        (libc::SYS_setdomainname, deny),
        // Kernel keyring access reaches credentials the filesystem
        // policy cannot see.
        (libc::SYS_add_key, deny),
        (libc::SYS_request_key, deny),
        (libc::SYS_keyctl, deny),
        // Swap, quota, and raw device administration.
        (libc::SYS_swapon, kill),
        (libc::SYS_swapoff, kill),
        (libc::SYS_quotactl, deny),
        // Re-entering another process's namespaces would undo the
        // isolation the sandbox just established.
        (libc::SYS_setns, kill),
        // NUMA and scheduler policy changes affect the whole machine.
        (libc::SYS_mbind, deny),
        (libc::SYS_migrate_pages, deny),
        (libc::SYS_move_pages, deny),
        // `userfaultfd` and `bpf` are recurring local-escalation
        // primitives with no place in a build.
        (libc::SYS_userfaultfd, deny),
        (libc::SYS_bpf, deny),
        // Performance counters expose other processes.
        (libc::SYS_perf_event_open, deny),
        // `kexec` replaces the running kernel.
        (libc::SYS_kexec_load, kill),
    ]
}

/// Builds the filter program.
///
/// A syscall issued under a different architecture personality would
/// carry different numbers, so the program kills anything whose `arch`
/// is not the one the numbers were compiled for before it looks at
/// `nr` at all.
fn program() -> Vec<SockFilter> {
    let denied = denied_syscalls();
    let mut filter = Vec::with_capacity(denied.len() * 2 + 5);
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_ARCH,
    });
    filter.push(SockFilter {
        code: BPF_JMP | BPF_JEQ | BPF_K,
        jt: 1,
        jf: 0,
        k: AUDIT_ARCH,
    });
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_KILL_PROCESS,
    });
    filter.push(SockFilter {
        code: BPF_LD | BPF_W | BPF_ABS,
        jt: 0,
        jf: 0,
        k: OFFSET_NR,
    });
    for (number, verdict) in denied {
        let Ok(number) = u32::try_from(number) else {
            continue;
        };
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: number,
        });
        filter.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: verdict,
        });
    }
    filter.push(SockFilter {
        code: BPF_RET | BPF_K,
        jt: 0,
        jf: 0,
        k: SECCOMP_RET_ALLOW,
    });
    filter
}

/// The filter, built before the fork so the pre-exec path only
/// installs it.
pub(crate) struct Filter(Vec<SockFilter>);

impl Filter {
    /// Compiles the filter.
    #[must_use]
    pub(crate) fn new() -> Self {
        Self(program())
    }

    /// Whether this architecture has syscall numbers the filter knows.
    #[must_use]
    pub(crate) fn is_supported() -> bool {
        AUDIT_ARCH != 0
    }

    /// Installs the filter on the calling process, where it is
    /// inherited across `exec` and by every descendant.
    ///
    /// `no_new_privs` must already be set; the kernel refuses an
    /// unprivileged filter otherwise.
    pub(crate) fn install(&self) -> Result<(), i32> {
        let program = SockFprog {
            len: u16::try_from(self.0.len()).unwrap_or(u16::MAX),
            filter: self.0.as_ptr(),
        };
        let installed = unsafe {
            libc::prctl(
                libc::PR_SET_SECCOMP,
                libc::SECCOMP_MODE_FILTER,
                std::ptr::from_ref(&program),
                0,
                0,
            )
        };
        if installed == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error().raw_os_error().unwrap_or(-1))
        }
    }

    /// Every syscall the filter refuses, for `--explain`.
    #[must_use]
    pub(crate) fn denied_count() -> usize {
        denied_syscalls().len()
    }
}

#[cfg(test)]
mod seccomp_tests {
    use super::*;

    #[test]
    fn the_program_checks_the_architecture_before_any_syscall_number() {
        let filter = program();
        assert_eq!(filter[0].k, OFFSET_ARCH);
        assert_eq!(filter[2].k, SECCOMP_RET_KILL_PROCESS);
        assert_eq!(filter[3].k, OFFSET_NR);
    }

    #[test]
    fn the_program_ends_by_allowing_what_it_did_not_name() {
        let filter = program();
        assert_eq!(
            filter.last().expect("non-empty program").k,
            SECCOMP_RET_ALLOW
        );
    }

    #[test]
    fn every_denied_syscall_is_documented_by_being_listed_once() {
        let mut numbers: Vec<libc::c_long> =
            denied_syscalls().into_iter().map(|(nr, _)| nr).collect();
        let before = numbers.len();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(before, numbers.len(), "a syscall is listed twice");
    }

    #[test]
    fn ptrace_and_setns_are_refused_outright() {
        let denied = denied_syscalls();
        let verdict = |nr| {
            denied
                .iter()
                .find(|(number, _)| *number == nr)
                .map(|(_, verdict)| *verdict)
        };
        assert_eq!(verdict(libc::SYS_ptrace), Some(SECCOMP_RET_KILL_PROCESS));
        assert_eq!(verdict(libc::SYS_setns), Some(SECCOMP_RET_KILL_PROCESS));
    }
}
