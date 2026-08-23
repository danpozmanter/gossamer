//! Keeps the weekly Miri shard runnable.
//!
//! Miri interprets MIR: it has no socket or process syscalls, and one
//! unsupported operation aborts the whole test binary, so every test after it
//! in the shard stops running too. A test that needs a real socket or a child
//! process must therefore carry a Miri gate, and a test that hands a
//! `gos_rt_*` string parameter a bare `CString` breaks the same shard a
//! different way - the C ABI reads a length header that a host C string does
//! not carry, so the read lands before the allocation.
//!
//! Both are invisible until the weekly job runs. This guard makes them fail in
//! the ordinary suite, naming the test and the fix.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .to_path_buf()
}

/// The crates the Miri workflow runs, read from the workflow itself so this
/// guard covers exactly what that job covers.
fn miri_crates(root: &Path) -> Vec<String> {
    let workflow = std::fs::read_to_string(root.join(".github/workflows/miri.yml"))
        .expect("read .github/workflows/miri.yml");
    let crates: Vec<String> = workflow
        .lines()
        .filter_map(|line| line.trim().strip_prefix("crates:"))
        .flat_map(|rest| rest.split_whitespace().map(str::to_string))
        .collect();
    assert!(
        !crates.is_empty(),
        "the Miri workflow must name the crates it runs"
    );
    crates
}

/// Calls into OS facilities the MIR interpreter cannot execute.
const UNSUPPORTED_CALLS: &[&str] = &[
    "TcpListener::bind",
    "TcpStream::connect",
    "TcpStream::connect_timeout",
    "UdpSocket::bind",
    "UnixListener::bind",
    "UnixStream::connect",
    "Command::new",
];

/// One `fn` item: the attributes written above it, and its body.
struct Item {
    name: String,
    attrs: String,
    body: String,
}

/// The source with comments and literal contents blanked to spaces, so brace
/// matching and call detection never see punctuation spelled inside a string
/// or a comment. Byte offsets are preserved.
fn code_mask(src: &str) -> String {
    let bytes = src.as_bytes();
    let mut out = vec![b' '; bytes.len()];
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'/' if bytes[i..].starts_with(b"//") => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if bytes[i..].starts_with(b"/*") => {
                let mut depth = 1;
                i += 2;
                while i < bytes.len() && depth > 0 {
                    if bytes[i..].starts_with(b"/*") {
                        depth += 1;
                        i += 2;
                    } else if bytes[i..].starts_with(b"*/") {
                        depth -= 1;
                        i += 2;
                    } else {
                        i += 1;
                    }
                }
            }
            b'r' if bytes[i..].starts_with(b"r\"") || bytes[i..].starts_with(b"r#\"") => {
                let hashes = bytes[i + 1..].iter().take_while(|b| **b == b'#').count();
                let open = i + 1 + hashes;
                let mut close = String::from("\"");
                close.push_str(&"#".repeat(hashes));
                i = match src[open + 1..].find(&close) {
                    Some(end) => open + 1 + end + close.len(),
                    None => bytes.len(),
                };
            }
            b'"' => {
                i += 1;
                while i < bytes.len() && bytes[i] != b'"' {
                    i += if bytes[i] == b'\\' { 2 } else { 1 };
                }
                i += 1;
            }
            b'\'' => {
                // A char literal closes within a few bytes; a lifetime or a
                // loop label never closes and stays ordinary code.
                if let Some(k) = (i + 1..bytes.len().min(i + 7)).find(|k| bytes[*k] == b'\'') {
                    i = k + 1;
                } else {
                    out[i] = bytes[i];
                    i += 1;
                }
            }
            b'\n' => {
                out[i] = b'\n';
                i += 1;
            }
            c => {
                out[i] = c;
                i += 1;
            }
        }
    }
    String::from_utf8(out).expect("blanked bytes stay valid UTF-8")
}

/// The attribute and doc-comment run written directly above `at`.
fn attrs_above(src: &str, code: &str, at: usize) -> String {
    let bytes = code.as_bytes();
    let mut start = at;
    loop {
        let mut scan = start;
        while scan > 0 && bytes[scan - 1].is_ascii_whitespace() {
            scan -= 1;
        }
        if scan == 0 {
            break;
        }
        if bytes[scan - 1] == b']' {
            let mut depth = 0usize;
            let mut k = scan;
            while k > 0 {
                k -= 1;
                match bytes[k] {
                    b']' => depth += 1,
                    b'[' => {
                        depth -= 1;
                        if depth == 0 {
                            break;
                        }
                    }
                    _ => {}
                }
            }
            if depth != 0 || k == 0 || bytes[k - 1] != b'#' {
                break;
            }
            start = k - 1;
            continue;
        }
        // A doc comment is blanked in the mask, so a line that is all spaces
        // there but not in the source is a comment line above the item.
        let line_start = src[..scan].rfind('\n').map_or(0, |n| n + 1);
        if src[line_start..scan].trim_start().starts_with("//") {
            start = line_start;
            continue;
        }
        break;
    }
    src[start..at].to_string()
}

