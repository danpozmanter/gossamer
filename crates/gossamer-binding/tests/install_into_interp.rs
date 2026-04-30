//! End-to-end test: declare a binding, call `install_all`, build
//! a fresh interpreter, and assert the qualified name resolves
//! through the global builtin table.

use gossamer_binding::{install_all, register_module};
use gossamer_interp::value::Value;

register_module! {
    install_test_bindings,
    path: "binding_install_test",
    doc: "Binding install integration test.",

    fn answer() -> i64 {
        42
    }

    fn echo(x: i64) -> i64 {
        x
    }
}

#[test]
fn install_registers_qualified_names() {
    let installed = install_all();
    assert!(installed >= 2);

    let interp = gossamer_interp::Interpreter::new();
    let item = interp
        .lookup_global("binding_install_test::answer")
        .expect("binding_install_test::answer registered");
    assert!(matches!(item, Value::Native(_)));

    let item = interp
        .lookup_global("binding_install_test::echo")
        .expect("binding_install_test::echo registered");
    assert!(matches!(item, Value::Native(_)));
}

#[test]
fn install_populates_resolve_table() {
    let _ = install_all();
    let module = gossamer_resolve::lookup_external_module("binding_install_test")
        .expect("module registered with resolver");
    assert!(module.items.iter().any(|i| i.name == "answer"));

    let item = gossamer_resolve::lookup_external_item("binding_install_test::echo")
        .expect("echo registered with resolver");
    assert_eq!(item.params, vec![gossamer_resolve::BindingType::I64]);
    assert_eq!(item.ret, gossamer_resolve::BindingType::I64);
}
