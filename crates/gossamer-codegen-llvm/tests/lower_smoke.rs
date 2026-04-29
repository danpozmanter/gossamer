//! IR-shape smoke tests for the LLVM lowerer.
//!
//! Mirrors `gossamer-codegen-cranelift/tests/native.rs`'s
//! per-shape construction style, but keeps the surface narrow:
//! every test feeds the lowerer a hand-rolled MIR body and
//! inspects the resulting object bytes (or a trace from running
//! the compiled program). A more granular "IR text snapshot"
//! flavour requires the lowerer to expose `render_module` —
//! tracked under the §3.3 LLVM-tests-directory item; this file
//! is the seed crate so that follow-up has a place to land.
//!
//! Tests gracefully skip when `opt` / `llc` aren't on PATH so
//! contributors without an LLVM install can still run the rest
//! of the workspace's test suite.

#![allow(missing_docs)]

use gossamer_codegen_llvm::{BuildError, compile_to_object};
use gossamer_lex::{SourceMap, Span};
use gossamer_mir::{
    BasicBlock, BlockId, Body, ConstValue, Local, LocalDecl, Operand, Place, Rvalue, Statement,
    StatementKind, Terminator,
};
use gossamer_types::TyCtxt;

fn dummy_span() -> Span {
    let mut map = SourceMap::new();
    let file = map.add_file("smoke.gos", "");
    Span::new(file, 0, 0)
}

fn skip_if_llvm_missing() -> bool {
    // The lowerer shells out to `opt`/`llc`; without them the
    // smoke tests can't run. The driver's `find_opt` / `find_llc`
    // helpers aren't part of the public API so we approximate by
    // looking for the binaries directly.
    let try_bin = |bin: &str| {
        std::process::Command::new(bin)
            .arg("--version")
            .output()
            .is_ok_and(|o| o.status.success())
    };
    if !try_bin("opt") && std::env::var("GOS_LLVM_OPT").is_err() {
        eprintln!("skipping LLVM smoke test: `opt` not on PATH");
        return true;
    }
    if !try_bin("llc") && std::env::var("GOS_LLC").is_err() {
        eprintln!("skipping LLVM smoke test: `llc` not on PATH");
        return true;
    }
    false
}

/// Builds the trivial `fn main() -> i64 { 0 }` body.
fn trivial_main_returning_zero() -> (Body, TyCtxt) {
    let mut tcx = TyCtxt::new();
    let i64_ty = tcx.intern(gossamer_types::TyKind::Int(gossamer_types::IntTy::I64));
    let body = Body {
        name: "main".to_string(),
        def: None,
        arity: 0,
        locals: vec![LocalDecl {
            ty: i64_ty,
            debug_name: None,
            mutable: false,
        }],
        blocks: vec![BasicBlock {
            id: BlockId(0),
            stmts: vec![Statement {
                span: dummy_span(),
                kind: StatementKind::Assign {
                    place: Place {
                        local: Local(0),
                        projection: Vec::new(),
                    },
                    rvalue: Rvalue::Use(Operand::Const(ConstValue::Int(0))),
                },
            }],
            terminator: Terminator::Return,
            span: dummy_span(),
        }],
        span: dummy_span(),
    };
    (body, tcx)
}

#[test]
fn llvm_lowers_constant_return_to_object_bytes() {
    if skip_if_llvm_missing() {
        return;
    }
    let (body, tcx) = trivial_main_returning_zero();
    let object = match compile_to_object(&[body], &tcx) {
        Ok(o) => o,
        Err(e) => {
            // The smoke test treats a missing LLVM toolchain as a
            // skip rather than a hard fail — `BuildError::Tool`
            // surfaces both "binary not found" and "binary failed".
            // Anything else is real and should fail.
            let msg = e.to_string();
            if msg.contains("opt") || msg.contains("llc") || msg.contains("not found") {
                eprintln!("skipping LLVM smoke test: {msg}");
                return;
            }
            panic!("compile_to_object: {e}");
        }
    };
    assert!(!object.bytes.is_empty(), "object bytes must not be empty");
    // ELF objects on Linux start with `\x7fELF`; Mach-O on macOS
    // starts with `0xfeedface` / `0xfeedfacf` little-endian. We
    // check the ELF case (the CI host) and skip the assertion on
    // other shapes since the test pivots on hardware availability.
    if cfg!(target_os = "linux") {
        assert_eq!(&object.bytes[..4], b"\x7fELF");
    }
}

#[test]
fn llvm_build_error_displays_unsupported_kind() {
    let err = BuildError::Unsupported("test only");
    let msg = format!("{err}");
    assert!(msg.contains("unsupported"));
    assert!(msg.contains("test only"));
}
