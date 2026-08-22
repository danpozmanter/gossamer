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
const BPF_ALU: u16 = 0x04;
const BPF_AND: u16 = 0x50;
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
/// Byte offset of the low half of `args[0]` inside `struct
/// seccomp_data`, which is where a little-endian load of a flag word
/// finds it.
const OFFSET_ARG0: u32 = 16;

/// `CLONE_NEWUSER`, as a mask the filter can test.
#[allow(
    clippy::cast_sign_loss,
    reason = "the constant is a single positive flag bit"
)]
const CLONE_NEWUSER: u32 = libc::CLONE_NEWUSER as u32;

#[cfg(target_arch = "x86_64")]
const AUDIT_ARCH: u32 = 0xc000_003e;
#[cfg(target_arch = "aarch64")]
const AUDIT_ARCH: u32 = 0xc000_00b7;
#[cfg(not(any(target_arch = "x86_64", target_arch = "aarch64")))]
const AUDIT_ARCH: u32 = 0;

/// One entry of the deny-list.
#[derive(Clone, Copy)]
struct Rule {
    number: libc::c_long,
    verdict: u32,
    /// A bit of the first argument that has to be set for the verdict
    /// to apply, for a syscall whose argument decides whether the call
    /// is the one being refused. `None` refuses it whatever it was
    /// asked to do.
    flag: Option<u32>,
}

impl Rule {
    /// Ends the process: the call has no legitimate use inside a build.
    const fn kill(number: libc::c_long) -> Self {
        Self {
            number,
            verdict: SECCOMP_RET_KILL_PROCESS,
            flag: None,
        }
    }

    /// Answers `EPERM`: something a program might legitimately probe
    /// for and handle.
    const fn deny(number: libc::c_long) -> Self {
        Self {
            number,
            verdict: SECCOMP_RET_ERRNO | (libc::EPERM as u32),
            flag: None,
        }
    }

    /// Answers `EPERM` only when the first argument carries `flag`.
    const fn deny_when(number: libc::c_long, flag: u32) -> Self {
        Self {
            number,
            verdict: SECCOMP_RET_ERRNO | (libc::EPERM as u32),
            flag: Some(flag),
        }
    }
}

