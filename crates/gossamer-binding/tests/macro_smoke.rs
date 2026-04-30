//! End-to-end smoke test for the `register_module!` macro.
//!
//! Declares a tiny binding, walks the registry, dispatches each
//! item through its thunk, and asserts results.

use gossamer_binding::{NativeDispatch, REGISTRY, RuntimeError, RuntimeResult, Value};
use gossamer_interp::value::SmolStr;

gossamer_binding::register_module! {
    smoke_bindings,
    path: "test::smoke",
    doc: "Smoke-test binding.",

    fn add(a: i64, b: i64) -> i64 {
        a + b
    }

    fn greet(name: String) -> String {
        format!("hello, {name}")
    }

    fn divide(a: i64, b: i64) -> Result<i64, String> {
        if b == 0 {
            Err("divide by zero".to_string())
        } else {
            Ok(a / b)
        }
    }

    fn maybe(flag: bool) -> Option<i64> {
        if flag { Some(1) } else { None }
    }

    fn sum(items: Vec<i64>) -> i64 {
        items.into_iter().sum()
    }
}

struct NullDispatch;
impl NativeDispatch for NullDispatch {
    fn call_fn(&mut self, _name: &str, _args: Vec<Value>) -> RuntimeResult<Value> {
        Err(RuntimeError::Unsupported("call_fn"))
    }
    fn call_value(&mut self, _callee: &Value, _args: Vec<Value>) -> RuntimeResult<Value> {
        Err(RuntimeError::Unsupported("call_value"))
    }
    fn spawn_callable(&mut self, _callable: Value, _args: Vec<Value>) -> RuntimeResult<()> {
        Err(RuntimeError::Unsupported("spawn_callable"))
    }
}

fn lookup(qualified: &str) -> &'static gossamer_binding::ItemFn {
    let (path, name) = qualified.rsplit_once("::").unwrap();
    let module = REGISTRY
        .iter()
        .copied()
        .find(|m| m.path == path)
        .unwrap_or_else(|| panic!("missing module {path}"));
    module
        .items
        .iter()
        .find(|i| i.name == name)
        .unwrap_or_else(|| panic!("missing item {qualified}"))
}

#[test]
fn module_registers_via_linkme() {
    let module = REGISTRY
        .iter()
        .copied()
        .find(|m| m.path == "test::smoke")
        .expect("test::smoke registered");
    assert_eq!(module.items.len(), 5);
    assert_eq!(module.doc, "Smoke-test binding.");
}

#[test]
fn add_dispatches_through_thunk() {
    let item = lookup("test::smoke::add");
    let mut d = NullDispatch;
    let out = (item.call)(&mut d, &[Value::Int(3), Value::Int(4)]).unwrap();
    assert!(matches!(out, Value::Int(7)));
}

#[test]
fn greet_marshals_strings() {
    let item = lookup("test::smoke::greet");
    let mut d = NullDispatch;
    let arg = Value::String(SmolStr::from_str("jane"));
    let out = (item.call)(&mut d, &[arg]).unwrap();
    if let Value::String(s) = out {
        assert_eq!(s.as_str(), "hello, jane");
    } else {
        panic!("expected String");
    }
}

#[test]
fn divide_returns_result_variant() {
    let item = lookup("test::smoke::divide");
    let mut d = NullDispatch;
    let ok = (item.call)(&mut d, &[Value::Int(10), Value::Int(2)]).unwrap();
    if let Value::Variant(inner) = ok {
        assert_eq!(inner.name, "Ok");
        assert!(matches!(inner.fields[0], Value::Int(5)));
    } else {
        panic!("expected Variant");
    }

    let err = (item.call)(&mut d, &[Value::Int(10), Value::Int(0)]).unwrap();
    if let Value::Variant(inner) = err {
        assert_eq!(inner.name, "Err");
    } else {
        panic!("expected Variant");
    }
}

#[test]
fn maybe_returns_option_variant() {
    let item = lookup("test::smoke::maybe");
    let mut d = NullDispatch;
    let some = (item.call)(&mut d, &[Value::Bool(true)]).unwrap();
    if let Value::Variant(inner) = some {
        assert_eq!(inner.name, "Some");
    } else {
        panic!("expected Variant");
    }
    let none = (item.call)(&mut d, &[Value::Bool(false)]).unwrap();
    if let Value::Variant(inner) = none {
        assert_eq!(inner.name, "None");
    } else {
        panic!("expected Variant");
    }
}

