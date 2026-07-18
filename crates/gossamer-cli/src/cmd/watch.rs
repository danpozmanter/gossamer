//! Restart-based development supervisor for long-running Gossamer services.
//!
//! `gos watch` owns a child `gos run` process. On each relevant source or
//! manifest change it validates the next revision, gracefully stops the
//! current child, then starts a replacement. A failed revision leaves the last
//! known-good service running.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gossamer_std::exec;
use gossamer_std::fs::{Event, Watcher};
use gossamer_std::signal::{self, Notifier, sigs};

use crate::paths::{
    project_edition_for_entry, read_entry_source, resolve_entry_arg, stderr_supports_colour,
};

const MAX_BATCH: Duration = Duration::from_secs(2);

/// Parsed `gos watch` settings.
pub(crate) struct Options {
    pub(crate) file: Option<PathBuf>,
    pub(crate) debounce: Duration,
    pub(crate) grace: Duration,
    pub(crate) check: bool,
    pub(crate) clear: bool,
    pub(crate) locked: bool,
    pub(crate) args: Vec<String>,
}

/// Runs the single-owner development supervisor until interrupted.
pub(crate) fn run(options: Options) -> Result<()> {
    let entry = resolve_entry_arg(options.file.clone())?;
    let entry = entry
        .canonicalize()
        .with_context(|| format!("resolve {}", entry.display()))?;
    let status = Status::new();
    status.info(&format!("watching {}", entry.display()));
    if options.check {
        validate(&entry, options.locked, &status)?;
    }
    let signals = SupervisorSignals::new();
    let mut generation = 1_u64;
    let mut child = start_child(&entry, &options, generation, &status)?;

    loop {
        let (watcher, events) = watch_inputs(&entry)?;
        let decision = wait_for_batch(&events, options.debounce, &signals)?;
        drop(watcher);
        if decision == WatchDecision::Shutdown {
            stop_child(&mut child, options.grace, &status)?;
            return Ok(());
        }
        if let Some(status_code) = child.try_wait()? {
            status.warn(&format!(
                "child exited with {status_code}; restarting current generation"
            ));
            child = start_child(&entry, &options, generation, &status)?;
            continue;
        }
        if options.check {
            match validate(&entry, options.locked, &status) {
                Ok(()) => {}
                Err(error) => {
                    status.error(&format!(
                        "check failed; keeping generation {generation} running"
                    ));
                    eprintln!("{error:#}");
                    continue;
                }
            }
        }
        stop_child(&mut child, options.grace, &status)?;
        generation += 1;
        child = start_child(&entry, &options, generation, &status)?;
    }
}

struct Status {
    colour: bool,
}

impl Status {
    fn new() -> Self {
        Self {
            colour: stderr_supports_colour(),
        }
    }

    fn info(&self, message: &str) {
        self.line("info", "\x1b[36m", message);
    }

    fn ok(&self, message: &str) {
        self.line("ok", "\x1b[32m", message);
    }

    fn warn(&self, message: &str) {
        self.line("warn", "\x1b[33m", message);
    }

    fn error(&self, message: &str) {
        self.line("error", "\x1b[31m", message);
    }

    fn line(&self, label: &str, colour: &str, message: &str) {
        if self.colour {
            eprintln!("{colour}gos watch:{label}\x1b[0m {message}");
        } else {
            eprintln!("gos watch:{label} {message}");
        }
    }
}

struct SupervisorSignals {
    int: Notifier,
    term: Notifier,
}

impl SupervisorSignals {
    fn new() -> Self {
        Self {
            int: signal::on(sigs::SIGINT),
            term: signal::on(sigs::SIGTERM),
        }
    }

    fn received(&self) -> bool {
        self.int.try_wait() || self.term.try_wait()
    }
}

fn validate(entry: &Path, locked: bool, status: &Status) -> Result<()> {
    let stage = Instant::now();
    let _cwd = if let Some(root) = crate::paths::project_root_for_entry(entry) {
        Some(CurrentDirGuard::push(&root)?)
    } else {
        None
    };
    if locked {
        crate::cmd::pkg::enforce_lockfile_if_requested(true)?;
    }
    let source_path = entry.to_path_buf();
    let user_source = read_entry_source(&source_path)?;
    let source = gossamer_parse::autoderive::augment_source(&user_source);
    let source = crate::comptime_fold::fold_comptime(source, &entry.to_string_lossy())?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(entry.to_string_lossy().into_owned(), source.clone());
    let outcome = gossamer_driver::check_frontend_with_edition(
        &source,
        file_id,
        project_edition_for_entry(entry),
    );
    let render_opts = gossamer_diagnostics::RenderOptions {
        colour: stderr_supports_colour(),
    };
    for diag in &outcome.diagnostics {
        eprintln!("{}", gossamer_diagnostics::render(diag, &map, render_opts));
    }
    if !outcome.diagnostics.is_empty() {
        return Err(anyhow!(
            "check failed with {} diagnostic(s)",
            outcome.diagnostics.len()
        ));
    }
    status.ok(&format!(
        "validated {} item(s) in {} ms",
        outcome.checked.sf.items.len(),
        stage.elapsed().as_millis()
    ));
    Ok(())
}

fn start_child(entry: &Path, options: &Options, generation: u64, status: &Status) -> Result<Child> {
    if options.clear {
        print!("\x1b[2J\x1b[H");
    }
    let exe = std::env::current_exe().context("locate gos executable")?;
    let mut command = Command::new(exe);
    if let Some(root) = crate::paths::project_root_for_entry(entry) {
        command.current_dir(root);
    }
    command.arg("run").arg(entry);
    if options.locked {
        command.arg("--locked");
    }
    command.arg("--").args(&options.args);
    configure_child_group(&mut command);
    let child = command
        .spawn()
        .with_context(|| format!("start generation {generation}"))?;
    status.ok(&format!(
        "running pid={} generation={generation}",
        child.id()
    ));
    Ok(child)
}

