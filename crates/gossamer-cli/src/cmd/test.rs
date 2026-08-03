//! `gos test [PATH] [--run RX] [--parallel N] [--format junit]
//! [--junit-out FILE] [--race] [--coverage FILE]` discovers and
//! runs every `#[test]`-annotated function under `PATH`, plus every
//! fenced doc-test it can extract from `//` comments.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow};

use crate::cmd::attr_walk::item_has_attr;
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

fn assertion_elapsed_summary(assertions: u32, elapsed_ms: u128) -> String {
    format!(
        "({assertions} {asserts}, {elapsed_ms}ms)",
        asserts = if assertions == 1 {
            "assertion"
        } else {
            "assertions"
        },
    )
}

fn is_worker_harness_line(line: &str) -> bool {
    let trimmed = line.trim_start();
    (trimmed.starts_with("===") && trimmed.ends_with("==="))
        || trimmed.starts_with("PASS ")
        || trimmed.starts_with("FAIL ")
        || trimmed.starts_with("test: ")
}

fn print_worker_user_output(captured: &str) {
    for line in captured.lines() {
        if !is_worker_harness_line(line) {
            println!("{line}");
        }
    }
}

/// Options threaded into [`run_with_opts`].
#[allow(
    clippy::struct_excessive_bools,
    reason = "CLI switches preserve independent user-selected test runner controls"
)]
pub(crate) struct TestOpts {
    pub path: Option<PathBuf>,
    pub run_filter: Option<String>,
    pub list: bool,
    pub exact: Option<String>,
    pub fail_fast: bool,
    pub include_ignored: bool,
    pub ignored_only: bool,
    pub shuffle: bool,
    pub seed: Option<u64>,
    pub timeout: Option<std::time::Duration>,
    pub worker: bool,
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

pub(crate) fn parse_timeout(input: &str) -> Result<std::time::Duration> {
    let input = input.trim();
    let (number, multiplier) = if let Some(value) = input.strip_suffix("ms") {
        (value, 1u64)
    } else if let Some(value) = input.strip_suffix('s') {
        (value, 1_000)
    } else if let Some(value) = input.strip_suffix('m') {
        (value, 60_000)
    } else {
        return Err(anyhow!(
            "invalid duration `{input}`: expected ms, s, or m suffix"
        ));
    };
    let value: u64 = number
        .parse()
        .map_err(|_| anyhow!("invalid duration `{input}`"))?;
    let millis = value
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("duration `{input}` is too large"))?;
    if millis == 0 {
        return Err(anyhow!("timeout must be greater than zero"));
    }
    Ok(std::time::Duration::from_millis(millis))
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
    timed_out: bool,
    status: TestStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TestStatus {
    Passed,
    Failed,
    Panicked,
    TimedOut,
    Ignored,
    Skipped,
}

#[derive(Debug, Clone)]
struct TestSpec {
    function: String,
    name: String,
    args: Vec<gossamer_interp::Value>,
    ignored: bool,
    skipped: bool,
    timeout: Option<std::time::Duration>,
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

    let mut discovered: Vec<(PathBuf, TestSpec)> = Vec::new();
    let mut records: Vec<TestRecord> = Vec::new();
    let mut load_errors: Vec<String> = Vec::new();
    for file in &files {
        let tests = match discover_tests(file) {
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
        if opts.list {
            for test in tests {
                if filter
                    .as_ref()
                    .is_some_and(|regex| !regex.is_match(&test.name))
                    || opts.exact.as_ref().is_some_and(|exact| exact != &test.name)
                    || (opts.ignored_only && !test.ignored)
                {
                    continue;
                }
                let suffix = if test.ignored {
                    " (ignored)"
                } else if test.skipped {
                    " (skipped)"
                } else {
                    ""
                };
                println!("{}::{}{suffix}", file.display(), test.name);
            }
            continue;
        }
        if !tests.is_empty() {
            if let Err(err) = validate_test_file(file) {
                eprintln!("error: {err}");
                load_errors.push(format!("{}: {err}", file.display()));
                continue;
            }
        }
        for test in tests {
            if let Some(re) = filter.as_ref() {
                if !re.is_match(&test.name) {
                    continue;
                }
            }
            if opts.exact.as_ref().is_some_and(|exact| exact != &test.name) {
                continue;
            }
            let run_ignored = opts.include_ignored || opts.ignored_only;
            if test.skipped
                || (test.ignored && !run_ignored)
                || (opts.ignored_only && !test.ignored)
            {
                if !opts.ignored_only || test.ignored {
                    let status = if test.skipped {
                        TestStatus::Skipped
                    } else {
                        TestStatus::Ignored
                    };
                    if !want_junit {
                        let label = if status == TestStatus::Skipped {
                            "SKIPPED"
                        } else {
                            "IGNORED"
                        };
                        println!("  {} {}::{}", style.dim(label), file.display(), test.name);
                    }
                    records.push(TestRecord {
                        file: file.to_string_lossy().into_owned(),
                        name: test.name,
                        passed: true,
                        elapsed_ms: 0,
                        failure_message: None,
                        assertions: 0,
                        timed_out: false,
                        status,
                    });
                }
                continue;
            }
            discovered.push((file.clone(), test));
        }
    }

    if opts.list {
        return Ok(());
    }

    if opts.shuffle {
        let seed = opts.seed.unwrap_or_else(|| {
            use std::time::{SystemTime, UNIX_EPOCH};
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos() as u64)
        });
        println!("test: shuffle seed {seed}");
        deterministic_shuffle(&mut discovered, seed);
    }

