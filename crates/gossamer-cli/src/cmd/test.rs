//! `gos test [PATH] [--run RX] [--parallel N] [--format junit]
//! [--junit-out FILE] [--race] [--coverage FILE]` - discovers and
//! runs every `#[test]`-annotated function under `PATH`, plus every
//! fenced doc-test it can extract from `///` comments.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::cmd::attr_walk::{collect_selected_fn_names, item_has_attr};
use crate::loaders::{load_and_check, load_and_check_with_sf};
use crate::paths::{collect_lint_targets, default_test_root, read_entry_source, read_source};

/// ANSI styling shared by the test-runner output. Disabled when
/// stdout isn't a TTY (CI captures, pipes), or when the user
/// explicitly opts out via `NO_COLOR=1`. See <https://no-color.org>.
#[derive(Clone)]
pub(crate) struct TestStyle {
    enabled: bool,
}

impl TestStyle {
    fn detect() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|v| !v.is_empty());
        let tty = std::io::IsTerminal::is_terminal(&std::io::stdout());
        Self {
            enabled: tty && !no_color,
        }
    }
    fn pass(&self) -> &'static str {
        if self.enabled {
            "\x1b[32mPASS\x1b[0m"
        } else {
            "PASS"
        }
    }
    fn fail(&self) -> &'static str {
        if self.enabled {
            "\x1b[31mFAIL\x1b[0m"
        } else {
            "FAIL"
        }
    }
    fn dim<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            format!("\x1b[2m{s}\x1b[0m").into()
        } else {
            s.into()
        }
    }
    fn bold<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            format!("\x1b[1m{s}\x1b[0m").into()
        } else {
            s.into()
        }
    }
    fn green<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            format!("\x1b[32m{s}\x1b[0m").into()
        } else {
            s.into()
        }
    }
    fn red<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            format!("\x1b[31m{s}\x1b[0m").into()
        } else {
            s.into()
        }
    }
    fn cyan<'a>(&self, s: &'a str) -> std::borrow::Cow<'a, str> {
        if self.enabled {
            format!("\x1b[36m{s}\x1b[0m").into()
        } else {
            s.into()
        }
    }
}

/// Options threaded into [`run_with_opts`].
pub(crate) struct TestOpts {
    pub path: Option<PathBuf>,
    pub run_filter: Option<String>,
    pub parallel: usize,
    pub format: String,
    pub junit_out: Option<PathBuf>,
    /// Enable the runtime data-race detector while running tests.
    pub race: bool,
    /// Optional lcov-format coverage output path.
    pub coverage: Option<PathBuf>,
    /// When true, run the cross-tier parity walk (VM + compiled)
    /// instead of the per-`#[test]` discovery flow. The walk
    /// targets every `.gos` file under `path` (defaults to
    /// `examples/` + `feature-testing-examples/`) and writes its
    /// per-tier outcome into a JSON sidecar driven by `report`.
    pub tier_parity: bool,
    /// Sidecar emission selector. `Some("status")` writes
    /// `target/debug/.feature-status.json` consumed by
    /// `gos feature-status`. Other values are reserved for future
    /// report shapes.
    pub report: Option<String>,
}

/// One test outcome, structured so `JUnit` XML and the human renderer
/// share the same data.
#[derive(Debug, Clone)]
struct TestRecord {
    file: String,
    name: String,
    passed: bool,
    elapsed_ms: u128,
    failure_message: Option<String>,
    assertions: u32,
}

/// Aggregate doc-test outcome for a single source file.
struct DocTestFileSummary {
    passes: u32,
    failures: u32,
}

/// One fenced code block extracted from a `//` doc comment.
struct DocTest {
    /// Human-readable label: `<file>:<open-fence-line>`.
    name: String,
    /// Body of the fence, with `// ` prefixes stripped.
    code: String,
}

