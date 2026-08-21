//! Adversarial enforcement tests: each case tries the thing and the
//! test passes when the attempt is denied.
//!
//! Happy-path tests prove a sandbox does not break a build. Only these
//! prove it contains one.

use std::path::PathBuf;

use gossamer_sandbox::{Level, Network, Sandbox, SandboxPolicy, Stdio};

fn workspace(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("gos-sandbox-enforce-{tag}"));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).expect("create workspace");
    root.canonicalize().expect("canonicalize workspace")
}

/// A policy that grants `root` read-write plus the system directories
/// a shell needs to start, and nothing else.
fn shell_policy(root: &PathBuf, level: Level) -> SandboxPolicy {
    let mut policy = SandboxPolicy::new()
        .read_write(root)
        .working_directory(root)
        .env_allow(["PATH"])
        .env_set("PATH", "/usr/bin:/bin")
        .network(Network::Deny)
        .level(level);
    for system in ["/usr", "/bin", "/lib", "/lib64", "/etc/ld.so.cache"] {
        if PathBuf::from(system).exists() {
            policy = policy.read_only(system);
        }
    }
    policy
}

fn shell(sandbox: &Sandbox, script: &str) -> Result<i32, String> {
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script.to_string()];
    sandbox
        .run_with(&argv, Stdio::Capture)
        .map(|output| output.code)
        .map_err(|error| error.to_string())
}

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
    let policy = shell_policy(&root, Level::Standard)
        .env_allow(["LD_PRELOAD"])
        .env_set("LD_PRELOAD", "/tmp/evil.so");
    let sandbox = Sandbox::new(&policy).expect("build sandbox");
    let argv = vec![
        "/bin/sh".to_string(),
        "-c".to_string(),
        "echo ${LD_PRELOAD:-absent}".to_string(),
    ];
    let output = sandbox.run_with(&argv, Stdio::Capture).expect("run");
    assert_eq!(
        output.stdout_text().trim(),
        "absent",
        "a policy cannot grant its way past the loader denylist"
    );
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

#[cfg(target_os = "linux")]
#[test]
fn a_timeout_kills_the_whole_tree_including_a_grandchild() {
    if skip_unless_enforcing() {
        return;
    }
    let root = workspace("timeout-tree");
    let marker = root.join("grandchild-alive");
    let policy =
        shell_policy(&root, Level::Standard).timeout(std::time::Duration::from_millis(300));
    let sandbox = Sandbox::new(&policy).expect("build sandbox");

    let script = format!("( sleep 30; echo alive > {} ) & sleep 30", marker.display());
    let argv = vec!["/bin/sh".to_string(), "-c".to_string(), script];
    let error = sandbox
        .run_with(&argv, Stdio::Capture)
        .expect_err("the run must time out");
    assert!(error.to_string().contains("timeout"), "{error}");

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
