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

use std::alloc::{Layout, alloc, dealloc, handle_alloc_error};
use std::ffi::CStr;
use std::os::raw::c_char;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

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
// helpers (future). The ABI pointer remains the first content byte, but every
// runtime string now has an explicit carrier immediately before its legacy
// header.  The carrier is selected by the pointer's fixed low-bit shape before
// any backwards read, so public C-ABI helpers never probe a foreign C string.

/// ABI-versioned ownership carrier preceding every Gossamer string header.
/// The legacy `[rc, cap, len, tag]` suffix stays immediately before the C
/// string body, preserving all native-code offsets.
#[repr(C)]
struct StringOwner {
    abi_version: u16,
    kind: u16,
    destructor: u32,
    generation: u64,
}

const STRING_OWNER_VERSION: u16 = 1;
const STRING_OWNER_KIND: u16 = 2;
const STRING_DTOR_HEAP: u32 = 1;
const STRING_DTOR_REGION: u32 = 2;
const STRING_DTOR_STATIC: u32 = 3;
const STRING_OWNER_BYTES: usize = std::mem::size_of::<StringOwner>();
const STRING_LEGACY_HEADER_BYTES: usize = 13;
const STRING_BODY_OFFSET: usize = STRING_OWNER_BYTES + STRING_LEGACY_HEADER_BYTES;
const STRING_BODY_TAG: usize = STRING_BODY_OFFSET & 7;
static NEXT_STRING_GENERATION: AtomicU64 = AtomicU64::new(1);

const _: () = assert!(STRING_OWNER_BYTES == 16);
const _: () = assert!(STRING_BODY_TAG == 5);

#[inline]
fn str_owner(s: *const c_char) -> Option<&'static StringOwner> {
    if s.is_null() || (s as usize & 7) != STRING_BODY_TAG {
        return None;
    }
    // The low-bit shape is assigned only by the three carrier layouts below;
    // it is checked before the backwards offset, so ordinary foreign C
    // strings (allocator-aligned bodies) stay borrowed and untouched.
    let owner = unsafe { &*s.cast::<u8>().sub(STRING_BODY_OFFSET).cast::<StringOwner>() };
    (owner.abi_version == STRING_OWNER_VERSION
        && owner.kind == STRING_OWNER_KIND
        && matches!(
            owner.destructor,
            STRING_DTOR_HEAP | STRING_DTOR_REGION | STRING_DTOR_STATIC
        ))
    .then_some(owner)
}

#[inline]
fn managed_string_owner(s: *const c_char) -> Option<&'static StringOwner> {
    str_owner(s).filter(|owner| owner.destructor == STRING_DTOR_HEAP)
}

pub(crate) unsafe fn c_str_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    unsafe { CStr::from_ptr(s).to_bytes().len() }
}

/// Borrows a runtime string's content bytes without a NUL scan when the
/// string carries a length header (builder / static / region layouts all
/// store `len:u32` at `ptr[-5]`); foreign / untagged pointers fall back to
/// `strlen`. Map key shims call this instead of `CStr::from_ptr(_).to_bytes()`
/// so hashing a k-mer key reads its length in O(1) rather than rescanning the
/// bytes the hash is about to read again.
///
/// SAFETY: `s` is null or points at a valid c-string (NUL-terminated when it
/// has no header). The returned slice borrows `s`; the caller keeps `s` alive
/// for the borrow.
#[inline]
pub(crate) unsafe fn gos_str_key_bytes<'a>(s: *const c_char) -> &'a [u8] {
    if s.is_null() {
        return &[];
    }
    let len = unsafe { str_header_len(s) }.unwrap_or_else(|| unsafe { c_str_len(s) });
    unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len) }
}

/// Returns the byte length of a compiler-typed Gossamer string without the
/// allocation-registry lookup used by raw C ABI entry points.
///
/// Generated code only passes values of the language's `String` type here, so
/// the byte immediately before the payload is always readable: heap builders,
/// static literals, and region strings carry a length header; foreign strings
/// are not part of this internal contract. Keeping this path separate from
/// [`str_header_len`] preserves the defensive registry check for APIs that can
/// genuinely receive arbitrary C pointers.
#[inline]
unsafe fn typed_str_len(s: *const c_char) -> usize {
    if s.is_null() {
        return 0;
    }
    let tag = unsafe { *s.cast::<u8>().sub(1) };
    if matches!(tag, STR_BUILDER_TAG | STR_STATIC_TAG | STR_REGION_TAG) {
        let p = unsafe { s.cast::<u8>().sub(5) };
        return u32::from_le_bytes(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] }) as usize;
    }
    unsafe { c_str_len(s) }
}

/// Borrows bytes from a compiler-typed Gossamer string. See
/// [`typed_str_len`] for why this does not consult the allocation registry.
#[inline]
unsafe fn typed_str_bytes<'a>(s: *const c_char) -> &'a [u8] {
    if s.is_null() {
        return &[];
    }
    let len = unsafe { typed_str_len(s) };
    unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len) }
}

/// Tests the private builder tag on a compiler-typed string.
#[inline]
unsafe fn is_typed_builder(s: *const c_char) -> bool {
    managed_string_owner(s).is_some() && unsafe { *s.cast::<u8>().sub(1) == STR_BUILDER_TAG }
}

/// Tag for growable strings allocated by `alloc_growable`.
/// Layout: `[cap:u32 LE][len:u32 LE][tag=0xAB][content(cap bytes)][NUL]`
/// `ptr` is 9 bytes past the start of the allocation (at `content[0]`).
/// `ptr[-1]` = tag, `ptr[-5..-1]` = len (u32 LE), `ptr[-9..-5]` = cap (u32 LE).
/// Total allocation: cap + 10 bytes.
const STR_BUILDER_TAG: u8 = 0xAB;

/// High bit of a `STR_BUILDER` string's `rc:u32` field, set once the string
/// has escaped to another goroutine (`gos_rt_rc_mark_shared`). When set,
/// `gos_rt_str_retain` / `gos_rt_str_free` adjust the count with atomic
/// read-modify-write instead of the non-atomic fast path, so concurrent
/// clone/drop across goroutines cannot tear the count (the same biased-RC
/// protocol `RcHeader` objects use via their `SHARED_BIT`). The live count is
/// the low 31 bits; a string never reaches 2^31 references, so the bit never
/// collides with a real count.
pub(crate) const STR_SHARED: u32 = 1 << 31;

/// Tag for static string literals emitted into compiler-owned rodata.
/// `is_gos_string` uses this only on values already known by typed runtime RC
/// metadata to be Gossamer values; public raw-string entry points never probe
/// this prefix.
const STR_STATIC_TAG: u8 = 0xA8;

/// Tag for growable strings whose backing bytes live in an arena region.
/// Same `[cap][len][tag][content][NUL]` layout as `STR_BUILDER_TAG` (so
/// length reads and in-place append work identically), but the bytes are
/// freed wholesale at `arena_pop`, so `gos_rt_str_free` skips them.
const STR_REGION_TAG: u8 = 0xAA;

#[inline]
fn is_managed_string(s: *const c_char) -> bool {
    managed_string_owner(s).is_some() && unsafe { *s.cast::<u8>().sub(1) == STR_BUILDER_TAG }
}

