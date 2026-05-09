//! Tier parity gate — VM, Cranelift debug, LLVM release.
//!
//! Every `.gos` source under `examples/` and
//! `feature-testing-examples/` is run in all three tiers and the
//! captured stdout / exit code must match. The harness is the
//! single source of truth for cross-tier behaviour: a regression in
//! any backend turns this suite red.
//!
//! Examples needing CLI args, stdin, or running an HTTP server
//! carry a row in `SPECS` describing the fixture. Server-style
//! examples are bounded with a hard 60 s wall clock cap so a
//! regression that hangs a tier cannot stall CI.
//!
//! `GOSSAMER_FAIL_ON_LLVM_FALLBACK` is enabled separately by
//! `llvm_release_lowers_every_example_without_fallback`, surfacing
//! "LLVM body silently routed to Cranelift" regressions distinct
//! from output-level parity.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PER_RUN_TIMEOUT: Duration = Duration::from_secs(60);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-parity-{pid}-{n}-{tag}",
        pid = std::process::id(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Tier {
    Vm,
    Cranelift,
    Llvm,
}

impl Tier {
    fn label(self) -> &'static str {
        match self {
            Tier::Vm => "vm",
            Tier::Cranelift => "cranelift",
            Tier::Llvm => "llvm",
        }
    }
}

struct Spec {
    /// Path relative to the workspace root.
    path: &'static str,
    /// Args appended after the source on `gos run`, or passed
    /// directly to the compiled binary.
    args: &'static [&'static str],
    /// Stdin to feed to every tier's run.
    stdin: &'static [u8],
    /// Stdout is non-deterministic; compare line multisets only.
    nondeterministic: bool,
    /// Allow non-zero exit (must still match across tiers).
    allow_nonzero: bool,
    /// Skip parity entirely; the VM still has to run cleanly.
    skip_parity: Option<&'static str>,
    /// Skip everything (including the VM run) with a reason.
    skip_all: Option<&'static str>,
    /// HTTP-server fixture: spawn, sleep `boot_ms`, send a probe,
    /// kill, compare the probe response across tiers.
    server: Option<ServerFixture>,
}

#[derive(Clone, Copy)]
struct ServerFixture {
    /// Wait this long after launch before issuing the probe.
    boot_ms: u64,
    /// Listen address baked into the example.
    addr: &'static str,
    /// Probe path, e.g. `/health`.
    probe_path: &'static str,
}

const fn spec(path: &'static str) -> Spec {
    Spec {
        path,
        args: &[],
        stdin: &[],
        nondeterministic: false,
        allow_nonzero: false,
        skip_parity: None,
        skip_all: None,
        server: None,
    }
}

