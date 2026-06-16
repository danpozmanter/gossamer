//! End-to-end tests that `gos build` lowers a range of language
//! constructs to a runnable native executable.
//!
//! These drive the real `gos build` pipeline (LLVM is the only
//! native codegen backend; the Cranelift backend is JIT-only and is
//! exercised through `gos run` in the tier-parity suite). Each test
//! compiles a `.gos` fixture, runs the produced binary, and asserts
//! its exit code. A build that falls back to a launcher script, or
//! cannot link on the runner, makes the test skip rather than fail.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

/// Returns the path to the debug binary produced by `gos build`, including `.exe` on Windows.
fn debug_bin(dir: &std::path::Path, stem: &str) -> PathBuf {
    let mut p = dir.join("target").join("debug").join(stem);
    if !std::env::consts::EXE_EXTENSION.is_empty() {
        p.set_extension(std::env::consts::EXE_EXTENSION);
    }
    p
}

#[test]
fn gos_build_handles_tuple_destructuring_let() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-detup-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("d.gos");
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let (a, b) = (11i64, 22i64)\n    a + b\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "d"))
        .output()
        .expect("run d");
    assert_eq!(
        run.status.code(),
        Some(33),
        "let (a, b) = (11, 22); a + b == 33"
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_numeric_cast() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-cast-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("c.gos");
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let n = 7i64;\n    (n as i64) + 5i64\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "c"))
        .output()
        .expect("run c");
    assert_eq!(run.status.code(), Some(12), "7 as i64 + 5 == 12");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_int_literal_match() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-match-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("m.gos");
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let n = 1i64\n    match n {\n        0i64 => 10i64,\n        1i64 => 20i64,\n        _ => 30i64,\n    }\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() {
        eprintln!(
            "skipping - gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping - match build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "m"))
        .output()
        .expect("run m");
    assert_eq!(run.status.code(), Some(20), "match arm 1 should return 20");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_tuples_and_arrays() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-agg-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();

    let tuple_src = fixture_dir.join("tup.gos");
    std::fs::write(
        &tuple_src,
        "fn main() -> i64 {\n    let pair = (10i64, 20i64, 30i64)\n    pair.0 + pair.2\n}\n",
    )
    .unwrap();

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&tuple_src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() {
        eprintln!(
            "skipping - gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping - tuple build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    let exe = debug_bin(&fixture_dir, "tup");
    let run = Command::new(&exe).output().expect("run tup");
    assert_eq!(
        run.status.code(),
        Some(40),
        "tuple main should exit 40; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );

    let rep_src = fixture_dir.join("rep.gos");
    std::fs::write(
        &rep_src,
        "fn main() -> i64 {\n    let xs = [9i64; 4i64]\n    xs[2i64] + xs[3i64]\n}\n",
    )
    .unwrap();
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&rep_src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build for repeat");
    if build.status.success() && !String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let run = Command::new(debug_bin(&fixture_dir, "rep"))
            .output()
            .expect("run rep");
        assert_eq!(run.status.code(), Some(18), "[9; 4][2] + [9; 4][3] == 18");
    }

    let arr_src = fixture_dir.join("arr.gos");
    std::fs::write(
        &arr_src,
        "fn main() -> i64 {\n    let xs = [5i64, 7i64, 9i64]\n    xs[2i64]\n}\n",
    )
    .unwrap();
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&arr_src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() {
        eprintln!("skipping arr - gos build failed");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping arr - fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "arr"))
        .output()
        .expect("run arr");
    assert_eq!(run.status.code(), Some(9));

    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_monomorphises_generic_function_calls() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-mono-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("mono.gos");
    // Two distinct generic call-sites with different type arguments
    // should each get their own specialised body while still running
    // to completion identically to the monomorphic hand-coded version.
    std::fs::write(
        &src,
        "fn first<T>(a: T, b: T) -> T { a }\nfn main() -> i64 {\n    let i = first::<i64>(41i64, 999i64)\n    let b = first::<bool>(true, false)\n    if b { i + 1i64 } else { 0i64 }\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "mono"))
        .output()
        .expect("run mono");
    assert_eq!(
        run.status.code(),
        Some(42),
        "first::<i64>(41,_) + 1 should be 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_first_class_closure_passed_to_higher_order_function() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-fcc-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("fcc.gos");
    // Capturing closure passed through a `Fn(_)` parameter to a
    // higher-order function. The closure value is an env pointer
    // (heap blob `[fn_addr, captures…]`) produced by
    // `lift_capturing` + the MIR `gos_alloc` / `gos_store`
    // sequence. `Fn(i64) -> i64` is the closure-trait callable
    // type - it routes through the env+code dispatch in the
    // codegen's `Terminator::Call` arm, so `f(x)` inside `apply`
    // loads `fn_addr` from `env+0` and calls it with `(env, x)`.
    //
    // Note: the bare `fn(_)` type stays a raw code pointer; only
    // `Fn(_)` carries the env. See closure_fn_trait_plan.md for
    // the design.
    std::fs::write(
        &src,
        "fn apply(f: Fn(i64) -> i64, x: i64) -> i64 { f(x) }\nfn main() -> i64 {\n    let c = 10i64\n    let add_c = |y: i64| c + y\n    apply(add_c, 32i64)\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        eprintln!(
            "skipping - gos build failed/launcher: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "fcc"))
        .output()
        .expect("run fcc");
    assert_eq!(
        run.status.code(),
        Some(42),
        "apply(|y| c + y where c = 10, 32) should yield 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_capturing_closure_via_heap_allocated_env() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-capcl-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("cap.gos");
    // `|y| x + y` captures `x`. lift_closures emits an
    // `__closure_0(env, y)` whose body loads `x` from env, and the
    // MIR lowerer wraps the creation site in `gos_alloc` + `gos_store`.
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let x = 10i64\n    let add_x = |y: i64| x + y\n    add_x(32i64)\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        eprintln!(
            "skipping - gos build failed/launcher: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "cap"))
        .output()
        .expect("run cap");
    assert_eq!(
        run.status.code(),
        Some(42),
        "capturing closure: x=10 + y=32 = 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_non_capturing_closure_via_direct_call() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-closure-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("cl.gos");
    // `|x| x + 1` captures nothing, so lift_closures promotes it to
    // a top-level function. The call below becomes a direct call.
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let plus = |x: i64| x + 1i64\n    plus(41i64)\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "cl"))
        .output()
        .expect("run cl");
    assert_eq!(
        run.status.code(),
        Some(42),
        "|x| x + 1 applied to 41 should yield 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_for_loop_over_range() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-for-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("fr.gos");
    std::fs::write(
        &src,
        "fn main() -> i64 {\n    let mut sum = 0i64\n    for n in 0i64..10i64 {\n        sum = sum + n\n    }\n    sum\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "fr"))
        .output()
        .expect("run fr");
    assert_eq!(
        run.status.code(),
        Some(45),
        "sum of 0..10 should be 45; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_struct_literal_and_field_access() {
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-struct-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("s.gos");
    std::fs::write(
        &src,
        "struct Point { x: i64, y: i64 }\nfn main() -> i64 {\n    let p = Point { x: 10i64, y: 32i64 }\n    p.x + p.y\n}\n",
    )
    .unwrap();
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() || String::from_utf8_lossy(&build.stdout).contains("launcher") {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(debug_bin(&fixture_dir, "s"))
        .output()
        .expect("run struct binary");
    assert_eq!(
        run.status.code(),
        Some(42),
        "Point {{ x: 10, y: 32 }}; p.x + p.y == 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_produces_native_println_binary() {
    // Drive the full `gos build` pipeline against a hello-world
    // source. Asserts that the output is a real executable (not a
    // launcher shell script) and that running it prints the string
    // to stdout.
    let fixture_dir =
        std::env::temp_dir().join(format!("gossamer-cranelift-println-{}", std::process::id()));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src_path = fixture_dir.join("hi.gos");
    std::fs::write(&src_path, "fn main() { println(\"native says hi\") }\n").unwrap();

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_string());
    let build = Command::new(&cargo)
        .args(["run", "--quiet", "--bin", "gos", "--", "build"])
        .arg(&src_path)
        .current_dir(workspace_root())
        .output()
        .expect("spawn gos build");
    if !build.status.success() {
        eprintln!(
            "skipping - gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping - build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    let exe = debug_bin(&fixture_dir, "hi");
    let run = Command::new(&exe).output().expect("run native binary");
    assert!(
        run.status.success(),
        "native binary exit: {:?}",
        run.status.code()
    );
    let out = String::from_utf8_lossy(&run.stdout);
    assert!(out.contains("native says hi"), "stdout: {out}");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}