struct CurrentDirGuard {
    previous: PathBuf,
}

impl CurrentDirGuard {
    fn push(path: &Path) -> Result<Self> {
        let previous = std::env::current_dir().context("read current directory")?;
        std::env::set_current_dir(path)
            .with_context(|| format!("enter project root {}", path.display()))?;
        Ok(Self { previous })
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        let _ = std::env::set_current_dir(&self.previous);
    }
}

#[cfg(unix)]
fn configure_child_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(not(unix))]
fn configure_child_group(_command: &mut Command) {}

fn stop_child(child: &mut Child, grace: Duration, status: &Status) -> Result<()> {
    status.info(&format!("stopping pid={} gracefully", child.id()));
    request_shutdown(child)?;
    let deadline = Instant::now() + grace;
    loop {
        if child.try_wait()?.is_some() {
            status.ok("previous generation stopped; port is ready");
            return Ok(());
        }
        if Instant::now() >= deadline {
            child.kill().context("force-kill watched child")?;
            let _ = child.wait();
            status.warn("grace expired; killed previous generation");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}

#[cfg(unix)]
fn request_shutdown(child: &Child) -> Result<()> {
    if exec::send_group_term(i64::from(child.id())) {
        Ok(())
    } else {
        Err(anyhow!("could not signal child process group"))
    }
}

#[cfg(not(unix))]
fn request_shutdown(child: &Child) -> Result<()> {
    let _ = child;
    Ok(())
}

fn watch_inputs(entry: &Path) -> Result<(Watcher, Receiver<Event>)> {
    let watcher = Watcher::new().context("create filesystem watcher")?;
    for root in watch_roots(entry) {
        watcher
            .add(&root.to_string_lossy())
            .with_context(|| format!("watch {}", root.display()))?;
    }
    let events = watcher
        .events()
        .ok_or_else(|| anyhow!("watcher event receiver already taken"))?;
    Ok((watcher, events))
}

fn watch_roots(entry: &Path) -> Vec<PathBuf> {
    let mut roots = BTreeSet::new();
    roots.insert(entry.to_path_buf());
    if let Some(root) = crate::paths::project_root_for_entry(entry) {
        roots.insert(root);
    }
    roots.extend(crate::paths::local_path_dependency_roots(entry));
    roots.into_iter().collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WatchDecision {
    Changed,
    Shutdown,
}

fn wait_for_batch(
    events: &Receiver<Event>,
    debounce: Duration,
    signals: &SupervisorSignals,
) -> Result<WatchDecision> {
    loop {
        if signals.received() {
            return Ok(WatchDecision::Shutdown);
        }
        let first = match events.recv_timeout(Duration::from_millis(50)) {
            Ok(event) => event,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => {
                return Err(anyhow!("filesystem watcher disconnected"));
            }
        };
        if !is_relevant(Path::new(&first.path)) {
            continue;
        }
        let started = Instant::now();
        let mut latest = started;
        loop {
            let elapsed = started.elapsed();
            let quiet_left = debounce.saturating_sub(latest.elapsed());
            let limit = quiet_left.min(MAX_BATCH.saturating_sub(elapsed));
            if limit.is_zero() {
                return Ok(WatchDecision::Changed);
            }
            if signals.received() {
                return Ok(WatchDecision::Shutdown);
            }
            match events.recv_timeout(limit) {
                Ok(event) if is_relevant(Path::new(&event.path)) => latest = Instant::now(),
                Ok(_) => {}
                Err(RecvTimeoutError::Timeout) => return Ok(WatchDecision::Changed),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(anyhow!("filesystem watcher disconnected"));
                }
            }
        }
    }
}

/// Whether an event path can affect a project revision. Kept pure so the
/// platform watcher adapter stays a small, testable boundary.
fn is_relevant(path: &Path) -> bool {
    if path
        .components()
        .any(|component| matches!(component.as_os_str().to_str(), Some(".git" | "target")))
    {
        return false;
    }
    let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
        return false;
    };
    if name.starts_with('.')
        || name.ends_with('~')
        || path
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| ext.eq_ignore_ascii_case("swp") || ext.eq_ignore_ascii_case("tmp"))
    {
        return false;
    }
    matches!(name, "project.toml" | "project.lock")
        || path.extension().and_then(|ext| ext.to_str()) == Some("gos")
}

#[cfg(test)]
mod tests {
    use super::{SupervisorSignals, WatchDecision, is_relevant, wait_for_batch};
    use gossamer_std::signal;
    use std::path::Path;
    use std::sync::mpsc;

    #[test]
    fn filters_build_outputs_and_editor_files() {
        assert!(is_relevant(Path::new("src/main.gos")));
        assert!(is_relevant(Path::new("project.toml")));
        assert!(!is_relevant(Path::new("target/debug/main.gos")));
        assert!(!is_relevant(Path::new("src/main.gos~")));
        assert!(!is_relevant(Path::new("src/.main.gos")));
    }

    #[test]
    fn watcher_loop_exits_on_shutdown_signal() {
        let (_tx, rx) = mpsc::channel();
        let signals = SupervisorSignals::new();

        signal::deliver(signal::sigs::SIGTERM);

        let decision = wait_for_batch(&rx, std::time::Duration::from_millis(150), &signals)
            .expect("shutdown decision");
        assert_eq!(decision, WatchDecision::Shutdown);
    }
}
