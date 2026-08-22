//! Toolchain discovery.
//!
//! A policy that grants `/usr/bin` and `/usr/lib` and calls the
//! toolchain covered is wrong on any machine using a version manager,
//! which is most of them: `node` lives under `~/.nvm`, `pnpm` under
//! `~/.local/share/pnpm`, `rustc` under `~/.rustup` - all inside a
//! `HOME` the default policy denies. So a grant is discovered by
//! resolving the command through `PATH`, following the link to the real
//! binary, and granting its install prefix.
//!
//! Nothing here runs the tool. A build system answers questions about
//! itself with the project's own configuration applied, so asking it
//! would let a repository choose what the sandbox grants, from outside
//! the sandbox and before the policy exists.

use std::path::{Path, PathBuf};

/// The real binary `name` resolves to on `PATH`, with symlinks
/// followed.
#[must_use]
pub fn resolve_on_path(name: &str) -> Option<PathBuf> {
    let candidate = Path::new(name);
    if candidate.components().count() > 1 {
        return candidate.canonicalize().ok();
    }
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for suffix in executable_suffixes() {
            let full = directory.join(format!("{name}{suffix}"));
            if full.is_file() {
                return full.canonicalize().ok();
            }
        }
    }
    None
}

fn executable_suffixes() -> &'static [&'static str] {
    if cfg!(windows) {
        &["", ".exe", ".cmd", ".bat"]
    } else {
        &[""]
    }
}

/// The install prefix of the binary at `binary`: the directory holding
/// its `bin/`, or its own directory when there is none.
///
/// `~/.nvm/versions/node/v22.17.1/bin/node` yields
/// `~/.nvm/versions/node/v22.17.1`, which is the whole installation
/// including the libraries the binary loads.
#[must_use]
pub fn install_prefix(binary: &Path) -> PathBuf {
    let directory = binary.parent().unwrap_or(binary);
    if directory.file_name().is_some_and(|name| name == "bin") {
        return directory.parent().unwrap_or(directory).to_path_buf();
    }
    directory.to_path_buf()
}

/// The install prefix of the command `name`, discovered rather than
/// assumed.
#[must_use]
pub fn prefix_of(name: &str) -> Option<PathBuf> {
    resolve_on_path(name).map(|binary| install_prefix(&binary))
}

/// The Rust toolchain directories a build reads.
///
/// Discovered from the environment rather than by asking `rustc`. A
/// tool answers about itself with the project's own configuration
/// applied - `rust-toolchain.toml` picks the toolchain, and a rustup
/// shim will fetch and run one to answer - so running it in a directory
/// whose contents the sandbox exists to contain is the wrong side of
/// the boundary. Every one of these paths is a fixed location under
/// `RUSTUP_HOME`, and granting the toolchains directory covers whatever
/// sysroot the project selects without executing anything to find out.
#[must_use]
pub fn rust_toolchain_paths() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut add = |root: PathBuf| {
        roots.push(root.join("toolchains"));
        roots.push(root.join("settings.toml"));
    };
    if let Some(rustup_home) = std::env::var_os("RUSTUP_HOME") {
        add(PathBuf::from(rustup_home));
    }
    if let Some(home) = crate::home_directory() {
        add(home.join(".rustup"));
    }
    roots.retain(|path| path.exists());
    roots
}

/// Expands a leading `~` and any `$VAR` in `text` against the
/// environment.
///
/// Profiles are written with `~/.cargo/registry` and
/// `$GOMODCACHE`, not with one machine's absolute paths, so this is
/// where a profile becomes a host path.
///
/// `None` when the text names a variable this machine does not set:
/// splicing an empty string would turn `$GOPATH/pkg/mod` into
/// `/pkg/mod`, and a grant is not a thing to guess at.
#[must_use]
pub fn expand(text: &str) -> Option<PathBuf> {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    if let Some(tail) = rest.strip_prefix("~/") {
        expanded.push_str(&crate::home_directory()?.to_string_lossy());
        expanded.push('/');
        rest = tail;
    }
    let mut chars = rest.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '$' {
            expanded.push(ch);
            continue;
        }
        let mut name = String::new();
        while let Some(next) = chars.peek() {
            if next.is_alphanumeric() || *next == '_' {
                name.push(*next);
                chars.next();
            } else {
                break;
            }
        }
        expanded.push_str(std::env::var(&name).ok()?.as_str());
    }
    Some(PathBuf::from(expanded))
}

#[cfg(test)]
mod discover_tests {
    use super::*;

    #[test]
    fn a_bin_directory_yields_the_installation_above_it() {
        assert_eq!(
            install_prefix(Path::new("/home/u/.nvm/versions/node/v22.17.1/bin/node")),
            PathBuf::from("/home/u/.nvm/versions/node/v22.17.1")
        );
    }

    #[test]
    fn a_binary_not_under_bin_yields_its_own_directory() {
        assert_eq!(
            install_prefix(Path::new("/home/u/.local/share/pnpm/pnpm")),
            PathBuf::from("/home/u/.local/share/pnpm")
        );
    }

    #[test]
    fn a_tilde_expands_to_the_home_directory() {
        let Some(home) = crate::home_directory() else {
            return;
        };
        assert_eq!(
            expand("~/.cargo/registry"),
            Some(home.join(".cargo/registry"))
        );
    }

    #[test]
    fn an_environment_variable_expands_in_place() {
        let path = std::env::var("PATH").expect("PATH is set");
        assert_eq!(expand("$PATH"), Some(PathBuf::from(path)));
    }

    /// An unset variable makes the whole entry name nothing, rather
    /// than splicing an empty string and granting a path one directory
    /// from the root.
    #[test]
    fn an_entry_naming_an_unset_variable_expands_to_nothing() {
        assert_eq!(expand("$GOS_NO_SUCH_VARIABLE_9d3f/pkg/mod"), None);
    }

    #[test]
    fn a_command_on_path_resolves_to_a_real_binary() {
        let resolved = resolve_on_path(if cfg!(windows) { "cmd" } else { "sh" });
        assert!(resolved.is_some_and(|path| path.is_file()));
    }
}
