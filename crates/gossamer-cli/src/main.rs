//! `gos` — command-line driver for the Gossamer toolchain.
//! Subcommands wired so far (by implementation-plan phase):
//! - `parse FILE`: print the parsed AST.
//! - `check FILE`/9: parse + resolve + typecheck + exhaustiveness.
//! - `run FILE`/12: bytecode VM by default, `--tree-walker` for the recursive interpreter.
//! - `build FILE`/21: emit a linked artifact (`--target` optional).
//!
//! Phases 27+ (package manager) and 31 (`doc`/`fmt`/`test`/`bench`) arrive
//! in later milestones and are intentionally absent here.

#![forbid(unsafe_code)]
#![allow(clippy::similar_names, clippy::ptr_arg)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context, Result, anyhow};
use clap::{Parser, Subcommand};

mod doc;
mod repl;
mod repl_helper;

/// Top-level parsed command line for the `gos` binary.
#[derive(Debug, Parser)]
#[command(name = "gos", version, about = "The Gossamer toolchain")]
struct Cli {
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
    Check {
        /// Path to a `.gos` source file.
        file: PathBuf,
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
    Run {
        /// Path to a `.gos` source file.
        file: PathBuf,
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
    /// Produce a linked artifact on disk. Default output path is
    /// either `project.output` (walking up from the source file for
    /// the nearest `project.toml`) or the source stem beside the
    /// input when no manifest is present.
    Build {
        /// Path to a `.gos` source file.
        file: PathBuf,
        /// Cross-compilation target triple (e.g. `aarch64-apple-darwin`).
        #[arg(long)]
        target: Option<String>,
        /// Route codegen through the LLVM backend with `-O3` for
        /// production builds. Falls back to Cranelift when LLVM lowerer
        /// does not cover a construct yet.
        #[arg(long)]
        release: bool,
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
        /// Project identifier with optional `@VERSION` suffix.
        spec: String,
        /// Path to the manifest. Defaults to `./project.toml`.
        #[arg(long)]
        manifest: Option<PathBuf>,
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
    Fmt {
        /// Path to a `.gos` source file.
        file: PathBuf,
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
    /// Discover and run `#[test]` functions through the tree-walker.
    /// Accepts either a single `.gos` file or a directory; when
    /// given a directory, walks every `.gos` under it. When no
    /// path is supplied, walks `src/` from the nearest enclosing
    /// `project.toml` (or the current directory when no project
    /// manifest is found).
    Test {
        /// Path to a `.gos` source file or a directory to walk.
        /// Optional: defaults to the project's `src/` directory.
        path: Option<PathBuf>,
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
    Lint {
        /// Path to a `.gos` source file or a directory to walk.
        path: PathBuf,
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
    /// `rustc --explain`. Stream H.6.
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
    /// changes. Stream H.5.
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
    /// also drops into this. Stream K.
    Repl,
    /// Start the language-server-protocol adapter on stdio. Intended
    /// to be invoked by an editor, not a human.
    Lsp,
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

/// Entry point. Returns a non-zero exit code when a subcommand fails.
fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        None => repl::cmd_repl(),
        Some(Command::Parse { file }) => cmd_parse(&file),
        Some(Command::Check { file, timings }) => cmd_check(&file, timings),
        Some(Command::Run {
            file,
            tree_walker,
            no_jit,
            args,
        }) => {
            let mode = if tree_walker {
                RunMode::TreeWalker
            } else {
                RunMode::Vm
            };
            if no_jit {
                gossamer_interp::set_jit_disabled();
            }
            cmd_run(&file, mode, &args)
        }
        Some(Command::Build {
            file,
            target,
            release,
        }) => cmd_build(&file, target.as_deref(), release),
        Some(Command::Init { id }) => cmd_init(&id),
        Some(Command::New { id, path, template }) => cmd_new(&id, path, &template),
        Some(Command::Add { spec, manifest }) => cmd_add(&spec, manifest),
        Some(Command::Remove { id, manifest }) => cmd_remove(&id, manifest),
        Some(Command::Tidy { manifest }) => cmd_tidy(manifest),
        Some(Command::Fetch { manifest, offline }) => cmd_fetch(manifest, offline),
        Some(Command::Vendor { manifest, out }) => cmd_vendor(manifest, out),
        Some(Command::Fmt { file, check }) => cmd_fmt(&file, check),
        Some(Command::Doc { file, html }) => doc::cmd_doc(&file, html.as_deref()),
        Some(Command::Test { path }) => cmd_test(path.as_deref()),
        Some(Command::Bench { file, iterations }) => cmd_bench(&file, iterations.unwrap_or(100)),
        Some(Command::Lint {
            path,
            deny_warnings,
            explain,
            fix,
        }) => cmd_lint(&path, deny_warnings, explain.as_deref(), fix),
        Some(Command::Explain { code }) => cmd_explain(&code),
        Some(Command::SkillPrompt) => {
            cmd_skill_prompt();
            Ok(())
        }
        Some(Command::Watch {
            command,
            path,
            forward,
        }) => cmd_watch(&command, &path, &forward),
        Some(Command::Repl) => repl::cmd_repl(),
        Some(Command::Lsp) => cmd_lsp(),
        Some(Command::Clean { vendor, dry_run }) => cmd_clean(vendor, dry_run),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_parse(file: &PathBuf) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !diags.is_empty() {
        for diag in &diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!("{} parse error(s)", diags.len()));
    }
    println!("{sf}");
    Ok(())
}

fn load_or_parse(
    source: &str,
    file_id: gossamer_lex::FileId,
    cache_key: &gossamer_driver::FrontendCacheKey,
    trace: bool,
) -> (
    gossamer_ast::SourceFile,
    Vec<gossamer_parse::ParseDiagnostic>,
) {
    if let Some(cached) = gossamer_driver::load_blob::<gossamer_ast::SourceFile>(cache_key) {
        if trace {
            eprintln!("cache: parse skipped for {}", cache_key.as_hex());
        }
        return (cached, Vec::new());
    }
    gossamer_parse::parse_source_file(source, file_id)
}

fn print_timings(
    source_len: usize,
    parse: std::time::Duration,
    resolve: std::time::Duration,
    typeck: std::time::Duration,
    exhaust: std::time::Duration,
) {
    let total = parse + resolve + typeck + exhaust;
    println!(
        "timings: {source_len} bytes source; parse {:>6.2}ms, resolve {:>6.2}ms, typeck {:>6.2}ms, exhaust {:>6.2}ms, total {:>6.2}ms",
        parse.as_secs_f64() * 1000.0,
        resolve.as_secs_f64() * 1000.0,
        typeck.as_secs_f64() * 1000.0,
        exhaust.as_secs_f64() * 1000.0,
        total.as_secs_f64() * 1000.0,
    );
}

fn cmd_check(file: &PathBuf, timings: bool) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let cache_key = gossamer_driver::FrontendCacheKey::new(&source, env!("CARGO_PKG_VERSION"));
    let trace = std::env::var_os("GOSSAMER_CACHE_TRACE").is_some();
    let stage_parse = std::time::Instant::now();
    let (sf, parse_diags) = load_or_parse(&source, file_id, &cache_key, trace);
    let parse_elapsed = stage_parse.elapsed();
    let render_opts = gossamer_diagnostics::RenderOptions::default();
    let mut total_errors = parse_diags.len();
    for diag in &parse_diags {
        let structured = diag.to_diagnostic();
        eprintln!(
            "parse: {}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let stage_resolve = std::time::Instant::now();
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    let resolve_elapsed = stage_resolve.elapsed();
    let unresolved: Vec<_> = resolve_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_resolve::ResolveError::UnresolvedName { .. }
                    | gossamer_resolve::ResolveError::DuplicateItem { .. }
            )
        })
        .collect();
    total_errors += unresolved.len();
    let in_scope: Vec<&str> = collect_top_level_names(&sf);
    for diag in unresolved {
        let structured = diag.to_diagnostic(&in_scope);
        eprintln!(
            "resolve: {}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let stage_typeck = std::time::Instant::now();
    let (table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    let typeck_elapsed = stage_typeck.elapsed();
    total_errors += type_diags.len();
    for diag in &type_diags {
        let structured = diag.to_diagnostic();
        eprintln!(
            "type: {}",
            gossamer_diagnostics::render(&structured, &map, render_opts)
        );
    }
    let stage_exhaust = std::time::Instant::now();
    let exhaustive_diags = gossamer_types::check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    let exhaust_elapsed = stage_exhaust.elapsed();
    let nonexhaustive: Vec<_> = exhaustive_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_types::ExhaustivenessError::NonExhaustive { .. }
            )
        })
        .collect();
    total_errors += nonexhaustive.len();
    for diag in nonexhaustive {
        eprintln!("match: {diag}");
    }
    if total_errors > 0 {
        return Err(anyhow!("check failed with {total_errors} diagnostic(s)"));
    }
    if trace && gossamer_driver::observe_hit(&cache_key) {
        eprintln!(
            "cache: frontend hit for {} (parse skipped)",
            cache_key.as_hex()
        );
    }
    gossamer_driver::mark_success(&cache_key);
    gossamer_driver::store_blob(&cache_key, &sf);
    println!("check: ok ({} items typed)", sf.items.len());
    if timings {
        print_timings(
            source.len(),
            parse_elapsed,
            resolve_elapsed,
            typeck_elapsed,
            exhaust_elapsed,
        );
    }
    Ok(())
}

fn collect_top_level_names(sf: &gossamer_ast::SourceFile) -> Vec<&str> {
    let mut out = Vec::new();
    for item in &sf.items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Struct(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Enum(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Trait(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::TypeAlias(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Const(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Static(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Mod(decl) => out.push(decl.name.name.as_str()),
            gossamer_ast::ItemKind::Impl(_) | gossamer_ast::ItemKind::AttrItem(_) => {}
        }
    }
    out
}

/// Parses, resolves, type-checks, and exhaustiveness-checks
/// `source`. Returns the lowered HIR program on success. When any
/// stage produces error-severity diagnostics, prints them through
/// the shared renderer and returns `Err` — no subsequent execution
/// may happen. Used by every `gos` subcommand that runs user code
/// so the interpreter, native build, test runner, and bench runner
/// all reject the same static-invalid programs.
fn load_and_check(
    source: &str,
    file_id: gossamer_lex::FileId,
    map: &gossamer_lex::SourceMap,
) -> Result<(gossamer_hir::HirProgram, gossamer_types::TyCtxt)> {
    load_and_check_with_sf(source, file_id, map).map(|(program, _, tcx)| (program, tcx))
}

/// Same as [`load_and_check`] but also returns the parsed
/// [`gossamer_ast::SourceFile`] for callers (`gos bench`, `gos test`)
/// that need AST-level item walks on top of the lowered program.
fn load_and_check_with_sf(
    source: &str,
    file_id: gossamer_lex::FileId,
    map: &gossamer_lex::SourceMap,
) -> Result<(
    gossamer_hir::HirProgram,
    gossamer_ast::SourceFile,
    gossamer_types::TyCtxt,
)> {
    let render_opts = gossamer_diagnostics::RenderOptions::default();
    let (sf, parse_diags) = gossamer_parse::parse_source_file(source, file_id);
    if !parse_diags.is_empty() {
        for diag in &parse_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "parse: {}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to execute",
            parse_diags.len()
        ));
    }
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    let in_scope: Vec<&str> = collect_top_level_names(&sf);
    let unresolved: Vec<_> = resolve_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_resolve::ResolveError::UnresolvedName { .. }
                    | gossamer_resolve::ResolveError::DuplicateItem { .. }
            )
        })
        .collect();
    if !unresolved.is_empty() {
        for diag in &unresolved {
            let structured = diag.to_diagnostic(&in_scope);
            eprintln!(
                "resolve: {}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} resolve error(s); refusing to execute",
            unresolved.len()
        ));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !type_diags.is_empty() {
        for diag in &type_diags {
            let structured = diag.to_diagnostic();
            eprintln!(
                "type: {}",
                gossamer_diagnostics::render(&structured, map, render_opts)
            );
        }
        return Err(anyhow!(
            "{} type error(s); refusing to execute",
            type_diags.len()
        ));
    }
    let exhaustive_diags = gossamer_types::check_exhaustiveness(&sf, &resolutions, &table, &tcx);
    let nonexhaustive: Vec<_> = exhaustive_diags
        .iter()
        .filter(|d| {
            matches!(
                d.error,
                gossamer_types::ExhaustivenessError::NonExhaustive { .. }
            )
        })
        .collect();
    if !nonexhaustive.is_empty() {
        for diag in nonexhaustive {
            eprintln!("match: {diag}");
        }
        return Err(anyhow!("non-exhaustive match; refusing to execute"));
    }
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let cache_key = gossamer_driver::FrontendCacheKey::new(source, env!("CARGO_PKG_VERSION"));
    if std::env::var_os("GOSSAMER_CACHE_TRACE").is_some()
        && gossamer_driver::observe_hit(&cache_key)
    {
        eprintln!(
            "cache: frontend hit for {} (skip not wired yet)",
            cache_key.as_hex()
        );
    }
    gossamer_driver::mark_success(&cache_key);
    Ok((program, sf, tcx))
}

/// How `gos run` executes a program.
#[derive(Clone, Copy, PartialEq, Eq)]
enum RunMode {
    /// Default: register-based bytecode VM. Silently falls back
    /// to the tree-walker when the VM compiler hits an HIR
    /// construct it doesn't yet lower (closures-with-late-binding,
    /// etc.).
    Vm,
    /// `--tree-walker`: force the tree-walker. Slower but covers
    /// every construct; useful for debugging the VM or chasing
    /// parity differences.
    TreeWalker,
}

fn cmd_run(file: &PathBuf, mode: RunMode, forwarded: &[String]) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    // Static checks always run first, regardless of execution
    // mode. A program with parse / resolve / type errors has no
    // business reaching the VM — execution would either crash
    // or produce unsound output.
    let (program, mut tcx) = load_and_check(&source, file_id, &map)?;
    gossamer_interp::set_program_args(forwarded);
    if mode == RunMode::TreeWalker {
        return run_tree_walker(&program);
    }
    // Default: VM with tree-walker fallback. Load failure usually
    // means the VM compiler refused an HIR shape; the tree-walker
    // covers the long tail.
    let mut vm = gossamer_interp::Vm::new();
    match vm.load(&program, &mut tcx) {
        Ok(()) => {
            let r = vm.call("main", Vec::new()).map(|_| ());
            // JIT-promoted bodies print through the runtime's
            // thread-local `STDOUT_BUF` rather than the bytecode
            // VM's writer. Drain the buffer so any output that
            // bypassed the bytecode path still reaches the user
            // before we exit.
            gossamer_interp::flush_runtime_stdout();
            match r {
                Ok(()) => Ok(()),
                Err(_) => {
                    // VM hit a runtime error (panic, type
                    // mismatch, etc.). The VM doesn't carry a
                    // call-stack trace today, so re-run via the
                    // tree-walker — same program, same outcome,
                    // but the tree-walker reports the function
                    // chain leading up to the failure (the
                    // diagnostic the user actually wants).
                    run_tree_walker(&program)
                }
            }
        }
        Err(err) => {
            if std::env::var("GOS_VM_TRACE").is_ok() {
                eprintln!("vm load failed ({err}); falling back to tree-walker");
            }
            run_tree_walker(&program)
        }
    }
}

fn run_tree_walker(program: &gossamer_hir::HirProgram) -> Result<()> {
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(program);
    let result = interp.call("main", Vec::new());
    gossamer_interp::join_outstanding_goroutines();
    if let Err(err) = result {
        let stack = interp.call_stack();
        let trace = if stack.is_empty() {
            String::new()
        } else {
            let mut rendered = String::from("\n  call stack (outermost first):");
            for name in &stack {
                rendered.push_str("\n    at ");
                rendered.push_str(name);
            }
            rendered
        };
        return Err(anyhow!("runtime error: {err}{trace}"));
    }
    Ok(())
}

fn cmd_build(file: &PathBuf, target: Option<&str>, release: bool) -> Result<()> {
    let source = read_source(file)?;

    // Validate source before attempting any codegen.  A broken AST or
    // unresolved name must fail the build immediately rather than
    // producing a segfaulting native binary or a launcher that
    // panics at runtime.
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !parse_diags.is_empty() {
        for diag in &parse_diags {
            eprintln!("parse: {diag}");
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to build",
            parse_diags.len()
        ));
    }
    let (resolutions, resolve_diags) = gossamer_resolve::resolve_source_file(&sf);
    if !resolve_diags.is_empty() {
        for diag in &resolve_diags {
            eprintln!("resolve: {diag}");
        }
        return Err(anyhow!(
            "{} resolve error(s); refusing to build",
            resolve_diags.len()
        ));
    }
    let mut tcx = gossamer_types::TyCtxt::new();
    let (_table, type_diags) = gossamer_types::typecheck_source_file(&sf, &resolutions, &mut tcx);
    if !type_diags.is_empty() {
        for diag in &type_diags {
            eprintln!("type: {diag}");
        }
        return Err(anyhow!(
            "{} type error(s); refusing to build",
            type_diags.len()
        ));
    }

    // Validate `--target` if explicitly provided. The Cranelift
    // happy-path uses the host ISA; non-host targets fall through
    // to the legacy artifact path (a deterministic byte stream
    // wrapping the rendered module). Reject unknown triples
    // early so the error is a clean parse failure, not a linker
    // blow-up.
    let target_options = match target {
        Some(triple) => Some(
            gossamer_driver::LinkerOptions::for_target(triple)
                .ok_or_else(|| anyhow!("unknown target `{triple}`"))?,
        ),
        None => None,
    };
    let unit_name = file.file_stem().and_then(|s| s.to_str()).unwrap_or("main");
    let out_path = resolve_output_path(file, unit_name)?;

    // Native (host) path: Cranelift / LLVM produce an object
    // and `cc` links it against the runtime. When `--target`
    // names a non-host triple we instead emit the
    // platform-agnostic Gossamer artifact byte stream — actual
    // cross-codegen is a follow-up milestone.
    if let Some(options) = target_options {
        let host = gossamer_driver::TargetTriple::host();
        if options.target.as_str() != host.as_str() {
            let artifact = gossamer_driver::compile_source(&source, unit_name, &options);
            fs::write(&out_path, &artifact.bytes)
                .map_err(|err| anyhow!("build: writing {}: {err}", out_path.display()))?;
            set_executable(&out_path)?;
            println!(
                "build: {bytes}B artifact at {path} (target {triple}, cross-link pending)",
                bytes = artifact.bytes.len(),
                path = out_path.display(),
                triple = options.target.as_str(),
            );
            return Ok(());
        }
    }
    let outcome = try_native_build(&source, unit_name, file, &out_path, release)
        .map_err(|err| anyhow!("build: {}", err.user_message()))?;
    println!(
        "build: {bytes}B native executable at {path} ({note})",
        bytes = outcome.size,
        path = out_path.display(),
        note = outcome.note,
    );
    Ok(())
}

struct NativeBuildOutcome {
    size: u64,
    note: String,
}

/// Why the native-build path bailed. Each variant carries a pre-
/// formatted one-line reason suitable for user output.
enum NativeBuildError {
    /// Cranelift/MIR couldn't lower some construct.
    LowerFailed(String),
    /// Host `cc` ran but returned non-zero.
    LinkerFailed(String),
    /// Host `cc` (or `$CC`) was not executable.
    LinkerMissing(String),
    /// Filesystem error writing the object file or output binary.
    Io(anyhow::Error),
}

impl NativeBuildError {
    fn user_message(&self) -> String {
        match self {
            Self::LowerFailed(reason) => {
                format!("native codegen cannot yet lower this program: {reason}")
            }
            Self::LinkerFailed(reason) => format!("linker failed: {reason}"),
            Self::LinkerMissing(reason) => format!("linker unavailable: {reason}"),
            Self::Io(err) => format!("filesystem error during build: {err:#}"),
        }
    }
}

/// Lowers `source` through the Cranelift backend and links the
/// resulting object file with the host `cc`. Each failure mode maps
/// to a distinct `NativeBuildError` variant so the caller can
/// report the specific cause and decide whether to fall back.
/// Locates `libgossamer_runtime.a` — the static library produced
/// by the `gossamer-runtime` crate with `crate-type =
/// ["staticlib", "rlib"]`. First tries `$GOS_RUNTIME_LIB`, then
/// walks up from the executable looking for `target/<profile>/`,
/// then finally from the manifest directory at build time.
fn find_runtime_lib() -> std::result::Result<PathBuf, NativeBuildError> {
    if let Ok(env) = std::env::var("GOS_RUNTIME_LIB") {
        let p = PathBuf::from(env);
        if p.exists() {
            return Ok(p);
        }
    }
    // Static-lib name varies by toolchain: GNU emits
    // `libgossamer_runtime.a`; MSVC emits `gossamer_runtime.lib`.
    let lib_names: &[&str] = if cfg!(target_env = "msvc") {
        &["gossamer_runtime.lib", "libgossamer_runtime.a"]
    } else {
        &["libgossamer_runtime.a", "gossamer_runtime.lib"]
    };
    let mut candidates: Vec<PathBuf> = Vec::new();
    // Compile-time path emitted by `build.rs`; this is the absolute
    // location into which the runtime staticlib was copied at
    // build-time. Highest priority because it survives any cwd /
    // exe-path quirk in CI.
    if let Some(baked) = option_env!("GOSSAMER_RUNTIME_LIB_PATH") {
        candidates.push(PathBuf::from(baked));
    }
    let mut push_with_names = |dir: &Path| {
        for name in lib_names {
            candidates.push(dir.join(name));
        }
    };
    // Walk up from the current executable, which lives under
    // `target/<profile>/gos` in development and at the OS install
    // prefix (`<prefix>/bin/gos`) post-`install.sh`.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() {
            // 1. Sibling of the binary — `target/<profile>/` in dev,
            //    `<prefix>/bin/` for an install.sh layout that staged
            //    the lib next to the binary.
            push_with_names(parent);
            // 2. `target/<profile>/deps/` (the `cargo test` exe lives
            //    here; the staticlib is one directory up).
            if let Some(grandparent) = parent.parent() {
                push_with_names(grandparent);
                // 3. Standard install layout: `<prefix>/lib/`.
                push_with_names(&grandparent.join("lib"));
            }
        }
    }
    // Workspace-root fallbacks (cargo's default target dir layout).
    push_with_names(Path::new("target/release"));
    push_with_names(Path::new("target/debug"));
    for c in &candidates {
        if c.exists() {
            return Ok(c.clone());
        }
    }
    Err(NativeBuildError::LinkerMissing(format!(
        "runtime static lib not found (tried both libgossamer_runtime.a \
         and gossamer_runtime.lib); set GOS_RUNTIME_LIB or run \
         `cargo build --release --package gossamer-runtime`. tried: {candidates:?}"
    )))
}

