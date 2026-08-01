//! Cross-surface regressions for Rust-guided built-in collection contracts.

use std::env;
use std::path::PathBuf;
use std::process::Command;

fn gos_bin() -> PathBuf {
    PathBuf::from(env::var("CARGO_BIN_EXE_gos").expect("CARGO_BIN_EXE_gos"))
}

fn run(source: &str) -> std::process::Output {
    let fixture = env::temp_dir().join(format!(
        "gossamer-builtin-collection-contracts-{}-{}.gos",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system time after epoch")
            .as_nanos()
    ));
    std::fs::write(&fixture, source).expect("write fixture");
    let output = Command::new(gos_bin())
        .arg(&fixture)
        .output()
        .expect("run fixture");
    let _ = std::fs::remove_file(fixture);
    output
}

#[test]
fn vec_insert_returns_result_without_replacing_the_receiver() {
    let output = run(
        "fn main() {\n    let mut values: Vec<i64> = [1, 2, 3]\n    println(values.insert(1, 9))\n    println(values)\n    println(values.insert(99, 8).is_err())\n    println(values)\n    println(values.swap(0, 3))\n    println(values)\n    println(values.swap(0, 99).is_err())\n    println(values)\n}\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Ok(())\n[1, 9, 2, 3]\ntrue\n[1, 9, 2, 3]\nOk(())\n[3, 9, 2, 1]\ntrue\n[3, 9, 2, 1]\n"
    );
}

#[test]
fn map_insert_and_collection_from_follow_rust_shaped_contracts() {
    let output = run(
        "use std::collections::{HashMap, HashSet}\n\nfn main() {\n    let mut map: HashMap<String, i64> = HashMap::from({})\n    println(map.len())\n    println(map.insert(\"a\", 1))\n    println(map.insert(\"a\", 2))\n    println(map.get(\"a\"))\n    println(map.remove(\"a\"))\n    println(map.remove(\"a\"))\n    let made: HashMap<String, i64> = HashMap::from([(\"x\", 3), (\"y\", 4)])\n    println(made.len())\n    let set: HashSet<i64> = HashSet::from([1, 2, 2, 3])\n    println(set.len())\n}\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "0\nNone\nSome(1)\nSome(2)\nSome(2)\nNone\n2\n3\n"
    );
}

#[test]
fn option_method_dispatch_matches_the_single_option_surface() {
    let output = run(
        "fn main() {\n    let present: Option<i64> = Some(12)\n    let absent: Option<i64> = None\n    println(present.and_then(|value| Some(value + 1)))\n    println(present.filter(|value| value > 10))\n    println(Some(present).flatten())\n    println(absent.is_none())\n    println(present.is_some())\n    println(present.iter())\n    println(absent.iter())\n    println(present.map(|value| value * 2))\n    println(absent.or(Some(4)))\n    println(absent.or_else(|| Some(5)))\n    println(absent.unwrap_or(6))\n    println(absent.unwrap_or_else(|| 7))\n    println(present.zip(Some(3)))\n}\n",
    );
    assert!(
        output.status.success(),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout),
        "Some(13)\nSome(12)\nSome(12)\ntrue\ntrue\n[12]\n[]\nSome(24)\nSome(4)\nSome(5)\n6\n7\nSome((12, 3))\n"
    );
}
