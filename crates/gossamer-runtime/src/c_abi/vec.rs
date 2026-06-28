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

use super::*;

// ---------------------------------------------------------------
// Vec runtime - a `{ elem_bytes, len, cap, ptr }` struct
// ---------------------------------------------------------------

/// Element kind tag carried in the `GosVec` header so
/// `gos_rt_vec_free` can free element payloads instead of just the
/// backing byte buffer. Default `0` (primitive) preserves the
/// shallow-free behaviour every existing call site assumes; typed
/// vecs created via `gos_rt_vec_new_typed` opt in to deep free.
///
/// Encoding is deliberately small (one byte) so the field fits in
/// the existing 4-byte padding between `elem_bytes` (u32) and
/// `ptr` (8-byte aligned pointer). Adding it does not change the
/// struct size, the offset of `ptr`, or the offset of `len` - all
/// of which the codegen reads at fixed offsets.
pub mod vec_elem_kind {
    /// Element payload is a primitive value owning no other heap
    /// memory (i64, f64, u8, bool, etc.). Shallow free of the
    /// backing buffer is correct.
    pub const PRIMITIVE: u8 = 0;
    /// Element is a `*mut c_char` cstring; each element is freed
    /// via `gos_rt_str_free` before the buffer itself is reclaimed.
    pub const STRING: u8 = 1;
    /// Element is a `*mut GosVec`; each element is recursively
    /// freed via `gos_rt_vec_free`.
    pub const VEC: u8 = 2;
    /// Element is a `*mut GosMap`; each element is freed via
    /// `gos_rt_map_free`.
    pub const MAP: u8 = 3;
    /// Element is a `*mut GosError`; each element is freed via
    /// `gos_rt_error_free`.
    pub const ERROR: u8 = 4;
    /// Element is a multi-slot struct/tuple stored inline whose option
    /// payload words may hold copy-blob pointers. The per-type guarded
    /// meta lives in the side table (`vec_elem_meta`); `gos_rt_vec_free`
    /// releases each element's guarded children, and `gos_rt_vec_push`
    /// retains them when the element bytes are copied in.
    pub const AGGR_GUARDED: u8 = 5;
    /// Element is a multi-slot struct/tuple stored inline whose slots
    /// embed heap pointers (runtime strings / nested vecs) OWNED by the
    /// vec. The per-vec slot layout lives in the side table
    /// ([`super::vec::vec_slot_children`]); `gos_rt_vec_free` deep-frees
    /// each live child even when the vec was never iterated (the
    /// early-`break` path), and `gos_rt_vec_push` retains the copied
    /// slot's children. Set via [`super::vec::vec_set_slot_children`] by
    /// runtime materializer shims - never by codegen.
    pub const AGGR_OWNED: u8 = 6;
    /// Slot-child kind (NOT a vec-level `elem_kind`): the slot word holds
    /// a reference-counted heap node - a user enum or struct pointer
    /// (possibly tag-bit-encoded) - retained via `gos_rt_rc_retain` and
    /// released via `gos_rt_rc_release`, both of which mask the low tag
    /// bits and walk the node's own child meta. Used inside `AGGR_OWNED`
    /// slot-children layouts for an aggregate element's enum/struct
    /// fields.
    pub const RC_NODE: u8 = 7;
}

#[repr(C)]
pub struct GosVec {
    pub len: i64,
    pub cap: i64,
    pub elem_bytes: u32,
    /// Element-kind tag (see [`vec_elem_kind`]) so `gos_rt_vec_free`
    /// can deep-free pointer-bearing element types. Sits in the
    /// padding before `ptr` so the struct layout (size, ptr offset,
    /// len offset) is unchanged from prior 0.5 releases.
    pub elem_kind: u8,
    /// Region-allocation marker (was `_reserved[0]`, struct offset 21):
    /// `VEC_REGION_FLAG` when this header and its buffer live in an arena
    /// slab, so `gos_rt_vec_free` skips them.
    pub region_flag: u8,
    /// Strong refcount of a non-region Vec - an actual `AtomicU16` so its
    /// atomic loads/stores are sound. (A prior `[u8; 3]` reinterpret-cast to
    /// `AtomicU16` tripped Miri's Stacked-Borrows model: a read-only,
    /// single-byte-provenance pointer cannot be retagged for atomic
    /// read-write access.) Same offset (22) and size as the bytes it
    /// replaces, so `ptr` stays at offset 24 and the layout is unchanged.
    pub rc: std::sync::atomic::AtomicU16,
    pub ptr: SyncRawPtr<u8>,
}

/// `region_flag` value marking a GosVec (and its backing buffer) as
/// arena-region-allocated: both live in region slabs, so `gos_rt_vec_free`
/// must skip them (they are freed wholesale at `arena_pop`).
const VEC_REGION_FLAG: u8 = 1;

/// Allocate a GosVec header from the active region if one is open (so it is
/// freed wholesale at pop and `gos_rt_vec_free` skips it), else from the
/// global allocator via `Box`. Sets the region flag accordingly.
unsafe fn alloc_vec_header(mut v: GosVec) -> *mut GosVec {
    let p = crate::c_abi::rc::region_alloc_bytes(std::mem::size_of::<GosVec>());
    if p.is_null() {
        crate::c_abi::ledger::vec_inc();
        vec_set_rc(&v, 1);
        Box::into_raw(Box::new(v))
    } else {
        v.region_flag = VEC_REGION_FLAG;
        let hp = p.cast::<GosVec>();
        unsafe { std::ptr::write(hp, v) };
        hp
    }
}

