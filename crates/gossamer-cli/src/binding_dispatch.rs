//! Pre-clap dispatch step that diverts `gos run` / `gos build` /
//! `gos check` to a per-project Rust-binding runner when the
//! current project's `project.toml` declares `[rust-bindings]`.
//!
//! The runner is a Cargo binary statically linking every binding;
//! it's built on demand by [`gossamer_driver::BindingRunner`].

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use gossamer_driver::binding_runner::{
    BindingRunner, BindingRunnerError, DumpedType, Profile as RunnerProfile, parse_signature_dump,
};
use gossamer_pkg::{Manifest, find_manifest};
use gossamer_resolve::{
    BindingType, ExternalItem, ExternalModule, all_external_modules, set_external_modules,
};

/// Outcome of [`dispatch_runner_if_needed`].
#[derive(Debug)]
pub enum DispatchOutcome {
    /// Runner not needed — fall through to the in-process CLI.
    InProcess,
    /// Runner was dispatched; this never returns on success
    /// because the runner replaces the current process. Returned
    /// only on failure.
    Failed(gossamer_driver::binding_runner::BindingRunnerError),
}

/// Subcommands that load user code and therefore want a runner.
const RUNNER_SUBCOMMANDS: &[&str] = &["run", "build", "check", "doc", "repl", "test"];

/// Returns whether the parsed argv warrants a runner dispatch.
///
/// Filters out re-entry (`GOSSAMER_IN_RUNNER=1`), commands that
/// don't load user code, and explicit overrides
/// (`GOSSAMER_NO_RUNNER=1`).
#[must_use]
pub fn needs_runner_dispatch(args: &[OsString]) -> bool {
    if std::env::var_os("GOSSAMER_IN_RUNNER").is_some() {
        return false;
    }
    if std::env::var_os("GOSSAMER_NO_RUNNER").is_some() {
        return false;
    }
    let Some(sub) = first_subcommand(args) else {
        return false;
    };
    RUNNER_SUBCOMMANDS.contains(&sub.as_str())
}

/// Walks `argv` past the binary name and global flags, returning
/// the first positional that names a subcommand.
fn first_subcommand(args: &[OsString]) -> Option<String> {
    for arg in args.iter().skip(1) {
        let s = arg.to_string_lossy();
        if s.starts_with('-') {
            continue;
        }
        return Some(s.into_owned());
    }
    None
}

/// Top-level pre-step: if the current project declares
/// `[rust-bindings]`, build the runner and `exec` into it. The
/// runner sets `GOSSAMER_IN_RUNNER=1` so the second pass through
/// this function is a no-op.
///
/// Returns:
/// - `DispatchOutcome::InProcess` — caller should continue with
///   the in-process CLI.
/// - `DispatchOutcome::Failed(err)` — runner build / spawn
///   failed. Caller should print the error and exit non-zero.
///
/// Successful dispatch never returns: [`BindingRunner::exec`]
/// calls `std::process::exit` after the child completes.
pub fn dispatch_runner_if_needed(args: &[OsString]) -> DispatchOutcome {
    if !needs_runner_dispatch(args) {
        return DispatchOutcome::InProcess;
    }
    let Ok(cwd) = std::env::current_dir() else {
        return DispatchOutcome::InProcess;
    };
    let Some(manifest_path) = find_manifest(&cwd) else {
        return DispatchOutcome::InProcess;
    };
    let Ok(manifest_text) = std::fs::read_to_string(&manifest_path) else {
        return DispatchOutcome::InProcess;
    };
    let Ok(manifest) = Manifest::parse(&manifest_text) else {
        return DispatchOutcome::InProcess;
    };
    if manifest.rust_bindings.is_empty() {
        return DispatchOutcome::InProcess;
    }
    let manifest_dir = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let Some(gossamer_root) = locate_gossamer_root() else {
        return DispatchOutcome::Failed(gossamer_driver::binding_runner::BindingRunnerError::Io(
            std::io::Error::other("cannot locate gossamer source root (set GOSSAMER_ROOT)"),
        ));
    };
    let profile = profile_for_args(args);
    let runner =
        match BindingRunner::from_manifest(&manifest, &manifest_dir, &gossamer_root, profile) {
            Ok(Some(r)) => r,
            Ok(None) => return DispatchOutcome::InProcess,
            Err(err) => {
                return DispatchOutcome::Failed(
                    gossamer_driver::binding_runner::BindingRunnerError::Io(err),
                );
            }
        };
    if std::env::var_os("GOSSAMER_DISPATCH_TRACE").is_some() {
        eprintln!("dispatch: runner ({})", runner.fingerprint_hex);
    }
    let bin_path = match runner.ensure_built() {
        Ok(p) => p,
        Err(err) => return DispatchOutcome::Failed(err),
    };
    let err = BindingRunner::exec(&bin_path, args);
    DispatchOutcome::Failed(err)
}

