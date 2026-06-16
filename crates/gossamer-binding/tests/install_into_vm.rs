//! End-to-end test: declare a binding, call `install_all`, build a
//! fresh VM, and assert the qualified name is reachable and invocable
//! through the global native table.

use gossamer_binding::{install_all, register_module};
use gossamer_interp::Vm;
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

    // A fresh VM merges the installed-natives snapshot into its
    // globals, so each binding resolves and runs under its
    // fully-qualified `module::item` name.
    let vm = Vm::new();
    let answer = vm
        .call("binding_install_test::answer", Vec::new())
        .expect("binding_install_test::answer invokes");
    assert!(matches!(answer, Value::Int(42)));

    let echoed = vm
        .call("binding_install_test::echo", vec![Value::Int(7)])
        .expect("binding_install_test::echo invokes");
    assert!(matches!(echoed, Value::Int(7)));
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