/// Per-vec guarded element meta, keyed by the stable `GosVec` header
/// pointer. Only vecs tagged `vec_elem_kind::AGGR_GUARDED` have entries,
/// so ordinary vecs never consult the table - the tag byte (already in
/// the header) gates every lookup. Entries are removed when the vec is
/// reclaimed, so a reused header address cannot inherit a stale meta.
static VEC_ELEM_METAS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<usize, usize>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

pub(crate) fn vec_elem_meta(v: *const GosVec) -> *const i64 {
    *VEC_ELEM_METAS.lock().get(&(v as usize)).unwrap_or(&0) as *const i64
}

pub(crate) fn vec_elem_meta_remove(v: *const GosVec) {
    // PRIMITIVE vecs never have side-table entries, so skip both locks entirely.
    if unsafe { (*v).elem_kind } == vec_elem_kind::PRIMITIVE {
        return;
    }
    VEC_ELEM_METAS.lock().remove(&(v as usize));
    VEC_SLOT_CHILDREN.lock().remove(&(v as usize));
}

/// One owned heap child inside each element slot of an
/// [`vec_elem_kind::AGGR_OWNED`] vec.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VecSlotChild {
    /// Discriminant value under which the child word holds a live
    /// pointer (`0` = Ok/Some side); negative means unconditional.
    pub gate: i64,
    /// Word index (8-byte units) of the discriminant within the slot.
    /// Ignored when `gate` is negative.
    pub disc_word: usize,
    /// Word index of the child pointer within the slot.
    pub word: usize,
    /// Child kind - [`vec_elem_kind::STRING`] or [`vec_elem_kind::VEC`]
    /// - selecting the free / retain routine.
    pub kind: u8,
}

/// Per-vec owned-slot-children layouts, keyed by the stable `GosVec`
/// header pointer. Only vecs tagged `vec_elem_kind::AGGR_OWNED` have
/// entries; the tag byte gates every lookup. Entries are removed when
/// the vec is reclaimed, so a reused header address cannot inherit a
/// stale layout.
static VEC_SLOT_CHILDREN: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<usize, &'static [VecSlotChild]>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Slot-children layout of an `AGGR_OWNED` vec, or `None` for any
/// other vec.
pub fn vec_slot_children(v: *const GosVec) -> Option<&'static [VecSlotChild]> {
    VEC_SLOT_CHILDREN.lock().get(&(v as usize)).copied()
}

/// Tags `v` as an [`vec_elem_kind::AGGR_OWNED`] vec and records where
/// the owned heap children live inside each element slot, so
/// `gos_rt_vec_free` deep-frees them even when the vec was never
/// (or only partially) iterated.
///
/// Materializer shims MUST call this AFTER their construction pushes:
/// a freshly `alloc_cstring`'d child's initial reference is the vec's
/// own share, while `gos_rt_vec_push` retains the children of every
/// slot pushed onto an already-tagged vec (the push-site source keeps
/// its own share, mirroring the `AGGR_GUARDED` contract).
///
/// No-op for null / region vecs (region storage is freed wholesale at
/// `arena_pop` and never walked).
pub fn vec_set_slot_children(v: *mut GosVec, children: &'static [VecSlotChild]) {
    if v.is_null() {
        return;
    }
    // SAFETY: callers hand a live header they just allocated.
    let vec = unsafe { &mut *v };
    if vec_is_region(vec) {
        return;
    }
    vec.elem_kind = vec_elem_kind::AGGR_OWNED;
    VEC_SLOT_CHILDREN.lock().insert(v as usize, children);
}

/// Calls `f` with each live, non-null child pointer (and its kind)
/// inside the element slot at `slot`, per the `AGGR_OWNED` layout.
unsafe fn visit_slot_children(
    slot: *const u8,
    children: &[VecSlotChild],
    mut f: impl FnMut(*mut u8, u8),
) {
    for c in children {
        if c.gate >= 0 {
            let disc = unsafe { slot.add(c.disc_word * 8).cast::<i64>().read_unaligned() };
            if disc != c.gate {
                continue;
            }
        }
        // Slots hold child pointers exposed as integers by the flat-slot
        // ABI; recover provenance before use.
        let raw = unsafe { slot.add(c.word * 8).cast::<usize>().read_unaligned() };
        let child: *mut u8 = std::ptr::with_exposed_provenance_mut(raw);
        if !child.is_null() {
            f(child, c.kind);
        }
    }
}

/// Retain the owned children of the element slot at `slot` of the
/// `AGGR_OWNED` vec `v` (a slot copy that now shares them).
pub(crate) unsafe fn vec_retain_slot_children(v: *const GosVec, slot: *const u8) {
    let Some(children) = vec_slot_children(v) else {
        return;
    };
    unsafe {
        visit_slot_children(slot, children, |child, kind| match kind {
            vec_elem_kind::STRING => crate::c_abi::string::gos_rt_str_retain(child.cast()),
            vec_elem_kind::VEC => vec_retain_header(child.cast()),
            vec_elem_kind::RC_NODE => crate::c_abi::rc::gos_rt_rc_retain(child),
            _ => {}
        });
    }
}

/// Builds a borrowing `*mut GosVec` view over an array, for a `&[T]` /
/// `&Vec<T>` parameter fed a `&array`. Byte-for-byte identical to
/// [`gos_rt_vec_from_arr`], but the MIR metadata pass deliberately leaves
/// the result `PRIMITIVE` (never `AGGR_OWNED`): a borrow must not free the
/// element children, since the borrowed array still owns them and reclaims
/// them at its own drop. The view's own drop frees only its slot-copy
/// buffer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_borrow_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    unsafe { gos_rt_vec_from_arr(elem_bytes, data, len) }
}

