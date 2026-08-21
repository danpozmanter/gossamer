//! Toolchain discovery.
//!
//! A policy that grants `/usr/bin` and `/usr/lib` and calls the
//! toolchain covered is wrong on any machine using a version manager,
//! which is most of them: `node` lives under `~/.nvm`, `pnpm` under
//! `~/.local/share/pnpm`, `rustc` under `~/.rustup` - all inside a
//! `HOME` the default policy denies. So a grant is discovered by
//! resolving the command through `PATH`, following the link to the
//! real binary, and granting its install prefix; and where a tool will
//! answer for itself, by asking it.

use std::path::{Path, PathBuf};
use std::process::Command;

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

/// Runs `program args...` and returns its trimmed standard output.
///
/// Used for the queries a tool answers about itself, which is always
/// better than inferring: `rustc --print sysroot`, `go env GOMODCACHE`,
/// `npm config get cache`.
#[must_use]
pub fn query(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!text.is_empty() && text != "undefined" && text != "null").then_some(text)
}

/// Each line of a multi-line query answer, for `go env A B C`.
#[must_use]
pub fn query_lines(program: &str, args: &[&str]) -> Vec<String> {
    query(program, args).map_or_else(Vec::new, |text| {
        text.lines()
            .map(|line| line.trim().trim_matches('"').to_string())
            .filter(|line| !line.is_empty())
            .collect()
    })
}

/// Expands a leading `~` and any `$VAR` in `text` against the
/// environment.
///
/// Profiles are written with `~/.cargo/registry`, not with one
/// machine's absolute paths, so this is where a profile becomes a host
/// path.
#[must_use]
pub fn expand(text: &str) -> PathBuf {
    let mut expanded = String::with_capacity(text.len());
    let mut rest = text;
    if let Some(tail) = rest.strip_prefix("~/") {
        if let Some(home) = crate::home_directory() {
            expanded.push_str(&home.to_string_lossy());
            expanded.push('/');
        }
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
        if let Ok(value) = std::env::var(&name) {
            expanded.push_str(&value);
        }
    }
    PathBuf::from(expanded)
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
        assert_eq!(expand("~/.cargo/registry"), home.join(".cargo/registry"));
    }

    #[test]
    fn an_environment_variable_expands_in_place() {
        // SAFETY-free: reads only, using a variable the process
        // certainly has.
        let path = std::env::var("PATH").expect("PATH is set");
        assert_eq!(expand("$PATH"), PathBuf::from(path));
    }

    #[test]
    fn a_command_on_path_resolves_to_a_real_binary() {
        let resolved = resolve_on_path(if cfg!(windows) { "cmd" } else { "sh" });
        assert!(resolved.is_some_and(|path| path.is_file()));
    }
}
