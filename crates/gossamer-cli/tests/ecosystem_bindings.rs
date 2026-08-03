//! End-to-end proof that ecosystem adapters use the public binding ABI.

use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_BIN_EXE_gos").expect("gos binary path"))
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn assert_expected_output(stdout: &str) {
    for expected in ["rows=1", "attrs=2", "args=2", "token=access-token"] {
        assert!(
            stdout.contains(expected),
            "missing {expected:?} in {stdout}"
        );
    }
}

#[test]
fn external_binding_supports_ecosystem_library_shapes_without_builtins() {
    let root = workspace_root();
    let binding_api = root.join("crates/gossamer-binding");
    let dir = std::env::temp_dir().join(format!(
        "gos-ecosystem-binding-{}-{}",
        std::process::id(),
        std::thread::current().name().unwrap_or("test")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("ecosystem-binding/src")).expect("create binding crate");
    std::fs::create_dir_all(dir.join("src")).expect("create source directory");
    std::fs::write(
        dir.join("project.toml"),
        "[project]\nid = \"example.com/ecosystem-binding\"\nversion = \"0.1.0\"\n\n\
         [rust-bindings]\necosystem-binding = { path = \"ecosystem-binding\" }\n",
    )
    .expect("write project manifest");
    std::fs::write(
        dir.join("ecosystem-binding/Cargo.toml"),
        format!(
            "[package]\nname = \"ecosystem-binding\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             publish = false\n\n[workspace]\n\n[lib]\ncrate-type = [\"rlib\"]\n\n\
             [dependencies]\ngossamer-binding = {{ path = {binding_api:?} }}\n"
        ),
    )
    .expect("write binding manifest");
    std::fs::write(
        dir.join("ecosystem-binding/src/lib.rs"),
        r#"
use std::collections::HashMap;
use gossamer_binding::{Bytes, register_module};

register_module!(
    name: ecosystem,
    doc: "Generic external-library capability fixture.",
    // SQL/Postgres-style row batches and fallible driver calls.
    fn database_query(sql: String) -> Result<Vec<String>, String> {
        if sql == "select id" { Ok(vec!["row:7".to_string()]) } else { Err("bad query".to_string()) }
    }
    // OpenTelemetry-style attribute maps.
    fn attribute_count(attrs: HashMap<String, String>) -> i64 {
        i64::try_from(attrs.len()).unwrap_or(i64::MAX)
    }
    // CLI parser output and diagnostics.
    fn parse_args(args: Vec<String>) -> Result<Vec<String>, String> {
        if args.is_empty() { Err("missing command".to_string()) } else { Ok(args) }
    }
    // Redis/RPC/protobuf/MessagePack/CBOR payloads share typed Bytes.
    fn binary_round_trip(payload: Bytes) -> Result<Bytes, String> { Ok(payload) }
    // OAuth/OIDC wrappers expose tokens through the ordinary Result ABI.
    fn exchange_code(code: String) -> Result<String, String> {
        if code == "good" { Ok("access-token".to_string()) } else { Err("invalid code".to_string()) }
    }
);

pub fn __bindings_force_link() { __gos_ecosystem::force_link(); }
"#,
    )
    .expect("write external binding source");
    std::fs::write(
        dir.join("src/main.gos"),
        r#"
use ecosystem::database_query
use ecosystem::attribute_count
use ecosystem::parse_args
use ecosystem::exchange_code
use std::collections::HashMap

fn main() {
    match database_query("select id") {
        Ok(rows) => { println!("rows={}", rows.len()) },
        Err(err) => { panic(err) },
    }
    let mut attrs: HashMap<String, String> = HashMap::new()
    attrs.insert("service", "fixture")
    attrs.insert("environment", "test")
    println!("attrs={}", attribute_count(attrs))
    match parse_args(["serve", "--dry-run"]) {
        Ok(args) => { println!("args={}", args.len()) },
        Err(err) => { panic(err) },
    }
    match exchange_code("good") {
        Ok(token) => { println!("token={}", token) },
        Err(err) => { panic(err) },
    }
}
"#,
    )
    .expect("write Gossamer source");

    let out = Command::new(gos_bin())
        .arg("run")
        .arg("src/main.gos")
        .current_dir(&dir)
        .env("GOSSAMER_ROOT", &root)
        .env("GOSSAMER_CACHE", dir.join("cache"))
        .output()
        .expect("run ecosystem binding fixture");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let _ = std::fs::remove_dir_all(&dir);
    assert!(
        out.status.success(),
        "ecosystem binding fixture failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    assert_expected_output(&stdout);
}
