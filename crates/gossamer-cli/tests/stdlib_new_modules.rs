// Integration tests for the new stdlib modules added in the P0 gap-fill:
// std::math, std::math::bits, std::unicode, std::utf8 (expanded),
// std::utf16, std::iter, std::encoding::csv, std::encoding::pem,
// std::encoding::binary (full), std::encoding::yaml, std::crypto.

#![allow(missing_docs)]

use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

const TIMEOUT: Duration = Duration::from_secs(30);

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn scratch(tag: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let n = N.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-stdlib-{pid}-{n}-{tag}",
        pid = std::process::id()
    ));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

fn run_gos(src: &Path) -> (String, String, Option<i32>) {
    let mut child = Command::new(gos_bin())
        .arg("run")
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn gos run");
    let deadline = Instant::now() + TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(20)),
        }
    }
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n"),
        String::from_utf8_lossy(&out.stderr).replace("\r\n", "\n"),
        out.status.code(),
    )
}

fn assert_vm_output(tag: &str, src: &str, expected: &str) {
    let dir = scratch(tag);
    let path = dir.join("main.gos");
    fs::File::create(&path)
        .unwrap()
        .write_all(src.as_bytes())
        .unwrap();
    let (stdout, stderr, code) = run_gos(&path);
    assert_eq!(
        stdout.trim_end(),
        expected,
        "[{tag}/vm] stdout mismatch\nstderr: {stderr}\ncode: {code:?}"
    );
}

// -----------------------------------------------------------------------
// std::math

#[test]
fn math_abs_and_sqrt() {
    assert_vm_output(
        "math_abs_sqrt",
        r#"
use std::math
fn main() {
    println!("{}", math::abs(-3.0))
    println!("{}", math::sqrt(9.0))
}
"#,
        "3\n3",
    );
}

#[test]
fn math_trig_floor_ceil() {
    assert_vm_output(
        "math_trig_floor",
        r#"
use std::math
fn main() {
    let s = math::sin(0.0)
    let c = math::cos(0.0)
    let f = math::floor(2.7)
    let cl = math::ceil(2.1)
    println!("{} {} {} {}", s, c, f, cl)
}
"#,
        "0 1 2 3",
    );
}

#[test]
fn math_min_max() {
    assert_vm_output(
        "math_min_max",
        r#"
use std::math
fn main() {
    println!("{}", math::min(3.0, 5.0))
    println!("{}", math::max(3.0, 5.0))
}
"#,
        "3\n5",
    );
}

// -----------------------------------------------------------------------
// std::math::bits

#[test]
fn math_bits_count_ones_and_len() {
    assert_vm_output(
        "math_bits",
        r#"
use std::math
fn main() {
    println!("{}", math::bits::count_ones(255))
    println!("{}", math::bits::len(8))
    println!("{}", math::bits::leading_zeros(1))
}
"#,
        "8\n4\n63",
    );
}

// -----------------------------------------------------------------------
// std::unicode

#[test]
fn unicode_predicates() {
    assert_vm_output(
        "unicode_predicates",
        r#"
use std::unicode
fn main() {
    println!("{}", unicode::is_letter('a'))
    println!("{}", unicode::is_digit('5'))
    println!("{}", unicode::is_space(' '))
    println!("{}", unicode::is_upper('A'))
    println!("{}", unicode::is_lower('z'))
    println!("{}", unicode::to_upper('a'))
    println!("{}", unicode::to_lower('A'))
}
"#,
        "true\ntrue\ntrue\ntrue\ntrue\nA\na",
    );
}

// -----------------------------------------------------------------------
// std::utf8

#[test]
fn utf8_is_valid_and_rune_count() {
    assert_vm_output(
        "utf8_basics",
        r#"
use std::utf8
fn main() {
    println!("{}", utf8::is_valid("hello"))
    println!("{}", utf8::rune_count("café"))
    println!("{}", utf8::rune_len('€'))
}
"#,
        "true\n4\n3",
    );
}

// -----------------------------------------------------------------------
// std::strings (Unicode-sensitive additions)

