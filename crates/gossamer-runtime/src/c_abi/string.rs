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

use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::Ordering;

use super::*;

// ---------------------------------------------------------------
// String runtime
// ---------------------------------------------------------------
// Strings are represented as owning `CString`-shaped pointers
// allocated by Rust's `String::into_boxed_str`/`into_raw`. The
// pointer passed across the FFI is the first byte of the UTF-8
// payload; it is nul-terminated so C code can `%s`-print it. We
// track length separately by scanning for the nul byte in the C
// ABI; users that want O(1) length should use the GosStr header
// helpers (future). For L2 the single-owner story is enough.

unsafe fn c_str_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    unsafe { CStr::from_ptr(s).to_bytes().len() }
}

/// Allocator-provenance tag written 1 byte BEFORE every cstring
/// returned by `alloc_cstring`. `gos_rt_str_free` reads this byte
/// and refuses to reclaim anything whose prefix does not match,
/// turning "free a foreign pointer" from a heap-corruption silent
/// crash into a one-line stderr leak. Bump the value when the
/// allocator layout changes so older binaries' frees don't
/// collide with the new shape.
const STR_ALLOC_TAG: u8 = 0xA9;

/// Tag for growable strings allocated by `alloc_growable`.
/// Layout: `[cap:u32 LE][len:u32 LE][tag=0xAB][content(cap bytes)][NUL]`
/// `ptr` is 9 bytes past the start of the allocation (at `content[0]`).
/// `ptr[-1]` = tag, `ptr[-5..-1]` = len (u32 LE), `ptr[-9..-5]` = cap (u32 LE).
/// Total allocation: cap + 10 bytes.
const STR_BUILDER_TAG: u8 = 0xAB;

/// Tag for static string literals emitted into rodata by codegen with a
/// length-carrying header `[len:u32 LE][tag=0xA8][content][NUL]` (body at
/// base+5). Like the builder layout, the length is at `ptr[-5..-1]`, giving
/// O(1) `gos_rt_str_len`; the distinct tag tells `gos_rt_str_free` to leak
/// (never `free` rodata).
const STR_STATIC_TAG: u8 = 0xA8;

/// Tag for growable strings whose backing bytes live in an arena region.
/// Same `[cap][len][tag][content][NUL]` layout as `STR_BUILDER_TAG` (so
/// length reads and in-place append work identically), but the bytes are
/// freed wholesale at `arena_pop`, so `gos_rt_str_free` skips them.
const STR_REGION_TAG: u8 = 0xAA;

/// O(1) byte length for strings carrying a length header (builder + static
/// layouts both store `len:u32` at `ptr[-5]`). Returns `None` for foreign /
/// untagged pointers, where the caller must fall back to `strlen`. Reading
/// `ptr[-5..-1]` and `ptr[-1]` on a foreign pointer is the same probabilistic
/// prefix probe `gos_rt_str_free` already relies on.
#[inline]
unsafe fn str_header_len(s: *const c_char) -> Option<usize> {
    if s.is_null() {
        return Some(0);
    }
    let tag = unsafe { *s.cast::<u8>().sub(1) };
    if tag == STR_BUILDER_TAG || tag == STR_STATIC_TAG || tag == STR_REGION_TAG {
        let p = unsafe { s.cast::<u8>().sub(5) };
        let len = u32::from_le_bytes(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] });
        Some(len as usize)
    } else {
        None
    }
}

/// Allocates a growable string with `cap` bytes of content capacity.
/// `parts` are concatenated into the initial content (total must be <= cap).
/// Returns a pointer to `content[0]`; the 9-byte header lives just before it.
fn alloc_growable(parts: &[&[u8]], cap: usize) -> *mut c_char {
    let content_len: usize = parts.iter().map(|p| p.len()).sum();
    debug_assert!(cap >= content_len, "alloc_growable: cap < content length");
    // rc(4) + cap(4) + len(4) + tag(1) + content(cap) + NUL(1) = cap + 14.
    // Refcount at the FRONT keeps cap(-9)/len(-5)/tag(-1) offsets unchanged.
    let total = 4 + 4 + 4 + 1 + cap + 1;
    // Inside an arena region, allocate the bytes from the region (freed
    // wholesale at pop; `gos_rt_str_free` skips the region tag). Else a
    // boxed slice on the global allocator.
    let region_base = crate::c_abi::rc::region_alloc_bytes(total);
    let (base, tag) = if region_base.is_null() {
        let v: Vec<u8> = vec![0u8; total];
        (
            Box::into_raw(v.into_boxed_slice()).cast::<u8>(),
            STR_BUILDER_TAG,
        )
    } else {
        // region bytes are already zeroed.
        (region_base, STR_REGION_TAG)
    };
    // SAFETY: `base` points to `total` writable, zeroed bytes.
    unsafe {
        std::ptr::copy_nonoverlapping(1u32.to_le_bytes().as_ptr(), base, 4);
        std::ptr::copy_nonoverlapping((cap as u32).to_le_bytes().as_ptr(), base.add(4), 4);
        std::ptr::copy_nonoverlapping((content_len as u32).to_le_bytes().as_ptr(), base.add(8), 4);
        *base.add(12) = tag;
        let mut off = 13;
        for p in parts {
            std::ptr::copy_nonoverlapping(p.as_ptr(), base.add(off), p.len());
            off += p.len();
        }
        // NUL already zero. Region-allocated strings are bulk-freed at
        // `arena_pop` and intentionally skipped by `gos_rt_str_free`, so they
        // never `str_dec`. Counting them in the per-string leak ledger would
        // report a false positive (the memory is reclaimed wholesale at pop);
        // only individually-managed (`STR_BUILDER`) strings are tracked.
        if tag != STR_REGION_TAG {
            crate::c_abi::ledger::str_inc();
        }
        base.add(13).cast::<c_char>()
    }
}

/// Reclaims a c-string previously returned by [`alloc_cstring`].
/// Reads the allocator-provenance tag at `s[-1]` and reconstructs
/// the original `Box<[u8]>` covering `tag(1) + content(strlen) +
/// NUL(1)`. The cleanup pass emits a call to this helper at every
/// body return for a non-escaping String produced by a known
/// String allocator (e.g. `gos_rt_stream_read_to_string`); the
/// escape analyser's non-capturing-callee whitelist ensures only
/// owning bindings reach this path so the drop never observes an
/// aliased pointer.
///
/// SAFETY: caller guarantees that `s` was allocated by
/// `alloc_cstring` (so the byte at offset `-1` is `STR_ALLOC_TAG`)
/// and that no other live pointer aliases it. If the prefix byte
/// does not match, the call leaks the allocation rather than
/// corrupting the allocator's free list.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_free(s: *mut c_char) {
    ffi_entry!((), {
        if s.is_null() {
            return;
        }
        // Tag check at offset -1. A mismatch means the caller handed
        // us a cstring that did NOT come from `alloc_cstring` (foreign
        // allocation, libc-owned argv string, or a static literal).
        // Reclaiming such a pointer with `Box::from_raw` corrupts the
        // global allocator's free list — leak instead.
        let tag_ptr = unsafe { s.cast::<u8>().sub(1) };
        let tag = unsafe { *tag_ptr };
        if tag == STR_REGION_TAG {
            // Region-allocated: freed wholesale at arena_pop, never here.
            return;
        }
        if tag == STR_ALLOC_TAG {
            // Fixed layout: [tag(1)][content][NUL(1)]
            let content_len = unsafe { c_str_len(s) };
            let total = 1 + content_len + 1;
            let slice = std::ptr::slice_from_raw_parts_mut(tag_ptr, total);
            drop(unsafe { Box::from_raw(slice) });
        } else if tag == STR_BUILDER_TAG {
            // Refcounted: [rc:u32][cap:u32][len:u32][tag(1)][content][NUL].
            // Decrement; reclaim only at zero.
            let hdr = unsafe { s.cast::<u8>().sub(13) };
            let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
            if rc > 1 {
                unsafe {
                    std::ptr::copy_nonoverlapping((rc - 1).to_le_bytes().as_ptr(), hdr, 4);
                }
                return;
            }
            let cap =
                u32::from_le_bytes(unsafe { [*hdr.add(4), *hdr.add(5), *hdr.add(6), *hdr.add(7)] })
                    as usize;
            let total = 4 + 4 + 4 + 1 + cap + 1;
            let slice = std::ptr::slice_from_raw_parts_mut(hdr, total);
            drop(unsafe { Box::from_raw(slice) });
            crate::c_abi::ledger::str_dec();
        } else if tag == STR_STATIC_TAG {
            // Static rodata literal — never freed.
        } else {
            eprintln!(
                "gos_rt_str_free: allocator tag mismatch (got 0x{tag:02x}, \
             expected 0x{STR_ALLOC_TAG:02x}) — refusing to free"
            );
        }
    });
}

