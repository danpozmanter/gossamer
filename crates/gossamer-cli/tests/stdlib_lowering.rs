//! Stdlib LLVM-tier lowering acceptance suite.
//!
//! Each `#[test]` writes a minimal well-typed Gossamer program that
//! *calls* one stdlib free function, builds it through the LLVM tier
//! (`gos build`), runs the binary, and asserts exit 0 plus expected
//! stdout. A function that the MIR builder fails to lower surfaces as
//! `opt: use of undefined value '@module::fn'` - the build fails and
//! the test goes red.
//!
//! This file is the acceptance gate for "every stdlib function is
//! lowered through the compiled tier".
//!
//! Run just this suite:
//!     `cargo test --release --test stdlib_lowering`

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const PER_RUN_TIMEOUT: Duration = Duration::from_mins(2);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn fresh_dir(tag: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-stdlib-low-{pid}-{n}-{tag}",
        pid = std::process::id(),
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
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
        .is_some_and(|e| e.eq_ignore_ascii_case("exe"))
}

fn build_release(src: &Path, scratch: &Path) -> Result<PathBuf, String> {
    let out = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg("--out-dir")
        .arg(scratch)
        .arg(src)
        .output()
        .expect("spawn gos build");
    if !out.status.success() {
        return Err(String::from_utf8_lossy(&out.stderr).into_owned());
    }
    for entry in fs::read_dir(scratch).map_err(|e| e.to_string())?.flatten() {
        let p = entry.path();
        if p.is_file() && is_executable(&p) {
            return Ok(p);
        }
    }
    Err("no binary produced".to_string())
}

mod common;

fn run_bin(bin: &Path) -> (String, common::RunExit) {
    let mut child = Command::new(bin)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");
    let deadline = Instant::now() + PER_RUN_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return (String::new(), common::aborted("timed out"));
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            Err(_) => return (String::new(), common::aborted("wait failed")),
        }
    }
    let out = child.wait_with_output().expect("wait");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        common::describe_exit(out.status),
    )
}

