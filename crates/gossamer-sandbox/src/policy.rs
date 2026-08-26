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
    /// Not reachable at all. A grant of the same path outranks it; a
    /// denial outranks a grant only where it is the more specific rule.
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

/// Environment variables no policy may pass through, whatever a
/// profile or a caller asks for.
///
/// Each one redirects the dynamic loader or an interpreter's startup to
/// a caller-chosen path, so the code that runs is not the code the
/// command names. That is not a containment boundary - what runs is
/// still inside the sandbox - it is the guarantee that the sandbox runs
/// the program it was asked to run.
///
/// Asking to pass one is refused rather than dropped: see
/// [`SandboxPolicy::compile`].
pub const NEVER_PASSED_ENVIRONMENT: &[&str] = &[
    // The loader itself, on each platform's spelling.
    "LD_PRELOAD",
    "LD_AUDIT",
    "LD_LIBRARY_PATH",
    "LD_LIBRARY_PATH_64",
    "DYLD_INSERT_LIBRARIES",
    "DYLD_LIBRARY_PATH",
    // glibc loads a conversion or locale module from these before
    // `main`.
    "GCONV_PATH",
    "LOCPATH",
    // Interpreter startup: each names code to run or a tree to load a
    // runtime from, before the named program's first statement.
    "NODE_OPTIONS",
    "PYTHONHOME",
    "PYTHONPATH",
    "PYTHONSTARTUP",
    "JAVA_TOOL_OPTIONS",
    "JDK_JAVA_OPTIONS",
    "_JAVA_OPTIONS",
    "CLASSPATH",
    "RUBYOPT",
    "RUBYLIB",
    "PERL5OPT",
    "PERL5LIB",
    "BASH_ENV",
    "ENV",
    "GIT_SSH_COMMAND",
];

/// Prefix of an exported shell function, which `bash` evaluates as code
/// at startup for any name that carries it.
const SHELL_FUNCTION_PREFIX: &str = "BASH_FUNC_";

/// Whether `name` is a variable no policy may pass.
#[must_use]
pub fn is_never_passed(name: &str) -> bool {
    NEVER_PASSED_ENVIRONMENT.contains(&name) || name.starts_with(SHELL_FUNCTION_PREFIX)
}

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
    /// Whether the whole tree is killed when the sandbox exits, by
    /// whatever mechanism the backend has for reaching a descendant
    /// that left the process group.
    pub kill_tree_on_exit: bool,
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
            kill_tree_on_exit: true,
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
        // Against the written path rather than a resolved one: whether
        // the daemon socket happens to exist on this host today does
        // not change whether a policy may name it.
        refuse_grants_under_the_floor(&self.read_only_paths, &self.read_write_paths)?;
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

        // Refused, not filtered. Everywhere else in this compiler a rule
        // that cannot be honored is an error, and a silently dropped
        // environment setting is the same lie in a quieter place: the
        // caller believes the policy says something it does not.
        for name in self
            .environment_allowlist
            .iter()
            .chain(self.environment_set.keys())
        {
            if is_never_passed(name) {
                return Err(SandboxError::Policy(format!(
                    "{name} cannot be passed to a sandboxed command: it redirects the loader \
                     or an interpreter's startup, so the program that runs would not be the \
                     one named"
                )));
            }
        }
        let mut allowlist: Vec<String> = self.environment_allowlist.clone();
        allowlist.sort_unstable();
        allowlist.dedup();
        let environment_set: BTreeMap<String, String> = self.environment_set.clone();

        Ok(CompiledPolicy {
            rules,
            temp: self.temp.clone(),
            temp_directory: self.temp_directory.clone(),
            network: self.network,
            environment_allowlist: allowlist,
            environment_set,
            kill_tree_on_exit: self.kill_tree_on_exit,
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
    /// Whether the whole tree is killed when the sandbox exits.
    pub kill_tree_on_exit: bool,
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
        env.retain(|name, _| !is_never_passed(name));
        env
    }

    /// The policy as a JSON document, for `--explain`, `doctor --json`,
    /// and test oracles.
    #[must_use]
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }
}