/// True when `s` is a Gossamer-allocated string (tag byte at offset -1 in
/// `0xA8..=0xAB`). An RC object's byte at -1 is its (canonical) `meta` high
/// byte = `0x00`, so the ranges never collide — the RC retain/release dispatch
/// uses this to route strings to the string allocator.
#[inline]
pub unsafe fn is_gos_string(s: *const c_char) -> bool {
    !s.is_null() && matches!(unsafe { *s.cast::<u8>().sub(1) }, 0xA8..=0xAB)
}

/// Increment a heap (`STR_BUILDER_TAG`) string's refcount; no-op otherwise.
pub(crate) unsafe fn gos_rt_str_retain(s: *const c_char) {
    if s.is_null() || unsafe { *s.cast::<u8>().sub(1) } != STR_BUILDER_TAG {
        return;
    }
    let hdr = unsafe { s.cast::<u8>().sub(13) };
    let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
    unsafe {
        std::ptr::copy_nonoverlapping(
            rc.saturating_add(1).to_le_bytes().as_ptr(),
            hdr.cast_mut(),
            4,
        );
    }
}

/// Allocate an owned, NUL-terminated heap string holding `s`'s bytes (the
/// `STR_ALLOC_TAG` allocator shape).
pub fn alloc_cstring(s: &[u8]) -> *mut c_char {
    // Pick the first NUL (if any) so we never copy past it.
    let nul = s.iter().position(|&b| b == 0).unwrap_or(s.len());
    let len = nul;
    alloc_cstring_from_slices(&[&s[..len]])
}

/// Allocates one c-string holding the byte-wise concatenation of
/// `parts`, with a single allocator round trip. Used by
/// `gos_rt_str_concat` (which previously allocated a transient
/// `Vec<u8>` and then re-allocated through `alloc_cstring`,
/// paying two malloc/free pairs per `+`).
///
/// Layout: one allocator-tag byte, then the joined content bytes,
/// then NUL. The returned pointer is 1 byte into the allocation
/// (the content head) so `CStr::from_ptr` and `strlen` see a
/// normal c-string; `gos_rt_str_free` reads `ptr[-1]` to verify
/// the allocation originated here.
pub fn alloc_cstring_from_slices(parts: &[&[u8]]) -> *mut c_char {
    // Use the length-carrying builder layout (cap = content length) so the
    // result has its byte length stored at `ptr[-5]` for O(1)
    // `gos_rt_str_len` / `gos_rt_str_slice`. A later in-place `+=` finds no
    // spare capacity and reallocates with doubling — correctness and the
    // amortised growth analysis are unchanged. `gos_rt_str_free` and the
    // concat fast path already handle `STR_BUILDER_TAG`.
    let total: usize = parts.iter().map(|p| p.len()).sum();
    alloc_growable(parts, total)
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_len(s: *const c_char) -> i64 {
    ffi_entry!(-1, {
        match unsafe { str_header_len(s) } {
            Some(len) => len as i64,
            None => unsafe { c_str_len(s) as i64 },
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_is_empty(s: *const c_char) -> bool {
    ffi_entry!(false, { unsafe { gos_rt_str_len(s) == 0 } })
}

/// Generic length-zero check used by `is_empty` for any
/// receiver whose length is reachable through `gos_rt_len`
/// (Vec / array / slice / hashmap …).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_len_is_zero(p: *const i64) -> bool {
    ffi_entry!(false, { unsafe { gos_rt_len(p) == 0 } })
}

/// Clones a `*mut GosVec` element-by-element. Used by
/// `xs.to_vec()` so the result is independent of the source —
/// without this, the previous identity lowering aliased the
/// source buffer and mutations like `out.swap(i, j)` clobbered
/// the caller's input.
///
/// **Allocator domain:** the header is `Box::into_raw` and the
/// data buffer is `Vec<u8>` (`Global`-allocated, then `forget`-ed),
/// so the buffer matches the layout `gos_rt_vec_push` reconstructs
/// via `Vec::from_raw_parts(...)` when the vec needs to grow. The
/// previous version allocated both from the bump arena
/// (`gos_rt_gc_alloc`); a subsequent push past `cap` would feed an
/// arena interior pointer to the global allocator's deallocator, a
/// cross-domain free that produced heisencrashes anywhere else in
/// the runtime malloc'd next. See
/// `~/dev/contexts/lang/fix_architecture_ownership.md` §3.1.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_clone(src: *const GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if src.is_null() {
            return unsafe { gos_rt_vec_new(8) };
        }
        let s = unsafe { &*src };
        let bytes = (s.len as usize) * (s.elem_bytes as usize);
        let data: *mut u8 = if bytes == 0 || s.ptr.is_null() {
            std::ptr::null_mut::<u8>()
        } else {
            let mut buf: Vec<u8> = vec![0u8; bytes];
            unsafe {
                std::ptr::copy_nonoverlapping(s.ptr.as_ptr(), buf.as_mut_ptr(), bytes);
            }
            let p = buf.as_mut_ptr();
            std::mem::forget(buf);
            p
        };
        // Ledger + initial strong count, symmetric with `alloc_vec_header`
        // (gos_rt_vec_free always decrements the ledger).
        crate::c_abi::ledger::vec_inc();
        let mut cloned = GosVec {
            len: s.len,
            cap: s.len,
            elem_bytes: s.elem_bytes,
            elem_kind: s.elem_kind,
            _reserved: [0; 3],
            ptr: SyncRawPtr::new(data),
        };
        crate::c_abi::vec::vec_set_rc(&mut cloned, 1);
        let out = Box::into_raw(Box::new(cloned));
        // A clone of a guarded-aggregate vec shares every element's
        // copy-blob children with the source; register the same meta and
        // give the clone its own shares.
        if s.elem_kind == crate::c_abi::vec::vec_elem_kind::AGGR_GUARDED {
            let meta = crate::c_abi::vec::vec_elem_meta(src);
            if !meta.is_null() {
                unsafe { crate::c_abi::vec::gos_rt_vec_set_elem_meta(out, meta) };
                let stride = s.elem_bytes as usize;
                for i in 0..s.len.max(0) as usize {
                    unsafe {
                        crate::c_abi::rc::gos_rt_aggr_retain_children(data.add(i * stride), meta);
                    }
                }
            }
        }
        // STRING / VEC / AGGR_OWNED elements: the raw byte copy above
        // shares every element's heap children with the source; give
        // the clone its own shares so both deep-frees are balanced.
        unsafe { crate::c_abi::vec::vec_share_owned_elements(src, out) };
        out
    })
}

/// Materialises `s.as_bytes()` as a real `GosVec<u8>` so callees
/// receiving `&[u8]` can call `.len()` / `.iter()` / index it
/// the same way they would any other slice. The previous
/// identity lowering returned the raw c-string ptr — `.len()`
/// on it read the first 8 content bytes as a Vec length prefix,
/// and `.iter()` walked off into garbage. Backing buffer +
/// header are arena-allocated; the next `gos_rt_gc_reset`
/// reclaims them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_as_bytes(s: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let len = if s.is_null() {
            0
        } else {
            unsafe { CStr::from_ptr(s).to_bytes().len() }
        };
        // The returned Vec is consumed by `bytes[i]` indexing in
        // user code, which the codegen lowers via the Vec/Slice
        // dispatch (`gos_rt_vec_get_i64`) — every slot is i64-shaped.
        // Materialise each byte as a zero-extended i64 so the load
        // returns the byte's value rather than 8 packed buffer
        // bytes. Use `gos_rt_vec_with_capacity` so the resulting
        // header is `Box::from_raw`-compatible — the auto-emitted
        // `gos_rt_vec_free` at scope-end relies on that
        // provenance.
        let v = unsafe { gos_rt_vec_with_capacity(8, len as i64) };
        if v.is_null() {
            return v;
        }
        if len > 0 && !s.is_null() {
            unsafe {
                let src = s.cast::<u8>();
                let header = &mut *v;
                let dst = header.ptr.cast::<i64>();
                for i in 0..len {
                    *dst.add(i) = i64::from(*src.add(i));
                }
                header.len = len as i64;
            }
        }
        v
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_byte_at(s: *const c_char, i: i64) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || i < 0 {
            return 0;
        }
        // Strings are null-terminated and treated as immutable
        // bytes. The previous implementation called
        // `CStr::from_ptr(s).to_bytes()` which walks the string with
        // `strlen` on every access — fasta-style hot loops doing
        // `s[idx % len]` paid O(strlen) per byte. The user's loop is
        // expected to keep `idx` in range (e.g. `% alu_len` against
        // a precomputed `alu_len = alu.len()`); reading past the
        // null terminator returns zero, which is what callers expect
        // anyway.
        let byte = unsafe { *s.cast::<u8>().add(i as usize) };
        i64::from(byte)
    })
}

