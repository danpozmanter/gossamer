//! Build script: emits a per-compile stamp folded into the frontend-cache key.

fn main() {
    // The stamp must change whenever FRONTEND behavior changes, not just
    // this crate: watch every crate whose code shapes the cached AST.
    for dir in [
        "../gossamer-lex/src",
        "../gossamer-ast/src",
        "../gossamer-parse/src",
        "../gossamer-resolve/src",
        "../gossamer-hir/src",
        "src",
    ] {
        println!("cargo:rerun-if-changed={dir}");
    }
    // Per-compile stamp folded into the frontend-cache key: the
    // declared crate version alone is constant across development
    // rebuilds, so cached ASTs parsed by an older compiler were
    // served as fresh (stale parse-time rewrites, stale autoderive).
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos().to_string())
        .unwrap_or_default();
    println!("cargo:rustc-env=GOS_DRIVER_BUILD_STAMP={stamp}");
}
