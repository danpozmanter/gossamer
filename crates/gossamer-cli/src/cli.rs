//! `gos` argument parsing + dispatch table.
//!
//! Owning the `clap`-derived `Cli` / `Command` types here keeps
//! `main.rs` to just a runtime entry point. Every variant matches in
//! `run` to a single line that delegates to a `crate::cmd::*`
//! module.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Arg, CommandFactory, FromArgMatches, Parser, Subcommand, ValueHint};

use crate::cmd::{self, TestOpts};
use crate::style;
use crate::{doc, repl};

/// Top-level parsed command line for the `gos` binary.
#[derive(Debug, Parser)]
#[command(name = "gos", version, about = "The Gossamer toolchain")]
pub(crate) struct Cli {
    /// Print additional progress information for the command being run.
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Execute inline Gossamer source.
    #[arg(short = 'c', long = "command")]
    eval: Option<String>,
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
        /// Diagnostic output format.
        ///
        /// `plain` (default) renders each diagnostic as a
        /// rustc/elm-style coloured text frame on stderr. `json`
        /// renders each diagnostic as a single-line JSON object
        /// with a stable schema; consumers can stream the output
        /// through `jq` or equivalent. The schema lives in
        /// `gossamer_diagnostics::render_json`.
        #[arg(long, value_enum, default_value_t = MessageFormat::Plain)]
        message_format: MessageFormat,
    },
    /// Restart a development program whenever local project inputs change.
    #[command(alias = "dev")]
    Watch {
        /// Path to a `.gos` source file. Defaults to the project's entry point.
        file: Option<PathBuf>,
        /// Quiet period after the final edit event before checking and restarting.
        #[arg(long, default_value_t = 150, value_name = "MS")]
        debounce: u64,
        /// Maximum graceful-shutdown time for the replaced child.
        #[arg(long, default_value_t = 5_000, value_name = "MS")]
        grace: u64,
        /// Skip validation and restart immediately after an edit.
        #[arg(long)]
        no_check: bool,
        /// Clear the terminal before each successful restart.
        #[arg(long)]
        clear: bool,
        /// Require a matching project.lock before check and run.
        #[arg(long)]
        locked: bool,
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
        /// Run the full LLVM `-O3` optimisation pipeline. Without
        /// this flag, `gos build` uses the lightweight debug MIR
        /// pipeline and minimal register promotion plus `llc -O0`
        /// (faster compile, lightly canonicalised native code). Both modes use the LLVM
        /// backend; the Cranelift code path is reserved for the
        /// in-process JIT and is no longer reachable from `gos
        /// build`. Any MIR shape the LLVM lowerer cannot handle is
        /// a hard build failure.
        #[arg(long)]
        release: bool,
        /// Emit LLVM instrumentation that writes raw execution profiles to
        /// this path when the resulting release binary exits.
        #[arg(
            long,
            value_name = "PATH",
            requires = "release",
            conflicts_with = "pgo_profile"
        )]
        pgo_collect: Option<PathBuf>,
        /// Optimise a release build with a merged LLVM `.profdata` file.
        #[arg(
            long,
            value_name = "PATH",
            requires = "release",
            conflicts_with = "pgo_collect"
        )]
        pgo_profile: Option<PathBuf>,
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
        /// Emit one machine-readable line with build-phase timings.
        #[arg(long)]
        timings: bool,
        /// Print the selected MIR, LLVM, and target linker optimization plan.
        #[arg(long)]
        explain_profile: bool,
        /// Produce a bit-identical artifact across two clean builds
        /// of the same source on the same target. Pins the build
        /// timestamp via `SOURCE_DATE_EPOCH`, strips embedded
        /// absolute paths, and sorts symbol tables. Two reproducible
        /// builds of the same input compared with `cmp` should
        /// match byte-for-byte.
        #[arg(long)]
        reproducible: bool,
        /// Override the directory the linked binary is written to.
        /// When unset, the binary lands under `target/{debug,release}`
        /// next to the entry source's manifest. The directory is
        /// created if it does not exist.
        #[arg(long = "out-dir")]
        out_dir: Option<PathBuf>,
        /// Allow individual function bodies to silently fall back to
        /// the Cranelift codegen when the LLVM backend hits an
        /// unsupported MIR shape. Default release builds reject any
        /// such fallback as a hard error so users get the LLVM
        /// quality they pay for; this flag is the explicit opt-out.
        #[arg(long = "allow-llvm-fallback")]
        allow_llvm_fallback: bool,
        /// Require `project.lock` to be present and match the
        /// resolver's output for every dep. Drift is a hard error.
        /// CI builds should set this so a stale lockfile never
        /// silently builds against an upgraded dep.
        #[arg(long)]
        locked: bool,
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
            value_parser = ["bin", "lib", "service", "workspace", "binding"],
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
        /// Re-walk the registry index and rewrite `project.lock`
        /// even when the existing lock pins a satisfying version.
        #[arg(long)]
        update: bool,
    },
    /// Resolve the newest dependency versions allowed by the manifest,
    /// refresh the local cache, and rewrite `project.lock`.
    Update {
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Resolve using only registry metadata and packages already cached.
        #[arg(long)]
        offline: bool,
    },
    /// Publish the current project to a registry.
    Publish {
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
        /// Override the registry URL. Defaults to the project's
        /// `[registries].default` or `$GOS_REGISTRY_URL`.
        #[arg(long)]
        registry: Option<String>,
        /// Pack + sign + print metadata without uploading.
        #[arg(long)]
        dry_run: bool,
    },
    /// Yank a previously-published version.
    Yank {
        /// Spec of the form `<id>@<version>`.
        spec: String,
        /// Optional human-readable reason recorded with the yank.
        #[arg(long)]
        reason: Option<String>,
    },
    /// Save a bearer token for a registry in
    /// `~/.gossamer/credentials.toml`.
    Login {
        /// Registry URL the token authorises against.
        #[arg(long)]
        registry: String,
    },
    /// Drop the saved bearer token for a registry.
    Logout {
        /// Registry URL whose credential to drop.
        #[arg(long)]
        registry: String,
    },
    /// Manage owners (publisher ACL) of a published project.
    Owner {
        /// Operation: `add`, `remove`, or `list`.
        op: String,
        /// Project id (e.g. `example.com/widget`).
        id: String,
        /// User to add or remove. Omit for `list`.
        user: Option<String>,
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
    /// Scaffold a `#[gos_module]` binding skeleton from a Rust
    /// source file. Walks the supplied file's `pub fn` items,
    /// classifies each by whether its signature uses types the
    /// binding ABI supports (`String`, `i64`, `bool`, `Vec<T>`,
    /// `Option<T>`, `Result<T, E>`, tuples, `Bytes`, user structs
    /// with `#[derive(GosStruct)]`), and emits a ready-to-edit
    /// binding crate under the output directory. Functions whose
    /// signatures use unsupported types are emitted as `///
    /// Unsupported` comments so the binding author sees the gap.
    Bindgen {
        /// Path to the Rust source file or crate root to scan.
        input: PathBuf,
        /// Output directory for the scaffolded binding crate.
        /// Defaults to `./.gos-bindings/<crate-name>/`.
        #[arg(long)]
        output: Option<PathBuf>,
        /// Gossamer-side module path to use. Defaults to the
        /// crate-name with `-` replaced by `_`.
        #[arg(long)]
        module: Option<String>,
    },
    /// Emit an item listing derived from doc comments / signatures.
    Doc {
        /// Path to a `.gos` source file. Optional when using
        /// `--emit-stdlib`.
        file: Option<PathBuf>,
        /// Write an HTML page to this path instead of printing a
        /// plain-text index to stdout.
        #[arg(long)]
        html: Option<PathBuf>,
        /// Emit one Markdown page per stdlib module under the
        /// supplied output directory. Walks
        /// `gossamer_std::manifest::ALL_MODULES`. The page set
        /// matches what's published to the GitHub Pages site.
        #[arg(long, value_name = "DIR")]
        emit_stdlib: Option<PathBuf>,
        /// Verify that committed docs match the manifest. Fails
        /// with non-zero exit when any page is missing or stale.
        #[arg(long, requires = "emit_stdlib")]
        check: bool,
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
        /// List discovered tests without executing or typechecking modules.
        #[arg(long)]
        list: bool,
        /// Select one test by its complete name.
        #[arg(long, value_name = "NAME", conflicts_with = "run")]
        exact: Option<String>,
        /// Stop scheduling tests after the first failure.
        #[arg(long)]
        fail_fast: bool,
        /// Include tests carrying `#[ignore]`.
        #[arg(long)]
        include_ignored: bool,
        /// Run only tests carrying `#[ignore]`.
        #[arg(long, conflicts_with = "include_ignored")]
        ignored_only: bool,
        /// Randomize test order and print the replayable seed.
        #[arg(long)]
        shuffle: bool,
        /// Seed used by `--shuffle`.
        #[arg(long, requires = "shuffle")]
        seed: Option<u64>,
        /// Maximum duration for each test file, such as `30s` or `500ms`.
        #[arg(long, value_name = "DURATION")]
        timeout: Option<String>,
        /// Internal isolated-test worker marker.
        #[arg(long, hide = true)]
        test_worker: bool,
        /// Number of test files to run in parallel. Defaults to the
        /// number of logical CPUs. Use `--serial` to force sequential
        /// execution, or `--parallel 1` for the same effect.
        #[arg(long)]
        parallel: Option<usize>,
        /// Run tests sequentially (equivalent to `--parallel 1`).
        /// Useful for reproducible output ordering or when tests share
        /// global process state that parallelism would corrupt.
        #[arg(long, conflicts_with = "parallel")]
        serial: bool,
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
        /// Run the cross-tier parity walk instead of `#[test]`
        /// discovery. Targets every `.gos` source under `path`
        /// (defaults to `examples/` + `feature-testing-examples/`),
        /// running each through the VM and the LLVM-compiled binary.
        #[arg(long = "tier-parity")]
        tier_parity: bool,
        /// Report shape for `--tier-parity`. Only `status` is
        /// implemented today; it writes
        /// `target/debug/.feature-status.json` consumed by
        /// `gos feature-status`.
        #[arg(long)]
        report: Option<String>,
    },
    /// Discover and time `#[bench]` functions.
    ///
    /// With no path, walks `src/` from the nearest `project.toml`.
    /// With a directory, walks every `.gos` under it. With a file,
    /// benches just that file.
    Bench {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        path: Option<PathBuf>,
        /// Number of files to bench in parallel. Defaults to 1 so
        /// per-bench timings stay reproducible - two CPU-bound
        /// benches sharing a core perturb each other's measurements.
        #[arg(long)]
        parallel: Option<usize>,
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
    /// Interactive read-eval-print loop. Bare `gos` with no args
    /// also drops into this.
    Repl,
    /// Start the language-server-protocol adapter on stdio. Intended
    /// to be invoked by an editor, not a human.
    Lsp,
    /// Start the model-context-protocol server on stdio. Exposes the
    /// toolchain (check / explain / execute / build / test / fmt / doc)
    /// and semantic navigation as MCP tools for AI coding agents;
    /// intended to be launched by an MCP client, not a human.
    Mcp,
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
    /// Inspect, prune, or clear Gossamer cache roots.
    Cache {
        /// Print only cache-class names and paths.
        #[arg(long, conflicts_with_all = ["prune", "clear"])]
        path: bool,
        /// Prune files older than the retention window or beyond the cache cap.
        #[arg(long, conflicts_with_all = ["path", "clear"])]
        prune: bool,
        /// Remove every Gossamer cache class.
        #[arg(long, conflicts_with_all = ["path", "prune"])]
        clear: bool,
        /// Report cache removal without deleting files.
        #[arg(long, short = 'n')]
        dry_run: bool,
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
        /// Remove frontend parse cache entries.
        #[arg(long)]
        frontend: bool,
        /// Remove LLVM incremental object caches.
        #[arg(long)]
        ir: bool,
        /// Remove cached Rust-binding runner and staticlib builds.
        #[arg(long)]
        runners: bool,
        /// Remove fetched package source cache entries.
        #[arg(long)]
        packages: bool,
        /// Remove legacy build cache entries.
        #[arg(long)]
        build_cache: bool,
        /// Remove every toolchain cache class.
        #[arg(long)]
        all: bool,
    },
    /// Print lifecycle status for every language feature and stdlib
    /// item. Joins the registry in
    /// `gossamer_std::manifest::FEATURE_STATUS` with per-tier
    /// outcomes loaded from `target/debug/.feature-status.json`.
    /// Pass `--check` to enforce the CI gate (every `Stable` item
    /// must have a doc page plus an all-tiers-pass test record).
    #[command(name = "feature-status")]
    FeatureStatus {
        /// Output format. Defaults to a pipe-separated table.
        #[arg(long, default_value = "table")]
        format: String,
        /// CI gate mode - exit non-zero with a punch list when any
        /// `Stable` item lacks a doc page or a passing tier-parity test.
        #[arg(long)]
        check: bool,
        /// Optional glob filter on the qualified path (`std::http::*`).
        #[arg(long)]
        filter: Option<String>,
        /// Optional status filter (`stable` / `shipped` / `experimental` / `planned` / `removed`).
        #[arg(long)]
        status: Option<String>,
        /// Override the JSON sidecar path. Defaults to
        /// `target/debug/.feature-status.json`.
        #[arg(long)]
        sidecar: Option<PathBuf>,
        /// Override the docs root used by `--check`. Defaults to
        /// `docs_src/` next to the workspace root.
        #[arg(long = "docs-root")]
        docs_root: Option<PathBuf>,
    },
}

/// Parses `argv`, dispatches the chosen subcommand, and maps any
/// `Err` into a non-zero exit code with a styled `error:` prefix.
pub(crate) fn run() -> ExitCode {
    let cli = parse_cli();
    match dispatch(cli.command, cli.verbose, cli.eval) {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {err:#}", style::error("error"));
            ExitCode::FAILURE
        }
    }
}

/// Returns whether `args` are a direct program invocation such as
/// `gos ./tool`, `gos ./tool.gos`, or `gos ./project`. This is the form the
/// kernel uses for a `#!/usr/bin/env gos` hashbang.
#[must_use]
pub fn is_direct_script_invocation(args: &[std::ffi::OsString]) -> bool {
    parse_direct_script(args).is_some()
}

/// Executes a program passed directly to `gos`, forwarding every remaining
/// argument to its `main` function. Execution options must precede the path,
/// so program arguments beginning with `-` are forwarded unchanged.
#[must_use]
pub fn try_script_run(args: &[std::ffi::OsString]) -> Option<ExitCode> {
    let parsed = parse_direct_script(args)?;
    let result = (|| {
        crate::cmd::pkg::enforce_lockfile_if_requested(parsed.locked)?;
        if parsed.no_jit {
            gossamer_interp::set_jit_disabled();
        }
        cmd::run::dispatch(Some(parsed.file), parsed.main_thread, &parsed.forwarded)
    })();
    Some(match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("{}: {err:#}", style::error("error"));
            ExitCode::FAILURE
        }
    })
}

