//! The two policies the toolchain ships, as constructors.
//!
//! Both consumers and the `std::sandbox` module build these, so there
//! is one definition of "the build policy" and one of "the default
//! command policy" rather than three that drift. A program that
//! reassembles a dozen grants by hand will get one wrong.

use std::path::{Path, PathBuf};

use crate::level::Level;
use crate::policy::{Network, SandboxPolicy, Temp};

/// Credential files and directories no policy grants.
///
/// Denied by name as well as by never being granted: a profile that
/// grants `$HOME` would otherwise reach them, and the denial is what
/// makes that impossible rather than merely unlikely.
#[must_use]
pub fn credential_paths() -> Vec<PathBuf> {
    let Some(home) = home_directory() else {
        return Vec::new();
    };
    [
        ".ssh",
        ".aws",
        ".gnupg",
        ".netrc",
        ".docker",
        ".config/gh",
        ".config/gcloud",
        ".kube",
        ".npmrc",
        ".pypirc",
        ".cargo/credentials.toml",
        ".m2/settings.xml",
        ".m2/settings-security.xml",
        ".gradle/gradle.properties",
        ".git-credentials",
    ]
    .iter()
    .map(|leaf| home.join(leaf))
    .collect()
}

/// The caller's home directory.
#[must_use]
pub fn home_directory() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|path| !path.as_os_str().is_empty())
}

/// Environment variables every policy passes through when set.
///
/// Small on purpose: everything not here is dropped, which is what
/// makes `SSH_AUTH_SOCK`, `AWS_*`, `GITHUB_TOKEN`, and
/// `CARGO_REGISTRY_TOKEN` unreachable by construction rather than by a
/// denylist somebody has to keep current.
#[must_use]
pub fn base_environment() -> Vec<String> {
    [
        "PATH",
        "HOME",
        "USERPROFILE",
        "TMPDIR",
        "TEMP",
        "TMP",
        "LANG",
        "LC_ALL",
        "TZ",
        "TERM",
    ]
    .iter()
    .map(|name| (*name).to_string())
    .collect()
}

/// Directories a command needs merely to start: the system libraries,
/// the loader, and every directory on `PATH`.
#[must_use]
pub fn system_read_paths() -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = Vec::new();
    for candidate in [
        "/usr",
        "/bin",
        "/sbin",
        "/lib",
        "/lib64",
        "/opt",
        "/etc/alternatives",
        "/etc/ld.so.cache",
        "/etc/ld.so.conf",
        "/etc/ld.so.conf.d",
        "/etc/localtime",
        "/etc/ssl",
        "/etc/pki",
        "/etc/passwd",
        "/etc/group",
        "/System/Library",
        "/Library/Developer",
        "/private/var/db/dyld",
        // `/proc/self/exe`, `/proc/self/maps`, and the cpu and memory
        // files are read by nearly every toolchain, so `/proc` is
        // granted rather than left out. At `strict` this is the
        // private `/proc` of the run's own PID namespace; at
        // `standard` it is the host's, which is a documented limit
        // rather than an oversight.
        "/proc",
        // Narrowly, for the toolchains that size their thread pools
        // from the machine rather than from `nproc`.
        "/sys/devices/system/cpu",
        "/sys/fs/cgroup",
    ] {
        let path = PathBuf::from(candidate);
        if path.canonicalize().is_ok() {
            paths.push(path);
        }
    }
    if let Some(value) = std::env::var_os("PATH") {
        for entry in std::env::split_paths(&value) {
            if entry.canonicalize().is_ok() {
                paths.push(entry);
            }
        }
    }
    paths
}

