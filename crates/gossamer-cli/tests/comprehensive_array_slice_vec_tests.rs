//! Rust-oracle coverage for fixed arrays, unsized slices, and growable Vecs.

use std::env;
use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

static NEXT_FIXTURE: AtomicU64 = AtomicU64::new(0);

fn diagnostics(body: &str) -> Vec<String> {
    let source = format!("fn main() {{\n{body}\n}}\n");
    let mut map = SourceMap::new();
    let file = map.add_file("comprehensive_array_slice_vec_tests.gos", source.clone());
    let (parsed, parse_diagnostics) = parse_source_file(&source, file);
    if !parse_diagnostics.is_empty() {
        return parse_diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.error.to_string())
            .collect();
    }
    let (resolutions, resolve_diagnostics) = resolve_source_file(&parsed);
    if !resolve_diagnostics.is_empty() {
        return resolve_diagnostics
            .into_iter()
            .map(|diagnostic| diagnostic.error.to_string())
            .collect();
    }
    let mut tcx = TyCtxt::new();
    let (_, type_diagnostics) = typecheck_source_file(&parsed, &resolutions, &mut tcx);
    type_diagnostics
        .into_iter()
        .map(|diagnostic| format!("{}: {}", diagnostic.error.code(), diagnostic.error))
        .collect()
}

fn assert_accepted(label: &str, body: &str) {
    let found = diagnostics(body);
    assert!(found.is_empty(), "{label} should be accepted: {found:?}");
}

fn assert_rejected(label: &str, body: &str, expected: &str) {
    let found = diagnostics(body);
    assert!(
        found.iter().any(|diagnostic| diagnostic.contains(expected)),
        "{label} should contain {expected:?}: {found:?}"
    );
}

