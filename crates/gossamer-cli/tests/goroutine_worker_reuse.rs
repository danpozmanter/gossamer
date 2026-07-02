//! In-process multi-program goroutine isolation.
//!
//! A pooled worker thread caches its `Vm` across tasks, and a thread can
//! outlive one program (the wasm playground runs every task on the main
//! thread across successive runs; an embedding can load several programs
//! in one process). Goroutines spawned by a later program must resolve
//! their callees against that program's own globals, so the cached
//! worker `Vm` is keyed on the globals' identity and rebuilt on
//! mismatch. This test loads two programs that share every function
//! name but differ in one constant, occupies every pool worker with the
//! first program via a barrier rendezvous, then asserts the second
//! program's goroutines all produce the second program's value.

#![allow(missing_docs)]

/// Same worker-count formula as the interp's goroutine pool, so the
/// barrier occupies every worker without deadlocking on a queued task.
fn pool_workers() -> usize {
    std::thread::available_parallelism()
        .map_or(4, std::num::NonZeroUsize::get)
        .min(64)
}

/// Each of the `k` goroutines parks on the barrier until all `k` (plus
/// main) have arrived - pinning one goroutine per pool worker - then
/// reports `tag_value()` back over the channel; main asserts every
/// report carries this program's tag.
fn tagged_program(tag: i64, k: usize) -> String {
    format!(
        r#"use std::sync
use std::sync::channel

fn tag_value() -> i64 {{ {tag} }}

fn worker(b: sync::Barrier, tx: Sender<i64>) {{
    sync::Barrier::wait(b)
    tx.send(tag_value())
}}

fn main() {{
    let b = sync::Barrier::new({parties})
    let (tx, rx) = channel()
    for _ in 0..{k} {{
        go worker(b, tx)
    }}
    sync::Barrier::wait(b)
    let mut total = 0
    for _ in 0..{k} {{
        if let Some(v) = rx.recv() {{
            total += v
        }}
    }}
    assert(total == {expected}, "goroutine ran a stale program's function")
}}
"#,
        parties = k + 1,
        expected = tag * k as i64,
    )
}

/// Front-end + lower + VM run, mirroring the playground's in-process
/// pipeline (no subprocess: the goroutine pool must be shared between
/// both programs for the test to mean anything).
fn run_in_process(source: &str, label: &str) -> Result<(), String> {
    let mut map = gossamer_lex::SourceMap::new();
    let file_id = map.add_file(format!("{label}.gos"), source.to_string());
    let outcome = gossamer_driver::check_frontend(source, file_id);
    assert!(outcome.is_ok(), "{label}: front-end rejected the program");
    let gossamer_driver::CheckedFrontend {
        sf,
        resolutions,
        table,
        mut tcx,
    } = outcome.checked;
    let program = gossamer_hir::lower_source_file(&sf, &resolutions, &table, &mut tcx);
    let mut vm = gossamer_interp::Vm::new();
    vm.load(&program, tcx, true)
        .map_err(|err| format!("{label}: load failed: {err}"))?;
    let call = vm.call("main", Vec::new());
    gossamer_interp::join_outstanding_goroutines();
    call.map(|_| ()).map_err(|err| format!("{label}: {err}"))
}

#[test]
fn goroutines_of_a_second_program_use_its_own_globals() {
    let k = pool_workers();
    run_in_process(&tagged_program(1, k), "first").expect("first program");
    run_in_process(&tagged_program(2, k), "second").expect("second program");
}
