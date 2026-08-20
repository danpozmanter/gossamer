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

use bzip2::Compression;
use bzip2::read::{BzDecoder, BzEncoder};

// ---------------------------------------------------------------
// bzip2 module - `std::compress::bzip2::{compress, decompress}`.
// Input is a `Vec<u8>` (GosVec), output is a
// `Result<Vec<u8>, errors::Error>` (GosResult: disc 0 Ok with a
// GosVec payload, disc 1 Err with a gos error handle). Byte-output
// is produced via the same `bzip2` crate + `read::Bz{Encoder,Decoder}`
// API the interpreter tier uses, so all three tiers agree bit-for-bit.
// ---------------------------------------------------------------

/// Reads a Gossamer `Vec<u8>` into owned bytes.
unsafe fn gosvec_u8_to_vec(v: *const super::vec::GosVec) -> Vec<u8> {
    unsafe { super::encoding::gosvec_u8(v) }
}

/// Wraps `bytes` in an `Ok(Vec<u8>)` `GosResult`.
fn ok_bytes_result(bytes: &[u8]) -> i128 {
    let v = super::encoding::bytes_to_gosvec(bytes);
    unsafe { super::vec::gos_rt_result_new(0, v as i64) }
}

fn err_bytes_result(msg: &str) -> i128 {
    let err = crate::c_abi::errors::error_new_from_bytes(msg.as_bytes());
    unsafe { super::vec::gos_rt_result_new(1, err as i64) }
}

/// `compress::bzip2::compress(data, level) -> Result<[u8], Error>` -
/// `level` clamped to 0..=9 (0 fastest, 9 best), matching the std impl.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_compress_bzip2_compress(
    data: *const super::vec::GosVec,
    level: i64,
) -> i128 {
    ffi_entry!(0i128, {
        let input = unsafe { gosvec_u8_to_vec(data) };
        let lvl = Compression::new(level.clamp(0, 9) as u32);
        use std::io::Read;
        let mut enc = BzEncoder::new(&input[..], lvl);
        let mut out = Vec::new();
        match enc.read_to_end(&mut out) {
            Ok(_) => ok_bytes_result(&out),
            Err(e) => err_bytes_result(&format!("bzip2: {e}")),
        }
    })
}

/// `compress::bzip2::decompress(data) -> Result<[u8], Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_compress_bzip2_decompress(data: *const super::vec::GosVec) -> i128 {
    ffi_entry!(0i128, {
        let input = unsafe { gosvec_u8_to_vec(data) };
        use std::io::Read;
        let mut dec = BzDecoder::new(&input[..]);
        let mut out = Vec::new();
        match dec.read_to_end(&mut out) {
            Ok(_) => ok_bytes_result(&out),
            Err(e) => err_bytes_result(&format!("bzip2: {e}")),
        }
    })
}