#[allow(
    clippy::cognitive_complexity,
    clippy::too_many_lines,
    reason = "test runner orchestrates discovery, parallel exec, junit output, coverage end-to-end"
)]
pub(crate) fn run_with_opts(opts: TestOpts) -> Result<()> {
    if opts.tier_parity {
        return tier_parity::run(&opts);
    }
    gossamer_resolve::set_test_cfg(true);
    // Run tests on pure bytecode. The per-test assertion tally and the
    // coverage counters are interp-side mechanisms the whole-program
    // JIT bypasses (a JIT-promoted `#[test]` would record neither), so
    // a prior test warming up the JIT must not silently drop a later
    // test's assertions. Determinism over throughput here.
    gossamer_interp::set_jit_disabled();
    if opts.race {
        gossamer_runtime::race::enable();
        gossamer_codegen_llvm::set_race_instrumentation(true);
    }
    if opts.coverage.is_some() {
        gossamer_runtime::coverage::set_enabled(true);
        gossamer_runtime::coverage::reset();
    }
    let resolved = match opts.path.as_ref() {
        Some(p) => p.clone(),
        None => default_test_root()?,
    };
    let style = TestStyle::detect();
    let files = collect_lint_targets(&resolved)?;
    if files.is_empty() {
        return Err(anyhow!(
            "no `.gos` sources found under {}",
            resolved.display()
        ));
    }
    let want_junit = opts.format == "junit";
    let filter = if let Some(pat) = opts.run_filter.as_deref() {
        Some(regex::Regex::new(pat).map_err(|e| anyhow!("invalid --run regex `{pat}`: {e}"))?)
    } else {
        None
    };

    let mut discovered: Vec<(PathBuf, String)> = Vec::new();
    let mut load_errors: Vec<String> = Vec::new();
    for file in &files {
        let names = match collect_test_names(file) {
            Ok(names) => names,
            Err(err) => {
                // The diagnostic itself was already streamed to
                // stderr by `load_and_check_with_sf`; surface the
                // accompanying anyhow trailer ("N … error(s);
                // refusing to execute") so the user sees the
                // refusal explicitly.
                eprintln!("error: {err}");
                load_errors.push(format!("{}: {err}", file.display()));
                continue;
            }
        };
        for name in names {
            if let Some(re) = filter.as_ref() {
                if !re.is_match(&name) {
                    continue;
                }
            }
            discovered.push((file.clone(), name));
        }
    }

    let mut records: Vec<TestRecord> = Vec::new();
    let mut total_doc_passes = 0u32;
    let mut total_doc_failures = 0u32;

    let by_file: std::collections::BTreeMap<PathBuf, Vec<String>> = {
        let mut map: std::collections::BTreeMap<PathBuf, Vec<String>> =
            std::collections::BTreeMap::new();
        for (f, n) in discovered {
            map.entry(f).or_default().push(n);
        }
        map
    };

    let parallel = opts.parallel.max(1);
    let by_file_vec: Vec<(PathBuf, Vec<String>)> = by_file.into_iter().collect();
    let collected: Vec<(PathBuf, Vec<TestRecord>)> = if parallel > 1 && by_file_vec.len() > 1 {
        run_files_parallel(&by_file_vec, parallel, &style, want_junit)
    } else {
        by_file_vec
            .iter()
            .map(|(file, names)| {
                let recs = run_tests_filtered(file, names, &style, want_junit);
                (file.clone(), recs)
            })
            .collect()
    };
    for (_, recs) in collected {
        records.extend(recs);
    }
    for file in &files {
        let doc_summary = run_doc_tests_in_file(file, &style);
        total_doc_passes += doc_summary.passes;
        total_doc_failures += doc_summary.failures;
    }

    let total_passes =
        u32::try_from(records.iter().filter(|r| r.passed).count()).unwrap_or(0) + total_doc_passes;
    let total_failures = u32::try_from(records.iter().filter(|r| !r.passed).count()).unwrap_or(0)
        + total_doc_failures;
    let total_assertions: u32 = records.iter().map(|r| r.assertions).sum();
    let total_doc_tests = total_doc_passes + total_doc_failures;
    let empty_files = u32::try_from(
        files
            .iter()
            .filter(|f| !records.iter().any(|r| r.file == f.to_string_lossy()))
            .count(),
    )
    .unwrap_or(0);

    if want_junit {
        let xml = render_junit(&records);
        if let Some(out) = opts.junit_out.as_ref() {
            std::fs::write(out, &xml)
                .map_err(|e| anyhow!("write junit xml to {}: {e}", out.display()))?;
        } else {
            print!("{xml}");
        }
    } else {
        if total_passes == 0
            && total_failures == 0
            && total_doc_tests == 0
            && load_errors.is_empty()
        {
            // Help users distinguish "all tests passed" (which
            // can also be 0/0 when nothing matched a `--run`
            // filter) from "the file genuinely has nothing
            // marked `#[test]`".
            println!(
                "test: no #[test] functions found under {}",
                resolved.display()
            );
        }
        let pass_part = format!("{total_passes} passed");
        let fail_part = format!("{total_failures} failed");
        let pass_styled = if total_failures == 0 {
            style.green(&style.bold(&pass_part)).into_owned()
        } else {
            style.green(&pass_part).into_owned()
        };
        let fail_styled = if total_failures > 0 {
            style.red(&style.bold(&fail_part)).into_owned()
        } else {
            style.dim(&fail_part).into_owned()
        };
        let trailing = format!(
            "{total_assertions} assertion(s), {total_doc_tests} doc-test(s), across {} file(s), {empty_files} with no tests",
            files.len()
        );
        println!(
            "test: {pass_styled}, {fail_styled}, {}",
            style.dim(&trailing)
        );
    }
    if total_failures > 0 {
        return Err(anyhow!("{total_failures} test failure(s)"));
    }
    if !load_errors.is_empty() {
        // A file the user pointed at refused to parse / resolve /
        // typecheck. Bubble up so the harness exits non-zero -
        // running tests against statically-broken source is worse
        // than reporting nothing.
        let summary = if load_errors.len() == 1 {
            "1 file failed to load".to_string()
        } else {
            format!("{} files failed to load", load_errors.len())
        };
        return Err(anyhow!("{summary}"));
    }
    if opts.race {
        let races = gossamer_runtime::race::drain_races();
        if !races.is_empty() {
            for race in &races {
                eprintln!("{race}");
            }
            return Err(anyhow!(
                "{} data race(s) detected (see --race output above)",
                races.len()
            ));
        }
    }
    if let Some(out) = opts.coverage.as_ref() {
        let lcov = render_lcov(&records, &files);
        std::fs::write(out, lcov)
            .map_err(|e| anyhow!("write coverage to {}: {e}", out.display()))?;
    }
    Ok(())
}

