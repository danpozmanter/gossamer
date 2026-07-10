//! Byte-slice strconv runtime tests.

use gossamer_runtime::c_abi::{
    gos_rt_result_disc, gos_rt_result_payload, gos_rt_result_payload_f64,
    gos_rt_strconv_parse_f64_bytes, gos_rt_strconv_parse_f64_range, gos_rt_strconv_parse_i64_bytes,
    gos_rt_strconv_parse_i64_range,
};

#[test]
fn parse_i64_bytes_trims_and_avoids_temporary_string_contract() {
    let input = b" \t-42\n";
    // SAFETY: `input` is a live byte slice for the duration of the call.
    unsafe {
        let result = gos_rt_strconv_parse_i64_bytes(input.as_ptr(), input.len() as i64);
        assert_eq!(gos_rt_result_disc(result), 0);
        assert_eq!(gos_rt_result_payload(result), -42);
    }
}

#[test]
fn parse_f64_bytes_trims_and_returns_bits_payload() {
    let input = b" 3.5 ";
    // SAFETY: `input` is a live byte slice for the duration of the call.
    unsafe {
        let result = gos_rt_strconv_parse_f64_bytes(input.as_ptr(), input.len() as i64);
        assert_eq!(gos_rt_result_disc(result), 0);
        assert_eq!(gos_rt_result_payload_f64(result), 3.5);
    }
}

#[test]
fn parse_bytes_rejects_invalid_utf8() {
    let input = [0xff, b'1'];
    // SAFETY: `input` is a live byte slice for the duration of the call.
    unsafe {
        let result = gos_rt_strconv_parse_i64_bytes(input.as_ptr(), input.len() as i64);
        assert_eq!(gos_rt_result_disc(result), 1);
    }
}

#[test]
fn parse_i64_range_validates_like_string_slice() {
    let input = std::ffi::CString::new("xx -17 yy").unwrap();
    // SAFETY: `input` is a live C string for the duration of the calls.
    unsafe {
        let ok = gos_rt_strconv_parse_i64_range(input.as_ptr(), 2, 6);
        assert_eq!(gos_rt_result_disc(ok), 0);
        assert_eq!(gos_rt_result_payload(ok), -17);

        let bad = gos_rt_strconv_parse_i64_range(input.as_ptr(), 6, 2);
        assert_eq!(gos_rt_result_disc(bad), 1);
    }
}

#[test]
fn parse_f64_range_returns_bits_payload() {
    let input = std::ffi::CString::new("score=12.25;").unwrap();
    // SAFETY: `input` is a live C string for the duration of the call.
    unsafe {
        let result = gos_rt_strconv_parse_f64_range(input.as_ptr(), 6, 11);
        assert_eq!(gos_rt_result_disc(result), 0);
        assert_eq!(gos_rt_result_payload_f64(result), 12.25);
    }
}
