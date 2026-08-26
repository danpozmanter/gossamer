//! End-to-end tests for the faithful `gos fmt` command.
//!
//! The token-stream engine itself is covered by corpus tests in
//! `gossamer-parse/tests/fmt_faithful.rs`; these tests shell out to
//! the `gos` binary and assert the command-level guarantees: comments
//! and macros survive a rewrite, `--check` agrees with the canonical
//! form, and unparseable input refuses to format without touching the
//! file.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn write_fixture(name: &str, source: &str) -> PathBuf {
    let mut path = env::temp_dir();
    path.push(format!(
        "gossamer-fmt-faithful-{}-{}.gos",
        name,
        std::process::id()
    ));
    std::fs::write(&path, source).expect("write fixture");
    path
}

/// The historical mangling case: a hand-written forwarder with a port
/// number in a comment. The old AST printer deleted every comment and
/// rewrote `println!` into `__concat`; the faithful formatter must
/// leave this file byte-identical.
const PORT_FORWARDER: &str = "use std::net\n\n// Local URL forwarder.\n// Listens on 127.0.0.1:8080 and forwards to the upstream below.\nconst UPSTREAM: String = \"127.0.0.1:9090\"  // staging box\n\nfn main() {\n    let listener = net::TcpListener::bind(\"127.0.0.1:8080\")\n    // accept loop: one goroutine per connection\n    loop {\n        let stream = listener.accept()\n        spawn(|| forward(stream))\n    }\n}\n\nfn forward(stream: net::TcpStream) {\n    /* copy bytes both ways until either side closes */\n    let upstream = net::TcpStream::connect(UPSTREAM)\n    println(\"forwarding to {}\", UPSTREAM)\n}\n";

#[test]
fn fmt_leaves_canonical_commented_source_byte_identical() {
    let fixture = write_fixture("portfwd", PORT_FORWARDER);
    let out = Command::new(gos_bin())
        .arg("fmt")
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&fixture).unwrap();
    assert_eq!(after, PORT_FORWARDER, "fmt altered a canonical file");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_check_accepts_canonical_commented_source() {
    let fixture = write_fixture("portfwd-check", PORT_FORWARDER);
    let out = Command::new(gos_bin())
        .args(["fmt", "--check"])
        .arg(&fixture)
        .output()
        .expect("spawn fmt --check");
    assert!(
        out.status.success(),
        "--check rejected canonical source: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_rewrite_preserves_comments_and_builtin_calls() {
    let messy =
        "fn   main( ){\n    let port=8080  // upstream port\n    println(\"on {}\",port)\n}\n";
    let fixture = write_fixture("messy", messy);
    let out = Command::new(gos_bin())
        .arg("fmt")
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let after = std::fs::read_to_string(&fixture).unwrap();
    assert_eq!(
        after,
        "fn main() {\n    let port = 8080  // upstream port\n    println(\"on {}\", port)\n}\n"
    );
    assert!(after.contains("// upstream port"), "comment dropped");
    assert!(after.contains("println("), "builtin call rewritten");
    assert!(!after.contains("__concat"), "builtin call desugared");
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_removes_match_arm_commas_before_trailing_comments() {
    let source = "fn main() {\n    let a = 1\n    let value = match a {\n        1 => a + 1, // line comment\n        2 => a + 2, /* block comment */\n    }\n    println(value)\n}\n";
    let expected = "fn main() {\n    let a = 1\n    let value = match a {\n        1 => a + 1 // line comment\n        2 => a + 2 /* block comment */\n    }\n    println(value)\n}\n";
    let fixture = write_fixture("match-arm-comments", source);
    let out = Command::new(gos_bin())
        .arg("fmt")
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&fixture).unwrap(), expected);
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_aligns_parameters_after_generic_types() {
    let source = "fn many_params(\n    one: Vec<i64>\n        two: i64\n    three: Vec<Vec<String>>\n        four: i64\n) {\n    one[0] + two + four\n}\n";
    let expected = "fn many_params(\n    one: Vec<i64>\n    two: i64\n    three: Vec<Vec<String>>\n    four: i64\n) {\n    one[0] + two + four\n}\n";
    let fixture = write_fixture("generic-parameters", source);
    let out = Command::new(gos_bin())
        .arg("fmt")
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(
        out.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(std::fs::read_to_string(&fixture).unwrap(), expected);

    let check = Command::new(gos_bin())
        .args(["fmt", "--check"])
        .arg(&fixture)
        .output()
        .expect("spawn fmt --check");
    assert!(
        check.status.success(),
        "formatted file was not idempotent: {}",
        String::from_utf8_lossy(&check.stderr)
    );
    let _ = std::fs::remove_file(&fixture);
}

#[test]
fn fmt_refuses_unparseable_input_and_leaves_file_untouched() {
    let broken = "fn broken( {\n";
    let fixture = write_fixture("broken", broken);
    let out = Command::new(gos_bin())
        .arg("fmt")
        .arg(&fixture)
        .output()
        .expect("spawn fmt");
    assert!(!out.status.success(), "fmt accepted unparseable input");
    let after = std::fs::read_to_string(&fixture).unwrap();
    assert_eq!(after, broken, "fmt modified a file it refused to format");
    let _ = std::fs::remove_file(&fixture);
}

/// Repository root, two levels above this crate.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// Every `.gos` source the repository ships as an example is written in
/// the canonical form, so a reader diffs semantics rather than layout and
/// `gos fmt` is the one style a generated program is measured against.
#[test]
fn shipped_gossamer_sources_are_canonically_formatted() {
    let root = workspace_root();
    let mut drift: Vec<String> = Vec::new();
    for dir in ["examples", "feature-testing-examples"] {
        let entries = std::fs::read_dir(root.join(dir))
            .unwrap_or_else(|e| panic!("read {dir}: {e}"))
            .flatten();
        for entry in entries {
            let path = entry.path();
            if path.extension().and_then(|s| s.to_str()) != Some("gos") {
                continue;
            }
            let out = Command::new(gos_bin())
                .args(["fmt", "--check"])
                .arg(&path)
                .output()
                .expect("spawn fmt --check");
            if !out.status.success() {
                drift.push(format!("{dir}/{}", entry.file_name().to_string_lossy()));
            }
        }
    }
    drift.sort();
    assert!(
        drift.is_empty(),
        "{} shipped source(s) are not canonically formatted; run `gos fmt` on: {}",
        drift.len(),
        drift.join(", ")
    );
}