/// Device nodes a program needs to run at all, granted individually
/// rather than by granting `/dev`.
///
/// Granting `/dev` wholesale would hand over `/dev/mem`, the raw disks,
/// and `/dev/kvm`; naming the safe nodes denies those by never granting
/// them, which is identical on every backend.
#[must_use]
pub fn device_paths() -> Vec<PathBuf> {
    [
        "/dev/null",
        "/dev/zero",
        "/dev/full",
        "/dev/random",
        "/dev/urandom",
        "/dev/tty",
        "/dev/ptmx",
        "/dev/pts",
        "/dev/shm",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|path| path.canonicalize().is_ok())
    .collect()

    // `/dev/stdin`, `/dev/stdout`, `/dev/stderr`, and `/dev/fd` are
    // deliberately absent: each is a per-process symlink into
    // `/proc/self/fd`, so it resolves to whatever the caller's stream
    // happens to be. The child already holds those descriptors, and a
    // descriptor is outside every filesystem policy by construction.
}

/// Files a networked phase needs to resolve a name and verify a
/// certificate.
///
/// A policy that grants the network but not these fails at DNS, which
/// is a confusing way to fail.
#[must_use]
pub fn resolver_read_paths() -> Vec<PathBuf> {
    [
        "/etc/resolv.conf",
        "/etc/hosts",
        "/etc/nsswitch.conf",
        "/etc/services",
    ]
    .iter()
    .map(PathBuf::from)
    .filter(|path| path.canonicalize().is_ok())
    .collect()
}

impl SandboxPolicy {
    /// The default policy for a command run with no flags.
    ///
    /// Two choices here are deliberate. The **working directory is
    /// read-write**, because the overwhelming use is "run this build or
    /// tool here" and what needs protecting is the rest of the machine;
    /// a read-only default would put `--rw .` on every real invocation
    /// and train people to stop reading flags. **Network is denied**,
    /// because per-command network denial is the thing almost nothing
    /// else gives you and the reason to reach for a sandbox at all.
    /// Both are stated in the run banner, not only under `--explain`.
    #[must_use]
    pub fn command_default(working_directory: &Path) -> Self {
        let mut policy = Self::new()
            .read_write(working_directory)
            .working_directory(working_directory)
            .temp(Temp::Private)
            .network(Network::None)
            .env_allow(base_environment())
            .level(Level::Standard);
        for path in system_read_paths() {
            policy = policy.read_only(path);
        }
        for path in device_paths() {
            policy = policy.read_write(path);
        }
        for path in credential_paths() {
            policy = policy.deny(path);
        }
        policy
    }

    /// The policy `gos build --sandbox` compiles under.
    ///
    /// The project root is read-write because a build writes into it:
    /// `target/` and `.gos-cache/` are created there, so a read-only
    /// source tree fails every build. The global cache roots are
    /// read-write for the same reason, and the toolchain is read-only.
    #[must_use]
    pub fn build_default(
        project_root: &Path,
        cache_roots: &[PathBuf],
        toolchain: &[PathBuf],
    ) -> Self {
        let mut policy = Self::new()
            .read_write(project_root)
            .working_directory(project_root)
            .temp(Temp::Private)
            .network(Network::None)
            .env_allow(build_environment())
            .level(Level::Standard);
        for root in cache_roots {
            if root.canonicalize().is_ok() {
                policy = policy.read_write(root);
            }
        }
        for path in toolchain {
            if path.canonicalize().is_ok() {
                policy = policy.read_only(path);
            }
        }
        for path in system_read_paths() {
            policy = policy.read_only(path);
        }
        for path in device_paths() {
            policy = policy.read_write(path);
        }
        for path in credential_paths() {
            policy = policy.deny(path);
        }
        policy
    }

    /// Adds what a networked fetch phase needs on top of a build
    /// policy: the resolver files, the CA bundle, and the network.
    #[must_use]
    pub fn for_fetch_phase(mut self) -> Self {
        // A fetch connects out and never listens, so client access is
        // what the phase needs; a caller that asked for more keeps it.
        if self.network == Network::None {
            self = self.network(Network::Client);
        }
        for path in resolver_read_paths() {
            self = self.read_only(path);
        }
        self
    }
}

