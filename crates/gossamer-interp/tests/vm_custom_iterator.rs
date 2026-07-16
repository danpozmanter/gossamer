//! VM-native custom-iterator `for` loop tests (destroy-tree-walker).
//!
//! `for x in <user impl Iterator>` desugars to `loop { match (&mut
//! __for_iter).next() { Some(x) => body, None => break } }`. The
//! `next()` call rides the `&mut self` write-back path, so the
//! iterator's state advances each iteration with no walker fallback.
//! Method-result-collection loops (`for (k, v) in map.iter()`,
//! `map.keys()`, `map.values()`) materialise the Vec once and walk it
//! by index. All programs run entirely through the bytecode [`Vm`].

use std::cell::RefCell;

use gossamer_hir::lower_source_file;
use gossamer_interp::{Vm, set_stdout_writer};
use gossamer_lex::SourceMap;
use gossamer_parse::autoderive::parse_with_autoderive;
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
    let file = map.add_file("custom_iter.gos", source.to_string());
    let (sf, parse_diags) = parse_with_autoderive(source, file);
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
fn for_loop_over_user_iterator_sums_elements() {
    let src = r#"
struct Counter { next_value: i64, end: i64 }
trait Iterator { fn next(&mut self) -> Option<i64> }
impl Iterator for Counter {
    fn next(&mut self) -> Option<i64> {
        if self.next_value < self.end {
            let v = self.next_value
            self.next_value = self.next_value + 1
            Some(v)
        } else { None }
    }
}
fn main() {
    let mut c = Counter(0, 5)
    let mut total = 0
    for x in c { total = total + x }
    println!("total={}", total)
}
"#;
    assert_eq!(run_main(src), "total=10\n");
}

#[test]
fn for_loop_over_user_iterator_terminates_empty() {
    let src = r#"
struct Counter { next_value: i64, end: i64 }
trait Iterator { fn next(&mut self) -> Option<i64> }
impl Iterator for Counter {
    fn next(&mut self) -> Option<i64> {
        if self.next_value < self.end {
            let v = self.next_value
            self.next_value = self.next_value + 1
            Some(v)
        } else { None }
    }
}
fn main() {
    let mut c = Counter(3, 3)
    let mut count = 0
    for _x in c { count = count + 1 }
    println!("count={}", count)
}
"#;
    assert_eq!(run_main(src), "count=0\n");
}

#[test]
fn for_loop_over_map_iter_pairs() {
    // Sum is order-independent, so it stays deterministic regardless of
    // HashMap iteration order.
    let src = r#"
use std::collections::HashMap
fn main() {
    let mut m: HashMap<i64, i64> = HashMap::new()
    m.insert(1, 10)
    m.insert(2, 20)
    m.insert(3, 30)
    let mut sum = 0
    for (k, v) in m.iter() { sum = sum + k + v }
    println!("sum={}", sum)
}
"#;
    assert_eq!(run_main(src), "sum=66\n");
}

#[test]
fn for_loop_over_map_keys_and_values() {
    let src = r#"
use std::collections::HashMap
fn main() {
    let mut m: HashMap<i64, i64> = HashMap::new()
    m.insert(1, 100)
    m.insert(2, 200)
    let mut ks = 0
    for k in m.keys() { ks = ks + k }
    let mut vs = 0
    for v in m.values() { vs = vs + v }
    println!("ks={} vs={}", ks, vs)
}
"#;
    assert_eq!(run_main(src), "ks=3 vs=300\n");
}

#[test]
fn nested_for_loop_over_user_iterator() {
    let src = r#"
struct Range2 { cur: i64, end: i64 }
trait Iterator { fn next(&mut self) -> Option<i64> }
impl Iterator for Range2 {
    fn next(&mut self) -> Option<i64> {
        if self.cur < self.end {
            let v = self.cur
            self.cur = self.cur + 1
            Some(v)
        } else { None }
    }
}
fn main() {
    let mut acc = 0
    let mut outer = Range2(0, 3)
    for i in outer {
        let mut inner = Range2(0, 3)
        for j in inner { acc = acc + i * j }
    }
    println!("acc={}", acc)
}
"#;
    // sum over i,j in 0..3 of i*j = (0+1+2)^2 = 9
    assert_eq!(run_main(src), "acc=9\n");
}