/// Builds `src` through the LLVM tier and asserts the binary runs to a
/// clean exit and prints `expect`. A lowering gap fails the build with
/// `use of undefined value`, surfaced verbatim in the panic message.
fn assert_lowers(tag: &str, src: &str, expect: &str) {
    let dir = fresh_dir(tag);
    let path = dir.join(format!("{tag}.gos"));
    fs::write(&path, src).expect("write source");
    let scratch = dir.join("bin");
    fs::create_dir_all(&scratch).unwrap();
    let result = build_release(&path, &scratch);
    let bin = match result {
        Ok(b) => b,
        Err(e) => {
            panic!(
                "`{tag}` failed to build through the LLVM tier:\n{e}\nartifacts: {}",
                dir.display()
            );
        }
    };
    let (stdout, exit) = run_bin(&bin);
    assert!(
        exit.success,
        "`{tag}` did not exit cleanly: {exit}; stdout: {stdout:?}\nartifacts: {dir}",
        exit = exit.text,
        dir = dir.display(),
    );
    assert!(
        stdout.contains(expect),
        "`{tag}` stdout {stdout:?} did not contain {expect:?}\nartifacts: {dir}",
        dir = dir.display(),
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn math_big_factorial_lowers() {
    assert_lowers(
        "math_big_factorial",
        r#"
use std::math
fn main() {
    println!("{}", math::big::factorial(10))
    let a = math::big::int_from_i64(1000000000)
    let p = math::big::int_mul(a, math::big::int_from_i64(1000000000))
    println!("{}", p)
}
"#,
        "3628800",
    );
}

#[test]
fn encoding_xml_escape_lowers() {
    assert_lowers(
        "encoding_xml_escape",
        r#"
use std::encoding::xml
fn main() {
    println!("{}", xml::escape(&"a<b>&c".to_string()))
}
"#,
        "a&lt;b&gt;&amp;c",
    );
}

#[test]
fn encoding_base32_decode_string_lowers() {
    assert_lowers(
        "encoding_base32_decode",
        r#"
use std::encoding::base32
fn main() {
    let enc = base32::encode_string(&"hi".to_string())
    match base32::decode_string(&enc) {
        Ok(s) => println!("{}", s),
        Err(_) => println!("err"),
    }
}
"#,
        "hi",
    );
}

#[test]
fn crypto_hmac_sha256_mac_lowers() {
    assert_lowers(
        "crypto_hmac_sha256_mac",
        r#"
use std::crypto::hmac
fn main() {
    let mac = hmac::sha256_mac("key".to_string().as_bytes(), "msg".to_string().as_bytes())
    println!("len={}", mac.len() > 0)
}
"#,
        "len=true",
    );
}

#[test]
fn compress_flate_roundtrip_lowers() {
    assert_lowers(
        "compress_flate",
        r#"
use std::compress::flate
fn main() {
    let data = "hello hello hello hello".to_string()
    let bytes = data.as_bytes()
    match flate::compress(bytes, 6) {
        Ok(packed) => match flate::decompress(packed) {
            Ok(back) => println!("{}", back.len() == bytes.len()),
            Err(_) => println!("decompress-err"),
        },
        Err(_) => println!("compress-err"),
    }
}
"#,
        "true",
    );
}

#[test]
fn html_escape_lowers() {
    assert_lowers(
        "html_escape",
        r#"
use std::html
fn main() {
    println!("{}", html::escape(&"a<b>'c".to_string()))
}
"#,
        "a&lt;b&gt;&#39;c",
    );
}

#[test]
fn encoding_hex_encode_lowers() {
    assert_lowers(
        "encoding_hex_encode",
        r#"
use std::encoding::hex
fn main() {
    println!("{}", hex::encode("hi".to_string().as_bytes()))
}
"#,
        "6869",
    );
}

#[test]
fn encoding_base32_encode_bytes_lowers() {
    assert_lowers(
        "encoding_base32_encode_bytes",
        r#"
use std::encoding::base32
fn main() {
    println!("{}", base32::encode("foobar".to_string().as_bytes()))
}
"#,
        "MZXW6YTBOI======",
    );
}

#[test]
fn compress_zlib_roundtrip_lowers() {
    assert_lowers(
        "compress_zlib",
        r#"
use std::compress::zlib
fn main() {
    let data = "zlib zlib zlib zlib".to_string()
    let bytes = data.as_bytes()
    match zlib::compress(bytes, 6) {
        Ok(packed) => match zlib::decompress(packed) {
            Ok(back) => println!("{}", back.len() == bytes.len()),
            Err(_) => println!("d-err"),
        },
        Err(_) => println!("c-err"),
    }
}
"#,
        "true",
    );
}

#[test]
fn result_default_with_lowers() {
    assert_lowers(
        "result_default_with",
        r#"
use std::{errors, result}
fn parse(s: &String) -> Result<i64, errors::Error> {
    Err(errors::new("nope"))
}
fn main() {
    let v = parse(&"x".to_string()) |> result::unwrap_or_else(|e| { let _ = e; -1 })
    println!("{}", v)
}
"#,
        "-1",
    );
}

#[test]
fn encoding_base64_roundtrip_lowers() {
    assert_lowers(
        "encoding_base64_roundtrip",
        r#"
use std::encoding
fn main() {
    let enc = encoding::base64::encode("Hello, Gossamer!")
    println!("enc={}", enc)
    match encoding::base64::decode(enc) {
        Ok(bytes) => println!("dec={}", bytes.len()),
        Err(e) => println!("err={}", e),
    }
}
"#,
        "dec=16",
    );
}

#[test]
fn encoding_hex_decode_lowers() {
    assert_lowers(
        "encoding_hex_decode",
        r#"
use std::encoding
fn main() {
    match encoding::hex::decode("48656c6c6f") {
        Ok(bytes) => println!("n={}", bytes.len()),
        Err(e) => println!("err={}", e),
    }
}
"#,
        "n=5",
    );
}

#[test]
fn html_unescape_lowers() {
    assert_lowers(
        "html_unescape",
        r#"
use std::html
fn main() {
    println!("{}", html::unescape("a &lt;b&gt; &amp; &#39;c&#39;"))
}
"#,
        "a <b> & 'c'",
    );
}