/// Release the owned children of every element of an `AGGR_OWNED`
/// vec. Called by `gos_rt_vec_free` before the buffer is reclaimed,
/// closing the early-`break` leak: slots the consumer never walked
/// still drop their strings / nested vecs here.
pub(crate) unsafe fn vec_release_owned_children(v: &GosVec) {
    let Some(children) = vec_slot_children(v) else {
        return;
    };
    if v.ptr.is_null() {
        return;
    }
    let stride = v.elem_bytes as usize;
    if stride == 0 {
        return;
    }
    for i in 0..v.len.max(0) as usize {
        unsafe {
            visit_slot_children(v.ptr.add(i * stride), children, |child, kind| match kind {
                vec_elem_kind::STRING => crate::c_abi::string::gos_rt_str_free(child.cast()),
                vec_elem_kind::VEC => crate::c_abi::map::gos_rt_vec_free(child.cast()),
                vec_elem_kind::RC_NODE => crate::c_abi::rc::gos_rt_rc_release(child),
                _ => {}
            });
        }
    }
}

/// Bump a non-region `GosVec`'s strong count by one (the header-RC
/// counterpart of `gos_rt_str_retain` for nested-vec children).
pub(crate) unsafe fn vec_retain_header(v: *mut GosVec) {
    if v.is_null() {
        return;
    }
    let vec = unsafe { &*v };
    if vec_is_region(vec) {
        return;
    }
    // Headers from constructors that never wrote a count read 0; in that
    // case a simple +1 would bring it to 1, and the next release would
    // reclaim a still-live header. Use a CAS loop to atomically jump from
    // 0 to 2 (treating 0 as the same as 1), while a normal retain is +1.
    let atomic = vec_rc_atomic(vec);
    let mut current = atomic.load(std::sync::atomic::Ordering::Relaxed);
    loop {
        let next = current.max(1).saturating_add(1);
        match atomic.compare_exchange_weak(
            current,
            next,
            std::sync::atomic::Ordering::Relaxed,
            std::sync::atomic::Ordering::Relaxed,
        ) {
            Ok(_) => break,
            Err(actual) => current = actual,
        }
    }
}

/// Propagates ownership-bearing element kinds from `src` to `out`
/// after `out` received a raw copy of (some of) `src`'s slots:
/// re-tags `out` and retains each copied slot's heap children so both
/// vecs own their shares. Covers `STRING`, `VEC` and `AGGR_OWNED`
/// element kinds; `AGGR_GUARDED` keeps its dedicated copy-blob path at
/// the existing call sites. No-op for primitive / region / null vecs.
pub(crate) unsafe fn vec_share_owned_elements(src: *const GosVec, out: *mut GosVec) {
    if src.is_null() || out.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let o = unsafe { &mut *out };
    match s.elem_kind {
        vec_elem_kind::STRING | vec_elem_kind::VEC if s.elem_bytes == 8 => {
            o.elem_kind = s.elem_kind;
            for i in 0..o.len.max(0) as usize {
                // Exposed-integer slot (flat-slot ABI); recover provenance.
                let raw = unsafe { o.ptr.add(i * 8).cast::<usize>().read_unaligned() };
                let child: *mut u8 = std::ptr::with_exposed_provenance_mut(raw);
                if child.is_null() {
                    continue;
                }
                match s.elem_kind {
                    vec_elem_kind::STRING => unsafe {
                        crate::c_abi::string::gos_rt_str_retain(child.cast());
                    },
                    _ => unsafe { vec_retain_header(child.cast()) },
                }
            }
        }
        vec_elem_kind::AGGR_OWNED => {
            if let Some(children) = vec_slot_children(src) {
                vec_set_slot_children(out, children);
                let stride = o.elem_bytes as usize;
                if stride == 0 || o.ptr.is_null() {
                    return;
                }
                for i in 0..o.len.max(0) as usize {
                    unsafe { vec_retain_slot_children(out, o.ptr.add(i * stride)) };
                }
            } else {
                // Layout unknown (cannot happen for live vecs; defensive):
                // fall back to a shallow copy that never double-frees.
                o.elem_kind = vec_elem_kind::PRIMITIVE;
            }
        }
        _ => {}
    }
}

/// Tags `v` as holding guarded aggregate elements and records the
/// per-type meta used to retain/release their copy-blob children.
/// Emitted by the MIR lowering right after constructing a vec whose
/// element type carries a guarded meta. No-op for null / region vecs
/// (region storage is freed wholesale and never walked).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_set_elem_meta(v: *mut GosVec, meta: *const i64) {
    if v.is_null() || meta.is_null() {
        return;
    }
    let vec = unsafe { &mut *v };
    if vec_is_region(vec) {
        return;
    }
    vec.elem_kind = vec_elem_kind::AGGR_GUARDED;
    VEC_ELEM_METAS.lock().insert(v as usize, meta as usize);
}

/// Parsed `VecSlotChild` layouts, keyed by the (static) meta blob
/// address so each per-type layout is parsed and leaked at most once.
static SLOT_CHILD_LAYOUTS: std::sync::LazyLock<
    parking_lot::Mutex<std::collections::HashMap<usize, &'static [VecSlotChild]>>,
> = std::sync::LazyLock::new(|| parking_lot::Mutex::new(std::collections::HashMap::new()));

