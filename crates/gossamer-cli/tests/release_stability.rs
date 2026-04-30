//! Release-tier stability gauge.
//!
//! Each test below writes a small deterministic Gossamer program,
//! builds it with `gos build --release` (LLVM + Cranelift fallback
//! — the production tier), runs the binary, and asserts the
//! produced stdout byte-for-byte against a fixed expected string.
//!
//! The `--release` pipeline is the gold-standard target: interp
//! and `gos build` (debug/Cranelift) are dev-loop tooling, but
//! `--release` is what real deployments ship. So every test here
//! exercises that exact pipeline, no fallback, no skip.
//!
//! Tests that pass are regression gates: a future change that
//! silently breaks (say) `HashMap.inc` or recursive-enum walking
//! in the release tier will turn this suite red. Tests carrying
//! `#[ignore = "release-tier wiring gap: …"]` document a known
//! wiring failure where `gos build --release` accepts the program
//! but the produced binary diverges from the language semantics
//! — those entries form the today-snapshot of the gauge. Removing
//! the `#[ignore]` is the right way to claim a gap is closed.

#![allow(missing_docs)]

use std::env;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

/// Per-test scratch directory. Concurrent tests must not share a
/// directory: `gos build` writes the produced binary to
/// `<source-dir>/target/release/<stem>`, and a clobber would let
/// one test execute another's bits.
fn fresh_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU32 = AtomicU32::new(0);
    let n = COUNTER.fetch_add(1, Ordering::Relaxed);
    let dir = env::temp_dir().join(format!(
        "gos-rel-stab-{pid}-{n}-{name}",
        pid = std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

struct Program {
    dir: PathBuf,
    bin: PathBuf,
}

impl Drop for Program {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn build_release(name: &str, body: &str) -> Program {
    let dir = fresh_dir(name);
    let source = dir.join(format!("{name}.gos"));
    std::fs::write(&source, body).expect("write source");
    let out = Command::new(gos_bin())
        .arg("build")
        .arg("--release")
        .arg(&source)
        .output()
        .expect("spawn gos build --release");
    assert!(
        out.status.success(),
        "gos build --release {name} failed:\n--- stdout ---\n{}\n--- stderr ---\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr),
    );
    let bin = dir.join("target").join("release").join(name);
    assert!(bin.exists(), "release binary missing at {}", bin.display());
    Program { dir, bin }
}

fn run(prog: &Program) -> (i32, String, String) {
    let out = Command::new(&prog.bin).output().expect("run binary");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// Asserts the released binary exits 0 and emits exactly `expected`
/// on stdout. Failure messages dump both streams plus the exit code
/// so the source of any drift is visible without re-running.
fn assert_release_stdout_eq(name: &str, body: &str, expected: &str) {
    let prog = build_release(name, body);
    let (code, stdout, stderr) = run(&prog);
    assert_eq!(
        code, 0,
        "{name}: exit={code}\n--- stdout ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
    assert_eq!(
        stdout, expected,
        "{name}: stdout drift\n--- expected ---\n{expected}\n--- actual ---\n{stdout}\n--- stderr ---\n{stderr}"
    );
}

// ---------------------------------------------------------------
// Passing checks — these are the regression gates. Each one
// covers a behaviour that the release pipeline is supposed to
// honour and has been confirmed working at the time of writing.
// A red light here means a recent change broke something.
// ---------------------------------------------------------------

#[test]
fn release_recursive_enum_walks_full_list() {
    // Catches recursive-enum aggregate-layout regressions: the
    // `Box<List>` payload must round-trip through pass-by-value
    // and the `match` arms must dispatch on the discriminant.
    assert_release_stdout_eq(
        "rec_enum",
        r#"
enum List {
    Nil,
    Cons(i64, Box<List>),
}

fn cons(v: i64, rest: List) -> List { List::Cons(v, Box::new(rest)) }

fn length(list: &List) -> i64 {
    match list {
        List::Nil => 0,
        List::Cons(_, rest) => 1 + length(rest),
    }
}

fn sum(list: &List) -> i64 {
    match list {
        List::Nil => 0,
        List::Cons(v, rest) => *v + sum(rest),
    }
}

fn main() {
    let xs = cons(1, cons(2, cons(3, cons(4, cons(5, List::Nil)))))
    println!("len={} sum={}", length(&xs), sum(&xs))
}
"#,
        "len=5 sum=15\n",
    );
}

#[test]
fn release_closure_captures_value_at_definition() {
    // Defines `bump` *before* mutating `k`, so each call sees `k`
    // as captured (zero), making `bump(k)` collapse to `k + 1`.
    // sum_{k=0..99}(k+1) = 5050. Catches closure-capture ABI
    // regressions where the capture is silently aliased to a
    // mutable upvar.
    assert_release_stdout_eq(
        "closure_capture",
        r#"
fn main() {
    let mut acc: i64 = 0
    let mut k: i64 = 0
    let bump = |x: i64| { k + x + 1 }
    while k < 100 {
        acc = acc + bump(k)
        k = k + 1
    }
    println!("acc={}", acc)
}
"#,
        "acc=5050\n",
    );
}

#[test]
fn release_channel_send_recv_drains_in_order() {
    // FIFO channel semantics — main pushes 5 values, drains 5 via
    // `if let Some(v) = rx.recv()`. Catches Option<T> aggregate
    // construction from runtime returns + channel ABI.
    assert_release_stdout_eq(
        "channel_drain",
        r#"
use std::sync::channel

fn main() {
    let (tx, rx) = channel()
    let mut k = 0
    while k < 5 {
        tx.send(k * 10)
        k = k + 1
    }
    let mut sum: i64 = 0
    let mut n = 0
    while n < 5 {
        if let Some(v) = rx.recv() {
            sum = sum + v
        }
        n = n + 1
    }
    println!("sum={}", sum)
}
"#,
        "sum=100\n",
    );
}

#[test]
fn release_waitgroup_blocks_main_until_workers_done() {
    // Spawns three goroutines, each calling `wg.done()`. Main
    // blocks on `wg.wait()`. Catches WaitGroup wiring (add/done/
    // wait) and goroutine spawn-via-block in release.
    assert_release_stdout_eq(
        "wg_block",
        r#"
use std::sync

fn main() {
    let wg = sync::WaitGroup::new()
    wg.add(3)
    let mut k = 0
    while k < 3 {
        go {
            wg.done()
        }
        k = k + 1
    }
    wg.wait()
    println!("done")
}
"#,
        "done\n",
    );
}

#[test]
fn release_hashmap_inc_idiom_counts_words() {
    // Catches the HashMap.inc counter idiom — a known weak point:
    // a recent fix landed for the round-2 String<->i64 lowering.
    // Verifies inc() defaults to +1, increments persist across
    // calls, and `get_or(default)` reads the right slot.
    assert_release_stdout_eq(
        "hm_inc",
        r#"
use std::collections::HashMap

fn main() {
    let mut tally: HashMap<String, i64> = HashMap::new()
    let words = ["apple", "banana", "apple", "apple", "banana", "cherry"]
    for w in words {
        tally.inc(w)
    }
    println!("apple={} banana={} cherry={}",
        tally.get_or("apple", 0),
        tally.get_or("banana", 0),
        tally.get_or("cherry", 0))
}
"#,
        "apple=3 banana=2 cherry=1\n",
    );
}

#[test]
fn release_btreemap_iter_yields_sorted_pairs() {
    // BTreeMap's `for (k, v) in m.iter()` shape — destructured
    // iteration over an ordered map. Catches both the
    // tuple-destructuring binding and the sorted-by-key
    // invariant.
    assert_release_stdout_eq(
        "btmap",
        r#"
use std::collections::BTreeMap

fn main() {
    let mut m: BTreeMap<String, i64> = BTreeMap::new()
    m.insert("c", 3)
    m.insert("a", 1)
    m.insert("b", 2)
    let mut sum: i64 = 0
    for (k, v) in m.iter() {
        println!("{}={}", k, v)
        sum = sum + v
    }
    println!("sum={}", sum)
}
"#,
        "a=1\nb=2\nc=3\nsum=6\n",
    );
}

#[test]
fn release_match_guard_and_range_patterns_classify() {
    // Match arm with guard (`x if x < 0`), exact literal (`0`),
    // inclusive range (`1..=9`), and wildcard. Ensures the
    // pattern compiler in release covers all four shapes.
    assert_release_stdout_eq(
        "patterns",
        r#"
fn classify(n: i64) -> String {
    match n {
        x if x < 0 => "negative",
        0 => "zero",
        1..=9 => "single",
        _ => "many",
    }
}

fn main() {
    for n in [-5, 0, 3, 42] {
        println!("{}={}", n, classify(n))
    }
}
"#,
        "-5=negative\n0=zero\n3=single\n42=many\n",
    );
}

#[test]
fn release_trait_impl_dispatches_through_concrete_types() {
    // Two distinct types implement the same trait. Calls via
    // concrete-typed bindings must reach the right impl. Catches
    // method-table generation in release.
    assert_release_stdout_eq(
        "trait_impl",
        r#"
trait Shape {
    fn area(&self) -> f64
    fn name(&self) -> String
}

struct Circle { radius: f64 }
struct Rect { w: f64, h: f64 }

impl Shape for Circle {
    fn area(&self) -> f64 { 3.14159265 * self.radius * self.radius }
    fn name(&self) -> String { "circle" }
}

impl Shape for Rect {
    fn area(&self) -> f64 { self.w * self.h }
    fn name(&self) -> String { "rect" }
}

fn main() {
    let c = Circle { radius: 2.0 }
    let r = Rect { w: 3.0, h: 4.0 }
    println!("{} area={:.4}", c.name(), c.area())
    println!("{} area={:.4}", r.name(), r.area())
}
"#,
        "circle area=12.5664\nrect area=12.0000\n",
    );
}

#[test]
fn release_struct_methods_chain_returning_value() {
    // Builder-style chain where each method takes `self` and
    // returns a fresh struct. Catches aggregate-return ABI in
    // release (one of the four root causes the compiled-impl
    // method-dispatch fix had to close).
    assert_release_stdout_eq(
        "method_chain",
        r#"
struct Counter { value: i64 }

impl Counter {
    fn new() -> Counter { Counter { value: 0 } }
    fn inc(self, by: i64) -> Counter { Counter { value: self.value + by } }
    fn double(self) -> Counter { Counter { value: self.value * 2 } }
    fn get(self) -> i64 { self.value }
}

fn main() {
    let c = Counter::new().inc(3).double().inc(1).double()
    println!("got={}", c.get())
}
"#,
        "got=14\n",
    );
}

#[test]
fn release_recursive_fib_returns_correct_value() {
    // Naive recursive `fib(25)` — a smoke test for stack-rooted
    // values, register reuse, and call-conv across deep call
    // chains.
    assert_release_stdout_eq(
        "fib",
        r#"
fn fib(n: i64) -> i64 {
    if n < 2 { n } else { fib(n - 1) + fib(n - 2) }
}

fn main() {
    println!("fib(20)={}", fib(20))
    println!("fib(25)={}", fib(25))
}
"#,
        "fib(20)=6765\nfib(25)=75025\n",
    );
}

#[test]
fn release_vec_push_iter_sums_correctly() {
    // Push 1000 i64s into a heap-vec, iterate via `.iter()`,
    // dereference each element, accumulate. Catches the i64-vec
    // alloc-set-iter triple and the dereference path.
    assert_release_stdout_eq(
        "vec_iter",
        r#"
fn main() {
    let mut v: [i64] = []
    let mut k: i64 = 0
    while k < 1000 {
        v.push(k)
        k = k + 1
    }
    let mut sum: i64 = 0
    for x in v.iter() {
        sum = sum + *x
    }
    println!("len={} sum={}", v.len(), sum)
}
"#,
        "len=1000 sum=499500\n",
    );
}

#[test]
fn release_string_split_trim_parse_totals() {
    // Splits a comma list, trims each piece, parses each into
    // i64, accumulates. Catches the str.split / str.trim /
    // str.parse trio in release plus Result<i64,_> match in a
    // hot path.
    assert_release_stdout_eq(
        "split_trim_parse",
        r#"
fn main() {
    let line = "1, 2, 3, 4, 5"
    let mut sum: i64 = 0
    let mut count: i64 = 0
    for piece in line.split(',') {
        let trimmed = piece.trim()
        let n: i64 = match trimmed.parse() {
            Ok(v) => v,
            Err(_) => 0,
        }
        sum = sum + n
        count = count + 1
    }
    println!("count={} sum={}", count, sum)
}
"#,
        "count=5 sum=15\n",
    );
}

#[test]
fn release_nested_format_macro_handles_precision() {
    // `format!("{:.4}", pi)` nested inside another `format!` —
    // catches the LLVM `__concat` buffering fix and precision
    // wiring on the inner string boundary.
    assert_release_stdout_eq(
        "nested_fmt",
        r#"
fn main() {
    let pi = 3.14159265358979
    let nested = format!("[{}]", format!("{:.4}", pi))
    println!("{}", nested)
    let multi = format!("a={:.2} b={:.4}", pi, pi * 2.0)
    println!("{}", multi)
}
"#,
        "[3.1416]\na=3.14 b=6.2832\n",
    );
}

#[test]
fn release_float_edges_round_trip_to_expected_booleans() {
    // NaN != NaN, +0 == -0, division-by-zero produces signed
    // infinities. Catches H7 (NaN-boxing low-mantissa loss) any
    // time it might re-emerge as a regression.
    assert_release_stdout_eq(
        "float_edges",
        r#"
fn main() {
    let zero: f64 = 0.0
    let neg_zero: f64 = -0.0
    let inf: f64 = 1.0 / zero
    let neg_inf: f64 = -1.0 / zero
    println!("zero==neg_zero: {}", zero == neg_zero)
    println!("inf>0: {}", inf > 0.0)
    println!("neg_inf<0: {}", neg_inf < 0.0)
    let nan: f64 = inf - inf
    println!("nan==nan: {}", nan == nan)
}
"#,
        "zero==neg_zero: true\ninf>0: true\nneg_inf<0: true\nnan==nan: false\n",
    );
}

#[test]
fn release_for_range_inclusive_and_iter_match_exclusive() {
    // Three for-loop shapes: `0..n`, `0..=n`, `vec.iter()`. The
    // VM has a documented fast-path for the exclusive form (H3)
    // and used to silently fall back on the others; release
    // should handle all three.
    assert_release_stdout_eq(
        "for_shapes",
        r#"
fn main() {
    let mut excl: i64 = 0
    for i in 0..5 { excl = excl + i }

    let mut incl: i64 = 0
    for i in 0..=5 { incl = incl + i }

    let v = [10, 20, 30, 40]
    let mut iter_sum: i64 = 0
    for x in v.iter() { iter_sum = iter_sum + *x }

    println!("excl={} incl={} iter={}", excl, incl, iter_sum)
}
"#,
        "excl=10 incl=15 iter=100\n",
    );
}

// ---------------------------------------------------------------
// Known release-tier wiring gaps. Each `#[ignore]` reason names
// the surface area where `gos build --release` accepts the
// program but the produced binary diverges from language
// semantics (silent no-op, silent empty, segfault, etc.).
//
// These are the gauge: when a gap is closed, drop the `#[ignore]`
// and the test becomes a permanent regression gate.
// ---------------------------------------------------------------

#[test]
fn release_atomic_fetch_add_persists_across_goroutines() {
    // 100 goroutines each `fetch_add(1)` on a shared AtomicI64.
    // The constructor rename map gained `AtomicI64::new` /
    // `sync::AtomicI64::new` so the receiver is a real
    // `*mut GosAtomicI64` instead of null; without that the
    // helper silently saw `a.is_null()` and returned 0 every
    // time.
    assert_release_stdout_eq(
        "atomic_inc",
        r#"
use std::sync

fn main() {
    let counter = sync::AtomicI64::new(0)
    let wg = sync::WaitGroup::new()
    let mut k = 0
    while k < 100 {
        wg.add(1)
        go {
            counter.fetch_add(1)
            wg.done()
        }
        k = k + 1
    }
    wg.wait()
    println!("counter={}", counter.load())
}
"#,
        "counter=100\n",
    );
}

#[test]
fn release_owned_string_push_str_holds_value() {
    // `String::new()` lowers to an empty-string literal and
    // `b.push_str(s)` is rewritten to `b = __concat(b, s)`
    // (gossamer-mir/src/lower.rs::lower_method_call). Owned
    // `String` is the runtime's `*const c_char` representation
    // — concat-and-reassign keeps the receiver local rooted to
    // the new bytes pointer.
    assert_release_stdout_eq(
        "owned_str",
        r#"
fn main() {
    let mut b: String = String::new()
    b.push_str("hi")
    println!("b={}", b)
}
"#,
        "b=hi\n",
    );
}

#[test]
fn release_result_map_err_replaces_error() {
    // The HIR lift pass turns non-capturing closures into a
    // bare-name path that lowers to a string-literal pointer.
    // `gos_rt_result_map_err` reads the first 8 bytes of its
    // closure arg as a function address, so the raw pointer
    // segfaulted. The MIR lower for `map_err` / `map` now wraps
    // bare-name closure args into a heap blob `[fn_addr, _]`
    // (gossamer-mir/src/lower.rs::lower_method_call).
    assert_release_stdout_eq(
        "map_err",
        r#"
use std::errors

fn main() {
    let raw: String = "oops"
    let r: Result<i64, _> = raw.parse()
    let mapped = r.map_err(|_| errors::new("custom"))
    match mapped {
        Ok(n) => println!("ok {}", n),
        Err(e) => println!("err {}", e.message()),
    }
}
"#,
        "err custom\n",
    );
}

#[test]
fn release_iter_enumerate_yields_index_value_pairs() {
    // `v.iter().enumerate()` lowering is now in
    // `gossamer-mir/src/lower.rs::lower_for_enumerate`. Strips
    // the `enumerate()` and an inner wrapping `iter()`, then
    // drives the standard array / vec counter loop while binding
    // the per-iteration counter to the tuple's first slot.
    assert_release_stdout_eq(
        "enumerate",
        r#"
fn main() {
    let v = [10, 20, 30, 40]
    for (idx, x) in v.iter().enumerate() {
        println!("idx={} x={}", idx, *x)
    }
}
"#,
        "idx=0 x=10\nidx=1 x=20\nidx=2 x=30\nidx=3 x=40\n",
    );
}