const SPECS: &[Spec] = &[
    // --- examples/ ---
    spec("examples/binary_search.gos"),
    spec("examples/bubble_sort.gos"),
    spec("examples/caesar_cipher.gos"),
    Spec {
        args: &[
            "--name",
            "jane",
            "--port",
            "9000",
            "--verbose",
            "alpha",
            "beta",
        ],
        ..spec("examples/cli_args.gos")
    },
    spec("examples/concurrency.gos"),
    spec("examples/control_flow.gos"),
    spec("examples/data_structures.gos"),
    spec("examples/digit_sum.gos"),
    spec("examples/environment.gos"),
    spec("examples/errors.gos"),
    spec("examples/factorial.gos"),
    spec("examples/fibonacci.gos"),
    spec("examples/file_io.gos"),
    spec("examples/fizz_buzz.gos"),
    spec("examples/fnv_hash.gos"),
    spec("examples/function_piping.gos"),
    spec("examples/gcd.gos"),
    Spec {
        nondeterministic: true,
        skip_parity: Some(
            "goroutine completion count differs across tiers under scheduling pressure",
        ),
        ..spec("examples/go_spawn.gos")
    },
    Spec {
        args: &["needle"],
        stdin: b"alpha line\nneedle hidden here\nanother needle\nclosing\n",
        ..spec("examples/grep.gos")
    },
    spec("examples/hello_world.gos"),
    Spec {
        skip_all: Some("needs live web_server.gos on :8080 — covered by web_server smoke tests"),
        ..spec("examples/http_client.gos")
    },
    spec("examples/line_count.gos"),
    spec("examples/linked_list.gos"),
    spec("examples/list_dir.gos"),
    spec("examples/prime_check.gos"),
    spec("examples/range_sum.gos"),
    spec("examples/regex.gos"),
    spec("examples/reverse_string.gos"),
    spec("examples/shapes.gos"),
    spec("examples/sleep_demo.gos"),
    spec("examples/temperature.gos"),
    Spec {
        skip_parity: Some(
            "fn main is empty stub — coverage comes from `gos test examples/testing.gos`",
        ),
        ..spec("examples/testing.gos")
    },
    spec("examples/vowel_count.gos"),
    Spec {
        server: Some(ServerFixture {
            boot_ms: 800,
            addr: "127.0.0.1:8080",
            probe_path: "/health",
        }),
        ..spec("examples/web_server.gos")
    },
    spec("examples/word_count.gos"),
    // --- feature-testing-examples/ ---
    spec("feature-testing-examples/array_bounds_probe.gos"),
    spec("feature-testing-examples/channel_close_drain.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/channel_fan_in.gos")
    },
    spec("feature-testing-examples/closure_capture_mutation.gos"),
    spec("feature-testing-examples/closure_lifetime_inference.gos"),
    spec("feature-testing-examples/defer_unwind_order.gos"),
    spec("feature-testing-examples/doc_test_vs_unit_test_drift.gos"),
    spec("feature-testing-examples/error_chain_inspection.gos"),
    spec("feature-testing-examples/error_question_mark_propagation.gos"),
    spec("feature-testing-examples/float_cast_drift.gos"),
    spec("feature-testing-examples/format_precision_padding.gos"),
    spec("feature-testing-examples/fs_temp_file_lifecycle.gos"),
    spec("feature-testing-examples/generic_function_monomorphization.gos"),
    spec("feature-testing-examples/goroutine_panic_isolation.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/hashmap_counter_race.gos")
    },
    spec("feature-testing-examples/integer_overflow_edges.gos"),
    spec("feature-testing-examples/iter_combinator_chain.gos"),
    spec("feature-testing-examples/json_round_trip_fuzz.gos"),
    spec("feature-testing-examples/method_dispatch_collision.gos"),
    spec("feature-testing-examples/mutex_poison_recovery.gos"),
    spec("feature-testing-examples/mutex_vs_channel_counter.gos"),
    spec("feature-testing-examples/numeric_conversion_matrix.gos"),
    spec("feature-testing-examples/option_unwrap_chain.gos"),
    spec("feature-testing-examples/os_signal_handler.gos"),
    spec("feature-testing-examples/panic_recover_round_trip.gos"),
    spec("feature-testing-examples/pattern_match_exhaustiveness.gos"),
    spec("feature-testing-examples/pipe_operator_precedence.gos"),
    spec("feature-testing-examples/process_spawn_pipe.gos"),
    spec("feature-testing-examples/recursive_enum_walk.gos"),
    spec("feature-testing-examples/reference_alias_mutation.gos"),
    spec("feature-testing-examples/regex_unicode_categories.gos"),
    Spec {
        skip_parity: Some("poll-attempt count is scheduler-dependent; output varies across tiers"),
        ..spec("feature-testing-examples/select_default_timing.gos")
    },
    spec("feature-testing-examples/slice_subslicing.gos"),
    spec("feature-testing-examples/sort_with_closure.gos"),
    spec("feature-testing-examples/string_concatenation_stress.gos"),
    spec("feature-testing-examples/string_unicode_boundaries.gos"),
    spec("feature-testing-examples/time_monotonic_vs_wall.gos"),
    spec("feature-testing-examples/trait_object_dispatch.gos"),
    Spec {
        nondeterministic: true,
        ..spec("feature-testing-examples/tuple_destructuring_loop.gos")
    },
    spec("feature-testing-examples/variable_shadowing_ladder.gos"),
    spec("feature-testing-examples/literal_forms.gos"),
    spec("feature-testing-examples/loop_continue.gos"),
    spec("feature-testing-examples/match_or_patterns.gos"),
    spec("feature-testing-examples/static_items.gos"),
    spec("feature-testing-examples/struct_update_base.gos"),
    spec("feature-testing-examples/at_binding_subpattern.gos"),
    spec("feature-testing-examples/scheduler_drain.gos"),
    spec("feature-testing-examples/static_mut_basic.gos"),
    spec("feature-testing-examples/closure_goroutine.gos"),
];

#[derive(Debug)]
struct Run {
    stdout: String,
    stderr: String,
    code: Option<i32>,
}

fn normalize_newlines(s: &str) -> String {
    s.replace("\r\n", "\n")
}

