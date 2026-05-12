//! Pins the documented stdlib surface so refactors that drop a
//! name without a deprecation alias fail loudly. The assertion is
//! a lower bound: adding items is fine and only requires bumping
//! the literal; removing items must be a deliberate choice the
//! engineer makes by lowering the literal in the same commit.

use gossamer_std::manifest::ALL_MODULES;

#[test]
fn stdlib_surface_snapshot() {
    let mut pairs: Vec<(String, String)> = Vec::new();
    for m in ALL_MODULES {
        for item in m.items {
            pairs.push((m.path.to_string(), item.name.to_string()));
        }
    }
    pairs.sort();
    let count = pairs.len();
    for (m, i) in &pairs {
        eprintln!("{m}::{i}");
    }
    assert!(count >= 480, "stdlib surface shrunk: {count} items");
}
