//! VM-native non-call `go` tests (destroy-tree-walker Phase 6).
//!
//! Every program runs entirely through the bytecode [`Vm`].
//! Non-call `go` shapes (`go { block }`, `go` in
//! expression position) lift the spawned expression into a zero-arg
//! closure (`Op::MakeClosure`) and spawn it on the goroutine pool
//! (`Op::Spawn`).  Channel + drain
//! synchronization keeps output deterministic, so a magic sleep is
//! never needed to observe the goroutine's effect.

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Vm, set_stdout_writer};
use gossamer_lex::SourceMap;
use gossamer_parse::parse_source_file;
use gossamer_resolve::resolve_source_file;
use gossamer_types::{TyCtxt, typecheck_source_file};

thread_local! {
    static CAPTURED: RefCell<String> = const { RefCell::new(String::new()) };
}

fn capture_writer(text: &str) {
    CAPTURED.with(|cell| cell.borrow_mut().push_str(text));
}

fn run_main(source: &str) -> String {
    let mut map = SourceMap::new();
    let file = map.add_file("go_block.gos", source.to_string());
    let (sf, parse_diags) = parse_source_file(source, file);
    assert!(parse_diags.is_empty(), "parse: {parse_diags:?}");
    let (resolutions, _resolve_diags) = resolve_source_file(&sf);
    let mut tcx = TyCtxt::new();
    let (table, _type_diags) = typecheck_source_file(&sf, &resolutions, &mut tcx);
    let program = lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = Vm::new();
    vm.load(&program, tcx, true).expect("load");
    CAPTURED.with(|cell| cell.borrow_mut().clear());
    let prev = set_stdout_writer(capture_writer);
    let result = vm.call("main", Vec::new());
    set_stdout_writer(prev);
    result.expect("main failed");
    CAPTURED.with(|cell| cell.borrow().clone())
}

#[test]
fn single_producer_go_block_drains_in_order() {
    // One `go { block }` produces an ordered sequence and closes the
    // channel; the `while let` drain sees the values in send order, so
    // the output is fully deterministic.
    let src = r#"
use std::sync::channel

fn compute(n: i64) -> i64 { n * n }

fn main() {
    let (tx, rx) = channel()
    let base = 10
    go {
        let mut i = 0
        while i < 4 {
            tx.send(base + compute(i))
            i += 1
        }
        tx.close()
    }
    while let Some(v) = rx.recv() {
        println!("got {}", v)
    }
    println!("done")
}
"#;
    assert_eq!(run_main(src), "got 10\ngot 11\ngot 14\ngot 19\ndone\n");
}

#[test]
fn loop_spawning_go_blocks_stays_native_and_aggregates() {
    // A loop body that spawns a `go { block }` each iteration must stay
    // on the bytecode path (no whole-loop defer). Each goroutine sends
    // one value; main drains a known count, so the order-independent
    // sum is deterministic regardless of scheduling.
    let src = r#"
use std::sync::channel

fn main() {
    let (tx, rx) = channel()
    let n = 5
    let mut i = 0
    while i < n {
        let k = i
        go {
            tx.send(k * k)
        }
        i += 1
    }
    let mut total = 0
    let mut got = 0
    while got < n {
        if let Some(v) = rx.recv() {
            total += v
            got += 1
        }
    }
    println!("total {}", total)
}
"#;
    assert_eq!(run_main(src), "total 30\n");
}

#[test]
fn go_block_in_expression_position_runs() {
    // `go { block }` used in expression position (its `()` result fed to
    // a `let _`) must lower natively and run the goroutine.
    let src = r#"
use std::sync::channel

fn main() {
    let (tx, rx) = channel()
    let _ = go {
        tx.send(7)
        tx.send(35)
        tx.close()
    }
    let mut sum = 0
    while let Some(v) = rx.recv() {
        sum += v
    }
    println!("sum {}", sum)
}
"#;
    assert_eq!(run_main(src), "sum 42\n");
}
