//! Rust-level coverage for the new `std::env` module - proves the
//! Rust surface compiles and behaves; the Gossamer-level dispatch
//! test lives in the interp crate.

use gossamer_std::env;

#[test]
fn env_args_returns_argv() {
    let v = env::args();
    assert!(!v.is_empty(), "argv should at least contain argv[0]");
}

#[test]
fn env_program_name_is_non_empty() {
    assert!(!env::program_name().is_empty());
}

#[test]
fn env_var_roundtrip() {
    // Use a key unique to this test so we don't collide with
    // parallel test runs touching the same name.
    let key = "GOSSAMER_ENV_TEST_KEY_42";
    env::set_var(key, "abc").unwrap();
    assert_eq!(env::var(key), Some("abc".to_string()));
    env::unset_var(key);
    assert_eq!(env::var(key), None);
}

#[test]
fn env_current_dir_returns_a_path() {
    let cwd = env::current_dir().expect("cwd");
    assert!(!cwd.is_empty());
}

#[test]
fn env_temp_dir_is_non_empty() {
    assert!(!env::temp_dir().is_empty());
}