/// Tags `v` as an [`vec_elem_kind::AGGR_OWNED`] vec carrying inline
/// aggregate elements whose RC child pointers (strings, nested vecs,
/// user enum/struct heap nodes) the vec owns, recording where they sit
/// inside each element slot. Emitted by the MIR lowering right after
/// constructing such a vec. The `meta` blob is a static, codegen-owned
/// `i64` array: `[count, (gate, disc_word, word, kind) * count]`, with
/// `kind` a [`vec_elem_kind`] slot-child tag (`STRING` / `VEC` /
/// `RC_NODE`). No-op for null / region vecs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_set_slot_children(v: *mut GosVec, meta: *const i64) {
    if v.is_null() || meta.is_null() {
        return;
    }
    let layout = {
        let mut cache = SLOT_CHILD_LAYOUTS.lock();
        if let Some(l) = cache.get(&(meta as usize)) {
            *l
        } else {
            let count = unsafe { *meta }.max(0) as usize;
            let mut children = Vec::with_capacity(count);
            for i in 0..count {
                let base = 1 + i * 4;
                children.push(VecSlotChild {
                    gate: unsafe { *meta.add(base) },
                    disc_word: unsafe { *meta.add(base + 1) }.max(0) as usize,
                    word: unsafe { *meta.add(base + 2) }.max(0) as usize,
                    kind: unsafe { *meta.add(base + 3) } as u8,
                });
            }
            let leaked: &'static [VecSlotChild] = Box::leak(children.into_boxed_slice());
            cache.insert(meta as usize, leaked);
            leaked
        }
    };
    vec_set_slot_children(v, layout);
}

/// Release the guarded children of every element of an
/// `AGGR_GUARDED` vec. Called by `gos_rt_vec_free` before the buffer
/// is reclaimed.
pub(crate) unsafe fn vec_release_guarded_elements(v: &GosVec) {
    let meta = vec_elem_meta(v);
    if meta.is_null() || v.ptr.is_null() {
        return;
    }
    let stride = v.elem_bytes as usize;
    if stride == 0 {
        return;
    }
    for i in 0..v.len.max(0) as usize {
        unsafe {
            crate::c_abi::rc::gos_rt_aggr_release_children(v.ptr.add(i * stride), meta);
        }
    }
}

/// True when this GosVec was allocated inside an arena region.
#[inline]
pub fn vec_is_region(v: &GosVec) -> bool {
    v.region_flag == VEC_REGION_FLAG
}

/// Reads element `idx` of `v` as an i64, honoring the header's
/// `elem_bytes` (packed byte vecs zero-extend, word vecs read the
/// full 8 bytes). `idx` must already be bounds-checked by the
/// caller. 16-byte elements are regex-internal and never reach the
/// scalar helpers; reading their first word is a safe fallback.
pub(crate) unsafe fn vec_elem_load_i64(v: &GosVec, idx: i64) -> i64 {
    let p = unsafe { v.ptr.add((idx as usize) * (v.elem_bytes as usize)) };
    match v.elem_bytes {
        1 => i64::from(unsafe { p.read() }),
        2 => i64::from(unsafe { p.cast::<u16>().read_unaligned() }),
        4 => i64::from(unsafe { p.cast::<u32>().read_unaligned() }),
        _ => {
            debug_assert!(
                v.elem_bytes >= 8,
                "vec_elem_load_i64: unexpected elem_bytes {}",
                v.elem_bytes
            );
            unsafe { p.cast::<i64>().read_unaligned() }
        }
    }
}

/// Reads element `idx` of `v` as the word an `Option<T>` payload carries.
/// A scalar / `String` / single-word element is the word itself (its value
/// or heap pointer). A multi-word element (a struct or tuple, `elem_bytes >
/// 8`) is stored inline; the payload must be a *pointer* to that element so
/// the consumer derefs it for field access, not the truncated first word.
/// `idx` must already be bounds-checked by the caller.
pub(crate) unsafe fn vec_elem_payload_word(v: &GosVec, idx: i64) -> i64 {
    if v.elem_bytes > 8 {
        unsafe { v.ptr.add((idx as usize) * (v.elem_bytes as usize)) as i64 }
    } else {
        unsafe { vec_elem_load_i64(v, idx) }
    }
}

/// Writes `value` to element `idx` of `v`, truncating to the
/// header's `elem_bytes`. Same preconditions as
/// [`vec_elem_load_i64`].
pub(crate) unsafe fn vec_elem_store_i64(v: &GosVec, idx: i64, value: i64) {
    let p = unsafe { v.ptr.add((idx as usize) * (v.elem_bytes as usize)) };
    match v.elem_bytes {
        1 => unsafe { p.write(value as u8) },
        2 => unsafe { p.cast::<u16>().write_unaligned(value as u16) },
        4 => unsafe { p.cast::<u32>().write_unaligned(value as u32) },
        _ => {
            debug_assert!(
                v.elem_bytes >= 8,
                "vec_elem_store_i64: unexpected elem_bytes {}",
                v.elem_bytes
            );
            unsafe { p.cast::<i64>().write_unaligned(value) };
        }
    }
}

/// The atomic refcount field. Interior-mutable, so a shared `&GosVec` can
/// update it without any pointer reinterpretation.
#[inline]
pub fn vec_rc_atomic(v: &GosVec) -> &std::sync::atomic::AtomicU16 {
    &v.rc
}

/// Strong refcount of a non-region Vec, stored in the `rc` atomic field.
/// A Vec aliased > 65535 times is unreachable; the count saturates rather than
/// wrapping. Region Vecs ignore this (they are freed wholesale at region pop).
#[inline]
pub fn vec_rc(v: &GosVec) -> u16 {
    vec_rc_atomic(v).load(std::sync::atomic::Ordering::Relaxed)
}

#[inline]
pub fn vec_set_rc(v: &GosVec, rc: u16) {
    vec_rc_atomic(v).store(rc, std::sync::atomic::Ordering::Relaxed);
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_new(elem_bytes: u32) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        unsafe {
            alloc_vec_header(GosVec {
                len: 0,
                cap: 0,
                elem_bytes,
                elem_kind: vec_elem_kind::PRIMITIVE,
                region_flag: 0,
                rc: std::sync::atomic::AtomicU16::new(0),
                ptr: SyncRawPtr::NULL,
            })
        }
    })
}

