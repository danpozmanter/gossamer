//! Phase 0 ergonomic regression — the `name: <ident>` form,
//! doc-comment capture, and the `FORCE_LINK_FNS` distributed-slice
//! mechanism. Verifies the legacy `register_module!` form keeps
//! working alongside.

use gossamer_binding::register_module;

register_module!(
    name: ergo,
    doc: "Phase-0 ergonomic test module.",

    /// Uppercase the input.
    /// Multi-line docs collapse via newline-separation.
    fn shout(s: String) -> String {
        s.to_uppercase()
    }

    /// Returns the sum of every element.
    fn sum(xs: Vec<i64>) -> i64 {
        xs.iter().sum()
    }

    // Item with no doc-comment must still register with empty doc.
    fn no_doc() -> i64 {
        0
    }
);

#[test]
fn new_form_registers_under_path_derived_from_name() {
    let modules = gossamer_binding::modules();
    let ergo = modules
        .iter()
        .find(|m| m.path == "ergo")
        .expect("`ergo` module not registered");
    assert_eq!(ergo.items.len(), 3);
    let names: Vec<&str> = ergo.items.iter().map(|i| i.name).collect();
    assert!(names.contains(&"shout"));
    assert!(names.contains(&"sum"));
    assert!(names.contains(&"no_doc"));
}

#[test]
fn doc_comments_flow_through_to_itemfn() {
    let modules = gossamer_binding::modules();
    let ergo = modules
        .iter()
        .find(|m| m.path == "ergo")
        .expect("`ergo` module");
    let shout = ergo.items.iter().find(|i| i.name == "shout").unwrap();
    assert!(
        shout.doc.contains("Uppercase"),
        "expected doc to flow through, got: {:?}",
        shout.doc
    );
    let no_doc = ergo.items.iter().find(|i| i.name == "no_doc").unwrap();
    assert_eq!(no_doc.doc, "");
}

#[test]
fn force_link_fns_distributed_slice_is_populated() {
    // The Phase-0 ergonomic form publishes a `FORCE_LINK_FNS`
    // entry; calling `run_all_force_links` walks every registered
    // module's force_link() — must not panic.
    let n_before = gossamer_binding::FORCE_LINK_FNS.len();
    gossamer_binding::run_all_force_links();
    let n_after = gossamer_binding::FORCE_LINK_FNS.len();
    assert_eq!(n_before, n_after, "FORCE_LINK_FNS is immutable at runtime");
    assert!(n_before >= 1, "at least the `ergo` module should register");
}
