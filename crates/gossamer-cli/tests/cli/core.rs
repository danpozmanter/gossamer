// End-to-end CLI tests.
// Shells out to the `gos` binary Cargo produces for this crate and
// asserts behaviour for `parse`, `check`, `run`, `build`, plus
// cross-compilation via `--target`.


use std::env;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::PathBuf;
use std::process::{Command, Stdio};

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

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
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
fn stdin_read_line_appends_to_mut_string() {
    let fixture = write_fixture(
        "stdin-read-line",
        r#"use std::io

fn main() {
    let mut input = String::new()
    io::stdin().read_line(&mut input).unwrap()
    println!("typed={} bytes={}", input.trim(), input.len())
}
"#,
    );
    let mut child = Command::new(gos_bin())
        .args(["run"])
        .arg(&fixture)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn run");
    child
        .stdin
        .as_mut()
        .expect("stdin")
        .write_all(b"hello\n")
        .expect("write stdin");
    let out = child.wait_with_output().expect("wait run");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "typed=hello bytes=6\n"
    );
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

#[cfg(target_os = "linux")]
#[test]
fn build_rss_profile_reports_frontend_release_and_backend_peak() {
    let dir = env::temp_dir().join(format!("gos-build-rss-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("rss.gos");
    std::fs::write(&source_path, "fn main() { println(\"rss\") }\n").unwrap();

    let out = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .env("GOS_PROFILE_RSS", "1")
        .output()
        .expect("spawn build with RSS profiling");
    assert!(
        out.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    for stage in [
        "build_frontend_checked",
        "build_frontend_released",
        "build_backend_emitted",
    ] {
        assert!(stderr.contains(&format!("rss: stage={stage} ")), "{stderr}");
    }
    assert!(stderr.contains("peak_bytes="), "{stderr}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(target_os = "macos")]
#[test]
fn macos_build_records_15_0_deployment_target() {
    let dir = env::temp_dir().join(format!(
        "gos-macos-deployment-target-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let source_path = dir.join("deployment_target.gos");
    std::fs::write(&source_path, "fn main() { println(\"macos-15\") }\n").unwrap();

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&source_path)
        .env_remove("MACOSX_DEPLOYMENT_TARGET")
        .output()
        .expect("spawn gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let binary = dir
        .join("target")
        .join("debug")
        .join("deployment_target");
    let metadata = Command::new("otool")
        .arg("-l")
        .arg(&binary)
        .output()
        .expect("run otool");
    assert!(
        metadata.status.success(),
        "otool failed: {}",
        String::from_utf8_lossy(&metadata.stderr)
    );
    let metadata = String::from_utf8(metadata.stdout).expect("otool output is UTF-8");
    assert!(
        metadata.lines().any(|line| line.trim() == "minos 15.0"),
        "Mach-O does not record macOS 15.0 as LC_BUILD_VERSION minos:\n{metadata}"
    );

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
    // not silently stubbed. Pick the darwin triple for the *other*
    // arch: on an Apple Silicon macOS runner (host `aarch64-apple-darwin`,
    // what `macos-latest` is today) the same-arch triple equals the host
    // and takes the native, non-cross build path instead of being refused.
    let other_arch_darwin = if cfg!(target_arch = "aarch64") {
        "x86_64-apple-darwin"
    } else {
        "aarch64-apple-darwin"
    };
    let darwin = Command::new(gos_bin())
        .args(["build", "--target", other_arch_darwin])
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
fn update_is_a_first_class_package_command() {
    let out = Command::new(gos_bin())
        .args(["update", "--help"])
        .output()
        .expect("spawn update help");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("newest dependency versions"), "{stdout}");
    assert!(stdout.contains("--offline"), "{stdout}");
}

#[test]
fn tidy_removes_only_unimported_project_dependencies() {
    let dir = env::temp_dir().join(format!("gos-tidy-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = dir.join("project.toml");
    std::fs::write(
        &manifest,
        "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.com/used\" = \"1.0.0\"\n\"example.com/unused\" = \"1.0.0\"\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/main.gos"),
        "use \"example.com/used\" as used\nfn main() { used::run() }\n",
    )
    .unwrap();

    let out = Command::new(gos_bin())
        .args(["tidy", "--manifest"])
        .arg(&manifest)
        .output()
        .expect("spawn tidy");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let rewritten = std::fs::read_to_string(&manifest).unwrap();
    assert!(rewritten.contains("example.com/used"), "{rewritten}");
    assert!(!rewritten.contains("example.com/unused"), "{rewritten}");
    assert!(
        String::from_utf8_lossy(&out.stdout).contains("1 unused dependency/dependencies removed")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tidy_does_not_edit_manifest_when_a_source_file_has_parse_errors() {
    let dir = env::temp_dir().join(format!("gos-tidy-parse-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    let manifest = dir.join("project.toml");
    let original = "[project]\nid = \"example.com/app\"\nversion = \"0.1.0\"\n\n[dependencies]\n\"example.com/keep\" = \"1.0.0\"\n";
    std::fs::write(&manifest, original).unwrap();
    std::fs::write(dir.join("src/main.gos"), "fn main( {\n").unwrap();

    let out = Command::new(gos_bin())
        .args(["tidy", "--manifest"])
        .arg(&manifest)
        .output()
        .expect("spawn tidy");
    assert!(!out.status.success(), "tidy must reject malformed source");
    assert_eq!(std::fs::read_to_string(&manifest).unwrap(), original);
    let _ = std::fs::remove_dir_all(&dir);
}