fn rust_accepts(body: &str) -> bool {
    let unique = NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed);
    let root = env::temp_dir().join(format!(
        "gossamer-rust-sequence-oracle-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&root).expect("create Rust oracle directory");
    let source = root.join("oracle.rs");
    let binary = root.join("oracle-bin");
    fs::write(&source, format!("fn main() {{\n{body}\n}}\n")).expect("write Rust oracle");
    let output = Command::new("rustc")
        .args(["--edition=2024", "--crate-name", "sequence_oracle"])
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("rustc is required for the Rust sequence oracle");
    let _ = fs::remove_dir_all(root);
    output.status.success()
}

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

#[test]
fn rust_and_gossamer_agree_on_owned_sequence_assignments() {
    let accepted = [
        ("fixed array", "let a: [i64; 3] = [1, 2, 3]; let _b = a;"),
        (
            "explicit Vec construction",
            "let a: Vec<i64> = Vec::from([1, 2, 3]); let _b = a;",
        ),
    ];
    for (label, body) in accepted {
        assert!(rust_accepts(body), "Rust rejected oracle case {label}");
        assert_accepted(label, body);
    }

    assert!(
        !rust_accepts("let _a: Vec<i64> = [1, 2, 3];"),
        "Rust unexpectedly converted an array to Vec"
    );
    assert_rejected(
        "array does not become Vec",
        "let _a: Vec<i64> = [1, 2, 3]",
        "expected `Vec<i64>`, found `[i64; 3]`",
    );
    assert_rejected("owned slice local", "let _a: [i64] = [1, 2, 3]", "GT0049");
}

#[test]
fn all_four_rust_unsizing_coercions_are_accepted() {
    let gossamer = "    let array = [1, 2, 3];\n\
        let array_slice: &[i64] = &array;\n\
        let vector: Vec<i64> = Vec::from([4, 5, 6]);\n\
        let vec_slice: &[i64] = &vector;\n\
        let mut mutable_array = [7, 8, 9];\n\
        let mutable_array_slice: &mut [i64] = &mut mutable_array;\n\
        mutable_array_slice[0] = 10;\n\
        let mut mutable_vector: Vec<i64> = Vec::from([11, 12, 13]);\n\
        let mutable_vec_slice: &mut [i64] = &mut mutable_vector;\n\
        mutable_vec_slice[0] = 14;\n\
        let _sum = array_slice[0] + vec_slice[0] + mutable_array_slice[0] + mutable_vec_slice[0];";
    assert!(rust_accepts(gossamer), "Rust rejected the coercion oracle");
    assert_accepted("all four slice-reference coercions", gossamer);
}

#[test]
fn parameters_returns_and_owned_assignments_keep_each_sequence_identity() {
    let accepted = "fn array_id(value: [i64; 3]) -> [i64; 3] { value }\n\
        fn slice_len(value: &[i64]) -> usize { value.len() as usize }\n\
        fn slice_set(value: &mut [i64]) { value[0] = 9 }\n\
        fn vec_id(value: Vec<i64>) -> Vec<i64> { value }\n\
        let fixed = array_id([1, 2, 3]);\n\
        let mut fixed_mut = fixed;\n\
        slice_set(&mut fixed_mut);\n\
        let grown = vec_id(Vec::from([4, 5, 6]));\n\
        let _total = slice_len(&fixed_mut) + slice_len(&grown);";
    assert!(rust_accepts(accepted), "Rust rejected the parameter oracle");
    assert_accepted("array, slice, and Vec parameters and returns", accepted);

    assert!(
        !rust_accepts("fn bad() -> [i64] { [1, 2, 3] }"),
        "Rust unexpectedly accepted an unsized slice return"
    );
    assert_rejected(
        "unsized slice return",
        "fn bad() -> [i64] { [1, 2, 3] }",
        "GT0049",
    );
    assert_rejected(
        "Vec return does not accept an array",
        "fn bad() -> Vec<i64> { [1, 2, 3] }",
        "expected `Vec<i64>`, found `[i64; 3]`",
    );
}

#[test]
fn slices_and_arrays_reject_every_vec_only_operation() {
    let methods = [
        "push(4)",
        "pop()",
        "insert(0, 4)",
        "remove(0)",
        "clear()",
        "truncate(1)",
        "extend(Vec::from([4]))",
        "reserve(8)",
        "capacity()",
        "shrink_to_fit()",
        "drain()",
    ];
    for method in methods {
        assert_rejected(
            &format!("array {method}"),
            &format!("let mut values = [1, 2, 3]\nvalues.{method}"),
            "GT0050",
        );
        assert_rejected(
            &format!("slice {method}"),
            &format!(
                "let mut storage = [1, 2, 3]\nlet values: &mut [i64] = &mut storage\nvalues.{method}"
            ),
            "GT0050",
        );
    }
}

#[test]
fn collection_methods_do_not_leak_eager_vec_combinators_into_arrays_or_slices() {
    for method in [
        "sum()",
        "take(2)",
        "filter(|value| value > 1)",
        "fold(0, |a, b| a + b)",
    ] {
        assert!(
            !rust_accepts(&format!(
                "let values = [1_i64, 2, 3]; let _ = values.{method};"
            )),
            "Rust unexpectedly accepted array.{method}"
        );
        assert_rejected(
            &format!("array {method}"),
            &format!("let values = [1, 2, 3]\nvalues.{method}"),
            "no method named",
        );
        assert_rejected(
            &format!("slice {method}"),
            &format!("let storage = [1, 2, 3]\nlet values: &[i64] = &storage\nvalues.{method}"),
            "no method named",
        );
    }

    assert_accepted(
        "Vec eager combinators remain Vec methods",
        "let values: Vec<i64> = Vec::from([1, 2, 3])\nlet _sum = values.sum()\nlet _head = values.take(2)",
    );
    assert_accepted(
        "fixed array clone preserves its fixed type",
        "let values = [1, 2, 3]\nlet copy: [i64; 3] = values.clone()",
    );
}

#[test]
fn every_cataloged_shared_sequence_method_typechecks_on_its_valid_receiver() {
    let immutable_calls = [
        "values.len()",
        "values.is_empty()",
        "values.slice(0, 2)",
        "values.first()",
        "values.last()",
        "values.get(1)",
        "values.contains(2)",
        "values.index_of(2)",
        "values.count_of(2)",
        "values.windows(2)",
        "values.chunks(2)",
        "values.join(\",\")",
        "values.to_vec()",
        "values.iter()",
    ];
    for call in immutable_calls {
        assert_accepted(
            &format!("fixed array shared method {call}"),
            &format!("let values = [1, 2, 3]\nlet _result = {call}"),
        );
        assert_accepted(
            &format!("slice shared method {call}"),
            &format!(
                "let storage = [1, 2, 3]\nlet values: &[i64] = &storage\nlet _result = {call}"
            ),
        );
    }

    let mutable_calls = [
        "values.sort()",
        "values.sort_by(|a, b| a - b)",
        "values.sort_by_key(|value| value)",
        "values.reverse()",
        "values.swap(0, 2)",
        "values.fill(7)",
    ];
    for call in mutable_calls {
        assert_accepted(
            &format!("mutable array shared method {call}"),
            &format!("let mut values = [3, 1, 2]\nlet _result = {call}"),
        );
        assert_accepted(
            &format!("mutable slice shared method {call}"),
            &format!(
                "let mut storage = [3, 1, 2]\nlet values: &mut [i64] = &mut storage\nlet _result = {call}"
            ),
        );
    }
}

#[test]
fn indexing_iteration_and_non_resizing_mutation_execute() {
    let source = "let mut array = [3, 1, 2]; let slice: &mut [i64] = &mut array; let _ = slice.swap(0, 1); slice.sort(); slice.reverse(); slice.fill(4); let mut total = 0; for value in slice { total += *value }; println(array); println(total)";
    let output = Command::new(gos_bin())
        .args(["-e", source])
        .output()
        .expect("execute sequence program");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "[4, 4, 4]\n12\n");
}

