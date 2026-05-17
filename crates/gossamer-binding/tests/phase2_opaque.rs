//! Phase-2 `#[gos_opaque]` smoke. Exposes a tiny `Counter` opaque
//! type with constructor + `&self` + `&mut self` methods; verifies
//! every method appears as a `Counter::method` binding item.

#![allow(
    missing_docs,
    dead_code,
    clippy::must_use_candidate,
    clippy::new_without_default
)]

use gossamer_binding::gos_opaque;

pub struct Counter {
    pub value: i64,
}

#[gos_opaque]
impl Counter {
    pub fn new() -> Self {
        Self { value: 0 }
    }

    pub fn get(&self) -> i64 {
        self.value
    }

    pub fn inc(&mut self) -> i64 {
        self.value += 1;
        self.value
    }
}

#[test]
fn opaque_methods_register_as_type_qualified_items() {
    let m = gossamer_binding::modules()
        .iter()
        .find(|m| m.path == "Counter")
        .copied()
        .expect("Counter module registered");
    let names: Vec<&str> = m.items.iter().map(|i| i.name).collect();
    assert!(names.contains(&"Counter__new"));
    assert!(names.contains(&"Counter__get"));
    assert!(names.contains(&"Counter__inc"));
}
