//!: strings, strconv, and collection helpers.

use gossamer_std::collections::{Deque, HashMapS, HashSetS, TreeMap, TreeSet, Vector};
use gossamer_std::strconv::{ParseError, format_f64, format_i64, parse_bool, parse_f64, parse_i64};
use gossamer_std::strings::{
    contains, ends_with, find, lines, repeat, replace, split, split_whitespace, splitn,
    starts_with, to_lowercase, to_uppercase, trim,
};

#[test]
fn strings_split_and_splitn_respect_limits() {
    assert_eq!(split("a,b,c", ","), vec!["a", "b", "c"]);
    assert_eq!(splitn("a,b,c,d", 2, ","), vec!["a", "b,c,d"]);
    assert_eq!(split_whitespace(" a\tb   c "), vec!["a", "b", "c"]);
}

#[test]
fn strings_trim_find_contains_replace() {
    assert_eq!(trim("  hi\n"), "hi");
    assert!(contains("ribeye", "eye"));
    assert_eq!(find("abcdef", "cd"), Some(2));
    assert_eq!(find("abcdef", "zz"), None);
    assert_eq!(replace("a.b.c", ".", "/"), "a/b/c");
}

#[test]
fn strings_case_helpers_handle_unicode_ascii() {
    assert_eq!(to_lowercase("HELLO"), "hello");
    assert_eq!(to_uppercase("hello"), "HELLO");
}

#[test]
fn strings_prefix_and_suffix_predicates() {
    assert!(starts_with("filename.txt", "file"));
    assert!(ends_with("filename.txt", ".txt"));
    assert!(!starts_with("filename.txt", "tile"));
}

#[test]
fn strings_repeat_and_lines() {
    assert_eq!(repeat("ab", 3), "ababab");
    assert_eq!(lines("one\ntwo\nthree"), vec!["one", "two", "three"]);
}

#[test]
fn strconv_parse_i64_rejects_bad_inputs() {
    assert_eq!(parse_i64("42").unwrap(), 42);
    assert_eq!(parse_i64("-7").unwrap(), -7);
    assert!(matches!(parse_i64("").unwrap_err(), ParseError::Empty));
    assert!(matches!(
        parse_i64("abc").unwrap_err(),
        ParseError::Invalid(_)
    ));
    // Overflow past i64::MAX.
    let overflow = "999999999999999999999";
    assert!(matches!(
        parse_i64(overflow).unwrap_err(),
        ParseError::Overflow(_)
    ));
}

#[test]
#[allow(clippy::approx_constant)]
fn strconv_parse_f64_and_bool() {
    assert!((parse_f64("3.14").unwrap() - 3.14).abs() < 1e-9);
    assert!(parse_bool("true").unwrap());
    assert!(!parse_bool("false").unwrap());
    assert!(parse_bool("Maybe").is_err());
}

#[test]
fn strconv_format_inverse_of_parse() {
    let formatted = format_i64(-123);
    assert_eq!(formatted, "-123");
    assert_eq!(parse_i64(&formatted).unwrap(), -123);
    let f = format_f64(2.5);
    assert_eq!(f, "2.5");
}

#[test]
fn vector_push_pop_get_iter() {
    let mut v = Vector::<i64>::new();
    for i in 0..5 {
        v.push(i);
    }
    assert_eq!(v.len(), 5);
    assert_eq!(v.pop(), Some(4));
    assert_eq!(v.get(0), Some(&0));
    let sum: i64 = v.iter().copied().sum();
    assert_eq!(sum, 1 + 2 + 3);
}

#[test]
fn deque_supports_front_and_back_operations() {
    let mut d = Deque::<&'static str>::new();
    d.push_back("b");
    d.push_back("c");
    d.push_front("a");
    assert_eq!(d.pop_front(), Some("a"));
    assert_eq!(d.pop_back(), Some("c"));
    assert_eq!(d.pop_front(), Some("b"));
    assert!(d.is_empty());
}

#[test]
fn hashmap_and_treemap_round_trip_keys() {
    let mut hm = HashMapS::<String, i64>::new();
    hm.insert("answer".to_string(), 42);
    assert_eq!(hm.get(&"answer".to_string()), Some(&42));
    assert!(hm.contains_key(&"answer".to_string()));
    assert_eq!(hm.remove(&"answer".to_string()), Some(42));
    assert!(hm.is_empty());

    let mut tm = TreeMap::<i64, String>::new();
    tm.insert(1, "one".to_string());
    tm.insert(2, "two".to_string());
    assert_eq!(tm.get(&2), Some(&"two".to_string()));
    assert_eq!(tm.len(), 2);
}

#[test]
fn hashset_and_treeset_deduplicate_inputs() {
    let mut hs = HashSetS::<i64>::new();
    for _ in 0..3 {
        hs.insert(1);
    }
    assert_eq!(hs.len(), 1);
    assert!(hs.contains(&1));

    let mut ts = TreeSet::<&'static str>::new();
    ts.insert("b");
    ts.insert("a");
    ts.insert("a");
    assert_eq!(ts.len(), 2);
    assert!(ts.contains(&"a"));
}

#[test]
fn vector_from_rust_vec_preserves_order() {
    let v: Vector<i64> = vec![10, 20, 30].into();
    assert_eq!(v.as_slice(), &[10, 20, 30]);
}