/// Renders an lcov report from the runtime coverage counter
/// snapshot plus the per-test-record function-level summary.
///
/// 0.8.0: real per-line + per-branch counters land via
/// `gos_rt_cov_record` calls emitted by codegen / the interp at
/// every statement boundary. The previous synthetic per-function
/// shape is preserved as the `FN:`/`FNF:`/`FNH:` fallback for
/// files that ran no instrumented code (header-only, doc-only,
/// no executable line). Format conforms to the lcov spec so
/// `genhtml`, `lcov-summary`, and CI dashboards parse it
/// directly.
fn render_lcov(records: &[TestRecord], files: &[PathBuf]) -> String {
    use std::collections::BTreeMap;

    let snapshot = gossamer_runtime::coverage::snapshot();

    let mut lines_by_file: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    let mut branches_by_file: BTreeMap<String, BTreeMap<(u32, u32), u64>> = BTreeMap::new();
    for c in &snapshot {
        if c.branch == 0 {
            *lines_by_file
                .entry(c.file.clone())
                .or_default()
                .entry(c.line)
                .or_insert(0) += c.hits;
        } else {
            *branches_by_file
                .entry(c.file.clone())
                .or_default()
                .entry((c.line, c.branch))
                .or_insert(0) += c.hits;
        }
    }

    let mut by_file: BTreeMap<&str, (u32, u32)> = BTreeMap::new();
    for record in records {
        let entry = by_file.entry(record.file.as_str()).or_insert((0, 0));
        if record.passed {
            entry.0 += 1;
        }
        entry.1 += 1;
    }

    let mut out = String::new();
    for file in files {
        let path = file.to_string_lossy();
        let (fns_hit, fns_total) = by_file.get(path.as_ref()).copied().unwrap_or((0, 0));
        out.push_str("TN:\n");
        out.push_str(&format!("SF:{path}\n"));
        out.push_str(&format!("FNF:{fns_total}\n"));
        out.push_str(&format!("FNH:{fns_hit}\n"));
        if let Some(lines) = lines_by_file.get(path.as_ref()) {
            let mut lh = 0u32;
            for (&line, &hits) in lines {
                out.push_str(&format!("DA:{line},{hits}\n"));
                if hits > 0 {
                    lh += 1;
                }
            }
            out.push_str(&format!("LF:{}\n", lines.len()));
            out.push_str(&format!("LH:{lh}\n"));
        }
        if let Some(branches) = branches_by_file.get(path.as_ref()) {
            let mut brh = 0u32;
            for (&(line, branch), &hits) in branches {
                out.push_str(&format!("BRDA:{line},0,{branch},{hits}\n"));
                if hits > 0 {
                    brh += 1;
                }
            }
            out.push_str(&format!("BRF:{}\n", branches.len()));
            out.push_str(&format!("BRH:{brh}\n"));
        }
        out.push_str("end_of_record\n");
    }
    out
}

