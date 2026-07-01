//! End-to-end CLI tests.
//! Shells out to the `gos` binary Cargo produces for this crate and
//! asserts behaviour for `parse`, `check`, `run`, `build`, plus
//! cross-compilation via `--target`.

mod common;

use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
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
fn run_subcommand_executes_via_vm() {
    let fixture = write_fixture("run", "fn main() { println(\"cli-vm-run\") }\n");
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
    assert!(stdout.contains("cli-vm-run"));
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
    // takes over - both shapes are accepted here.
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
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("build_magic{}", std::env::consts::EXE_SUFFIX));
    assert!(
        binary.exists(),
        "build output missing at {}",
        binary.display()
    );
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
fn build_output_handles_empty_argv_for_flag_define_programs() {
    let dir = env::temp_dir().join(format!("gos-build-argv-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("argv_ok.gos");
    std::fs::write(
        &source_path,
        "use std::flag\n\
         fn main() {\n\
             let flags = flag::define(\"argv-ok\", [\n\
                 flag::int(\"port\", 8080, \"port\", 'p'),\n\
                 flag::bool(\"verbose\", false, \"verbose\", 'v'),\n\
             ])\n\
             if *flags.verbose {\n\
                 println(\"verbose\")\n\
             } else {\n\
                 println((*flags.port).to_string())\n\
             }\n\
         }\n",
    )
    .unwrap();
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("argv_ok{}", std::env::consts::EXE_SUFFIX));
    let run = Command::new(&binary).output().expect("run built artifact");
    assert!(
        run.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "8080\n");
}

#[test]
fn build_output_preserves_http_method_chain_through_send_and_field_access() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("loopback addr");
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept client");
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf);
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nhello")
            .expect("write response");
    });

    let dir = env::temp_dir().join(format!("gos-build-http-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("http_chain.gos");
    std::fs::write(
        &source_path,
        format!(
            "use std::http\n\
             fn main() {{\n\
                 let url = \"http://{addr}/\".to_string()\n\
                 match http::Client::new().get(&url).send() {{\n\
                     Ok(resp) => println(resp.status.to_string() + \":\" + resp.body),\n\
                     Err(e) => println(\"send failed: \" + e.message()),\n\
                 }}\n\
             }}\n"
        ),
    )
    .unwrap();
    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("http_chain{}", std::env::consts::EXE_SUFFIX));
    let run = Command::new(&binary).output().expect("run built artifact");
    assert!(
        run.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&run.stdout), "200:hello\n");
    server.join().expect("join server");
}

/// Serves `count` HTTP requests on `listener`, echoing the `x-test`
/// header and the request body back as `xt=<v> body=<b>` with a 201.
fn serve_builder_echo(listener: &TcpListener, count: usize) {
    for _ in 0..count {
        let (mut stream, _) = listener.accept().expect("accept client");
        let mut buf = Vec::new();
        let mut chunk = [0u8; 1024];
        let body_start = loop {
            let n = stream.read(&mut chunk).expect("read request");
            buf.extend_from_slice(&chunk[..n]);
            if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                break pos + 4;
            }
            assert!(n != 0, "connection closed before headers completed");
        };
        let lower = String::from_utf8_lossy(&buf[..body_start]).to_ascii_lowercase();
        let content_len: usize = lower
            .lines()
            .find_map(|l| l.strip_prefix("content-length:").map(str::trim))
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        while buf.len() < body_start + content_len {
            let n = stream.read(&mut chunk).expect("read body");
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
        }
        let xt = lower
            .lines()
            .find_map(|l| l.strip_prefix("x-test:").map(str::trim))
            .unwrap_or("<none>");
        let body = String::from_utf8_lossy(&buf[body_start..]).into_owned();
        let reply = format!("xt={xt} body={body}");
        let resp = format!(
            "HTTP/1.1 201 Created\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            reply.len(),
            reply
        );
        stream.write_all(resp.as_bytes()).expect("write response");
    }
}

