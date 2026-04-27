//!: registry coverage + filesystem round-trip.

use std::env;

use gossamer_std::{
    fmt as gfmt,
    io::{InMemoryReader, InMemoryWriter, Reader, Writer},
    item, module, modules, os,
    registry::StdItemKind,
};

#[test]
fn registry_lists_phase_22_modules() {
    for path in ["std::fmt", "std::io", "std::os"] {
        assert!(module(path).is_some(), "missing {path}");
    }
}

#[test]
fn fmt_module_exposes_println_and_traits() {
    let m = module("std::fmt").unwrap();
    assert!(m.items.iter().any(|i| i.name == "println"));
    let display = m.items.iter().find(|i| i.name == "Display").unwrap();
    assert_eq!(display.kind, StdItemKind::Trait);
}

#[test]
fn io_module_exposes_buffered_wrappers() {
    let m = module("std::io").unwrap();
    assert!(m.items.iter().any(|i| i.name == "BufReader"));
    assert!(m.items.iter().any(|i| i.name == "BufWriter"));
}

#[test]
fn os_module_lists_filesystem_helpers() {
    let m = module("std::os").unwrap();
    let names: Vec<_> = m.items.iter().map(|i| i.name).collect();
    for expected in [
        "args",
        "env",
        "exit",
        "open",
        "create",
        "read_file",
        "write_file",
        "exists",
        "mkdir",
        "mkdir_all",
        "read_dir",
        "File",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }
}

#[test]
fn item_lookup_finds_qualified_names() {
    let (_m, item_decl) = item("std::fmt::println").expect("println registered");
    assert_eq!(item_decl.name, "println");
    assert_eq!(item_decl.kind, StdItemKind::Macro);
    assert!(item("std::fmt::nope").is_none());
}

#[test]
fn modules_are_listed_in_phase_introduction_order() {
    let paths: Vec<_> = modules().iter().map(|m| m.path).collect();
    let phase22_idx = paths.iter().position(|p| *p == "std::fmt").unwrap();
    let phase23_idx = paths.iter().position(|p| *p == "std::collections").unwrap();
    let phase24_idx = paths.iter().position(|p| *p == "std::net").unwrap();
    let phase25_idx = paths
        .iter()
        .position(|p| *p == "std::encoding::json")
        .unwrap();
    let phase26_idx = paths.iter().position(|p| *p == "std::sync").unwrap();
    assert!(phase22_idx < phase23_idx);
    assert!(phase23_idx < phase24_idx);
    assert!(phase24_idx < phase25_idx);
    assert!(phase25_idx < phase26_idx);
}

#[test]
fn fmt_helpers_format_basic_primitives() {
    assert_eq!(gfmt::format_int(42), "42");
    assert_eq!(gfmt::format_int(-7), "-7");
    assert_eq!(gfmt::format_bool(true), "true");
    assert_eq!(gfmt::format_bool(false), "false");
    assert_eq!(gfmt::join_with_spaces(["a", "b", "c"]), "a b c");
}

#[test]
fn in_memory_writer_collects_bytes() {
    let mut w = InMemoryWriter::default();
    w.write_all(b"hello, ").unwrap();
    w.write_all(b"world").unwrap();
    w.flush().unwrap();
    assert_eq!(w.buffer, b"hello, world");
}

#[test]
fn in_memory_reader_drains_to_eof() {
    let mut r = InMemoryReader::new(b"abc".to_vec());
    let mut buf = [0u8; 2];
    let n = r.read(&mut buf).unwrap();
    assert_eq!(n, 2);
    assert_eq!(&buf[..n], b"ab");
    let n = r.read(&mut buf).unwrap();
    assert_eq!(n, 1);
    assert_eq!(&buf[..n], b"c");
    let n = r.read(&mut buf).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn os_filesystem_round_trip_against_tmp_dir() {
    let mut dir = env::temp_dir();
    dir.push("gossamer-std-phase22");
    let _ = os::remove_file(dir.to_str().unwrap());
    os::mkdir_all(dir.to_str().unwrap()).expect("mkdir_all");
    let path = dir.join("hello.txt");
    let path = path.to_str().unwrap();
    os::write_file(path, b"hi from gossamer").unwrap();
    assert!(os::exists(path));
    let bytes = os::read_file(path).unwrap();
    assert_eq!(bytes, b"hi from gossamer");
    let text = os::read_file_to_string(path).unwrap();
    assert_eq!(text, "hi from gossamer");
    let listing = os::read_dir(dir.to_str().unwrap()).unwrap();
    assert!(listing.iter().any(|e| e == "hello.txt"));
    os::remove_file(path).unwrap();
    assert!(!os::exists(path));
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn os_set_env_round_trips_through_safe_runtime_wrapper() {
    let key = "GOSSAMER_PHASE22_SET_ENV";
    os::set_env(key, "ok").expect("set_env should now succeed via safe wrapper");
    assert_eq!(os::env(key).as_deref(), Some("ok"));
    os::unset_env(key);
    assert_eq!(os::env(key), None);
}

#[test]
fn os_args_returns_at_least_the_executable_path() {
    let argv = os::args();
    assert!(!argv.is_empty());
}