struct DirectScript {
    file: PathBuf,
    no_jit: bool,
    main_thread: bool,
    locked: bool,
    forwarded: Vec<String>,
}

/// Parses the narrow direct-invocation grammar without taking ownership of
/// ordinary subcommands. The path may name a source file or a project
/// directory; the latter resolves through `project.toml` in `cmd::run`.
fn parse_direct_script(args: &[std::ffi::OsString]) -> Option<DirectScript> {
    let mut no_jit = false;
    let mut main_thread = false;
    let mut locked = false;
    let mut index = 1;

    while let Some(arg) = args.get(index) {
        let text = arg.to_str()?;
        match text {
            "--no-jit" if !no_jit => no_jit = true,
            "--main-thread" if !main_thread => main_thread = true,
            "--locked" if !locked => locked = true,
            _ if text.starts_with('-') => return None,
            _ => {
                let file = PathBuf::from(arg);
                let path = Path::new(&file);
                if !path.is_file()
                    && !path.is_dir()
                    && !crate::paths::resolve_gos_source(&file).is_file()
                    && path.extension().is_none_or(|extension| extension != "gos")
                {
                    return None;
                }
                let forwarded = args[index + 1..]
                    .iter()
                    .map(|arg| arg.to_str().map(str::to_owned))
                    .collect::<Option<Vec<_>>>()?;
                return Some(DirectScript {
                    file,
                    no_jit,
                    main_thread,
                    locked,
                    forwarded,
                });
            }
        }
        index += 1;
    }
    None
}

