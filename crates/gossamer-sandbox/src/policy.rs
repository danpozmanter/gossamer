//! The platform-neutral policy every backend compiles.
//!
//! One policy, three enforcement mechanisms. Anything a backend cannot
//! express is reported through [`crate::SandboxCapabilities`] rather
//! than quietly dropped, and anything the model cannot state is not in
//! the model: `allow_hosts` is deliberately absent because no backend
//! can enforce it without a proxy, and a promise in the type is how a
//! guarantee ends up half-kept.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::SandboxError;
use crate::level::Level;

/// Access a path grant confers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    /// Reachable for reading only.
    ReadOnly,
    /// Reachable for reading and writing.
    ReadWrite,
    /// Not reachable at all. Beats every grant at equal specificity.
    Deny,
}

/// One compiled path rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PathRule {
    /// Canonical path the rule applies to, and to everything beneath
    /// it.
    pub path: PathBuf,
    /// Access the rule confers.
    pub access: Access,
}

/// Where the child's temporary directory comes from.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Temp {
    /// A fresh directory the child alone can reach, removed on exit.
    #[default]
    Private,
    /// Whatever the caller's environment already names.
    Inherit,
    /// A caller-chosen directory.
    Path(PathBuf),
}

/// Whether the child may reach the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Network {
    /// No network at all, every protocol.
    #[default]
    None,
    /// Outbound connections only: the child may connect out and may not
    /// bind or listen. This is what a dependency fetch needs and what a
    /// service does not.
    Client,
    /// The network as the caller has it.
    Open,
}

/// Limits a policy asks for. A backend that cannot enforce one reports
/// `resource_limits` as unenforced rather than pretending.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Resources {
    /// Wall-clock bound on the whole process tree.
    pub timeout: Option<Duration>,
    /// Maximum number of live processes in the tree.
    pub max_processes: Option<u32>,
    /// Maximum resident memory across the tree, in bytes.
    pub max_memory: Option<u64>,
    /// Maximum accumulated CPU time across the tree.
    pub max_cpu_time: Option<Duration>,
    /// Maximum size of any single file the child creates, in bytes.
    pub max_file_size: Option<u64>,
}

/// Environment variables no policy may pass through, whatever a
/// profile or a caller asks for.
///
/// Each one makes the dynamic loader or a language runtime execute
/// caller-chosen code inside the sandbox, which would make the rest of
/// the policy decorative.
pub const NEVER_PASSED_ENVIRONMENT: &[&str] = &[
    "LD_PRELOAD",
    "LD_AUDIT",
    "LD_LIBRARY_PATH_64",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    "NODE_OPTIONS",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "JAVA_TOOL_OPTIONS",
    "_JAVA_OPTIONS",
    "RUBYOPT",
    "PERL5OPT",
    "BASH_ENV",
    "ENV",
    "GIT_SSH_COMMAND",
];

/// Paths no policy may grant, at any level above `none`.
///
/// Each is a socket whose far end is a daemon running outside the
/// sandbox, so reaching it hands the work to an unconfined process and
/// the sandbox contains nothing but the client.
#[must_use]
pub fn never_granted_paths() -> Vec<PathBuf> {
    let mut paths = vec![
        PathBuf::from("/var/run/docker.sock"),
        PathBuf::from("/run/docker.sock"),
        PathBuf::from("/run/podman/podman.sock"),
        PathBuf::from("/run/containerd/containerd.sock"),
        PathBuf::from("/run/systemd/private"),
    ];
    if let Ok(runtime_dir) = std::env::var("XDG_RUNTIME_DIR") {
        paths.push(Path::new(&runtime_dir).join("docker.sock"));
        paths.push(Path::new(&runtime_dir).join("podman/podman.sock"));
        paths.push(Path::new(&runtime_dir).join("bus"));
    }
    if let Ok(agent) = std::env::var("SSH_AUTH_SOCK") {
        paths.push(PathBuf::from(agent));
    }
    paths
}