/// Refuses a grant that names one of the paths no policy may reach.
///
/// An explicit allow outranks the policy's own denials - that is what
/// makes a denial a default rather than a verdict - but the floor in
/// [`never_granted_paths`] is not a denial the caller wrote. Each of
/// those is a socket whose far end runs outside the sandbox, so a grant
/// that named one would hand the work to an unconfined process. Asking
/// is refused rather than dropped: a caller that names one believes the
/// policy says something it does not.
fn refuse_grants_under_the_floor(
    read_only: &[PathBuf],
    read_write: &[PathBuf],
) -> Result<(), SandboxError> {
    let floor: Vec<PathBuf> = never_granted_paths()
        .iter()
        .map(|path| absolute(path))
        .collect();
    for granted in read_only
        .iter()
        .chain(read_write)
        .map(|path| absolute(path))
    {
        if let Some(denied) = floor.iter().find(|denied| granted.starts_with(denied)) {
            return Err(SandboxError::Policy(format!(
                "{} cannot be granted: {} is reachable only outside a sandbox, so no policy grants it",
                granted.display(),
                denied.display()
            )));
        }
    }
    Ok(())
}

/// Orders rules most-specific first, and at one path puts the widest
/// grant ahead of a narrower one and both ahead of a denial: an
/// explicit allow outranks a deny of the same path, while a denial of
/// something beneath a grant still wins by being the more specific
/// rule.
fn sort_rules(rules: &mut Vec<PathRule>) {
    rules.sort_by(|left, right| {
        right
            .path
            .components()
            .count()
            .cmp(&left.path.components().count())
            .then_with(|| precedence(left.access).cmp(&precedence(right.access)))
            .then_with(|| left.path.cmp(&right.path))
    });
    rules.dedup_by(|left, right| left.path == right.path && left.access == right.access);
}

/// Rank of an access at one path, lowest first.
const fn precedence(access: Access) -> u8 {
    match access {
        Access::ReadWrite => 0,
        Access::ReadOnly => 1,
        Access::Deny => 2,
    }
}

/// First-match access lookup over rules ordered by [`sort_rules`].
///
/// The query path is resolved the way the rules were, so a caller that
/// names a directory through a symlink - or, on Windows, without the
/// verbatim prefix canonicalization answers with - gets the verdict
/// that actually applies to it rather than a default denial.
fn access_for(rules: &[PathRule], path: &Path) -> Access {
    let target = canonicalize(path).unwrap_or_else(|| absolute(path));
    rules
        .iter()
        .find(|rule| target.starts_with(&rule.path))
        .map_or(Access::Deny, |rule| rule.access)
}

/// The canonical form of `path`, or `None` when it has no target.
///
/// On Windows the resolved form is the one a program accepts as its
/// working directory, not the verbatim one the filesystem answers with.
fn canonicalize(path: &Path) -> Option<PathBuf> {
    path.canonicalize()
        .ok()
        .map(|resolved| simplified(&resolved))
}

/// `path` with the Windows verbatim prefix removed where a plain
/// spelling names the same object.
///
/// `Path::canonicalize` answers `\\?\C:\...` on Windows. The Win32 file
/// APIs take it and little else does: `cmd.exe` refuses to start in one
/// and runs in the Windows directory instead, so a child would run
/// somewhere the policy never named. A drive path and a UNC share have a
/// plain spelling and get it; a volume GUID path has none, so it keeps
/// the prefix that is what reaches it.
#[cfg(windows)]
pub(crate) fn simplified(path: &Path) -> PathBuf {
    use std::path::Prefix;

    let mut components = path.components();
    let Some(Component::Prefix(prefix)) = components.next() else {
        return path.to_path_buf();
    };
    let plain = match prefix.kind() {
        Prefix::VerbatimDisk(letter) => format!("{}:\\", char::from(letter)),
        Prefix::VerbatimUNC(server, share) => format!(
            "\\\\{}\\{}\\",
            server.to_string_lossy(),
            share.to_string_lossy()
        ),
        _ => return path.to_path_buf(),
    };
    let mut out = PathBuf::from(plain);
    for component in components {
        if let Component::Normal(part) = component {
            out.push(part);
        }
    }
    out
}

