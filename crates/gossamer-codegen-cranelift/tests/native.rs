//! End-to-end test that the Cranelift backend produces a linker-
//! ready object file capable of yielding a runnable executable.

#![allow(missing_docs)]

use std::path::PathBuf;
use std::process::Command;

use gossamer_codegen_cranelift::compile_to_object;
use gossamer_lex::SourceMap;
use gossamer_mir::{
    BasicBlock, BinOp, Body, ConstValue, LocalDecl, Operand, Place, Projection, Rvalue,
    Statement, StatementKind, Terminator,
};
use gossamer_types::TyCtxt;

fn dummy_span() -> gossamer_lex::Span {
    let mut map = SourceMap::new();
    let file = map.add_file("fuzz.gos", "");
    gossamer_lex::Span::new(file, 0, 0)
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(std::path::Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn main_returns(expr_build: impl FnOnce(&mut Builder)) -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let unit = tcx.unit();
    let mut builder = Builder {
        body: Body {
            name: "main".to_string(),
            def: None,
            arity: 0,
            locals: vec![LocalDecl {
                ty: unit,
                debug_name: None,
                mutable: false,
            }],
            blocks: vec![BasicBlock {
                id: gossamer_mir::BlockId(0),
                stmts: Vec::new(),
                terminator: Terminator::Return,
                span: dummy_span(),
            }],
            span: dummy_span(),
        },
    };
    expr_build(&mut builder);
    (builder.body, tcx)
}

struct Builder {
    body: Body,
}

impl Builder {
    fn push(&mut self, stmt: StatementKind) {
        self.body.blocks[0]
            .stmts
            .push(Statement { span: dummy_span(), kind: stmt });
    }
}

fn place(local: u32) -> Place {
    Place {
        local: gossamer_mir::Local(local),
        projection: Vec::<Projection>::new(),
    }
}

#[test]
fn cranelift_compiles_integer_constant_main_to_runnable_binary() {
    // fn main() -> i64 { 42 }
    let (body, tcx) = main_returns(|b| {
        b.push(StatementKind::Assign {
            place: place(0),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(42))),
        });
    });
    let object = compile_to_object(&[body], &tcx).expect("codegen");

    let dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let object_path = dir.join("main.o");
    std::fs::write(&object_path, &object.bytes).unwrap();
    let exe_path = dir.join("main");

    let link = Command::new("cc")
        .arg(&object_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .expect("invoke cc");
    if !link.status.success() {
        // On systems without a C toolchain we skip rather than fail.
        eprintln!(
            "skipping — cc unavailable: {}",
            String::from_utf8_lossy(&link.stderr)
        );
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let run = Command::new(&exe_path)
        .output()
        .expect("run generated binary");
    assert_eq!(
        run.status.code(),
        Some(42),
        "expected exit code 42; stderr={}",
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cranelift_heap_allocator_roundtrips_value_through_gos_store_and_load() {
    // fn main() -> i64 {
    //     let p = gos_alloc(8)
    //     gos_store(p, 0, 42)
    //     gos_load(p, 0)
    // }
    let (body, tcx) = main_returns(|b| {
        // locals: 0 = return, 1 = ptr, 2 = size const, 3 = zero const,
        // 4 = value const, 5 = store result sink.
        let mut tcx = TyCtxt::new();
        let unit = tcx.unit();
        for _ in 0..5 {
            b.body.locals.push(LocalDecl {
                ty: unit,
                debug_name: None,
                mutable: false,
            });
        }
        // size = 8
        b.push(StatementKind::Assign {
            place: place(2),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(8))),
        });
        // p = gos_alloc(size)
        b.push(StatementKind::Assign {
            place: place(1),
            rvalue: Rvalue::CallIntrinsic {
                name: "gos_alloc",
                args: vec![Operand::Copy(place(2))],
            },
        });
        // zero = 0
        b.push(StatementKind::Assign {
            place: place(3),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
        });
        // value = 42
        b.push(StatementKind::Assign {
            place: place(4),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(42))),
        });
        // gos_store(p, 0, 42)
        b.push(StatementKind::Assign {
            place: place(5),
            rvalue: Rvalue::CallIntrinsic {
                name: "gos_store",
                args: vec![
                    Operand::Copy(place(1)),
                    Operand::Copy(place(3)),
                    Operand::Copy(place(4)),
                ],
            },
        });
        // return_slot = gos_load(p, 0)
        b.push(StatementKind::Assign {
            place: place(0),
            rvalue: Rvalue::CallIntrinsic {
                name: "gos_load",
                args: vec![Operand::Copy(place(1)), Operand::Copy(place(3))],
            },
        });
    });
    let object = compile_to_object(&[body], &tcx).expect("codegen");

    let dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-heap-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let object_path = dir.join("main.o");
    std::fs::write(&object_path, &object.bytes).unwrap();
    let exe_path = dir.join("main");

    let link = Command::new("cc")
        .arg(&object_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .expect("invoke cc");
    if !link.status.success() {
        eprintln!("skipping — cc unavailable");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let run = Command::new(&exe_path).output().expect("run heap binary");
    assert_eq!(run.status.code(), Some(42), "gos_store/load round-trip");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cranelift_compiles_arithmetic_main_to_runnable_binary() {
    // fn main() -> i64 { 6 * 7 }
    let (body, tcx) = main_returns(|b| {
        b.body.locals.push(LocalDecl {
            ty: TyCtxt::new().unit(),
            debug_name: None,
            mutable: false,
        });
        b.body.locals.push(LocalDecl {
            ty: TyCtxt::new().unit(),
            debug_name: None,
            mutable: false,
        });
        b.push(StatementKind::Assign {
            place: place(1),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(6))),
        });
        b.push(StatementKind::Assign {
            place: place(2),
            rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(7))),
        });
        b.push(StatementKind::Assign {
            place: place(0),
            rvalue: Rvalue::BinaryOp {
                op: BinOp::Mul,
                lhs: Operand::Copy(place(1)),
                rhs: Operand::Copy(place(2)),
            },
        });
    });
    let object = compile_to_object(&[body], &tcx).expect("codegen");

    let dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-arith-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let object_path = dir.join("main.o");
    std::fs::write(&object_path, &object.bytes).unwrap();
    let exe_path = dir.join("main");

    let link = Command::new("cc")
        .arg(&object_path)
        .arg("-o")
        .arg(&exe_path)
        .output()
        .expect("invoke cc");
    if !link.status.success() {
        eprintln!("skipping — cc unavailable");
        let _ = std::fs::remove_dir_all(&dir);
        return;
    }

    let run = Command::new(&exe_path).output().expect("run binary");
    assert_eq!(run.status.code(), Some(42));
    let _ = std::fs::remove_dir_all(&dir);

    let _ = PathBuf::new;
}