/// A policy as written, before canonicalization.
///
/// Built with the `read_write` / `read_only` / `deny` methods, then
/// turned into a [`CompiledPolicy`] by [`SandboxPolicy::compile`],
/// which resolves every path and rejects a policy that cannot be
/// enforced as written.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxPolicy {
    /// Paths reachable for reading.
    pub read_only_paths: Vec<PathBuf>,
    /// Paths reachable for reading and writing.
    pub read_write_paths: Vec<PathBuf>,
    /// Paths not reachable at all.
    pub deny_paths: Vec<PathBuf>,
    /// Where the child's temporary directory comes from.
    pub temp: Temp,
    /// Whether the child may reach the network.
    pub network: Network,
    /// Environment variables passed through from the caller, when set.
    pub environment_allowlist: Vec<String>,
    /// Environment variables set explicitly, whatever the caller has.
    pub environment_set: BTreeMap<String, String>,
    /// Whether the child may start further processes.
    pub allow_exec: bool,
    /// Whether the child's descendants are isolated from the host
    /// process table.
    pub process_tree_isolated: bool,
    /// Whether the whole tree is killed when the sandbox exits.
    pub kill_tree_on_exit: bool,
    /// Limits the policy asks for.
    pub resources: Resources,
    /// Working directory the child starts in.
    pub working_directory: Option<PathBuf>,
    /// Level the policy asks for.
    pub level: Level,
    /// Resolved temporary directory, filled in when the sandbox
    /// materializes the policy's [`Temp`] choice.
    pub temp_directory: Option<PathBuf>,
}

impl Default for SandboxPolicy {
    fn default() -> Self {
        Self::new()
    }
}

impl SandboxPolicy {
    /// An empty policy at [`Level::Standard`]: nothing reachable, no
    /// network, a private temp directory, tree killed on exit.
    #[must_use]
    pub fn new() -> Self {
        Self {
            read_only_paths: Vec::new(),
            read_write_paths: Vec::new(),
            deny_paths: Vec::new(),
            temp: Temp::Private,
            network: Network::None,
            environment_allowlist: Vec::new(),
            environment_set: BTreeMap::new(),
            allow_exec: true,
            process_tree_isolated: false,
            kill_tree_on_exit: true,
            resources: Resources::default(),
            working_directory: None,
            level: Level::Standard,
            temp_directory: None,
        }
    }

    /// Grants read and write beneath `path`.
    #[must_use]
    pub fn read_write(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_write_paths.push(path.into());
        self
    }

    /// Grants read beneath `path`.
    #[must_use]
    pub fn read_only(mut self, path: impl Into<PathBuf>) -> Self {
        self.read_only_paths.push(path.into());
        self
    }

    /// Denies `path` and everything beneath it.
    #[must_use]
    pub fn deny(mut self, path: impl Into<PathBuf>) -> Self {
        self.deny_paths.push(path.into());
        self
    }

    /// Sets whether the child may reach the network.
    #[must_use]
    pub fn network(mut self, network: Network) -> Self {
        self.network = network;
        self
    }

    /// Passes `names` through from the caller's environment when set.
    #[must_use]
    pub fn env_allow<S: AsRef<str>>(mut self, names: impl IntoIterator<Item = S>) -> Self {
        self.environment_allowlist
            .extend(names.into_iter().map(|name| name.as_ref().to_string()));
        self
    }

    /// Sets `name` to `value` in the child's environment.
    #[must_use]
    pub fn env_set(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.environment_set.insert(name.into(), value.into());
        self
    }

    /// Chooses where the child's temporary directory comes from.
    #[must_use]
    pub fn temp(mut self, temp: Temp) -> Self {
        self.temp = temp;
        self
    }

