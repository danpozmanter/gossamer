//! The sandbox `gos build --sandbox` puts Cargo in.
//!
//! Attached at [`run_under_sandbox`], which every Cargo invocation the
//! driver makes goes through, rather than at the `build` subcommand:
//! `[rust-bindings]` compilation is reached from `build`, `check`,
//! `doc`, `repl`, `run`, and `test`, so a sandbox attached to one of
//! them leaves five doors open - including `check`, the one an editor
//! and CI run unattended.
//!
//! The dangerous act is not downloading a dependency; it is executing
//! what was downloaded. So the run is split: a fetch phase with the
//! network and writes confined to the cache roots, then a build phase
//! that runs `--offline` with the network denied and `build.rs`, proc
//! macros, and the linker inside the policy.

use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};

use gossamer_sandbox::{Level, Network, Sandbox, SandboxPolicy, Stdio};

/// What `--sandbox` and its companions asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildSandbox {
    /// Level the build runs at. `Level::None` means no sandbox, which
    /// is the default for this release.
    pub level: Level,
    /// Whether the build phase may reach the network. Off unless a
    /// build genuinely needs it.
    pub network_in_build: bool,
    /// Extra read-write grants from `--sandbox-rw`.
    pub read_write: Vec<PathBuf>,
    /// Extra read-only grants from `--sandbox-ro`.
    pub read_only: Vec<PathBuf>,
    /// Print the compiled policy and the mechanisms instead of
    /// building.
    pub explain: bool,
}

impl Default for BuildSandbox {
    fn default() -> Self {
        Self {
            // `--sandbox=none` stays the default for this release, so
            // a policy gap surfaces as a failed opt-in rather than a
            // broken build for everyone. Flipping it is its own
            // release note.
            level: Level::None,
            network_in_build: false,
            read_write: Vec::new(),
            read_only: Vec::new(),
            explain: false,
        }
    }
}

impl BuildSandbox {
    /// Whether any enforcement is asked for.
    #[must_use]
    pub fn is_active(&self) -> bool {
        self.level != Level::None
    }
}

/// The request in force for this process.
fn slot() -> &'static Mutex<BuildSandbox> {
    static SLOT: OnceLock<Mutex<BuildSandbox>> = OnceLock::new();
    SLOT.get_or_init(|| Mutex::new(BuildSandbox::default()))
}

/// Records what the command line asked for. Called once by the CLI
/// before any command runs.
pub fn set(request: BuildSandbox) {
    *slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = request;
}

/// The request in force.
#[must_use]
pub fn active() -> BuildSandbox {
    slot()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Everything a build touches on this host, discovered rather than
/// assumed.
#[derive(Debug, Clone, Default)]
pub struct BuildRoots {
    /// The project root, or the working directory for a bare-file
    /// build. Read-write, because a build creates `target/` and
    /// `.gos-cache/` there.
    pub project: Vec<PathBuf>,
    /// Package, git, and Cargo caches. Read-write.
    pub caches: Vec<PathBuf>,
    /// Toolchain installations. Read-only.
    pub toolchain: Vec<PathBuf>,
}

impl BuildRoots {
    /// The roots for a build rooted at `project`.
    #[must_use]
    pub fn discover(project: &Path) -> Self {
        let mut caches = Vec::new();
        if let Some(dir) = std::env::var_os("GOS_CACHE_DIR") {
            caches.push(PathBuf::from(dir));
        } else if let Some(home) = gossamer_sandbox::home_directory() {
            caches.push(home.join(".gossamer").join("cache"));
        }
        if let Some(home) = gossamer_sandbox::home_directory() {
            caches.push(home.join(".gossamer").join("build"));
            caches.push(home.join(".cargo").join("registry"));
            caches.push(home.join(".cargo").join("git"));
        }
        if let Some(cargo_home) = std::env::var_os("CARGO_HOME") {
            let root = PathBuf::from(cargo_home);
            caches.push(root.join("registry"));
            caches.push(root.join("git"));
        }

        let mut toolchain = Vec::new();
        if let Some(sysroot) = gossamer_sandbox::discover::query("rustc", &["--print", "sysroot"]) {
            toolchain.push(PathBuf::from(sysroot));
        }
        for command in [
            "cargo", "rustc", "cc", "gcc", "clang", "ld", "lld", "strip", "ar",
        ] {
            if let Some(prefix) = gossamer_sandbox::discover::prefix_of(command) {
                toolchain.push(prefix);
            }
        }
        if let Some(home) = gossamer_sandbox::home_directory() {
            toolchain.push(home.join(".rustup"));
            toolchain.push(home.join(".cargo").join("bin"));
        }
        // A discovered prefix can widen past what it was meant to
        // cover: `cargo` resolves to `~/.cargo/bin/cargo`, whose
        // install prefix is `~/.cargo`, which holds the registry
        // credentials. Granting the narrower thing is what keeps the
        // credential denial from having to fight a grant.
        let credentials = gossamer_sandbox::credential_paths();
        toolchain.retain(|prefix| {
            !credentials
                .iter()
                .any(|credential| credential.starts_with(prefix))
        });

        Self {
            project: vec![project.to_path_buf()],
            caches,
            toolchain,
        }
    }
}

/// Which half of the split a command belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    /// Downloads dependencies. Network allowed; no dependency code
    /// runs, which is what makes the split sound.
    Fetch,
    /// Compiles them. Network denied; `build.rs`, proc macros, and the
    /// linker all run inside the policy.
    Build,
}

