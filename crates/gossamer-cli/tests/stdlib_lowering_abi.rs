//! Every compiled-tier lowering must pass the pointer its shim expects.
//!
//! A stdlib function is declared once in `stdlib_signatures.rs`, lowered to a
//! `gos_rt_*` symbol in the MIR builder, and implemented as a C-ABI shim whose
//! first parameter is either a `*const c_char` (a `String`) or a
//! `*const GosVec` (a `Vec<u8>`). Nothing in the type system ties the three
//! together, so a byte-taking function can be lowered to the string-taking
//! shim of the same family: the call then hands a vector header to a function
//! that reads it as a NUL-terminated string, which answers nonsense and reads
//! past the buffer. This test is what ties them together.

#![allow(missing_docs)]

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn read(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

fn walk(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// `module::name` -> the declared Gossamer signature.
fn declared_signatures() -> BTreeMap<String, String> {
    let src = read("crates/gossamer-types/src/stdlib_signatures.rs");
    let mut out = BTreeMap::new();
    let mut rest = src.as_str();
    while let Some(i) = rest.find("module_path:") {
        rest = &rest[i..];
        let Some(module) = between(rest, "module_path:", '"') else {
            break;
        };
        let Some(name) = between(rest, "name:", '"') else {
            break;
        };
        let Some(signature) = between(rest, "signature:", '"') else {
            break;
        };
        let leaf = module.rsplit("::").next().unwrap_or(&module).to_string();
        out.insert(format!("{leaf}::{name}"), signature);
        rest = &rest["module_path:".len()..];
    }
    out
}

/// The text between `key` and the next pair of `delim`.
fn between(src: &str, key: &str, delim: char) -> Option<String> {
    let after = &src[src.find(key)? + key.len()..];
    let start = after.find(delim)? + 1;
    let end = after[start..].find(delim)? + start;
    Some(after[start..end].to_string())
}

/// `gos_rt_*` -> its first parameter's Rust type.
fn shim_first_params() -> BTreeMap<String, String> {
    let mut files = Vec::new();
    walk(
        &repo_root().join("crates/gossamer-runtime/src/c_abi"),
        &mut files,
    );
    let mut out = BTreeMap::new();
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap_or_default();
        for chunk in src.split("pub unsafe extern \"C\" fn ").skip(1) {
            let Some(open) = chunk.find('(') else {
                continue;
            };
            let Some(close) = chunk.find(')') else {
                continue;
            };
            if close < open {
                continue;
            }
            let name = chunk[..open].trim().to_string();
            if !name.starts_with("gos_rt_") {
                continue;
            }
            let params = &chunk[open + 1..close];
            let Some(first) = params.split(',').next() else {
                continue;
            };
            let ty = first.split_once(':').map_or("", |(_, t)| t).trim();
            if !ty.is_empty() {
                out.insert(name, ty.to_string());
            }
        }
    }
    out
}

/// `module::name` -> the `gos_rt_*` symbol the MIR builder lowers it to.
fn lowerings() -> BTreeMap<String, String> {
    let mut files = Vec::new();
    walk(&repo_root().join("crates/gossamer-mir/src"), &mut files);
    let mut out = BTreeMap::new();
    for file in files {
        let src = std::fs::read_to_string(&file).unwrap_or_default();
        // The table reads `"module::name" => ("gos_rt_symbol", ty)`.
        for (idx, _) in src.match_indices("=> (\"gos_rt_") {
            let before = &src[..idx];
            let Some(q_end) = before.rfind('"') else {
                continue;
            };
            let Some(q_start) = before[..q_end].rfind('"') else {
                continue;
            };
            let call = before[q_start + 1..q_end].to_string();
            let Some(symbol) = between(&src[idx..], "=> (", '"') else {
                continue;
            };
            if call.contains("::") && !call.contains(' ') {
                out.entry(call).or_insert(symbol);
            }
        }
    }
    out
}

fn first_param(signature: &str) -> Option<String> {
    let open = signature.find('(')?;
    let close = signature.rfind(')')?;
    let inner = signature.get(open + 1..close)?.trim();
    if inner.is_empty() {
        return None;
    }
    let first = inner.split(',').next()?;
    Some(
        first
            .split_once(':')
            .map_or(first, |(_, t)| t)
            .trim()
            .to_string(),
    )
}

#[test]
fn every_lowering_passes_the_pointer_its_shim_expects() {
    let signatures = declared_signatures();
    let shims = shim_first_params();
    let lowerings = lowerings();
    assert!(
        lowerings.len() > 100,
        "the lowering table was not parsed; found {} entries",
        lowerings.len()
    );

    let mut mismatched = Vec::new();
    for (call, symbol) in &lowerings {
        let (Some(signature), Some(shim_ty)) = (signatures.get(call), shims.get(symbol)) else {
            continue;
        };
        let Some(gos_ty) = first_param(signature) else {
            continue;
        };
        let takes_bytes = gos_ty.contains("Vec<u8>") || gos_ty.starts_with("[u8]");
        let takes_string = gos_ty == "String";
        let shim_takes_vec = shim_ty.contains("GosVec");
        let shim_takes_cstr = shim_ty.contains("c_char");
        if (takes_bytes && shim_takes_cstr) || (takes_string && shim_takes_vec) {
            mismatched.push(format!(
                "  {call} takes {gos_ty} but lowers to {symbol}, whose first parameter is {shim_ty}"
            ));
        }
    }
    assert!(
        mismatched.is_empty(),
        "a compiled-tier lowering hands its shim the wrong pointer:\n{}",
        mismatched.join("\n")
    );
}
