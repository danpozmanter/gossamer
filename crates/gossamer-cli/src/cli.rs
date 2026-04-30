//! `gos` argument parsing + dispatch table.
//!
//! Owning the `clap`-derived [`Cli`] / [`Command`] types here keeps
//! `main.rs` to just a runtime entry point. Every variant matches in
//! [`run`] to a single line that delegates to a `crate::cmd::*`
//! module.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};

use crate::cmd::{self, RunMode, TestOpts};
use crate::style;
use crate::{doc, repl};

/// Top-level parsed command line for the `gos` binary.
#[derive(Debug, Parser)]
#[command(name = "gos", version, about = "The Gossamer toolchain")]
pub(crate) struct Cli {
    /// Subcommand to dispatch; omit for a bare no-op that still
    /// prints `--version`.
    #[command(subcommand)]
    command: Option<Command>,
}

/// Subcommands accepted by the `gos` binary.
#[derive(Debug, Subcommand)]
enum Command {
    /// Parse a source file and dump its AST.
    Parse {
        /// Path to a `.gos` source file.
        file: PathBuf,
    },
    /// Run the full frontend (parse + resolve + typecheck + exhaustiveness).
    ///
    /// With no path: when a `project.toml` is reachable above the
    /// current directory, every `.gos` under the project's `src/`
    /// is checked.
    Check {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        file: Option<PathBuf>,
        /// Print per-stage wall-clock timings on success.
        #[arg(long)]
        timings: bool,
    },
    /// Execute a program by invoking its `main` function.
    ///
    /// The default path is the register-based bytecode VM. When
    /// the VM's compiler hits an HIR construct it doesn't yet
    /// support the runner silently falls back to the tree-walker.
    /// Use `--tree-walker` to force the tree-walker for
    /// development / debugging.
    ///
    /// With no path: defaults to `<project-root>/src/main.gos`
    /// when a `project.toml` is reachable.
    Run {
        /// Path to a `.gos` source file. Optional: defaults to the
        /// project's `src/main.gos`.
        file: Option<PathBuf>,
        /// Use the tree-walker (the original recursive
        /// interpreter) instead of the VM. Slower but covers
        /// every language construct today; mostly useful when
        /// debugging the VM or chasing parity bugs.
        #[arg(long)]
        tree_walker: bool,
        /// Disable the cranelift JIT (deferred whole-program
        /// compile triggered by per-chunk hot-counter tier-up).
        /// The JIT is on by default — pass `--no-jit` to fall back
        /// to pure bytecode dispatch. Equivalent to setting
        /// `GOS_JIT=0` in the environment.
        #[arg(long)]
        no_jit: bool,
        /// Arguments forwarded to the interpreted program (after `--`).
        #[arg(last = true)]
        args: Vec<String>,
    },
    /// Compile the program to a native executable.
    ///
    /// Output path: `project.output` from the manifest if set,
    /// else the source stem beside the input file. With no path,
    /// builds `<project-root>/src/main.gos`.
    Build {
        /// Path to a `.gos` source file. Optional: defaults to the
        /// project's `src/main.gos`.
        file: Option<PathBuf>,
        /// Cross-compilation target triple (e.g. `aarch64-apple-darwin`).
        #[arg(long)]
        target: Option<String>,
        /// Route codegen through the LLVM backend with `-O3` for
        /// production builds. Falls back to Cranelift when LLVM lowerer
        /// does not cover a construct yet.
        #[arg(long)]
        release: bool,
        /// Embed DWARF debug information so `gdb` / `lldb` can step
        /// through Gossamer source. Sets the `GOS_BUILD_DEBUG` env
        /// var the LLVM lowerer reads. Also suppresses the default
        /// `--strip-all` applied to release binaries.
        #[arg(short = 'g', long = "debug-info")]
        debug_info: bool,
        /// Force the legacy dynamic-glibc link path on Linux, even
        /// when the rustup `x86_64-unknown-linux-musl` target is
        /// available. Default release builds produce a fully-static
        /// musl binary on Linux when the target is installed.
        #[arg(long)]
        dynamic: bool,
        /// Produce a bit-identical artifact across two clean builds
        /// of the same source on the same target. Pins the build
        /// timestamp via `SOURCE_DATE_EPOCH`, strips embedded
        /// absolute paths, and sorts symbol tables. Two reproducible
        /// builds of the same input compared with `cmp` should
        /// match byte-for-byte.
        #[arg(long)]
        reproducible: bool,
    },
    /// Create a `project.toml` in the current directory.
    Init {
        /// Project identifier (e.g. `example.com/myproj`).
        id: String,
    },
    /// Scaffold a new project directory with `project.toml` and a
    /// starter source tree. Defaults to a binary template; pass
    /// `--template` to scaffold a library or workspace instead.
    New {
        /// Project identifier (e.g. `example.com/myproj`).
        id: String,
        /// Optional output directory. Defaults to the project tail
        /// (last `/`-separated component).
        #[arg(long)]
        path: Option<PathBuf>,
        /// Project template to scaffold. `bin` writes an executable
        /// `src/main.gos`; `lib` writes a reusable `src/lib.gos`
        /// with a smoke test; `service` writes an HTTP handler that
        /// binds `0.0.0.0:8080`; `workspace` writes a `project.toml`
        /// with empty `[workspace.members]` and no source tree.
        #[arg(
            long,
            value_parser = ["bin", "lib", "service", "workspace"],
            default_value = "bin",
        )]
        template: String,
    },
    /// Add a dependency entry to `project.toml`.
    Add {
        /// Project identifier with optional `@VERSION` suffix, or
        /// the Cargo crate spec when `--rust-binding` is set
        /// (e.g. `ratatui@0.26` or `ratatui` for crates.io,
        /// `path:./vendor/ratatui` for a local crate).
        spec: String,
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Add the entry to `[rust-bindings]` instead of
        /// `[dependencies]`. The spec is interpreted as a Cargo
        /// crate spec; `gos` scaffolds a wrapper crate under
        /// `.gos-bindings/<crate-name>/` so user-supplied
        /// `register_module!` blocks can expose the crate to
        /// Gossamer code.
        #[arg(long = "rust-binding")]
        rust_binding: bool,
    },
    /// Remove a dependency entry from `project.toml`.
    Remove {
        /// Project identifier to drop.
        id: String,
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Re-emit `project.toml` keeping only its declared dependencies.
    Tidy {
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
    },
    /// Resolve and fetch every dependency into the local cache.
    Fetch {
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Refuse to populate cache entries that aren't already
        /// present.
        #[arg(long)]
        offline: bool,
    },
    /// Copy fetched dependencies into a local `./vendor/` directory.
    Vendor {
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Output directory. Defaults to `./vendor`.
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// Reformat a source file using the AST pretty-printer.
    ///
    /// With no path: when a `project.toml` is reachable above the
    /// current directory, every `.gos` under the project's `src/`
    /// is formatted in place.
    Fmt {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        file: Option<PathBuf>,
        /// Check whether the file is already formatted; exit 1 if not.
        #[arg(long)]
        check: bool,
    },
    /// Emit an item listing derived from doc comments / signatures.
    Doc {
        /// Path to a `.gos` source file.
        file: PathBuf,
        /// Write an HTML page to this path instead of printing a
        /// plain-text index to stdout.
        #[arg(long)]
        html: Option<PathBuf>,
    },
    /// Discover and run `#[test]` functions.
    ///
    /// With no path, walks `src/` from the nearest `project.toml`.
    /// With a directory, walks every `.gos` under it. With a file,
    /// runs just that file.
    Test {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        path: Option<PathBuf>,
        /// Run only tests whose name matches this regex.
        #[arg(long)]
        run: Option<String>,
        /// Number of tests to run in parallel. Defaults to 1.
        #[arg(long)]
        parallel: Option<usize>,
        /// Output format. Defaults to the human-readable line
        /// format. `junit` writes `JUnit` XML to stdout.
        #[arg(long)]
        format: Option<String>,
        /// Optional path to write `JUnit` XML output to. If omitted
        /// while `--format junit`, the XML goes to stdout.
        #[arg(long)]
        junit_out: Option<PathBuf>,
        /// Enable the data-race detector. Instruments heap accesses
        /// with `gos_rt_race_access` calls and prints a non-empty
        /// race report (and exits non-zero) when an unsynchronised
        /// access pair is observed.
        #[arg(long)]
        race: bool,
        /// Write per-test branch coverage to `<path>` in lcov format.
        #[arg(long, value_name = "FILE")]
        coverage: Option<PathBuf>,
    },
    /// Discover and time `#[bench]` functions.
    Bench {
        /// Path to a `.gos` source file.
        file: PathBuf,
        /// Number of iterations to average. Defaults to 100.
        #[arg(long)]
        iterations: Option<u32>,
    },
    /// Run the built-in lint suite over one file or every `.gos`
    /// source under a directory.
    ///
    /// With no path: when a `project.toml` is reachable above the
    /// current directory, every `.gos` under the project's `src/`
    /// is linted.
    Lint {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        path: Option<PathBuf>,
        /// Promote every lint hit to an error.
        #[arg(long)]
        deny_warnings: bool,
        /// Print an explanation for a specific lint id and exit.
        #[arg(long)]
        explain: Option<String>,
        /// Apply every auto-fixable suggestion and write the file
        /// back. Reports the number of edits applied.
        #[arg(long)]
        fix: bool,
    },
    /// Print the long-form explanation for a diagnostic error code.
    ///
    /// Codes come from the diagnostics framework (`GP0001`,
    /// `GR0001`, `GT0001`, …) plus lint codes (`GL0001`…). Mirrors
    /// `rustc --explain`.
    Explain {
        /// The error code to look up.
        code: String,
    },
    /// Print the Gossamer SKILL card to stdout.
    ///
    /// The SKILL card is a self-contained dialect prompt aimed at
    /// LLM coding assistants. Pipe it into a model's system prompt
    /// (e.g. `gos skill-prompt | claude --append-system-prompt`)
    /// to teach the model idiomatic Gossamer in one step.
    SkillPrompt,
    /// Re-run `gos <inner>` whenever a `.gos` file under `path`
    /// changes.
    Watch {
        /// Subcommand to invoke on every change.
        #[arg(long, default_value = "check")]
        command: String,
        /// Directory to watch, or a single file.
        path: PathBuf,
        /// Extra arguments forwarded to the inner command.
        #[arg(last = true)]
        forward: Vec<String>,
    },
    /// Interactive read-eval-print loop. Bare `gos` with no args
    /// also drops into this.
    Repl,
    /// Start the language-server-protocol adapter on stdio. Intended
    /// to be invoked by an editor, not a human.
    Lsp,
    /// Print toolchain environment for diagnosing install issues.
    ///
    /// Surfaces the `gos` version, runtime static-lib path, host
    /// triple, target dir, project root, and host `cc` path. Drop
    /// in any "is my install OK?" support ticket to halve the
    /// back-and-forth.
    Env,
    /// Generate shell completion script for the chosen shell.
    ///
    /// Pipe the output into the shell's completion directory:
    ///   bash:  `gos completion bash > /etc/bash_completion.d/gos`
    ///   zsh:   `gos completion zsh > $fpath[1]/_gos`
    ///   fish:  `gos completion fish > ~/.config/fish/completions/gos.fish`
    Completion {
        /// Shell to emit completions for.
        shell: clap_complete::Shell,
    },
    /// Remove cached artefacts produced by the toolchain.
    ///
    /// By default clears the frontend parse cache (where `gos check`
    /// stores parsed ASTs keyed by source hash). Pass `--vendor` to
    /// also remove the current project's `./vendor/` directory.
    Clean {
        /// Also remove `./vendor/` (the fetched-dependencies tree).
        #[arg(long)]
        vendor: bool,
        /// Report what would be removed without touching anything.
        #[arg(long, short = 'n')]
        dry_run: bool,
    },
}

/// Parses `argv`, dispatches the chosen subcommand, and maps any
/// `Err` into a non-zero exit code with a styled `error:` prefix.
pub(crate) fn run() -> ExitCode {
    let cli = Cli::parse();
    match dispatch(cli.command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {err:#}", style::error("error"));
            ExitCode::FAILURE
        }
    }
}

/// Routes the parsed [`Command`] to the matching `cmd::*` module.
/// Kept as a flat match so each new subcommand is one line — the
/// place to look when a flag stops landing where you expect.
fn dispatch(command: Option<Command>) -> anyhow::Result<()> {
    match command {
        None | Some(Command::Repl) => repl::cmd_repl(),
        Some(Command::Parse { file }) => cmd::parse::run(&file),
        Some(Command::Check { file, timings }) => cmd::check::dispatch(file, timings),
        Some(Command::Run {
            file,
            tree_walker,
            no_jit,
            args,
        }) => dispatch_run(file, tree_walker, no_jit, &args),
        Some(Command::Build {
            file,
            target,
            release,
            debug_info,
            dynamic,
            reproducible,
        }) => dispatch_build(
            file,
            target.as_deref(),
            BuildFlags {
                mode: if release {
                    BuildMode::Release
                } else {
                    BuildMode::Debug
                },
                link: if dynamic {
                    LinkMode::Dynamic
                } else {
                    LinkMode::Static
                },
                debug_info,
                reproducible,
            },
        ),
        Some(Command::Init { id }) => cmd::scaffold::init(&id),
        Some(Command::New { id, path, template }) => cmd::scaffold::new(&id, path, &template),
        Some(Command::Add {
            spec,
            manifest,
            rust_binding,
        }) => {
            if rust_binding {
                cmd::pkg::add_rust_binding(&spec, manifest)
            } else {
                cmd::pkg::add(&spec, manifest)
            }
        }
        Some(Command::Remove { id, manifest }) => cmd::pkg::remove(&id, manifest),
        Some(Command::Tidy { manifest }) => cmd::pkg::tidy(manifest),
        Some(Command::Fetch { manifest, offline }) => cmd::pkg::fetch(manifest, offline),
        Some(Command::Vendor { manifest, out }) => cmd::pkg::vendor(manifest, out),
        Some(Command::Fmt { file, check }) => cmd::fmt_cmd::dispatch(file, check),
        Some(Command::Doc { file, html }) => doc::cmd_doc(&file, html.as_deref()),
        Some(Command::Test {
            path,
            run,
            parallel,
            format,
            junit_out,
            race,
            coverage,
        }) => cmd::test::run_with_opts(TestOpts {
            path: path.as_deref().map(Path::to_path_buf),
            run_filter: run,
            parallel: parallel.unwrap_or(1),
            format: format.unwrap_or_else(|| "human".to_string()),
            junit_out,
            race,
            coverage,
        }),
        Some(Command::Bench { file, iterations }) => {
            cmd::bench::run(&file, iterations.unwrap_or(100))
        }
        Some(Command::Lint {
            path,
            deny_warnings,
            explain,
            fix,
        }) => cmd::lint_cmd::dispatch(path, deny_warnings, explain.as_deref(), fix),
        Some(Command::Explain { code }) => cmd::explain::run(&code),
        Some(Command::SkillPrompt) => {
            cmd::skill_prompt::run();
            Ok(())
        }
        Some(Command::Watch {
            command,
            path,
            forward,
        }) => cmd::watch::run(&command, &path, &forward),
        Some(Command::Lsp) => cmd::lsp_cmd::run(),
        Some(Command::Env) => {
            cmd::env_cmd::run();
            Ok(())
        }
        Some(Command::Completion { shell }) => {
            use clap::CommandFactory;
            clap_complete::generate(shell, &mut Cli::command(), "gos", &mut std::io::stdout());
            Ok(())
        }
        Some(Command::Clean { vendor, dry_run }) => cmd::clean::run(vendor, dry_run),
    }
}

fn dispatch_run(
    file: Option<PathBuf>,
    tree_walker: bool,
    no_jit: bool,
    args: &[String],
) -> anyhow::Result<()> {
    let mode = if tree_walker {
        RunMode::TreeWalker
    } else {
        RunMode::Vm
    };
    if no_jit {
        gossamer_interp::set_jit_disabled();
    }
    cmd::run::dispatch(file, mode, args)
}

/// Codegen tier. `Debug` routes through Cranelift end-to-end; `Release`
/// runs the LLVM pipeline at `-O3` with per-function fallback to
/// Cranelift for un-lowered constructs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildMode {
    Debug,
    Release,
}

