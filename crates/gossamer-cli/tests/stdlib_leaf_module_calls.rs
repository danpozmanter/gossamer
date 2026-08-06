//! Leaf-module call gate for the stdlib.
//!
//! `use std::compress::gzip` brings the leaf module into scope, so the
//! call is spelled `gzip::encode(..)`. The compiled tiers accept both
//! that and the fully-qualified `compress::gzip::encode`; the VM has to
//! bind both too, or a program type-checks and then dies at runtime with
//! `GX0002` on the interpreter while building fine natively.

#![allow(missing_docs)]

#[test]
fn every_nested_stdlib_function_is_callable_by_its_leaf_module() {
    let live: std::collections::BTreeSet<&str> =
        gossamer_interp::registered_names().into_iter().collect();
    let mut unbound: Vec<String> = Vec::new();
    for name in gossamer_resolve::STDLIB_QUALIFIED {
        let segments: Vec<&str> = name.split("::").collect();
        if segments.len() < 3 {
            continue;
        }
        let leaf_module = segments[segments.len() - 2];
        // Associated functions on a type keep their qualified spelling.
        if leaf_module.chars().next().is_some_and(char::is_uppercase) {
            continue;
        }
        if !live.contains(*name) {
            continue;
        }
        let leaf = format!("{leaf_module}::{}", segments[segments.len() - 1]);
        if !live.contains(leaf.as_str()) {
            unbound.push(format!("{name} is not callable as {leaf}"));
        }
    }
    assert!(
        unbound.is_empty(),
        "{} nested stdlib functions have no leaf-module binding:\n{}",
        unbound.len(),
        unbound.join("\n")
    );
}