/// O(1) byte length for a compiler-typed Gossamer string. Returns `None` for a
/// foreign/untagged pointer, where callers fall back to `strlen`.
#[inline]
pub(crate) unsafe fn str_header_len(s: *const c_char) -> Option<usize> {
    str_owner(s)?;
    let tag = unsafe { *s.cast::<u8>().sub(1) };
    if !matches!(tag, STR_BUILDER_TAG | STR_STATIC_TAG | STR_REGION_TAG) {
        return None;
    }
    let p = unsafe { s.cast::<u8>().sub(5) };
    let len = u32::from_le_bytes(unsafe { [*p, *p.add(1), *p.add(2), *p.add(3)] });
    Some(len as usize)
}

/// Copies `n` non-overlapping bytes from `src` to `dst`, keeping short copies
/// inline with overlapping fixed-width loads/stores instead of calling the
/// platform `memcpy`. The static-musl release link resolves `memcpy` to musl's
/// scalar routine, whose per-call overhead dominates the short copies that k-mer
/// keys and small-string content produce (glibc hides this with a SIMD ifunc;
/// musl does not). Large copies fall through to `memcpy`, where throughput
/// dominates and the call is amortised. This mirrors how the Go runtime's
/// `memmove` and optimised libc `memcpy`s special-case small sizes.
///
/// SAFETY: `src` is readable and `dst` writable for `n` bytes, and the two
/// ranges do not overlap.
#[inline]
pub(crate) unsafe fn copy_small_bytes(src: *const u8, dst: *mut u8, n: usize) {
    unsafe {
        if n >= 32 {
            std::ptr::copy_nonoverlapping(src, dst, n);
        } else if n >= 16 {
            let a0 = (src as *const u64).read_unaligned();
            let a1 = (src.add(8) as *const u64).read_unaligned();
            let b0 = (src.add(n - 16) as *const u64).read_unaligned();
            let b1 = (src.add(n - 8) as *const u64).read_unaligned();
            (dst as *mut u64).write_unaligned(a0);
            (dst.add(8) as *mut u64).write_unaligned(a1);
            (dst.add(n - 16) as *mut u64).write_unaligned(b0);
            (dst.add(n - 8) as *mut u64).write_unaligned(b1);
        } else if n >= 8 {
            let a = (src as *const u64).read_unaligned();
            let b = (src.add(n - 8) as *const u64).read_unaligned();
            (dst as *mut u64).write_unaligned(a);
            (dst.add(n - 8) as *mut u64).write_unaligned(b);
        } else if n >= 4 {
            let a = (src as *const u32).read_unaligned();
            let b = (src.add(n - 4) as *const u32).read_unaligned();
            (dst as *mut u32).write_unaligned(a);
            (dst.add(n - 4) as *mut u32).write_unaligned(b);
        } else if n >= 2 {
            let a = (src as *const u16).read_unaligned();
            let b = (src.add(n - 2) as *const u16).read_unaligned();
            (dst as *mut u16).write_unaligned(a);
            (dst.add(n - 2) as *mut u16).write_unaligned(b);
        } else if n == 1 {
            *dst = *src;
        }
    }
}

/// Copies a string part into a newly allocated builder, tolerating allocator
/// address reuse that places the destination over stale source storage.
#[inline]
unsafe fn copy_builder_part(src: *const u8, dst: *mut u8, n: usize) {
    let src_addr = src as usize;
    let dst_addr = dst as usize;
    let overlaps =
        n != 0 && src_addr < dst_addr.saturating_add(n) && dst_addr < src_addr.saturating_add(n);
    if overlaps {
        // SAFETY: the caller provides readable/writable ranges of `n` bytes;
        // `copy` explicitly permits overlap (memmove semantics).
        unsafe { std::ptr::copy(src, dst, n) };
    } else {
        // SAFETY: the range check above proves the caller's valid ranges do
        // not overlap, satisfying `copy_small_bytes`' stronger contract.
        unsafe { copy_small_bytes(src, dst, n) };
    }
}

/// Allocates an owned `Box<[u8]>` holding `src`'s bytes via the inline
/// small-copy path, so short keys avoid a libc `memcpy` call (see
/// [`copy_small_bytes`]). Used by the string-keyed map insert paths, where a
/// k-mer key is copied into the map's own storage on a miss.
#[inline]
pub(crate) fn boxed_bytes(src: &[u8]) -> Box<[u8]> {
    let mut b: Box<[std::mem::MaybeUninit<u8>]> = Box::new_uninit_slice(src.len());
    // SAFETY: `b` has `src.len()` writable bytes, all written by the copy below;
    // `src` and the fresh `b` are distinct allocations, so they do not overlap.
    unsafe {
        copy_small_bytes(src.as_ptr(), b.as_mut_ptr().cast::<u8>(), src.len());
        b.assume_init()
    }
}

/// Allocates a growable string with `cap` bytes of content capacity.
/// `parts` are concatenated into the initial content (total must be <= cap).
/// Returns a pointer to `content[0]`; the 9-byte header lives just before it.
fn alloc_growable(parts: &[&[u8]], cap: usize) -> *mut c_char {
    alloc_growable_forced(parts, cap, false)
}

/// Allocates a growable string, promoting it to the heap when `force_heap` is
/// set or any non-empty input slice points into region storage.
fn alloc_growable_forced(parts: &[&[u8]], cap: usize, force_heap: bool) -> *mut c_char {
    let content_len: usize = parts.iter().map(|p| p.len()).sum();
    // A region-backed source can escape the compiler-generated arena scope
    // that created it. If the destination were allocated in the next active
    // region, slab recycling could place it over its own source bytes. Promote
    // copies of region storage to the heap before allocating the destination.
    let force_heap = force_heap
        || parts
            .iter()
            .any(|part| crate::c_abi::rc::in_region_arena(part.as_ptr()));
    alloc_growable_with_fill(content_len, cap, force_heap, |out| {
        let mut off = 0;
        for p in parts {
            // SAFETY: `alloc_growable_with_fill` passes `cap` writable content
            // bytes and `cap >= content_len`; this loop writes each input part
            // exactly once into the first `content_len` bytes.
            unsafe {
                copy_builder_part(p.as_ptr(), out.add(off), p.len());
            }
            off += p.len();
        }
    })
}

