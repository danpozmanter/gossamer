//! CI gate: every `unsafe` block in the runtime's safe modules
//! must have a `// SAFETY:` comment within the 8 lines preceding
//! it, OR sit inside an `unsafe fn` whose contract documents the
//! invariants (the FFI surface).
//!
//! The `c_abi/` tree is the FFI boundary - every function there is
//! `pub unsafe extern "C"`, so the function's own `unsafe` keyword
//! is the safety contract and inline `unsafe { ... }` blocks
//! inherit it. The test only enforces SAFETY comments on unsafe
//! blocks living in *non-`c_abi`* runtime files: that's the surface
//! where the audit's "every new unsafe needs a doc-comment" rule
//! is interesting.

use std::fs;
use std::path::{Path, PathBuf};

fn walk(dir: &Path, into: &mut Vec<PathBuf>) {
    if let Ok(rd) = fs::read_dir(dir) {
        for entry in rd.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, into);
            } else if path.extension().is_some_and(|e| e == "rs") {
                into.push(path);
            }
        }
    }
}

fn unsafe_block_lines(text: &str) -> Vec<usize> {
    text.lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim_start();
            if trimmed.starts_with("unsafe {")
                || trimmed == "unsafe"
                || trimmed.starts_with("unsafe ")
                    && trimmed.contains('{')
                    && !trimmed.contains("fn ")
                    && !trimmed.contains("impl ")
                    && !trimmed.contains("trait ")
                    && !trimmed.contains("extern ")
            {
                Some(i)
            } else {
                None
            }
        })
        .collect()
}

fn has_safety_comment_within(text: &str, line_idx: usize) -> bool {
    let lines: Vec<&str> = text.lines().collect();
    let lo = line_idx.saturating_sub(8);
    for line in &lines[lo..line_idx] {
        if line.contains("SAFETY") || line.contains("Safety:") {
            return true;
        }
    }
    false
}

#[test]
fn every_unsafe_block_outside_c_abi_has_safety_comment() {
    let runtime_src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    walk(&runtime_src, &mut files);

    let mut violations: Vec<String> = Vec::new();
    for path in &files {
        let rel = path.strip_prefix(&runtime_src).unwrap_or(path);
        let rel_str = rel.to_string_lossy();

        // c_abi/ - every function is `pub unsafe extern "C"`; the
        // contract is on the function itself, inline blocks
        // inherit. Skip.
        if rel_str.starts_with("c_abi") {
            continue;
        }
        // ffi.rs at the top level is also part of the FFI surface.
        if rel_str == "ffi.rs" {
            continue;
        }

        let Ok(text) = fs::read_to_string(path) else {
            continue;
        };
        for line_idx in unsafe_block_lines(&text) {
            if !has_safety_comment_within(&text, line_idx) {
                violations.push(format!(
                    "{}:{}: unsafe block without // SAFETY: comment within 8 lines above",
                    rel_str,
                    line_idx + 1
                ));
            }
        }
    }

    if !violations.is_empty() {
        let joined = violations.join("\n  ");
        panic!(
            "audit-unsafe: {} unsafe block(s) lack a SAFETY comment:\n  {}",
            violations.len(),
            joined
        );
    }
}
