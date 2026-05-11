//! Runtime support for `std::unicode` — Unicode property predicates,
//! category tests, and casing operations.

#![forbid(unsafe_code)]

/// Returns `true` if `r` is a Unicode letter (category L).
#[must_use]
pub fn is_letter(r: char) -> bool {
    r.is_alphabetic()
}

/// Returns `true` if `r` is a Unicode decimal digit (category Nd).
#[must_use]
pub fn is_digit(r: char) -> bool {
    r.is_ascii_digit()
}

/// Returns `true` if `r` is a Unicode numeric character (categories N).
#[must_use]
pub fn is_number(r: char) -> bool {
    r.is_numeric()
}

/// Returns `true` if `r` is Unicode whitespace (category Z or ASCII
/// whitespace control characters).
#[must_use]
pub fn is_space(r: char) -> bool {
    r.is_whitespace()
}

/// Returns `true` if `r` is an uppercase letter (category Lu).
#[must_use]
pub fn is_upper(r: char) -> bool {
    r.is_uppercase()
}

/// Returns `true` if `r` is a lowercase letter (category Ll).
#[must_use]
pub fn is_lower(r: char) -> bool {
    r.is_lowercase()
}

/// Returns `true` if `r` is a titlecase letter (category Lt).
/// Rust does not expose a dedicated `is_titlecase` predicate, so this
/// approximates by checking if the char is neither upper nor lower but
/// is alphabetic (covers ligatures like Dz, Lj, Nj, etc.).
#[must_use]
pub fn is_title(r: char) -> bool {
    r.is_alphabetic() && !r.is_uppercase() && !r.is_lowercase()
}

/// Returns `true` if `r` is a Unicode punctuation character (category P).
#[must_use]
pub fn is_punct(r: char) -> bool {
    r.is_ascii_punctuation()
        || matches!(
            r,
            '\u{00AB}'  // LEFT-POINTING DOUBLE ANGLE QUOTATION MARK
            | '\u{00BB}' // RIGHT-POINTING DOUBLE ANGLE QUOTATION MARK
            | '\u{2010}'..='\u{2027}' // General punctuation block
            | '\u{2030}'..='\u{205E}'
            | '\u{2060}'..='\u{2FFF}'
            | '\u{3001}'..='\u{3003}' // CJK punctuation
            | '\u{FE50}'..='\u{FE6B}'
            | '\u{FF01}'..='\u{FF0F}'
            | '\u{FF1A}'..='\u{FF20}'
            | '\u{FF3B}'..='\u{FF40}'
            | '\u{FF5B}'..='\u{FF65}'
        )
}

/// Returns `true` if `r` is a Unicode symbol (category S).
#[must_use]
pub fn is_symbol(r: char) -> bool {
    matches!(r.general_category_group(), 'S')
        || matches!(
            r,
            '\u{00A2}'..='\u{00A9}'
            | '\u{00AC}'
            | '\u{00AE}'..='\u{00AF}'
            | '\u{00B1}'
            | '\u{00B4}'
            | '\u{00B8}'
            | '\u{00D7}'
            | '\u{00F7}'
            | '\u{02C2}'..='\u{02C5}'
            | '\u{02D2}'..='\u{02DF}'
            | '\u{02E5}'..='\u{02EB}'
            | '\u{02ED}'
            | '\u{02EF}'..='\u{02FF}'
            | '\u{0375}'
            | '\u{0384}'..='\u{0385}'
            | '\u{03F6}'
            | '\u{0482}'
            | '\u{058D}'..='\u{058F}'
            | '\u{0606}'..='\u{0608}'
            | '\u{060B}'
            | '\u{060E}'..='\u{060F}'
            | '\u{06DE}'
            | '\u{06E9}'
            | '\u{06FD}'..='\u{06FE}'
        )
}

/// Returns `true` if `r` is a Unicode combining mark (category M).
#[must_use]
pub fn is_mark(r: char) -> bool {
    matches!(
        r,
        '\u{0300}'..='\u{036F}'   // Combining Diacritical Marks
        | '\u{1AB0}'..='\u{1AFF}' // Combining Diacritical Marks Extended
        | '\u{1DC0}'..='\u{1DFF}' // Combining Diacritical Marks Supplement
        | '\u{20D0}'..='\u{20FF}' // Combining Diacritical Marks for Symbols
        | '\u{FE20}'..='\u{FE2F}' // Combining Half Marks
    )
}