    let mut total_doc_passes = 0u32;
    let mut total_doc_failures = 0u32;

    let by_file: std::collections::BTreeMap<PathBuf, Vec<TestSpec>> = {
        let mut map: std::collections::BTreeMap<PathBuf, Vec<TestSpec>> =
            std::collections::BTreeMap::new();
        for (f, n) in discovered {
            map.entry(f).or_default().push(n);
        }
        map
    };

    let parallel = if opts.fail_fast {
        1
    } else {
        opts.parallel.max(1)
    };
    let by_file_vec: Vec<(PathBuf, Vec<TestSpec>)> = by_file.into_iter().collect();
    // Process isolation is the normal mode so cwd, environment, output, and
    // leaked goroutines cannot contaminate the next test. Coverage and race
    // counters currently remain in-process because their collectors are
    // process-local; timeout metadata still forces isolation for either mode.
    let has_test_timeout = by_file_vec
        .iter()
        .any(|(_, tests)| tests.iter().any(|test| test.timeout.is_some()));
    let needs_isolation = !opts.worker
        && ((!opts.race && opts.coverage.is_none()) || opts.timeout.is_some() || has_test_timeout);
    let collected: Vec<(PathBuf, Vec<TestRecord>)> = if needs_isolation {
        run_tests_isolated(
            &by_file_vec,
            opts.timeout,
            opts.fail_fast,
            &style,
            want_junit,
        )?
    } else if parallel > 1 && by_file_vec.len() > 1 {
        run_files_parallel(&by_file_vec, parallel, &style, want_junit)
    } else {
        let mut output = Vec::new();
        for (file, names) in &by_file_vec {
            let recs = run_tests_filtered(file, names, &style, want_junit);
            let failed = recs.iter().any(|record| !record.passed);
            output.push((file.clone(), recs));
            if opts.fail_fast && failed {
                break;
            }
        }
        output
    };
    for (_, recs) in collected {
        records.extend(recs);
    }
    if !opts.worker {
        for file in &files {
            let doc_summary = run_doc_tests_in_file(file, &style);
            total_doc_passes += doc_summary.passes;
            total_doc_failures += doc_summary.failures;
        }
    }