/// `os::read_dir(path) -> Result<Vec<String>, errors::Error>` —
/// returns the entry names under `path` as a `*mut GosVec` of
/// `*const c_char`. Gossamer programs treat the call as
/// fallible, but the MIR pin keeps it as a plain `Vec<String>`
/// today (matching the interp's shape) — error cases land as an
/// empty vec rather than a Result-shaped Adt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_read_dir(path: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let p = if path.is_null() {
            ".".to_string()
        } else {
            unsafe { CStr::from_ptr(path).to_string_lossy().into_owned() }
        };
        let entries: Vec<String> = match std::fs::read_dir(&p) {
            Ok(it) => {
                let mut names: Vec<String> = it
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect();
                names.sort();
                names
            }
            Err(_) => Vec::new(),
        };
        // STRING-typed: the vec owns the entry-name strings.
        let out = unsafe {
            crate::c_abi::vec::gos_rt_vec_new_typed(8, crate::c_abi::vec::vec_elem_kind::STRING)
        };
        for name in entries {
            let cs = alloc_cstring(name.as_bytes()) as i64;
            unsafe {
                gos_rt_vec_push_i64(out, cs);
            }
        }
        out
    })
}

/// `s.substring(start, end)` — byte-range slice. Clamps `start`
/// and `end` into `[0, len(s)]` and returns the indicated byte
/// substring as a fresh `*mut c_char`. Mirrors the interp
/// builtin so user code that calls `s.substring(a, b)` runs the
/// same way under `gos run` and `gos build` — without this
/// helper the compiled tier saw `s.substring(...)` as an
/// undispatched method, fell through to a free-fn lookup, and
/// resolved to a user-defined `pub fn substring` (askq's
/// `util::substring` calls `s.substring` recursively, which then
/// stack-overflowed instead of reaching the runtime slice).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_substring(
    s: *const c_char,
    start: i64,
    end: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if s.is_null() {
            return alloc_cstring(b"");
        }
        let bytes = unsafe { CStr::from_ptr(s) }.to_bytes();
        let len = bytes.len() as i64;
        let lo = start.clamp(0, len) as usize;
        let hi = end.clamp(0, len).max(start.clamp(0, len)) as usize;
        alloc_cstring(&bytes[lo..hi])
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_concat(a: *const c_char, b: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        // Cheap empty-checks that only touch the first byte. We still
        // need `strlen` on the non-empty side(s) to size the new
        // allocation, but writing into the destination directly via
        // `alloc_cstring_from_slices` saves one intermediate `Vec`
        // and the two `extend_from_slice` copies the previous body
        // performed.
        let a_empty = a.is_null() || unsafe { *a.cast::<u8>() } == 0;
        let b_empty = b.is_null() || unsafe { *b.cast::<u8>() } == 0;
        let a_bytes: &[u8] = if a_empty {
            &[]
        } else {
            unsafe { CStr::from_ptr(a).to_bytes() }
        };
        let b_bytes: &[u8] = if b_empty {
            &[]
        } else {
            unsafe { CStr::from_ptr(b).to_bytes() }
        };
        alloc_cstring_from_slices(&[a_bytes, b_bytes])
    })
}

/// Concatenates `a + b`, frees `a`, and returns the result.
///
/// Implements amortized O(1) string accumulation: when `a` is already a
/// growable string (`STR_BUILDER_TAG`) with enough spare capacity, `b` is
/// appended in-place without any allocation. When capacity is exhausted the
/// buffer is reallocated with 2x the required size, giving O(n) total copy
/// work across n append operations (standard doubling analysis).
///
/// Safe when `a` is null or a rodata literal: those paths allocate a fresh
/// growable buffer rather than attempting to free an unowned pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_concat_drop_a(
    a: *const c_char,
    b: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let a_empty = a.is_null() || unsafe { *a.cast::<u8>() } == 0;
        let b_empty = b.is_null() || unsafe { *b.cast::<u8>() } == 0;

        if b_empty {
            // Nothing new to append; return a as-is when it is already owned.
            if !a.is_null() {
                let tag = unsafe { *a.cast::<u8>().sub(1) };
                if tag == STR_BUILDER_TAG || tag == STR_ALLOC_TAG || tag == STR_REGION_TAG {
                    return a.cast_mut();
                }
            }
            let a_bytes: &[u8] = if a_empty {
                &[]
            } else {
                unsafe { CStr::from_ptr(a).to_bytes() }
            };
            return alloc_growable(&[a_bytes], 64.max(a_bytes.len()));
        }

        let b_bytes: &[u8] = unsafe { CStr::from_ptr(b).to_bytes() };
        let len_b = b_bytes.len();

        // Fast path: a is a growable string (heap or region) — try in-place
        // append. Region builders share the layout; their backing bytes are
        // writable until pop, and the realloc branch routes a fresh buffer
        // through the region too (and `gos_rt_str_free(a)` no-ops on it).
        if !a.is_null() {
            let tag = unsafe { *a.cast::<u8>().sub(1) };
            if tag == STR_BUILDER_TAG || tag == STR_REGION_TAG {
                let hdr = unsafe { a.cast::<u8>().sub(13) };
                let rc =
                    u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
                let cap = u32::from_le_bytes(unsafe {
                    [*hdr.add(4), *hdr.add(5), *hdr.add(6), *hdr.add(7)]
                }) as usize;
                let len_a = u32::from_le_bytes(unsafe {
                    [*hdr.add(8), *hdr.add(9), *hdr.add(10), *hdr.add(11)]
                }) as usize;
                let new_len = len_a + len_b;
                // In-place only when sole owner (rc == 1): mutating a shared
                // buffer would corrupt other holders.
                if new_len <= cap && (rc == 1 || tag == STR_REGION_TAG) {
                    unsafe {
                        let dst = (a as *mut u8).add(len_a);
                        std::ptr::copy_nonoverlapping(b_bytes.as_ptr(), dst, len_b);
                        *dst.add(len_b) = 0;
                        let hdr_mut = hdr.cast_mut();
                        std::ptr::copy_nonoverlapping(
                            (new_len as u32).to_le_bytes().as_ptr(),
                            hdr_mut.add(8),
                            4,
                        );
                    }
                    return a.cast_mut();
                }
                // Shared or capacity exhausted: copy, allocate fresh, drop one ref.
                let a_content = unsafe { std::slice::from_raw_parts(a.cast::<u8>(), len_a) };
                let new_cap = (new_len * 2).max(64);
                let result = alloc_growable(&[a_content, b_bytes], new_cap);
                unsafe { gos_rt_str_free(a.cast_mut()) };
                return result;
            }
        }

        // a is null, a literal, or a fixed heap string — allocate fresh growable.
        let a_bytes: &[u8] = if a_empty {
            &[]
        } else {
            unsafe { CStr::from_ptr(a).to_bytes() }
        };
        let new_len = a_bytes.len() + len_b;
        let new_cap = (new_len * 2).max(64);
        let result = alloc_growable(&[a_bytes, b_bytes], new_cap);
        if !a.is_null() {
            let tag = unsafe { *a.cast::<u8>().sub(1) };
            if tag == STR_ALLOC_TAG {
                unsafe { gos_rt_str_free(a.cast_mut()) };
            }
        }
        result
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_trim(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        let st = std::str::from_utf8(bytes).unwrap_or("");
        alloc_cstring(st.trim().as_bytes())
    })
}

/// `s.trim_start() / strings::trim_start(s)` — strips leading
/// Unicode whitespace, mirroring Rust's `str::trim_start`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_trim_start(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        let st = std::str::from_utf8(bytes).unwrap_or("");
        alloc_cstring(st.trim_start().as_bytes())
    })
}

