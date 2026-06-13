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
const PORT_FORWARDER: &str = "use std::net\n\n// Local URL forwarder.\n// Listens on 127.0.0.1:8080 and forwards to the upstream below.\nconst UPSTREAM: String = \"127.0.0.1:9090\"  // staging box\n\nfn main() {\n    let listener = net::TcpListener::bind(\"127.0.0.1:8080\")\n    // accept loop: one goroutine per connection\n    loop {\n        let stream = listener.accept()\n        go forward(stream)\n    }\n}\n\nfn forward(stream: net::TcpStream) {\n    /* copy bytes both ways until either side closes */\n    let upstream = net::TcpStream::connect(&UPSTREAM)\n    println!(\"forwarding to {}\", UPSTREAM)\n}\n";

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
fn fmt_rewrite_preserves_comments_and_macros() {
    let messy =
        "fn   main( ){\n    let port=8080  // upstream port\n    println!(\"on {}\",port)\n}\n";
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
        "fn main() {\n    let port = 8080  // upstream port\n    println!(\"on {}\", port)\n}\n"
    );
    assert!(after.contains("// upstream port"), "comment dropped");
    assert!(after.contains("println!"), "macro rewritten");
    assert!(!after.contains("__concat"), "macro desugared");
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