/// What a sandboxed command left behind.
pub struct SandboxedRun {
    /// The command's exit code.
    pub code: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

/// Compiles the policy for `phase` and runs `argv` under it.
///
/// `environment` names variables set explicitly for the child on top
/// of the build allowlist.
pub fn run_under_sandbox(
    request: &BuildSandbox,
    roots: &BuildRoots,
    phase: Phase,
    argv: &[String],
    environment: &[(String, String)],
) -> Result<SandboxedRun, String> {
    let sandbox = compile(request, roots, phase, environment)?;
    let output = sandbox
        .run_with(argv, Stdio::Capture)
        .map_err(|error| error.to_string())?;
    Ok(SandboxedRun {
        code: output.code,
        stdout: output.stdout_text(),
        stderr: output.stderr_text(),
    })
}

/// The compiled sandbox for `phase`.
pub fn compile(
    request: &BuildSandbox,
    roots: &BuildRoots,
    phase: Phase,
    environment: &[(String, String)],
) -> Result<Sandbox, String> {
    let project = roots
        .project
        .first()
        .cloned()
        .unwrap_or_else(|| PathBuf::from("."));
    let mut policy = SandboxPolicy::build_default(&project, &roots.caches, &roots.toolchain)
        .level(request.level);
    for extra in &roots.project[1..] {
        policy = policy.read_write(extra.clone());
    }
    for path in &request.read_write {
        policy = policy.read_write(path.clone());
    }
    for path in &request.read_only {
        policy = policy.read_only(path.clone());
    }
    for (name, value) in environment {
        policy = policy.env_set(name.clone(), value.clone());
    }
    policy = match phase {
        Phase::Fetch => policy.for_fetch_phase(),
        Phase::Build if request.network_in_build => policy.for_fetch_phase(),
        Phase::Build => policy.network(Network::Deny),
    };
    Sandbox::new(&policy).map_err(|error| error.to_string())
}

/// The `--sandbox-explain` report for both phases.
#[must_use]
pub fn explain(request: &BuildSandbox, roots: &BuildRoots) -> String {
    let mut out = String::new();
    for phase in [Phase::Fetch, Phase::Build] {
        let label = match phase {
            Phase::Fetch => "fetch",
            Phase::Build => "build",
        };
        out.push_str(&format!("phase {label}:\n"));
        match compile(request, roots, phase, &[]) {
            Ok(sandbox) => {
                let policy = sandbox.policy();
                out.push_str(&format!("  level:       {}\n", policy.level));
                out.push_str(&format!(
                    "  network:     {}\n",
                    match policy.network {
                        Network::Deny => "denied",
                        Network::Allow => "allowed",
                    }
                ));
                for line in sandbox.mechanisms() {
                    out.push_str(&format!("  mechanism:   {line}\n"));
                }
                for rule in policy.grants() {
                    out.push_str(&format!(
                        "  {:<11} {}\n",
                        match rule.access {
                            gossamer_sandbox::Access::ReadWrite => "read-write:",
                            _ => "read-only:",
                        },
                        rule.path.display()
                    ));
                }
                for rule in policy.denials() {
                    out.push_str(&format!("  denied:      {}\n", rule.path.display()));
                }
            }
            Err(reason) => out.push_str(&format!("  unavailable: {reason}\n")),
        }
    }
    out.push_str(
        "\nThis flag contains the Cargo invocation that compiles `[rust-bindings]`, every\n\
         `build.rs` and proc macro it runs, the linker, and every descendant. It does not\n\
         sandbox your own program under `gos run`; build a policy with\n\
         `std::sandbox` for that.\n",
    );
    out
}

#[cfg(test)]
mod build_sandbox_tests {
    use super::*;