fn run_with_timeout(mut child: Child, stdin: &[u8], deadline: Instant) -> Run {
    if let Some(mut sin) = child.stdin.take() {
        let _ = sin.write_all(stdin);
        drop(sin);
    }
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
            Err(_) => break,
        }
    }
    let out = child.wait_with_output().expect("wait_with_output");
    Run {
        stdout: normalize_newlines(&String::from_utf8_lossy(&out.stdout)),
        stderr: normalize_newlines(&String::from_utf8_lossy(&out.stderr)),
        code: out.status.code(),
    }
}

fn run_vm(src: &Path, args: &[&str], stdin: &[u8]) -> Run {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("run").arg(src);
    if !args.is_empty() {
        cmd.arg("--").args(args);
    }
    let child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos run");
    run_with_timeout(child, stdin, Instant::now() + PER_RUN_TIMEOUT)
}

fn build_native(src: &Path, release: bool, scratch: &Path) -> Result<PathBuf, String> {
    let mut cmd = Command::new(gos_bin());
    cmd.arg("build");
    if release {
        cmd.arg("--release");
    }
    cmd.arg("--out-dir").arg(scratch).arg(src);
    let out = cmd.output().expect("spawn gos build");
    if !out.status.success() {
        return Err(format!(
            "gos build {flag} failed:\n  stdout: {}\n  stderr: {}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr),
            flag = if release { "--release" } else { "" },
        ));
    }
    // The unit name is manifest-derived (project id tail) for sources
    // inside a project, or the file stem for loose-file builds. Scan
    // the scratch dir for a single executable instead of guessing.
    let mut binaries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(scratch)
        .map_err(|e| format!("read_dir {}: {e}", scratch.display()))?
        .flatten()
    {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        if is_executable(&p) {
            binaries.push(p);
        }
    }
    if binaries.is_empty() {
        return Err(format!(
            "gos build produced no executable in {}",
            scratch.display(),
        ));
    }
    if binaries.len() > 1 {
        return Err(format!(
            "gos build produced multiple executables in {}: {binaries:?}",
            scratch.display(),
        ));
    }
    Ok(binaries.into_iter().next().expect("checked len == 1"))
}

#[cfg(unix)]
fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(p: &Path) -> bool {
    p.extension()
        .and_then(|s| s.to_str())
        .map(|e| e.eq_ignore_ascii_case("exe"))
        .unwrap_or(false)
}

fn run_native(bin: &Path, args: &[&str], stdin: &[u8]) -> Run {
    let child = Command::new(bin)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn native binary");
    run_with_timeout(child, stdin, Instant::now() + PER_RUN_TIMEOUT)
}

fn run_tier(spec: &Spec, tier: Tier) -> Result<Run, String> {
    let src = workspace_root().join(spec.path);
    match tier {
        Tier::Vm => Ok(run_vm(&src, spec.args, spec.stdin)),
        Tier::Cranelift => {
            let scratch = fresh_dir(&format!("cl-{}", file_tag(spec.path)));
            let bin = build_native(&src, false, &scratch)?;
            let run = run_native(&bin, spec.args, spec.stdin);
            let _ = fs::remove_dir_all(&scratch);
            Ok(run)
        }
        Tier::Llvm => {
            let scratch = fresh_dir(&format!("ll-{}", file_tag(spec.path)));
            let bin = build_native(&src, true, &scratch)?;
            let run = run_native(&bin, spec.args, spec.stdin);
            let _ = fs::remove_dir_all(&scratch);
            Ok(run)
        }
    }
}

fn file_tag(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("x")
        .to_string()
}

fn stdout_matches(a: &str, b: &str, nondeterministic: bool) -> bool {
    if nondeterministic {
        let mut la: Vec<&str> = a.lines().collect();
        let mut lb: Vec<&str> = b.lines().collect();
        la.sort_unstable();
        lb.sort_unstable();
        la == lb
    } else {
        a == b
    }
}

fn divergence(spec: &Spec, lhs: (Tier, &Run), rhs: (Tier, &Run)) -> Option<String> {
    if !stdout_matches(&lhs.1.stdout, &rhs.1.stdout, spec.nondeterministic) {
        return Some(format!(
            "{path}: stdout diverged between {a} and {b}\n  {a}: {astdout:?}\n  {b}: {bstdout:?}",
            path = spec.path,
            a = lhs.0.label(),
            b = rhs.0.label(),
            astdout = lhs.1.stdout,
            bstdout = rhs.1.stdout,
        ));
    }
    if !spec.allow_nonzero && lhs.1.code != rhs.1.code {
        return Some(format!(
            "{path}: exit code diverged: {a}={ac:?} {b}={bc:?}",
            path = spec.path,
            a = lhs.0.label(),
            ac = lhs.1.code,
            b = rhs.0.label(),
            bc = rhs.1.code,
        ));
    }
    None
}