/// Allocates a growable runtime string and lets `fill` initialise exactly the
/// first `content_len` bytes of the content region.
fn alloc_growable_with_fill<F>(
    content_len: usize,
    cap: usize,
    force_heap: bool,
    fill: F,
) -> *mut c_char
where
    F: FnOnce(*mut u8),
{
    debug_assert!(
        cap >= content_len,
        "alloc_growable_with_fill: cap < content length"
    );
    // The builder header stores length and capacity as `u32` (offsets
    // `ptr[-5]` / `ptr[-9]`). A value past `u32::MAX` cannot be represented,
    // so refuse it here rather than truncate and later index the buffer with a
    // wrapped length. A single string this large is not a real workload; treat
    // it like the allocation failure it effectively is (aborting, matching the
    // OOM discipline of the `Box::new_uninit_slice` path below) instead of
    // corrupting the heap. This is on the string-append hot path, so the check
    // is two comparisons and no allocation.
    if cap > u32::MAX as usize || content_len > u32::MAX as usize {
        eprintln!(
            "gossamer: string length {content_len} exceeds the 4 GiB builder-header limit; aborting"
        );
        std::process::abort();
    }
    // owner(16) + rc(4) + cap(4) + len(4) + tag(1) + content(cap) + NUL(1).
    // Refcount at the FRONT keeps cap(-9)/len(-5)/tag(-1) offsets unchanged.
    let total = STRING_BODY_OFFSET + cap + 1;
    crate::c_abi::ledger::benchmark_allocation(total);
    // Inside an arena region, allocate fresh builders from the region. A copy
    // whose source is already region-backed must be promoted to the heap so a
    // recycled slab cannot place the destination over its own source bytes.
    let region_base = if force_heap {
        std::ptr::null_mut()
    } else {
        crate::c_abi::rc::region_alloc_bytes(total)
    };
    let (base, tag, zero_tail) = if region_base.is_null() {
        let layout = Layout::from_size_align(total, 8).expect("string layout is valid");
        // SAFETY: `layout` has non-zero size and a power-of-two alignment. The
        // matching `dealloc` below reconstructs the exact same layout.
        let base = unsafe { alloc(layout) };
        if base.is_null() {
            handle_alloc_error(layout);
        }
        (base, STR_BUILDER_TAG, true)
    } else {
        (region_base, STR_REGION_TAG, false)
    };
    // SAFETY: `base` points to `total` writable bytes. Header fields and the
    // trailing zero region are initialised here; `fill` initialises the content
    // prefix promised by its caller.
    unsafe {
        let owner = base.cast::<StringOwner>();
        owner.write(StringOwner {
            abi_version: STRING_OWNER_VERSION,
            kind: STRING_OWNER_KIND,
            destructor: if tag == STR_REGION_TAG {
                STRING_DTOR_REGION
            } else {
                STRING_DTOR_HEAP
            },
            generation: NEXT_STRING_GENERATION.fetch_add(1, Ordering::Relaxed),
        });
        let hdr = base.add(STRING_OWNER_BYTES);
        std::ptr::copy_nonoverlapping(1u32.to_le_bytes().as_ptr(), hdr, 4);
        std::ptr::copy_nonoverlapping((cap as u32).to_le_bytes().as_ptr(), hdr.add(4), 4);
        std::ptr::copy_nonoverlapping((content_len as u32).to_le_bytes().as_ptr(), hdr.add(8), 4);
        *hdr.add(12) = tag;
        let content = base.add(STRING_BODY_OFFSET);
        fill(content);
        if zero_tail {
            // Region allocations arrive zeroed. Heap allocations need their
            // spare capacity and trailing NUL initialized explicitly.
            std::ptr::write_bytes(content.add(content_len), 0, cap - content_len + 1);
        }
        if tag != STR_REGION_TAG {
            crate::c_abi::ledger::str_inc();
        }
        content.cast::<c_char>()
    }
}

/// Reclaims a live heap c-string previously returned by [`alloc_cstring`].
/// The cleanup pass emits a call to this helper at every
/// body return for a non-escaping String produced by a known
/// String allocator (e.g. `gos_rt_stream_read_to_string`); the
/// escape analyser's non-capturing-callee whitelist ensures only
/// owning bindings reach this path so the drop never observes an
/// aliased pointer.
///
/// SAFETY: caller guarantees that `s` remains a valid C string for this call
/// and that it owns one live runtime reference. Foreign, static, and
/// region-backed strings are ignored without probing a private prefix. As with
/// every raw-pointer ABI, a stale pointer whose address has been reused cannot
/// be distinguished without a generation-bearing carrier type.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_free(s: *mut c_char) {
    ffi_entry!((), {
        if s.is_null() {
            return;
        }
        if !is_managed_string(s) {
            return;
        }
        crate::c_abi::ledger::benchmark_arc_release();
        // Refcounted carrier: [owner][rc:u32][cap:u32][len:u32][tag][content][NUL].
        // Carrier validation above establishes that the legacy suffix belongs
        // to a live runtime allocation.
        let hdr = unsafe { s.cast::<u8>().sub(13) };
        let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
        if rc & STR_SHARED != 0 {
            let cell = unsafe { AtomicU32::from_ptr(hdr.cast::<u32>()) };
            let prev = cell.fetch_sub(1, Ordering::Release);
            if prev & !STR_SHARED != 1 {
                return;
            }
            std::sync::atomic::fence(Ordering::Acquire);
        } else if rc > 1 {
            unsafe {
                std::ptr::copy_nonoverlapping((rc - 1).to_le_bytes().as_ptr(), hdr, 4);
            }
            return;
        }
        let cap =
            u32::from_le_bytes(unsafe { [*hdr.add(4), *hdr.add(5), *hdr.add(6), *hdr.add(7)] })
                as usize;
        let total = STRING_BODY_OFFSET + cap + 1;
        let layout = Layout::from_size_align(total, 8).expect("string layout is valid");
        // SAFETY: builder allocation uses this exact layout, and this is the
        // last strong reference after the count logic above. The carrier owns
        // the allocation base; `hdr` is only its legacy suffix.
        unsafe { dealloc(s.cast::<u8>().sub(STRING_BODY_OFFSET), layout) };
        crate::c_abi::ledger::str_dec();
    });
}

/// True when `s` is a string value inside a compiler-typed Gossamer object.
///
/// SAFETY: unlike public raw C-string entry points, this internal RC dispatch
/// helper may only receive a pointer whose surrounding typed metadata already
/// establishes it as a valid Gossamer value. Static and region strings have no
/// registry entry, so their compiler-owned tag is read here to route cleanup
/// away from the RC header path. Do not use this to validate a foreign pointer.
#[inline]
pub unsafe fn is_gos_string(s: *const c_char) -> bool {
    str_owner(s).is_some()
}

/// Increment a heap (`STR_BUILDER_TAG`) string's refcount; no-op otherwise.
pub(crate) unsafe fn gos_rt_str_retain(s: *const c_char) {
    if !is_managed_string(s) {
        return;
    }
    crate::c_abi::ledger::benchmark_arc_retain();
    let hdr = unsafe { s.cast::<u8>().sub(13) };
    let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
    if rc & STR_SHARED != 0 {
        // Goroutine-shared: atomic increment of the low-31-bit count. `hdr` is
        // the allocation base (allocator-aligned >= 4), so the cast is sound;
        // the count cannot reach the shared bit, so `fetch_add` preserves it.
        let cell = unsafe { AtomicU32::from_ptr(hdr.cast_mut().cast::<u32>()) };
        cell.fetch_add(1, Ordering::Relaxed);
        return;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            rc.saturating_add(1).to_le_bytes().as_ptr(),
            hdr.cast_mut(),
            4,
        );
    }
}

/// Marks a `STR_BUILDER` string as goroutine-shared so subsequent
/// retain/release use atomic counting. No-op for other string kinds (static /
/// region / fixed `STR_ALLOC` strings carry no refcount). Called from
/// `gos_rt_rc_mark_shared` when a string escapes to another goroutine.
pub(crate) unsafe fn gos_rt_str_mark_shared(s: *const c_char) {
    if !is_managed_string(s) {
        return;
    }
    let hdr = unsafe { s.cast::<u8>().sub(13) };
    let cell = unsafe { AtomicU32::from_ptr(hdr.cast_mut().cast::<u32>()) };
    cell.fetch_or(STR_SHARED, Ordering::Relaxed);
}