/// Builds the command tree used for generated shell completions.
///
/// Direct script execution is recognized before Clap parses the command line,
/// so it does not otherwise appear in Clap's schema. Adding a file-path
/// positional here teaches shells that `gos <TAB>` accepts any path while
/// retaining completion for the ordinary subcommands.
fn completion_command() -> clap::Command {
    Cli::command().arg(
        Arg::new("script")
            .help("Path to a Gossamer script or project directory")
            .value_name("TARGET")
            .value_hint(ValueHint::FilePath)
            .index(1),
    )
}

fn completion_script(shell: clap_complete::Shell) -> Vec<u8> {
    let mut output = Vec::new();
    clap_complete::generate(shell, &mut completion_command(), "gos", &mut output);
    if shell == clap_complete::Shell::Bash {
        let script = String::from_utf8(output).expect("Clap emits UTF-8 completion text");
        let script = script.replacen(
            "if [[ ${cur} == -* || ${COMP_CWORD} -eq 1 ]] ; then",
            "if [[ ${cur} == -* ]] ; then",
            1,
        );
        let script = script.replacen(
            "COMPREPLY=( $(compgen -W \"${opts}\" -- \"${cur}\") )\n            return 0",
            "COMPREPLY=( $(compgen -W \"${opts}\" -- \"${cur}\") $(compgen -f -- \"${cur}\") )\n            return 0",
            2,
        );
        return script.into_bytes();
    }
    output
}