fn try_native_build(
    source: &str,
    unit_name: &str,
    input_path: &PathBuf,
    out_path: &PathBuf,
    release: bool,
) -> std::result::Result<NativeBuildOutcome, NativeBuildError> {
    // Default: Cranelift. `--release`: LLVM at `-O3` with a
    // graceful per-function fall-back to Cranelift for any
    // bodies the LLVM lowerer cannot cover yet. The two
    // objects are linked together so a partial-LLVM module
    // still gets the optimised path on the bodies it accepts.
    let tmp_dir =
        std::env::temp_dir().join(format!("gos-build-{}-{}", std::process::id(), unit_name));
    fs::create_dir_all(&tmp_dir)
        .map_err(|err| NativeBuildError::Io(anyhow!("creating {}: {err}", tmp_dir.display())))?;
    let (object_paths, object_triple) = emit_native_objects(source, unit_name, &tmp_dir, release)?;
    let runtime_lib = find_runtime_lib()?;
    let cc = std::env::var("CC").unwrap_or_else(|_| "cc".to_string());
    let mut cmd = std::process::Command::new(&cc);
    for p in &object_paths {
        cmd.arg(p);
    }
    cmd.arg(&runtime_lib)
        .arg("-o")
        .arg(out_path)
        .arg("-lpthread")
        .arg("-ldl")
        .arg("-lm");
    let status = cmd.status();
    let _ = fs::remove_dir_all(&tmp_dir);
    match status {
        Ok(status) if status.success() => {
            set_executable(out_path).map_err(NativeBuildError::Io)?;
            let size = fs::metadata(out_path).map_or(0, |m| m.len());
            let _ = input_path;
            Ok(NativeBuildOutcome {
                size,
                note: format!(
                    "target {triple}",
                    triple = object_triple.as_deref().unwrap_or("unknown"),
                ),
            })
        }
        Ok(status) => Err(NativeBuildError::LinkerFailed(format!(
            "{cc} exited with {status}"
        ))),
        Err(err) => Err(NativeBuildError::LinkerMissing(format!("{cc}: {err}"))),
    }
}