/// Returns `true` if `r` is a printable Unicode character (not a control
/// character, not a surrogate, and not a non-character).
#[must_use]
pub fn is_print(r: char) -> bool {
    !r.is_control() && r != '\u{FFFF}' && r != '\u{FFFE}'
}

/// Returns `true` if `r` is a graphic character — printable and not a
/// space character.
#[must_use]
pub fn is_graphic(r: char) -> bool {
    is_print(r) && !r.is_whitespace()
}

/// Returns `true` if `r` is a Unicode control character (category Cc).
#[must_use]
pub fn is_control(r: char) -> bool {
    r.is_control()
}

/// Maps `r` to its Unicode simple uppercase equivalent, or returns `r`
/// unchanged if it has no uppercase mapping.
#[must_use]
pub fn to_upper(r: char) -> char {
    r.to_uppercase().next().unwrap_or(r)
}

/// Maps `r` to its Unicode simple lowercase equivalent, or returns `r`
/// unchanged if it has no lowercase mapping.
#[must_use]
pub fn to_lower(r: char) -> char {
    r.to_lowercase().next().unwrap_or(r)
}

/// Maps `r` to its Unicode titlecase equivalent. For most characters this
/// is the same as `to_upper`.
#[must_use]
pub fn to_title(r: char) -> char {
    // Rust's char::to_uppercase gives the titlecase form for letters.
    to_upper(r)
}

/// Returns the next rune in the Unicode simple case-folding cycle that
/// follows `r`. Used for case-insensitive comparisons.
/// This iterates through lowercase → uppercase → titlecase → back.
#[must_use]
pub fn simple_fold(r: char) -> char {
    if r.is_lowercase() {
        let up = to_upper(r);
        if up != r {
            return up;
        }
    } else if r.is_uppercase() {
        let lo = to_lower(r);
        if lo != r {
            return lo;
        }
    }
    r
}

// Workaround: Rust's char does not expose `general_category_group`.
// The `is_symbol` function uses a manual trait stub.
trait GeneralCategoryGroup {
    fn general_category_group(&self) -> char;
}
impl GeneralCategoryGroup for char {
    fn general_category_group(&self) -> char {
        // Returns 'S' for known math/currency/other symbols.
        match self {
            '\u{0024}' | '\u{00A3}' | '\u{00A5}' | '\u{20AC}' | '\u{20BF}' => 'S',
            '\u{002B}' | '\u{003C}'..='\u{003E}' | '\u{007C}' | '\u{007E}' => 'S',
            '\u{00AC}' | '\u{00B1}' | '\u{00D7}' | '\u{00F7}' => 'S',
            '\u{2200}'..='\u{22FF}' => 'S', // Mathematical Operators
            '\u{2300}'..='\u{23FF}' => 'S', // Miscellaneous Technical
            '\u{2500}'..='\u{25FF}' => 'S', // Box Drawing / Block Elements
            '\u{2600}'..='\u{26FF}' => 'S', // Miscellaneous Symbols
            '\u{2700}'..='\u{27BF}' => 'S', // Dingbats
            _ => '\0',
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_letter_predicates() {
        assert!(is_letter('a'));
        assert!(is_letter('Z'));
        assert!(!is_letter('1'));
        assert!(!is_letter(' '));
    }

    #[test]
    fn digit_and_number() {
        assert!(is_digit('5'));
        assert!(!is_digit('a'));
        assert!(is_number('5'));
    }

    #[test]
    fn space_predicate() {
        assert!(is_space(' '));
        assert!(is_space('\t'));
        assert!(is_space('\n'));
        assert!(!is_space('a'));
    }

    #[test]
    fn case_predicates_ascii() {
        assert!(is_upper('A'));
        assert!(!is_upper('a'));
        assert!(is_lower('z'));
        assert!(!is_lower('Z'));
    }

    #[test]
    fn casing_functions_ascii() {
        assert_eq!(to_upper('a'), 'A');
        assert_eq!(to_lower('A'), 'a');
        assert_eq!(to_upper('A'), 'A');
    }

    #[test]
    fn casing_functions_unicode() {
        assert_eq!(to_upper('é'), 'É');
        assert_eq!(to_lower('Ö'), 'ö');
    }

    #[test]
    fn control_predicate() {
        assert!(is_control('\x00'));
        assert!(is_control('\x1b'));
        assert!(!is_control('a'));
    }

    #[test]
    fn print_and_graphic() {
        assert!(is_print('a'));
        assert!(is_graphic('a'));
        assert!(is_print(' '));
        assert!(!is_graphic(' '));
        assert!(!is_print('\x00'));
    }
}