fn collect_test_names(file: &Path) -> Result<Vec<String>> {
    let source = read_source(&file.to_path_buf())?;
    // Discover this file's own `#[test]` names from its syntax alone. A full
    // resolve + typecheck of the file in isolation would fail on any file that
    // names a sibling module's item by bare name - valid only against the
    // bundled whole-package source, not one file alone - so parse for the
    // names. The synthesized serde / derive impls carry no `#[test]`, so the
    // raw source suffices for discovery.
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, _diags) = gossamer_parse::parse_source_file(&source, file_id);
    let mut names = Vec::new();
    collect_selected_fn_names(&sf.items, &|item| item_has_attr(item, "test"), &mut names);
    if names.is_empty() {
        return Ok(names);
    }
    // The file carries tests: refuse to run any if the program does not
    // statically resolve and typecheck (a harness over broken code is worse
    // than useless). Validate the bundled whole-package source - the same
    // augment + comptime-fold + check the execution path uses - so cross-file
    // references stay valid and a real static error is surfaced with its
    // "refusing to execute" trailer rather than swallowed.
    let entry = read_entry_source(&file.to_path_buf())?;
    let augmented = gossamer_parse::autoderive::augment_source(&entry);
    let augmented = if augmented.contains("comptime") {
        match crate::comptime_fold::fold_comptime(augmented.clone(), &file.to_string_lossy()) {
            Ok(folded) => folded,
            Err(_) => augmented,
        }
    } else {
        augmented
    };
    let mut check_map = gossamer_lex::SourceMap::new();
    let check_id = check_map.add_file(file.to_string_lossy().into_owned(), augmented.clone());
    let _ = load_and_check_with_sf(&augmented, check_id, &check_map)?;
    Ok(names)
}

fn run_tests_filtered(
    file: &Path,
    names: &[String],
    style: &TestStyle,
    quiet: bool,
) -> Vec<TestRecord> {
    // Execute on a thread with a large native stack so a deeply
    // recursive `#[test]` does not overflow the host's default
    // main-thread stack (see `cmd::with_vm_stack`). Covers both the
    // serial and the parallel-worker call sites.
    let file = file.to_path_buf();
    let names = names.to_vec();
    let style = style.clone();
    crate::cmd::with_vm_stack(move || run_tests_filtered_inner(&file, &names, &style, quiet))
}

