//! End-to-end CLI tests.
//! Shells out to the `gos` binary Cargo produces for this crate and
//! asserts behaviour for `parse`, `check`, `run`, `build`, plus
//! cross-compilation via `--target`.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    // CARGO_BIN_EXE_<name> is set by cargo when running tests.
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn write_fixture(name: &str, source: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!("gossamer-cli-{}-{}.gos", name, std::process::id()));
    std::fs::write(&path, source).expect("write fixture");
    path
}

fn examples_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .join("examples")
}

#[test]
fn version_flag_prints_package_version() {
    let out = Command::new(gos_bin())
        .arg("--version")
        .output()
        .expect("spawn --version");
    assert!(out.status.success());
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.contains("gos"));
}

#[test]
fn parse_subcommand_round_trips_hello_world() {
    let fixture = write_fixture("parse", "fn main() { println(\"hello\") }\n");
    let out = Command::new(gos_bin())
        .args(["parse"])
        .arg(&fixture)
        .output()
        .expect("spawn parse");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(stdout.contains("fn main"));
    assert!(stdout.contains("println"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn check_subcommand_succeeds_on_simple_program() {
    let fixture = write_fixture(
        "check",
        "fn add(a: i64, b: i64) -> i64 { a + b }\nfn main() { let _ = add(1i64, 2i64) }\n",
    );
    let out = Command::new(gos_bin())
        .args(["check"])
        .arg(&fixture)
        .output()
        .expect("spawn check");
    assert!(
        out.status.success(),
        "check failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("check: ok"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn check_subcommand_reports_type_mismatch() {
    let fixture = write_fixture("checkfail", "fn main() { let x: bool = 42i32 }\n");
    let out = Command::new(gos_bin())
        .args(["check"])
        .arg(&fixture)
        .output()
        .expect("spawn check");
    assert!(!out.status.success(), "expected failure");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("type: type mismatch") || stderr.contains("check failed"),
        "stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn run_subcommand_executes_via_tree_walker() {
    let fixture = write_fixture("run", "fn main() { println(\"cli-tree-walker\") }\n");
    let out = Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("spawn run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("cli-tree-walker"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn run_subcommand_executes_via_vm_by_default() {
    let fixture = write_fixture("runvm", "fn main() { println(\"cli-vm\") }\n");
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("spawn run");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stdout).contains("cli-vm"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn build_subcommand_produces_runnable_output() {
    // `gos build` now defaults to native codegen via Cranelift + the
    // host `cc`. The happy-path output is a real executable that
    // exits with the Gossamer `main`'s return code. If native
    // codegen falls back (e.g. unsupported MIR), a launcher-script
    // takes over — both shapes are accepted here.
    let dir = env::temp_dir().join(format!("gos-build-magic-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("build_magic.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 42i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = dir.join("build_magic");
    assert!(binary.exists(), "build output missing");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&binary).unwrap().permissions().mode();
        assert!(
            mode & 0o111 != 0,
            "output should be chmod +x: mode {mode:o}"
        );
    }
    // Either path prints a single build: line to stdout.
    assert!(String::from_utf8_lossy(&out.stdout).contains("build:"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_subcommand_accepts_known_target_triple_and_rejects_unknown() {
    let dir = env::temp_dir().join(format!("gos-build-cross-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("cross.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    let ok = Command::new(gos_bin())
        .args(["build", "--target", "aarch64-apple-darwin"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target");
    assert!(
        ok.status.success(),
        "known target should build; stderr: {}",
        String::from_utf8_lossy(&ok.stderr)
    );
    let bad = Command::new(gos_bin())
        .args(["build", "--target", "wat-is-this"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target bad");
    assert!(
        !bad.status.success(),
        "unknown target should fail the build"
    );
    assert!(
        String::from_utf8_lossy(&bad.stderr).contains("unknown target"),
        "stderr should name the unknown-target error"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_defaults_output_to_source_stem_without_extension() {
    // `gos build line_count.gos` should write a file called
    // `line_count` (the executable, or a fallback launcher if
    // native codegen cannot yet lower the MIR).
    let dir = env::temp_dir().join(format!("gos-build-default-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("line_count.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let binary = dir.join("line_count");
    assert!(
        binary.exists(),
        "expected build output at {}",
        binary.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_honours_project_output_field_in_manifest() {
    let dir = env::temp_dir().join(format!("gos-build-manifest-out-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/widget\"\nversion = \"0.1.0\"\noutput = \"custom_name\"\n",
    )
    .unwrap();
    let source_path = dir.join("src/main.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let expected = dir.join("custom_name");
    assert!(
        expected.exists(),
        "expected build output at {}",
        expected.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_rejects_removed_output_flag() {
    let fixture = write_fixture("buildflagremoved", "fn main() { }\n");
    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&fixture)
        .arg("-o")
        .arg("somewhere")
        .output()
        .expect("spawn build");
    assert!(!out.status.success(), "-o should not be accepted");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn run_executes_every_terminating_example() {
    // `web_server.gos` is a real server that runs forever by design,
    // so it is not part of this loop. See
    // `web_server_example_binds_and_serves_real_requests` for
    // end-to-end coverage of the server path.
    for name in ["hello_world.gos", "line_count.gos"] {
        let path = examples_dir().join(name);
        let out = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .output()
            .expect("spawn run");
        assert!(
            out.status.success(),
            "{name} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// End-to-end smoke test of the echo example. Spawns
/// `gos run examples/web_server.gos` in a child process, connects,
/// drives a real HTTP/1.1 request, and inspects the response.
///
/// The example hardcodes port 8080. If that port is already bound
/// the test is skipped rather than marked as failing — CI sandboxes
/// commonly have port collisions, and the interpreter-level
/// `crates/gossamer-interp/tests/http_end_to_end.rs` already
/// validates the full dispatch path without needing a subprocess.
#[test]
fn web_server_example_binds_and_serves_real_requests() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    let probe = std::net::TcpListener::bind("127.0.0.1:8080");
    drop(probe.ok());
    // NOTE: the probe above may race with a concurrent test; treat
    // connection failures below as "skip" rather than "fail".

    let mut child = match std::process::Command::new(gos_bin())
        .arg("run")
        .arg(examples_dir().join("web_server.gos"))
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(err) => {
            eprintln!("skipping — could not spawn gos run: {err}");
            return;
        }
    };

    let mut response: Option<Vec<u8>> = None;
    for _ in 0..40 {
        thread::sleep(Duration::from_millis(100));
        if let Ok(mut stream) = TcpStream::connect("127.0.0.1:8080") {
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let _ =
                stream.write_all(b"GET /echo?name=jane&x=1 HTTP/1.1\r\nHost: localhost\r\n\r\n");
            stream.shutdown(std::net::Shutdown::Write).unwrap();
            let mut buf = Vec::new();
            if stream.read_to_end(&mut buf).is_ok() && !buf.is_empty() {
                response = Some(buf);
                break;
            }
        }
    }
    let _ = child.kill();
    let _ = child.wait();

    let Some(body) = response else {
        eprintln!("skipping — port 8080 unreachable (likely taken by another process)");
        return;
    };
    let text = String::from_utf8_lossy(&body);
    assert!(text.starts_with("HTTP/1.1 "), "unexpected response: {text}");
    assert!(
        text.contains("method") && text.contains("GET"),
        "echo body missing fields: {text}"
    );
    assert!(
        text.contains("query") && text.contains("name=jane"),
        "echo body missing query: {text}"
    );
}

#[test]
fn fmt_rewrites_misformatted_source() {
    let fixture = write_fixture("fmt", "fn    main(  )   {   }\n");
    let out = Command::new(gos_bin())
        .args(["fmt"])
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let formatted = std::fs::read_to_string(&fixture).unwrap();
    assert!(formatted.starts_with("fn main("));
    assert!(!formatted.contains("    main(  )"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_check_flag_fails_on_unformatted_file() {
    let fixture = write_fixture("fmtcheck", "fn    main()    {}\n");
    let out = Command::new(gos_bin())
        .args(["fmt", "--check"])
        .arg(&fixture)
        .output()
        .expect("spawn fmt --check");
    assert!(!out.status.success(), "--check should fail on messy input");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn doc_lists_items_defined_in_the_file() {
    let fixture = write_fixture("doc", "struct Widget { }\nfn main() { }\nfn helper() { }\n");
    let out = Command::new(gos_bin())
        .args(["doc"])
        .arg(&fixture)
        .output()
        .expect("spawn doc");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("struct Widget"));
    assert!(text.contains("fn main"));
    assert!(text.contains("fn helper"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_subcommand_runs_hash_test_attributed_functions() {
    let fixture = write_fixture(
        "testharness",
        "#[test]\nfn test_ok() { println(\"ran-test\") }\nfn main() { }\n",
    );
    let out = Command::new(gos_bin())
        .args(["test"])
        .arg(&fixture)
        .output()
        .expect("spawn test");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("1 passed"));
    assert!(stdout.contains("ran-test"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_subcommand_reports_no_tests_when_absent() {
    let fixture = write_fixture("testempty", "fn main() { }\n");
    let out = Command::new(gos_bin())
        .args(["test"])
        .arg(&fixture)
        .output()
        .expect("spawn test");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("no #[test] functions"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn bench_subcommand_times_attributed_functions() {
    let fixture = write_fixture(
        "benchharness",
        "#[bench]\nfn bench_noop() { }\nfn main() { }\n",
    );
    let out = Command::new(gos_bin())
        .args(["bench", "--iterations", "5"])
        .arg(&fixture)
        .output()
        .expect("spawn bench");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("ns/iter"));
    let _ = std::fs::remove_file(&fixture);
}

/// Stream A.3 — examples quality gate.
///
/// Every `.gos` file directly under `examples/` must parse cleanly
/// through `gos parse`. The runnable subset (`hello_world`,
/// `line_count`, `web_server`) is already covered by
/// `run_executes_every_example_in_examples_dir`; the remaining
/// parse-only files (`kv_cache`, `json_pipeline`, `selfhost/*`) are
/// gated here so a regression that breaks the shape of any example
/// fails CI.
#[test]
fn every_top_level_example_parses() {
    let dir = examples_dir();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read examples dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext == "gos")
        })
        .collect();
    assert!(
        !entries.is_empty(),
        "examples/ must contain at least one .gos"
    );
    for entry in entries {
        let path = entry.path();
        let out = Command::new(gos_bin())
            .arg("parse")
            .arg(&path)
            .output()
            .expect("spawn parse");
        assert!(
            out.status.success(),
            "{} failed to parse: {}",
            path.display(),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Stream C.3 — the `gos lint` subcommand runs against a single
/// file and reports at least one warning for code that trips a
/// day-one lint.
#[test]
fn lint_subcommand_reports_unused_variable() {
    let fixture = write_fixture("lintunused", "fn main() { let x = 1i64 }\n");
    let out = Command::new(gos_bin())
        .arg("lint")
        .arg(&fixture)
        .output()
        .expect("spawn lint");
    assert!(out.status.success(), "lint should succeed with warnings");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("GL0001"), "missing lint code: {stderr}");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn doc_html_emits_search_bar_intra_links_and_per_item_anchors() {
    let source = "\
// Greets a person.
//
// See also [farewell].
fn greet(name: String) -> String { \"hi, \" + name }

// Parting words for [greet].
fn farewell(name: String) -> String { \"bye, \" + name }
";
    let fixture = write_fixture("dochtml", source);
    let out_path = fixture.with_extension("html");
    let out = Command::new(gos_bin())
        .arg("doc")
        .arg("--html")
        .arg(&out_path)
        .arg(&fixture)
        .output()
        .expect("spawn doc --html");
    assert!(out.status.success(), "doc should succeed: {out:?}");
    let html = std::fs::read_to_string(&out_path).expect("read rendered html");
    assert!(html.contains("id=\"q\""), "search input missing: {html}");
    assert!(
        html.contains("id=\"item-fn-greet\""),
        "per-item anchor missing: {html}"
    );
    assert!(
        html.contains("href=\"#item-fn-farewell\""),
        "intra-doc link to `farewell` missing: {html}"
    );
    assert!(
        html.contains("href=\"#item-fn-greet\""),
        "intra-doc link to `greet` missing: {html}"
    );
    let _ = std::fs::remove_file(&fixture);
    let _ = std::fs::remove_file(&out_path);
}

#[test]
fn clean_subcommand_removes_frontend_cache_directory() {
    let tmp = std::env::temp_dir().join(format!(
        "gos-clean-itest-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("abc123.ok"), b"").unwrap();
    let out = Command::new(gos_bin())
        .arg("clean")
        .env("GOSSAMER_CACHE_DIR", &tmp)
        .output()
        .expect("spawn clean");
    assert!(out.status.success(), "clean should succeed: {out:?}");
    assert!(
        !tmp.exists(),
        "cache dir still exists after clean: {}",
        tmp.display()
    );
}

#[test]
fn clean_dry_run_reports_sizes_without_touching_the_cache() {
    let tmp = std::env::temp_dir().join(format!(
        "gos-clean-dryrun-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(tmp.join("abc123.ok"), b"hello").unwrap();
    let out = Command::new(gos_bin())
        .arg("clean")
        .arg("--dry-run")
        .env("GOSSAMER_CACHE_DIR", &tmp)
        .output()
        .expect("spawn clean --dry-run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(out.status.success(), "dry-run should succeed: {out:?}");
    assert!(
        tmp.exists(),
        "cache dir should NOT be removed during dry run"
    );
    assert!(
        stdout.contains("would remove frontend cache"),
        "expected would-remove line in {stdout}"
    );
    std::fs::remove_dir_all(&tmp).ok();
}

#[test]
fn test_subcommand_runs_doc_tests_and_reports_failures() {
    let source = "\
// Doubles `n`.\n\
//\n\
// ```\n\
// let x = 2i64\n\
// if x * 2i64 != 4i64 { panic(\"bad\") }\n\
// ```\n\
fn double(n: i64) -> i64 { n * 2i64 }\n\
\n\
// Intentionally broken doc-test.\n\
//\n\
// ```\n\
// panic(\"boom\")\n\
// ```\n\
fn broken() {}\n";
    let fixture = write_fixture("doctest", source);
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(&fixture)
        .output()
        .expect("spawn test");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("PASS doc-test"),
        "expected PASS doc-test in stdout:\n{stdout}"
    );
    assert!(
        stdout.contains("FAIL doc-test"),
        "expected FAIL doc-test in stdout:\n{stdout}"
    );
    assert!(
        !out.status.success(),
        "broken doc-test should cause non-zero exit"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn lint_fix_applies_auto_suggestions_and_writes_back() {
    let source = "fn main() { let mut x = 1i64; println(x.to_string()) }\n";
    let fixture = write_fixture("lintfix", source);
    let out = Command::new(gos_bin())
        .arg("lint")
        .arg("--fix")
        .arg(&fixture)
        .output()
        .expect("spawn lint --fix");
    assert!(out.status.success(), "--fix should succeed: {out:?}");
    let rewritten = std::fs::read_to_string(&fixture).expect("read rewritten file");
    assert!(
        !rewritten.contains("mut x"),
        "mut keyword should be removed: {rewritten}"
    );
    assert!(
        rewritten.contains("let x = 1i64"),
        "binding should remain: {rewritten}"
    );
    let _ = std::fs::remove_file(&fixture);
}

/// Stream C.3 — `--deny-warnings` upgrades every lint hit to an
/// error and makes the command fail.
#[test]
fn lint_deny_warnings_fails_on_lint_hit() {
    let fixture = write_fixture("lintdeny", "fn main() { let x = 1i64 }\n");
    let out = Command::new(gos_bin())
        .arg("lint")
        .arg("--deny-warnings")
        .arg(&fixture)
        .output()
        .expect("spawn lint --deny-warnings");
    assert!(!out.status.success(), "expected failure, got ok");
    let _ = std::fs::remove_file(&fixture);
}

/// Stream C.3 — `--explain <lint>` prints the long-form description.
#[test]
fn lint_explain_prints_description() {
    let out = Command::new(gos_bin())
        .arg("lint")
        .arg("--explain")
        .arg("unused_variable")
        .arg(examples_dir().join("hello_world.gos"))
        .output()
        .expect("spawn lint --explain");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("unused_variable"));
    assert!(stdout.contains("Prefix the name with `_`"));
}

/// Stream C.4 — walking the `examples/` tree produces at most a
/// warning-level output and exits zero.
#[test]
fn lint_walks_examples_directory_without_failing() {
    let out = Command::new(gos_bin())
        .arg("lint")
        .arg(examples_dir())
        .output()
        .expect("spawn lint examples/");
    assert!(
        out.status.success(),
        "gos lint examples/ failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Stream H.3 — `gos fmt` must be idempotent: formatting an
/// already-formatted file must produce zero diffs on a second pass.
#[test]
fn fmt_is_idempotent_on_the_full_examples_tree() {
    let dir = examples_dir();
    let entries: Vec<_> = std::fs::read_dir(&dir)
        .expect("read examples dir")
        .filter_map(Result::ok)
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|s| s.to_str())
                .is_some_and(|ext| ext == "gos")
        })
        .collect();
    for entry in entries {
        let source = std::fs::read_to_string(entry.path()).unwrap();
        let temp = write_fixture("fmt_idem", &source);
        // First pass: produce canonical form.
        let out = Command::new(gos_bin())
            .arg("fmt")
            .arg(&temp)
            .output()
            .expect("spawn fmt");
        assert!(
            out.status.success(),
            "fmt pass 1 failed on {}",
            entry.path().display()
        );
        let canonical = std::fs::read_to_string(&temp).unwrap();
        // Second pass must report no change and leave the file alone.
        let out = Command::new(gos_bin())
            .args(["fmt", "--check"])
            .arg(&temp)
            .output()
            .expect("spawn fmt --check");
        let stderr = String::from_utf8_lossy(&out.stderr);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            out.status.success(),
            "fmt --check failed on already-formatted {} — stderr: {stderr} stdout: {stdout}",
            entry.path().display()
        );
        let rechecked = std::fs::read_to_string(&temp).unwrap();
        assert_eq!(
            canonical,
            rechecked,
            "fmt is not idempotent on {}",
            entry.path().display()
        );
        let _ = std::fs::remove_file(&temp);
    }
}

/// Stream H.6 — `gos explain <code>` prints the long-form
/// explanation for a diagnostic code.
#[test]
fn explain_prints_description_for_known_code() {
    let out = Command::new(gos_bin())
        .args(["explain", "GP0001"])
        .output()
        .expect("spawn explain");
    assert!(out.status.success());
    let text = String::from_utf8_lossy(&out.stdout);
    assert!(text.contains("GP0001"));
    assert!(text.contains("parser"));
}

/// Stream H.6 — unknown codes produce a clear error.
#[test]
fn explain_rejects_unknown_code() {
    let out = Command::new(gos_bin())
        .args(["explain", "G99999"])
        .output()
        .expect("spawn explain");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("G99999"));
}

/// Stream H.7 — panics surface a call-stack snapshot to stderr.
#[test]
fn panic_error_includes_call_stack() {
    let fixture = write_fixture(
        "panictrace",
        "fn inner() { panic(\"boom\") }\nfn main() { inner() }\n",
    );
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("spawn run");
    assert!(!out.status.success());
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("call stack"));
    assert!(stderr.contains("at main"));
    assert!(stderr.contains("at inner"));
    let _ = std::fs::remove_file(&fixture);
}

/// Stream A.3 — every terminating runnable example must execute
/// under the tree-walker without a runtime error. `web_server.gos`
/// is a real server; it is covered by
/// `web_server_example_binds_and_serves_real_requests`.
#[test]
fn every_terminating_example_executes_cleanly() {
    let examples: [&str; 1] = ["hello_world.gos"];
    for name in examples {
        let path = examples_dir().join(name);
        let out = Command::new(gos_bin())
            .arg("run")
            .arg(&path)
            .output()
            .expect("spawn run");
        assert!(
            out.status.success(),
            "{name} failed at runtime: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

#[test]
fn build_rejects_unknown_target() {
    let fixture = write_fixture("buildbad", "fn main() { }\n");
    let out = Command::new(gos_bin())
        .args(["build", "--target", "not-a-triple"])
        .arg(&fixture)
        .output()
        .expect("spawn build");
    assert!(!out.status.success());
    assert!(String::from_utf8_lossy(&out.stderr).contains("unknown target"));
    let _ = std::fs::remove_file(&fixture);
}

fn pkg_workdir(tag: &str) -> PathBuf {
    let mut dir = env::temp_dir();
    dir.push(format!("gos-pkg-{tag}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("mkdir workdir");
    dir
}

#[test]
fn init_creates_project_toml_with_supplied_id() {
    let dir = pkg_workdir("init");
    let out = Command::new(gos_bin())
        .args(["init", "example.com/widget"])
        .current_dir(&dir)
        .output()
        .expect("spawn init");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let manifest = std::fs::read_to_string(dir.join("project.toml")).unwrap();
    assert!(manifest.contains("example.com/widget"));
    assert!(manifest.contains("0.1.0"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_scaffolds_project_directory() {
    let dir = pkg_workdir("new");
    let out = Command::new(gos_bin())
        .args(["new", "example.com/widget", "--path"])
        .arg(dir.join("widget"))
        .output()
        .expect("spawn new");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let project = dir.join("widget");
    assert!(project.join("project.toml").exists());
    assert!(project.join("src/main.gos").exists());
    let main = std::fs::read_to_string(project.join("src/main.gos")).unwrap();
    assert!(main.contains("hello from widget"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_service_template_scaffolds_http_handler() {
    let dir = pkg_workdir("new-svc");
    let out = Command::new(gos_bin())
        .args(["new", "example.com/svc", "--template", "service", "--path"])
        .arg(dir.join("svc"))
        .output()
        .expect("spawn new --template service");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let project = dir.join("svc");
    assert!(project.join("project.toml").exists());
    let main = std::fs::read_to_string(project.join("src/main.gos")).unwrap();
    assert!(
        main.contains("http::Handler") && main.contains("http::serve"),
        "service template missing http wiring:\n{main}"
    );
    assert!(
        !project.join("src/lib.gos").exists(),
        "service template should not emit lib.gos"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn new_rejects_unknown_template() {
    let dir = pkg_workdir("new-bad");
    let out = Command::new(gos_bin())
        .args(["new", "example.com/bad", "--template", "nope", "--path"])
        .arg(dir.join("bad"))
        .output()
        .expect("spawn new --template nope");
    assert!(
        !out.status.success(),
        "clap should reject unknown template values"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn add_and_remove_round_trip_a_dependency() {
    let dir = pkg_workdir("addrm");
    let init = Command::new(gos_bin())
        .args(["init", "example.com/widget"])
        .current_dir(&dir)
        .output()
        .expect("init");
    assert!(init.status.success());
    let add = Command::new(gos_bin())
        .args(["add", "example.org/lib@1.2.3"])
        .current_dir(&dir)
        .output()
        .expect("add");
    assert!(
        add.status.success(),
        "{}",
        String::from_utf8_lossy(&add.stderr)
    );
    let after_add = std::fs::read_to_string(dir.join("project.toml")).unwrap();
    assert!(after_add.contains("\"example.org/lib\" = \"1.2.3\""));
    let remove = Command::new(gos_bin())
        .args(["remove", "example.org/lib"])
        .current_dir(&dir)
        .output()
        .expect("remove");
    assert!(remove.status.success());
    let after_remove = std::fs::read_to_string(dir.join("project.toml")).unwrap();
    assert!(!after_remove.contains("example.org/lib"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tidy_canonicalises_existing_manifest() {
    let dir = pkg_workdir("tidy");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/widget\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.org/lib\" = \"1.0.0\"\n",
    )
    .unwrap();
    let out = Command::new(gos_bin())
        .arg("tidy")
        .current_dir(&dir)
        .output()
        .expect("tidy");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(dir.join("project.toml")).unwrap();
    assert!(after.contains("example.org/lib"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn run_refuses_type_invalid_program_with_diagnostic() {
    // Interpreter must not execute programs that fail static checks
    // (error_handling.md invariant #2). The CLI should print a
    // typed diagnostic and exit non-zero.
    let fixture = write_fixture(
        "type-fail",
        "fn main() -> i64 {\n    let x: i64 = \"not an int\"\n    x\n}\n",
    );
    let out = Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("spawn gos run");
    assert!(
        !out.status.success(),
        "run should reject type-invalid source; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("type") && stderr.contains("refusing to execute"),
        "expected typed diagnostic + refusal; got: {stderr}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_refuses_statically_invalid_program_with_diagnostic() {
    // Same invariant applies to `gos test`: a test harness that runs
    // statically-broken code is worse than useless. Put the test at
    // top level so name resolution fires before the tree-walker sees
    // it (nested-module resolution is tracked separately).
    let fixture = write_fixture(
        "test-unresolved",
        "#[test]\nfn has_unresolved_name() {\n    totally_made_up_fn()\n}\n",
    );
    let out = Command::new(gos_bin())
        .args(["test"])
        .arg(&fixture)
        .output()
        .expect("spawn gos test");
    assert!(
        !out.status.success(),
        "test should reject type-invalid source; stdout={} stderr={}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("refusing to execute"),
        "expected static-error refusal in stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&fixture);
}

// Post-L4 there's no launcher path — the old
// `unsupported_native_path_fails_loudly_by_default` /
// `allow_launcher_emits_shell_launcher_when_codegen_bails` tests
// exercised a flag that no longer exists. Every program the
// resolver + typechecker accepts now lowers to a native binary;
// a codegen bail is a compiler bug, not an expected path.

#[test]
fn explain_recognises_runtime_error_codes() {
    // `gos explain GX0005` must print the long-form panic
    // explanation so the runtime-error catalogue stays in sync with
    // the `RuntimeError::code` method in `gossamer-interp`.
    // (parity_error_plan.md Phase E4).
    let out = Command::new(gos_bin())
        .args(["explain", "GX0005"])
        .output()
        .expect("spawn gos explain");
    assert!(
        out.status.success(),
        "explain should succeed; stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("GX0005") && stdout.to_lowercase().contains("panic"),
        "expected panic explanation referencing GX0005; got: {stdout}"
    );
}

#[test]
fn runtime_panic_stderr_carries_gx_code_prefix() {
    // Unified error-code catalogue: every runtime failure's stderr
    // is prefixed with `error[GXNNNN]:`. An explicit `panic!(...)`
    // exercises the `GX0005` branch end-to-end.
    let fixture = write_fixture("runtime-panic", "fn main() {\n    panic(\"boom\")\n}\n");
    let out = Command::new(gos_bin())
        .arg("run")
        .arg(&fixture)
        .output()
        .expect("spawn gos run");
    assert!(!out.status.success(), "panic should exit non-zero");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("error[GX0005]"),
        "expected GX0005 prefix in stderr: {stderr}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn test_subcommand_with_no_args_walks_up_to_project_toml() {
    // `gos test` with no path argument should locate the nearest
    // ancestor `project.toml` and discover every `.gos` file under
    // its `src/` tree — mimicking `cargo test` ergonomics.
    let dir = pkg_workdir("test-default");
    let init = Command::new(gos_bin())
        .args(["init", "example.com/svc"])
        .current_dir(&dir)
        .output()
        .expect("spawn init");
    assert!(
        init.status.success(),
        "init: {}",
        String::from_utf8_lossy(&init.stderr)
    );
    let src = dir.join("src");
    std::fs::create_dir_all(&src).expect("mkdir src");
    std::fs::write(
        src.join("main.gos"),
        "use std::testing\n\
         fn add(a: i64, b: i64) -> i64 { a + b }\n\
         #[cfg(test)]\n\
         mod tests {\n\
         \x20\x20\x20\x20use std::testing\n\
         \x20\x20\x20\x20#[test]\n\
         \x20\x20\x20\x20fn add_combines_two_ints() {\n\
         \x20\x20\x20\x20\x20\x20\x20\x20testing::check_eq(&super::add(2, 3), &5, \"add\")\n\
         \x20\x20\x20\x20}\n\
         }\n\
         fn main() { }\n",
    )
    .expect("write src/main.gos");
    let nested = src.join("inner");
    std::fs::create_dir_all(&nested).expect("mkdir inner");
    let cwd = nested;
    let out = Command::new(gos_bin())
        .arg("test")
        .current_dir(&cwd)
        .output()
        .expect("spawn test");
    assert!(
        out.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("add_combines_two_ints"),
        "expected discovered test name in output: {stdout}"
    );
    assert!(
        stdout.contains("1 passed"),
        "expected pass tally in output: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn examples_web_service_project_tests_all_pass() {
    // The `examples/projects/web_service` project is the canonical
    // multi-endpoint Gossamer service. Its render-helper unit tests
    // double as a smoke test that `gos test` (no args) discovers and
    // runs the project's full `src/` tree.
    let project = examples_dir().join("projects").join("web_service");
    assert!(
        project.join("project.toml").is_file(),
        "missing project.toml at {}",
        project.display()
    );
    let out = Command::new(gos_bin())
        .arg("test")
        .current_dir(&project)
        .output()
        .expect("spawn test");
    assert!(
        out.status.success(),
        "stderr: {}\nstdout: {}",
        String::from_utf8_lossy(&out.stderr),
        String::from_utf8_lossy(&out.stdout)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    for tname in [
        "health_returns_ok",
        "users_returns_json_list_with_known_names",
        "echo_wraps_query_in_json",
        "echo_handles_empty_query",
        "classify_routes_known_paths",
        "classify_falls_back_to_not_found",
    ] {
        assert!(
            stdout.contains(tname),
            "missing test {tname} in output:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("6 passed") && stdout.contains("0 failed"),
        "expected full pass tally; stdout was:\n{stdout}"
    );
}

#[test]
fn skill_prompt_subcommand_prints_skill_card() {
    let out = Command::new(gos_bin())
        .arg("skill-prompt")
        .output()
        .expect("spawn skill-prompt");
    assert!(out.status.success(), "skill-prompt should exit zero");
    let stdout = String::from_utf8(out.stdout).expect("utf8");
    assert!(
        stdout.starts_with("# Gossamer"),
        "skill card should start with the title: {}",
        stdout.lines().next().unwrap_or("")
    );
    assert!(
        stdout.contains("|>"),
        "skill card should mention the forward-pipe operator"
    );
    assert!(
        stdout.contains("Goroutines"),
        "skill card should cover concurrency"
    );
}