#[test]
fn vm_runs_every_example_without_crashing() {
    let mut failures = Vec::new();
    for spec in SPECS {
        if let Some(reason) = spec.skip_all {
            eprintln!("skip vm: {} ({reason})", spec.path);
            continue;
        }
        if spec.server.is_some() {
            // Server VM coverage lives in `web_server_smoke_vm`.
            continue;
        }
        let run = match run_tier(spec, Tier::Vm) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: vm error: {e}", spec.path));
                continue;
            }
        };
        if !spec.allow_nonzero && run.code != Some(0) {
            failures.push(format!(
                "{}: vm exit={:?}\n  stderr: {}",
                spec.path, run.code, run.stderr,
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{} VM run failures:\n{}",
        failures.len(),
        failures.join("\n\n"),
    );
}

#[test]
fn cranelift_debug_matches_vm_on_every_example() {
    parity_walk(Tier::Cranelift);
}

#[test]
fn llvm_release_matches_vm_on_every_example() {
    parity_walk(Tier::Llvm);
}

fn parity_walk(compiled: Tier) {
    let mut failures = Vec::new();
    for spec in SPECS {
        if spec.skip_all.is_some() || spec.skip_parity.is_some() || spec.server.is_some() {
            continue;
        }
        let vm = match run_tier(spec, Tier::Vm) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: vm error: {e}", spec.path));
                continue;
            }
        };
        let other = match run_tier(spec, compiled) {
            Ok(r) => r,
            Err(e) => {
                failures.push(format!("{}: {} error: {e}", spec.path, compiled.label()));
                continue;
            }
        };
        if let Some(d) = divergence(spec, (Tier::Vm, &vm), (compiled, &other)) {
            failures.push(d);
        }
    }
    assert!(
        failures.is_empty(),
        "{} {} parity failures:\n{}",
        failures.len(),
        compiled.label(),
        failures.join("\n\n"),
    );
}

// ----------------------------------------------------------------
// Server fixtures.
//
// `web_server.gos` is the only HTTP server in the example set. We
// verify that each tier boots the listener within the boot
// budget, responds 200 to `GET /health`, and exits cleanly when
// the test process tears it down. The probe is a hand-rolled
// `TcpStream` so the test depends on no crate-level HTTP client.
// ----------------------------------------------------------------

#[test]
fn web_server_smoke_vm() {
    server_smoke(Tier::Vm);
}

#[test]
fn web_server_smoke_cranelift() {
    server_smoke(Tier::Cranelift);
}

#[test]
fn web_server_smoke_llvm() {
    server_smoke(Tier::Llvm);
}

/// Serialises the `web_server.gos` smoke tests across all three
/// tiers. The example hardcodes `0.0.0.0:8080`; running the three
/// `#[test]` variants in parallel races on that port and produces
/// spurious connection-refused failures on whichever tier the
/// scheduler started second.
static SERVER_PORT_LOCK: Mutex<()> = Mutex::new(());

fn server_smoke(tier: Tier) {
    let _port_guard = SERVER_PORT_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let spec = SPECS
        .iter()
        .find(|s| s.path == "examples/web_server.gos")
        .expect("web_server spec");
    let server = spec.server.expect("server fixture");
    let deadline = Instant::now() + PER_RUN_TIMEOUT;

    let src = workspace_root().join(spec.path);
    let (mut child, scratch) = match tier {
        Tier::Vm => {
            let child = Command::new(gos_bin())
                .arg("run")
                .arg(&src)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn gos run web_server");
            (child, None)
        }
        compiled => {
            let release = matches!(compiled, Tier::Llvm);
            let scratch = fresh_dir(&format!("server-{}", compiled.label()));
            let bin = match build_native(&src, release, &scratch) {
                Ok(p) => p,
                Err(e) => panic!("{} build of web_server.gos failed: {e}", compiled.label()),
            };
            let child = Command::new(&bin)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn web_server binary");
            (child, Some(scratch))
        }
    };

    std::thread::sleep(Duration::from_millis(server.boot_ms));

    let probe = http_probe(server.addr, server.probe_path, deadline);
    let _ = child.kill();
    let _ = child.wait();
    if let Some(s) = scratch {
        let _ = fs::remove_dir_all(s);
    }

    let (status, body) = probe.unwrap_or_else(|e| {
        panic!("{} web_server probe failed: {e}", tier.label());
    });
    assert_eq!(
        status,
        200,
        "{} web_server returned status {status}, body={body:?}",
        tier.label(),
    );
    assert!(
        !body.is_empty(),
        "{} web_server returned empty body",
        tier.label(),
    );
}

