//! Parity check across the 0.4.0 HTTP-module bridges. Runs the
//! same `.gos` source in `gos run` (interp) and `gos build` →
//! native, asserts byte-identical stdout. Covers the surfaces
//! that exist in BOTH tiers — stateful types (`Router`,
//! `FileServer` method chains, `Proxy`, full `NativeClient`,
//! WebSocket framing) are interp-only and tracked in #54.

use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    let mut p = std::env::current_exe().unwrap();
    p.pop();
    p.pop();
    p.push("gos");
    p
}

const BRIDGE_SRC: &str = r#"
use std::http::chunked
use std::http::sse
use std::http::middleware
use std::http::websocket
use std::http::static_files

fn main() {
    let encoded = chunked::encode("payload")
    println!("CHUNKED_LEN={}", encoded.len())

    println!("SSE_EVENT={}", sse::encode_event("tick", "v", "1"))
    println!("SSE_COMMENT={}", sse::encode_comment("ka"))
    println!("SSE_RETRY={}", sse::encode_retry(1500))

    println!("GZIP_YES={}", middleware::accepts_gzip("gzip, deflate"))
    println!("GZIP_NO={}", middleware::accepts_gzip("deflate"))

    println!("WS_ACCEPT={}", websocket::accept_key("dGhlIHNhbXBsZSBub25jZQ=="))

    println!("MIME_HTML={}", static_files::mime_for_path("/x.html"))
    println!("MIME_PNG={}", static_files::mime_for_path("/x.png"))
    println!("MIME_UNK={}", static_files::mime_for_path("/x.zzz"))
}
"#;

fn write_source() -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let path = std::env::temp_dir().join(format!("gos-bridge-{pid}-{nanos}.gos"));
    std::fs::write(&path, BRIDGE_SRC).unwrap();
    path
}

#[test]
fn http_bridge_interp_matches_compiled() {
    let src = write_source();
    let interp = Command::new(gos_bin())
        .arg("run")
        .arg(&src)
        .output()
        .expect("gos run");
    assert!(
        interp.status.success(),
        "interp failed: {}",
        String::from_utf8_lossy(&interp.stderr)
    );

    let build = Command::new(gos_bin())
        .arg("build")
        .arg(&src)
        .output()
        .expect("gos build");
    assert!(
        build.status.success(),
        "build failed: {}",
        String::from_utf8_lossy(&build.stderr)
    );

    let stem = src.file_stem().unwrap().to_str().unwrap();
    let bin = std::env::temp_dir().join("target").join("debug").join(stem);
    let compiled = Command::new(&bin).output().expect("compiled run");
    assert!(
        compiled.status.success(),
        "compiled binary failed: {}",
        String::from_utf8_lossy(&compiled.stderr)
    );

    let interp_out = String::from_utf8_lossy(&interp.stdout).into_owned();
    let compiled_out = String::from_utf8_lossy(&compiled.stdout).into_owned();
    assert_eq!(
        interp_out, compiled_out,
        "bridge parity mismatch\n--interp--\n{interp_out}\n--compiled--\n{compiled_out}"
    );

    // Spot-check known-good values from the parity output so a
    // regression in EITHER tier (interp drifting from compiled
    // OR both drifting together) trips the bound.
    let stdout = interp_out.as_str();
    assert!(stdout.contains("WS_ACCEPT=s3pPLMBiTxaQ9kYGzzhZRbK+xOo="));
    assert!(stdout.contains("MIME_HTML=text/html; charset=utf-8"));
    assert!(stdout.contains("MIME_PNG=image/png"));
    assert!(stdout.contains("MIME_UNK=application/octet-stream"));
    assert!(stdout.contains("GZIP_YES=true"));
    assert!(stdout.contains("GZIP_NO=false"));

    let _ = std::fs::remove_file(&src);
}
