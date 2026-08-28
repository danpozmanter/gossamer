//! Adversarial enforcement tests: each case tries the thing and the
//! test passes when the attempt is denied.
//!
//! Happy-path tests prove a sandbox does not break a build. Only these
//! prove it contains one.

use std::path::{Path, PathBuf};

use gossamer_sandbox::{Level, Sandbox, SandboxPolicy, Stdio};
// `Network` is named only by the Linux cases, whose policies state a
// verdict the portable ones inherit from the preset.
#[cfg(target_os = "linux")]
use gossamer_sandbox::Network;

fn workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("gos-sandbox-enforce-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create workspace");
    root.canonicalize().expect("canonicalize workspace")
}

/// A policy that grants `root` read-write plus the system directories
/// and device nodes a shell needs to start, and nothing else.
///
/// The device nodes are not decoration: a shell redirects a background
/// job's standard input from `/dev/null`, so without them a script
/// cannot put anything in the background and a case about descendants
/// proves nothing.
///
/// Linux-only, with the cases that use it: the grants below name a
/// POSIX layout, and what they prove is Landlock's behaviour on it.
/// The macOS and Windows backends are covered by the portable cases at
/// the bottom of this file and by the unit tests in the crate.
#[cfg(target_os = "linux")]
fn shell_policy(root: &PathBuf, level: Level) -> SandboxPolicy {
    let mut policy = SandboxPolicy::new()
        .read_write(root)
        .working_directory(root)
        .env_allow(["PATH"])
        .env_set("PATH", "/usr/bin:/bin")
        .network(Network::None)
        .level(level);
    for system in ["/usr", "/bin", "/lib", "/lib64", "/etc/ld.so.cache"] {
        if PathBuf::from(system).exists() {
            policy = policy.read_only(system);
        }
    }
    for device in gossamer_sandbox::device_paths() {
        policy = policy.read_write(device);
    }
    policy
}

#[cfg(target_os = "linux")]
fn shell(sandbox: &Sandbox, script: &str) -> Result<i32, String> {
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    sandbox
        .run_with(&argv, Stdio::Capture)
        .map(|output| output.code)
        .map_err(|error| error.to_string())
}

#[cfg(target_os = "linux")]
fn skip_unless_enforcing() -> bool {
    let host = gossamer_sandbox::capabilities();
    if host.max_level < Level::Standard {
        eprintln!(
            "skipping: this host tops out at {} ({:?})",
            host.max_level, host.notes
        );
        return true;
    }
    false
}

#[cfg(target_os = "linux")]
#[test]
fn a_granted_directory_is_writable_and_the_rest_of_the_disk_is_not() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("write-confinement");
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");

    assert_eq!(
        shell(&sandbox, "echo inside > allowed.txt").expect("run"),
        0,
        "a grant that is read-write must be writable"
    );
    assert!(root.join("allowed.txt").is_file());

    let escape = std::env::temp_dir().join("gos-sandbox-escaped-write.txt");
    let _ = std::fs::remove_file(&escape);
    let code = shell(&sandbox, &format!("echo escaped > {}", escape.display())).expect("run");
    assert_ne!(code, 0, "a write outside every grant must fail");
    assert!(
        !escape.exists(),
        "the denied write must leave nothing behind"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_credential_file_outside_the_policy_cannot_be_read() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("credential-read");
    let secrets = std::env::temp_dir().join("gos-sandbox-fake-credentials");
    std::fs::write(&secrets, "token\n").expect("write fixture");
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");

    let code = shell(&sandbox, &format!("cat {}", secrets.display())).expect("run");
    assert_ne!(code, 0, "a file no rule grants must not be readable");
}

#[cfg(target_os = "linux")]
#[test]
fn a_symlink_pointing_out_of_the_sandbox_is_denied_at_its_target() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("symlink-escape");
    let secrets = std::env::temp_dir().join("gos-sandbox-symlink-target");
    std::fs::write(&secrets, "token\n").expect("write fixture");
    std::os::unix::fs::symlink(&secrets, root.join("link")).expect("symlink");
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");

    let code = shell(&sandbox, "cat link").expect("run");
    assert_ne!(
        code, 0,
        "Landlock resolves the link, so the read is refused at the target"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn the_environment_is_an_allowlist_not_an_addition() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("environment-allowlist");
    // Cargo sets this for the test process, so it is a variable the
    // caller genuinely has and the policy genuinely does not name.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok());
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo ${CARGO_PKG_NAME:-absent}".to_string(),
    ];
    let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
    assert_eq!(output.stdout_text().trim(), "absent");
}