#[test]
fn strings_contains_rune_and_fields() {
    assert_vm_output(
        "strings_unicode",
        r#"
use std::strings
fn main() {
    println!("{}", strings::contains_rune("café", 'é'))
    let fs = strings::fields("  hello   world  ")
    println!("{}", fs.len())
}
"#,
        "true\n2",
    );
}

#[test]
fn strings_equal_fold() {
    assert_vm_output(
        "strings_equal_fold",
        r#"
use std::strings
fn main() {
    println!("{}", strings::equal_fold("Hello", "hello"))
    println!("{}", strings::equal_fold("Go", "Python"))
}
"#,
        "true\nfalse",
    );
}

// -----------------------------------------------------------------------
// std::iter

#[test]
fn iter_take_skip_chain() {
    assert_vm_output(
        "iter_basics",
        r#"
use std::iter
fn main() {
    let xs = [1, 2, 3, 4, 5]
    let first = iter::take(xs, 3)
    let rest = iter::skip(xs, 3)
    let merged = iter::chain(first, rest)
    println!("{}", merged.len())
}
"#,
        "5",
    );
}

#[test]
fn iter_map_filter_fold() {
    assert_vm_output(
        "iter_closures",
        r#"
use std::iter
fn main() {
    let xs = [1, 2, 3, 4, 5]
    let doubled = iter::map(xs, |x: i64| x * 2)
    let evens = iter::filter(doubled, |x: i64| x % 4 == 0)
    let total = iter::fold(evens, 0, |acc: i64, x: i64| acc + x)
    println!("{}", total)
}
"#,
        "12",
    );
}

#[test]
fn iter_any_all() {
    assert_vm_output(
        "iter_any_all",
        r#"
use std::iter
fn main() {
    let xs = [2, 4, 6, 8]
    println!("{}", iter::all(xs, |x: i64| x % 2 == 0))
    println!("{}", iter::any(xs, |x: i64| x > 5))
}
"#,
        "true\ntrue",
    );
}

#[test]
fn iter_enumerate_zip() {
    assert_vm_output(
        "iter_enum_zip",
        r#"
use std::iter
fn main() {
    let pairs = iter::zip([1, 2, 3], [4, 5, 6])
    println!("{}", pairs.len())
    let indexed = iter::enumerate([10, 20, 30])
    println!("{}", indexed.len())
}
"#,
        "3\n3",
    );
}

#[test]
fn iter_flatten() {
    assert_vm_output(
        "iter_flatten",
        r#"
use std::iter
fn main() {
    let xs = iter::flatten([[1, 2], [3], [4, 5]])
    println!("{}", xs.len())
}
"#,
        "5",
    );
}

#[test]
fn iter_count_sum_reversed() {
    assert_vm_output(
        "iter_count_sum_rev",
        r#"
use std::iter
fn main() {
    let xs = [10, 20, 30]
    println!("{}", iter::count(xs))
    println!("{}", iter::sum(xs))
    let r = iter::reversed(xs)
    println!("{}", r[0])
}
"#,
        "3\n60\n30",
    );
}

// -----------------------------------------------------------------------
// std::encoding::csv

#[test]
fn encoding_csv_read_write() {
    assert_vm_output(
        "csv_roundtrip",
        r#"
use std::encoding
fn main() {
    let rows = encoding::csv::read("a,b,c\n1,2,3\n").unwrap_or([[]])
    println!("{}", rows.len())
    println!("{}", rows[1][2])
}
"#,
        "2\n3",
    );
}

// -----------------------------------------------------------------------
// std::encoding::binary