/// `gos_rt_vec_new`-like constructor that records the element kind
/// in the header so `gos_rt_vec_free` can deep-free pointer-bearing
/// payloads. `elem_kind` must be a value from [`vec_elem_kind`];
/// out-of-range values fall back to `PRIMITIVE` with an `eprintln!`
/// warning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_new_typed(elem_bytes: u32, elem_kind: u8) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let kind = if elem_kind > vec_elem_kind::ERROR {
            eprintln!(
                "gos_rt_vec_new_typed: unknown elem_kind {elem_kind}; falling back to PRIMITIVE"
            );
            vec_elem_kind::PRIMITIVE
        } else {
            elem_kind
        };
        unsafe {
            alloc_vec_header(GosVec {
                len: 0,
                cap: 0,
                elem_bytes,
                elem_kind: kind,
                region_flag: 0,
                rc: std::sync::atomic::AtomicU16::new(0),
                ptr: SyncRawPtr::NULL,
            })
        }
    })
}

/// Allocates a zeroed `bytes`-byte vec element buffer with 8-byte
/// alignment. Vec slots hold 8-byte words (`i64` / pointer), so the
/// buffer must be word-aligned for the slot accesses across the
/// runtime to be sound; a `Vec<u8>` (align 1) only happens to work
/// because the system allocator over-aligns. Backed by a leaked
/// `Vec<u64>`; free with [`free_vec_buffer`] passing the same `bytes`.
pub(crate) fn alloc_vec_buffer(bytes: usize) -> *mut u8 {
    let words = bytes.div_ceil(8).max(1);
    let mut buf: Vec<u64> = vec![0u64; words];
    let ptr = buf.as_mut_ptr().cast::<u8>();
    std::mem::forget(buf);
    ptr
}

/// Frees a buffer from [`alloc_vec_buffer`]. `bytes` must equal the
/// value passed at allocation; every GosVec buffer is sized
/// `cap * elem_bytes`, stable across the buffer's life.
pub(crate) unsafe fn free_vec_buffer(ptr: *mut u8, bytes: usize) {
    let words = bytes.div_ceil(8).max(1);
    // SAFETY: `ptr` came from `alloc_vec_buffer(bytes)`, so the same
    // word count reconstructs its `Vec<u64>` layout exactly.
    drop(unsafe { Vec::<u64>::from_raw_parts(ptr.cast::<u64>(), words, words) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity(elem_bytes: u32, cap: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if cap <= 0 {
            return unsafe { gos_rt_vec_new(elem_bytes) };
        }
        let bytes = (cap as usize) * (elem_bytes as usize);
        // Zero-initialised so the backing storage is always valid to
        // read (clippy::uninit_vec). The interpreter never observes a
        // slot before it's been explicitly written via push/insert,
        // but zeroing is cheap and removes the UB risk.
        let ptr = alloc_vec_buffer(bytes);
        // Ledger + initial strong count, symmetric with `alloc_vec_header`
        // (gos_rt_vec_free always decrements the ledger).
        crate::c_abi::ledger::vec_inc();
        let v = GosVec {
            len: 0,
            cap,
            elem_bytes,
            elem_kind: vec_elem_kind::PRIMITIVE,
            region_flag: 0,
            rc: std::sync::atomic::AtomicU16::new(0),
            ptr: SyncRawPtr::new(ptr),
        };
        vec_set_rc(&v, 1);
        Box::into_raw(Box::new(v))
    })
}

/// `gos_rt_vec_with_capacity` variant that records the element
/// kind in the header so `gos_rt_vec_free` can deep-free
/// pointer-bearing payloads. See [`vec_elem_kind`] for the tag
/// encoding. Out-of-range tags fall back to `PRIMITIVE` with an
/// `eprintln!` warning.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity_typed(
    elem_bytes: u32,
    cap: i64,
    elem_kind: u8,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let kind = if elem_kind > vec_elem_kind::ERROR {
            eprintln!(
                "gos_rt_vec_with_capacity_typed: unknown elem_kind {elem_kind}; falling back to PRIMITIVE"
            );
            vec_elem_kind::PRIMITIVE
        } else {
            elem_kind
        };
        if cap <= 0 {
            return unsafe { gos_rt_vec_new_typed(elem_bytes, kind) };
        }
        let bytes = (cap as usize) * (elem_bytes as usize);
        let ptr = alloc_vec_buffer(bytes);
        // Ledger + initial strong count, symmetric with `alloc_vec_header`.
        crate::c_abi::ledger::vec_inc();
        let v = GosVec {
            len: 0,
            cap,
            elem_bytes,
            elem_kind: kind,
            region_flag: 0,
            rc: std::sync::atomic::AtomicU16::new(0),
            ptr: SyncRawPtr::new(ptr),
        };
        vec_set_rc(&v, 1);
        Box::into_raw(Box::new(v))
    })
}