// Lowers `source` into one or two object files under `tmp_dir`, picking the
// codegen tier from `release`. Returns the object paths plus the recorded
// target triple for the linker step.
fn emit_native_objects(
    source: &str,
    unit_name: &str,
    tmp_dir: &Path,
    release: bool,
) -> std::result::Result<(Vec<PathBuf>, Option<String>), NativeBuildError> {
    let mut object_paths: Vec<PathBuf> = Vec::new();
    if !release {
        let object = gossamer_driver::compile_source_native(source, unit_name)
            .map_err(|err| NativeBuildError::LowerFailed(err.to_string()))?;
        let object_path = tmp_dir.join(format!("{unit_name}.o"));
        fs::write(&object_path, &object.bytes).map_err(|err| {
            NativeBuildError::Io(anyhow!("writing {}: {err}", object_path.display()))
        })?;
        let triple = Some(object.triple);
        object_paths.push(object_path);
        return Ok((object_paths, triple));
    }
    match gossamer_driver::compile_source_native_release_with_fallback(source, unit_name) {
        Ok(build) => {
            let llvm_path = tmp_dir.join(format!("{unit_name}.llvm.o"));
            fs::write(&llvm_path, &build.llvm.bytes).map_err(|err| {
                NativeBuildError::Io(anyhow!("writing {}: {err}", llvm_path.display()))
            })?;
            let triple = Some(build.llvm.triple.clone());
            object_paths.push(llvm_path);
            if let Some(cl) = build.cranelift {
                let cl_path = tmp_dir.join(format!("{unit_name}.cl.o"));
                fs::write(&cl_path, &cl.bytes).map_err(|err| {
                    NativeBuildError::Io(anyhow!("writing {}: {err}", cl_path.display()))
                })?;
                object_paths.push(cl_path);
                if std::env::var("GOS_LLVM_TRACE").is_ok() {
                    eprintln!(
                        "build: per-function fallback engaged for {n} bodies: {names:?}",
                        n = build.fallback_bodies.len(),
                        names = build.fallback_bodies,
                    );
                }
            }
            Ok((object_paths, triple))
        }
        Err(err) => {
            if std::env::var("GOS_LLVM_TRACE").is_ok() {
                eprintln!(
                    "build: LLVM path rejected `{unit_name}`: {err}; falling back to Cranelift"
                );
            }
            let object = gossamer_driver::compile_source_native(source, unit_name)
                .map_err(|e| NativeBuildError::LowerFailed(e.to_string()))?;
            let object_path = tmp_dir.join(format!("{unit_name}.o"));
            fs::write(&object_path, &object.bytes).map_err(|err| {
                NativeBuildError::Io(anyhow!("writing {}: {err}", object_path.display()))
            })?;
            let triple = Some(object.triple);
            object_paths.push(object_path);
            Ok((object_paths, triple))
        }
    }
}