/// `s.trim_end() / strings::trim_end(s)` — strips trailing
/// Unicode whitespace, mirroring Rust's `str::trim_end`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_trim_end(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        let st = std::str::from_utf8(bytes).unwrap_or("");
        alloc_cstring(st.trim_end().as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_upper(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        let st = std::str::from_utf8(bytes).unwrap_or("");
        alloc_cstring(st.to_uppercase().as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_lower(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let bytes = if s.is_null() {
            b"" as &[u8]
        } else {
            unsafe { CStr::from_ptr(s).to_bytes() }
        };
        let st = std::str::from_utf8(bytes).unwrap_or("");
        alloc_cstring(st.to_lowercase().as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_contains(s: *const c_char, needle: *const c_char) -> i32 {
    ffi_entry!(-1, {
        if s.is_null() || needle.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(s).to_bytes() };
        let n = unsafe { CStr::from_ptr(needle).to_bytes() };
        if n.is_empty() {
            return 1;
        }
        if s.len() < n.len() {
            return 0;
        }
        for i in 0..=(s.len() - n.len()) {
            if &s[i..i + n.len()] == n {
                return 1;
            }
        }
        0
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_starts_with(s: *const c_char, prefix: *const c_char) -> i32 {
    ffi_entry!(-1, {
        if s.is_null() || prefix.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(s).to_bytes() };
        let p = unsafe { CStr::from_ptr(prefix).to_bytes() };
        i32::from(s.starts_with(p))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_ends_with(s: *const c_char, suffix: *const c_char) -> i32 {
    ffi_entry!(-1, {
        if s.is_null() || suffix.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(s).to_bytes() };
        let suf = unsafe { CStr::from_ptr(suffix).to_bytes() };
        i32::from(s.ends_with(suf))
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_find(s: *const c_char, needle: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || needle.is_null() {
            return -1;
        }
        let s = unsafe { CStr::from_ptr(s).to_bytes() };
        let n = unsafe { CStr::from_ptr(needle).to_bytes() };
        if n.is_empty() {
            return 0;
        }
        if s.len() < n.len() {
            return -1;
        }
        for i in 0..=(s.len() - n.len()) {
            if &s[i..i + n.len()] == n {
                return i as i64;
            }
        }
        -1
    })
}

/// `s.find(needle) -> Option<i64>` packed as a `*mut GosResult`
/// (`disc 0 = Some(idx)`, `disc 1 = None`). Wraps the raw i64
/// `gos_rt_str_find` return so cranelift's match-on-Option
/// lowering reads the right discriminant — the bare i64 form
/// produces a Value the SwitchInt path always treats as Some
/// because -1 doesn't correspond to either Some-disc (0) or
/// None-disc (1).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_find_opt(s: *const c_char, needle: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let idx = unsafe { gos_rt_str_find(s, needle) };
        if idx < 0 {
            unsafe { gos_rt_result_new(1, 0) }
        } else {
            unsafe { gos_rt_result_new(0, idx) }
        }
    })
}

/// `s.rfind(needle) -> Option<i64>` packed as a `*mut GosResult`
/// (`disc 0 = Some(idx)`, `disc 1 = None`). Byte-level scan from
/// the right; empty needle returns `Some(s.len())` to mirror Rust's
/// `str::rfind` semantics.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_rfind_opt(s: *const c_char, needle: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() || needle.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let hay = unsafe { CStr::from_ptr(s).to_bytes() };
        let n = unsafe { CStr::from_ptr(needle).to_bytes() };
        if n.is_empty() {
            return unsafe { gos_rt_result_new(0, hay.len() as i64) };
        }
        if hay.len() < n.len() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let upper = hay.len() - n.len();
        for i in (0..=upper).rev() {
            if &hay[i..i + n.len()] == n {
                return unsafe { gos_rt_result_new(0, i as i64) };
            }
        }
        unsafe { gos_rt_result_new(1, 0) }
    })
}

/// `s == t` for string operands. Compares byte-for-byte. NULL
/// pointers compare equal to empty strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_eq(a: *const c_char, b: *const c_char) -> bool {
    ffi_entry!(false, {
        let a = if a.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(a).to_str() }.unwrap_or("")
        };
        let b = if b.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(b).to_str() }.unwrap_or("")
        };
        a == b
    })
}

/// Lexicographic ordering of two C strings. Returns negative / zero /
/// positive like libc `strcmp`, but through Rust `Ord` so UTF-8 bytes
/// compare correctly. Used by the compiled tier for `a < b`, `a > b`,
/// etc. when both operands are `String` or `&String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_compare(a: *const c_char, b: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let a = if a.is_null() {
            b""
        } else {
            unsafe { CStr::from_ptr(a).to_bytes() }
        };
        let b = if b.is_null() {
            b""
        } else {
            unsafe { CStr::from_ptr(b).to_bytes() }
        };
        match a.cmp(b) {
            std::cmp::Ordering::Less => -1,
            std::cmp::Ordering::Equal => 0,
            std::cmp::Ordering::Greater => 1,
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_replace(
    s: *const c_char,
    from: *const c_char,
    to: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let f = if from.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(from).to_str().unwrap_or("") }
        };
        let t = if to.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(to).to_str().unwrap_or("") }
        };
        alloc_cstring(s.replace(f, t).as_bytes())
    })
}

/// `s.split_once(sep) -> Option<(String, String)>`. Returns a
/// `*mut GosResult` with `disc=0` holding a heap-allocated
/// `{a: *mut c_char, b: *mut c_char}` pair; `disc=1` for None
/// (separator not found, or null/empty input). Mirrors the
/// `find_opt` packing convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_split_once(s: *const c_char, sep: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() || sep.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        let sep = unsafe { CStr::from_ptr(sep).to_str().unwrap_or("") };
        if sep.is_empty() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match s.split_once(sep) {
            None => unsafe { gos_rt_result_new(1, 0) },
            Some((a, b)) => {
                #[repr(C)]
                struct Pair {
                    a: i64,
                    b: i64,
                }
                let pair = Box::into_raw(Box::new(Pair {
                    a: alloc_cstring(a.as_bytes()) as i64,
                    b: alloc_cstring(b.as_bytes()) as i64,
                }));
                unsafe { gos_rt_result_new(0, pair as i64) }
            }
        }
    })
}

/// `s.rsplit_once(sep) -> Option<(String, String)>`. Same shape as
/// `split_once` but anchored at the last occurrence of `sep`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_rsplit_once(s: *const c_char, sep: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() || sep.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        let sep = unsafe { CStr::from_ptr(sep).to_str().unwrap_or("") };
        if sep.is_empty() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        match s.rsplit_once(sep) {
            None => unsafe { gos_rt_result_new(1, 0) },
            Some((a, b)) => {
                #[repr(C)]
                struct Pair {
                    a: i64,
                    b: i64,
                }
                let pair = Box::into_raw(Box::new(Pair {
                    a: alloc_cstring(a.as_bytes()) as i64,
                    b: alloc_cstring(b.as_bytes()) as i64,
                }));
                unsafe { gos_rt_result_new(0, pair as i64) }
            }
        }
    })
}

/// `s.count(needle) -> i64`. Counts non-overlapping occurrences.
/// Empty needle returns 0 (avoid the infinite "match between every
/// byte" that Rust's `matches("")` produces).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_count(s: *const c_char, needle: *const c_char) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() || needle.is_null() {
            return 0;
        }
        let s = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        let n = unsafe { CStr::from_ptr(needle).to_str().unwrap_or("") };
        if n.is_empty() {
            return 0;
        }
        s.matches(n).count() as i64
    })
}

/// `s.strip_chars(cutset)` — trims any char in `cutset` from both
/// ends. Empty cutset is a no-op.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_strip_chars(
    s: *const c_char,
    cutset: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let cutset = if cutset.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(cutset).to_str().unwrap_or("") }
        };
        if cutset.is_empty() {
            return alloc_cstring(s.as_bytes());
        }
        let pat: Vec<char> = cutset.chars().collect();
        alloc_cstring(s.trim_matches(pat.as_slice()).as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_lstrip_chars(
    s: *const c_char,
    cutset: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let cutset = if cutset.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(cutset).to_str().unwrap_or("") }
        };
        if cutset.is_empty() {
            return alloc_cstring(s.as_bytes());
        }
        let pat: Vec<char> = cutset.chars().collect();
        alloc_cstring(s.trim_start_matches(pat.as_slice()).as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_rstrip_chars(
    s: *const c_char,
    cutset: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let cutset = if cutset.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(cutset).to_str().unwrap_or("") }
        };
        if cutset.is_empty() {
            return alloc_cstring(s.as_bytes());
        }
        let pat: Vec<char> = cutset.chars().collect();
        alloc_cstring(s.trim_end_matches(pat.as_slice()).as_bytes())
    })
}

/// `s.zfill(width)` — pad with `'0'` on the left until at least
/// `width` characters wide.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_zfill(s: *const c_char, width: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        if width <= 0 {
            return alloc_cstring(s.as_bytes());
        }
        let cur = s.chars().count();
        let w = width as usize;
        if cur >= w {
            return alloc_cstring(s.as_bytes());
        }
        let mut out = String::with_capacity(w);
        for _ in 0..(w - cur) {
            out.push('0');
        }
        out.push_str(s);
        alloc_cstring(out.as_bytes())
    })
}

