// `std::uuid` — UUID v4 (random) and v7 (timestamp-ordered) generation,
// parsing, and formatting. Backed by the `uuid` crate.
//
// Logic also lives in `gossamer_runtime::c_abi::gos_rt_uuid_*` so the
// compiled tier (Cranelift / LLVM) reaches the same code via static
// linkage. This module is the interp-tier entry point.

#![forbid(unsafe_code)]

use uuid::Uuid;

/// Generates a fresh random v4 UUID and returns its canonical
/// hyphenated form (e.g. `"550e8400-e29b-41d4-a716-446655440000"`).
#[must_use]
pub fn v4() -> String {
    Uuid::new_v4().hyphenated().to_string()
}

/// Generates a fresh v7 UUID — a timestamp-ordered UUID whose
/// leading bits encode the current unix epoch milliseconds.
/// Useful as a primary key when insertion order matters.
#[must_use]
pub fn v7() -> String {
    Uuid::now_v7().hyphenated().to_string()
}

/// `true` iff `s` parses as a canonical UUID.
#[must_use]
pub fn is_valid(s: &str) -> bool {
    Uuid::parse_str(s).is_ok()
}

/// Returns the lowercase canonical form of `s` if it parses, or
/// the empty string. Useful for normalizing user-supplied UUIDs.
#[must_use]
pub fn normalize(s: &str) -> String {
    match Uuid::parse_str(s) {
        Ok(u) => u.hyphenated().to_string(),
        Err(_) => String::new(),
    }
}

/// Returns the 32-character unhyphenated form (`550e8400e29b...`)
/// or empty string on parse failure.
#[must_use]
pub fn simple(s: &str) -> String {
    match Uuid::parse_str(s) {
        Ok(u) => u.simple().to_string(),
        Err(_) => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_is_36_chars() {
        let u = v4();
        assert_eq!(u.len(), 36);
        assert_eq!(u.chars().filter(|c| *c == '-').count(), 4);
    }

    #[test]
    fn v4_is_random() {
        assert_ne!(v4(), v4());
    }

    #[test]
    fn v7_is_36_chars_and_increasing() {
        let a = v7();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let b = v7();
        assert_eq!(a.len(), 36);
        assert_eq!(b.len(), 36);
        // v7 is time-ordered; lexicographic comparison should agree with
        // generation order (within the same millisecond it may tie).
        assert!(b >= a);
    }

    #[test]
    fn is_valid_accepts_canonical() {
        assert!(is_valid("550e8400-e29b-41d4-a716-446655440000"));
    }

    #[test]
    fn is_valid_rejects_garbage() {
        assert!(!is_valid("not-a-uuid"));
        assert!(!is_valid(""));
    }

    #[test]
    fn normalize_lowercases() {
        let u = "550E8400-E29B-41D4-A716-446655440000";
        assert_eq!(normalize(u), "550e8400-e29b-41d4-a716-446655440000");
    }

    #[test]
    fn simple_strips_hyphens() {
        let u = "550e8400-e29b-41d4-a716-446655440000";
        assert_eq!(simple(u), "550e8400e29b41d4a716446655440000");
    }
}