fn set_executable(path: &PathBuf) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let meta = fs::metadata(path).with_context(|| format!("stat {}", path.display()))?;
        let mut perms = meta.permissions();
        perms.set_mode(perms.mode() | 0o111);
        fs::set_permissions(path, perms).with_context(|| format!("chmod +x {}", path.display()))?;
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
    Ok(())
}

/// Runs the Gossamer LSP server over stdio. Blocks until the client
/// sends `exit` (after `shutdown`) or closes stdin.
fn cmd_clean(vendor: bool, dry_run: bool) -> Result<()> {
    let mut removed_bytes: u64 = 0;
    let mut removed_files: u32 = 0;
    let cache = gossamer_driver::cache_dir();
    if cache.is_dir() {
        let bytes = dir_size(&cache);
        if dry_run {
            println!(
                "would remove frontend cache at {} ({bytes} bytes)",
                cache.display()
            );
        } else {
            fs::remove_dir_all(&cache).with_context(|| format!("remove {}", cache.display()))?;
            println!(
                "removed frontend cache at {} ({bytes} bytes)",
                cache.display()
            );
        }
        removed_bytes += bytes;
        removed_files += 1;
    } else {
        println!("frontend cache absent at {}", cache.display());
    }
    if vendor {
        let vendor_dir = std::env::current_dir()?.join("vendor");
        if vendor_dir.is_dir() {
            let bytes = dir_size(&vendor_dir);
            if dry_run {
                println!(
                    "would remove vendor tree at {} ({bytes} bytes)",
                    vendor_dir.display()
                );
            } else {
                fs::remove_dir_all(&vendor_dir)
                    .with_context(|| format!("remove {}", vendor_dir.display()))?;
                println!(
                    "removed vendor tree at {} ({bytes} bytes)",
                    vendor_dir.display()
                );
            }
            removed_bytes += bytes;
            removed_files += 1;
        } else {
            println!("vendor tree absent at {}", vendor_dir.display());
        }
    }
    let verb = if dry_run { "would remove" } else { "removed" };
    println!("clean: {verb} {removed_files} entr(y|ies), {removed_bytes} bytes total");
    Ok(())
}