#[test]
fn encoding_binary_u64_roundtrip() {
    assert_vm_output(
        "binary_u64",
        r#"
use std::encoding
fn main() {
    let n = 72623859790382856
    let buf = encoding::binary::put_u64_be(0, n)
    match encoding::binary::get_u64_be(buf) {
        Ok(back) => println!("{}", back == n),
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "true",
    );
}

// -----------------------------------------------------------------------
// std::encoding::yaml

#[test]
fn encoding_yaml_parse() {
    assert_vm_output(
        "yaml_parse",
        r#"
use std::encoding
fn main() {
    let result = encoding::yaml::parse("hello: world\ncount: 42")
    match result {
        Ok(v) => println!("ok"),
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "ok",
    );
}

// -----------------------------------------------------------------------
// std::crypto

#[test]
fn crypto_sha256_hex() {
    assert_vm_output(
        "crypto_sha256",
        r#"
use std::crypto
fn main() {
    let h = crypto::sha256::hex("hello")
    println!("{}", h.len())
}
"#,
        "64",
    );
}

#[test]
fn crypto_rand_bytes() {
    assert_vm_output(
        "crypto_rand",
        r#"
use std::crypto
fn main() {
    match crypto::rand::bytes(16) {
        Ok(b) => println!("{}", b.len()),
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "16",
    );
}

// -----------------------------------------------------------------------
// std::compress

#[test]
fn compress_gzip_roundtrip() {
    assert_vm_output(
        "compress_gzip",
        r#"
use std::compress
fn main() {
    let data = [104, 101, 108, 108, 111]
    match compress::gzip::encode(data, 6) {
        Ok(enc) => {
            match compress::gzip::decode(enc) {
                Ok(dec) => println!("{}", dec.len()),
                Err(e) => println!("decode err: {}", e),
            }
        }
        Err(e) => println!("encode err: {}", e),
    }
}
"#,
        "5",
    );
}

#[test]
fn compress_flate_roundtrip() {
    assert_vm_output(
        "compress_flate",
        r#"
use std::compress
fn main() {
    let data = [104, 101, 108, 108, 111]
    match compress::flate::compress(data, 6) {
        Ok(enc) => {
            match compress::flate::decompress(enc) {
                Ok(dec) => println!("{}", dec.len()),
                Err(e) => println!("decompress err: {}", e),
            }
        }
        Err(e) => println!("compress err: {}", e),
    }
}
"#,
        "5",
    );
}

#[test]
fn compress_zlib_roundtrip() {
    assert_vm_output(
        "compress_zlib",
        r#"
use std::compress
fn main() {
    let data = [104, 101, 108, 108, 111]
    match compress::zlib::compress(data, 6) {
        Ok(enc) => {
            match compress::zlib::decompress(enc) {
                Ok(dec) => println!("{}", dec.len()),
                Err(e) => println!("decompress err: {}", e),
            }
        }
        Err(e) => println!("compress err: {}", e),
    }
}
"#,
        "5",
    );
}

// -----------------------------------------------------------------------
// std::hash::fnv

#[test]
fn hash_fnv_hash_string() {
    assert_vm_output(
        "hash_fnv",
        r#"
use std::hash
fn main() {
    let h1 = hash::fnv::hash_string("hello")
    let h2 = hash::fnv::hash_string("hello")
    println!("{}", h1 == h2)
    let h3 = hash::fnv::hash_string("world")
    println!("{}", h1 == h3)
}
"#,
        "true\nfalse",
    );
}

#[test]
fn hash_fnv_hash64_bytes() {
    assert_vm_output(
        "hash_fnv64",
        r#"
use std::hash
fn main() {
    let h = hash::fnv::hash64([])
    println!("{}", h != 0)
}
"#,
        "true",
    );
}

// -----------------------------------------------------------------------
// std::archive

#[test]
fn archive_zip_roundtrip() {
    assert_vm_output(
        "archive_zip",
        r#"
use std::archive
fn main() {
    let files = [("hello.txt", [104, 101, 108, 108, 111])]
    match archive::zip::write(files) {
        Ok(zip_bytes) => {
            match archive::zip::read(zip_bytes) {
                Ok(entries) => {
                    println!("{}", entries.len())
                    println!("{}", entries[0].data.len())
                }
                Err(e) => println!("read err: {}", e),
            }
        }
        Err(e) => println!("write err: {}", e),
    }
}
"#,
        "1\n5",
    );
}

#[test]
fn archive_tar_roundtrip() {
    assert_vm_output(
        "archive_tar",
        r#"
use std::archive
fn main() {
    let files = [("hello.txt", [104, 101, 108, 108, 111])]
    match archive::tar::write(files) {
        Ok(tar_bytes) => {
            match archive::tar::read(tar_bytes) {
                Ok(entries) => {
                    println!("{}", entries.len())
                    println!("{}", entries[0].data.len())
                }
                Err(e) => println!("read err: {}", e),
            }
        }
        Err(e) => println!("write err: {}", e),
    }
}
"#,
        "1\n5",
    );
}

// -----------------------------------------------------------------------
// sync::AtomicU64 and sync::Barrier

#[test]
fn sync_atomic_u64_basic() {
    assert_vm_output(
        "sync_atomic_u64",
        r#"
use std::sync
fn main() {
    let a = sync::AtomicU64::new(0)
    sync::AtomicU64::store(a, 42)
    let v = sync::AtomicU64::load(a)
    println!("{}", v)
    let prev = sync::AtomicU64::fetch_add(a, 8)
    println!("{}", prev)
    println!("{}", sync::AtomicU64::load(a))
}
"#,
        "42\n42\n50",
    );
}

#[test]
fn sync_barrier_releases_all() {
    assert_vm_output(
        "sync_barrier",
        r#"
use std::sync
fn main() {
    let b = sync::Barrier::new(1)
    sync::Barrier::wait(b)
    println!("done")
}
"#,
        "done",
    );
}

// -----------------------------------------------------------------------
// P1: crypto breadth

#[test]
fn crypto_sha512_hex() {
    assert_vm_output(
        "crypto_sha512_hex",
        r#"
use std::crypto
fn main() {
    let h = crypto::sha512::hex("abc")
    println!("{}", h.len())
    println!("{}", h.starts_with("dd"))
}
"#,
        "128\ntrue",
    );
}

#[test]
fn crypto_blake3_hex() {
    assert_vm_output(
        "crypto_blake3_hex",
        r#"
use std::crypto
fn main() {
    let h = crypto::blake3::hex("")
    println!("{}", h.len())
    println!("{}", h.starts_with("af"))
}
"#,
        "64\ntrue",
    );
}

#[test]
fn crypto_aes_gcm_roundtrip() {
    assert_vm_output(
        "crypto_aes_gcm",
        r#"
use std::crypto
fn main() {
    match crypto::rand::bytes(32) {
        Ok(key) => {
            match crypto::rand::bytes(12) {
                Ok(nonce) => {
                    let pt = "hello aes"
                    match crypto::aead::aes_256_gcm_seal(key, nonce, pt, "") {
                        Ok(ct) => {
                            match crypto::aead::aes_256_gcm_open(key, nonce, ct, "") {
                                Ok(dec) => println!("{}", dec.len()),
                                Err(e) => println!("open err: {}", e)
                            }
                        }
                        Err(e) => println!("seal err: {}", e)
                    }
                }
                Err(e) => println!("nonce err: {}", e)
            }
        }
        Err(e) => println!("key err: {}", e)
    }
}
"#,
        "9",
    );
}

#[test]
fn crypto_ed25519_roundtrip() {
    assert_vm_output(
        "crypto_ed25519",
        r#"
use std::crypto
fn main() {
    match crypto::ed25519::keypair() {
        Ok(pair) => {
            let secret = pair.0
            let public = pair.1
            let msg = "test message"
            match crypto::ed25519::sign(secret, msg) {
                Ok(sig) => {
                    match crypto::ed25519::verify(public, msg, sig) {
                        Ok(_) => println!("verified"),
                        Err(e) => println!("verify err: {}", e)
                    }
                }
                Err(e) => println!("sign err: {}", e)
            }
        }
        Err(e) => println!("keypair err: {}", e)
    }
}
"#,
        "verified",
    );
}

#[test]
fn crypto_kdf_pbkdf2() {
    assert_vm_output(
        "crypto_pbkdf2",
        r#"
use std::crypto
fn main() {
    let key = crypto::kdf::pbkdf2_sha256("password", "salt", 1, 32)
    println!("{}", key.len())
}
"#,
        "32",
    );
}

// -----------------------------------------------------------------------
// P1: hash crc32 and adler32

#[test]
fn hash_crc32_known_value() {
    assert_vm_output(
        "hash_crc32",
        r#"
use std::hash
fn main() {
    // 0x0D4A_1185 = 222957957
    let c = hash::crc32::checksum_string("hello world")
    println!("{}", c == 222957957)
}
"#,
        "true",
    );
}

#[test]
fn hash_adler32_known_value() {
    assert_vm_output(
        "hash_adler32",
        r#"
use std::hash
fn main() {
    let c = hash::adler32::checksum_string("Wikipedia")
    println!("{}", c == 300286872)
}
"#,
        "true",
    );
}

// -----------------------------------------------------------------------
// P1: json builtins

#[test]
fn json_parse_and_encode() {
    assert_vm_output(
        "json_parse_encode",
        r#"
use std::encoding
fn main() {
    let src = "{\"x\":1,\"y\":2}"
    match encoding::json::parse(src) {
        Ok(v) => {
            let out = encoding::json::encode(v)
            println!("{}", encoding::json::valid(out))
        }
        Err(e) => println!("err: {}", e)
    }
}
"#,
        "true",
    );
}

#[test]
fn json_valid_rejects_bad() {
    assert_vm_output(
        "json_valid_bad",
        r#"
use std::encoding
fn main() {
    println!("{}", encoding::json::valid("not json"))
    println!("{}", encoding::json::valid("42"))
}
"#,
        "false\ntrue",
    );
}

// -----------------------------------------------------------------------
// P1: time completeness

#[test]
fn time_now_returns_positive() {
    assert_vm_output(
        "time_now",
        r#"
use std::time
fn main() {
    let ms = time::now()
    println!("{}", ms > 0)
}
"#,
        "true",
    );
}

#[test]
fn time_format_parse_rfc3339() {
    assert_vm_output(
        "time_rfc3339",
        r#"
use std::time
fn main() {
    let ms = 0
    match time::format_rfc3339(ms) {
        Ok(s) => {
            println!("{}", s.starts_with("1970"))
            match time::parse_rfc3339(s) {
                Ok(back) => println!("{}", back == 0),
                Err(e) => println!("parse err: {}", e)
            }
        }
        Err(e) => println!("format err: {}", e)
    }
}
"#,
        "true\ntrue",
    );
}

// -----------------------------------------------------------------------
// P1: net::ip

#[test]
fn net_ip_parse_and_check() {
    assert_vm_output(
        "net_ip_parse",
        r#"
use std::net
fn main() {
    println!("{}", net::ip::is_valid("192.168.1.1"))
    println!("{}", net::ip::is_valid("not-an-ip"))
    println!("{}", net::ip::is_v4("10.0.0.1"))
    println!("{}", net::ip::is_v6("::1"))
    match net::ip::parse("127.0.0.1") {
        Ok(ip) => println!("{}", net::ip::is_loopback(ip)),
        Err(e) => println!("err: {}", e)
    }
}
"#,
        "true\nfalse\ntrue\ntrue\ntrue",
    );
}

// -----------------------------------------------------------------------
// P1: thread builtins

#[test]
fn thread_num_cpus_positive() {
    assert_vm_output(
        "thread_num_cpus",
        r#"
use std::thread
fn main() {
    let n = thread::num_cpus()
    println!("{}", n > 0)
}
"#,
        "true",
    );
}

// -----------------------------------------------------------------------
// P1: html escape / unescape

#[test]
fn html_escape_unescape() {
    assert_vm_output(
        "html_escape",
        r#"
use std::html
fn main() {
    let escaped = html::escape("<b>Hello & 'World'</b>")
    println!("{}", escaped)
    let back = html::unescape(escaped)
    println!("{}", back)
}
"#,
        "&lt;b&gt;Hello &amp; &#39;World&#39;&lt;/b&gt;\n<b>Hello & 'World'</b>",
    );
}

// -----------------------------------------------------------------------
// P2: encoding::base32

#[test]
fn encoding_base32_roundtrip() {
    assert_vm_output(
        "base32_roundtrip",
        r#"
use std::encoding
fn main() {
    let enc = encoding::base32::encode("foobar")
    println!("{}", enc)
    let dec = encoding::base32::decode_string(enc)
    match dec {
        Ok(s) => println!("{}", s),
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "MZXW6YTBOI======\nfoobar",
    );
}

// -----------------------------------------------------------------------
// P2: encoding::ascii85

#[test]
fn encoding_ascii85_roundtrip() {
    assert_vm_output(
        "ascii85_roundtrip",
        r#"
use std::encoding
fn main() {
    let enc = encoding::ascii85::encode("hello")
    let dec = encoding::ascii85::decode(enc)
    match dec {
        Ok(bytes) => println!("{}", bytes.len()),
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "5",
    );
}

// -----------------------------------------------------------------------
// P2: encoding::xml

#[test]
fn encoding_xml_escape() {
    assert_vm_output(
        "xml_escape",
        r#"
use std::encoding
fn main() {
    let s = encoding::xml::escape("<hello & world>")
    println!("{}", s)
}
"#,
        "&lt;hello &amp; world&gt;",
    );
}

#[test]
fn encoding_xml_parse_and_roundtrip() {
    assert_vm_output(
        "xml_parse",
        r#"
use std::encoding
use std::strings
fn main() {
    let src = "<root><child>text</child></root>"
    let result = encoding::xml::parse(src)
    match result {
        Ok(node) => {
            let re_encoded = encoding::xml::encode(node)
            let has_root = strings::contains(re_encoded, "root")
            println!("{}", has_root)
        }
        Err(e) => println!("err: {}", e),
    }
}
"#,
        "true",
    );
}

// -----------------------------------------------------------------------
// P2: crypto::insecure

#[test]
fn crypto_insecure_md5_hex() {
    assert_vm_output(
        "insecure_md5",
        r#"
use std::crypto
fn main() {
    let h = crypto::insecure::md5_hex("")
    println!("{}", h)
}
"#,
        "d41d8cd98f00b204e9800998ecf8427e",
    );
}

#[test]
fn crypto_insecure_sha1_hex() {
    assert_vm_output(
        "insecure_sha1",
        r#"
use std::crypto
fn main() {
    let h = crypto::insecure::sha1_hex("abc")
    println!("{}", h)
}
"#,
        "a9993e364706816aba3e25717850c26c9cd0d89d",
    );
}

// -----------------------------------------------------------------------
// P2: compress::bzip2

#[test]
fn compress_bzip2_roundtrip() {
    assert_vm_output(
        "bzip2_roundtrip",
        r#"
use std::compress
fn main() {
    let data = "hello, gossamer lang!"
    let enc = compress::bzip2::compress(data, 6)
    match enc {
        Ok(compressed) => {
            let dec = compress::bzip2::decompress(compressed)
            match dec {
                Ok(bytes) => println!("{}", bytes.len()),
                Err(e) => println!("dec err: {}", e),
            }
        }
        Err(e) => println!("enc err: {}", e),
    }
}
"#,
        "21",
    );
}

// -----------------------------------------------------------------------
// P2: math::big

#[test]
fn math_big_int_arithmetic() {
    assert_vm_output(
        "big_int_arith",
        r#"
use std::math
fn main() {
    let a = math::big::int_from_i64(1000000000)
    let b = math::big::int_from_i64(1000000000)
    let c = math::big::int_mul(a, b)
    println!("{}", c)
    let d = math::big::int_add(c, math::big::int_from_i64(1))
    println!("{}", d)
}
"#,
        "1000000000000000000\n1000000000000000001",
    );
}

#[test]
fn math_big_factorial() {
    assert_vm_output(
        "big_factorial",
        r#"
use std::math
fn main() {
    let f = math::big::factorial(20)
    println!("{}", f)
}
"#,
        "2432902008176640000",
    );
}

#[test]
fn math_big_uint_pow_mod() {
    assert_vm_output(
        "big_uint_pow_mod",
        r#"
use std::math
fn main() {
    let base = math::big::uint_from_u64(2)
    let exp = math::big::uint_from_u64(10)
    let modulus = math::big::uint_from_u64(1000)
    let result = math::big::uint_pow_mod(base, exp, modulus)
    println!("{}", result)
}
"#,
        "24",
    );
}
