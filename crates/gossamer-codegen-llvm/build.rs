//! Build-time cache stamp for LLVM codegen source changes.

use std::fs;
use std::path::{Path, PathBuf};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for &byte in bytes {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(FNV_PRIME);
    }
}

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=build.rs");

    let manifest_dir = PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("cargo sets CARGO_MANIFEST_DIR"),
    );
    let mut files = Vec::new();
    collect_rs_files(&manifest_dir.join("src"), &mut files);
    files.sort();

    let mut hash = FNV_OFFSET;
    for path in files {
        hash_bytes(&mut hash, path.to_string_lossy().as_bytes());
        hash_bytes(&mut hash, b"\0");
        if let Ok(bytes) = fs::read(&path) {
            hash_bytes(&mut hash, &bytes);
        }
        hash_bytes(&mut hash, b"\0");
    }

    println!("cargo:rustc-env=GOSSAMER_LLVM_CODEGEN_CACHE_STAMP={hash:016x}");
}