/// Builds a fresh `*mut GosVec` from a stack/heap array. Copies
/// `len * elem_bytes` bytes from `data` into a freshly-allocated
/// data buffer; `Box::into_raw`s the resulting GosVec header.
///
/// Used at the binding-call boundary to convert a Gossamer
/// `[T; N]` array literal (or similarly-shaped value) into the
/// `*mut GosVec` shape the binding's C-ABI thunk expects for a
/// `Vec<T>` parameter.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_from_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        let len = len.max(0);
        let n = (len as usize) * (elem_bytes as usize);
        let buf_ptr = if n == 0 || data.is_null() {
            std::ptr::null_mut()
        } else {
            let p = alloc_vec_buffer(n);
            unsafe {
                std::ptr::copy_nonoverlapping(data, p, n);
            }
            p
        };
        // Ledger + initial strong count, symmetric with `alloc_vec_header`.
        crate::c_abi::ledger::vec_inc();
        let v = GosVec {
            len,
            cap: len,
            elem_bytes,
            elem_kind: vec_elem_kind::PRIMITIVE,
            region_flag: 0,
            rc: std::sync::atomic::AtomicU16::new(0),
            ptr: SyncRawPtr::new(buf_ptr),
        };
        vec_set_rc(&v, 1);
        Box::into_raw(Box::new(v))
    })
}

/// Converts a flat 2-level nested array `[Array{T,inner_len}; outer_len]` into
/// a `Vec<*mut GosVec>` where every inner flat array has been promoted to a
/// heap-allocated `GosVec`. Needed when a `[[T]]` literal is coerced at a
/// call site that expects `Vec<Vec<T>>`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_nested_arr_to_vec(
    inner_elem_bytes: i64,
    inner_len: i64,
    raw: *const u8,
    outer_len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // Outer Vec holds pointer-sized elements (*mut GosVec).
        let outer = unsafe { gos_rt_vec_new(8) };
        if raw.is_null() || outer_len <= 0 || inner_len <= 0 || inner_elem_bytes <= 0 {
            return outer;
        }
        let stride = (inner_len as usize) * (inner_elem_bytes as usize);
        for i in 0..(outer_len as usize) {
            let inner_raw = unsafe { raw.add(i * stride) };
            let inner_vec =
                unsafe { gos_rt_vec_from_arr(inner_elem_bytes as u32, inner_raw, inner_len) };
            let inner_ptr_i64 = inner_vec as i64;
            let bytes = inner_ptr_i64.to_ne_bytes();
            unsafe { gos_rt_vec_push(outer, bytes.as_ptr()) };
        }
        outer
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_len(v: *const GosVec) -> i64 {
    ffi_entry!(-1, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).len }
    })
}

/// Typed-i64 wrapper around [`gos_rt_vec_push`]. Spills the value
/// to a stack slot and forwards its address so the byte-erased
/// push helper can `memcpy` it into the vec's storage. Used by the
/// dynamic-count `[value; n]` lowering - passing an i64 directly
/// to the byte-erased helper would otherwise need a per-call-site
/// stack slot in cranelift.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_i64(v: *mut GosVec, value: i64) {
    ffi_entry!((), {
        let bytes = value.to_ne_bytes();
        unsafe { gos_rt_vec_push(v, bytes.as_ptr()) };
    });
}

/// Pushes a 16-byte `i128` element (the by-value `Result`/`Option`
/// representation) by forwarding its address to the byte-erased push. The
/// vec's `elem_bytes` must be 16.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push_i128(v: *mut GosVec, value: i128) {
    ffi_entry!((), {
        let bytes = value.to_ne_bytes();
        unsafe { gos_rt_vec_push(v, bytes.as_ptr()) };
    });
}

/// Reads a 16-byte `i128` element (by-value `Result`/`Option`) at `idx`.
/// Null vec / out-of-range → 0 (matching `gos_rt_vec_get_i64`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_get_i128(v: *const GosVec, idx: i64) -> i128 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        let vec = unsafe { &*v };
        if idx < 0 || idx >= vec.len {
            return 0;
        }
        let p = unsafe { vec.ptr.add((idx as usize) * (vec.elem_bytes as usize)) };
        unsafe { (p as *const i128).read_unaligned() }
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push(v: *mut GosVec, elem: *const u8) {
    ffi_entry!((), {
        if v.is_null() || elem.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        if vec.len == vec.cap {
            // Grow geometrically (cap -> max(4, cap*2)).
            let new_cap = if vec.cap == 0 { 4 } else { vec.cap * 2 };
            let old_bytes = (vec.cap as usize) * (vec.elem_bytes as usize);
            let new_bytes = (new_cap as usize) * (vec.elem_bytes as usize);
            if vec_is_region(vec) {
                // Region-allocated: grow into a fresh region buffer (zeroed)
                // and abandon the old one in the region - it is reclaimed
                // wholesale at `arena_pop`, never individually freed.
                let region_buf = crate::c_abi::rc::region_alloc_bytes(new_bytes);
                if region_buf.is_null() {
                    // No active region (grown after its pop - unusual): fall
                    // back to a global buffer; the region flag stays set so
                    // free still skips it (small bounded leak in this edge).
                    let new_buf = alloc_vec_buffer(new_bytes);
                    if !vec.ptr.is_null() && old_bytes > 0 {
                        unsafe {
                            std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), new_buf, old_bytes);
                        }
                    }
                    vec.ptr = SyncRawPtr::new(new_buf);
                    vec.cap = new_cap;
                } else {
                    if !vec.ptr.is_null() && old_bytes > 0 {
                        unsafe {
                            std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), region_buf, old_bytes);
                        }
                    }
                    vec.ptr = SyncRawPtr::new(region_buf);
                    vec.cap = new_cap;
                }
            } else {
                // Zero-initialised - see `gos_rt_vec_with_capacity`.
                let new_buf = alloc_vec_buffer(new_bytes);
                if !vec.ptr.is_null() && old_bytes > 0 {
                    unsafe {
                        std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), new_buf, old_bytes);
                        // Free the old buffer; every helper that writes
                        // `vec.ptr` allocates through `alloc_vec_buffer`, so
                        // `free_vec_buffer` matches its layout exactly.
                        free_vec_buffer(vec.ptr.as_ptr(), old_bytes);
                    }
                }
                vec.ptr = SyncRawPtr::new(new_buf);
                vec.cap = new_cap;
            }
        }
        // STRING / VEC / MAP elements are pointer-sized and transferred by
        // REFERENCE: the drop pass retains the inbound value at the push site
        // (so the container holds a reference-counted element) and
        // `gos_rt_vec_free` releases each one through its `elem_kind` deep-free.
        // Storing the pointer directly - no per-push clone - lets the
        // compile-time RC own the element exactly once. The previous clone left
        // the caller's original retained-but-never-released (a per-push leak,
        // since the container held the copy, not the original). `gos_rt_str_free`
        // tag-checks each pointer at deep-free, so a stored `.rodata` literal or
        // region string is skipped rather than mis-freed.
        let dst = unsafe { vec.ptr.add((vec.len as usize) * (vec.elem_bytes as usize)) };
        unsafe {
            std::ptr::copy_nonoverlapping(elem, dst, vec.elem_bytes as usize);
        }
        vec.len += 1;
        // A guarded aggregate element shares its copy-blob children with
        // the source slots (which keep their own shares and release them
        // when the source dies); the vec's copy must hold its own.
        if vec.elem_kind == vec_elem_kind::AGGR_GUARDED {
            let meta = vec_elem_meta(v);
            if !meta.is_null() {
                unsafe { crate::c_abi::rc::gos_rt_aggr_retain_children(dst, meta) };
            }
        }
        // Same sharing contract for owned-slot-children vecs: the source
        // slot keeps its share, the vec's copy holds its own.
        if vec.elem_kind == vec_elem_kind::AGGR_OWNED {
            unsafe { vec_retain_slot_children(v, dst) };
        }
    });
}