/// Sums every regular file's byte length under `root`. Broken
/// symlinks and per-entry I/O errors are treated as 0 bytes — the
/// tally is advisory, never required for correctness.
fn dir_size(root: &std::path::Path) -> u64 {
    let mut total: u64 = 0;
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(meta) = entry.metadata() else { continue };
            if meta.is_dir() {
                stack.push(path);
            } else if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

fn cmd_lsp() -> Result<()> {
    gossamer_lsp::run_stdio().map_err(|e| anyhow!("lsp: {e}"))
}

/// Returns the path the REPL uses to persist line-edit history
/// across sessions. Prefers `$GOSSAMER_HISTORY` → `$XDG_STATE_HOME/
/// gossamer/history` → `$HOME/.gossamer_history`. `None` is returned
/// only when no reasonable home directory can be discovered, in
/// which case history is kept in-memory for the current session.
pub(crate) fn repl_history_path() -> Option<PathBuf> {
    if let Ok(explicit) = std::env::var("GOSSAMER_HISTORY") {
        return Some(PathBuf::from(explicit));
    }
    if let Ok(state) = std::env::var("XDG_STATE_HOME") {
        let mut path = PathBuf::from(state);
        path.push("gossamer");
        let _ = fs::create_dir_all(&path);
        path.push("history");
        return Some(path);
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut path = PathBuf::from(home);
        path.push(".gossamer_history");
        return Some(path);
    }
    None
}

/// Resolves the build output path.
///
/// Resolution order (first hit wins):
/// 1. `project.output` in the nearest enclosing `project.toml`
///    (relative paths resolve against the manifest's directory).
/// 2. `<source-dir>/<source-stem>` — the source stem with no
///    extension, next to the input file.
fn resolve_output_path(file: &PathBuf, unit_name: &str) -> Result<PathBuf> {
    if let Some(manifest_path) = gossamer_pkg::find_manifest(file) {
        let manifest_text = fs::read_to_string(&manifest_path)
            .with_context(|| format!("reading {}", manifest_path.display()))?;
        let manifest = gossamer_pkg::Manifest::parse(&manifest_text)
            .with_context(|| format!("parsing {}", manifest_path.display()))?;
        if let Some(output) = manifest.project.output {
            let raw = PathBuf::from(&output);
            let resolved = if raw.is_absolute() {
                raw
            } else {
                manifest_path
                    .parent()
                    .map_or_else(|| raw.clone(), |dir| dir.join(&raw))
            };
            return Ok(resolved);
        }
    }
    let parent = file.parent().filter(|p| !p.as_os_str().is_empty());
    Ok(match parent {
        Some(dir) => dir.join(unit_name),
        None => PathBuf::from(unit_name),
    })
}

pub(crate) fn read_source(file: &PathBuf) -> Result<String> {
    let resolved = resolve_gos_source(file);
    fs::read_to_string(&resolved).with_context(|| format!("reading {}", resolved.display()))
}

/// When `path` is a shell launcher script (starts with `#!`) or has no
/// `.gos` extension but `path.gos` exists, returns the `.gos` file.
/// This prevents `gos run examples/get_xkcd` from trying to parse the
/// launcher script generated by `gos build`.
fn resolve_gos_source(path: &PathBuf) -> PathBuf {
    if path.extension().and_then(|s| s.to_str()) != Some("gos") {
        let with_ext = path.with_extension("gos");
        if with_ext.exists() {
            return with_ext;
        }
    }
    if let Ok(text) = fs::read_to_string(path) {
        if text.starts_with("#!") {
            let with_ext = path.with_extension("gos");
            if with_ext.exists() {
                return with_ext;
            }
        }
    }
    path.clone()
}

fn cmd_init(id: &str) -> Result<()> {
    let project =
        gossamer_pkg::ProjectId::parse(id).with_context(|| format!("invalid id `{id}`"))?;
    let manifest_path = PathBuf::from("project.toml");
    if manifest_path.exists() {
        return Err(anyhow!("`project.toml` already exists"));
    }
    let manifest =
        gossamer_pkg::render_initial_manifest(&project, gossamer_pkg::Version::new(0, 1, 0));
    fs::write(&manifest_path, &manifest)
        .with_context(|| format!("writing {}", manifest_path.display()))?;
    println!("init: created project.toml for {project}");
    Ok(())
}

fn cmd_new(id: &str, path: Option<PathBuf>, template: &str) -> Result<()> {
    let project =
        gossamer_pkg::ProjectId::parse(id).with_context(|| format!("invalid id `{id}`"))?;
    let dir = path.unwrap_or_else(|| PathBuf::from(project.tail()));
    if dir.exists() {
        return Err(anyhow!("{} already exists", dir.display()));
    }
    let manifest =
        gossamer_pkg::render_initial_manifest(&project, gossamer_pkg::Version::new(0, 1, 0));
    match template {
        "bin" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(
                dir.join("src/main.gos"),
                gossamer_pkg::render_main_source(&project),
            )?;
        }
        "lib" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(dir.join("src/lib.gos"), lib_template_source(&project))?;
            fs::write(dir.join("src/lib_test.gos"), lib_template_test_source())?;
        }
        "service" => {
            fs::create_dir_all(dir.join("src"))
                .with_context(|| format!("creating {}", dir.display()))?;
            fs::write(dir.join("project.toml"), &manifest)?;
            fs::write(dir.join("src/main.gos"), service_template_source(&project))?;
        }
        "workspace" => {
            fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
            fs::write(
                dir.join("project.toml"),
                workspace_template_manifest(&project),
            )?;
            fs::write(dir.join("README.md"), workspace_template_readme(&project))?;
        }
        other => {
            return Err(anyhow!(
                "unknown template `{other}` — expected bin, lib, service, or workspace"
            ));
        }
    }
    println!(
        "new: scaffolded {} ({} template) at {}",
        project,
        template,
        dir.display()
    );
    Ok(())
}

/// Returns the seed `src/lib.gos` for `--template lib`.
///
/// Exports a single `greet(&str) -> String` function that the paired
/// `src/lib_test.gos` exercises as a smoke test. Consumers are
/// expected to replace this scaffolding before publishing.
fn lib_template_source(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "//! {project} — library crate.\n\
         //!\n\
         //! Replace this scaffolding with the real API before\n\
         //! publishing.\n\
         \n\
         /// Returns a greeting addressed to `name`.\n\
         pub fn greet(name: &str) -> String {{\n\
         \x20\x20\x20\x20\"hello, \".to_string() + name\n\
         }}\n",
    )
}

/// Returns the seed `src/main.gos` for `--template service`.
///
/// Wires an `http::Handler` that answers `/health` with a 200 text
/// response and every other path with a 404. Consumers replace the
/// match arms with their real routes.
fn service_template_source(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "//! {project} — HTTP service entry point.\n\
         //!\n\
         //! Listens on 0.0.0.0:8080 and answers `/health` with a 200.\n\
         //! Replace the match arms with your real routes before shipping.\n\
         \n\
         use std::http\n\
         \n\
         struct App {{ }}\n\
         \n\
         impl http::Handler for App {{\n\
         \x20\x20\x20\x20fn serve(&self, request: http::Request) -> Result<http::Response, http::Error> {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20match request.path() {{\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\"/health\" => Ok(http::Response::text(200, \"ok\".to_string())),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20\x20_ => Ok(http::Response::text(404, \"not found\".to_string())),\n\
         \x20\x20\x20\x20\x20\x20\x20\x20}}\n\
         \x20\x20\x20\x20}}\n\
         }}\n\
         \n\
         fn main() -> Result<(), http::Error> {{\n\
         \x20\x20\x20\x20let app = App {{ }}\n\
         \x20\x20\x20\x20println!(\"listening on 0.0.0.0:8080\")\n\
         \x20\x20\x20\x20http::serve(\"0.0.0.0:8080\".to_string(), app)\n\
         }}\n",
    )
}

/// Returns the seed test fixture for `--template lib`.
fn lib_template_test_source() -> String {
    "//! Smoke tests for the library crate.\n\
     \n\
     use std::testing\n\
     \n\
     #[test]\n\
     fn greet_includes_name() {\n\
     \x20\x20\x20\x20testing::check_eq(&greet(\"gossamer\"), &\"hello, gossamer\".to_string(), \"greet round-trips\").expect(\"mismatch\")\n\
     }\n"
        .to_string()
}

/// Returns the `project.toml` contents for `--template workspace`.
///
/// The manifest declares the project id and an empty
/// `[workspace.members]` table; consumers add members via
/// `gos new <child-id> --path members/<tail>` and register them
/// here. Keeps the root manifest minimal to avoid drift.
fn workspace_template_manifest(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "[package]\n\
         id = \"{project}\"\n\
         version = \"0.1.0\"\n\
         \n\
         [workspace]\n\
         members = []\n",
    )
}

/// Returns a README.md stub for `--template workspace`.
fn workspace_template_readme(project: &gossamer_pkg::ProjectId) -> String {
    format!(
        "# {project}\n\
         \n\
         A Gossamer workspace. Add members under `members/` and list\n\
         their ids under `[workspace.members]` in `project.toml`.\n",
    )
}

fn cmd_add(spec: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let (id_text, version_text) = match spec.split_once('@') {
        Some((id, ver)) => (id, ver),
        None => (spec, "0.1.0"),
    };
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let version = gossamer_pkg::Version::parse(version_text)
        .with_context(|| format!("invalid version `{version_text}`"))?;
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let changed = gossamer_pkg::add_registry(&mut m, &id, version);
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!(
        "add: {action} {id} ({version})",
        action = if changed { "added" } else { "kept" }
    );
    Ok(())
}