    let total_passes = u32::try_from(
        records
            .iter()
            .filter(|r| r.status == TestStatus::Passed)
            .count(),
    )
    .unwrap_or(0)
        + total_doc_passes;
    let total_failures = u32::try_from(
        records
            .iter()
            .filter(|r| {
                matches!(
                    r.status,
                    TestStatus::Failed | TestStatus::Panicked | TestStatus::TimedOut
                )
            })
            .count(),
    )
    .unwrap_or(0)
        + total_doc_failures;
    let total_assertions: u32 = records.iter().map(|r| r.assertions).sum();
    let total_ignored = records
        .iter()
        .filter(|r| r.status == TestStatus::Ignored)
        .count();
    let total_skipped = records
        .iter()
        .filter(|r| r.status == TestStatus::Skipped)
        .count();
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
            "{total_assertions} assertion(s), {total_ignored} ignored, {total_skipped} skipped, {total_doc_tests} doc-test(s), across {} file(s), {empty_files} with no tests",
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

fn discover_tests(file: &Path) -> Result<Vec<TestSpec>> {
    let source = read_source(file)?;
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(file.to_string_lossy().into_owned(), source.clone());
    let (sf, _diags) = gossamer_parse::parse_source_file(&source, file_id);
    let mut tests = Vec::new();
    collect_test_metadata(&sf.items, &mut tests)?;
    Ok(tests)
}

fn collect_test_metadata(items: &[gossamer_ast::Item], output: &mut Vec<TestSpec>) -> Result<()> {
    for item in items {
        match &item.kind {
            gossamer_ast::ItemKind::Fn(decl) if item_has_attr(item, "test") => {
                let cases: Vec<_> = item
                    .attrs
                    .outer
                    .iter()
                    .filter(|attr| {
                        attr.path
                            .segments
                            .last()
                            .is_some_and(|segment| segment.name.name == "test_case")
                    })
                    .collect();
                let timeout = match item.attrs.outer.iter().find(|attr| {
                    attr.path
                        .segments
                        .last()
                        .is_some_and(|segment| segment.name.name == "timeout")
                }) {
                    Some(attr) => Some(parse_timeout(
                        attr.tokens
                            .as_deref()
                            .ok_or_else(|| anyhow!("#[timeout] requires a duration"))?
                            .trim_matches('"'),
                    )?),
                    None => None,
                };
                let mut push_case = |args: Vec<gossamer_interp::Value>, suffix: Option<String>| {
                    output.push(TestSpec {
                        function: decl.name.name.clone(),
                        name: suffix.map_or_else(
                            || decl.name.name.clone(),
                            |suffix| format!("{}[{suffix}]", decl.name.name),
                        ),
                        args,
                        ignored: item_has_attr(item, "ignore"),
                        skipped: item_has_attr(item, "skip"),
                        timeout,
                    });
                };
                if cases.is_empty() {
                    push_case(Vec::new(), None);
                } else {
                    for attr in cases {
                        let raw = attr.tokens.as_deref().unwrap_or("");
                        let args = parse_test_case_args(raw)?;
                        push_case(args, Some(raw.to_string()));
                    }
                }
            }
            gossamer_ast::ItemKind::Mod(module) => {
                if let gossamer_ast::ModBody::Inline(items) = &module.body {
                    collect_test_metadata(items, output)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

fn parse_test_case_args(raw: &str) -> Result<Vec<gossamer_interp::Value>> {
    let mut parts = Vec::new();
    let mut start = 0usize;
    let mut quote = None;
    let mut escaped = false;
    for (index, ch) in raw.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if ch == '\\' && quote.is_some() {
            escaped = true;
            continue;
        }
        if matches!(ch, '\'' | '"') {
            if quote == Some(ch) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(ch);
            }
        } else if ch == ',' && quote.is_none() {
            parts.push(raw[start..index].trim());
            start = index + 1;
        }
    }
    if quote.is_some() {
        return Err(anyhow!("unterminated string in #[test_case({raw})]"));
    }
    if !raw.trim().is_empty() {
        parts.push(raw[start..].trim());
    }
    parts.into_iter().map(parse_test_case_literal).collect()
}

fn parse_test_case_literal(raw: &str) -> Result<gossamer_interp::Value> {
    let compact_number;
    let raw = if raw.starts_with("- ") || raw.starts_with("+ ") {
        compact_number = raw.replace(' ', "");
        compact_number.as_str()
    } else {
        raw
    };
    if raw == "true" {
        return Ok(gossamer_interp::Value::Bool(true));
    }
    if raw == "false" {
        return Ok(gossamer_interp::Value::Bool(false));
    }
    if let Ok(value) = raw.parse::<i64>() {
        return Ok(gossamer_interp::Value::Int(value));
    }
    if let Ok(value) = raw.parse::<f64>() {
        return Ok(gossamer_interp::Value::Float(value));
    }
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        let mut decoded = String::new();
        let mut chars = raw[1..raw.len() - 1].chars();
        while let Some(ch) = chars.next() {
            if ch != '\\' {
                decoded.push(ch);
                continue;
            }
            decoded.push(
                match chars
                    .next()
                    .ok_or_else(|| anyhow!("incomplete escape in #[test_case]"))?
                {
                    'n' => '\n',
                    'r' => '\r',
                    't' => '\t',
                    '\\' => '\\',
                    '"' => '"',
                    escape => {
                        return Err(anyhow!("unsupported escape `\\{escape}` in #[test_case]"));
                    }
                },
            );
        }
        return Ok(gossamer_interp::Value::String(decoded.into()));
    }
    Err(anyhow!(
        "#[test_case] arguments must be bool, number, or string literals; found `{raw}`"
    ))
}

fn validate_test_file(file: &Path) -> Result<()> {
    // The file carries tests: refuse to run any if the program does not
    // statically resolve and typecheck (a harness over broken code is worse
    // than useless). Validate the bundled whole-package source - the same
    // augment + comptime-fold + check the execution path uses - so cross-file
    // references stay valid and a real static error is surfaced with its
    // "refusing to execute" trailer rather than swallowed.
    let entry = read_entry_source(file)?;
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
    Ok(())
}

fn deterministic_shuffle<T>(values: &mut [T], mut state: u64) {
    if state == 0 {
        state = 0x9e37_79b9_7f4a_7c15;
    }
    for index in (1..values.len()).rev() {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        values.swap(index, (state as usize) % (index + 1));
    }
}

fn run_tests_filtered(
    file: &Path,
    tests: &[TestSpec],
    style: &TestStyle,
    quiet: bool,
) -> Vec<TestRecord> {
    // Execute on a thread with a large native stack so a deeply
    // recursive `#[test]` does not overflow the host's default
    // main-thread stack (see `cmd::with_vm_stack`). Covers both the
    // serial and the parallel-worker call sites.
    let file = file.to_path_buf();
    let tests = tests.to_vec();
    let style = style.clone();
    crate::cmd::with_vm_stack(move || run_tests_filtered_inner(&file, &tests, &style, quiet))
}

#[allow(
    clippy::too_many_lines,
    reason = "one VM session owns test execution, tally capture, and stable reporting"
)]
fn run_tests_filtered_inner(
    file: &Path,
    tests: &[TestSpec],
    style: &TestStyle,
    quiet: bool,
) -> Vec<TestRecord> {
    // Bundle sibling `*.gos` modules into the compilation, exactly as
    // `gos` / `gos build` do, so a `#[test]` can reach a sibling
    // module (`super::helper::triple` where `src/helper.gos` is declared
    // `mod helper;`). Test-name collection stays unbundled so sibling
    // tests are not double-counted against this file.
    let Ok(source) = read_entry_source(file) else {
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
    // Publish the source map before `load` for runtime traceback locations and
    // per-statement coverage hits when `gos test --coverage` is active.
    vm.set_source_map(std::sync::Arc::new(map));
    if vm.load(&program, tcx, false).is_err() {
        return Vec::new();
    }
    vm.clear_source_map();
    let mut records = Vec::new();
    if !quiet && !tests.is_empty() {
        let header = format!("=== {} ===", file.display());
        println!("{}", style.cyan(&header));
    }
    for test in tests {
        gossamer_interp::reset_test_tally();
        let started = std::time::Instant::now();
        let outcome = vm.call(&test.function, test.args.clone());
        let elapsed = started.elapsed();
        // Snapshot the VM call chain immediately: a failing test
        // preserves its frames, and the next `vm.call` clears them.
        let call_trace = if outcome.is_err() {
            crate::cmd::traceback::render_call_stack(&vm.call_stack_frames())
        } else {
            String::new()
        };
        let tally = gossamer_interp::take_test_tally();
        let panicked = outcome.as_ref().err().map(ToString::to_string);
        let assertion_failure = tally.failures > 0;
        let passed = panicked.is_none() && !assertion_failure;
        let status = if passed {
            TestStatus::Passed
        } else if panicked.is_some() {
            TestStatus::Panicked
        } else {
            TestStatus::Failed
        };
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
            name: test.name.clone(),
            passed,
            elapsed_ms: elapsed.as_millis(),
            failure_message: failure_message.clone(),
            assertions: tally.assertions,
            timed_out: false,
            status,
        });
        if !quiet {
            if passed {
                let assertion_summary =
                    assertion_elapsed_summary(tally.assertions, elapsed.as_millis());
                println!(
                    "  {} {} {}",
                    style.pass(),
                    test.name,
                    style.dim(&assertion_summary)
                );
            } else {
                let elapsed_str = format!("({}ms)", elapsed.as_millis());
                println!(
                    "  {} {} {}: {}",
                    style.fail(),
                    test.name,
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

type FileQueue = std::sync::Arc<parking_lot::Mutex<Vec<(PathBuf, Vec<TestSpec>)>>>;
type FileResults = std::sync::Arc<parking_lot::Mutex<Vec<(PathBuf, Vec<TestRecord>)>>>;

#[allow(
    clippy::too_many_lines,
    reason = "process lifecycle and timeout cleanup stay together for isolation safety"
)]
fn run_tests_isolated(
    by_file: &[(PathBuf, Vec<TestSpec>)],
    default_timeout: Option<std::time::Duration>,
    fail_fast: bool,
    style: &TestStyle,
    quiet: bool,
) -> Result<Vec<(PathBuf, Vec<TestRecord>)>> {
    let executable =
        std::env::current_exe().map_err(|error| anyhow!("locate test worker: {error}"))?;
    let mut output = Vec::new();
    let mut stop = false;
    for (file, tests) in by_file {
        let mut records = Vec::new();
        for test in tests {
            if stop {
                break;
            }
            let timeout = test.timeout.or(default_timeout);
            let mut command = std::process::Command::new(&executable);
            command
                .args([
                    "test",
                    &file.to_string_lossy(),
                    "--exact",
                    &test.name,
                    "--serial",
                    "--test-worker",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped());
            if test.ignored {
                command.arg("--include-ignored");
            }
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            #[cfg(windows)]
            {
                use std::os::windows::process::CommandExt;
                command.creation_flags(0x0000_0200);
            }
            let mut child = command
                .spawn()
                .map_err(|error| anyhow!("spawn isolated test `{}`: {error}", test.name))?;
            let stdout = child.stdout.take().map(|mut stream| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = stream.read_to_end(&mut bytes);
                    bytes
                })
            });
            let stderr = child.stderr.take().map(|mut stream| {
                std::thread::spawn(move || {
                    let mut bytes = Vec::new();
                    let _ = stream.read_to_end(&mut bytes);
                    bytes
                })
            });
            let started = std::time::Instant::now();
            let (status, timed_out) = if let Some(timeout) = timeout {
                loop {
                    if let Some(status) = child.try_wait().map_err(|error| {
                        anyhow!("wait for isolated test `{}`: {error}", test.name)
                    })? {
                        break (Some(status), false);
                    }
                    if started.elapsed() >= timeout {
                        let _ = gossamer_std::exec::send_group_term(i64::from(child.id()));
                        let _ = child.kill();
                        let _ = child.wait();
                        break (None, true);
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
            } else {
                (
                    Some(child.wait().map_err(|error| {
                        anyhow!("wait for isolated test `{}`: {error}", test.name)
                    })?),
                    false,
                )
            };
            let stdout = stdout
                .and_then(|thread| thread.join().ok())
                .unwrap_or_default();
            let stderr = stderr
                .and_then(|thread| thread.join().ok())
                .unwrap_or_default();
            let captured = format!(
                "{}{}",
                String::from_utf8_lossy(&stdout),
                String::from_utf8_lossy(&stderr)
            );
            let passed = status.is_some_and(|status| status.success()) && !timed_out;
            let assertions = captured
                .lines()
                .find_map(|line| {
                    let marker = line.find(" assertion")?;
                    line[..marker]
                        .split_whitespace()
                        .last()?
                        .parse::<u32>()
                        .ok()
                })
                .unwrap_or(0);
            let failure_message = (!passed).then(|| {
                if timed_out {
                    format!(
                        "timeout after {}ms",
                        timeout.expect("timed out only with deadline").as_millis()
                    )
                } else if captured.trim().is_empty() {
                    "isolated test failed".to_string()
                } else {
                    captured.trim().to_string()
                }
            });
            let status = if timed_out {
                TestStatus::TimedOut
            } else if passed {
                TestStatus::Passed
            } else if captured.contains("panic:") {
                TestStatus::Panicked
            } else {
                TestStatus::Failed
            };
            let record = TestRecord {
                file: file.to_string_lossy().into_owned(),
                name: test.name.clone(),
                passed,
                elapsed_ms: started.elapsed().as_millis(),
                failure_message,
                assertions,
                timed_out,
                status,
            };
            if !quiet {
                if record.passed {
                    print_worker_user_output(&captured);
                    let assertion_summary =
                        assertion_elapsed_summary(record.assertions, record.elapsed_ms);
                    println!(
                        "  {} {} {}",
                        style.pass(),
                        record.name,
                        style.dim(&assertion_summary)
                    );
                } else {
                    println!(
                        "  {} {}: {}",
                        style.fail(),
                        record.name,
                        record.failure_message.as_deref().unwrap_or("failed")
                    );
                }
            }
            stop = fail_fast && !record.passed;
            records.push(record);
        }
        output.push((file.clone(), records));
        if stop {
            break;
        }
    }
    Ok(output)
}

fn run_files_parallel(
    by_file: &[(PathBuf, Vec<TestSpec>)],
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
    let total_failures = records
        .iter()
        .filter(|r| matches!(r.status, TestStatus::Failed | TestStatus::Panicked))
        .count();
    let total_errors = records
        .iter()
        .filter(|r| r.status == TestStatus::TimedOut)
        .count();
    let total_skipped = records
        .iter()
        .filter(|r| matches!(r.status, TestStatus::Ignored | TestStatus::Skipped))
        .count();
    out.push_str(&format!(
        "<testsuites tests=\"{total_tests}\" failures=\"{total_failures}\" errors=\"{total_errors}\" skipped=\"{total_skipped}\">\n"
    ));
    for (suite, tests) in &suites {
        let n = tests.len();
        let failures = tests
            .iter()
            .filter(|r| matches!(r.status, TestStatus::Failed | TestStatus::Panicked))
            .count();
        let errors = tests
            .iter()
            .filter(|r| r.status == TestStatus::TimedOut)
            .count();
        let skipped = tests
            .iter()
            .filter(|r| matches!(r.status, TestStatus::Ignored | TestStatus::Skipped))
            .count();
        let total_ms: u128 = tests.iter().map(|r| r.elapsed_ms).sum();
        let seconds = (total_ms as f64) / 1000.0;
        out.push_str(&format!(
            "  <testsuite name=\"{}\" tests=\"{n}\" failures=\"{failures}\" errors=\"{errors}\" skipped=\"{skipped}\" time=\"{seconds:.3}\">\n",
            xml_escape(suite)
        ));
        for record in tests {
            let elapsed_s = (record.elapsed_ms as f64) / 1000.0;
            out.push_str(&format!(
                "    <testcase classname=\"{cls}\" name=\"{name}\" time=\"{elapsed_s:.3}\"",
                cls = xml_escape(suite),
                name = xml_escape(&record.name),
            ));
            if matches!(record.status, TestStatus::Ignored | TestStatus::Skipped) {
                let kind = if record.status == TestStatus::Ignored {
                    "ignored"
                } else {
                    "skipped"
                };
                out.push_str(&format!(
                    ">\n      <skipped message=\"{kind}\"/>\n    </testcase>\n"
                ));
            } else if record.passed {
                out.push_str("/>\n");
            } else if record.timed_out {
                out.push_str(">\n      <error type=\"timeout\" message=\"");
                out.push_str(&xml_escape(
                    record.failure_message.as_deref().unwrap_or("timed out"),
                ));
                out.push_str("\"/>\n    </testcase>\n");
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

#[cfg(test)]
mod focused_tests {
    use super::*;

    #[test]
    fn timeout_units_and_zero_are_checked() {
        assert_eq!(
            parse_timeout("250ms").unwrap(),
            std::time::Duration::from_millis(250)
        );
        assert_eq!(
            parse_timeout("2s").unwrap(),
            std::time::Duration::from_secs(2)
        );
        assert!(parse_timeout("0s").is_err());
        assert!(parse_timeout("10").is_err());
    }

    #[test]
    fn parameter_literals_and_shuffle_are_reproducible() {
        let args = parse_test_case_args("1, -2.5, true, \"a\\nline\"").unwrap();
        assert!(
            matches!(args.as_slice(), [gossamer_interp::Value::Int(1), gossamer_interp::Value::Float(value), gossamer_interp::Value::Bool(true), gossamer_interp::Value::String(text)] if *value == -2.5 && text.as_str() == "a\nline")
        );
        let mut first = vec![1, 2, 3, 4, 5];
        let mut second = first.clone();
        deterministic_shuffle(&mut first, 42);
        deterministic_shuffle(&mut second, 42);
        assert_eq!(first, second);
    }
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
        vm.clear_source_map();
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
            // Cranelift is the in-process JIT under `gos`; tier
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