/// Allocate an owned, NUL-terminated heap string holding `s`'s bytes (the
/// growable runtime-string allocator shape).
pub fn alloc_cstring(s: &[u8]) -> *mut c_char {
    alloc_cstring_from_slices(&[s])
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
/// normal c-string. Runtime ownership is recorded when the allocation is
/// created, so release never needs to inspect memory before an arbitrary raw
/// pointer.
pub fn alloc_cstring_from_slices(parts: &[&[u8]]) -> *mut c_char {
    // Use the length-carrying builder layout (cap = content length) so the
    // result has its byte length stored at `ptr[-5]` for O(1)
    // `gos_rt_str_len` / `gos_rt_str_slice`. A later in-place `+=` finds no
    // spare capacity and reallocates with doubling - correctness and the
    // amortised growth analysis are unchanged. `gos_rt_str_free` and the
    // concat fast path already handle `STR_BUILDER_TAG`.
    let total: usize = parts.iter().map(|p| p.len()).sum();
    alloc_growable(parts, total)
}

/// Allocate one runtime string and fill it with ASCII-uppercase bytes from
/// `src`. The caller has already proven `src.is_ascii()`, so Unicode
/// expansion never applies and the output length equals the input length.
fn alloc_ascii_upper_cstring(src: &[u8]) -> *mut c_char {
    let len = src.len();
    let force_heap = crate::c_abi::rc::in_region_arena(src.as_ptr());
    alloc_growable_with_fill(len, len, force_heap, |out| {
        for (i, &b) in src.iter().enumerate() {
            let upper = if b.is_ascii_lowercase() {
                b - (b'a' - b'A')
            } else {
                b
            };
            // SAFETY: `alloc_growable_with_fill` passes `len` writable content
            // bytes and this loop writes each byte exactly once.
            unsafe {
                *out.add(i) = upper;
            }
        }
    })
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

/// `s.clear()` for compiled String method lowering. Strings are immutable at
/// the ABI boundary, so this returns a fresh empty string for caller writeback.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_str_clear() -> *mut c_char {
    alloc_cstring(b"")
}

/// Allocates an empty owned string with at least `capacity` writable bytes.
/// This is the compiled implementation of `String::with_capacity`; subsequent
/// unique `push_str` calls reuse the allocation until the reserved space is
/// exhausted.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_str_with_capacity(capacity: i64) -> *mut c_char {
    if capacity < 0 {
        unsafe { gos_rt_panic(c"String::with_capacity: capacity must be non-negative".as_ptr()) };
    }
    let capacity = usize::try_from(capacity).unwrap_or(u32::MAX as usize);
    alloc_growable(&[], capacity.min(u32::MAX as usize))
}

/// `s.truncate(n)` for compiled String method lowering. The public method takes
/// a byte length; if `n` lands inside a UTF-8 scalar, truncate to the preceding
/// valid boundary so the returned Gossamer String remains well-formed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_truncate(s: *const c_char, n: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if s.is_null() || n <= 0 {
            return alloc_cstring(b"");
        }
        let len = unsafe { str_header_len(s) }.unwrap_or_else(|| unsafe { c_str_len(s) });
        let cap = (n as usize).min(len);
        let bytes = unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len) };
        let end = match std::str::from_utf8(bytes) {
            Ok(text) => text
                .char_indices()
                .map(|(idx, _)| idx)
                .chain(std::iter::once(text.len()))
                .take_while(|idx| *idx <= cap)
                .last()
                .unwrap_or(0),
            Err(_) => cap,
        };
        alloc_cstring_from_slices(&[&bytes[..end]])
    })
}

/// `String::from_utf8(bytes) -> Result<String, errors::Error>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_string_from_utf8(bytes: *const GosVec) -> i128 {
    ffi_entry!(0i128, {
        if bytes.is_null() {
            return unsafe { gos_rt_result_new(0, alloc_cstring(b"") as i64) };
        }
        let vec = unsafe { &*bytes };
        let mut out = Vec::with_capacity(vec.len.max(0) as usize);
        for idx in 0..vec.len.max(0) {
            let b = unsafe { crate::c_abi::vec::vec_elem_load_i64(vec, idx) };
            out.push(b as u8);
        }
        match std::str::from_utf8(&out) {
            Ok(_) => unsafe { gos_rt_result_new(0, alloc_cstring_from_slices(&[&out]) as i64) },
            Err(e) => {
                let msg = format!("String::from_utf8: {e}");
                let cs = std::ffi::CString::new(msg).unwrap_or_default();
                let err = unsafe { gos_rt_error_new(cs.as_ptr()) };
                unsafe { gos_rt_result_new(1, err as i64) }
            }
        }
    })
}

/// Generic length-zero check used by `is_empty` for any
/// receiver whose length is reachable through `gos_rt_len`
/// (Vec / array / slice / hashmap …).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_len_is_zero(p: *const i64) -> bool {
    ffi_entry!(false, { unsafe { gos_rt_len(p) == 0 } })
}

/// Clones a `*mut GosVec` element-by-element. Used by
/// `xs.to_vec()` so the result is independent of the source -
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
        // Header + element buffer in one `Box<InlineVec>` (inline for a
        // small vec, else a separate buffer), then copy the source slots
        // into whichever data region `ptr` lands at. Ledger + strong count
        // are set by `alloc_box_vec`, symmetric with `gos_rt_vec_free`.
        let out =
            unsafe { crate::c_abi::vec::alloc_box_vec(s.elem_bytes, s.elem_kind, s.len, s.len) };
        let data = unsafe { (*out).ptr.as_ptr() };
        if bytes > 0 && !s.ptr.is_null() && !data.is_null() {
            unsafe { std::ptr::copy_nonoverlapping(s.ptr.as_ptr(), data, bytes) };
        }
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
/// identity lowering returned the raw c-string ptr - `.len()`
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
        // dispatch (`gos_rt_vec_get_i64`) - every slot is i64-shaped.
        // Materialise each byte as a zero-extended i64 so the load
        // returns the byte's value rather than 8 packed buffer
        // bytes. Use `gos_rt_vec_with_capacity` so the resulting
        // header is `Box::from_raw`-compatible - the auto-emitted
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

