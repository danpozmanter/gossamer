//! Linker / static-assembly tests.

use gossamer_driver::{
    ARTIFACT_MAGIC, LinkerOptions, TargetTriple, compile_source, fingerprint, link,
};

#[test]
fn release_options_shrink_the_artifact() {
    let source = "fn main() -> i64 { 0i64 }\nfn unused() -> i64 { 99i64 }\n";
    let default = LinkerOptions::default();
    let release = LinkerOptions::default().release();
    let unoptimised = compile_source(source, "small", &default);
    let optimised = compile_source(source, "small", &release);
    assert!(
        optimised.bytes.len() < unoptimised.bytes.len(),
        "release build should be smaller: {} vs {}",
        optimised.bytes.len(),
        unoptimised.bytes.len()
    );
}

#[test]
fn dce_drops_unreachable_functions() {
    let source = "fn main() -> i64 { 0i64 }\nfn unused() -> i64 { 1i64 }\n";
    let options = LinkerOptions::default().release();
    let artifact = compile_source(source, "dce", &options);
    let names: Vec<_> = artifact
        .symbols
        .iter()
        .map(|s| s.name.as_str())
        .collect();
    assert!(names.contains(&"main"));
    assert!(
        !names.contains(&"unused"),
        "unreachable `unused` should be pruned"
    );
}

#[test]
fn compact_symbols_are_shorter_than_verbose() {
    let source = "fn main_with_a_long_name() -> i64 { 0i64 }\n";
    let verbose_opts = LinkerOptions {
        entry: Some("main_with_a_long_name".to_string()),
        ..LinkerOptions::default()
    };
    let compact_opts = LinkerOptions {
        entry: Some("main_with_a_long_name".to_string()),
        compact_symbols: true,
        ..LinkerOptions::default()
    };
    let verbose = compile_source(source, "longmodulename", &verbose_opts);
    let compact = compile_source(source, "longmodulename", &compact_opts);
    let verbose_mangled = verbose
        .symbols
        .iter()
        .map(|s| s.mangled.len())
        .max()
        .unwrap_or(0);
    let compact_mangled = compact
        .symbols
        .iter()
        .map(|s| s.mangled.len())
        .max()
        .unwrap_or(0);
    assert!(
        compact_mangled < verbose_mangled,
        "compact mangled length {compact_mangled} should be < verbose {verbose_mangled}"
    );
}

#[test]
fn linker_merges_symbols_from_a_single_unit() {
    let artifact = compile_source(
        "fn main() -> i64 { 0i64 }\nfn helper() -> i64 { 1i64 }\n",
        "mainmod",
        &LinkerOptions::default(),
    );
    assert_eq!(artifact.symbols.len(), 2);
    let names: Vec<_> = artifact.symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"main"));
    assert!(names.contains(&"helper"));
}

#[test]
fn artifact_bytes_start_with_magic_header() {
    let artifact = compile_source(
        "fn main() -> i64 { 0i64 }\n",
        "hdr",
        &LinkerOptions::default(),
    );
    assert_eq!(&artifact.bytes[..8], ARTIFACT_MAGIC);
}

#[test]
fn linker_output_is_deterministic_across_runs() {
    let options = LinkerOptions::default();
    let source = "fn main() -> i64 { 1i64 + 2i64 }\n";
    let a = compile_source(source, "determ", &options);
    let b = compile_source(source, "determ", &options);
    assert_eq!(a.bytes, b.bytes);
    assert_eq!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn linker_output_reflects_entry_point() {
    let opts_with_main = LinkerOptions::default();
    let opts_no_entry = LinkerOptions {
        entry: None,
        target: TargetTriple::host(),
        runtime: None,
        dead_code_elim: false,
        compact_symbols: false,
    };
    let artifact_a = compile_source("fn main() -> i64 { 0i64 }\n", "m", &opts_with_main);
    let artifact_b = compile_source("fn main() -> i64 { 0i64 }\n", "m", &opts_no_entry);
    assert_ne!(artifact_a.bytes, artifact_b.bytes);
    assert_eq!(artifact_a.entry.as_deref(), Some("main"));
    assert!(artifact_b.entry.is_none());
}

#[test]
fn linking_multiple_units_preserves_symbol_ordering() {
    // Build two translation units with the same function name.
    let opts = LinkerOptions::default();
    let unit_a = compile_source("fn greet() -> i64 { 1i64 }\n", "alpha", &opts);
    let unit_b = compile_source("fn greet() -> i64 { 2i64 }\n", "beta", &opts);
    // Manually combine the emitted modules.
    let units = vec![
        gossamer_driver::TranslationUnit {
            name: "alpha".to_string(),
            module: reassemble_module(&unit_a),
        },
        gossamer_driver::TranslationUnit {
            name: "beta".to_string(),
            module: reassemble_module(&unit_b),
        },
    ];
    let combined = link(&units, &opts);
    assert_eq!(combined.symbols.len(), 2);
    // Because mangling includes the unit tag, the two `greet`
    // symbols coexist and sort deterministically by mangled name.
    let mangled: Vec<_> = combined
        .symbols
        .iter()
        .map(|s| s.mangled.as_str())
        .collect();
    assert_eq!(mangled, ["gos_alpha_greet", "gos_beta_greet"]);
}

fn reassemble_module(artifact: &gossamer_driver::Artifact) -> gossamer_codegen_cranelift::Module {
    // For the test we don't care about the text payload; a synthetic
    // empty function per symbol is enough to exercise the linker's
    // dedup/sort logic.
    let functions = artifact
        .symbols
        .iter()
        .map(|sym| gossamer_codegen_cranelift::FunctionText {
            name: sym.name.clone(),
            text: String::new(),
            arity: sym.arity,
            block_count: sym.blocks,
        })
        .collect();
    gossamer_codegen_cranelift::Module { functions }
}

#[test]
fn target_triple_host_is_non_empty() {
    let triple = TargetTriple::host();
    assert!(!triple.as_str().is_empty());
}

#[test]
fn fingerprint_differs_on_source_change() {
    let a = compile_source(
        "fn main() -> i64 { 1i64 }\n",
        "fp",
        &LinkerOptions::default(),
    );
    let b = compile_source(
        "fn main() -> i64 { 2i64 }\n",
        "fp",
        &LinkerOptions::default(),
    );
    assert_ne!(fingerprint(&a), fingerprint(&b));
}

#[test]
fn channel_program_compiles_natively() {
    // Regression: `channel()` must lower through native codegen
    // so the canonical goroutine + channel example builds. Prior
    // to this fix `gos build` errored with "unresolved callee
    // `channel`" because only `sync::channel` was wired.
    let source = r"
use std::sync::channel
fn main() {
    let pair = channel()
    let tx = pair.0
    let rx = pair.1
    tx.send(7i64)
    let _ = rx.recv()
}
";
    let artifact = compile_source(source, "chan", &LinkerOptions::default());
    assert!(
        !artifact.bytes.is_empty(),
        "channel program should produce a non-empty artifact"
    );
}
