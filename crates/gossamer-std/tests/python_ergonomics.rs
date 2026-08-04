//! Regression coverage for the Python-ergonomics surface:
//! - `regex::captures_named` / `regex::capture_names` (named
//!   group access on the existing regex API).
//! - `fs::TempDir` (RAII temp directory) and `fs::temp_file`
//!   (uniquely-named scratch file).
//! - `path::Path` value-type methods (suffix / parent / etc).

use std::io::Write;

use gossamer_std::fs;
use gossamer_std::path;
use gossamer_std::regex;

#[test]
fn regex_named_groups_round_trip() {
    let pat = regex::compile(r"(?P<year>\d{4})-(?P<month>\d{2})-(?P<day>\d{2})").expect("compile");
    let caps = regex::captures_named(&pat, "2026-05-18").expect("match");
    assert_eq!(caps.get("year").map(String::as_str), Some("2026"));
    assert_eq!(caps.get("month").map(String::as_str), Some("05"));
    assert_eq!(caps.get("day").map(String::as_str), Some("18"));
    let names = regex::capture_names(&pat);
    assert!(names.contains(&"year".to_string()));
    assert!(names.contains(&"month".to_string()));
    assert!(names.contains(&"day".to_string()));
}

#[test]
fn regex_named_groups_all_iterates_each_match() {
    let pat = regex::compile(r"(?P<k>\w+)=(?P<v>\w+)").expect("compile");
    let rows = regex::captures_named_all(&pat, "a=1 b=2 c=3");
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0].get("k").map(String::as_str), Some("a"));
    assert_eq!(rows[2].get("v").map(String::as_str), Some("3"));
}

#[test]
fn tempdir_creates_and_removes_on_drop() {
    let path;
    {
        let td = fs::TempDir::new().expect("tempdir");
        path = td.path().to_path_buf();
        assert!(path.exists(), "tempdir not created");
        // Drop the wrapper - path should be removed.
    }
    assert!(!path.exists(), "tempdir not cleaned up on drop");
}

#[test]
fn tempdir_into_path_preserves_directory() {
    let td = fs::TempDir::with_prefix("keep").expect("tempdir");
    let path = td.into_path();
    assert!(path.exists(), "into_path should not remove the directory");
    let _ = std::fs::remove_dir_all(&path);
}

#[test]
fn tempfile_returns_unique_writable_handle() {
    let (mut f1, p1) = fs::temp_file("u").expect("file1");
    let (mut f2, p2) = fs::temp_file("u").expect("file2");
    assert_ne!(p1, p2, "two temp files must have distinct paths");
    f1.write_all(b"hello").unwrap();
    f2.write_all(b"world").unwrap();
    drop(f1);
    drop(f2);
    let _ = std::fs::remove_file(&p1);
    let _ = std::fs::remove_file(&p2);
}

#[test]
fn path_prefixes_match_component_semantics() {
    assert_eq!(
        path::prefixes("/a//./b/c"),
        vec!["/", "/a", "/a/b", "/a/b/c"]
    );
    assert_eq!(
        path::prefixes("./a/../b"),
        vec![".", "./a", "./a/..", "./a/../b"]
    );
    assert!(path::prefixes("").is_empty());
}