fn run_tests_filtered_inner(
    file: &Path,
    names: &[String],
    style: &TestStyle,
    quiet: bool,
) -> Vec<TestRecord> {
    // Bundle sibling `*.gos` modules into the compilation, exactly as
    // `gos run` / `gos build` do, so a `#[test]` can reach a sibling
    // module (`super::helper::triple` where `src/helper.gos` is declared
    // `mod helper;`). Test-name collection stays unbundled so sibling
    // tests are not double-counted against this file.
    let Ok(source) = read_entry_source(&file.to_path_buf()) else {
        return Vec::new();
    };
    let augmented = gossamer_parse::autoderive::augment_source(&source);
    // Comptime fold so a `#[test]` compiles the same constant the
    // run / build tiers do. On a comptime failure, fall back to the
    // unfolded source - the VM still evaluates the region at runtime,
    // and `gos check` / `gos build` surface the error authoritatively.
    let augmented = if augmented.contains("comptime") {
        match crate::comptime_fold::fold_comptime(augmented.clone(), &file.to_string_lossy()) {
            Ok(folded) => folded,
            Err(_) => augmented,
        }
    } else {
        augmented
    };
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), augmented.clone());
    let Ok((program, _sf, tcx)) = load_and_check_with_sf(&augmented, file_id, &map) else {
        return Vec::new();
    };
    let mut vm = gossamer_interp::Vm::new();
    // Publish the source map before `load` so the VM compiler can emit
    // per-statement coverage hits when `gos test --coverage` is active.
    vm.set_source_map(std::sync::Arc::new(map));
    if vm.load(&program, tcx, false).is_err() {
        return Vec::new();
    }
    let mut records = Vec::new();
    if !quiet && !names.is_empty() {
        let header = format!("=== {} ===", file.display());
        println!("{}", style.cyan(&header));
    }
    for name in names {
        gossamer_interp::reset_test_tally();
        let started = std::time::Instant::now();
        let outcome = vm.call(name, Vec::new());
        let elapsed = started.elapsed();
        // Snapshot the VM call chain immediately: a failing test
        // preserves its frames, and the next `vm.call` clears them.
        let call_trace = if outcome.is_err() {
            crate::cmd::traceback::render_call_stack(&vm.call_stack_snapshot())
        } else {
            String::new()
        };
        let tally = gossamer_interp::take_test_tally();
        let panicked = outcome.as_ref().err().map(ToString::to_string);
        let assertion_failure = tally.failures > 0;
        let passed = panicked.is_none() && !assertion_failure;
        let mut failure_message: Option<String> = None;
        if !passed {
            let mut reason = String::new();
            if let Some(err) = panicked.as_deref() {
                reason.push_str(&format!("panic: {err}"));
            }
            if assertion_failure {
                if !reason.is_empty() {
                    reason.push_str(" · ");
                }
                reason.push_str(&format!("{} assertion(s) failed", tally.failures));
                if let Some(first) = tally.first_failure.as_ref() {
                    reason.push_str(" - ");
                    reason.push_str(first);
                }
            }
            failure_message = Some(reason);
        }
        records.push(TestRecord {
            file: file.to_string_lossy().into_owned(),
            name: name.clone(),
            passed,
            elapsed_ms: elapsed.as_millis(),
            failure_message: failure_message.clone(),
            assertions: tally.assertions,
        });
        if !quiet {
            if passed {
                let stats = format!(
                    "({} {asserts}, {}ms)",
                    tally.assertions,
                    elapsed.as_millis(),
                    asserts = if tally.assertions == 1 {
                        "assertion"
                    } else {
                        "assertions"
                    },
                );
                println!("  {} {name} {}", style.pass(), style.dim(&stats));
            } else {
                let elapsed_str = format!("({}ms)", elapsed.as_millis());
                println!(
                    "  {} {name} {}: {}",
                    style.fail(),
                    style.dim(&elapsed_str),
                    style.red(&failure_message.clone().unwrap_or_default())
                );
                // The call-chain traceback is additional context - the
                // failure message above stays byte-identical to before.
                if !call_trace.is_empty() {
                    println!("{}", style.dim(&call_trace));
                }
            }
        }
    }
    records
}