/// Picks debug or release runner based on the parsed argv. `gos
/// build --release` selects [`RunnerProfile::Release`]; everything
/// else uses [`RunnerProfile::Debug`].
fn profile_for_args(args: &[OsString]) -> RunnerProfile {
    let mut iter = args.iter().skip(1);
    let mut subcommand: Option<String> = None;
    for arg in iter.by_ref() {
        let s = arg.to_string_lossy();
        if s.starts_with('-') {
            continue;
        }
        subcommand = Some(s.into_owned());
        break;
    }
    if subcommand.as_deref() != Some("build") {
        return RunnerProfile::Debug;
    }
    for arg in iter {
        if arg == "--release" {
            return RunnerProfile::Release;
        }
    }
    RunnerProfile::Debug
}

/// Locates the gossamer source-tree root.
///
/// Order:
/// 1. `GOSSAMER_ROOT` env var (caller override).
/// 2. The compile-time `CARGO_MANIFEST_DIR` of `gossamer-cli`'s
///    parent's parent, when the binary was built from this very
///    workspace (covers `cargo run -p gossamer-cli ...`).
/// 3. Walk up from the binary's own location looking for a
///    `Cargo.toml` whose `[workspace]` includes `gossamer-cli`.
pub fn locate_gossamer_root() -> Option<PathBuf> {
    if let Some(s) = std::env::var_os("GOSSAMER_ROOT") {
        let p = PathBuf::from(s);
        if p.is_dir() {
            return Some(p);
        }
    }
    // Compile-time: gossamer-cli is at <root>/crates/gossamer-cli.
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    let p = PathBuf::from(manifest_dir);
    let candidate = p.parent().and_then(Path::parent).map(Path::to_path_buf);
    if let Some(root) = candidate
        && root.join("Cargo.toml").is_file()
    {
        return Some(root);
    }
    // Walk up from the running binary.
    if let Ok(exe) = std::env::current_exe() {
        let mut cursor: Option<&Path> = exe.parent();
        while let Some(dir) = cursor {
            if dir.join("crates").join("gossamer-cli").is_dir() {
                return Some(dir.to_path_buf());
            }
            cursor = dir.parent();
        }
    }
    None
}