#[test]
fn hashmap_from_uses_map_literal_syntax() {
    let source = "use std::collections::HashMap; let map: HashMap<String, i64> = HashMap::from({\"one\": 1, \"two\": 2}); println(map.get(\"one\")); println(map.len())";
    let output = Command::new(gos_bin())
        .args(["-e", source])
        .output()
        .expect("execute map literal program");
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&output.stdout), "Some(1)\n2\n");
}

#[test]
fn array_slice_and_vec_execution_matches_vm_forced_jit_and_llvm_release() {
    let root = env::temp_dir().join(format!(
        "gossamer-sequence-tier-parity-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create tier parity directory");
    let source = root.join("sequence_parity.gos");
    fs::write(
        &source,
        "fn sequence_total() -> i64 {\n\
            let mut fixed = [3, 1, 2]\n\
            let view: &mut [i64] = &mut fixed\n\
            view.sort()\n\
            view.reverse()\n\
            view.fill(2)\n\
            let mut grown: Vec<i64> = Vec::from([4, 5])\n\
            grown.push(6)\n\
            grown.fill(3)\n\
            let shared: &[i64] = &grown\n\
            let mut total = fixed[0] + view[1] + shared[2]\n\
            for value in &fixed { total += *value }\n\
            for value in shared { total += *value }\n\
            let mut fixed_words = [\"a\", \"b\"]\n\
            let word_view: &mut [String] = &mut fixed_words\n\
            word_view.fill(\"x\")\n\
            println(fixed_words)\n\
            let mut grown_words: Vec<String> = Vec::from([\"c\", \"d\"])\n\
            grown_words.fill(\"y\")\n\
            println(grown_words)\n\
            total\n\
        }\n\
        println(sequence_total())\n",
    )
    .expect("write sequence tier fixture");

    let vm = Command::new(gos_bin())
        .arg(&source)
        .env("GOS_JIT", "0")
        .output()
        .expect("run VM sequence fixture");
    assert!(
        vm.status.success(),
        "VM stderr: {}",
        String::from_utf8_lossy(&vm.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&vm.stdout), "[x, x]\n[y, y]\n22\n");

    let jit = Command::new(gos_bin())
        .arg(&source)
        .env("GOS_JIT_ONLY", "sequence_total")
        .env("GOS_JIT_TRACE", "1")
        .output()
        .expect("run JIT sequence fixture");
    assert!(
        jit.status.success(),
        "JIT stderr: {}",
        String::from_utf8_lossy(&jit.stderr)
    );
    assert_eq!(jit.stdout, vm.stdout);
    assert!(
        String::from_utf8_lossy(&jit.stderr).contains("jit: native hit sequence_total"),
        "fixture did not execute sequence_total natively: {}",
        String::from_utf8_lossy(&jit.stderr)
    );

    let build = Command::new(gos_bin())
        .args(["build", "--release", "--out-dir"])
        .arg(&root)
        .arg(&source)
        .output()
        .expect("build LLVM sequence fixture");
    assert!(
        build.status.success(),
        "LLVM build stderr: {}",
        String::from_utf8_lossy(&build.stderr)
    );
    let binary = fs::read_dir(&root)
        .expect("read LLVM output directory")
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.is_file() && path.extension().is_none())
        .expect("find LLVM sequence binary");
    let llvm = Command::new(binary)
        .output()
        .expect("run LLVM sequence fixture");
    assert!(
        llvm.status.success(),
        "LLVM stderr: {}",
        String::from_utf8_lossy(&llvm.stderr)
    );
    assert_eq!(llvm.stdout, vm.stdout);
    let _ = fs::remove_dir_all(root);
}