/// `s.center(width, pad_char)` — symmetric pad to `width`. Pads
/// with `' '` if `pad_char` is 0 (caller defaulted).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_center(
    s: *const c_char,
    width: i64,
    pad_char: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        if width <= 0 {
            return alloc_cstring(s.as_bytes());
        }
        let cur = s.chars().count();
        let w = width as usize;
        if cur >= w {
            return alloc_cstring(s.as_bytes());
        }
        let pad = char::from_u32(pad_char as u32).unwrap_or(' ');
        let pad = if pad == '\0' { ' ' } else { pad };
        let total_pad = w - cur;
        let left_pad = total_pad / 2;
        let right_pad = total_pad - left_pad;
        let mut out = String::with_capacity(w * 4);
        for _ in 0..left_pad {
            out.push(pad);
        }
        out.push_str(s);
        for _ in 0..right_pad {
            out.push(pad);
        }
        alloc_cstring(out.as_bytes())
    })
}

/// `s.slice(start, end) -> Result<String, errors::Error>`. Errors on
/// out-of-range, inverted ranges, or non-UTF-8 boundary cuts. Result
/// payload pointers: `disc=0` → owned `*mut c_char`, `disc=1` →
/// `*mut GosError`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_slice(s: *const c_char, start: i64, end: i64) -> i128 {
    ffi_entry!(0i128, {
        // O(1) length when the string carries a header; `strlen` fallback for
        // foreign pointers. This is what makes a parser that slices a large
        // input at growing offsets O(n) instead of O(n^2).
        let len = if s.is_null() {
            0i64
        } else {
            match unsafe { str_header_len(s) } {
                Some(l) => l as i64,
                None => unsafe { c_str_len(s) as i64 },
            }
        };
        if start < 0 || end < 0 || start > end || end > len {
            let msg = format!("slice: range [{start}, {end}) out of bounds for length {len}");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let lo = start as usize;
        let hi = end as usize;
        // SAFETY: `0 <= lo <= hi <= len`, and the string body holds `len`
        // valid bytes; the range is within the allocation.
        let bytes: &[u8] = if s.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len as usize) }
        };
        let slice = &bytes[lo..hi];
        if std::str::from_utf8(slice).is_ok() {
            unsafe { gos_rt_result_new(0, alloc_cstring(slice) as i64) }
        } else {
            let msg = format!("slice: range [{start}, {end}) does not fall on UTF-8 boundaries");
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            unsafe { gos_rt_result_new(1, err as i64) }
        }
    })
}

/// Splits `s` on every occurrence of `sep` and returns a fresh
/// `*mut GosVec` of c-string pointers. Empty `sep` yields a
/// single-element vec containing the whole string (mirrors Rust's
/// `split` for the empty separator). Each split slice gets its
/// own heap-allocated nul-terminated copy so the caller can
/// hold them past the underlying string's lifetime.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_split(s: *const c_char, sep: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let sep = if sep.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(sep).to_str().unwrap_or("") }
        };
        let parts: Vec<*mut c_char> = if sep.is_empty() {
            vec![alloc_cstring(s.as_bytes())]
        } else {
            s.split(sep).map(|p| alloc_cstring(p.as_bytes())).collect()
        };
        // STRING-typed: the vec owns the pieces, so `gos_rt_vec_free`
        // reclaims them even when a consumer loop breaks early.
        let vec = unsafe {
            crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
                8,
                parts.len() as i64,
                crate::c_abi::vec::vec_elem_kind::STRING,
            )
        };
        for p in &parts {
            let pv = *p as i64;
            unsafe {
                gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>());
            }
        }
        vec
    })
}

/// `strings::join(parts, sep) -> String`. Joins the c-string
/// pointers held in `parts` (a `*mut GosVec` of `*mut c_char`)
/// with `sep` between each pair. Empty Vec yields `""`. Null
/// element pointers contribute the empty string for that slot.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_strings_join(
    parts: *const GosVec,
    sep: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if parts.is_null() {
            return alloc_cstring(b"");
        }
        let vec = unsafe { &*parts };
        let sep_str = if sep.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(sep).to_str().unwrap_or("") }
        };
        let len = vec.len.max(0) as usize;
        let mut out = String::new();
        for i in 0..len {
            if i > 0 {
                out.push_str(sep_str);
            }
            let p = unsafe { vec.ptr.add(i * (vec.elem_bytes as usize)) };
            // Each element is a `*const c_char` stored as i64 in the
            // Vec slot (matches `gos_rt_str_split` / `gos_rt_str_lines`
            // packing).
            let elem_ptr = unsafe { (p as *const i64).read_unaligned() } as *const c_char;
            if !elem_ptr.is_null() {
                let s = unsafe { CStr::from_ptr(elem_ptr).to_str().unwrap_or("") };
                out.push_str(s);
            }
        }
        alloc_cstring(out.as_bytes())
    })
}

/// Splits `s` on `\n` and returns a fresh `*mut GosVec` of
/// c-string pointers, one per line. Trailing empty lines
/// (from `"a\nb\n"`) are dropped to mirror Rust's `lines()`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_lines(s: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let parts: Vec<*mut c_char> = s.lines().map(|l| alloc_cstring(l.as_bytes())).collect();
        // STRING-typed — same ownership contract as `gos_rt_str_split`.
        let vec = unsafe {
            crate::c_abi::vec::gos_rt_vec_with_capacity_typed(
                8,
                parts.len() as i64,
                crate::c_abi::vec::vec_elem_kind::STRING,
            )
        };
        for p in &parts {
            let pv = *p as i64;
            unsafe {
                gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>());
            }
        }
        vec
    })
}

/// Returns `s` repeated `n` times. Rust's `String::repeat`
/// semantics: `n=0` returns the empty string, `n=1` returns a
/// fresh copy.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_repeat(s: *const c_char, n: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let n = if n < 0 { 0 } else { n as usize };
        alloc_cstring(s.repeat(n).as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_i64(s: *const c_char, ok_out: *mut i32) -> i64 {
    ffi_entry!(-1, {
        if s.is_null() {
            if !ok_out.is_null() {
                unsafe { *ok_out = 0 };
            }
            return 0;
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
        if let Ok(n) = text.parse::<i64>() {
            if !ok_out.is_null() {
                unsafe { *ok_out = 1 };
            }
            n
        } else {
            if !ok_out.is_null() {
                unsafe { *ok_out = 0 };
            }
            0
        }
    })
}

/// `text.parse::<i64>()` returning a `Result<i64, errors::Error>`.
/// Err payload is a `*mut GosError` so user code can call
/// `e.message()` directly without `map_err`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_i64_result(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            let cs = std::ffi::CString::new("parse: null input").unwrap();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            return unsafe { gos_rt_result_new(1, err as i64) };
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
        if let Ok(n) = text.parse::<i64>() {
            unsafe { gos_rt_result_new(0, n) }
        } else {
            let msg = format!(
                "unexpected byte 0x{:x} at 1:1",
                text.as_bytes().first().copied().unwrap_or(0)
            );
            let cs = std::ffi::CString::new(msg).unwrap_or_default();
            let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
            unsafe { gos_rt_result_new(1, err as i64) }
        }
    })
}

/// `result.map_err(closure)`. If Err, calls closure and rebuilds.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_map_err(result: i128, closure: *const u8) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(result) != 1 || closure.is_null() {
            return result;
        }
        // SAFETY: `closure` is a heap blob whose first word is the
        // lifted function's address (codegen invariant).
        let fn_addr = unsafe { *closure.cast::<i64>() };
        if fn_addr == 0 {
            return result;
        }
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr) };
        let new_payload = f(closure as i64, gos_rt_result_payload(result));
        unsafe { gos_rt_result_new(1, new_payload) }
    })
}

/// `result.map(closure)` for **capturing** closures whose lifted
/// function follows the env-first ABI `extern "C" fn(env, payload)
/// -> i64`. Non-capturing closures must dispatch through
/// [`gos_rt_result_map_bare`] instead — they have no env slot, so
/// passing one would shadow the payload arg and the closure would
/// transform the env pointer instead of the payload (the askq
/// round-2 corruption pre-fix).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_map(result: i128, closure: *const u8) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(result) != 0 || closure.is_null() {
            return result;
        }
        let fn_addr = unsafe { *closure.cast::<i64>() };
        if fn_addr == 0 {
            return result;
        }
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr) };
        let new_payload = f(closure as i64, gos_rt_result_payload(result));
        unsafe { gos_rt_result_new(0, new_payload) }
    })
}