/// `s.chars()` - materialises the string's Unicode scalar values as a
/// fresh `*mut GosVec` of i64 codepoints (one 8-byte slot per char), so
/// `for ch in s.chars()` reads each scalar via `gos_rt_vec_get_i64` and
/// binds a `char`. Mirrors the interp builtin so `gos run` and
/// `gos build` agree. The backing buffer + header are
/// `Box::from_raw`-compatible (via `gos_rt_vec_with_capacity`) so the
/// auto-emitted `gos_rt_vec_free` at scope-end reclaims them.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_chars(s: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let st = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let codepoints: Vec<i64> = st.chars().map(|c| i64::from(u32::from(c))).collect();
        let v = unsafe { gos_rt_vec_with_capacity(8, codepoints.len() as i64) };
        if v.is_null() {
            return v;
        }
        unsafe {
            let header = &mut *v;
            let dst = header.ptr.cast::<i64>();
            for (i, cp) in codepoints.iter().enumerate() {
                *dst.add(i) = *cp;
            }
            header.len = codepoints.len() as i64;
        }
        v
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_byte_at(s: *const c_char, i: i64) -> i64 {
    // Bare (no `ffi_entry!`): byte access is a generated-code primitive, is
    // panic-free, and commonly executes once per input byte. Wrapping each
    // read in `catch_unwind` and an allocation-registry lock made parsers pay
    // synchronization overhead in their innermost loop.
    if s.is_null() || i < 0 {
        return 0;
    }
    let len = unsafe { typed_str_len(s) };
    if i as usize >= len {
        return 0;
    }
    // SAFETY: `i` lies in `[0, len)`, so the byte at offset `i` is within the
    // compiler-typed string's content bytes.
    let byte = unsafe { *s.cast::<u8>().add(i as usize) };
    i64::from(byte)
}

/// `os::read_dir(path) -> Result<Vec<String>, errors::Error>` -
/// returns the entry names under `path` as a `*mut GosVec` of
/// `*const c_char`. Gossamer programs treat the call as
/// fallible, but the MIR pin keeps it as a plain `Vec<String>`
/// today (matching the interp's shape) - error cases land as an
/// empty vec rather than a Result-shaped Adt.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_os_read_dir(path: *const c_char) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let p = if path.is_null() {
            std::path::PathBuf::from(".")
        } else {
            let encoded = unsafe { CStr::from_ptr(path).to_string_lossy() };
            super::args::decode_os_path(&encoded)
        };
        let entries: Vec<String> = match std::fs::read_dir(&p) {
            Ok(it) => {
                let mut names: Vec<String> = it
                    .flatten()
                    .map(|e| super::args::encode_os_path(std::path::Path::new(&e.file_name())))
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

/// `s.substring(start, end)` - byte-range slice. Clamps `start`
/// and `end` into `[0, len(s)]` and returns the indicated byte
/// substring as a fresh `*mut c_char`. Mirrors the interp
/// builtin so user code that calls `s.substring(a, b)` runs the
/// same way under `gos run` and `gos build` - without this
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
        // O(1) length from the string's length header (every runtime-built
        // string carries one); an untagged rodata literal falls back to
        // strlen. Sizing the slice from the header keeps `substring`
        // proportional to the slice length, not the source length, so a
        // sliding-window scan over one string stays linear.
        let len = unsafe { str_header_len(s) }.unwrap_or_else(|| unsafe { c_str_len(s) });
        let len_i = len as i64;
        let lo = start.clamp(0, len_i) as usize;
        let hi = end.clamp(0, len_i).max(start.clamp(0, len_i)) as usize;
        let bytes = unsafe { std::slice::from_raw_parts(s.cast::<u8>(), len) };
        let slice = &bytes[lo..hi];
        // Every Gossamer string is valid UTF-8, an invariant consumers rely on
        // (`from_utf8_unchecked` in the set shims). A clamped byte range can
        // land mid-codepoint, so validate and repair with the same lossy
        // U+FFFD substitution the interpreter builtin (`str_substring_inline`)
        // uses, keeping the result well-formed and the tiers identical. The
        // valid path skips `alloc_cstring`'s first-NUL scan (the slice length
        // is authoritative).
        match std::str::from_utf8(slice) {
            Ok(_) => alloc_cstring_from_slices(&[slice]),
            Err(_) => alloc_cstring(String::from_utf8_lossy(slice).as_bytes()),
        }
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
            unsafe { gos_str_key_bytes(a) }
        };
        let b_bytes: &[u8] = if b_empty {
            &[]
        } else {
            unsafe { gos_str_key_bytes(b) }
        };
        let force_heap = crate::c_abi::rc::in_region_arena(a.cast())
            || crate::c_abi::rc::in_region_arena(b.cast());
        alloc_growable_forced(
            &[a_bytes, b_bytes],
            a_bytes.len() + b_bytes.len(),
            force_heap,
        )
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
    // Bare (no `ffi_entry!`): like the RC primitives, this is on the hot
    // string-accumulation path (one call per appended fragment) where the
    // per-call catch_unwind setup dominates, and it is panic-free across the
    // FFI boundary - pointer arithmetic, memcpy, and a stack `write!` never
    // unwind; the only failure path (`alloc_growable` OOM) aborts.
    {
        let a_empty = a.is_null() || unsafe { *a.cast::<u8>() } == 0;
        let b_empty = b.is_null() || unsafe { *b.cast::<u8>() } == 0;

        if b_empty {
            // Nothing new to append; return a as-is when it is already owned.
            if is_managed_string(a) {
                return a.cast_mut();
            }
            let a_bytes: &[u8] = if a_empty {
                &[]
            } else {
                unsafe { gos_str_key_bytes(a) }
            };
            let force_heap = crate::c_abi::rc::in_region_arena(a.cast());
            return alloc_growable_forced(&[a_bytes], 64.max(a_bytes.len()), force_heap);
        }

        let b_bytes: &[u8] = unsafe { typed_str_bytes(b) };
        let len_b = b_bytes.len();

        // Fast path: a is a known live heap builder - try in-place append.
        // Region/static pointers intentionally take the copying path: their
        // ABI has no registration callback, so probing a prefix would make a
        // foreign pointer unsafe to pass here.
        if unsafe { is_typed_builder(a) } {
            let hdr = unsafe { a.cast::<u8>().sub(13) };
            let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
            let cap =
                u32::from_le_bytes(unsafe { [*hdr.add(4), *hdr.add(5), *hdr.add(6), *hdr.add(7)] })
                    as usize;
            let len_a = u32::from_le_bytes(unsafe {
                [*hdr.add(8), *hdr.add(9), *hdr.add(10), *hdr.add(11)]
            }) as usize;
            let new_len = len_a + len_b;
            // In-place only when sole owner (rc == 1): mutating a shared
            // buffer would corrupt other holders.
            if new_len <= cap && rc == 1 {
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

        // a is null, a literal, or a fixed heap string - allocate fresh growable.
        let a_bytes: &[u8] = if a_empty {
            &[]
        } else {
            unsafe { gos_str_key_bytes(a) }
        };
        let new_len = a_bytes.len() + len_b;
        let new_cap = (new_len * 2).max(64);
        let force_heap = crate::c_abi::rc::in_region_arena(a.cast())
            || crate::c_abi::rc::in_region_arena(b.cast());
        let result = alloc_growable_forced(&[a_bytes, b_bytes], new_cap, force_heap);
        if is_managed_string(a) {
            unsafe { gos_rt_str_free(a.cast_mut()) };
        }
        result
    }
}

/// Appends `len` bytes at `b` onto growable string `acc`, freeing/reusing
/// `acc`, and returns the result. The byte-counted counterpart of
/// [`gos_rt_str_concat_drop_a`]: the caller supplies the fragment length
/// (a compile-time constant for string-literal appends), so the hot path
/// skips the `strlen` (`CStr::from_ptr`) that `concat_drop_a` pays per call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_append_bytes(
    acc: *const c_char,
    b: *const u8,
    len: i64,
) -> *mut c_char {
    let len_b = if len < 0 { 0 } else { len as usize };
    if len_b == 0 {
        return unsafe { gos_rt_str_concat_drop_a(acc, c"".as_ptr()) };
    }
    let b_bytes: &[u8] = unsafe { std::slice::from_raw_parts(b, len_b) };

    // Generated code supplies a typed String, so its private tag is directly
    // available without a global allocation-registry lookup.
    if unsafe { is_typed_builder(acc) } {
        let hdr = unsafe { acc.cast::<u8>().sub(13) };
        let rc = u32::from_le_bytes(unsafe { [*hdr, *hdr.add(1), *hdr.add(2), *hdr.add(3)] });
        let cap =
            u32::from_le_bytes(unsafe { [*hdr.add(4), *hdr.add(5), *hdr.add(6), *hdr.add(7)] })
                as usize;
        let len_a =
            u32::from_le_bytes(unsafe { [*hdr.add(8), *hdr.add(9), *hdr.add(10), *hdr.add(11)] })
                as usize;
        let new_len = len_a + len_b;
        if new_len <= cap && rc == 1 {
            unsafe {
                let dst = (acc as *mut u8).add(len_a);
                std::ptr::copy_nonoverlapping(b_bytes.as_ptr(), dst, len_b);
                *dst.add(len_b) = 0;
                let hdr_mut = hdr.cast_mut();
                std::ptr::copy_nonoverlapping(
                    (new_len as u32).to_le_bytes().as_ptr(),
                    hdr_mut.add(8),
                    4,
                );
            }
            return acc.cast_mut();
        }
        let a_content = unsafe { std::slice::from_raw_parts(acc.cast::<u8>(), len_a) };
        let result = alloc_growable(&[a_content, b_bytes], (new_len * 2).max(64));
        unsafe { gos_rt_str_free(acc.cast_mut()) };
        return result;
    }

    let a_empty = acc.is_null() || unsafe { *acc.cast::<u8>() } == 0;
    let a_bytes: &[u8] = if a_empty {
        &[]
    } else {
        unsafe { gos_str_key_bytes(acc) }
    };
    let force_heap =
        crate::c_abi::rc::in_region_arena(acc.cast()) || crate::c_abi::rc::in_region_arena(b);
    let result = alloc_growable_forced(
        &[a_bytes, b_bytes],
        ((a_bytes.len() + len_b) * 2).max(64),
        force_heap,
    );
    if is_managed_string(acc) {
        unsafe { gos_rt_str_free(acc.cast_mut()) };
    }
    result
}

/// Writes `bytes` into an exclusively owned builder whose caller already
/// reserved enough capacity, then publishes the new length and terminator.
/// This is the internal bulk-writer path used by serializers that own the
/// builder for their entire lifetime. It avoids repeating ownership and
/// capacity checks for every small formatter fragment.
///
/// # Safety
///
/// `acc` must be a live, uniquely owned growable Gossamer string. `offset`
/// must equal its current length, and `offset + bytes.len()` must not exceed
/// its capacity.
pub(crate) unsafe fn str_builder_write_reserved(acc: *mut c_char, offset: usize, bytes: &[u8]) {
    let new_len = offset
        .checked_add(bytes.len())
        .expect("reserved string length overflow");
    debug_assert!(unsafe { is_typed_builder(acc) });
    debug_assert!(u32::try_from(new_len).is_ok());
    unsafe {
        let dst = acc.cast::<u8>().add(offset);
        copy_small_bytes(bytes.as_ptr(), dst, bytes.len());
        *dst.add(bytes.len()) = 0;
        let len_header = acc.cast::<u8>().sub(5);
        std::ptr::copy_nonoverlapping((new_len as u32).to_le_bytes().as_ptr(), len_header, 4);
    }
}

/// Appends the decimal form of `n` straight onto growable string `acc`
/// and returns the (possibly reallocated) accumulator. The digits format
/// into a stack buffer, so the value reaches `acc` in a single copy - the
/// fused form of `acc += format!("{}", n)` that skips the concat buffer and
/// the throwaway result string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_append_i64(acc: *const c_char, n: i64) -> *mut c_char {
    // Bare + byte-counted: see gos_rt_str_concat_drop_a / gos_rt_str_append_bytes.
    // `itoa` formats into a stack buffer without the generic `fmt::Write`
    // machinery; this is the hot fused path for `s += format!("{}", i)`.
    let mut buf = itoa::Buffer::new();
    let digits = buf.format(n);
    unsafe { gos_rt_str_append_bytes(acc, digits.as_ptr(), digits.len() as i64) }
}