type FileQueue = std::sync::Arc<parking_lot::Mutex<Vec<(PathBuf, Vec<String>)>>>;
type FileResults = std::sync::Arc<parking_lot::Mutex<Vec<(PathBuf, Vec<TestRecord>)>>>;

fn run_files_parallel(
    by_file: &[(PathBuf, Vec<String>)],
    parallel: usize,
    style: &TestStyle,
    quiet: bool,
) -> Vec<(PathBuf, Vec<TestRecord>)> {
    use std::sync::Arc;

    use parking_lot::Mutex as PlMutex;
    let queue: FileQueue = Arc::new(PlMutex::new(by_file.to_vec()));
    let results: FileResults = Arc::new(PlMutex::new(Vec::new()));
    let n_workers = parallel.min(by_file.len()).max(1);
    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let style_owned: TestStyle = style.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                let next = {
                    let mut q = queue.lock();
                    q.pop()
                };
                let Some((file, names)) = next else {
                    return;
                };
                let recs = run_tests_filtered(&file, &names, &style_owned, quiet);
                results.lock().push((file, recs));
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let mut out = Arc::try_unwrap(results)
        .expect("results arc unwrap")
        .into_inner();
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn render_junit(records: &[TestRecord]) -> String {
    use std::collections::BTreeMap;
    let mut suites: BTreeMap<&str, Vec<&TestRecord>> = BTreeMap::new();
    for record in records {
        suites.entry(record.file.as_str()).or_default().push(record);
    }
    let mut out = String::new();
    out.push_str("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    let total_tests = records.len();
    let total_failures = records.iter().filter(|r| !r.passed).count();
    out.push_str(&format!(
        "<testsuites tests=\"{total_tests}\" failures=\"{total_failures}\">\n"
    ));
    for (suite, tests) in &suites {
        let n = tests.len();
        let failures = tests.iter().filter(|r| !r.passed).count();
        let total_ms: u128 = tests.iter().map(|r| r.elapsed_ms).sum();
        let seconds = (total_ms as f64) / 1000.0;
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{n}\" failures=\"{failures}\" time=\"{seconds:.3}\">\n",
            xml_escape(suite)
        ));
        for record in tests {
            let elapsed_s = (record.elapsed_ms as f64) / 1000.0;
            out.push_str(&format!(
                "    <testcase classname=\"{cls}\" name=\"{name}\" time=\"{elapsed_s:.3}\"",
                cls = xml_escape(suite),
                name = xml_escape(&record.name),
            ));
            if record.passed {
                out.push_str("/>\n");
            } else {
                out.push_str(">\n      <failure message=\"");
                out.push_str(&xml_escape(
                    record.failure_message.as_deref().unwrap_or("failed"),
                ));
                out.push_str("\"/>\n    </testcase>\n");
            }
        }
        out.push_str("  </testsuite>\n");
    }
    out.push_str("</testsuites>\n");
    out
}

fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

/// Extracts fenced code blocks from `//` doc comments and runs each
/// as a standalone program. A block that compiles and executes
/// without panicking passes. Returns a summary; a parse or runtime
/// error counts as a failure but does not abort sibling files.
fn run_doc_tests_in_file(file: &std::path::Path, style: &TestStyle) -> DocTestFileSummary {
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
        let Ok((program, tcx)) = load_and_check(&body, file_id, &map) else {
            println!("  {} doc-test {} (compile)", style.fail(), doc.name);
            failures += 1;
            continue;
        };
        let mut vm = gossamer_interp::Vm::new();
        vm.set_source_map(std::sync::Arc::new(map));
        if vm.load(&program, tcx, false).is_err() {
            println!("  {} doc-test {} (compile)", style.fail(), doc.name);
            failures += 1;
            continue;
        }
        match vm.call("main", Vec::new()) {
            Ok(_) => {
                println!("  {} doc-test {}", style.pass(), doc.name);
                passes += 1;
            }
            Err(err) => {
                println!("  {} doc-test {} (runtime): {err}", style.fail(), doc.name);
                failures += 1;
            }
        }
    }
    DocTestFileSummary { passes, failures }
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