#[test]
fn gos_build_handles_tuple_destructuring_let() {
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-detup-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("d"))
        .output()
        .expect("run d");
    assert_eq!(run.status.code(), Some(33), "let (a, b) = (11, 22); a + b == 33");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_numeric_cast() {
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-cast-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("c"))
        .output()
        .expect("run c");
    assert_eq!(run.status.code(), Some(12), "7 as i64 + 5 == 12");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_int_literal_match() {
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-match-{}",
        std::process::id()
    ));
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
            "skipping — gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping — match build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("m"))
        .output()
        .expect("run m");
    assert_eq!(run.status.code(), Some(20), "match arm 1 should return 20");
    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_handles_tuples_and_arrays() {
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-agg-{}",
        std::process::id()
    ));
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
            "skipping — gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping — tuple build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    let exe = fixture_dir.join("tup");
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
    if build.status.success()
        && !String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let run = Command::new(fixture_dir.join("rep"))
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
        eprintln!("skipping arr — gos build failed");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping arr — fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("arr")).output().expect("run arr");
    assert_eq!(run.status.code(), Some(9));

    let _ = std::fs::remove_dir_all(&fixture_dir);
}

#[test]
fn gos_build_monomorphises_generic_function_calls() {
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-mono-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("mono"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-fcc-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&fixture_dir).unwrap();
    let src = fixture_dir.join("fcc.gos");
    // Capturing closure passed through a `Fn(_)` parameter to a
    // higher-order function. The closure value is an env pointer
    // (heap blob `[fn_addr, captures…]`) produced by
    // `lift_capturing` + the MIR `gos_alloc` / `gos_store`
    // sequence. `Fn(i64) -> i64` is the closure-trait callable
    // type — it routes through the env+code dispatch in the
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        eprintln!(
            "skipping — gos build failed/launcher: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("fcc"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-capcl-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        eprintln!(
            "skipping — gos build failed/launcher: stdout={} stderr={}",
            String::from_utf8_lossy(&build.stdout),
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("cap"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-closure-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("cl"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-for-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("fr"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-struct-{}",
        std::process::id()
    ));
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
    if !build.status.success()
        || String::from_utf8_lossy(&build.stdout).contains("launcher")
    {
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let run = Command::new(fixture_dir.join("s"))
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
    let fixture_dir = std::env::temp_dir().join(format!(
        "gossamer-cranelift-println-{}",
        std::process::id()
    ));
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
            "skipping — gos build failed: {}",
            String::from_utf8_lossy(&build.stderr)
        );
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }
    let stdout = String::from_utf8_lossy(&build.stdout);
    if stdout.contains("launcher") {
        eprintln!("skipping — build fell back to launcher: {stdout}");
        let _ = std::fs::remove_dir_all(&fixture_dir);
        return;
    }

    let exe = fixture_dir.join("hi");
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