/// Appends the decimal form of `x` straight onto growable string `acc`.
/// See [`gos_rt_str_append_i64`]; the stack buffer holds every finite
/// `f64`'s shortest round-tripping decimal, with a heap fallback for the
/// pathological denormal lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_append_f64(acc: *const c_char, x: f64) -> *mut c_char {
    use std::io::{Cursor, Write};
    let mut buf = [0u8; 512];
    let mut cur = Cursor::new(&mut buf[..]);
    if write!(cur, "{x}").is_ok() {
        let len = cur.position() as i64;
        unsafe { gos_rt_str_append_bytes(acc, buf.as_ptr(), len) }
    } else {
        let s = format!("{x}");
        unsafe { gos_rt_str_append_bytes(acc, s.as_ptr(), s.len() as i64) }
    }
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

/// `s.trim_start() / strings::trim_start(s)` - strips leading
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

/// `s.trim_end() / strings::trim_end(s)` - strips trailing
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
        if bytes.is_ascii() {
            return alloc_ascii_upper_cstring(bytes);
        }
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
/// lowering reads the right discriminant - the bare i64 form
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

/// `s.to_i64() -> Option<i64>` packed as `{disc, payload}` (`disc 0 =
/// Some`, `disc 1 = None`). Strict full-string parse, no trimming.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_i64_opt(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let text = unsafe { std::ffi::CStr::from_ptr(s) }.to_string_lossy();
        match text.parse::<i64>() {
            Ok(n) => unsafe { gos_rt_result_new(0, n) },
            Err(_) => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `s.to_f64() -> Option<f64>`: the Some payload carries the value's
/// bits (`gos_rt_result_new_f64`), read back by the f64 payload path.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_f64_opt(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let text = unsafe { std::ffi::CStr::from_ptr(s) }.to_string_lossy();
        match text.parse::<f64>() {
            Ok(f) => crate::c_abi::gos_rt_result_new_f64(0, f),
            Err(_) => unsafe { gos_rt_result_new(1, 0) },
        }
    })
}