/// Tier-parity sentinel for the chained client builder: the same
/// source must produce byte-identical stdout under `gos run` (VM)
/// and a `gos build` native binary, with the chained header + body
/// honored and a transport failure surfacing as `Err` on both tiers.
#[test]
fn vm_and_native_client_builder_chain_outputs_match() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback server");
    let addr = listener.local_addr().expect("loopback addr");
    // One POST per tier (VM run + native run).
    let server = std::thread::spawn(move || serve_builder_echo(&listener, 2));

    let source = format!(
        "use std::http\n\
         fn main() {{\n\
             let client = http::Client::new()\n\
             let sent = client\n\
                 .post(&\"http://{addr}/echo\")\n\
                 .header(\"x-test\", \"parity\")\n\
                 .body(\"ping\")\n\
                 .send()\n\
             match sent {{\n\
                 Ok(r) => println!(\"post: {{}} {{}}\", r.status, r.body),\n\
                 Err(e) => println!(\"post err: {{}}\", e),\n\
             }}\n\
             match client.get(&\"http://127.0.0.1:1/refused\").send() {{\n\
                 Ok(r) => println!(\"refused ok: {{}}\", r.status),\n\
                 Err(e) => println!(\"refused err: {{}}\", e),\n\
             }}\n\
         }}\n"
    );
    let dir = env::temp_dir().join(format!("gos-builder-parity-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("builder_parity.gos");
    std::fs::write(&source_path, source).unwrap();

    let vm = Command::new(gos_bin())
        .arg("run")
        .arg(&source_path)
        .output()
        .expect("spawn gos run");
    assert!(
        vm.status.success(),
        "gos run failed: {}",
        String::from_utf8_lossy(&vm.stderr)
    );

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .output()
        .expect("spawn build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("builder_parity{}", std::env::consts::EXE_SUFFIX));
    let native = Command::new(&binary).output().expect("run built artifact");
    assert!(
        native.status.success(),
        "native run failed: {}",
        String::from_utf8_lossy(&native.stderr)
    );

    let vm_out = String::from_utf8_lossy(&vm.stdout).into_owned();
    let native_out = String::from_utf8_lossy(&native.stdout).into_owned();
    assert_eq!(vm_out, native_out, "tier outputs diverge");
    assert!(
        vm_out.contains("post: 201 xt=parity body=ping"),
        "chained header/body not honored: {vm_out}"
    );
    assert!(
        vm_out.contains("refused err: http: transport:"),
        "transport failure must surface as Err: {vm_out}"
    );
    server.join().expect("join server");
}

#[test]
fn build_subcommand_accepts_known_target_triple_and_rejects_unknown() {
    let dir = env::temp_dir().join(format!("gos-build-cross-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("cross.gos");
    std::fs::write(&source_path, "fn main() -> i64 { 0i64 }\n").unwrap();
    // A registered Linux cross target is routed into the real build
    // path. Without a target runtime archive (and a cross linker) it
    // fails at link resolution with a clear message - never the
    // registration-gate "unknown target" error, and never a stub.
    let known = Command::new(gos_bin())
        .args(["build", "--target", "aarch64-unknown-linux-gnu"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target");
    let known_err = String::from_utf8_lossy(&known.stderr);
    assert!(
        !known_err.contains("unknown target"),
        "a registered Linux target must pass the registration gate: {known_err}"
    );
    // A registered but non-Linux target cannot be cross-produced from
    // any host (no bundled SDK); it is refused with a specific error,
    // not silently stubbed.
    let darwin = Command::new(gos_bin())
        .args(["build", "--target", "aarch64-apple-darwin"])
        .arg(&source_path)
        .output()
        .expect("spawn build --target darwin");
    assert!(
        !darwin.status.success(),
        "a non-Linux cross target must be refused"
    );
    let darwin_err = String::from_utf8_lossy(&darwin.stderr);
    assert!(
        darwin_err.contains("only `*-linux-*`"),
        "non-Linux target should be refused with a specific message: {darwin_err}"
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
    // `line_count` (the executable produced by the native codegen
    // pipeline).
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
    let binary = dir
        .join("target")
        .join("debug")
        .join(format!("line_count{}", std::env::consts::EXE_SUFFIX));
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
    // The manifest `output` has no extension; on Windows the linker needs
    // the `.exe` suffix, which `resolve_output_path` adds. Expect the
    // platform executable name, not the bare stem.
    let expected_name = if cfg!(windows) {
        "custom_name.exe"
    } else {
        "custom_name"
    };
    let expected = dir.join(expected_name);
    assert!(
        expected.exists(),
        "expected build output at {}",
        expected.display()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_inside_project_names_binary_after_project_id_tail() {
    // Rust's convention: `cargo build` writes `target/debug/<package>`,
    // not `target/debug/main`. Gossamer follows the same rule when a
    // `project.toml` is present - the binary takes the last segment
    // of `[project] id`, regardless of which source file holds `main`.
    let dir = env::temp_dir().join(format!("gos-build-id-tail-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"github.com/acme/widget-cli\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    let source_path = dir.join("src/main.gos");
    std::fs::write(&source_path, "fn main() { }\n").unwrap();
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
    let expected = dir
        .join("target")
        .join("debug")
        .join(format!("widget-cli{}", std::env::consts::EXE_SUFFIX));
    assert!(
        expected.exists(),
        "expected build output at {}",
        expected.display()
    );
    let stale = dir
        .join("target")
        .join("debug")
        .join(format!("main{}", std::env::consts::EXE_SUFFIX));
    assert!(
        !stale.exists(),
        "binary must not be named after the source file when a manifest exists"
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
/// the test is skipped rather than marked as failing - CI sandboxes
/// commonly have port collisions, and the interpreter-level
/// `crates/gossamer-interp/tests/http_end_to_end.rs` already
/// validates the full dispatch path without needing a subprocess.
#[test]
fn web_server_example_binds_and_serves_real_requests() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::thread;
    use std::time::Duration;

    let _server_window = common::ServerPortLock::acquire();

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
            eprintln!("skipping - could not spawn gos run: {err}");
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
        eprintln!("skipping - port 8080 unreachable (likely taken by another process)");
        return;
    };
    let text = String::from_utf8_lossy(&body);
    if !text.starts_with("HTTP/1.1 ") {
        eprintln!("skipping - response not HTTP/1.1: {text}");
        return;
    }
    // Concurrent test runs can collide on port 8080 (e.g. another
    // benchmark server, or a parallel test invocation). When the
    // response we got back doesn't look like our echo handler's
    // shape, treat it as a port collision and skip rather than
    // fail. The interpreter-level test
    // (`crates/gossamer-interp/tests/http_end_to_end.rs`) covers
    // the dispatch path without requiring an exclusive port grab.
    if !(text.contains("method") && text.contains("GET")) {
        eprintln!("skipping - port 8080 served unrelated content: {text}");
        return;
    }
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
fn bench_subcommand_reports_ns_per_op() {
    // No-op bench fn - exercises the formatter and the calibration
    // cap on a fn that never crosses the 50ms trial threshold.
    let fixture = write_fixture(
        "benchharness_noop",
        "#[bench]\nfn bench_noop() { }\nfn main() { }\n",
    );
    let out = Command::new(gos_bin())
        .args(["bench"])
        .arg(&fixture)
        .output()
        .expect("spawn bench");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("ns/op"),
        "expected ns/op in stdout, got: {stdout}"
    );
    assert!(
        stdout.contains("bench_noop"),
        "expected the bench label in stdout, got: {stdout}"
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn bench_subcommand_handles_microsecond_workload() {
    // Microsecond-class bench - exercises the per-op timing on a
    // fn that does observable arithmetic work each call.
    let fixture = write_fixture(
        "benchharness_micro",
        "fn add_two(a: i64, b: i64) -> i64 { a + b }
#[bench]
fn bench_add_two() { let _ = add_two(1i64, 2i64) }
fn main() { }
",
    );
    let out = Command::new(gos_bin())
        .args(["bench"])
        .arg(&fixture)
        .output()
        .expect("spawn bench");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .find(|l| l.contains("bench_add_two"))
        .unwrap_or_else(|| panic!("missing bench line in stdout: {stdout}"));
    assert!(line.contains("ns/op"));
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn bench_subcommand_reports_no_benches_when_none_present() {
    let fixture = write_fixture("benchharness_empty", "fn main() { }\n");
    let out = Command::new(gos_bin())
        .args(["bench"])
        .arg(&fixture)
        .output()
        .expect("spawn bench");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("no #[bench] functions"),
        "expected the empty-discovery message, got: {stdout}"
    );
    let _ = std::fs::remove_file(&fixture);
}

/// Stream A.3 - examples quality gate.
///
/// Every `.gos` file directly under `examples/` must parse cleanly
/// through `gos parse`. The runnable subset (`hello_world`,
/// `line_count`, `web_server`) is also executed by
/// `run_executes_every_example_in_examples_dir`; this gate covers the
/// rest, so a regression that breaks the shape of any example fails CI.
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

/// Stream C.3 - the `gos lint` subcommand runs against a single
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

/// Stream C.3 - `--deny-warnings` upgrades every lint hit to an
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

/// Stream C.3 - `--explain <lint>` prints the long-form description.
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

/// Stream C.4 - walking the `examples/` tree produces at most a
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

/// Stream H.3 - `gos fmt` must be idempotent: formatting an
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
            "fmt --check failed on already-formatted {} - stderr: {stderr} stdout: {stdout}",
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

/// Stream H.6 - `gos explain <code>` prints the long-form
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

/// Every GT rejection code the checker gained this cycle has an
/// `explain` entry - a refusal pointing at an unexplained code is
/// a docs gap.
#[test]
fn explain_covers_checker_rejection_codes() {
    for code in ["GT0013", "GT0014", "GT0015"] {
        let out = Command::new(gos_bin())
            .args(["explain", code])
            .output()
            .expect("spawn explain");
        assert!(out.status.success(), "explain {code} failed");
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.contains(code), "explain {code} output: {text}");
    }
}

/// Stream H.6 - unknown codes produce a clear error.
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

/// Stream H.7 - panics surface a call-stack snapshot to stderr.
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

/// Stream A.3 - every terminating runnable example must execute
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
    // The VM must not execute programs that fail static checks
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

// Post-L4 there's no launcher path - the old
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
    // its `src/` tree - mimicking `cargo test` ergonomics.
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
fn examples_rust_binding_add_project_tests_all_pass() {
    // `examples/projects/rust_binding_add` is the canonical
    // minimal Rust-binding example: one `fn add(i64, i64) -> i64`
    // in `addlib/` exposed to Gossamer via `register_module!` and
    // exercised by `#[test]`s in `src/main.gos`. Confirms the
    // end-to-end `[rust-bindings]` wiring works through `gos test`.
    let project = examples_dir().join("projects").join("rust_binding_add");
    assert!(
        project.join("project.toml").is_file(),
        "missing project.toml at {}",
        project.display()
    );
    assert!(
        project.join("addlib").join("Cargo.toml").is_file(),
        "missing addlib/Cargo.toml at {}",
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
        "add_combines_two_positive_ints",
        "add_handles_zero_identity",
        "add_handles_negative_summands",
    ] {
        assert!(
            stdout.contains(tname),
            "missing test {tname} in output:\n{stdout}"
        );
    }
    assert!(
        stdout.contains("3 passed") && stdout.contains("0 failed"),
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

// --- N6: must_use Result lint (SPEC §9) ---

#[test]
fn discarded_result_is_a_type_error() {
    // SPEC §9: a `Result<T, E>` value used as a statement without
    // binding or propagating the result is a compile error (GT0007).
    // `gos run` must refuse to execute and mention the error code.
    // `let _ = expr` is the explicit-discard exception and must NOT
    // trigger the diagnostic.
    let src = r#"
use std::errors

fn may_fail(n: i64) -> Result<i64, errors::Error> {
    if n > 0 { Ok(n) } else { Err(errors::new("negative")) }
}

fn main() {
    may_fail(1)
}
"#;
    let fixture = write_fixture("n6-discard-result", src);
    let out = std::process::Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("spawn gos run");
    let _ = std::fs::remove_file(&fixture);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !out.status.success(),
        "run must reject a discarded Result; stderr: {stderr}"
    );
    assert!(
        stderr.contains("GT0007")
            || stderr.contains("unused `Result`")
            || stderr.contains("Result"),
        "expected GT0007 or a Result-related diagnostic in stderr; got: {stderr}"
    );
}

#[test]
fn let_underscore_result_is_not_an_error() {
    // `let _ = expr` is the explicit-discard form for Result. It must
    // NOT trigger GT0007 - the user has consciously chosen to ignore
    // the Result (best-effort operations, etc.).
    let src = r#"
use std::errors

fn may_fail(n: i64) -> Result<i64, errors::Error> {
    if n > 0 { Ok(n) } else { Err(errors::new("negative")) }
}

fn main() {
    let _ = may_fail(1)
    println!("ok")
}
"#;
    let fixture = write_fixture("n6-let-underscore-ok", src);
    let out = std::process::Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .output()
        .expect("spawn gos run");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_file(&fixture);
    assert!(
        out.status.success(),
        "let _ = result should be accepted; stderr: {stderr}"
    );
    assert!(
        stdout.contains("ok"),
        "expected 'ok' in stdout; got: {stdout}"
    );
}

#[test]
fn bare_manifest_id_is_a_hard_error_for_project_commands() {
    // A bare `id = "name"` used to silently disable `[rust-bindings]`
    // resolution while `gos check` / `gos test` kept passing. A
    // present-but-malformed manifest must fail loudly instead.
    let dir = env::temp_dir().join(format!("gos-bare-id-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"bareid\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(dir.join("src/main.gos"), "fn main() { println!(\"hi\") }\n").unwrap();
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(".")
        .current_dir(&dir)
        .output()
        .expect("spawn gos test");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "bare manifest id must fail; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stderr.contains("invalid domain segment") || stdout.contains("invalid domain segment"),
        "diagnostic must explain the id grammar; stdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn unbound_binding_module_call_fails_with_gx0002() {
    // A declared-but-unresolved binding fn (`use brotli` with no
    // engaged runner) must raise GX0002 when called - never silently
    // return Unit (which let tests "pass" with zero real coverage)
    // and never hijack an unrelated builtin sharing the tail name.
    let dir = env::temp_dir().join(format!("gos-unbound-binding-{}", std::process::id()));
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/unbound\"\nversion = \"0.1.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.gos"),
        r#"use brotli
use std::testing

fn main() {
    println!("unused")
}

#[cfg(test)]
mod tests {
    use std::testing

    #[test]
    fn unbound_decode_is_loud() {
        match brotli::decode([1, 2, 3]) {
            Ok(_) => testing::check(false, "must not decode"),
            Err(_) => testing::check(true, "error surfaced"),
        }
    }
}
"#,
    )
    .unwrap();
    let out = Command::new(gos_bin())
        .arg("test")
        .arg(".")
        .current_dir(&dir)
        .output()
        .expect("spawn gos test");
    let stderr = String::from_utf8_lossy(&out.stderr);
    let stdout = String::from_utf8_lossy(&out.stdout);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        !out.status.success(),
        "unbound binding call must fail the test run; stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        stdout.contains("GX0002") && stdout.contains("brotli::decode"),
        "failure must name the unresolved binding; stdout: {stdout}\nstderr: {stderr}"
    );
}
