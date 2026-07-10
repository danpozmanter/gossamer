//!: registry coverage + filesystem round-trip.

use std::env;

use gossamer_std::{
    env as genv, fmt as gfmt, fs,
    io::{InMemoryReader, InMemoryWriter, Reader, Writer},
    item, module, modules,
    registry::StdItemKind,
};

#[test]
fn registry_lists_phase_22_modules() {
    for path in ["std::fmt", "std::io", "std::os", "std::fs", "std::env"] {
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
fn canonical_modules_list_system_helpers() {
    let m = module("std::os").unwrap();
    let names: Vec<_> = m.items.iter().map(|i| i.name).collect();
    for expected in ["family", "arch"] {
        assert!(names.contains(&expected), "missing {expected}");
    }

    let m = module("std::env").unwrap();
    let names: Vec<_> = m.items.iter().map(|i| i.name).collect();
    for expected in [
        "args",
        "program_name",
        "var",
        "set_var",
        "unset_var",
        "current_dir",
        "set_current_dir",
        "home_dir",
        "temp_dir",
    ] {
        assert!(names.contains(&expected), "missing {expected}");
    }

    let m = module("std::fs").unwrap();
    let names: Vec<_> = m.items.iter().map(|i| i.name).collect();
    for expected in [
        "read",
        "read_to_string",
        "write",
        "exists",
        "read_dir",
        "create_dir_all",
        "remove_file",
        "remove_dir_all",
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
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).expect("create_dir_all");
    let path = dir.join("hello.txt");
    fs::write(&path, b"hi from gossamer").unwrap();
    assert!(fs::exists(&path));
    let bytes = fs::read(&path).unwrap();
    assert_eq!(bytes, b"hi from gossamer");
    let text = fs::read_to_string(&path).unwrap();
    assert_eq!(text, "hi from gossamer");
    let listing = fs::read_dir(&dir).unwrap();
    assert!(listing.iter().any(|e| e.name == "hello.txt" && e.is_file));
    fs::remove_file(&path).unwrap();
    assert!(!fs::exists(&path));
    let _ = std::fs::remove_dir(dir);
}

#[test]
fn os_set_env_round_trips_through_safe_runtime_wrapper() {
    let key = "GOSSAMER_PHASE22_SET_ENV";
    genv::set_var(key, "ok").expect("set_var should now succeed via safe wrapper");
    assert_eq!(genv::var(key).as_deref(), Some("ok"));
    genv::unset_var(key);
    assert_eq!(genv::var(key), None);
}

#[test]
fn os_args_returns_at_least_the_executable_path() {
    let argv = genv::args();
    assert!(!argv.is_empty());
}