/// `s.to_bool() -> Option<bool>`: accepts exactly `true` / `false`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_to_bool_opt(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        if s.is_null() {
            return unsafe { gos_rt_result_new(1, 0) };
        }
        let text = unsafe { std::ffi::CStr::from_ptr(s) }.to_string_lossy();
        match text.as_ref() {
            "true" => unsafe { gos_rt_result_new(0, 1) },
            "false" => unsafe { gos_rt_result_new(0, 0) },
            _ => unsafe { gos_rt_result_new(1, 0) },
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

/// `s.strip_chars(cutset)` - trims any char in `cutset` from both
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

/// `s.zfill(width)` - pad with `'0'` on the left until at least
/// `width` characters wide.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_zfill(s: *const c_char, width: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let s = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        if width < 0 {
            unsafe { gos_rt_panic(c"strings::center: width must be non-negative".as_ptr()) };
        }
        if width == 0 {
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

/// `s.center(width, pad_char)` - symmetric pad to `width`. Pads
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
/// `*mut GosVec` of c-string pointers. Mirrors Rust's `str::split`
/// (and `gossamer_std::strings::split`): an empty separator yields
/// an empty leading field, one field per character, and an empty
/// trailing field. Each split slice gets its own heap-allocated
/// nul-terminated copy so the caller can hold them past the
/// underlying string's lifetime.
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
        let parts: Vec<*mut c_char> = s.split(sep).map(|p| alloc_cstring(p.as_bytes())).collect();
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

/// Reads element `i` of a scalar Vec at its declared stride: 1-byte
/// slots widen from `u8`, everything else reads the full 8-byte word.
unsafe fn vec_scalar_word(vec: &GosVec, i: usize) -> i64 {
    let p = unsafe { vec.ptr.add(i * (vec.elem_bytes as usize)) };
    if vec.elem_bytes == 1 {
        i64::from(unsafe { *p })
    } else {
        unsafe { (p as *const i64).read_unaligned() }
    }
}

/// `xs.join(sep)` for an integer-element Vec: Display-render each
/// element, joined by `sep`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_join_i64(v: *const GosVec, sep: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"");
        }
        let vec = unsafe { &*v };
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
            let n = unsafe { vec_scalar_word(vec, i) };
            out.push_str(&format!("{n}"));
        }
        alloc_cstring(out.as_bytes())
    })
}

/// `xs.join(sep)` for an f64-element Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_join_f64(v: *const GosVec, sep: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"");
        }
        let vec = unsafe { &*v };
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
            let bits = unsafe { vec_scalar_word(vec, i) };
            let f = f64::from_bits(bits as u64);
            out.push_str(&format!("{f}"));
        }
        alloc_cstring(out.as_bytes())
    })
}

/// `xs.join(sep)` for a bool-element Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_join_bool(v: *const GosVec, sep: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"");
        }
        let vec = unsafe { &*v };
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
            let raw = unsafe { vec_scalar_word(vec, i) };
            out.push_str(if raw & 1 != 0 { "true" } else { "false" });
        }
        alloc_cstring(out.as_bytes())
    })
}

/// `xs.join(sep)` for a char-element Vec.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_join_char(v: *const GosVec, sep: *const c_char) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if v.is_null() {
            return alloc_cstring(b"");
        }
        let vec = unsafe { &*v };
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
            let raw = unsafe { vec_scalar_word(vec, i) };
            let ch = char::from_u32(raw as u32).unwrap_or('\u{FFFD}');
            out.push(ch);
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
        // STRING-typed - same ownership contract as `gos_rt_str_split`.
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

/// Append a Unicode codepoint to `s`, returning the new string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_push_char(s: *const c_char, c: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let base = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let ch = char::from_u32(c as u32).unwrap_or('\u{FFFD}');
        let mut out = String::with_capacity(base.len() + 4);
        out.push_str(base);
        out.push(ch);
        alloc_cstring(out.as_bytes())
    })
}

/// Append a byte (its Unicode codepoint, identical to ASCII for 0-127)
/// to `s`, returning the new string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_push_byte(s: *const c_char, b: i32) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let base = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        let ch = char::from(b as u8);
        let mut out = String::with_capacity(base.len() + 4);
        out.push_str(base);
        out.push(ch);
        alloc_cstring(out.as_bytes())
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
        if n < 0 {
            unsafe { gos_rt_panic(c"strings::repeat: count must be non-negative".as_ptr()) };
        }
        let n = n as usize;
        if s.len().checked_mul(n).is_none() {
            unsafe { gos_rt_panic(c"string repeat capacity overflow".as_ptr()) };
        }
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
        // The lifted function address is stored as a 64-bit word but a
        // function pointer is target-pointer-width (32-bit on wasm32),
        // so narrow through `usize` before reinterpreting. Identity on
        // 64-bit native.
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr as usize) };
        let new_payload = f(closure as i64, gos_rt_result_payload(result));
        unsafe { gos_rt_result_new(1, new_payload) }
    })
}

/// `result.map(closure)` for **capturing** closures whose lifted
/// function follows the env-first ABI `extern "C" fn(env, payload)
/// -> i64`. Non-capturing closures must dispatch through
/// [`gos_rt_result_map_bare`] instead - they have no env slot, so
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
        // The lifted function address is stored as a 64-bit word but a
        // function pointer is target-pointer-width (32-bit on wasm32),
        // so narrow through `usize` before reinterpreting. Identity on
        // 64-bit native.
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr as usize) };
        let new_payload = f(closure as i64, gos_rt_result_payload(result));
        unsafe { gos_rt_result_new(0, new_payload) }
    })
}

/// `result::default_with(closure, result)` - returns the `Ok` value
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
        // The lifted function address is stored as a 64-bit word but a
        // function pointer is target-pointer-width (32-bit on wasm32),
        // so narrow through `usize` before reinterpreting. Identity on
        // 64-bit native.
        let f: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_addr as usize) };
        f(closure as i64, gos_rt_result_payload(result))
    })
}

/// `result::default(fallback, result)` - returns the `Ok` payload,
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
/// i64` (no env slot - this is what `gossamer-hir::lift_closed`
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

/// `time::Duration::from_secs(n)` lowering - returns `n * 1000` as
/// the i64-millisecond Duration the compiled tier carries.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_duration_from_secs(secs: i64) -> i64 {
    ffi_entry!(-1, { secs.saturating_mul(1_000) })
}

// `flag::parse([decls])` declarative parser - takes an array of
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
        // future surface - `flag::parse(...)?.positional`).
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
/// Parses an RFC 3339 timestamp and returns unix milliseconds.
/// Accepts `T` or space as the date/time separator; accepts `Z`,
/// `+HH:MM`, `-HH:MM`, or no suffix (assumes UTC); sub-second
/// fractions are accepted and dropped. A faithful port of
/// `gossamer_std::time::parse_rfc3339` so the compiled tier matches
/// the VM bit-for-bit (timezone offsets, day-of-month validation,
/// and pre-1970 negative results all included).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_time_parse_rfc3339(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        let err = || -> i128 {
            let cs = alloc_cstring(b"time::parse: bad input");
            unsafe { gos_rt_result_new(1, cs as i64) }
        };
        if s.is_null() {
            return err();
        }
        let text = unsafe { CStr::from_ptr(s).to_str().unwrap_or("") };
        match parse_rfc3339_ms(text) {
            Some(ms) => unsafe { gos_rt_result_new(0, ms) },
            None => err(),
        }
    })
}

/// Parses one zero-padded unsigned field, mirroring `parse_unsigned`
/// in `gossamer_std::time` (rejects signs, spaces, and non-digits).
fn parse_rfc3339_uint(bytes: &[u8]) -> Option<i64> {
    std::str::from_utf8(bytes)
        .ok()?
        .parse::<u32>()
        .ok()
        .map(i64::from)
}

const fn is_leap_year(y: i64) -> bool {
    (y % 4 == 0 && y % 100 != 0) || y % 400 == 0
}

const fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if is_leap_year(year) {
                29
            } else {
                28
            }
        }
        _ => 0,
    }
}