#[cfg(target_os = "linux")]
#[test]
fn a_loader_variable_never_reaches_the_child() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("loader-variable");

    // Asking is refused with the reason, not accepted and dropped. A
    // caller who wrote the flag learns the policy will not carry it.
    for policy in [
        shell_policy(&root, Level::Standard).env_allow(["LD_PRELOAD"]),
        shell_policy(&root, Level::Standard).env_set("LD_PRELOAD", "/tmp/evil.so"),
        shell_policy(&root, Level::Standard).env_allow(["LD_LIBRARY_PATH"]),
    ] {
        let error = Sandbox::new(&policy)
            .expect_err("a loader variable must be refused rather than silently dropped");
        assert!(error.to_string().contains("cannot be passed"), "{error}");
    }

    // And the child gets none of them from the caller's environment,
    // because the environment is replaced rather than extended.
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo ${LD_PRELOAD:-absent} ${LD_LIBRARY_PATH:-absent}".to_string(),
    ];
    let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
    assert_eq!(output.stdout_text().trim(), "absent absent");
}

#[cfg(target_os = "linux")]
#[test]
fn a_denial_inside_a_grant_is_enforced_even_though_landlock_has_no_deny_rule() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("deny-inside-grant");
    std::fs::create_dir_all(root.join("work")).expect("create fixture");
    std::fs::write(root.join("secret.txt"), "token\n").expect("write fixture");

    let mut policy = shell_policy(&root, Level::Standard);
    policy = policy.deny(root.join("secret.txt"));
    let sandbox = Sandbox::new(&policy).expect("build sandbox");

    assert_eq!(
        shell(&sandbox, "echo ok > work/file").expect("run"),
        0,
        "the rest of the granted tree stays reachable"
    );
    assert_ne!(
        shell(&sandbox, "cat secret.txt").expect("run"),
        0,
        "a denial inside a grant is enforced by granting the siblings instead"
    );
}