fn cmd_remove(id_text: &str, manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let id = gossamer_pkg::ProjectId::parse(id_text)
        .with_context(|| format!("invalid id `{id_text}`"))?;
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let mut m = gossamer_pkg::Manifest::parse(&source)?;
    let removed = gossamer_pkg::remove(&mut m, &id);
    if !removed {
        return Err(anyhow!("dependency {id} is not declared"));
    }
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("remove: dropped {id}");
    Ok(())
}

fn cmd_tidy(manifest: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    // Reparse + re-render canonicalises whitespace and entry ordering.
    let m = gossamer_pkg::Manifest::parse(&source)?;
    fs::write(&path, m.render()).with_context(|| format!("writing {}", path.display()))?;
    println!("tidy: canonicalised {}", path.display());
    Ok(())
}

fn cmd_fetch(manifest: Option<PathBuf>, offline: bool) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions { offline });
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    println!("fetch: {} project(s) cached", fetched.len());
    for entry in &fetched {
        println!("  {} → {}", entry.resolved.id, entry.source.digest);
    }
    Ok(())
}

fn cmd_vendor(manifest: Option<PathBuf>, out: Option<PathBuf>) -> Result<()> {
    let path = manifest.unwrap_or_else(|| PathBuf::from("project.toml"));
    let source =
        fs::read_to_string(&path).with_context(|| format!("reading {}", path.display()))?;
    let m = gossamer_pkg::Manifest::parse(&source)?;
    let resolver = gossamer_pkg::Resolver::new(gossamer_pkg::VersionCatalogue::new());
    let resolved = resolver
        .resolve(&m)
        .map_err(|e| anyhow!("resolve failed: {e}"))?;
    let mut cache = gossamer_pkg::Cache::new();
    let fetcher = gossamer_pkg::Fetcher::new(gossamer_pkg::FetchOptions::default());
    let fetched = fetcher
        .fetch_all(&resolved, &mut cache)
        .map_err(|e| anyhow!("fetch failed: {e}"))?;
    let dest = out.unwrap_or_else(|| PathBuf::from("vendor"));
    let written = gossamer_pkg::vendor(&fetched, &dest)
        .with_context(|| format!("writing vendor dir {}", dest.display()))?;
    let total: usize = written.values().map(Vec::len).sum();
    println!(
        "vendor: wrote {total} file(s) for {} project(s) to {}",
        written.len(),
        dest.display()
    );
    Ok(())
}

fn cmd_fmt(file: &PathBuf, check_only: bool) -> Result<()> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, diags) = gossamer_parse::parse_source_file(&source, file_id);
    if !diags.is_empty() {
        for diag in &diags {
            eprintln!("{diag}");
        }
        return Err(anyhow!(
            "{} parse error(s); refusing to format",
            diags.len()
        ));
    }
    let formatted = format!("{sf}");
    let formatted = if formatted.ends_with('\n') {
        formatted
    } else {
        formatted + "\n"
    };
    if check_only {
        if formatted == source {
            println!("fmt: {} already formatted", file.display());
            return Ok(());
        }
        return Err(anyhow!("{} is not formatted", file.display()));
    }
    if formatted == source {
        println!("fmt: {} unchanged", file.display());
    } else {
        fs::write(file, &formatted).with_context(|| format!("writing {}", file.display()))?;
        println!("fmt: rewrote {}", file.display());
    }
    Ok(())
}

fn item_has_attr(item: &gossamer_ast::Item, name: &str) -> bool {
    item.attrs.outer.iter().any(|a| {
        a.path
            .segments
            .last()
            .is_some_and(|seg| seg.name.name == name)
    })
}

/// Walks `items` in source order, including nested inline modules,
/// and appends the name of every `Fn` matched by `selector` to `out`.
/// `gos test` uses this to discover `#[test]`-annotated functions
/// that sit inside a `#[cfg(test)] mod tests { ... }` block.
fn collect_selected_fn_names(
    items: &[gossamer_ast::Item],
    selector: &impl Fn(&gossamer_ast::Item) -> bool,
    out: &mut Vec<String>,
) {
    for item in items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) if selector(item) => {
                out.push(decl.name.name.clone());
            }
            gossamer_ast::ItemKind::Mod(mod_decl) => {
                if let gossamer_ast::ModBody::Inline(inner) = &mod_decl.body {
                    collect_selected_fn_names(inner, selector, out);
                }
            }
            _ => {}
        }
    }
}

fn run_selected_fns(
    file: &PathBuf,
    selector: impl Fn(&gossamer_ast::Item) -> bool,
    iterations: u32,
) -> Result<(u32, u32, u128)> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (program, sf, _tcx) = load_and_check_with_sf(&source, file_id, &map)?;
    let mut selected: Vec<String> = Vec::new();
    collect_selected_fn_names(&sf.items, &selector, &mut selected);
    if selected.is_empty() {
        return Ok((0, 0, 0));
    }
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(&program);
    let mut passes = 0u32;
    let mut failures = 0u32;
    let mut total_nanos: u128 = 0;
    for name in &selected {
        for _ in 0..iterations {
            let started = std::time::Instant::now();
            match interp.call(name, Vec::new()) {
                Ok(_) => {
                    total_nanos += started.elapsed().as_nanos();
                    passes += 1;
                }
                Err(err) => {
                    eprintln!("  FAIL {name}: {err}");
                    failures += 1;
                    break;
                }
            }
        }
    }
    Ok((passes, failures, total_nanos))
}

fn cmd_test(path: Option<&Path>) -> Result<()> {
    gossamer_resolve::set_test_cfg(true);
    let resolved = match path {
        Some(p) => p.to_path_buf(),
        None => default_test_root()?,
    };
    let files = collect_lint_targets(&resolved)?;
    if files.is_empty() {
        return Err(anyhow!(
            "no `.gos` sources found under {}",
            resolved.display()
        ));
    }
    let mut total_passes = 0u32;
    let mut total_failures = 0u32;
    let mut total_assertions = 0u32;
    let mut total_doc_tests = 0u32;
    let mut empty_files = 0u32;
    for file in &files {
        if files.len() > 1 {
            println!("=== {} ===", file.display());
        }
        let summary = run_tests_in_file(file)?;
        let doc_summary = run_doc_tests_in_file(file);
        total_doc_tests += doc_summary.passes + doc_summary.failures;
        total_passes += doc_summary.passes;
        total_failures += doc_summary.failures;
        if summary.passes == 0
            && summary.failures == 0
            && doc_summary.passes == 0
            && doc_summary.failures == 0
        {
            empty_files += 1;
            if files.len() > 1 {
                println!("    no #[test] functions or doc-tests");
            } else {
                println!(
                    "test: no #[test] functions or doc-tests found in {}",
                    file.display()
                );
                return Ok(());
            }
            continue;
        }
        total_passes += summary.passes;
        total_failures += summary.failures;
        total_assertions += summary.assertions;
    }
    println!(
        "test: {passes} passed, {failures} failed, {assertions} assertion(s), {doc} doc-test(s), across {files} file(s), {empty} with no tests",
        passes = total_passes,
        failures = total_failures,
        assertions = total_assertions,
        doc = total_doc_tests,
        files = files.len(),
        empty = empty_files,
    );
    if total_failures > 0 {
        return Err(anyhow!("{total_failures} test failure(s)"));
    }
    Ok(())
}

/// Aggregate doc-test outcome for a single source file.
struct DocTestFileSummary {
    passes: u32,
    failures: u32,
}