/// Linker strategy. `Static` (Linux release) drives `rust-lld -static`
/// against rustup's musl self-contained CRT; `Dynamic` falls back to
/// the host `cc` and a glibc-linked binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkMode {
    Static,
    Dynamic,
}

/// Bundled flags for `gos build`. Two orthogonal three-state knobs
/// (`mode`, `link`) plus two genuinely-boolean toggles. Kept as a
/// struct so the dispatch site stays under clippy's
/// `fn_params_excessive_bools` threshold and the field names tell
/// the reader what each toggle does at the call site.
#[derive(Debug, Clone, Copy)]
struct BuildFlags {
    mode: BuildMode,
    link: LinkMode,
    debug_info: bool,
    reproducible: bool,
}

fn dispatch_build(
    file: Option<PathBuf>,
    target: Option<&str>,
    flags: BuildFlags,
) -> anyhow::Result<()> {
    if flags.debug_info {
        gossamer_codegen_llvm::set_debug_info(true);
    }
    if flags.reproducible {
        gossamer_codegen_llvm::set_reproducible(true);
    }
    cmd::build::dispatch(
        file,
        target,
        flags.mode == BuildMode::Release,
        flags.debug_info,
        flags.link == LinkMode::Dynamic,
    )
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    #[test]
    fn bare_invocation_parses() {
        assert!(Cli::try_parse_from(["gos"]).is_ok());
    }

    #[test]
    fn parse_subcommand_requires_file() {
        assert!(Cli::try_parse_from(["gos", "parse"]).is_err());
        assert!(Cli::try_parse_from(["gos", "parse", "hello.gos"]).is_ok());
    }

    #[test]
    fn run_subcommand_parses_without_extra_flags() {
        let ok = Cli::try_parse_from(["gos", "run", "hello.gos"]);
        assert!(ok.is_ok());
    }

    #[test]
    fn run_subcommand_accepts_tree_walker_flag() {
        let ok = Cli::try_parse_from(["gos", "run", "hello.gos", "--tree-walker"]);
        assert!(ok.is_ok());
    }

    #[test]
    fn run_subcommand_rejects_removed_vm_flag() {
        // `--vm` was the strict-VM opt-in before VM became the
        // default. Keep an explicit rejection test so a future
        // resurrection of the flag is a deliberate decision.
        let err = Cli::try_parse_from(["gos", "run", "hello.gos", "--vm"]);
        assert!(err.is_err());
    }

    #[test]
    fn build_subcommand_parses_target() {
        let ok = Cli::try_parse_from([
            "gos",
            "build",
            "hello.gos",
            "--target",
            "x86_64-unknown-linux-gnu",
        ]);
        assert!(ok.is_ok());
    }

    #[test]
    fn build_subcommand_rejects_output_flag() {
        let err = Cli::try_parse_from(["gos", "build", "hello.gos", "-o", "hello"]);
        assert!(
            err.is_err(),
            "-o should be rejected now that output lives in project.toml"
        );
    }
}