/// A caller that bounds its own run gets the bound, at whatever level
/// the host honors.
///
/// `strict` is the case that has to be named: the namespace reaper
/// never execs, so a descriptor it holds is held for the length of the
/// payload. Holding the pipe `Command::spawn` reads would keep the
/// supervisor inside the spawn until the payload finished - no wait
/// loop, so no bound, no forwarded interrupt, and no captured output
/// until the end.
///
/// Linux-only with the helpers it uses; the namespace reaper the case
/// is about exists on no other backend.
#[cfg(target_os = "linux")]
#[test]
fn a_bound_ends_a_run_that_outlives_it() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("bounded-run");
    let policy = shell_policy(&root, gossamer_sandbox::capabilities().max_level);
    let sandbox = Sandbox::new(&policy).expect("build sandbox");

    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "while :; do :; done".to_string(),
    ];
    let started = std::time::Instant::now();
    let error = sandbox
        .run_bounded(&argv, Stdio::Capture, std::time::Duration::from_millis(500))
        .expect_err("the run must end at its bound");
    assert!(error.to_string().contains("bounded to"), "{error}");
    assert!(
        started.elapsed() < std::time::Duration::from_secs(10),
        "the bound has to end the run, not wait for the payload"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_detached_grandchild_dies_with_the_tree() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("teardown-tree");
    let started = root.join("grandchild-started");
    let marker = root.join("grandchild-alive");
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");

    // The child leaves a grandchild behind and exits, which is the
    // process the teardown has to reach on the ordinary exit path. It
    // waits for the grandchild to be live first: a grandchild the
    // scheduler had not reached yet would make the marker's absence
    // prove nothing, and that wait is what makes the case reliable on a
    // loaded machine rather than only on an idle one.
    let script = format!(
        "( echo up > {started}; sleep 30; echo alive > {marker} ) & \
         waited=0; \
         while [ ! -f {started} ] && [ $waited -lt 500 ]; do \
           waited=$((waited+1)); sleep 0.01; \
         done; \
         exit 0",
        started = started.display(),
        marker = marker.display()
    );
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
    assert_eq!(output.code, 0, "the child itself exits cleanly");

    // Without this the case passes on a host where the grandchild never
    // started, which is a shell the policy under-granted rather than a
    // teardown that worked.
    assert!(
        started.exists(),
        "the grandchild must have run for its death to mean anything"
    );
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(
        !marker.exists(),
        "the grandchild must be killed with the tree"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn a_descendant_inherits_the_policy() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("descendant-inherits");
    let escape = std::env::temp_dir().join("gos-sandbox-descendant-escape.txt");
    let _ = std::fs::remove_file(&escape);
    let sandbox = Sandbox::new(&shell_policy(&root, Level::Standard)).expect("build sandbox");

    let script = format!("/bin/sh -c 'echo escaped > {}'", escape.display());
    let code = shell(&sandbox, &script).expect("run");
    assert_ne!(code, 0, "a grandchild is bound by the same policy");
    assert!(!escape.exists());
}

#[cfg(target_os = "linux")]
#[test]
fn strict_hides_the_host_process_table_or_fails_closed() {
    let root = workspace("strict-process-table");
    let policy = shell_policy(&root, Level::Strict);
    match Sandbox::new(&policy) {
        Err(error) => {
            assert!(
                error.to_string().contains("unavailable"),
                "strict must name what blocks it: {error}"
            );
        }
        Ok(sandbox) => {
            let argv = vec![
                "/bin/sh".to_string(),
                "-c".to_string(),
                "ls /proc | grep -c '^[0-9]*$'".to_string(),
            ];
            let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
            let visible: usize = output.stdout_text().trim().parse().unwrap_or(usize::MAX);
            assert!(
                visible <= 3,
                "a private /proc shows only the sandboxed tree, saw {visible}"
            );
        }
    }
}

#[cfg(target_os = "linux")]
#[test]
fn strict_denies_every_network_protocol_or_fails_closed() {
    let root = workspace("strict-network");
    let policy = shell_policy(&root, Level::Strict);
    let Ok(sandbox) = Sandbox::new(&policy) else {
        return;
    };
    // A UDP send is the case Landlock's TCP-only layer cannot reach,
    // which is why network denial is the namespace.
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "cat < /dev/null > /dev/udp/8.8.8.8/53".to_string(),
    ];
    let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
    assert_ne!(output.code, 0, "a network namespace denies UDP too");
}

/// Environment variable that turns the probe below into the thing it
/// probes, so a bind can be attempted from inside a sandbox without
/// needing an interpreter on the host.
#[cfg(target_os = "linux")]
const LOOPBACK_PROBE: &str = "GOS_SANDBOX_LOOPBACK_PROBE";

/// Binds a loopback port when the cases below run this binary again
/// inside a sandbox, and does nothing otherwise.
#[cfg(target_os = "linux")]
#[test]
fn loopback_bind_probe() {
    if std::env::var(LOOPBACK_PROBE).is_err() {
        return;
    }
    std::net::TcpListener::bind("127.0.0.1:0").expect("bind a loopback port");
}

/// `policy` with what it takes to run this test binary again: the
/// directory it and its libraries live in, and the variable that turns
/// the probe on.
#[cfg(target_os = "linux")]
fn probe_policy(policy: SandboxPolicy) -> SandboxPolicy {
    let exe = std::env::current_exe().expect("this test binary");
    let deps = exe.parent().expect("the binary lives in a directory");
    policy
        .read_only(deps)
        .env_set(LOOPBACK_PROBE, "1")
        .env_set("RUST_TEST_THREADS", "1")
}

/// Runs the probe inside `sandbox`, answering its exit code, or
/// `None` when the run could not be made at all on this host.
#[cfg(target_os = "linux")]
fn run_probe(sandbox: &Sandbox) -> Option<i32> {
    let exe = std::env::current_exe().expect("this test binary");
    let argv = vec![
        exe.to_string_lossy().into_owned(),
        "--exact".to_string(),
        "loopback_bind_probe".to_string(),
    ];
    match sandbox.run_with(&argv, Stdio::Capture) {
        Ok(output) => Some(output.code),
        Err(error) => {
            eprintln!("skipping: {error}");
            None
        }
    }
}