    /// Bounds the whole process tree in wall-clock time.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.resources.timeout = Some(timeout);
        self
    }

    /// Asks for `level`.
    #[must_use]
    pub fn level(mut self, level: Level) -> Self {
        self.level = level;
        self
    }

    /// Starts the child in `path`.
    #[must_use]
    pub fn working_directory(mut self, path: impl Into<PathBuf>) -> Self {
        self.working_directory = Some(path.into());
        self
    }

    /// Resolves every path and produces the rule list a backend
    /// enforces.
    ///
    /// A path that does not resolve is a policy error rather than a
    /// dropped rule: silently ignoring a grant that names a typo is
    /// how a policy ends up looking stricter than it is, and silently
    /// ignoring a denial is worse.
    pub fn compile(&self) -> Result<CompiledPolicy, SandboxError> {
        let mut rules: Vec<PathRule> = Vec::new();
        for (paths, access) in [
            (&self.read_only_paths, Access::ReadOnly),
            (&self.read_write_paths, Access::ReadWrite),
            (&self.deny_paths, Access::Deny),
        ] {
            for path in paths {
                // A denial of a path that does not exist is honored as
                // written: the point of denying `~/.ssh` does not
                // depend on the directory being there today.
                let resolved = match canonicalize(path) {
                    Some(resolved) => resolved,
                    None if access == Access::Deny => absolute(path),
                    None => {
                        return Err(SandboxError::Policy(format!(
                            "{} does not resolve to a path on this host",
                            path.display()
                        )));
                    }
                };
                rules.push(PathRule {
                    path: resolved,
                    access,
                });
            }
        }
        for path in never_granted_paths() {
            rules.push(PathRule {
                path: absolute(&path),
                access: Access::Deny,
            });
        }
        refuse_grants_under_a_denial(&rules)?;
        sort_rules(&mut rules);

        let working_directory = match &self.working_directory {
            Some(path) => Some(canonicalize(path).ok_or_else(|| {
                SandboxError::Policy(format!(
                    "working directory {} does not resolve",
                    path.display()
                ))
            })?),
            None => None,
        };
        if let Some(directory) = &working_directory {
            if access_for(&rules, directory) == Access::Deny {
                return Err(SandboxError::Policy(format!(
                    "the working directory {} is denied by the policy",
                    directory.display()
                )));
            }
        }

        let mut allowlist: Vec<String> = self
            .environment_allowlist
            .iter()
            .filter(|name| !NEVER_PASSED_ENVIRONMENT.contains(&name.as_str()))
            .cloned()
            .collect();
        allowlist.sort_unstable();
        allowlist.dedup();
        let environment_set: BTreeMap<String, String> = self
            .environment_set
            .iter()
            .filter(|(name, _)| !NEVER_PASSED_ENVIRONMENT.contains(&name.as_str()))
            .map(|(name, value)| (name.clone(), value.clone()))
            .collect();

        Ok(CompiledPolicy {
            rules,
            temp: self.temp.clone(),
            temp_directory: self.temp_directory.clone(),
            network: self.network,
            environment_allowlist: allowlist,
            environment_set,
            allow_exec: self.allow_exec,
            process_tree_isolated: self.process_tree_isolated,
            kill_tree_on_exit: self.kill_tree_on_exit,
            resources: self.resources.clone(),
            working_directory,
            level: self.level,
        })
    }
}

/// A policy whose paths are resolved and whose rules are ordered.
///
/// This is what a backend reads. Rules are sorted most-specific first,
/// so [`CompiledPolicy::access`] is a first-match walk.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompiledPolicy {
    /// Path rules, most specific first.
    pub rules: Vec<PathRule>,
    /// Where the child's temporary directory comes from.
    pub temp: Temp,
    /// The directory the child's temporary files land in, resolved.
    ///
    /// A `Private` temp is a directory that has to exist and be
    /// granted, so the choice is resolved to a path before a backend
    /// sees it; the Linux `strict` backend mounts a private `tmpfs`
    /// here rather than over `/tmp`, which would hide a workspace that
    /// lives under it.
    pub temp_directory: Option<PathBuf>,
    /// Whether the child may reach the network.
    pub network: Network,
    /// Environment variables passed through when set.
    pub environment_allowlist: Vec<String>,
    /// Environment variables set explicitly.
    pub environment_set: BTreeMap<String, String>,
    /// Whether the child may start further processes.
    pub allow_exec: bool,
    /// Whether descendants are isolated from the host process table.
    pub process_tree_isolated: bool,
    /// Whether the whole tree is killed when the sandbox exits.
    pub kill_tree_on_exit: bool,
    /// Limits the policy asks for.
    pub resources: Resources,
    /// Working directory the child starts in.
    pub working_directory: Option<PathBuf>,
    /// Level the policy asks for.
    pub level: Level,
}