/// `result::default_with(closure, result)` — returns the `Ok` value
/// unchanged, or calls `closure` on the `Err` payload and returns its
/// result. The returned `i64` is the unwrapped `T` (a scalar value or
/// a pointer, depending on `T`). Mirrors `gos_rt_result_map`'s closure
/// invocation convention.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_default_with(result: i128, closure: *const u8) -> i64 {
    ffi_entry!(0, {
        if gos_rt_result_disc(result) == 0 {
            return gos_rt_result_payload(result);
        }
        if closure.is_null() {
            return 0;
        }
        let fn_addr = unsafe { *closure.cast::<i64>() };
        if fn_addr == 0 {
            return 0;
        }
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr) };
        f(closure as i64, gos_rt_result_payload(result))
    })
}

/// `result::default(fallback, result)` — returns the `Ok` payload,
/// or `fallback` when the Result is `Err`. The returned `i64` is the
/// unwrapped `T` (a scalar value or a pointer, depending on `T`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_default(fallback: i64, result: i128) -> i64 {
    ffi_entry!(0, {
        if gos_rt_result_disc(result) == 0 {
            gos_rt_result_payload(result)
        } else {
            fallback
        }
    })
}

/// `result::default(fallback, result)` specialised for f64 payloads:
/// the stored payload word is reinterpreted as its IEEE-754 bit
/// pattern, and the fallback rides the float register directly.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_default_f64(fallback: f64, result: i128) -> f64 {
    ffi_entry!(0.0, {
        if gos_rt_result_disc(result) == 0 {
            f64::from_bits(gos_rt_result_payload(result) as u64)
        } else {
            fallback
        }
    })
}

/// `result.map(closure)` for **non-capturing** closures whose
/// lifted function follows the bare ABI `extern "C" fn(payload) ->
/// i64` (no env slot — this is what `gossamer-hir::lift_closed`
/// produces). The MIR call-site dispatch picks this entry point
/// when the closure arg has a recorded `local_fn_name` (i.e. is
/// a direct path to a lifted function rather than a heap-allocated
/// env+code blob).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_map_bare(result: i128, fn_addr: i64) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(result) != 0 || fn_addr == 0 {
            return result;
        }
        let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(fn_addr as *const ()) };
        let new_payload = f(gos_rt_result_payload(result));
        gos_rt_result_new(0, new_payload)
    })
}

/// `result.map_err(closure)` for **non-capturing** closures.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_map_err_bare(result: i128, fn_addr: i64) -> i128 {
    ffi_entry!(0i128, {
        if gos_rt_result_disc(result) == 0 || fn_addr == 0 {
            return result;
        }
        let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(fn_addr as *const ()) };
        let new_payload = f(gos_rt_result_payload(result));
        gos_rt_result_new(1, new_payload)
    })
}

/// `*cell` for `flag::Set::string` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_str(cell: *const *const c_char) -> *const c_char {
    ffi_entry!(std::ptr::null(), {
        if cell.is_null() {
            return std::ptr::null();
        }
        unsafe { *cell }
    })
}

/// `*cell` for `flag::Set::uint` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_i64(cell: *const i64) -> i64 {
    ffi_entry!(-1, {
        if cell.is_null() {
            return 0;
        }
        unsafe { *cell }
    })
}

/// `*cell` for `flag::Set::bool` cells, widened to i64.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_bool(cell: *const bool) -> i64 {
    ffi_entry!(-1, {
        if cell.is_null() {
            return 0;
        }
        i64::from(unsafe { *cell })
    })
}

/// `time::Duration::from_secs(n)` lowering — returns `n * 1000` as
/// the i64-millisecond Duration the compiled tier carries.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_secs(secs: i64) -> i64 {
    ffi_entry!(-1, { secs.saturating_mul(1_000) })
}

// `flag::parse([decls])` declarative parser — takes an array of
// `FlagDecl`-shaped blobs and returns a `FlagMap` handle.
// Layout per blob: `[name_cs, short_char, kind_tag, int_val,
// str_cs]` (5 * 8 = 40 bytes). `kind_tag` is 0=Int, 1=Str, 2=Bool.
// Mirrors the interpreter's `builtin_flag_parse`.

#[derive(Debug, Clone)]
struct GosFlagMapEntry {
    name: String,
    short: Option<char>,
    kind: FlagKind,
    str_val: Option<Vec<u8>>,
    int_val: i64,
}

pub struct GosFlagMap {
    entries: Vec<GosFlagMapEntry>,
    positional: Vec<String>,
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_parse(decls: *mut GosVec) -> *mut GosFlagMap {
    ffi_entry!(std::ptr::null_mut(), {
        let mut entries: Vec<GosFlagMapEntry> = Vec::new();
        if !decls.is_null() {
            let len = unsafe { gos_rt_vec_len(decls) };
            for i in 0..len {
                let raw = unsafe { gos_rt_vec_get_i64(decls, i) };
                if raw == 0 {
                    continue;
                }
                let blob = raw as *const i64;
                let name_cs = unsafe { *blob.add(0) } as *const c_char;
                let short_raw = unsafe { *blob.add(1) };
                let kind_tag = unsafe { *blob.add(2) };
                let int_val = unsafe { *blob.add(3) };
                let str_cs = unsafe { *blob.add(4) } as *const c_char;
                let name = if name_cs.is_null() {
                    String::new()
                } else {
                    unsafe { CStr::from_ptr(name_cs).to_string_lossy().into_owned() }
                };
                let short = u32::try_from(short_raw).ok().and_then(char::from_u32);
                let kind = match kind_tag {
                    0 => FlagKind::Int,
                    1 => FlagKind::String,
                    2 => FlagKind::Bool,
                    _ => FlagKind::String,
                };
                let str_val = if matches!(kind, FlagKind::String) && !str_cs.is_null() {
                    Some(unsafe { CStr::from_ptr(str_cs).to_bytes().to_vec() })
                } else {
                    None
                };
                entries.push(GosFlagMapEntry {
                    name,
                    short,
                    kind,
                    str_val,
                    int_val,
                });
            }
        }
        let positional = parse_argv_flag_values(
            &mut entries,
            ARGS_PTR.load(Ordering::SeqCst),
            ARGS_LEN.load(Ordering::SeqCst),
        );
        Box::into_raw(Box::new(GosFlagMap {
            entries,
            positional,
        }))
    })
}

/// Parse `argv`/`argc` into positional strings, applying flag values
/// to `entries` in place.
fn parse_argv_flag_values(entries: &mut [GosFlagMapEntry], argv: usize, argc: i64) -> Vec<String> {
    let argv = argv as *const *const c_char;
    let mut idx: i64 = 0;
    let mut positional: Vec<String> = Vec::new();
    while idx < argc {
        let p = unsafe { *argv.offset(idx as isize) };
        if p.is_null() {
            idx += 1;
            continue;
        }
        let arg = unsafe { CStr::from_ptr(p).to_string_lossy().into_owned() };
        if arg == "--" {
            idx += 1;
            while idx < argc {
                let q = unsafe { *argv.offset(idx as isize) };
                if !q.is_null() {
                    let s = unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() };
                    positional.push(s);
                }
                idx += 1;
            }
            break;
        }
        if let Some(rest) = arg.strip_prefix("--") {
            let (name, explicit) = match rest.split_once('=') {
                Some((n, v)) => (n.to_string(), Some(v.to_string())),
                None => (rest.to_string(), None),
            };
            if let Some(entry) = entries.iter_mut().find(|e| e.name == name) {
                let value = if let Some(v) = explicit {
                    v
                } else if matches!(entry.kind, FlagKind::Bool) {
                    "true".to_string()
                } else if idx + 1 < argc {
                    idx += 1;
                    let q = unsafe { *argv.offset(idx as isize) };
                    if q.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() }
                    }
                } else {
                    String::new()
                };
                apply_decl_value(entry, &value);
                idx += 1;
                continue;
            }
            positional.push(arg);
            idx += 1;
            continue;
        }
        if let Some(rest) = arg.strip_prefix('-')
            && !rest.is_empty()
        {
            let mut chars = rest.chars();
            let first = chars.next().unwrap();
            let remainder: String = chars.collect();
            if let Some(entry) = entries.iter_mut().find(|e| e.short == Some(first)) {
                let value = if !remainder.is_empty() {
                    remainder
                } else if matches!(entry.kind, FlagKind::Bool) {
                    "true".to_string()
                } else if idx + 1 < argc {
                    idx += 1;
                    let q = unsafe { *argv.offset(idx as isize) };
                    if q.is_null() {
                        String::new()
                    } else {
                        unsafe { CStr::from_ptr(q).to_string_lossy().into_owned() }
                    }
                } else {
                    String::new()
                };
                apply_decl_value(entry, &value);
                idx += 1;
                continue;
            }
        }
        positional.push(arg);
        idx += 1;
    }
    positional
}