fn parse_cli() -> Cli {
    // Clap's derived command schema is deeply recursive. Construct it on an
    // explicitly sized stack so Windows' smaller main-thread stack does not
    // overflow before a command starts. Dispatch remains on the caller's
    // thread for commands that require main-thread execution.
    std::thread::Builder::new()
        .name("gos-cli-parser".to_string())
        .stack_size(4 * 1024 * 1024)
        .spawn(|| {
            let matches = Cli::command().term_width(cli_help_width()).get_matches();
            Cli::from_arg_matches(&matches).expect("Clap produced invalid CLI matches")
        })
        .expect("failed to start CLI parser")
        .join()
        .unwrap_or_else(|panic| std::panic::resume_unwind(panic))
}

fn cli_help_width() -> usize {
    const FALLBACK_COLUMNS: usize = 80;
    style::terminal_width(FALLBACK_COLUMNS, 24)
}

/// Routes the parsed [`Command`] to the matching `cmd::*` module.
/// Kept as a flat match so each new subcommand is one line - the
/// place to look when a flag stops landing where you expect.
#[allow(
    clippy::too_many_lines,
    reason = "intentional flat dispatch - one arm per subcommand keeps grep-ability"
)]
fn dispatch(command: Option<Command>, verbose: bool, eval: Option<String>) -> anyhow::Result<()> {
    if let Some(source) = eval {
        return cmd::run::command(source);
    }
    match command {
        None | Some(Command::Repl) => repl::cmd_repl(verbose),
        Some(Command::Parse { file }) => cmd::parse::run(&file),
        Some(Command::Check {
            file,
            timings,
            message_format,
        }) => cmd::check::dispatch(file, timings, message_format),
        Some(Command::Watch {
            file,
            debounce,
            grace,
            no_check,
            clear,
            locked,
            args,
        }) => cmd::watch::run(cmd::watch::Options {
            file,
            debounce: std::time::Duration::from_millis(debounce),
            grace: std::time::Duration::from_millis(grace),
            check: !no_check,
            clear,
            locked,
            args,
        }),
        Some(Command::Build {
            file,
            target,
            release,
            pgo_collect,
            pgo_profile,
            debug_info,
            dynamic,
            timings,
            explain_profile,
            reproducible,
            out_dir,
            locked,
            allow_llvm_fallback,
        }) => {
            crate::cmd::pkg::enforce_lockfile_if_requested(locked)?;
            configure_pgo(release, pgo_collect, pgo_profile)?;
            // 0.9.0 default: a release build that silently falls
            // back to Cranelift is a regression dressed up as a
            // feature. Default-on strict-lowering for --release
            // unless the user explicitly opts out.
            if release && !allow_llvm_fallback {
                gossamer_codegen_llvm::set_strict_lowering(true);
            }
            dispatch_build(
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
                out_dir,
                timings,
                explain_profile,
            )
        }
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
        Some(Command::Fetch {
            manifest,
            offline,
            update,
        }) => cmd::pkg::fetch(manifest, offline, update),
        Some(Command::Update { manifest, offline }) => cmd::pkg::fetch(manifest, offline, true),
        Some(Command::Vendor { manifest, out }) => cmd::pkg::vendor(manifest, out),
        Some(Command::Publish {
            manifest,
            registry,
            dry_run,
        }) => cmd::pkg::publish(manifest, registry, dry_run),
        Some(Command::Yank { spec, reason }) => cmd::pkg::yank(&spec, reason),
        Some(Command::Login { registry }) => cmd::pkg::login(registry),
        Some(Command::Logout { registry }) => cmd::pkg::logout(registry),
        Some(Command::Owner { op, id, user }) => cmd::pkg::owner(&op, &id, user),
        Some(Command::Fmt { file, check }) => cmd::fmt_cmd::dispatch(file, check),
        Some(Command::Bindgen {
            input,
            output,
            module,
        }) => cmd::bindgen::run(&input, output.as_deref(), module.as_deref()),
        Some(Command::Doc {
            file,
            html,
            emit_stdlib,
            check,
        }) => {
            if let Some(out) = emit_stdlib {
                doc::cmd_emit_stdlib(&out, check)
            } else if let Some(f) = file {
                doc::cmd_doc(&f, html.as_deref())
            } else {
                Err(anyhow::anyhow!(
                    "gos doc: pass a source file or --emit-stdlib DIR"
                ))
            }
        }
        Some(Command::Test {
            path,
            run,
            list,
            exact,
            fail_fast,
            include_ignored,
            ignored_only,
            shuffle,
            seed,
            timeout,
            test_worker,
            parallel,
            serial,
            format,
            junit_out,
            race,
            coverage,
            tier_parity,
            report,
        }) => {
            let cpu_count =
                std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
            let n_parallel = if serial {
                1
            } else {
                parallel.unwrap_or(cpu_count)
            };
            cmd::test::run_with_opts(TestOpts {
                path: path.as_deref().map(Path::to_path_buf),
                run_filter: run,
                list,
                exact,
                fail_fast,
                include_ignored,
                ignored_only,
                shuffle,
                seed,
                timeout: timeout
                    .as_deref()
                    .map(cmd::test::parse_timeout)
                    .transpose()?,
                worker: test_worker,
                parallel: n_parallel,
                format: format.unwrap_or_else(|| "human".to_string()),
                junit_out,
                race,
                coverage,
                tier_parity,
                report,
            })
        }
        Some(Command::Bench { path, parallel }) => {
            cmd::bench::run_with_opts(cmd::bench::BenchOpts {
                path,
                parallel: parallel.unwrap_or(1),
            })
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
        Some(Command::Lsp) => cmd::lsp_cmd::run(),
        Some(Command::Mcp) => cmd::mcp_cmd::run(),
        Some(Command::Env) => {
            cmd::env_cmd::run();
            Ok(())
        }
        Some(Command::Completion { shell }) => {
            std::io::stdout().write_all(&completion_script(shell))?;
            Ok(())
        }
        Some(Command::Cache {
            path,
            prune,
            clear,
            dry_run,
        }) => {
            if clear {
                cmd::cache::clear(dry_run)
            } else if prune {
                cmd::cache::prune(dry_run)
            } else {
                cmd::cache::status(path)
            }
        }
        Some(Command::Clean {
            vendor,
            dry_run,
            frontend,
            ir,
            runners,
            packages,
            build_cache,
            all,
        }) => {
            use gossamer_driver::cache_maintenance::CacheClass;
            let mut classes = Vec::new();
            if all {
                classes.extend(CacheClass::all());
            } else {
                for (enabled, class) in [
                    (frontend, CacheClass::Frontend),
                    (ir, CacheClass::Ir),
                    (runners, CacheClass::Runners),
                    (packages, CacheClass::Packages),
                    (build_cache, CacheClass::Build),
                ] {
                    if enabled {
                        classes.push(class);
                    }
                }
            }
            cmd::clean::run(cmd::clean::Options {
                vendor,
                dry_run,
                classes,
            })
        }
        Some(Command::FeatureStatus {
            format,
            check,
            filter,
            status,
            sidecar,
            docs_root,
        }) => dispatch_feature_status(
            &format,
            check,
            filter,
            status.as_deref(),
            sidecar,
            docs_root,
        ),
    }
}

fn configure_pgo(
    release: bool,
    collect: Option<PathBuf>,
    profile: Option<PathBuf>,
) -> anyhow::Result<()> {
    use gossamer_codegen_llvm::PgoMode;

    if collect.is_some() && profile.is_some() {
        return Err(anyhow::anyhow!(
            "--pgo-collect and --pgo-profile cannot be used together"
        ));
    }
    if (collect.is_some() || profile.is_some()) && !release {
        return Err(anyhow::anyhow!("PGO requires `gos build --release`"));
    }
    let mode = match (collect, profile) {
        (Some(path), None) => Some(PgoMode::Collect(path)),
        (None, Some(path)) if path.is_file() => Some(PgoMode::Profile(path)),
        (None, Some(path)) => {
            return Err(anyhow::anyhow!(
                "PGO profile does not exist or is not a file: {}",
                path.display()
            ));
        }
        (None, None) => None,
        (Some(_), Some(_)) => unreachable!("checked above"),
    };
    gossamer_codegen_llvm::set_pgo_mode(mode);
    Ok(())
}

fn dispatch_feature_status(
    format: &str,
    check: bool,
    filter: Option<String>,
    status: Option<&str>,
    sidecar: Option<PathBuf>,
    docs_root: Option<PathBuf>,
) -> anyhow::Result<()> {
    let format = cmd::feature_status::OutputFormat::parse(format)
        .ok_or_else(|| anyhow::anyhow!("unknown --format: {format} (table|json|markdown)"))?;
    let status = match status {
        Some(tag) => Some(gossamer_std::manifest::Status::parse(tag).ok_or_else(|| {
            anyhow::anyhow!("unknown --status: {tag} (stable|shipped|experimental|planned|removed)")
        })?),
        None => None,
    };
    cmd::feature_status::run(cmd::feature_status::FeatureStatusOpts {
        format,
        check,
        filter,
        status,
        sidecar,
        docs_root,
    })
}

/// Codegen optimisation level. Both modes go through the LLVM
/// backend; the difference is the MIR and LLVM optimization profile.
/// `Debug` uses the lightweight pipeline plus minimal `opt` and `llc -O0`;
/// `Release` runs the full mid-level pipeline plus Clang `-O3`. The Cranelift
/// backend is reserved for the in-process JIT and is not a `gos
/// build` target in 0.5.0.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuildMode {
    Debug,
    Release,
}