/// The namespace is the network boundary at strict, and the loopback
/// it brings up is inside it. A tool that asks the machine for a local
/// address - every JVM, and most Node toolchains - has to get one.
#[cfg(target_os = "linux")]
#[test]
fn a_loopback_bind_works_at_strict_where_the_namespace_is_the_boundary() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("strict-loopback");
    let policy = probe_policy(shell_policy(&root, Level::Strict));
    let Ok(sandbox) = Sandbox::new(&policy) else {
        return;
    };
    let Some(code) = run_probe(&sandbox) else {
        return;
    };
    assert_eq!(
        code, 0,
        "a loopback bind inside the run's own network namespace reaches nothing outside it"
    );
}

/// Without a namespace there is no "inside": a listening port on the
/// host's own loopback is reachable by every other process on the
/// machine, so a policy asking for no network still denies it.
#[cfg(target_os = "linux")]
#[test]
fn a_loopback_bind_is_denied_at_standard_where_the_host_stack_is_shared() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("standard-loopback");
    let policy = probe_policy(shell_policy(&root, Level::Standard));
    let Ok(sandbox) = Sandbox::new(&policy) else {
        return;
    };
    if sandbox.network_enforcement() == gossamer_sandbox::Enforcement::None {
        // This kernel's Landlock has no network layer, so nothing here
        // is installed to deny it and the run says so.
        return;
    }
    let Some(code) = run_probe(&sandbox) else {
        return;
    };
    assert_ne!(
        code, 0,
        "a loopback bind on the host's own stack is a listening port other processes can reach"
    );
}

// ----------------------------------------------------------------
// Portable cases.
//
// These run on every backend, so macOS and Windows are covered by
// something that spawns a real child rather than only by the policy
// unit tests. What they prove is the shared contract - the level gate,
// the environment allowlist, the private temp, the captured streams,
// and the exit code - not any one kernel's enforcement.
// ----------------------------------------------------------------

/// A command that prints one known line, spelled for the host's shell.
fn echo_command() -> Vec<String> {
    if cfg!(windows) {
        vec![
            "cmd".to_string(),
            "/C".to_string(),
            "echo sandboxed".to_string(),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "echo sandboxed".to_string(),
        ]
    }
}

/// A policy that grants the workspace and whatever the host needs for a
/// command to start at all.
fn portable_policy(root: &Path, level: Level) -> SandboxPolicy {
    let mut policy = SandboxPolicy::command_default(root).level(level);
    if cfg!(windows) {
        // `command_default` grants the `PATH` directories, which is
        // where `cmd.exe` lives; the system root is what it loads from.
        for system in ["C:\\Windows", "C:\\Windows\\System32"] {
            if std::path::Path::new(system).exists() {
                policy = policy.read_only(system);
            }
        }
    }
    policy
}

#[test]
fn a_child_runs_and_its_output_and_exit_code_come_back() {
    let root = workspace("portable-run");
    let sandbox = Sandbox::new(&portable_policy(&root, Level::Basic)).expect("build sandbox");
    let output = sandbox
        .run_with(&echo_command(), Stdio::Capture)
        .expect("run the child");
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(output.stdout_text().trim(), "sandboxed");
}

#[test]
fn the_environment_allowlist_holds_on_every_backend() {
    let root = workspace("portable-environment");
    // Cargo sets this for the test process, so it is a variable the
    // caller genuinely has and the policy genuinely does not name.
    assert!(std::env::var("CARGO_PKG_NAME").is_ok());
    let compiled = portable_policy(&root, Level::Basic)
        .compile()
        .expect("compile");
    assert!(!compiled.environment().contains_key("CARGO_PKG_NAME"));
    for never in gossamer_sandbox::NEVER_PASSED_ENVIRONMENT {
        assert!(!compiled.environment().contains_key(*never));
    }
}

#[test]
fn a_private_temp_is_a_real_directory_the_policy_grants() {
    let root = workspace("portable-temp");
    let sandbox = Sandbox::new(&portable_policy(&root, Level::Basic)).expect("build sandbox");
    let temp = sandbox
        .policy()
        .temp_directory
        .clone()
        .expect("a private temp resolves to a directory");
    assert!(temp.is_dir(), "{} is not a directory", temp.display());
    assert_eq!(
        sandbox.policy().access(&temp),
        gossamer_sandbox::Access::ReadWrite
    );
    assert_eq!(
        sandbox
            .policy()
            .environment()
            .get("TMPDIR")
            .map(String::as_str),
        Some(temp.to_string_lossy().as_ref()),
        "a toolchain looks for its temp directory in the environment"
    );
}

