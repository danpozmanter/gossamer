//! The alignment contract between a `{:spec}` placeholder and the padding
//! helpers it lowers to.
//!
//! A spec's alignment cannot be resolved where the spec is parsed, because
//! two of its cases depend on what is being padded: an omitted alignment
//! puts a number on the right and everything else on the left, and the `0`
//! flag means "sign-aware zero pad" on a number and nothing at all on any
//! other value. So the format expansion emits the *request* - the written
//! alignment plus the flag - and HIR lowering, which knows the value's
//! type, resolves it into one of the concrete alignments the runtime
//! implements.

/// No alignment was written: resolved by the padded value's type.
pub const PAD_REQUEST_DEFAULT: i64 = 0;
/// `{:<8}`.
pub const PAD_REQUEST_LEFT: i64 = 1;
/// `{:^8}`.
pub const PAD_REQUEST_CENTER: i64 = 2;
/// `{:>8}`.
pub const PAD_REQUEST_RIGHT: i64 = 3;
/// The `0` flag, or-ed into the written alignment.
pub const PAD_REQUEST_ZERO_FLAG: i64 = 8;
/// Masks the written alignment out of a request.
pub const PAD_REQUEST_ALIGN_MASK: i64 = 7;

/// Right-aligned: `gos_rt_fmt_pad` and its fused twins.
pub const PAD_ALIGN_RIGHT: i64 = 0;
/// Left-aligned.
pub const PAD_ALIGN_LEFT: i64 = 1;
/// Centre-aligned, with the odd cell going to the right.
pub const PAD_ALIGN_CENTER: i64 = 2;
/// Zeros between the value's sign (and radix prefix) and its digits.
pub const PAD_ALIGN_SIGN_AWARE_ZERO: i64 = 3;

/// Resolves a padding request against whether the padded value is a number.
///
/// Returns the concrete alignment and the fill character to use. `fill` is
/// the fill the spec wrote, which the `0` flag replaces only on a number.
#[must_use]
pub fn resolve_pad_request(request: i64, fill: char, numeric: bool) -> (i64, char) {
    let written = request & PAD_REQUEST_ALIGN_MASK;
    let zero_flag = request & PAD_REQUEST_ZERO_FLAG != 0;
    if zero_flag && numeric {
        return (PAD_ALIGN_SIGN_AWARE_ZERO, '0');
    }
    let align = match written {
        PAD_REQUEST_LEFT => PAD_ALIGN_LEFT,
        PAD_REQUEST_CENTER => PAD_ALIGN_CENTER,
        PAD_REQUEST_RIGHT => PAD_ALIGN_RIGHT,
        _ if numeric => PAD_ALIGN_RIGHT,
        _ => PAD_ALIGN_LEFT,
    };
    (align, fill)
}

/// Splits a rendered number into the prefix zero padding goes after and the
/// digits it goes before.
///
/// [`PAD_ALIGN_SIGN_AWARE_ZERO`] puts its zeros between a number's sign and
/// its digits, and after a `{:#x}`-style radix prefix, so `{:08}` on `-42`
/// reads `-0000042`.
#[must_use]
pub fn sign_aware_prefix_len(text: &str) -> usize {
    let bytes = text.as_bytes();
    let mut len = usize::from(matches!(bytes.first(), Some(b'-' | b'+')));
    if bytes.len() >= len + 2
        && bytes[len] == b'0'
        && matches!(bytes[len + 1], b'x' | b'X' | b'b' | b'B' | b'o' | b'O')
    {
        len += 2;
    }
    len
}

#[cfg(test)]
mod format_pad_tests {
    use super::*;

    #[test]
    fn omitted_alignment_follows_the_value() {
        assert_eq!(
            resolve_pad_request(PAD_REQUEST_DEFAULT, ' ', true),
            (PAD_ALIGN_RIGHT, ' ')
        );
        assert_eq!(
            resolve_pad_request(PAD_REQUEST_DEFAULT, ' ', false),
            (PAD_ALIGN_LEFT, ' ')
        );
    }

    #[test]
    fn zero_flag_reaches_numbers_only() {
        let request = PAD_REQUEST_DEFAULT | PAD_REQUEST_ZERO_FLAG;
        assert_eq!(
            resolve_pad_request(request, ' ', true),
            (PAD_ALIGN_SIGN_AWARE_ZERO, '0')
        );
        assert_eq!(
            resolve_pad_request(request, ' ', false),
            (PAD_ALIGN_LEFT, ' ')
        );
    }

    #[test]
    fn zero_flag_outranks_a_written_alignment_on_a_number() {
        let request = PAD_REQUEST_LEFT | PAD_REQUEST_ZERO_FLAG;
        assert_eq!(
            resolve_pad_request(request, ' ', true),
            (PAD_ALIGN_SIGN_AWARE_ZERO, '0')
        );
    }

    #[test]
    fn sign_and_radix_prefix_stay_left_of_the_zeros() {
        assert_eq!(sign_aware_prefix_len("-42"), 1);
        assert_eq!(sign_aware_prefix_len("42"), 0);
        assert_eq!(sign_aware_prefix_len("0xff"), 2);
        assert_eq!(sign_aware_prefix_len("-0b101"), 3);
        assert_eq!(sign_aware_prefix_len(""), 0);
    }

    #[test]
    fn a_written_zero_fill_is_not_sign_aware() {
        assert_eq!(
            resolve_pad_request(PAD_REQUEST_RIGHT, '0', true),
            (PAD_ALIGN_RIGHT, '0')
        );
    }
}