/// Every `fn` item in a file, with its attributes and its body.
fn items(src: &str) -> Vec<Item> {
    let code = code_mask(src);
    let bytes = code.as_bytes();
    let mut out = Vec::new();
    let mut search = 0;
    while let Some(offset) = code[search..].find("fn ") {
        let start = search + offset;
        search = start + 3;
        if start > 0 && (bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_') {
            continue;
        }
        let name: String = code[start + 3..]
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() {
            continue;
        }
        let Some(open) = code[start..].find('{') else {
            continue;
        };
        let body_start = start + open;
        let mut depth = 0usize;
        let mut end = code.len();
        for (k, c) in code[body_start..].char_indices() {
            match c {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = body_start + k + 1;
                        break;
                    }
                }
                _ => {}
            }
        }
        out.push(Item {
            name,
            attrs: attrs_above(src, &code, start),
            body: code[body_start..end].to_string(),
        });
        search = end.max(search);
    }
    out
}

/// Whether `body` calls `name`.
fn calls(body: &str, name: &str) -> bool {
    let bytes = body.as_bytes();
    let mut from = 0;
    while let Some(at) = body[from..].find(name) {
        let start = from + at;
        from = start + name.len();
        let boundary =
            start == 0 || !(bytes[start - 1].is_ascii_alphanumeric() || bytes[start - 1] == b'_');
        if boundary && body[from..].trim_start().starts_with('(') {
            return true;
        }
    }
    false
}

/// Which items reach an unsupported call, directly or through another item in
/// the same file.
fn reaching_items(items: &[Item]) -> Vec<bool> {
    let mut reaches: Vec<bool> = items
        .iter()
        .map(|item| {
            UNSUPPORTED_CALLS
                .iter()
                .any(|call| item.body.contains(call))
        })
        .collect();
    loop {
        let mut changed = false;
        for i in 0..items.len() {
            if reaches[i] {
                continue;
            }
            let via_helper = (0..items.len())
                .any(|j| i != j && reaches[j] && calls(&items[i].body, &items[j].name));
            if via_helper {
                reaches[i] = true;
                changed = true;
            }
        }
        if !changed {
            return reaches;
        }
    }
}

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

fn miri_crate_sources(root: &Path) -> Vec<PathBuf> {
    let mut sources = Vec::new();
    for name in miri_crates(root) {
        rust_sources(&root.join("crates").join(name).join("src"), &mut sources);
    }
    sources
}

fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[test]
fn every_socket_or_subprocess_test_in_a_miri_crate_is_gated() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in miri_crate_sources(&root) {
        let src = std::fs::read_to_string(&path).expect("read source");
        if !src.contains("#[test]") {
            continue;
        }
        let items = items(&src);
        let reaches = reaching_items(&items);
        for (item, unsupported) in items.iter().zip(&reaches) {
            if *unsupported && item.attrs.contains("#[test]") && !item.attrs.contains("miri") {
                violations.push(format!("{}: {}", shown(&root, &path), item.name));
            }
        }
    }
    assert!(
        violations.is_empty(),
        "Miri has no socket or process syscalls, and one unsupported operation \
         aborts the rest of the shard. Drive the code under test through an \
         in-memory transport, or add `#[cfg_attr(miri, ignore)]` naming the \
         reason:\n  {}",
        violations.join("\n  ")
    );
}

/// The identifiers a body binds from `CString::new(..)` or `CStr::from_bytes*`.
fn host_cstring_bindings(body: &str) -> Vec<String> {
    body.lines()
        .filter(|line| line.contains("CString::new") || line.contains("CStr::from_bytes"))
        .filter_map(|line| {
            let rest = line.trim_start().strip_prefix("let ")?;
            let name: String = rest
                .trim_start_matches("mut ")
                .chars()
                .take_while(|c| c.is_alphanumeric() || *c == '_')
                .collect();
            (!name.is_empty()).then_some(name)
        })
        .collect()
}

/// The argument text of every `gos_rt_*` call in `body`.
fn gos_rt_call_arguments(body: &str) -> Vec<String> {
    let bytes = body.as_bytes();
    let mut out = Vec::new();
    let mut from = 0;
    while let Some(at) = body[from..].find("gos_rt_") {
        let start = from + at;
        from = start + "gos_rt_".len();
        let Some(open) = body[start..].find('(') else {
            break;
        };
        let open = start + open;
        let mut depth = 0usize;
        for k in open..body.len() {
            match bytes[k] {
                b'(' => depth += 1,
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        out.push(body[open + 1..k].to_string());
                        from = k;
                        break;
                    }
                }
                _ => {}
            }
        }
    }
    out
}

#[test]
fn no_caller_hands_a_bare_cstring_to_a_gos_rt_parameter() {
    let root = workspace_root();
    let mut violations = Vec::new();
    for path in miri_crate_sources(&root) {
        let src = std::fs::read_to_string(&path).expect("read source");
        for item in items(&src) {
            let bindings = host_cstring_bindings(&item.body);
            if bindings.is_empty() {
                continue;
            }
            for args in gos_rt_call_arguments(&item.body) {
                for name in &bindings {
                    if args.contains(&format!("{name}.as_ptr()")) {
                        violations.push(format!(
                            "{}: {} passes `{name}` to a gos_rt_ parameter",
                            shown(&root, &path),
                            item.name
                        ));
                    }
                }
            }
        }
    }
    assert!(
        violations.is_empty(),
        "a `gos_rt_*` string parameter is read through the length header that \
         sits before it, which a host `CString` does not carry - build the \
         argument with `string::test_gos_str` / `test_gos_ptr`:\n  {}",
        violations.join("\n  ")
    );
}