fn apply_decl_value(entry: &mut GosFlagMapEntry, raw: &str) {
    match entry.kind {
        FlagKind::Int | FlagKind::Uint | FlagKind::Duration => {
            entry.int_val = raw.parse::<i64>().unwrap_or(entry.int_val);
        }
        FlagKind::Float => {
            entry.int_val = raw.parse::<f64>().unwrap_or(0.0).to_bits() as i64;
        }
        FlagKind::Bool => {
            entry.int_val = i64::from(matches!(raw, "true" | "1" | "yes" | "on"));
        }
        FlagKind::String | FlagKind::StringList => {
            entry.str_val = Some(raw.as_bytes().to_vec());
        }
    }
}

/// `FlagMap::get(map, key) -> Option<i64-or-string>`. Returns
/// `Result<int_or_str_ptr, ()>` (Result-as-Option in the
/// compiled tier) carrying either the i64 slot for numeric /
/// bool flags or the c-string pointer for string flags.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_map_get(map: *const GosFlagMap, key: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if map.is_null() || key.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let m = unsafe { &*map };
        let k = unsafe { CStr::from_ptr(key).to_string_lossy().into_owned() };
        if let Some(entry) = m.entries.iter().find(|e| e.name == k) {
            let payload = match entry.kind {
                FlagKind::String | FlagKind::StringList => {
                    let bytes = entry.str_val.as_deref().unwrap_or(&[]);
                    alloc_cstring(bytes) as i64
                }
                _ => entry.int_val,
            };
            return unsafe { gos_rt_result_new(0, payload) };
        }
        // Suppress unused-field warning on positional (kept for
        // future surface — `flag::parse(...)?.positional`).
        let _ = &m.positional;
        unsafe { gos_rt_result_new(1, 0) }
    })
}

/// `time::format_rfc3339(unix_ms) -> Result<String, errors::Error>`.
/// Renders a UTC RFC 3339 timestamp from a unix-milliseconds
/// instant. Mirrors the interpreter builtin.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_format_rfc3339(unix_ms: i64) -> i128 {
    ffi_entry!(0i128, {
        let secs = unix_ms.div_euclid(1_000);
        let nanos = (unix_ms.rem_euclid(1_000) * 1_000_000) as u32;
        let _ = nanos;
        let mut y: i64 = 1970;
        let mut remain = secs.div_euclid(86_400);
        let is_leap = |yr: i64| (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0;
        let dy = |yr: i64| if is_leap(yr) { 366 } else { 365 };
        if remain < 0 {
            while remain < 0 {
                y -= 1;
                remain += dy(y);
            }
        } else {
            while remain >= dy(y) {
                remain -= dy(y);
                y += 1;
            }
        }
        let dim = |m: i64, yr: i64| -> i64 {
            match m {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => {
                    if is_leap(yr) {
                        29
                    } else {
                        28
                    }
                }
                _ => 30,
            }
        };
        let mut m = 1_i64;
        while remain >= dim(m, y) {
            remain -= dim(m, y);
            m += 1;
        }
        let day = remain + 1;
        let s = secs.rem_euclid(86_400);
        let h = s / 3600;
        let mi = (s % 3600) / 60;
        let se = s % 60;
        let s_str = format!("{y:04}-{m:02}-{day:02}T{h:02}:{mi:02}:{se:02}Z");
        let cs = alloc_cstring(s_str.as_bytes());
        unsafe { gos_rt_result_new(0, cs as i64) }
    })
}

/// `time::parse_rfc3339(s) -> Result<i64, errors::Error>`.
/// Parses a UTC RFC 3339 timestamp and returns unix milliseconds.
/// Accepts the `YYYY-MM-DDTHH:MM:SSZ` form produced by format_rfc3339.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_parse_rfc3339(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let text = if s.is_null() {
            return unsafe {
                let msg = alloc_cstring(b"parse_rfc3339: null input");
                gos_rt_result_new(1, msg as i64)
            };
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim()
        };
        // Minimal RFC 3339 / ISO 8601 parser: YYYY-MM-DDTHH:MM:SS[.frac]Z
        let err = |msg: &str| -> i128 {
            let cs = alloc_cstring(msg.as_bytes());
            unsafe { gos_rt_result_new(1, cs as i64) }
        };
        if text.len() < 19 {
            return err("parse_rfc3339: input too short");
        }
        let parse_u32 = |s: &str| -> Option<u32> { s.parse::<u32>().ok() };
        let year = parse_u32(&text[0..4]).unwrap_or(0) as i64;
        let month = parse_u32(&text[5..7]).unwrap_or(0) as i64;
        let day = parse_u32(&text[8..10]).unwrap_or(0) as i64;
        let hour = parse_u32(&text[11..13]).unwrap_or(0) as i64;
        let min = parse_u32(&text[14..16]).unwrap_or(0) as i64;
        let sec = parse_u32(&text[17..19]).unwrap_or(0) as i64;
        if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
            return err("parse_rfc3339: invalid date fields");
        }
        let is_leap = |yr: i64| (yr % 4 == 0 && yr % 100 != 0) || yr % 400 == 0;
        let dim = |m: i64, yr: i64| -> i64 {
            match m {
                1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                4 | 6 | 9 | 11 => 30,
                2 => {
                    if is_leap(yr) {
                        29
                    } else {
                        28
                    }
                }
                _ => 30,
            }
        };
        // Days since Unix epoch (1970-01-01).
        let mut days: i64 = 0;
        let mut y = 1970_i64;
        while y < year {
            days += if is_leap(y) { 366 } else { 365 };
            y += 1;
        }
        while y > year {
            y -= 1;
            days -= if is_leap(y) { 366 } else { 365 };
        }
        for mo in 1..month {
            days += dim(mo, year);
        }
        days += day - 1;
        let unix_secs = days * 86_400 + hour * 3_600 + min * 60 + sec;
        let unix_ms = unix_secs * 1_000;
        unsafe { gos_rt_result_new(0, unix_ms) }
    })
}

/// `time::Duration::from_millis(n)` lowering — Duration is already
/// stored as i64 ms in the compiled tier, so this is the identity.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_millis(ms: i64) -> i64 {
    ffi_entry!(-1, { ms })
}

/// `*cell` for `flag::Set::float` cells.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_f64(cell: *const f64) -> f64 {
    ffi_entry!(f64::NAN, {
        if cell.is_null() {
            return 0.0;
        }
        unsafe { *cell }
    })
}

/// `*cell` for `flag::Set::string_list` cells. The cell stores a
/// `*mut GosVec` that the runtime owns; reads return a borrow.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_flag_cell_load_vec(cell: *const *mut GosVec) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if cell.is_null() {
            return std::ptr::null_mut();
        }
        unsafe { *cell }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_parse_f64(s: *const c_char, ok_out: *mut i32) -> f64 {
    ffi_entry!(f64::NAN, {
        if s.is_null() {
            if !ok_out.is_null() {
                unsafe { *ok_out = 0 };
            }
            return 0.0;
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }.trim();
        if let Ok(x) = text.parse::<f64>() {
            if !ok_out.is_null() {
                unsafe { *ok_out = 1 };
            }
            x
        } else {
            if !ok_out.is_null() {
                unsafe { *ok_out = 0 };
            }
            0.0
        }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_i64_to_str(n: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(n.to_string().as_bytes())
    })
}

/// Stringifies an *unsigned* 64-bit integer. Distinct from
/// `gos_rt_i64_to_str` so values `>= 2^63` print as their true
/// magnitude rather than a leading-`-` two's-complement view.
/// Used by the cranelift + LLVM lowerers when the source TyKind
/// resolves to `u8/u16/u32/u64/u128/usize`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_u64_to_str(n: u64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(n.to_string().as_bytes())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_to_str(x: f64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(format!("{x}").as_bytes())
    })
}

/// Stringifies an `f64` with `prec` fractional digits — the runtime
/// side of `format!("{:.N}", x)`. Routes through the Rust standard
/// library's float formatter so rounding matches the interpreter's
/// `{:.N}` Display output bit-for-bit. Negative `prec` is clamped to
/// zero; very large `prec` is clamped to a sane upper bound to keep
/// the allocation bounded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_prec_to_str(x: f64, prec: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let prec = prec.clamp(0, 64) as usize;
        alloc_cstring(format!("{x:.prec$}").as_bytes())
    })
}

