//! `gos test [PATH] [--run RX] [--parallel N] [--format junit]
//! [--junit-out FILE] [--race] [--coverage FILE]` — discovers and
//! runs every `#[test]`-annotated function under `PATH`, plus every
//! fenced doc-test it can extract from `///` comments.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::cmd::attr_walk::{collect_selected_fn_names, item_has_attr};
use crate::loaders::{load_and_check, load_and_check_with_sf};
use crate::paths::{collect_lint_targets, default_test_root, read_source};

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

/// Convenience entry point used by the dispatch in `main`. Threads
/// the supplied path through default-arguments for the other knobs.
#[allow(dead_code)]
pub(crate) fn run(path: Option<&Path>) -> Result<()> {
    run_with_opts(TestOpts {
        path: path.map(Path::to_path_buf),
        run_filter: None,
        parallel: 1,
        format: "human".to_string(),
        junit_out: None,
        race: false,
        coverage: None,
    })
}

#[allow(clippy::cognitive_complexity, clippy::too_many_lines)]
pub(crate) fn run_with_opts(opts: TestOpts) -> Result<()> {
    gossamer_resolve::set_test_cfg(true);
    if opts.race {
        gossamer_runtime::race::enable();
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
        // typecheck. Bubble up so the harness exits non-zero —
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

/// Renders an lcov report from a per-test-record summary.
///
/// MIR-level basic-block instrumentation that would let us compute
/// real line-by-line hit counters is a Phase-2 follow-up; until
/// then this writer attributes one synthetic execution count per
/// passing `#[test]` function to the file containing it. The shape
/// is well-formed lcov so `genhtml`, `lcov-summary`, and CI
/// dashboards parse it directly. When per-line counters land,
/// only the body of this function changes.
fn render_lcov(records: &[TestRecord], files: &[PathBuf]) -> String {
    let mut out = String::new();
    let mut by_file: std::collections::BTreeMap<&str, (u32, u32)> =
        std::collections::BTreeMap::new();
    for record in records {
        let entry = by_file.entry(record.file.as_str()).or_insert((0, 0));
        if record.passed {
            entry.0 += 1;
        }
        entry.1 += 1;
    }
    for file in files {
        let path = file.to_string_lossy();
        let (passed, total) = by_file.get(path.as_ref()).copied().unwrap_or((0, 0));
        out.push_str("TN:\n");
        out.push_str(&format!("SF:{path}\n"));
        out.push_str(&format!("FNF:{total}\n"));
        out.push_str(&format!("FNH:{passed}\n"));
        out.push_str("end_of_record\n");
    }
    out
}

fn collect_test_names(file: &Path) -> Result<Vec<String>> {
    let source = read_source(&file.to_path_buf())?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (_program, sf, _tcx) = load_and_check_with_sf(&source, file_id, &map)?;
    let mut names = Vec::new();
    collect_selected_fn_names(&sf.items, &|item| item_has_attr(item, "test"), &mut names);
    Ok(names)
}

fn run_tests_filtered(
    file: &Path,
    names: &[String],
    style: &TestStyle,
    quiet: bool,
) -> Vec<TestRecord> {
    let Ok(source) = read_source(&file.to_path_buf()) else {
        return Vec::new();
    };
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let Ok((program, _sf, _tcx)) = load_and_check_with_sf(&source, file_id, &map) else {
        return Vec::new();
    };
    let mut interp = gossamer_interp::Interpreter::new();
    interp.load(&program);
    let mut records = Vec::new();
    if !quiet && !names.is_empty() {
        let header = format!("=== {} ===", file.display());
        println!("{}", style.cyan(&header));
    }
    for name in names {
        gossamer_interp::reset_test_tally();
        let started = std::time::Instant::now();
        let outcome = interp.call(name, Vec::new());
        let elapsed = started.elapsed();
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
                    reason.push_str(" — ");
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
            }
        }
    }
    records
}

type FileQueue = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, Vec<String>)>>>;
type FileResults = std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, Vec<TestRecord>)>>>;

fn run_files_parallel(
    by_file: &[(PathBuf, Vec<String>)],
    parallel: usize,
    style: &TestStyle,
    quiet: bool,
) -> Vec<(PathBuf, Vec<TestRecord>)> {
    use std::sync::Arc;
    use std::sync::Mutex as StdMutex;
    let queue: FileQueue = Arc::new(StdMutex::new(by_file.to_vec()));
    let results: FileResults = Arc::new(StdMutex::new(Vec::new()));
    let n_workers = parallel.min(by_file.len()).max(1);
    let mut handles = Vec::with_capacity(n_workers);
    for _ in 0..n_workers {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let style_owned: TestStyle = style.clone();
        handles.push(std::thread::spawn(move || {
            loop {
                let next = {
                    let mut q = queue.lock().expect("queue lock");
                    q.pop()
                };
                let Some((file, names)) = next else {
                    return;
                };
                let recs = run_tests_filtered(&file, &names, &style_owned, quiet);
                results.lock().expect("results lock").push((file, recs));
            }
        }));
    }
    for h in handles {
        let _ = h.join();
    }
    let mut out = Arc::try_unwrap(results)
        .expect("results arc unwrap")
        .into_inner()
        .expect("results lock");
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
        let Ok((program, _tcx)) = load_and_check(&body, file_id, &map) else {
            println!("  {} doc-test {} (compile)", style.fail(), doc.name);
            failures += 1;
            continue;
        };
        let mut interp = gossamer_interp::Interpreter::new();
        interp.load(&program);
        match interp.call("main", Vec::new()) {
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