/// Environment a build passes through, on top of [`base_environment`].
///
/// `CARGO_REGISTRY_TOKEN` is absent by construction rather than
/// denied. `SOURCE_DATE_EPOCH` and `GOS_CACHE_DIR` are present because
/// without the first `--reproducible` breaks under the sandbox, and
/// without the second the sandbox grants one cache root while the
/// build writes to another.
#[must_use]
pub fn build_environment() -> Vec<String> {
    let mut names = base_environment();
    names.extend(
        [
            "SOURCE_DATE_EPOCH",
            "GOS_CACHE_DIR",
            "GOS_LLC",
            "GOS_LLVM_OPT",
            "RUSTC",
            "RUSTFLAGS",
            "RUSTDOCFLAGS",
            "RUSTUP_HOME",
            "RUSTUP_TOOLCHAIN",
            "CARGO_HOME",
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_JOBS",
            "CC",
            "CXX",
            "AR",
            "CFLAGS",
            "CXXFLAGS",
            "LDFLAGS",
            "LD_LIBRARY_PATH",
            "PKG_CONFIG_PATH",
            "MACOSX_DEPLOYMENT_TARGET",
            "SDKROOT",
            "DEVELOPER_DIR",
            "VCINSTALLDIR",
            "WindowsSdkDir",
            "INCLUDE",
            "LIB",
        ]
        .iter()
        .map(|name| (*name).to_string()),
    );
    names
}

#[cfg(test)]
mod preset_tests {
    use super::*;
    use crate::policy::Access;

    #[test]
    fn the_command_default_grants_the_working_directory_and_denies_the_network() {
        let cwd = std::env::temp_dir().canonicalize().expect("canonicalize");
        let policy = SandboxPolicy::command_default(&cwd);
        assert_eq!(policy.network, Network::None);
        let compiled = policy.compile().expect("compile");
        assert_eq!(compiled.access(&cwd), Access::ReadWrite);
    }

    #[test]
    fn no_preset_grants_a_credential_path() {
        let Some(home) = home_directory() else {
            return;
        };
        let cwd = std::env::temp_dir().canonicalize().expect("canonicalize");
        let compiled = SandboxPolicy::command_default(&cwd)
            .compile()
            .expect("compile");
        assert_eq!(compiled.access(&home.join(".ssh")), Access::Deny);
        assert_eq!(compiled.access(&home.join(".aws")), Access::Deny);
    }

    #[test]
    fn the_safe_device_nodes_are_granted_and_the_dangerous_ones_are_not() {
        let cwd = std::env::temp_dir().canonicalize().expect("canonicalize");
        let compiled = SandboxPolicy::command_default(&cwd)
            .compile()
            .expect("compile");
        // Device nodes are a POSIX concept, so on Windows none of them
        // exist and none are granted; the denials below hold anywhere.
        #[cfg(unix)]
        {
            assert_eq!(compiled.access(Path::new("/dev/null")), Access::ReadWrite);
            assert_eq!(
                compiled.access(Path::new("/dev/urandom")),
                Access::ReadWrite
            );
        }
        assert_eq!(compiled.access(Path::new("/dev/mem")), Access::Deny);
        assert_eq!(compiled.access(Path::new("/dev/kvm")), Access::Deny);
    }

    #[test]
    fn the_build_environment_keeps_what_reproducible_builds_need() {
        let names = build_environment();
        assert!(names.contains(&"SOURCE_DATE_EPOCH".to_string()));
        assert!(names.contains(&"GOS_CACHE_DIR".to_string()));
    }

    #[test]
    fn no_preset_passes_a_registry_token() {
        assert!(!build_environment().contains(&"CARGO_REGISTRY_TOKEN".to_string()));
        assert!(!build_environment().contains(&"GITHUB_TOKEN".to_string()));
        assert!(!build_environment().contains(&"SSH_AUTH_SOCK".to_string()));
    }

    #[test]
    fn the_fetch_phase_adds_the_network_and_what_resolving_a_name_needs() {
        let cwd = std::env::temp_dir().canonicalize().expect("canonicalize");
        let policy = SandboxPolicy::command_default(&cwd).for_fetch_phase();
        // A fetch connects out and never listens.
        assert_eq!(policy.network, Network::Client);
        for path in resolver_read_paths() {
            assert!(
                policy.read_only_paths.contains(&path),
                "{} must be readable when the network is allowed",
                path.display()
            );
        }
    }
}