#[test]
fn a_command_that_does_not_exist_is_reported_as_such() {
    let root = workspace("portable-not-found");
    let sandbox = Sandbox::new(&portable_policy(&root, Level::Basic)).expect("build sandbox");
    let error = sandbox
        .run_with(
            &["gossamer-sandbox-no-such-command-9f3a".to_string()],
            Stdio::Capture,
        )
        .expect_err("a missing command is an error, not an exit code");
    assert_eq!(
        error.exit_code(),
        gossamer_sandbox::EXIT_COMMAND_NOT_FOUND,
        "{error}"
    );
}

/// Serialises the cases that run at the host's maximum level.
///
/// Windows names one `AppContainer` profile per process and deletes it
/// when the run that created it ends, so two strict runs at once take
/// the profile out from under each other; a shared grant record has the
/// same property. One at a time is what the design allows.
static MAX_LEVEL_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The same contract at the level a host actually enforces at. A
/// backend that wraps `argv` in another program - macOS runs the
/// command through `sandbox-exec` - launches a wrapper that exists
/// whether or not the command does, so the not-found report has to be
/// produced before the wrapper is built or the wrapper's own failure
/// code stands in for it.
#[test]
fn a_command_that_does_not_exist_is_reported_as_such_at_the_hosts_maximum_level() {
    let _one_at_a_time = MAX_LEVEL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let host = gossamer_sandbox::capabilities();
    if host.max_level < Level::Basic {
        return;
    }
    let root = workspace("max-level-not-found");
    let sandbox =
        Sandbox::new(&portable_policy(&root, host.max_level)).expect("the reported level builds");
    let error = sandbox
        .run_with(
            &["gossamer-sandbox-no-such-command-4b7d".to_string()],
            Stdio::Capture,
        )
        .expect_err("a missing command is an error, not an exit code");
    assert_eq!(
        error.exit_code(),
        gossamer_sandbox::EXIT_COMMAND_NOT_FOUND,
        "{error}"
    );
}

#[test]
fn the_hosts_own_maximum_level_actually_runs_a_child() {
    let _one_at_a_time = MAX_LEVEL_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let host = gossamer_sandbox::capabilities();
    if host.max_level < Level::Basic {
        return;
    }
    let root = workspace("portable-max-level");
    // The level the capability report claims is the level a child must
    // actually start under: a report that promises more than the
    // backend delivers is the failure this whole design is against.
    let sandbox =
        Sandbox::new(&portable_policy(&root, host.max_level)).expect("the reported level builds");
    let output = sandbox
        .run_with(&echo_command(), Stdio::Capture)
        .expect("a child runs at the level the host reports");
    assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
    assert_eq!(output.stdout_text().trim(), "sandboxed");
}

#[test]
fn a_level_the_host_cannot_honor_is_refused_with_the_blocking_primitive_named() {
    let host = gossamer_sandbox::capabilities();
    if host.max_level == Level::Strict {
        return;
    }
    let error = Sandbox::new(&SandboxPolicy::new().level(Level::Strict))
        .expect_err("a level above the host maximum must fail closed");
    assert_eq!(error.exit_code(), gossamer_sandbox::EXIT_LEVEL_UNAVAILABLE);
    assert!(error.to_string().contains("the highest level"), "{error}");
}

// --- Windows backend ---------------------------------------------------
//
// The cases above that are not Linux-gated exercise the portable
// contract at `Level::Basic`, which on Windows is an ordinary
// `std::process::Command`. Everything the `strict` backend actually
// does - the restricted token, the app container, the host ACL grants,
// and the raw `CreateProcessAsUserW` that carries them - is reached
// only above that level, and had no behavioural coverage at all.

#[cfg(windows)]
#[allow(
    unsafe_code,
    reason = "swapping this process's own standard input is a raw Win32 \
              call; there is no safe spelling of it, and the previous \
              handle is restored before the helper returns"
)]
mod windows_backend {
    use super::{MAX_LEVEL_LOCK, portable_policy, workspace};
    use gossamer_sandbox::{Level, Sandbox, SandboxPolicy, Stdio};