/// A path has no verbatim prefix off Windows, so its plain spelling is
/// the one it already has.
#[cfg(not(windows))]
pub(crate) fn simplified(path: &Path) -> PathBuf {
    path.to_path_buf()
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
        // The policy records a path the way `simplified` spells it, so a
        // fixture that expects to match a compiled rule must spell it the
        // same way.
        simplified(&root.canonicalize().expect("canonicalize fixture"))
    }

    #[cfg(unix)]
    #[test]
    fn a_grant_is_found_through_a_path_that_reaches_it_by_another_name() {
        let root = temp_tree("resolved-lookup");
        let link = std::env::temp_dir().join("gos-sandbox-policy-resolved-link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(root.join("inner"), &link).expect("symlink");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .compile()
            .expect("compile");
        // The rules name resolved paths, so the lookup resolves too:
        // one directory reached by two names has one verdict.
        assert_eq!(compiled.access(&link), Access::ReadWrite);
    }

    /// The working directory a policy records is the one the child
    /// starts in, so it has to be in the spelling a child accepts.
    #[test]
    fn the_working_directory_is_recorded_in_the_spelling_a_child_accepts() {
        let root = temp_tree("plain-cwd");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .working_directory(&root)
            .compile()
            .expect("compile");
        let recorded = compiled
            .working_directory
            .as_deref()
            .expect("a working directory");
        assert!(
            !recorded.to_string_lossy().starts_with(r"\\?\"),
            "verbatim working directory: {}",
            recorded.display()
        );
        for rule in &compiled.rules {
            assert!(
                !rule.path.to_string_lossy().starts_with(r"\\?\"),
                "verbatim rule path: {}",
                rule.path.display()
            );
        }
    }

    #[cfg(windows)]
    #[test]
    fn a_verbatim_path_is_simplified_only_where_a_plain_spelling_exists() {
        assert_eq!(
            simplified(Path::new(r"\\?\C:\build\out")),
            PathBuf::from(r"C:\build\out")
        );
        assert_eq!(
            simplified(Path::new(r"\\?\UNC\server\share\dir")),
            PathBuf::from(r"\\server\share\dir")
        );
        assert_eq!(
            simplified(Path::new(r"C:\build")),
            PathBuf::from(r"C:\build")
        );
        let volume = r"\\?\Volume{9d3f}\build";
        assert_eq!(simplified(Path::new(volume)), PathBuf::from(volume));
    }

    #[test]
    fn an_explicit_grant_outranks_a_denial_of_the_same_path() {
        let root = temp_tree("allow-beats-deny");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .deny(&root)
            .compile()
            .expect("an explicit allow is honored over a deny of the same path");
        assert_eq!(compiled.access(&root), Access::ReadWrite);
    }

    #[test]
    fn a_grant_beneath_a_denial_reaches_only_what_it_names() {
        let root = temp_tree("grant-under-denial");
        let inner = root.join("inner");
        let compiled = SandboxPolicy::new()
            .deny(&root)
            .read_only(&inner)
            .compile()
            .expect("a grant beneath a denial is honored");
        assert_eq!(compiled.access(&inner), Access::ReadOnly);
        assert_eq!(
            compiled.access(&root),
            Access::Deny,
            "the denial still covers everything the grant does not name"
        );
    }

    #[test]
    fn a_denial_beneath_a_grant_still_wins_by_being_more_specific() {
        let root = temp_tree("denial-under-grant");
        let inner = root.join("inner");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .deny(&inner)
            .compile()
            .expect("compile");
        assert_eq!(compiled.access(&root), Access::ReadWrite);
        assert_eq!(compiled.access(&inner), Access::Deny);
    }

    #[test]
    fn no_policy_grants_a_path_that_is_only_reachable_outside_a_sandbox() {
        // The floor is not a denial the caller wrote, so an explicit
        // allow does not lift it.
        // The floor is checked against the written path, so the test
        // holds on a host where no container daemon is installed.
        let Some(socket) = never_granted_paths().into_iter().next() else {
            return;
        };
        let error = SandboxPolicy::new()
            .read_write(&socket)
            .compile()
            .expect_err("the floor refuses a grant that names it");
        assert!(
            error.to_string().contains("only outside a sandbox"),
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
    fn a_loader_variable_is_refused_rather_than_dropped() {
        let root = temp_tree("loader-variables");
        for policy in [
            SandboxPolicy::new()
                .read_write(&root)
                .env_allow(["PATH", "LD_PRELOAD"]),
            SandboxPolicy::new()
                .read_write(&root)
                .env_set("DYLD_INSERT_LIBRARIES", "/tmp/evil.dylib"),
            SandboxPolicy::new()
                .read_write(&root)
                .env_allow(["LD_LIBRARY_PATH"]),
            SandboxPolicy::new()
                .read_write(&root)
                .env_set("BASH_FUNC_ls%%", "() { curl evil; }"),
        ] {
            let error = policy
                .compile()
                .expect_err("a loader variable must be refused, not silently dropped");
            assert!(error.to_string().contains("cannot be passed"), "{error}");
        }
    }

    #[test]
    fn an_allowlist_a_policy_accepts_carries_no_loader_variable() {
        let root = temp_tree("loader-clean");
        let compiled = SandboxPolicy::new()
            .read_write(&root)
            .env_allow(["PATH", "HOME"])
            .compile()
            .expect("compile");
        assert_eq!(
            compiled.environment_allowlist,
            vec!["HOME".to_string(), "PATH".to_string()]
        );
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