/// Syscalls the filter refuses, and why each is on the list.
///
/// The verdict is `EPERM` rather than a kill for everything a program
/// might legitimately probe and handle, and a kill for the families
/// that have no legitimate use inside a build.
fn denied_syscalls() -> Vec<Rule> {
    vec![
        // Debugging another process is how a sandboxed build reads a
        // sibling's memory, including one holding credentials.
        Rule::kill(libc::SYS_ptrace),
        // The same read and write without `ptrace`. Listing one and
        // not the others would leave the reason for the first unmet.
        Rule::kill(libc::SYS_process_vm_readv),
        Rule::kill(libc::SYS_process_vm_writev),
        // Opening a file from an opaque handle bypasses path lookup, so
        // no path policy on any backend can see it.
        Rule::kill(libc::SYS_open_by_handle_at),
        // Loading a kernel module is a direct escape.
        Rule::kill(libc::SYS_init_module),
        Rule::kill(libc::SYS_finit_module),
        Rule::kill(libc::SYS_delete_module),
        // Rebooting or changing the machine's clock and hostname is
        // never part of compiling something.
        Rule::kill(libc::SYS_reboot),
        Rule::kill(libc::SYS_settimeofday),
        Rule::kill(libc::SYS_clock_settime),
        Rule::kill(libc::SYS_clock_adjtime),
        Rule::deny(libc::SYS_sethostname),
        Rule::deny(libc::SYS_setdomainname),
        // Kernel keyring access reaches credentials the filesystem
        // policy cannot see.
        Rule::deny(libc::SYS_add_key),
        Rule::deny(libc::SYS_request_key),
        Rule::deny(libc::SYS_keyctl),
        // Swap, quota, and raw device administration.
        Rule::kill(libc::SYS_swapon),
        Rule::kill(libc::SYS_swapoff),
        Rule::deny(libc::SYS_quotactl),
        // Re-entering another process's namespaces would undo the
        // isolation the sandbox just established.
        Rule::kill(libc::SYS_setns),
        // NUMA and scheduler policy changes affect the whole machine.
        Rule::deny(libc::SYS_mbind),
        Rule::deny(libc::SYS_migrate_pages),
        Rule::deny(libc::SYS_move_pages),
        // `userfaultfd` and `bpf` are recurring local-escalation
        // primitives with no place in a build.
        Rule::deny(libc::SYS_userfaultfd),
        Rule::deny(libc::SYS_bpf),
        // Performance counters expose other processes.
        Rule::deny(libc::SYS_perf_event_open),
        // `kexec` replaces the running kernel.
        Rule::kill(libc::SYS_kexec_load),
        // A nested user namespace carries `CAP_SYS_ADMIN` inside it,
        // which is the standing precondition of most of the kernel
        // surface the entries above remove. Only that flag is refused:
        // the other namespace kinds need a user namespace before an
        // unprivileged process can ask for them at all.
        //
        // `clone3` takes its flags through a pointer, and classic BPF
        // dereferences nothing, so that spelling is out of reach here
        // and named in `docs/limits.md`.
        Rule::deny_when(libc::SYS_unshare, CLONE_NEWUSER),
        Rule::deny_when(libc::SYS_clone, CLONE_NEWUSER),
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
    let mut filter = Vec::with_capacity(denied.len() * 6 + 5);
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
    for rule in denied {
        let Ok(number) = u32::try_from(rule.number) else {
            continue;
        };
        let Some(flag) = rule.flag else {
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
                k: rule.verdict,
            });
            continue;
        };
        // The accumulator holds the syscall number for every
        // comparison in this loop, so a rule that has to look at an
        // argument loads it and then loads the number back: both the
        // "not this syscall" and the "flag not set" paths land on that
        // reload.
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 4,
            k: number,
        });
        filter.push(SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: OFFSET_ARG0,
        });
        filter.push(SockFilter {
            code: BPF_ALU | BPF_AND | BPF_K,
            jt: 0,
            jf: 0,
            k: flag,
        });
        filter.push(SockFilter {
            code: BPF_JMP | BPF_JEQ | BPF_K,
            jt: 0,
            jf: 1,
            k: flag,
        });
        filter.push(SockFilter {
            code: BPF_RET | BPF_K,
            jt: 0,
            jf: 0,
            k: rule.verdict,
        });
        filter.push(SockFilter {
            code: BPF_LD | BPF_W | BPF_ABS,
            jt: 0,
            jf: 0,
            k: OFFSET_NR,
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
        let mut numbers: Vec<libc::c_long> = denied_syscalls()
            .into_iter()
            .map(|rule| rule.number)
            .collect();
        let before = numbers.len();
        numbers.sort_unstable();
        numbers.dedup();
        assert_eq!(before, numbers.len(), "a syscall is listed twice");
    }

    fn verdict_for(number: libc::c_long) -> Option<u32> {
        denied_syscalls()
            .iter()
            .find(|rule| rule.number == number)
            .map(|rule| rule.verdict)
    }

    #[test]
    fn ptrace_and_setns_are_refused_outright() {
        assert_eq!(
            verdict_for(libc::SYS_ptrace),
            Some(SECCOMP_RET_KILL_PROCESS)
        );
        assert_eq!(verdict_for(libc::SYS_setns), Some(SECCOMP_RET_KILL_PROCESS));
    }

    /// A nested user namespace is refused, and only for that flag: a
    /// blanket refusal of `unshare` would deny forms an unprivileged
    /// process cannot reach anyway, and would say so in a failure the
    /// caller then has to explain.
    #[test]
    fn a_nested_user_namespace_is_refused_by_the_flag_it_asks_for() {
        for number in [libc::SYS_unshare, libc::SYS_clone] {
            let rule = denied_syscalls()
                .into_iter()
                .find(|rule| rule.number == number)
                .expect("the user-namespace flag is refused");
            assert_eq!(rule.flag, Some(CLONE_NEWUSER));
        }
    }

    /// A rule that reads an argument leaves the accumulator holding
    /// that argument, so every path out of it has to load the syscall
    /// number back before the next comparison reads it.
    #[test]
    fn a_rule_that_reads_an_argument_restores_the_syscall_number() {
        let filter = program();
        let loads_argument = filter
            .iter()
            .position(|instruction| {
                instruction.code == BPF_LD | BPF_W | BPF_ABS && instruction.k == OFFSET_ARG0
            })
            .expect("an argument-reading rule is compiled");
        let reload = filter[loads_argument + 4];
        assert_eq!(reload.code, BPF_LD | BPF_W | BPF_ABS);
        assert_eq!(reload.k, OFFSET_NR);
        let skip_to_reload = filter[loads_argument - 1];
        assert_eq!(usize::from(skip_to_reload.jf), 4);
    }

    /// `ptrace` is on the list because it reads another process's
    /// memory. Anything else that does the same has to be there too, or
    /// the entry is a name rather than a rule.
    #[test]
    fn every_way_to_read_another_processs_memory_is_refused() {
        for number in [
            libc::SYS_ptrace,
            libc::SYS_process_vm_readv,
            libc::SYS_process_vm_writev,
        ] {
            assert_eq!(
                verdict_for(number),
                Some(SECCOMP_RET_KILL_PROCESS),
                "syscall {number} reads another process and is not refused"
            );
        }
    }
}