impl CompiledPolicy {
    /// The access `path` has under this policy.
    ///
    /// Deny beats read-write beats read-only at equal specificity, and
    /// the longest matching prefix wins; a path no rule matches is
    /// denied, because the model is an allow-list.
    #[must_use]
    pub fn access(&self, path: &Path) -> Access {
        access_for(&self.rules, path)
    }

    /// Every path the policy grants, with its access.
    pub fn grants(&self) -> impl Iterator<Item = &PathRule> {
        self.rules.iter().filter(|rule| rule.access != Access::Deny)
    }

    /// Every path the policy denies.
    pub fn denials(&self) -> impl Iterator<Item = &PathRule> {
        self.rules.iter().filter(|rule| rule.access == Access::Deny)
    }

    /// The environment the child gets: the allowlisted variables the
    /// caller has, plus the explicit settings, and nothing else.
    #[must_use]
    pub fn environment(&self) -> BTreeMap<String, String> {
        let mut env: BTreeMap<String, String> = BTreeMap::new();
        for name in &self.environment_allowlist {
            if let Ok(value) = std::env::var(name) {
                env.insert(name.clone(), value);
            }
        }
        for (name, value) in &self.environment_set {
            env.insert(name.clone(), value.clone());
        }
        for name in NEVER_PASSED_ENVIRONMENT {
            env.remove(*name);
        }
        env
    }

    /// The policy as a JSON document, for `--explain`, `doctor --json`,
    /// and test oracles.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Refuses a grant that names a denied path or anything beneath one.
///
/// A grant adds access; it never lifts a denial. Honoring
/// `--ro ~/.ssh` because it is more specific than the denial would make
/// every credential denial advisory, and honoring it silently would be
/// worse than refusing it: the caller would believe the policy said
/// something it did not.
fn refuse_grants_under_a_denial(rules: &[PathRule]) -> Result<(), SandboxError> {
    let denials: Vec<&PathRule> = rules
        .iter()
        .filter(|rule| rule.access == Access::Deny)
        .collect();
    for grant in rules.iter().filter(|rule| rule.access != Access::Deny) {
        if let Some(denial) = denials
            .iter()
            .find(|denial| grant.path.starts_with(&denial.path))
        {
            return Err(SandboxError::Policy(format!(
                "{} cannot be granted: the policy denies {}, and a grant never lifts a denial",
                grant.path.display(),
                denial.path.display()
            )));
        }
    }
    Ok(())
}

/// Orders rules most-specific first, with `deny` ahead of a grant at
/// the same specificity.
fn sort_rules(rules: &mut Vec<PathRule>) {
    rules.sort_by(|left, right| {
        right
            .path
            .components()
            .count()
            .cmp(&left.path.components().count())
            .then_with(|| right.access.cmp(&left.access))
            .then_with(|| left.path.cmp(&right.path))
    });
    rules.dedup_by(|left, right| left.path == right.path && left.access == right.access);
}

/// First-match access lookup over rules ordered by [`sort_rules`].
fn access_for(rules: &[PathRule], path: &Path) -> Access {
    let target = absolute(path);
    rules
        .iter()
        .find(|rule| target.starts_with(&rule.path))
        .map_or(Access::Deny, |rule| rule.access)
}

/// The canonical form of `path`, or `None` when it has no target.
fn canonicalize(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

/// `path` made absolute and lexically normalized, without touching the
/// filesystem.
fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(path)
    };
    let mut out = PathBuf::new();
    for component in joined.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

#[cfg(test)]
mod policy_tests {
    use super::*;