/// Diagnostic output format selected by `--message-format`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum MessageFormat {
    /// rustc/elm-style coloured text frame on stderr (default).
    Plain,
    /// One JSON object per diagnostic, single line, stable schema.
    /// See `gossamer_diagnostics::render_json`.
    Json,
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
    out_dir: Option<PathBuf>,
    timings: bool,
    explain_profile: bool,
) -> anyhow::Result<()> {
    if flags.debug_info {
        gossamer_codegen_llvm::set_debug_info(true);
    }
    if flags.reproducible {
        gossamer_codegen_llvm::set_reproducible(true);
    }
    cmd::build::dispatch(cmd::build::BuildRequest {
        path: file,
        target,
        link: cmd::build::LinkOptions {
            release: flags.mode == BuildMode::Release,
            debug_info: flags.debug_info,
            dynamic: flags.link == LinkMode::Dynamic,
        },
        out_dir,
        timings,
        explain_profile,
    })
}

#[cfg(test)]
mod tests {
    use super::{Cli, completion_script, configure_pgo, is_direct_script_invocation};
    use clap::Parser;

    fn os_args(args: &[&str]) -> Vec<std::ffi::OsString> {
        args.iter().map(std::ffi::OsString::from).collect()
    }

    #[test]
    fn direct_gos_file_is_reserved_for_script_execution() {
        assert!(is_direct_script_invocation(&os_args(&["gos", "tool.gos"])));
        assert!(!is_direct_script_invocation(&os_args(&["gos", "repl"])));
    }

