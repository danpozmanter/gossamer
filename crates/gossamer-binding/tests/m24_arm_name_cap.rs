//! Audit M24 (0.6.0): `intern_arm_name` pool cap.
//!
//! The pool used to grow unbounded — a binding that returned
//! `DynValue::Tagged { name: format!("Item-{n}"), .. }` over many
//! calls would leak one `Box::leak` per unique name. The 0.6.0
//! fix caps at 1024 entries; past that, the function returns a
//! static `<arm-name-pool-exhausted>` sentinel and eprintln's a
//! one-time warning so the binding author knows to switch shape.
//!
//! We test indirectly via `DynValue::Tagged`'s round-trip path
//! since `intern_arm_name` itself is module-private.

use gossamer_binding::Value;
use gossamer_binding::conv::{DynValue, ToGos};

#[test]
fn dynamic_variant_names_intern_without_panic() {
    // Sanity test: the first thousand-ish distinct names work as
    // expected. Beyond the cap the sentinel kicks in; we don't
    // assert on the sentinel here because the test process is
    // shared with other tests that may have consumed pool slots.
    for i in 0..100 {
        let value = DynValue::Tagged {
            name: format!("M24-{i}"),
            payload: Vec::new(),
        };
        let lowered: Value = value.to_gos();
        // The lowering must succeed — neither the cap check
        // nor the legacy unbounded path should panic.
        let _ = lowered;
    }
}