/// Cross-tier walk surface - runs every example through the VM and
/// the LLVM-compiled binary, capturing per-tier outcomes and
/// (optionally) writing the JSON sidecar that
/// `gos feature-status` reads.
mod tier_parity {
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::process::{Command, Stdio};
    use std::time::Duration;

    use anyhow::{Result, anyhow};

    use super::TestOpts;
    use crate::cmd::feature_status::{TierStatus, render_sidecar};

    pub(super) fn run(opts: &TestOpts) -> Result<()> {
        let roots = match opts.path.as_ref() {
            Some(p) => vec![p.clone()],
            None => default_walk_roots(),
        };
        let mut files: Vec<PathBuf> = Vec::new();
        for root in &roots {
            collect_gos(root, &mut files)?;
        }
        files.sort();
        if files.is_empty() {
            return Err(anyhow!(
                "no `.gos` sources found under {}",
                roots
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", "),
            ));
        }
        let mut records: Vec<(String, TierStatus)> = Vec::with_capacity(files.len());
        for file in &files {
            let name = file
                .strip_prefix(workspace_root_or_cwd().unwrap_or_else(|| PathBuf::from(".")))
                .unwrap_or(file)
                .display()
                .to_string();
            let vm = tier_outcome(run_vm(file));
            let llvm = tier_outcome(run_llvm(file));
            // Cranelift is the in-process JIT under `gos run`; tier
            // outcome maps to the VM outcome until a separate
            // dispatch lands.
            let cranelift = vm.clone();
            println!(
                "{name}: vm={} cranelift={} llvm={}",
                vm.as_deref().unwrap_or("-"),
                cranelift.as_deref().unwrap_or("-"),
                llvm.as_deref().unwrap_or("-"),
            );
            records.push((
                name,
                TierStatus {
                    vm,
                    cranelift,
                    llvm,
                },
            ));
        }
        let json = render_sidecar(&records);
        if opts.report.as_deref() == Some("status") {
            let out_path = sidecar_path();
            if let Some(parent) = out_path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            fs::write(&out_path, &json)
                .map_err(|e| anyhow!("writing sidecar {}: {e}", out_path.display()))?;
            println!(
                "feature-status sidecar written to {} ({} records)",
                out_path.display(),
                records.len(),
            );
        } else if opts.report.is_some() {
            return Err(anyhow!("unknown --report value (only `status` supported)"));
        }
        let failed: Vec<&(String, TierStatus)> =
            records.iter().filter(|(_, s)| !s.all_pass()).collect();
        if failed.is_empty() {
            Ok(())
        } else {
            Err(anyhow!(
                "{} tier-parity failure(s) - see sidecar for details",
                failed.len(),
            ))
        }
    }

    fn default_walk_roots() -> Vec<PathBuf> {
        let root = workspace_root_or_cwd().unwrap_or_else(|| PathBuf::from("."));
        vec![root.join("examples"), root.join("feature-testing-examples")]
            .into_iter()
            .filter(|p| p.is_dir())
            .collect()
    }

    fn workspace_root_or_cwd() -> Option<PathBuf> {
        let mut cur = std::env::current_dir().ok()?;
        loop {
            if cur.join("Cargo.toml").exists() && cur.join("crates").is_dir() {
                return Some(cur);
            }
            if !cur.pop() {
                return None;
            }
        }
    }

    fn sidecar_path() -> PathBuf {
        let base = std::env::var_os("CARGO_TARGET_DIR").map_or_else(
            || {
                workspace_root_or_cwd()
                    .map_or_else(|| PathBuf::from("target"), |r| r.join("target"))
            },
            PathBuf::from,
        );
        base.join("debug").join(".feature-status.json")
    }

    fn collect_gos(root: &Path, out: &mut Vec<PathBuf>) -> Result<()> {
        if root.is_file() {
            if root.extension().and_then(|e| e.to_str()) == Some("gos") {
                out.push(root.to_path_buf());
            }
            return Ok(());
        }
        if !root.is_dir() {
            return Ok(());
        }
        for entry in fs::read_dir(root).map_err(|e| anyhow!("read_dir {}: {e}", root.display()))? {
            let entry = entry?;
            collect_gos(&entry.path(), out)?;
        }
        Ok(())
    }

    fn tier_outcome(outcome: Outcome) -> Option<String> {
        match outcome {
            Outcome::Pass => Some("pass".into()),
            Outcome::Fail => Some("fail".into()),
            Outcome::Skipped => None,
        }
    }

    #[derive(Debug)]
    enum Outcome {
        Pass,
        Fail,
        Skipped,
    }

    fn gos_bin() -> PathBuf {
        std::env::current_exe()
            .ok()
            .unwrap_or_else(|| PathBuf::from("gos"))
    }

    fn run_vm(file: &Path) -> Outcome {
        let result = Command::new(gos_bin())
            .arg("run")
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn();
        let Ok(child) = result else {
            return Outcome::Skipped;
        };
        wait_bounded(child, Duration::from_mins(1))
    }

    fn run_llvm(file: &Path) -> Outcome {
        let scratch = std::env::temp_dir().join(format!(
            "gos-tier-parity-{}-{}",
            std::process::id(),
            file.file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("unknown"),
        ));
        let _ = fs::remove_dir_all(&scratch);
        if fs::create_dir_all(&scratch).is_err() {
            return Outcome::Skipped;
        }
        let build = Command::new(gos_bin())
            .arg("build")
            .arg("--release")
            .arg("--out-dir")
            .arg(&scratch)
            .arg(file)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        let ok = matches!(build, Ok(status) if status.success());
        if !ok {
            let _ = fs::remove_dir_all(&scratch);
            return Outcome::Fail;
        }
        // Find the produced executable (first non-dir, non-`.o` file).
        let mut binary: Option<PathBuf> = None;
        if let Ok(entries) = fs::read_dir(&scratch) {
            for entry in entries.flatten() {
                let path = entry.path();
                if path.is_file()
                    && path
                        .extension()
                        .and_then(|e| e.to_str())
                        .is_none_or(|e| !matches!(e, "o" | "ll" | "bc" | "S" | "asm"))
                {
                    binary = Some(path);
                    break;
                }
            }
        }
        let outcome = match binary {
            Some(p) => {
                let run = Command::new(&p)
                    .stdin(Stdio::null())
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn();
                match run {
                    Ok(child) => wait_bounded(child, Duration::from_mins(1)),
                    Err(_) => Outcome::Fail,
                }
            }
            None => Outcome::Skipped,
        };
        let _ = fs::remove_dir_all(&scratch);
        outcome
    }

    fn wait_bounded(mut child: std::process::Child, timeout: Duration) -> Outcome {
        let start = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    return if status.success() {
                        Outcome::Pass
                    } else {
                        Outcome::Fail
                    };
                }
                Ok(None) => {
                    if start.elapsed() >= timeout {
                        let _ = child.kill();
                        let _ = child.wait();
                        return Outcome::Fail;
                    }
                    std::thread::sleep(Duration::from_millis(40));
                }
                Err(_) => return Outcome::Fail,
            }
        }
    }

    // BTreeMap type import kept to suppress dead-code lint when the
    // sidecar shape is consumed only via render_sidecar.
    #[allow(dead_code)]
    fn _unused_btreemap() -> BTreeMap<String, TierStatus> {
        BTreeMap::new()
    }
}