/// Howard Hinnant's `days_from_civil`, matching the i32/u32 version
/// in `gossamer_std::time` over the representable Gregorian range.
fn civil_to_days(y: i64, m: i64, d: i64) -> i64 {
    let y_adj = y - i64::from(m <= 2);
    let era = if y_adj >= 0 {
        y_adj / 400
    } else {
        (y_adj - 399) / 400
    };
    let yoe = y_adj - era * 400;
    let m_eff = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * m_eff + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Faithful port of `gossamer_std::time::parse_rfc3339` returning
/// unix milliseconds, or `None` for any malformed/out-of-range input.
fn parse_rfc3339_ms(s: &str) -> Option<i64> {
    let bytes = s.as_bytes();
    if bytes.len() < 19 {
        return None;
    }
    let year: i64 = std::str::from_utf8(&bytes[0..4])
        .ok()?
        .parse::<i32>()
        .ok()? as i64;
    if bytes[4] != b'-' {
        return None;
    }
    let month = parse_rfc3339_uint(&bytes[5..7])?;
    if bytes[7] != b'-' {
        return None;
    }
    let day = parse_rfc3339_uint(&bytes[8..10])?;
    if !matches!(bytes[10], b'T' | b' ') {
        return None;
    }
    let hour = parse_rfc3339_uint(&bytes[11..13])?;
    if bytes[13] != b':' {
        return None;
    }
    let minute = parse_rfc3339_uint(&bytes[14..16])?;
    if bytes[16] != b':' {
        return None;
    }
    let second = parse_rfc3339_uint(&bytes[17..19])?;
    let mut cursor = 19;
    if bytes.get(cursor) == Some(&b'.') {
        cursor += 1;
        while cursor < bytes.len() && bytes[cursor].is_ascii_digit() {
            cursor += 1;
        }
    }
    let mut offset_seconds: i64 = 0;
    if cursor < bytes.len() {
        match bytes[cursor] {
            b'Z' => cursor += 1,
            b'+' | b'-' => {
                if cursor + 5 >= bytes.len() {
                    return None;
                }
                let sign: i64 = if bytes[cursor] == b'+' { 1 } else { -1 };
                let oh = parse_rfc3339_uint(&bytes[cursor + 1..cursor + 3])?;
                if bytes[cursor + 3] != b':' {
                    return None;
                }
                let om = parse_rfc3339_uint(&bytes[cursor + 4..cursor + 6])?;
                offset_seconds = sign * (oh * 3600 + om * 60);
                cursor += 6;
            }
            _ => return None,
        }
    }
    if cursor != bytes.len() {
        return None;
    }
    if !(1..=12).contains(&month)
        || !(1..=days_in_month(year, month)).contains(&day)
        || hour >= 24
        || minute >= 60
        || second >= 60
    {
        return None;
    }
    let unix_secs = civil_to_days(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second
        - offset_seconds;
    unix_secs.checked_mul(1_000)
}

/// `time::Duration::from_millis(n)` lowering - Duration is already
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

/// Stringifies an `f64` with `prec` fractional digits - the runtime
/// side of `format!("{:.N}", x)`. Routes through the Rust standard
/// library's float formatter so rounding matches the interpreter's
/// `{:.N}` Display output bit-for-bit. Very large `prec` is clamped
/// to a sane upper bound to keep the allocation bounded.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_f64_prec_to_str(x: f64, prec: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        if prec < 0 {
            unsafe { gos_rt_panic(c"__fmt_prec: precision must be non-negative".as_ptr()) };
        }
        let prec = prec.min(64) as usize;
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
        if n < 0 {
            unsafe { gos_rt_panic(c"strings::splitn: count must be non-negative".as_ptr()) };
        }
        let n = usize::try_from(n).unwrap_or(0);
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
        if n < 0 {
            unsafe { gos_rt_panic(c"strings::replacen: count must be non-negative".as_ptr()) };
        }
        let n = usize::try_from(n).unwrap_or(0);
        let out = unsafe { cstr(s) }.replacen(unsafe { cstr(from) }, unsafe { cstr(to) }, n);
        alloc_cstring(out.as_bytes())
    })
}

/// `strings::to_title(s) -> String` - capitalises the first
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

/// First Unicode scalar of `s`, or 32 (space) when `s` is empty or
/// null. Backs the `strings::pad_left/pad_right` lowering, whose
/// pad-char parameter is an `i64` codepoint but whose language-level
/// argument is a String pad glyph.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_first_codepoint(s: *const c_char) -> i64 {
    ffi_entry!(32, {
        unsafe { cstr(s) }.chars().next().map_or(32, |c| c as i64)
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
        if width < 0 {
            unsafe { gos_rt_panic(c"strings::pad_left: width must be non-negative".as_ptr()) };
        }
        let width = usize::try_from(width).unwrap_or(0);
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
        if width < 0 {
            unsafe { gos_rt_panic(c"strings::pad_right: width must be non-negative".as_ptr()) };
        }
        let width = usize::try_from(width).unwrap_or(0);
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

/// `__fmt_pad(s, width, fill, align)` - pads the already-rendered string `s`
/// to `width` characters with the `fill` codepoint. `align`: 0 = right
/// (pad on the left), 1 = left (pad on the right), 2 = center. Backs the
/// `{:>N}` / `{:<N}` / `{:^N}` / `{:0N}` format specs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_fmt_pad(
    s: *const c_char,
    width: i64,
    fill: i64,
    align: i64,
) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let text = if s.is_null() {
            ""
        } else {
            unsafe { CStr::from_ptr(s).to_str().unwrap_or("") }
        };
        if width < 0 {
            unsafe { gos_rt_panic(c"__fmt_pad: width must be non-negative".as_ptr()) };
        }
        let width = usize::try_from(width).unwrap_or(0);
        let pad_char = u32::try_from(fill)
            .ok()
            .and_then(char::from_u32)
            .unwrap_or(' ');
        let count = text.chars().count();
        if count >= width {
            return alloc_cstring(text.as_bytes());
        }
        let total = width - count;
        let (left, right) = match align {
            1 => (0, total),                     // left-align: pad on the right
            2 => (total / 2, total - total / 2), // center
            _ => (total, 0),                     // right-align / default
        };
        let mut out = String::with_capacity(text.len() + total);
        for _ in 0..left {
            out.push(pad_char);
        }
        out.push_str(text);
        for _ in 0..right {
            out.push(pad_char);
        }
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

/// `strings::equal_fold(a, b) -> bool` - case-insensitive compare.
/// Mirrors `gossamer_std::strings::equal_fold`: compares scalar by
/// scalar and requires both sequences to end together, so a string
/// is never equal to a fold-prefix of itself even when their byte
/// lengths coincide (e.g. KELVIN SIGN U+212A vs "kab").
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_str_equal_fold(a: *const c_char, b: *const c_char) -> i32 {
    ffi_entry!(-1, {
        let mut ac = unsafe { cstr(a) }.chars();
        let mut bc = unsafe { cstr(b) }.chars();
        loop {
            match (ac.next(), bc.next()) {
                (Some(x), Some(y)) if x.to_lowercase().eq(y.to_lowercase()) => {}
                (None, None) => return 1,
                _ => return 0,
            }
        }
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