#[test]
fn sum_marshals_vec() {
    let item = lookup("test::smoke::sum");
    let mut d = NullDispatch;
    let arg = Value::Array(std::sync::Arc::new(vec![
        Value::Int(1),
        Value::Int(2),
        Value::Int(3),
        Value::Int(4),
    ]));
    let out = (item.call)(&mut d, &[arg]).unwrap();
    assert!(matches!(out, Value::Int(10)));
}

#[test]
fn arity_mismatch_returns_typed_error() {
    let item = lookup("test::smoke::add");
    let mut d = NullDispatch;
    let err = (item.call)(&mut d, &[Value::Int(1)]).unwrap_err();
    assert!(matches!(
        err,
        RuntimeError::Arity {
            expected: 2,
            found: 1
        }
    ));
}

#[test]
fn signatures_are_advertised() {
    use gossamer_binding::Type;
    let item = lookup("test::smoke::add");
    assert_eq!(item.signature.params, &[Type::I64, Type::I64]);
    assert_eq!(item.signature.ret, Type::I64);
}

gossamer_binding::register_module! {
    cb_smoke_bindings,
    path: "test::cb_smoke",
    doc: "Callback-aware smoke binding.",

    cb_fn invoke_callable(d, callable: i64) -> i64 {
        // The first arg here is the callable identity, not a Value
        // — to keep the test simple. Real callbacks come through
        // `d.call_value(...)` which is exercised by a non-test
        // integration below.
        let _ = d;
        callable + 1
    }

    cb_fn double_via_dispatch(d, x: i64) -> i64 {
        // `d` is `&mut dyn NativeDispatch`. Real bindings call
        // `d.call_value(...)`; this stub returns x*2 to verify
        // the macro plumbs `d` through.
        let _ = d;
        x * 2
    }
}

#[test]
fn cb_fn_thunk_passes_dispatch_to_body() {
    let module = REGISTRY
        .iter()
        .copied()
        .find(|m| m.path == "test::cb_smoke")
        .expect("test::cb_smoke registered");
    assert_eq!(module.items.len(), 2);
    let mut d = NullDispatch;
    let item = module
        .items
        .iter()
        .find(|i| i.name == "double_via_dispatch")
        .unwrap();
    let out = (item.call)(&mut d, &[Value::Int(21)]).unwrap();
    assert!(matches!(out, Value::Int(42)));
}

/// A dispatcher that handles `call_value` for `Value::Closure` by
/// calling a hand-rolled function. Real interp Closure has a body;
/// this fake just returns the integer length of `args`.
struct CountingDispatch;
impl NativeDispatch for CountingDispatch {
    fn call_fn(&mut self, _name: &str, args: Vec<Value>) -> RuntimeResult<Value> {
        Ok(Value::Int(i64::try_from(args.len()).unwrap_or(i64::MAX)))
    }
    fn call_value(&mut self, _callee: &Value, args: Vec<Value>) -> RuntimeResult<Value> {
        Ok(Value::Int(i64::try_from(args.len()).unwrap_or(i64::MAX)))
    }
    fn spawn_callable(&mut self, _callable: Value, _args: Vec<Value>) -> RuntimeResult<()> {
        Ok(())
    }
}

gossamer_binding::register_module! {
    cb_real_bindings,
    path: "test::cb_real",
    doc: "Real-callback dispatch test.",

    cb_fn run(d, marker: i64) -> i64 {
        // Build a synthetic Value::Int representing the callee
        // (the test dispatcher ignores the actual callee identity).
        // Then call through dispatch with two args, confirming the
        // macro wired `d` correctly.
        let synthetic_callee = gossamer_binding::Value::Int(0);
        let _ = marker;
        d.call_value(
            &synthetic_callee,
            vec![gossamer_binding::Value::Int(1), gossamer_binding::Value::Int(2)],
        )
        .map_or(-1, |v| if let gossamer_binding::Value::Int(n) = v { n } else { -1 })
    }
}

#[test]
fn cb_fn_body_dispatches_to_call_value() {
    let module = REGISTRY
        .iter()
        .copied()
        .find(|m| m.path == "test::cb_real")
        .expect("test::cb_real registered");
    let item = module.items.iter().find(|i| i.name == "run").unwrap();
    let mut d = CountingDispatch;
    let out = (item.call)(&mut d, &[Value::Int(0)]).unwrap();
    // CountingDispatch returns args.len() as i64 — we passed two
    // args to call_value, so the binding should report 2.
    assert!(matches!(out, Value::Int(2)));
}