/// Stringifies a bool (passed as i32: nonzero = true). Used by
/// codegen to assemble multi-arg panic / format-style messages.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_bool_to_str(b: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        alloc_cstring(if b == 0 { b"false" } else { b"true" })
    })
}

/// Stringifies a char (passed as i32 Unicode scalar) into a freshly
/// heap-allocated UTF-8 c-string. Invalid scalars (surrogates,
/// > U+10FFFF) render as `\u{FFFD}` (REPLACEMENT CHARACTER).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_char_to_str(c: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let scalar = u32::try_from(c)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or('\u{FFFD}');
        let mut buf = [0u8; 4];
        let s = scalar.encode_utf8(&mut buf);
        alloc_cstring(s.as_bytes())
    })
}

// ---------------------------------------------------------------
// strings::* free-function surface (0.10.0 cross-tier wiring)
// ---------------------------------------------------------------
// These back the `strings::*` free functions that previously only
// existed in the bytecode VM. Each mirrors the corresponding
// `gossamer_std::strings` helper so `gos run` and `gos build`
// produce identical output.

unsafe fn cstr<'a>(p: *const c_char) -> &'a str {
    if p.is_null() {
        ""
    } else {
        unsafe { CStr::from_ptr(p).to_str().unwrap_or("") }
    }
}

/// Builds a `*mut GosVec` of c-string pointers from owned strings.
fn alloc_str_vec(parts: &[String]) -> *mut GosVec {
    let vec = unsafe { gos_rt_vec_with_capacity(8, parts.len() as i64) };
    for p in parts {
        let pv = alloc_cstring(p.as_bytes()) as i64;
        unsafe { gos_rt_vec_push(vec, std::ptr::addr_of!(pv).cast::<u8>()) };
    }
    vec
}

/// `strings::splitn(s, n, sep) -> [String]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_splitn(
    s: *const c_char,
    n: i64,
    sep: *const c_char,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let n = usize::try_from(n.max(0)).unwrap_or(0);
        let parts: Vec<String> = unsafe { cstr(s) }
            .splitn(n, unsafe { cstr(sep) })
            .map(str::to_string)
            .collect();
        alloc_str_vec(&parts)
    })
}

/// `strings::split_whitespace(s) -> [String]`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_split_whitespace(s: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let parts: Vec<String> = unsafe { cstr(s) }
            .split_whitespace()
            .map(str::to_string)
            .collect();
        alloc_str_vec(&parts)
    })
}

/// `strings::fields(s) -> [String]`. Same semantics as
/// `split_whitespace` (Go's `strings.Fields`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_fields(s: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let parts: Vec<String> = unsafe { cstr(s) }
            .split_whitespace()
            .map(str::to_string)
            .collect();
        alloc_str_vec(&parts)
    })
}

/// `strings::replacen(s, from, to, n) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_replacen(
    s: *const c_char,
    from: *const c_char,
    to: *const c_char,
    n: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let n = usize::try_from(n.max(0)).unwrap_or(0);
        let out = unsafe { cstr(s) }.replacen(unsafe { cstr(from) }, unsafe { cstr(to) }, n);
        alloc_cstring(out.as_bytes())
    })
}

/// `strings::to_title(s) -> String` — capitalises the first
/// character of each whitespace-separated word.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_title(s: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = unsafe { cstr(s) };
        let mut result = String::with_capacity(text.len());
        let mut capitalize_next = true;
        for c in text.chars() {
            if c.is_whitespace() {
                capitalize_next = true;
                result.push(c);
            } else if capitalize_next {
                result.extend(c.to_uppercase());
                capitalize_next = false;
            } else {
                result.push(c);
            }
        }
        alloc_cstring(result.as_bytes())
    })
}

/// `strings::trim_matches(s, cutset) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_trim_matches(
    s: *const c_char,
    cutset: *const c_char,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let cutset = unsafe { cstr(cutset) };
        let out = unsafe { cstr(s) }.trim_matches(|c| cutset.contains(c));
        alloc_cstring(out.as_bytes())
    })
}

/// `strings::pad_left(s, width, pad_char) -> String`. `pad_char` is
/// the Unicode scalar value; invalid scalars fall back to a space.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_pad_left(
    s: *const c_char,
    width: i64,
    pad_char: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = unsafe { cstr(s) };
        let width = usize::try_from(width.max(0)).unwrap_or(0);
        let pc = u32::try_from(pad_char)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or(' ');
        let count = text.chars().count();
        let out = if count >= width {
            text.to_string()
        } else {
            let mut out = String::new();
            for _ in 0..(width - count) {
                out.push(pc);
            }
            out.push_str(text);
            out
        };
        alloc_cstring(out.as_bytes())
    })
}

/// `strings::pad_right(s, width, pad_char) -> String`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_pad_right(
    s: *const c_char,
    width: i64,
    pad_char: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = unsafe { cstr(s) };
        let width = usize::try_from(width.max(0)).unwrap_or(0);
        let pc = u32::try_from(pad_char)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or(' ');
        let count = text.chars().count();
        let out = if count >= width {
            text.to_string()
        } else {
            let mut out = String::with_capacity(text.len() + width - count);
            out.push_str(text);
            for _ in 0..(width - count) {
                out.push(pc);
            }
            out
        };
        alloc_cstring(out.as_bytes())
    })
}

/// `strings::contains_rune(s, r) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_contains_rune(s: *const c_char, r: i64) -> i32 {
    ffi_entry!(-1, {
        let Some(rc) = u32::try_from(r).ok().and_then(char::from_u32) else {
            return 0;
        };
        i32::from(unsafe { cstr(s) }.contains(rc))
    })
}

/// `strings::contains_any(s, chars) -> bool`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_contains_any(s: *const c_char, chars: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let chars = unsafe { cstr(chars) };
        i32::from(unsafe { cstr(s) }.chars().any(|c| chars.contains(c)))
    })
}

/// `strings::equal_fold(a, b) -> bool` — case-insensitive compare.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_equal_fold(a: *const c_char, b: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let a = unsafe { cstr(a) };
        let b = unsafe { cstr(b) };
        if a.len() != b.len() && a.chars().count() != b.chars().count() {
            return 0;
        }
        let eq = a
            .chars()
            .zip(b.chars())
            .all(|(ac, bc)| ac.to_lowercase().eq(bc.to_lowercase()));
        i32::from(eq)
    })
}

/// `strings::index_rune(s, r) -> Option<i64>` byte index, packed as
/// a `*mut GosResult` (`disc 0 = Some(idx)`, `disc 1 = None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_index_rune(s: *const c_char, r: i64) -> i128 {
    ffi_entry!(0i128, {
        let rc = u32::try_from(r).ok().and_then(char::from_u32);
        match rc.and_then(|rc| unsafe { cstr(s) }.find(rc)) {
            Some(i) => unsafe { gos_rt_result_new(0, i as i64) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `strings::index_any(s, chars) -> Option<i64>` byte index of the
/// first character that appears in `chars`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_index_any(s: *const c_char, chars: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let chars = unsafe { cstr(chars) };
        match unsafe { cstr(s) }
            .char_indices()
            .find(|(_, c)| chars.contains(*c))
            .map(|(i, _)| i)
        {
            Some(i) => unsafe { gos_rt_result_new(0, i as i64) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `strings::last_index_any(s, chars) -> Option<i64>` byte index of
/// the last character that appears in `chars`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_last_index_any(s: *const c_char, chars: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let chars = unsafe { cstr(chars) };
        match unsafe { cstr(s) }
            .char_indices()
            .rev()
            .find(|(_, c)| chars.contains(*c))
            .map(|(i, _)| i)
        {
            Some(i) => unsafe { gos_rt_result_new(0, i as i64) },
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `strings::strip_prefix(s, prefix) -> Option<String>` packed as a
/// `*mut GosResult` (`disc 0 = Some(string-ptr)`, `disc 1 = None`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_strip_prefix(s: *const c_char, prefix: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        match unsafe { cstr(s) }.strip_prefix(unsafe { cstr(prefix) }) {
            Some(stripped) => {
                let p = alloc_cstring(stripped.as_bytes()) as i64;
                unsafe { gos_rt_result_new(0, p) }
            }
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `strings::strip_suffix(s, suffix) -> Option<String>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_strip_suffix(s: *const c_char, suffix: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        match unsafe { cstr(s) }.strip_suffix(unsafe { cstr(suffix) }) {
            Some(stripped) => {
                let p = alloc_cstring(stripped.as_bytes()) as i64;
                unsafe { gos_rt_result_new(0, p) }
            }
            None => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}