    fn temp_tree(tag: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!("gos-sandbox-policy-{tag}"));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("inner")).expect("create fixture tree");
        root.canonicalize().expect("canonicalize fixture")
    }

    #[test]
    fn a_grant_on_a_denied_path_is_refused_rather_than_honored() {
        let root = temp_tree("deny-beats-grant");
        let error = SandboxPolicy::new()
            .read_write(&root)
            .deny(&root)
            .compile()
            .expect_err("a grant must never lift a denial");
        assert!(matches!(error, SandboxError::Policy(_)), "{error}");
        assert!(
            error.to_string().contains("never lifts a denial"),
            "{error}"
        );
    }

    #[test]
    fn a_grant_beneath_a_denied_path_is_refused_too() {
        let root = temp_tree("grant-under-denial");
        let inner = root.join("inner");
        let error = SandboxPolicy::new()
            .deny(&root)
            .read_only(&inner)
            .compile()
            .expect_err("a more specific grant must not out-rank a denial");
        assert!(
            error.to_string().contains("never lifts a denial"),
            "{error}"
        );
    }

    #[test]
    fn the_longest_matching_prefix_wins() {
        let root = temp_tree("longest-prefix");
        let inner = root.join("inner");
        let compiled = SandboxPolicy::new()
            .read_only(&root)
            .read_write(&inner)
            .compile()
            .expect("compile");
        assert_eq!(compiled.access(&root), Access::ReadOnly);
        assert_eq!(compiled.access(&inner), Access::ReadWrite);
        assert_eq!(compiled.access(&inner.join("deep")), Access::ReadWrite);
    }

    #[test]
    fn a_path_no_rule_matches_is_denied() {
        let root = temp_tree("unmatched-denied");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .compile()
            .expect("compile");
        assert_eq!(compiled.access(Path::new("/etc")), Access::Deny);
    }

    #[test]
    fn a_grant_that_does_not_resolve_is_a_policy_error() {
        let missing = std::env::temp_dir().join("gos-sandbox-does-not-exist-9d3f");
        let error = SandboxPolicy::new()
            .read_write(&missing)
            .compile()
            .expect_err("a grant naming a missing path must not be dropped");
        assert!(matches!(error, SandboxError::Policy(_)), "{error}");
    }

    #[test]
    fn a_denial_of_a_missing_path_is_honored_as_written() {
        let missing = std::env::temp_dir().join("gos-sandbox-absent-credentials");
        let compiled = SandboxPolicy::new()
            .deny(&missing)
            .compile()
            .expect("a denial does not require the path to exist");
        assert_eq!(compiled.access(&missing), Access::Deny);
    }

    #[test]
    fn a_symlinked_grant_is_enforced_on_its_target() {
        let root = temp_tree("symlink-target");
        let target = root.join("inner");
        let link = root.join("link");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &link).expect("symlink");
        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&target, &link).expect("symlink");
        let compiled = SandboxPolicy::new()
            .read_write(&link)
            .compile()
            .expect("compile");
        assert_eq!(
            compiled.grants().map(|rule| &rule.path).collect::<Vec<_>>(),
            vec![&target],
            "a grant is enforced against the resolved path, not the link"
        );
    }

    #[test]
    fn the_daemon_sockets_are_denied_without_being_asked_for() {
        let root = temp_tree("daemon-sockets");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .compile()
            .expect("compile");
        assert_eq!(
            compiled.access(Path::new("/var/run/docker.sock")),
            Access::Deny
        );
    }

    #[test]
    fn a_loader_variable_cannot_be_allowlisted_or_set() {
        let root = temp_tree("loader-variables");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .env_allow(["PATH", "LD_PRELOAD"])
            .env_set("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib")
            .compile()
            .expect("compile");
        assert_eq!(compiled.environment_allowlist, vec!["PATH".to_string()]);
        assert!(compiled.environment_set.is_empty());
        assert!(!compiled.environment().contains_key("LD_PRELOAD"));
    }

    #[test]
    fn a_working_directory_the_policy_denies_is_a_policy_error() {
        let root = temp_tree("denied-cwd");
        let error = SandboxPolicy::new()
            .deny(&root)
            .working_directory(&root)
            .compile()
            .expect_err("a denied working directory cannot be honored");
        assert!(matches!(error, SandboxError::Policy(_)), "{error}");
    }

    #[test]
    fn a_compiled_policy_serializes_to_json() {
        let root = temp_tree("policy-json");
        let json = SandboxPolicy::new()
            .read_write(&root)
            .compile()
            .expect("compile")
            .to_json();
        assert!(json.contains("\"level\": \"standard\""), "{json}");
        assert!(json.contains("\"network\": \"none\""), "{json}");
    }
}
