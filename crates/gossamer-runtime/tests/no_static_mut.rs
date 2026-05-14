//! Structural invariant: `gossamer-runtime` contains no production
//! `static mut`.
//!
//! Rationale: the runtime's mutable globals are the canonical
//! Rust-2024 unsoundness surface. The 0.5.0 OWNERSHIP item retired
//! every `static mut` in production code by wrapping the storage
//! in `UnsafeCell` + an explicit `Sync` impl that documents the
//! serialization contract. This test scans the crate's source
//! files and fails if any new `static mut` is introduced.
//!
//! `#[cfg(test)]` modules and the existing test crate fixtures are
//! exempt; user code that compiles Gossamer-level `static mut`
//! declarations as i64 storage is unrelated to Rust-level static
//! state and lives outside this crate anyway.

#![allow(missing_docs)]

use std::path::{Path, PathBuf};

#[test]
fn crate_source_contains_no_production_static_mut() {
    let crate_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let src_dir = crate_root.join("src");
    let mut offenders: Vec<String> = Vec::new();
    walk_rs(&src_dir, &mut |path, body| {
        // Track `#[cfg(test)]` regions: anything inside one is
        // exempt from the no-`static mut` rule.
        let mut in_cfg_test = false;
        let mut cfg_test_brace_depth: i32 = 0;
        let mut brace_depth: i32 = 0;
        for (i, line) in body.lines().enumerate() {
            // Update brace depth before checking patterns so the
            // closing brace of a `cfg(test)` module exits the
            // region on the correct line.
            for ch in line.chars() {
                if ch == '{' {
                    brace_depth += 1;
                } else if ch == '}' {
                    brace_depth -= 1;
                    if in_cfg_test && brace_depth < cfg_test_brace_depth {
                        in_cfg_test = false;
                    }
                }
            }
            let trimmed = line.trim_start();
            if trimmed.starts_with("#[cfg(test)]") {
                in_cfg_test = true;
                cfg_test_brace_depth = brace_depth + 1;
                continue;
            }
            if in_cfg_test {
                continue;
            }
            // Skip comment lines so commentary mentioning the old
            // shape does not trip the audit.
            if trimmed.starts_with("//") || trimmed.starts_with("///") || trimmed.starts_with('*') {
                continue;
            }
            // Match `static mut` as a token sequence, not as a
            // substring; the rule does not fire on documentation
            // mentioning the words.
            let mut iter = trimmed.split_ascii_whitespace();
            let head = (iter.next(), iter.next());
            let matches_decl = matches!(head, (Some("static"), Some("mut")))
                || matches!(head, (Some("pub"), Some("static")) if iter.next() == Some("mut"));
            if matches_decl {
                offenders.push(format!(
                    "{}:{}: production `static mut` declaration",
                    path.display(),
                    i + 1,
                ));
            }
        }
    });
    assert!(
        offenders.is_empty(),
        "gossamer-runtime production code must not declare `static mut`. \
         The 0.5.0 OWNERSHIP rule wraps mutable globals in UnsafeCell with \
         an explicit Sync impl naming the serialization contract. \
         Offenders:\n{}",
        offenders.join("\n"),
    );
}

fn walk_rs(root: &Path, visit: &mut dyn FnMut(&Path, &str)) {
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|e| e == "rs") {
                let Ok(body) = std::fs::read_to_string(&path) else {
                    continue;
                };
                visit(&path, &body);
            }
        }
    }
}