    #[test]
    fn the_default_is_no_sandbox_so_a_policy_gap_is_an_opt_in_failure() {
        assert_eq!(BuildSandbox::default().level, Level::None);
        assert!(!BuildSandbox::default().is_active());
    }

    #[test]
    fn the_fetch_phase_allows_the_network_and_the_build_phase_denies_it() {
        let project = std::env::temp_dir().canonicalize().expect("canonicalize");
        let roots = BuildRoots::discover(&project);
        let request = BuildSandbox {
            level: Level::Basic,
            ..BuildSandbox::default()
        };
        let fetch = compile(&request, &roots, Phase::Fetch, &[]).expect("compile fetch");
        let build = compile(&request, &roots, Phase::Build, &[]).expect("compile build");
        assert_eq!(fetch.policy().network, Network::Allow);
        assert_eq!(build.policy().network, Network::Deny);
    }

    #[test]
    fn the_build_policy_keeps_the_project_root_writable() {
        let project = std::env::temp_dir().canonicalize().expect("canonicalize");
        let roots = BuildRoots::discover(&project);
        let request = BuildSandbox {
            level: Level::Basic,
            ..BuildSandbox::default()
        };
        let sandbox = compile(&request, &roots, Phase::Build, &[]).expect("compile");
        assert_eq!(
            sandbox.policy().access(&project),
            gossamer_sandbox::Access::ReadWrite,
            "a build creates target/ and .gos-cache/ in the project root"
        );
    }

    #[test]
    fn the_environment_keeps_what_reproducible_and_relocated_builds_need() {
        let project = std::env::temp_dir().canonicalize().expect("canonicalize");
        let roots = BuildRoots::discover(&project);
        let request = BuildSandbox {
            level: Level::Basic,
            ..BuildSandbox::default()
        };
        let sandbox = compile(&request, &roots, Phase::Build, &[]).expect("compile");
        let allowed = &sandbox.policy().environment_allowlist;
        assert!(allowed.contains(&"SOURCE_DATE_EPOCH".to_string()));
        assert!(allowed.contains(&"GOS_CACHE_DIR".to_string()));
        assert!(!allowed.contains(&"CARGO_REGISTRY_TOKEN".to_string()));
    }

    #[test]
    fn no_discovered_toolchain_grant_covers_a_credential() {
        let project = std::env::temp_dir().canonicalize().expect("canonicalize");
        let roots = BuildRoots::discover(&project);
        for credential in gossamer_sandbox::credential_paths() {
            for prefix in &roots.toolchain {
                assert!(
                    !credential.starts_with(prefix),
                    "the toolchain grant {} covers the credential {}",
                    prefix.display(),
                    credential.display()
                );
            }
        }
    }

    #[test]
    fn explain_names_both_phases_and_states_what_the_flag_does_not_cover() {
        let project = std::env::temp_dir().canonicalize().expect("canonicalize");
        let roots = BuildRoots::discover(&project);
        let request = BuildSandbox {
            level: Level::Basic,
            ..BuildSandbox::default()
        };
        let text = explain(&request, &roots);
        assert!(text.contains("phase fetch:"), "{text}");
        assert!(text.contains("phase build:"), "{text}");
        assert!(text.contains("does not"), "{text}");
    }
}