fn http_probe(addr: &str, path: &str, deadline: Instant) -> Result<(u16, String), String> {
    use std::io::{BufRead, BufReader, Read};
    use std::net::TcpStream;
    let connect_deadline = deadline.min(Instant::now() + Duration::from_secs(5));
    let mut stream = loop {
        match TcpStream::connect(addr) {
            Ok(s) => break s,
            Err(e) => {
                if Instant::now() >= connect_deadline {
                    return Err(format!("connect to {addr}: {e}"));
                }
                std::thread::sleep(Duration::from_millis(80));
            }
        }
    };
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| e.to_string())?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reader = BufReader::new(stream);
    let mut status_line = String::new();
    reader
        .read_line(&mut status_line)
        .map_err(|e| e.to_string())?;
    let parts: Vec<&str> = status_line.split_whitespace().collect();
    if parts.len() < 2 || !parts[0].starts_with("HTTP/") {
        return Err(format!("malformed status line: {status_line:?}"));
    }
    let code = parts[1].parse::<u16>().map_err(|e| e.to_string())?;
    let mut body = Vec::new();
    let _ = reader.read_to_end(&mut body);
    Ok((code, String::from_utf8_lossy(&body).into_owned()))
}

// ----------------------------------------------------------------
// LLVM strict-fallback gate.
//
// `gos build --release` silently routes a body to Cranelift if
// LLVM's lowerer raises `BuildError::Unsupported`. That fallback
// hides LLVM lowering gaps. With `GOSSAMER_FAIL_ON_LLVM_FALLBACK=1`
// the per-function fallback turns into a hard error, so this test
// fails the moment any example body cannot be lowered to LLVM
// directly. The list of currently-failing programs is captured in
// `~/dev/contexts/lang/ai_driven_gaps.md` and tracked one by one.
// ----------------------------------------------------------------

#[test]
fn llvm_release_lowers_every_example_without_fallback() {
    let mut fallbacks: Vec<String> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    for spec in SPECS {
        if spec.skip_all.is_some() {
            continue;
        }
        let src = workspace_root().join(spec.path);
        let scratch = fresh_dir(&format!("strict-{}", file_tag(spec.path)));
        let out = Command::new(gos_bin())
            .arg("build")
            .arg("--release")
            .arg("--out-dir")
            .arg(&scratch)
            .arg(&src)
            .env("GOSSAMER_FAIL_ON_LLVM_FALLBACK", "1")
            .output()
            .expect("spawn gos build --release");
        let _ = fs::remove_dir_all(&scratch);
        if out.status.success() {
            continue;
        }
        let stderr = String::from_utf8_lossy(&out.stderr);
        if stderr.contains("would fall back to Cranelift") {
            // First line typically reads:
            //   error: llvm backend: `<fn>` would fall back to Cranelift (<reason>) ...
            let summary = stderr
                .lines()
                .find(|l| l.contains("would fall back"))
                .unwrap_or(&stderr)
                .trim()
                .to_string();
            fallbacks.push(format!("{}: {summary}", spec.path));
        } else {
            errors.push(format!(
                "{}: gos build --release failed: {stderr}",
                spec.path
            ));
        }
    }
    if !fallbacks.is_empty() || !errors.is_empty() {
        let mut report = String::new();
        if !fallbacks.is_empty() {
            report.push_str(&format!(
                "{} LLVM fallback site(s) — see ai_driven_gaps.md for the open list:\n",
                fallbacks.len(),
            ));
            for f in &fallbacks {
                report.push_str("  ");
                report.push_str(f);
                report.push('\n');
            }
        }
        if !errors.is_empty() {
            report.push_str(&format!("\n{} build error(s):\n", errors.len()));
            for e in &errors {
                report.push_str("  ");
                report.push_str(e);
                report.push('\n');
            }
        }
        panic!("{report}");
    }
}
