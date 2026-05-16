#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::too_many_lines)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::similar_names)]
#![allow(clippy::many_single_char_names)]
#![allow(clippy::items_after_statements)]
#![allow(clippy::same_length_and_capacity)]
#![allow(clippy::cast_lossless)]
#![allow(clippy::doc_markdown)]
#![allow(clippy::cast_ptr_alignment)]
#![allow(clippy::ptr_as_ptr)]
#![allow(static_mut_refs)]
#![allow(unused_unsafe)]
#![allow(clippy::wildcard_imports)]

// ---------------------------------------------------------------
// Math helpers
// ---------------------------------------------------------------

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_sqrt(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.sqrt() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_pow(x: f64, y: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.powf(y) })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_sin(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.sin() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_cos(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.cos() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_log(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.ln() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_exp(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.exp() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_abs(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.abs() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_floor(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.floor() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_math_ceil(x: f64) -> f64 {
    ffi_entry!(f64::NAN, { x.ceil() })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_now_ms() -> i64 {
    ffi_entry!(-1, {
        use std::time::{SystemTime, UNIX_EPOCH};
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis() as i64)
    })
}
