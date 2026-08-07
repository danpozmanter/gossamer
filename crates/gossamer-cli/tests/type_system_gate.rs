//! Adversarial gate on the strength of the type system.
//!
//! Every case here is a program that a strongly-typed language must reject,
//! paired with the diagnostic code it must be rejected by, plus a control
//! that must still be accepted so the gate cannot be satisfied by rejecting
//! everything. The suite runs the same authoritative front-end `gos check`
//! runs, in-process, so it stays fast enough to gate every commit.
//!
//! Adding a case is the way to record a type-system guarantee. A case that
//! starts passing for the wrong reason (a rejection that moves to a
//! different code) fails loudly rather than silently weakening the gate.

use gossamer_driver::check_frontend;
use gossamer_lex::SourceMap;

/// What the front-end must do with a program.
#[derive(Debug, Clone, Copy)]
enum Expect {
    /// The program is well-typed.
    Accept,
    /// The program is rejected, carrying this diagnostic code.
    Reject(&'static str),
}

/// Runs the authoritative front-end and returns the codes it reported.
fn codes(source: &str) -> Vec<String> {
    let mut map = SourceMap::new();
    let file = map.add_file("type_system_gate.gos".to_string(), source.to_string());
    check_frontend(source, file)
        .diagnostics
        .iter()
        .map(|d| d.code.as_str().to_string())
        .collect()
}

/// Asserts every case in a dimension, reporting all failures at once so a
/// regression shows its whole blast radius rather than the first case only.
fn gate(dimension: &str, cases: &[(&str, &str, Expect)]) {
    let mut failures = Vec::new();
    for (name, source, expect) in cases {
        let got = codes(source);
        let ok = match expect {
            Expect::Accept => got.is_empty(),
            Expect::Reject(code) => got.iter().any(|c| c == code),
        };
        if !ok {
            failures.push(format!("  {name}: expected {expect:?}, got {got:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "{dimension} guarantees regressed:\n{}",
        failures.join("\n")
    );
}

#[test]
fn function_arguments_are_checked_by_type_and_arity() {
    gate(
        "function argument",
        &[
            (
                "wrong argument type",
                "fn f(x: i64) -> i64 { x }\nfn main() { println!(\"{}\", f(\"s\")) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "too few arguments",
                "fn f(a: i64, b: i64) -> i64 { a + b }\nfn main() { println!(\"{}\", f(1)) }\n",
                Expect::Reject("GT0018"),
            ),
            (
                "wrong return type",
                "fn f() -> i64 { \"s\" }\nfn main() { println!(\"{}\", f()) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "calling a non-callable",
                "fn main() { let a = 5\n println!(\"{}\", a(1)) }\n",
                Expect::Reject("GT0022"),
            ),
            (
                "matching argument is accepted",
                "fn f(x: i64) -> i64 { x }\nfn main() { println!(\"{}\", f(1)) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn numeric_conversions_are_never_implicit() {
    // Width and representation changes are written, never inferred: an
    // implicit widening here is what lets a value silently change meaning.
    gate(
        "numeric conversion",
        &[
            (
                "int is not a float",
                "fn f(x: f64) -> f64 { x }\nfn main() { println!(\"{}\", f(1)) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "float is not an int",
                "fn f(x: i64) -> i64 { x }\nfn main() { println!(\"{}\", f(1.5)) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "narrower int does not widen",
                "fn f(x: i64) -> i64 { x }\nfn main() { let a: i32 = 1\n println!(\"{}\", f(a)) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "int is not a bool",
                "fn main() { let b: bool = 1\n println!(\"{}\", b) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "int is not a String",
                "fn main() { let s: String = 5\n println!(\"{}\", s) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "a written cast is accepted",
                "fn f(x: f64) -> f64 { x }\nfn main() { println!(\"{}\", f(1 as f64)) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn references_distinguish_shared_from_mutable() {
    gate(
        "reference",
        &[
            (
                "shared reference does not satisfy a mutable parameter",
                "fn bump(x: &mut i64) { *x += 1 }\nfn main() { let mut a = 1\n bump(&a)\n println!(\"{}\", a) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "no write through a shared reference",
                "fn w(x: &i64) { *x = 5 }\nfn main() { let mut a = 1\n w(&a)\n println!(\"{}\", a) }\n",
                Expect::Reject("GT0031"),
            ),
            (
                "no assignment to an immutable binding",
                "fn main() { let a = 1\n a = 2\n println!(\"{}\", a) }\n",
                Expect::Reject("GT0030"),
            ),
            (
                "no field write through an immutable binding",
                "struct P { x: i64 }\nfn main() { let p = P { x: 1 }\n p.x = 2\n println!(\"{}\", p.x) }\n",
                Expect::Reject("GT0030"),
            ),
            (
                "a mutable reference is accepted",
                "fn bump(x: &mut i64) { *x += 1 }\nfn main() { let mut a = 1\n bump(&mut a)\n println!(\"{}\", a) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn collections_keep_their_element_and_key_types() {
    gate(
        "collection",
        &[
            (
                "Vec element type is enforced",
                "fn main() { let v: Vec<i64> = #[\"a\"]\n println!(\"{}\", v.len()) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "Vec element type is enforced across a call",
                "fn f(v: Vec<String>) -> i64 { v.len() }\nfn main() { println!(\"{}\", f(#[1,2])) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "Map value type is enforced",
                "fn main() { let m: Map<String, i64> = {\"a\": \"b\"}\n println!(\"{}\", m.len()) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "a Set is not a Vec",
                "fn f(v: Vec<i64>) -> i64 { v.len() }\nfn main() { println!(\"{}\", f(#{1,2})) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "indexing a non-indexable value",
                "fn main() { let a = 5\n println!(\"{}\", a[0]) }\n",
                Expect::Reject("GT0021"),
            ),
            (
                "a matching Vec is accepted",
                "fn f(v: Vec<i64>) -> i64 { v.len() }\nfn main() { println!(\"{}\", f(#[1,2])) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn aggregates_keep_their_declared_shape() {
    gate(
        "aggregate",
        &[
            (
                "tuple element types are enforced",
                "fn main() { let t: (i64, String) = (1, 2)\n println!(\"{}\", t.0) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "enum payload type is enforced",
                "enum E { A(i64) }\nfn main() { let e = E::A(\"s\")\n println!(\"ok\") }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "an Option is not its payload",
                "fn f() -> Option<i64> { Some(1) }\nfn main() { let x: i64 = f()\n println!(\"{}\", x) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "unknown struct field",
                "struct P { x: i64 }\nfn main() { let p = P { x: 1 }\n println!(\"{}\", p.z) }\n",
                Expect::Reject("GT0006"),
            ),
            (
                "a matching payload is accepted",
                "enum E { A(i64) }\nfn main() { let e = E::A(1)\n println!(\"ok\") }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn trait_bounds_are_authoritative() {
    // A type parameter stands for every type a caller may supply, so its
    // bounds are the whole of what it can do. Anything looser lets a method
    // bind an unrelated type's body and read the receiver at that layout.
    gate(
        "trait bound",
        &[
            (
                "unsatisfied bound is rejected at the call site",
                "trait Sh { fn a(&self) -> i64 }\nstruct R {}\nfn apply<T: Sh>(x: T) -> i64 { x.a() }\nfn main() { println!(\"{}\", apply(R{})) }\n",
                Expect::Reject("GT0017"),
            ),
            (
                "a method no bound declares is rejected",
                "trait Sh { fn a(&self) -> i64 }\nstruct R {}\nimpl Sh for R { fn a(&self) -> i64 { 1 } }\nfn apply<T: Sh>(x: T) -> i64 { x.zzz() }\nfn main() { println!(\"{}\", apply(R{})) }\n",
                Expect::Reject("GT0056"),
            ),
            (
                "an unbounded parameter has no methods",
                "struct R {}\nimpl R { fn a(&self) -> i64 { 1 } }\nfn apply<T>(x: T) -> i64 { x.a() }\nfn main() { println!(\"{}\", apply(R{})) }\n",
                Expect::Reject("GT0056"),
            ),
            (
                "a bound naming no trait is rejected",
                "fn apply<T: Nope>(x: T) -> i64 { 1 }\nfn main() { println!(\"{}\", apply(1)) }\n",
                Expect::Reject("GT0011"),
            ),
            (
                "iteration requires a bound that provides it",
                "fn total<T: Clone>(it: T) -> i64 { let mut s = 0\n for x in it { s += x }\n s }\nfn main() { println!(\"{}\", total(0..5)) }\n",
                Expect::Reject("GT0056"),
            ),
            (
                "a built-in iterator cannot instantiate an iteration bound",
                "fn total<T: Iterator>(it: T) -> i64 { let mut s = 0\n for x in it { s += x }\n s }\nfn main() { println!(\"{}\", total(0..5)) }\n",
                Expect::Reject("GT0057"),
            ),
            (
                "naming the iterator on the parameter is accepted",
                "fn total(it: Iterator<i64>) -> i64 { let mut s = 0\n for x in it { s += x }\n s }\nfn main() { println!(\"{}\", total(0..5)) }\n",
                Expect::Accept,
            ),
            (
                "a bound-provided method is accepted",
                "trait Sh { fn a(&self) -> i64 }\nstruct R {}\nimpl Sh for R { fn a(&self) -> i64 { 1 } }\nfn apply<T: Sh>(x: T) -> i64 { x.a() }\nfn main() { println!(\"{}\", apply(R{})) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn declared_traits_are_checked_even_when_named_like_a_builtin() {
    // Bound checking keys on the declaration, not the spelling, so a trait
    // that happens to share a built-in name keeps its guarantee.
    gate(
        "builtin-named trait",
        &[
            (
                "a user trait named Ord still constrains",
                "trait Ord { fn cmpx(&self) -> i64 }\nstruct D {}\nimpl Ord for D { fn cmpx(&self) -> i64 { 1 } }\nstruct R {}\nfn apply<T: Ord>(x: T) -> i64 { x.cmpx() }\nfn main() { println!(\"{}\", apply(R{})) }\n",
                Expect::Reject("GT0017"),
            ),
            (
                "the implementing type is accepted",
                "trait Ord { fn cmpx(&self) -> i64 }\nstruct D {}\nimpl Ord for D { fn cmpx(&self) -> i64 { 1 } }\nfn apply<T: Ord>(x: T) -> i64 { x.cmpx() }\nfn main() { println!(\"{}\", apply(D{})) }\n",
                Expect::Accept,
            ),
        ],
    );
}

#[test]
fn names_without_a_type_behind_them_are_rejected() {
    // A name in scope that binds to no type would accept any value and
    // defer the failure to run time.
    gate(
        "phantom type name",
        &[
            (
                "an undeclared type name is rejected",
                "fn main() { let x: Nonexistent = 5\n println!(\"{}\", x) }\n",
                Expect::Reject("GR0001"),
            ),
            (
                "a range converts to the iterator it advances through",
                "fn mk() -> Range<i64> { 0..5 }\nfn take(r: Iterator<i64>) -> i64 { let mut s = 0\n for x in r { s += x }\n s }\nfn main() { println!(\"{}\", take(mk())) }\n",
                Expect::Accept,
            ),
            (
                "an iterator does not convert back to a range",
                "fn mk() -> Iterator<i64> { 0..5 }\nfn take(r: Range<i64>) -> i64 { let mut s = 0\n for x in r { s += x }\n s }\nfn main() { println!(\"{}\", take(mk())) }\n",
                Expect::Reject("GT0001"),
            ),
            (
                "a range names a real type",
                "fn total(it: Range<i64>) -> i64 { let mut s = 0\n for x in it { s += x }\n s }\nfn main() { println!(\"{}\", total(0..5)) }\n",
                Expect::Accept,
            ),
        ],
    );
}