/// Extracts fenced code blocks from `//` doc comments and runs each
/// as a standalone program. A block that compiles and executes
/// without panicking passes. Returns a summary; a parse or runtime
/// error counts as a failure but does not abort sibling files.
fn run_doc_tests_in_file(file: &std::path::Path) -> DocTestFileSummary {
    let Ok(source) = fs::read_to_string(file) else {
        return DocTestFileSummary {
            passes: 0,
            failures: 0,
        };
    };
    let tests = extract_doc_tests(&source, &file.display().to_string());
    let mut passes = 0u32;
    let mut failures = 0u32;
    for doc in &tests {
        let body = if doc.code.contains("fn main") {
            doc.code.clone()
        } else {
            format!("fn main() {{\n{}\n}}\n", doc.code)
        };
        let mut map = gossamer_lex::SourceMap::new();
        let file_id = map.add_file(doc.name.clone(), body.clone());
        let Ok((program, _tcx)) = load_and_check(&body, file_id, &map) else {
            println!("  FAIL doc-test {} (compile)", doc.name);
            failures += 1;
            continue;
        };
        let mut interp = gossamer_interp::Interpreter::new();
        interp.load(&program);
        match interp.call("main", Vec::new()) {
            Ok(_) => {
                println!("  PASS doc-test {}", doc.name);
                passes += 1;
            }
            Err(err) => {
                println!("  FAIL doc-test {} (runtime): {err}", doc.name);
                failures += 1;
            }
        }
    }
    DocTestFileSummary { passes, failures }
}

/// One fenced code block extracted from a `//` doc comment.
struct DocTest {
    /// Human-readable label: `<file>:<open-fence-line>`.
    name: String,
    /// Body of the fence, with `// ` prefixes stripped.
    code: String,
}

/// Extracts every fenced code block enclosed in consecutive `//`
/// doc-comment lines. A blank or non-comment line terminates the
/// enclosing block and drops any open fence. Recognised fence
/// markers: ```` ``` ```` (optionally followed by `gos`). Blocks
/// marked with a different language tag are skipped.
fn extract_doc_tests(source: &str, display: &str) -> Vec<DocTest> {
    let mut out = Vec::new();
    let mut fence: Option<(usize, Vec<String>, bool)> = None;
    for (idx, line) in source.lines().enumerate() {
        let trimmed = line.trim_start();
        let Some(rest) = trimmed.strip_prefix("//") else {
            fence = None;
            continue;
        };
        let body = rest.strip_prefix(' ').unwrap_or(rest);
        let leading = body.trim_start();
        if let Some(after_ticks) = leading.strip_prefix("```") {
            if let Some((open_line, captured, runnable)) = fence.take() {
                if runnable {
                    out.push(DocTest {
                        name: format!("{display}:{open_line}"),
                        code: captured.join("\n"),
                    });
                }
            } else {
                let tag = after_ticks.trim();
                let runnable = tag.is_empty() || tag == "gos" || tag == "gossamer";
                fence = Some((idx + 1, Vec::new(), runnable));
            }
        } else if let Some((_, captured, _)) = fence.as_mut() {
            captured.push(body.to_string());
        }
    }
    out
}

/// Aggregate test outcome for a single source file.
struct TestFileSummary {
    passes: u32,
    failures: u32,
    assertions: u32,
}

/// Runs every `#[test]`-annotated function in `file`, resetting the
/// interpreter's assertion tally between tests and rendering a
/// per-test PASS/FAIL line that includes assertion counts. A test
/// is considered failed when either the body panics or any
/// `testing::check*` call observed a failure during its execution.
fn run_tests_in_file(file: &PathBuf) -> Result<TestFileSummary> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (program, sf, _tcx) = load_and_check_with_sf(&source, file_id, &map)?;
    let mut test_names: Vec<String> = Vec::new();
    collect_selected_fn_names(
        &sf.items,
        &|item| item_has_attr(item, "test"),
        &mut test_names,
    );
    if test_names.is_empty() {
        return Ok(TestFileSummary {
            passes: 0,
            failures: 0,
            assertions: 0,
        });
    }
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(&program);
    let mut passes = 0u32;
    let mut failures = 0u32;
    let mut assertions = 0u32;
    for name in &test_names {
        gossamer_interp::reset_test_tally();
        let started = std::time::Instant::now();
        let outcome = interp.call(name, Vec::new());
        let elapsed = started.elapsed();
        let tally = gossamer_interp::take_test_tally();
        assertions += tally.assertions;
        let panicked = outcome.as_ref().err();
        let assertion_failure = tally.failures > 0;
        if panicked.is_none() && !assertion_failure {
            passes += 1;
            println!(
                "  PASS {name} ({} assertions, {}ms)",
                tally.assertions,
                elapsed.as_millis()
            );
        } else {
            failures += 1;
            let mut reason = String::new();
            if let Some(err) = panicked {
                reason.push_str(&format!("panic: {err}"));
            }
            if assertion_failure {
                if !reason.is_empty() {
                    reason.push_str(" · ");
                }
                reason.push_str(&format!("{} assertion(s) failed", tally.failures));
                if let Some(first) = tally.first_failure.as_ref() {
                    reason.push_str(" — ");
                    reason.push_str(first);
                }
            }
            println!("  FAIL {name} ({}ms): {reason}", elapsed.as_millis());
        }
    }
    Ok(TestFileSummary {
        passes,
        failures,
        assertions,
    })
}

/// Embedded Gossamer skill-card. The canonical source lives in
/// `docs_src/skill_card.md` (mkdocs input); embedding it directly
/// avoids depending on the generated `docs/` output.
const SKILL_CARD: &str = include_str!("../../../docs_src/skill_card.md");

fn cmd_skill_prompt() {
    print!("{SKILL_CARD}");
}

fn cmd_explain(code: &str) -> Result<()> {
    let upper = code.to_ascii_uppercase();
    // Built-in diagnostic codes first; fall back to the lint table.
    if let Some(text) = diagnostic_explanation(&upper) {
        println!("{upper}\n\n{text}");
        return Ok(());
    }
    // Lint codes are indirectly explained via their id. Turn a lint
    // code `GL####` into its registered identifier.
    if let Some(id) = lint_id_for_code(&upper) {
        if let Some(text) = gossamer_lint::lint_explanation(id) {
            println!("{upper} ({id})\n\n{text}");
            return Ok(());
        }
    }
    Err(anyhow!(
        "no explanation registered for `{upper}`. See docs/diagnostics.md for the code catalogue."
    ))
}

fn diagnostic_explanation(code: &str) -> Option<&'static str> {
    Some(match code {
        "GP0001" => {
            "The parser saw a token where it expected a different one.\n\
                     Check for missing punctuation, an unmatched delimiter, or an \n\
                     out-of-place keyword."
        }
        "GP0002" => {
            "The parser reached end-of-file in the middle of a construct.\n\
                     Finish the expression, statement, or item — or remove it."
        }
        "GP0003" => {
            "A balanced construct (block, tuple, array, string literal) was\n\
                     left unterminated. Add the matching closing delimiter."
        }
        "GP0004" => {
            "Comparison operators like `==` / `!=` / `<` are not associative.\n\
                     Parenthesise the operands: `(a == b) && (b == c)`."
        }
        "GR0001" => {
            "A name used in source could not be resolved to a declaration.\n\
                     Check the spelling, whether a `use` brings the name into scope,\n\
                     and whether the item is visible at this location."
        }
        "GR0003" => {
            "Two items in the same module share a name. Rename one of them\n\
                     or move it into a distinct `mod`."
        }
        "GT0001" => {
            "The type checker could not reconcile two types it expected to\n\
                     match. The primary label shows the location of the mismatch;\n\
                     the `note:` line names the conflicting types."
        }
        "GT0002" => {
            "The type checker could not find a method with the supplied\n\
                     name on the receiver type. Check for a typo, a missing `use`,\n\
                     or a trait impl that lives in an unreachable module."
        }
        "GT0004" => {
            "A `match` expression does not cover every possible value. Add\n\
                     an arm for the pattern(s) listed under `help:`."
        }
        "GT0005" => {
            "The `as` cast is restricted to a whitelist of conversions:\n\
                     numeric <-> numeric, `bool`/`char` -> integer, `u8` -> `char`,\n\
                     and same-type no-ops. Struct / enum / String sources are\n\
                     rejected. Use a conversion method when you need serialisation;\n\
                     `as` does not run code."
        }
        "GX0001" => {
            "A runtime value had the wrong shape for the operation. The\n\
                     interpreter catches this at execution time; the native\n\
                     backend aborts with the same code."
        }
        "GX0002" => {
            "A name resolved at parse/resolve time to nothing callable at\n\
                     runtime. Usually means a stdlib builtin is not wired into the\n\
                     execution path that reached the call."
        }
        "GX0003" => {
            "A call supplied the wrong number of arguments for the callee's\n\
                     declared arity. Fix the call site or update the declaration."
        }
        "GX0004" => {
            "An arithmetic operation overflowed, divided by zero, or produced\n\
                     a value outside the representable range."
        }
        "GX0005" => {
            "Explicit `panic!(...)` or an assertion failure aborted the\n\
                     program. Wrap the fallible operation in a `Result` path if the\n\
                     failure is recoverable."
        }
        "GX0006" => {
            "A `match` expression failed to match any arm at runtime. The\n\
                     exhaustiveness checker catches most of these statically; a\n\
                     `GX0006` at runtime means a refinement check slipped through."
        }
        "GX0007" => {
            "The execution path (interpreter or native) does not yet\n\
                     implement the construct reached. File the example and use\n\
                     the other path in the meantime."
        }
        _ => return None,
    })
}