    /// Whether this host can actually build the container the cases
    /// below are about. A host that tops out lower skips rather than
    /// fails: the backend is not broken there, it is absent.
    fn skip_unless_strict() -> bool {
        let host = gossamer_sandbox::capabilities();
        if host.max_level < Level::Strict {
            eprintln!(
                "skipping: this host tops out at {} ({:?})",
                host.max_level, host.notes
            );
            return true;
        }
        false
    }

    fn strict_sandbox(tag: &str) -> (std::path::PathBuf, Sandbox) {
        let root = workspace(tag);
        let policy: SandboxPolicy = portable_policy(&root, Level::Strict);
        let sandbox = Sandbox::new(&policy).expect("a strict sandbox builds on this host");
        (root, sandbox)
    }

    fn cmd(script: &str) -> Vec<String> {
        vec!["cmd".to_string(), "/C".to_string(), script.to_string()]
    }

    /// The ACL of `path` as `icacls` renders it.
    ///
    /// An external reader on purpose: what the sandbox left behind has
    /// to be visible to something other than the code that wrote it,
    /// and a record file only says what a run intended.
    fn acl_text(path: &std::path::Path) -> String {
        let output = std::process::Command::new("icacls")
            .arg(path)
            .output()
            .expect("icacls runs");
        String::from_utf8_lossy(&output.stdout).into_owned()
    }

    /// Points this process's own standard input at `content` for the
    /// duration of `body`, and puts the previous handle back.
    ///
    /// `Stdio::Capture` inherits standard input rather than replacing
    /// it, so the only way to prove a child reads what the parent was
    /// given is to give the parent something known first.
    fn with_stdin_from<T>(content: &str, body: impl FnOnce() -> T) -> T {
        use std::io::Write as _;
        use std::os::windows::io::AsRawHandle as _;
        use windows_sys::Win32::System::Console::{GetStdHandle, STD_INPUT_HANDLE, SetStdHandle};

        let path = std::env::temp_dir().join(format!(
            "gos-sandbox-stdin-{}-{:p}.txt",
            std::process::id(),
            std::ptr::from_ref(content),
        ));
        {
            let mut file = std::fs::File::create(&path).expect("create the stdin file");
            file.write_all(content.as_bytes()).expect("write it");
        }
        let file = std::fs::File::open(&path).expect("reopen the stdin file");
        // SAFETY: both calls take a handle by value; the file outlives
        // the swap, and the previous handle is restored below.
        let previous = unsafe { GetStdHandle(STD_INPUT_HANDLE) };
        unsafe { SetStdHandle(STD_INPUT_HANDLE, file.as_raw_handle().cast()) };
        let result = body();
        unsafe { SetStdHandle(STD_INPUT_HANDLE, previous) };
        drop(file);
        let _ = std::fs::remove_file(&path);
        result
    }