// Tagged-union encoding for `Result<T, E>` and `Option<T>`: a 2-word
// BY-VALUE `i128` with the discriminant in the low 64 bits and the
// payload in the high 64 bits. Convention: `disc == 0` = Ok / Some,
// `disc == 1` = Err / None - the distinguishing bit pattern dispatch
// reads. Construction is a register pack with zero allocation; the
// payload flows as a normal value (a scalar, or a pointer to a
// heap-copied aggregate) managed by RC like any other binding.

/// Pack `(disc, payload)` into the 2-word Result/Option value.
#[inline]
#[must_use]
pub fn pack_result(disc: i64, payload: i64) -> i128 {
    (((payload as u64 as u128) << 64) | (disc as u64 as u128)) as i128
}

#[inline]
fn result_disc_of(r: i128) -> i64 {
    (r as u128 as u64) as i64
}

#[inline]
fn result_payload_of(r: i128) -> i64 {
    ((r as u128 >> 64) as u64) as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_new(disc: i64, payload: i64) -> i128 {
    pack_result(disc, payload)
}

/// `gos_rt_result_new` variant for f64 payloads - stores the value's
/// `to_bits()` so the symmetric `gos_rt_result_payload_f64` reads back the
/// original f64.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_new_f64(disc: i64, payload: f64) -> i128 {
    pack_result(disc, payload.to_bits() as i64)
}

/// Converts a `[rust-bindings]` `*mut GosVariant` (the binding ABI's
/// `Result` / `Option` wire shape, tag `1` = Ok/Some) into the
/// runtime's packed i128 result (disc `0` = Ok/Some). String payloads
/// arrive as bare NUL-terminated arena bytes and are re-allocated as
/// header'd runtime strings; every other payload word passes through
/// bit-exact (i64/bool/char values, f64 bits, GosVec / nested
/// pointers).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_binding_variant_to_result(p: *const u8) -> i128 {
    ffi_entry!(0i128, {
        if p.is_null() {
            return pack_result(1, 0);
        }
        // GosVariant layout (repr(C) in gossamer-binding):
        // tag i32 | payload_len i32 | payload *mut GosVariantValue.
        let tag = unsafe { *p.cast::<i32>() };
        let payload_len = unsafe { *p.add(4).cast::<i32>() };
        let payload_ptr = unsafe { *p.add(8).cast::<*const u8>() };
        let disc = i64::from(tag != 1);
        if payload_len <= 0 || payload_ptr.is_null() {
            return pack_result(disc, 0);
        }
        // GosVariantValue layout: tag i32 | (pad) | data union at +8.
        // Value tag 4 = string; see `gossamer-binding::native`.
        let value_tag = unsafe { *payload_ptr.cast::<i32>() };
        let word = unsafe { *payload_ptr.add(8).cast::<i64>() };
        let payload = if value_tag == 4 && word != 0 {
            let c = unsafe { std::ffi::CStr::from_ptr(word as *const std::ffi::c_char) };
            super::string::alloc_cstring(c.to_bytes()) as i64
        } else {
            word
        };
        pack_result(disc, payload)
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_disc(r: i128) -> i64 {
    result_disc_of(r)
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_dbg(p: i64) -> i64 {
    eprintln!("[rt] dbg called with raw i64 = {p:#x}");
    p
}

#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_payload(r: i128) -> i64 {
    result_payload_of(r)
}

/// `Result<f64, _>` / `Option<f64>` Ok-payload extractor that reinterprets
/// the stored bits as f64.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_payload_f64(r: i128) -> f64 {
    f64::from_bits(result_payload_of(r) as u64)
}

/// Payload extractor for payloads that are themselves a 2-word
/// by-value enum (`Result<Option<T>, E>`, nested Results, inline
/// user enums). Construction heap-copied the inner 2-word value and
/// stored its address in the payload word; this loads it back by
/// value so the destination local holds `[disc, payload]` directly.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_payload_i128(r: i128) -> i128 {
    let addr = result_payload_of(r);
    if addr == 0 {
        return 0;
    }
    // SAFETY: the payload word of an enum-payload Result/Option is a
    // pointer to the live 16-byte heap copy made at construction
    // (`gos_rt_result_new` aggregate path).
    unsafe {
        let p = addr as usize as *const i64;
        let lo = (*p) as u64 as u128;
        let hi = (*p.add(1)) as u64 as u128;
        ((hi << 64) | lo) as i128
    }
}