fn lint_id_for_code(code: &str) -> Option<&'static str> {
    match code {
        "GL0001" => Some("unused_variable"),
        "GL0002" => Some("unused_import"),
        "GL0003" => Some("unused_mut_variable"),
        "GL0004" => Some("needless_return"),
        "GL0005" => Some("needless_bool"),
        "GL0006" => Some("comparison_to_bool_literal"),
        "GL0007" => Some("single_match"),
        "GL0008" => Some("shadowed_binding"),
        "GL0009" => Some("unchecked_result"),
        "GL0010" => Some("empty_block"),
        "GL0011" => Some("panic_in_main"),
        "GL0012" => Some("redundant_clone"),
        "GL0013" => Some("double_negation"),
        "GL0014" => Some("self_assignment"),
        "GL0015" => Some("todo_macro"),
        _ => None,
    }
}

fn cmd_watch(command: &str, path: &PathBuf, forward: &[String]) -> Result<()> {
    let targets = collect_lint_targets(path)?;
    if targets.is_empty() {
        return Err(anyhow!("no `.gos` files found under {}", path.display()));
    }
    eprintln!(
        "watch: running `gos {command} <file>` on change under {} ({} files)",
        path.display(),
        targets.len()
    );
    let mut signatures = snapshot_mtimes(&targets);
    run_watch_command(command, &targets, forward);
    loop {
        std::thread::sleep(std::time::Duration::from_millis(500));
        let next = snapshot_mtimes(&targets);
        if next != signatures {
            eprintln!("watch: change detected; re-running");
            run_watch_command(command, &targets, forward);
            signatures = next;
        }
    }
}

fn snapshot_mtimes(files: &[PathBuf]) -> Vec<(PathBuf, Option<std::time::SystemTime>)> {
    files
        .iter()
        .map(|path| {
            let mtime = fs::metadata(path).and_then(|m| m.modified()).ok();
            (path.clone(), mtime)
        })
        .collect()
}

fn run_watch_command(command: &str, targets: &[PathBuf], forward: &[String]) {
    let exe = std::env::current_exe().unwrap_or_else(|_| PathBuf::from("gos"));
    for target in targets {
        let mut child = std::process::Command::new(&exe);
        child.arg(command).arg(target);
        for arg in forward {
            child.arg(arg);
        }
        let status = child.status();
        match status {
            Ok(s) if s.success() => {}
            Ok(s) => eprintln!("watch: {} exited with {s}", target.display()),
            Err(err) => eprintln!("watch: spawn failed: {err}"),
        }
    }
}

fn cmd_lint(path: &PathBuf, deny_warnings: bool, explain: Option<&str>, fix: bool) -> Result<()> {
    if let Some(id) = explain {
        match gossamer_lint::lint_explanation(id) {
            Some(text) => {
                println!("lint `{id}`\n\n{text}");
                return Ok(());
            }
            None => return Err(anyhow!("no lint registered under `{id}`")),
        }
    }
    let files = collect_lint_targets(path)?;
    if files.is_empty() {
        return Err(anyhow!("no `.gos` sources found under {}", path.display()));
    }
    let mut warnings = 0usize;
    let mut errors = 0usize;
    let mut edits_applied = 0usize;
    let render_opts = gossamer_diagnostics::RenderOptions::default();
    for file in files {
        let source = read_source(&file)?;
        let mut map = gossamer_lex::SourceMap::new();
        let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
        let (sf, parse_diags) = gossamer_parse::parse_source_file(&source, file_id);
        if !parse_diags.is_empty() {
            for diag in &parse_diags {
                eprintln!("parse: {diag}");
            }
            errors += parse_diags.len();
            continue;
        }
        let mut registry = gossamer_lint::Registry::with_defaults();
        if deny_warnings {
            for (id, _) in registry.entries() {
                registry.set(id, gossamer_lint::Level::Deny);
            }
        }
        for item in &sf.items {
            gossamer_lint::apply_attributes(&item.attrs, &mut registry);
        }
        if fix {
            let candidate_fixes = gossamer_lint::fixes(&sf, &registry, &source);
            if !candidate_fixes.is_empty() {
                let rewritten = gossamer_lint::apply_fixes(&source, &candidate_fixes);
                fs::write(&file, &rewritten)
                    .with_context(|| format!("write {}", file.display()))?;
                edits_applied += candidate_fixes.len();
                println!(
                    "fix: {} edit(s) applied to {}",
                    candidate_fixes.len(),
                    file.display()
                );
            }
            continue;
        }
        let diagnostics = gossamer_lint::run(&sf, &registry);
        for diag in &diagnostics {
            eprintln!("{}", gossamer_diagnostics::render(diag, &map, render_opts));
            match diag.severity {
                gossamer_diagnostics::Severity::Error => errors += 1,
                gossamer_diagnostics::Severity::Warning => warnings += 1,
                _ => {}
            }
        }
    }
    if fix {
        println!("fix: {edits_applied} total edit(s) applied");
        return Ok(());
    }
    println!("lint: {warnings} warning(s), {errors} error(s)");
    if errors > 0 {
        return Err(anyhow!("{errors} lint error(s)"));
    }
    Ok(())
}

/// Returns the directory `gos test` walks when invoked with no
/// path argument: the `src/` directory of the nearest enclosing
/// `project.toml` if there is one, otherwise the current
/// directory. Mirrors `cargo test`'s "find the workspace and run
/// everything in it" reflex.
fn default_test_root() -> Result<PathBuf> {
    let cwd = std::env::current_dir().context("read current directory")?;
    let mut cursor: &Path = &cwd;
    loop {
        if cursor.join("project.toml").is_file() {
            let src = cursor.join("src");
            if src.is_dir() {
                return Ok(src);
            }
            return Ok(cursor.to_path_buf());
        }
        let Some(parent) = cursor.parent() else { break };
        cursor = parent;
    }
    Ok(cwd)
}

fn collect_lint_targets(root: &PathBuf) -> Result<Vec<PathBuf>> {
    let meta = fs::metadata(root).with_context(|| format!("stat {}", root.display()))?;
    if meta.is_file() {
        return Ok(vec![root.clone()]);
    }
    let mut out = Vec::new();
    let mut stack = vec![root.clone()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("read_dir {}", dir.display()))? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().and_then(|s| s.to_str()) == Some("gos") {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}

fn cmd_bench(file: &PathBuf, iterations: u32) -> Result<()> {
    let iters = iterations.max(1);
    let (runs, failures, total_nanos) =
        run_selected_fns(file, |item| item_has_attr(item, "bench"), iters)?;
    if runs == 0 && failures == 0 {
        println!("bench: no #[bench] functions found in {}", file.display());
        return Ok(());
    }
    if failures > 0 {
        return Err(anyhow!("{failures} bench function(s) panicked"));
    }
    let mean = if runs == 0 {
        0
    } else {
        total_nanos / u128::from(runs)
    };
    println!("bench: {runs} iterations across #[bench] functions; mean {mean} ns/iter");
    Ok(())
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