    /// Standard input is the caller's, not a silently substituted empty
    /// one: `Stdio`'s own contract says capturing what a command says
    /// does not silence what it is told, and the raw-spawn path has to
    /// keep that promise too.
    #[test]
    fn a_strict_child_reads_the_standard_input_it_was_given() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let (_root, sandbox) = strict_sandbox("win-strict-stdin");
        let output = with_stdin_from("hello-from-the-parent\r\n", || {
            sandbox
                .run_with(&cmd("more"), Stdio::Capture)
                .expect("the child runs")
        });
        assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
        assert!(
            output.stdout_text().contains("hello-from-the-parent"),
            "stdout was {:?}",
            output.stdout_text()
        );
    }

    /// The one directory the policy grants read-write is writable. On
    /// Windows that is two host edits rather than one: the package SID
    /// needs an ACE, and the object's mandatory label has to come down
    /// to low, because the container runs at low integrity and
    /// integrity is checked before the DACL is.
    #[test]
    fn a_strict_child_writes_into_the_granted_working_directory() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let (root, sandbox) = strict_sandbox("win-strict-write");
        let output = sandbox
            .run_with(&cmd("echo WROTE_INSIDE> inside.txt"), Stdio::Capture)
            .expect("the child runs");
        assert_eq!(
            output.code,
            0,
            "stdout: {} stderr: {}",
            output.stdout_text(),
            output.stderr_text()
        );
        let written = std::fs::read_to_string(root.join("inside.txt"))
            .expect("the file the child wrote is there afterwards");
        assert!(written.contains("WROTE_INSIDE"), "{written:?}");
    }

    /// A path outside every grant stays unwritable, and the control is
    /// what proves the probe ran at all rather than the shell failing
    /// before it reached the write.
    #[test]
    fn a_strict_child_cannot_write_outside_every_grant() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let outside = workspace("win-strict-outside");
        let (_root, sandbox) = strict_sandbox("win-strict-denied");
        let target = outside.join("denied.txt");
        let control = sandbox
            .run_with(&cmd("echo CONTROL_RAN"), Stdio::Capture)
            .expect("the control runs");
        assert_eq!(control.stdout_text().trim(), "CONTROL_RAN");

        let output = sandbox
            .run_with(
                &cmd(&format!("echo NOPE> \"{}\"", target.display())),
                Stdio::Capture,
            )
            .expect("the child runs");
        assert_ne!(
            output.code,
            0,
            "a write outside every grant must fail: {}",
            output.stdout_text()
        );
        assert!(
            !target.exists(),
            "the file was created despite the policy denying it"
        );
    }

    /// The two output streams are separate handles. An inheriting child
    /// whose stderr is a duplicate of the caller's stdout puts both on
    /// one stream, which a caller that redirects them separately never
    /// sees again.
    #[test]
    fn a_strict_childs_streams_stay_apart() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let (_root, sandbox) = strict_sandbox("win-strict-streams");
        let output = sandbox
            .run_with(&cmd("echo ON_STDOUT& echo ON_STDERR 1>&2"), Stdio::Capture)
            .expect("the child runs");
        assert!(
            output.stdout_text().contains("ON_STDOUT"),
            "stdout was {:?}",
            output.stdout_text()
        );
        assert!(
            !output.stdout_text().contains("ON_STDERR"),
            "the error stream arrived on stdout: {:?}",
            output.stdout_text()
        );
        assert!(
            output.stderr_text().contains("ON_STDERR"),
            "stderr was {:?}",
            output.stderr_text()
        );
    }

    /// Every host object the run touched is left as it was found: the
    /// ACE on the granted directory, the traverse ACEs on the
    /// directories leading to it, and the mandatory label the write
    /// needed. Read back through `icacls`, so the assertion is about
    /// the object rather than about the record the run kept.
    #[test]
    fn a_strict_run_leaves_no_ace_behind_on_the_grant_or_its_ancestors() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let root = workspace("win-strict-residue");
        let watched: Vec<std::path::PathBuf> = std::iter::once(root.clone())
            .chain(root.ancestors().skip(1).map(std::path::Path::to_path_buf))
            .collect();
        let before: Vec<String> = watched.iter().map(|path| acl_text(path)).collect();

        {
            let sandbox = Sandbox::new(&portable_policy(&root, Level::Strict))
                .expect("a strict sandbox builds on this host");
            let output = sandbox
                .run_with(&cmd("echo RAN> ran.txt"), Stdio::Capture)
                .expect("the child runs");
            assert_eq!(output.code, 0, "stderr: {}", output.stderr_text());
        }

        for (path, was) in watched.iter().zip(before) {
            assert_eq!(
                acl_text(path),
                was,
                "{} was left changed after the run",
                path.display()
            );
        }
    }

    /// A directory every app container already reaches is left alone.
    /// Windows ships an `ALL APPLICATION PACKAGES` ACE on the system
    /// root, so a grant there would rewrite a system object's ACL to
    /// say what it already says.
    #[test]
    fn a_strict_run_does_not_rewrite_a_system_directorys_acl() {
        let _one_at_a_time = MAX_LEVEL_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if skip_unless_strict() {
            return;
        }
        let system = std::path::PathBuf::from("C:\\Windows");
        if !system.is_dir() {
            return;
        }
        let before = acl_text(&system);
        {
            let (_root, sandbox) = strict_sandbox("win-strict-system");
            let output = sandbox
                .run_with(&cmd("echo SYSTEM_OK"), Stdio::Capture)
                .expect("the child runs");
            assert_eq!(output.stdout_text().trim(), "SYSTEM_OK");
        }
        assert_eq!(
            acl_text(&system),
            before,
            "the run rewrote the ACL of {}",
            system.display()
        );
    }
}