/// Renders a single enum payload word for `{:?}` Debug output, matching the
/// VM's Display-style rendering (no string quoting). `kind`: 0=i64, 1=u64,
/// 2=f64 (bit pattern), 3=bool, 4=char, 5=String pointer.
fn debug_payload_string(payload: i64, kind: i64) -> String {
    match kind {
        1 => (payload as u64).to_string(),
        2 => format!("{}", f64::from_bits(payload as u64)),
        3 => if payload != 0 { "true" } else { "false" }.to_string(),
        4 => char::from_u32(payload as u32).map_or_else(String::new, |c| c.to_string()),
        5 => {
            if payload == 0 {
                String::new()
            } else {
                let sptr: *const std::ffi::c_char =
                    std::ptr::with_exposed_provenance(payload as usize);
                unsafe { std::ffi::CStr::from_ptr(sptr) }
                    .to_string_lossy()
                    .into_owned()
            }
        }
        _ => payload.to_string(),
    }
}

/// `{:?}` of an `Option<T>` (the by-value `i128` enum, disc 0 = Some): renders
/// `Some(<payload>)` or `None`, matching the VM. `payload_kind` selects the
/// payload formatter (see `debug_payload_string`).
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_debug_option(opt: i128, payload_kind: i64) -> *mut std::ffi::c_char {
    let s = if result_disc_of(opt) != 0 {
        "None".to_string()
    } else {
        format!(
            "Some({})",
            debug_payload_string(result_payload_of(opt), payload_kind)
        )
    };
    super::string::alloc_cstring(s.as_bytes())
}

/// `{:?}` of a `Result<T, E>` (the by-value `i128` enum, disc 0 = Ok): renders
/// `Ok(<payload>)` or `Err(<payload>)`, matching the VM. `ok_kind` / `err_kind`
/// select the per-arm payload formatter.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_debug_result(
    res: i128,
    ok_kind: i64,
    err_kind: i64,
) -> *mut std::ffi::c_char {
    let payload = result_payload_of(res);
    let s = if result_disc_of(res) == 0 {
        format!("Ok({})", debug_payload_string(payload, ok_kind))
    } else {
        format!("Err({})", debug_payload_string(payload, err_kind))
    };
    super::string::alloc_cstring(s.as_bytes())
}

/// `result.unwrap()` / `option.unwrap()`. Returns the payload on the happy
/// path; panics on Err / None.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_result_unwrap(r: i128) -> i64 {
    ffi_entry!(-1, {
        if result_disc_of(r) != 0 {
            let cs = std::ffi::CString::new("called `Result::unwrap()` on an `Err` value").unwrap();
            unsafe { gos_rt_panic(cs.as_ptr()) };
            return 0;
        }
        result_payload_of(r)
    })
}

/// `result.unwrap_or(default)` / `option.unwrap_or(default)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_unwrap_or(r: i128, default: i64) -> i64 {
    if result_disc_of(r) == 0 {
        result_payload_of(r)
    } else {
        default
    }
}

/// `result.ok()` / `option.ok()`. Returns the payload on Ok/Some, else 0.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_ok(r: i128) -> i64 {
    if result_disc_of(r) == 0 {
        result_payload_of(r)
    } else {
        0
    }
}

/// `result.err()`. Returns the error payload on Err, else 0.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_err(r: i128) -> i64 {
    if result_disc_of(r) == 1 {
        result_payload_of(r)
    } else {
        0
    }
}

/// `result.ok_or(new_err)`. On Ok, returns the receiver unchanged; on Err,
/// returns a new `Err(new_err)`.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_ok_or(r: i128, new_err: i64) -> i128 {
    if result_disc_of(r) == 0 {
        r
    } else {
        pack_result(1, new_err)
    }
}

/// `result.is_ok()` / `option.is_some()`. 1 on Ok/Some, 0 on Err/None.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_is_ok(r: i128) -> i64 {
    i64::from(result_disc_of(r) == 0)
}

/// `result.is_err()` / `option.is_none()`. 1 on Err/None, 0 on Ok/Some.
#[unsafe(no_mangle)]
pub extern "C" fn gos_rt_result_is_err(r: i128) -> i64 {
    i64::from(result_disc_of(r) != 0)
}

/// Maps a `gos_main` return value to a process exit code. A
/// `Result`-returning `main` yields its discriminant (`0` = `Ok`,
/// `1` = `Err`) in the low word; a unit or integer `main` yields its
/// value directly - both are the exit code. Also blocks until every
/// outstanding goroutine has settled so their stdout reaches the user
/// before the process exits.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_main_exit_code(raw: i64) -> i32 {
    ffi_entry!(-1, {
        // Drain goroutines spawned via `go expr` before exiting:
        // `live_goroutines` is incremented at spawn admission, so a
        // fast `go expr; return` main observes its goroutine here, and
        // a body that finished has already written into the buffered
        // stdout the flush below drains. Skips (and does not boot) the
        // scheduler when the program never used it.
        crate::sched_global::drain_goroutines_for_exit();
        // Flush any buffered stdout that workers wrote so it
        // reaches the user before the process exits.
        unsafe { gos_rt_flush_stdout() };
        raw as i32
    })
}