    #[test]
    fn direct_script_accepts_existing_files_with_any_extension() {
        let root =
            std::env::temp_dir().join(format!("gossamer-direct-script-{}", std::process::id()));
        std::fs::create_dir_all(&root).expect("create fixture directory");
        let extensionless = root.join("extensionless");
        let arbitrary = root.join("program.txt");
        let inferred = root.join("inferred.gos");
        for path in [&extensionless, &arbitrary, &inferred] {
            std::fs::write(path, "fn main() {}\n").expect("write fixture");
        }

        for path in [&extensionless, &arbitrary, &root.join("inferred")] {
            assert!(is_direct_script_invocation(&[
                "gos".into(),
                path.as_os_str().to_owned(),
            ]));
        }
    }

    #[test]
    fn generated_completion_offers_current_directory_filenames() {
        let output = completion_script(clap_complete::Shell::Bash);
        let output = String::from_utf8(output).expect("completion is utf8");
        assert!(output.contains("compgen -f -- \"${cur}\""), "{output}");
        assert!(!output.contains("gos__run"), "{output}");
    }

    #[test]
    fn unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["gos", "execute", "hello.gos"]).is_err());
    }

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
    fn watch_subcommand_parses_control_and_program_args() {
        let ok = Cli::try_parse_from([
            "gos",
            "watch",
            "--debounce",
            "20",
            "--grace",
            "50",
            "--locked",
            "src/main.gos",
            "--",
            "--port",
            "8080",
        ]);
        assert!(ok.is_ok());
        assert!(Cli::try_parse_from(["gos", "watch", "--debounce", "nope"]).is_err());
        assert!(Cli::try_parse_from(["gos", "dev", "src/main.gos"]).is_ok());
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
    fn build_subcommand_parses_timings() {
        assert!(Cli::try_parse_from(["gos", "build", "hello.gos", "--timings"]).is_ok());
    }

    #[test]
    fn build_subcommand_parses_profile_explanation() {
        assert!(Cli::try_parse_from(["gos", "build", "hello.gos", "--explain-profile"]).is_ok());
    }

    #[test]
    fn build_subcommand_parses_release_pgo_modes() {
        assert!(
            Cli::try_parse_from([
                "gos",
                "build",
                "--release",
                "--pgo-collect",
                "workload.profraw",
                "hello.gos",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gos",
                "build",
                "--release",
                "--pgo-profile",
                "merged.profdata",
                "hello.gos",
            ])
            .is_ok()
        );
        assert!(
            Cli::try_parse_from([
                "gos",
                "build",
                "--pgo-collect",
                "workload.profraw",
                "hello.gos",
            ])
            .is_err()
        );
        assert!(
            Cli::try_parse_from([
                "gos",
                "build",
                "--release",
                "--pgo-collect",
                "workload.profraw",
                "--pgo-profile",
                "merged.profdata",
                "hello.gos",
            ])
            .is_err()
        );
    }

    #[test]
    fn pgo_profile_requires_an_existing_file() {
        let err = configure_pgo(
            true,
            None,
            Some(std::path::PathBuf::from("does-not-exist.profdata")),
        )
        .expect_err("missing profile must be rejected before a build begins");
        assert!(err.to_string().contains("does not exist"));
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