/// Populates the resolver's external-modules table from the
/// per-project signature dump, when the current project declares
/// `[rust-bindings]`. Idempotent and silently skips when:
///
/// - we're already inside a runner (the runner ran
///   `gossamer_binding::install_all` which populated the table),
/// - no `project.toml` is reachable,
/// - the manifest declares no `[rust-bindings]`,
/// - the table is already non-empty.
///
/// Returns the number of external modules now visible to the
/// resolver (zero is fine — caller treats it as "no bindings").
pub fn ensure_external_signatures() -> Result<usize, BindingRunnerError> {
    if std::env::var_os("GOSSAMER_IN_RUNNER").is_some() {
        return Ok(all_external_modules().len());
    }
    if !all_external_modules().is_empty() {
        return Ok(all_external_modules().len());
    }
    let Ok(cwd) = std::env::current_dir() else {
        return Ok(0);
    };
    let Some(manifest_path) = find_manifest(&cwd) else {
        return Ok(0);
    };
    let Ok(manifest_text) = std::fs::read_to_string(&manifest_path) else {
        return Ok(0);
    };
    let Ok(manifest) = Manifest::parse(&manifest_text) else {
        return Ok(0);
    };
    if manifest.rust_bindings.is_empty() {
        return Ok(0);
    }
    let manifest_dir = manifest_path
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let Some(gossamer_root) = locate_gossamer_root() else {
        return Ok(0);
    };
    let runner = match BindingRunner::from_manifest(
        &manifest,
        &manifest_dir,
        &gossamer_root,
        RunnerProfile::Debug,
    ) {
        Ok(Some(r)) => r,
        Ok(None) => return Ok(0),
        Err(err) => return Err(BindingRunnerError::Io(err)),
    };
    let json_path = runner.ensure_signatures()?;
    let json = std::fs::read_to_string(&json_path).map_err(BindingRunnerError::Io)?;
    let dump = parse_signature_dump(&json)?;
    let modules: Vec<ExternalModule> = dump
        .modules
        .into_iter()
        .map(|m| ExternalModule {
            path: m.path,
            doc: m.doc,
            items: m
                .items
                .into_iter()
                .map(|item| ExternalItem {
                    name: item.name,
                    doc: item.doc,
                    params: item.params.iter().map(dumped_to_binding).collect(),
                    ret: dumped_to_binding(&item.ret),
                })
                .collect(),
        })
        .collect();
    let count = modules.len();
    set_external_modules(modules);
    Ok(count)
}

fn dumped_to_binding(t: &DumpedType) -> BindingType {
    match t {
        DumpedType::Unit => BindingType::Unit,
        DumpedType::Bool => BindingType::Bool,
        DumpedType::I64 => BindingType::I64,
        DumpedType::F64 => BindingType::F64,
        DumpedType::Char => BindingType::Char,
        DumpedType::String => BindingType::String,
        DumpedType::Tuple { items } => {
            BindingType::Tuple(items.iter().map(dumped_to_binding).collect())
        }
        DumpedType::Vec { of } => BindingType::Vec(Box::new(dumped_to_binding(of))),
        DumpedType::Option { of } => BindingType::Option(Box::new(dumped_to_binding(of))),
        DumpedType::Result { ok, err } => BindingType::Result(
            Box::new(dumped_to_binding(ok)),
            Box::new(dumped_to_binding(err)),
        ),
        DumpedType::Opaque { name } => BindingType::Opaque(name.clone()),
        DumpedType::Any => BindingType::Any,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;

    fn argv(parts: &[&str]) -> Vec<OsString> {
        parts.iter().map(|s| OsString::from(*s)).collect()
    }

    #[test]
    fn first_subcommand_skips_flags() {
        let a = argv(&["gos", "--quiet", "run", "x.gos"]);
        assert_eq!(first_subcommand(&a).as_deref(), Some("run"));
    }

    #[test]
    fn first_subcommand_returns_none_for_no_command() {
        let a = argv(&["gos"]);
        assert!(first_subcommand(&a).is_none());
    }

    #[test]
    fn profile_release_only_for_build_release() {
        assert!(matches!(
            profile_for_args(&argv(&["gos", "run", "x.gos"])),
            RunnerProfile::Debug
        ));
        assert!(matches!(
            profile_for_args(&argv(&["gos", "build", "x.gos"])),
            RunnerProfile::Debug
        ));
        assert!(matches!(
            profile_for_args(&argv(&["gos", "build", "x.gos", "--release"])),
            RunnerProfile::Release
        ));
        assert!(matches!(
            profile_for_args(&argv(&["gos", "build", "--release", "x.gos"])),
            RunnerProfile::Release
        ));
    }
}
