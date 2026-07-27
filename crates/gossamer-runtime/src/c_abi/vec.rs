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
    /// meta lives in the Vec's versioned owner carrier; `gos_rt_vec_free`
    /// releases each element's guarded children, and `gos_rt_vec_push`
    /// retains them when the element bytes are copied in.
    pub const AGGR_GUARDED: u8 = 5;
    /// Element is a multi-slot struct/tuple stored inline whose slots
    /// embed heap pointers (runtime strings / nested vecs) OWNED by the
    /// vec. The per-vec slot layout lives in the versioned owner carrier
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
    /// Element is a reference-counted heap node pointer (a payload-bearing
    /// user enum, possibly tag-bit-encoded) OWNED by the vec: a push moves
    /// the frame's share in (the MIR treats `gos_rt_vec_push` as consuming,
    /// same as `STRING` elements), `gos_rt_vec_free` releases each element
    /// via `gos_rt_rc_release`, and a storage duplication (clone / slice)
    /// retains each copied element so both storages own their shares. Set
    /// via [`super::vec::gos_rt_vec_mark_rc_elems`], emitted by the MIR
    /// lowering right after constructing a vec of payload-enum elements.
    pub const RC_ENUM: u8 = 8;
    /// A runtime-owned, fixed-width primitive `Vec<Vec<i64>>` payload. The
    /// outer `GosVec` points at a `PackedRows` descriptor instead of an array
    /// of child pointers; indexed access returns one stable row header from
    /// that descriptor. This is intentionally distinct from `VEC` so normal
    /// deep-free and pointer-slot paths can never mistake the descriptor for
    /// a child Vec pointer.
    pub const PACKED_ROWS: u8 = 9;
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
    /// Header flags (was `_reserved[0]`, struct offset 21), a small
    /// bitfield: `VEC_REGION_FLAG` when this header and its buffer live
    /// in an arena slab (so `gos_rt_vec_free` skips them), and
    /// `VEC_SPLIT_FLAG` when `ptr` points at a separately-allocated
    /// buffer rather than the inline one that rides with the header (so
    /// `gos_rt_vec_free` knows to reclaim that buffer on its own). The
    /// two are mutually exclusive: region vecs never split.
    pub region_flag: u8,
    /// Strong refcount of a non-region Vec - an actual `AtomicU16` so its
    /// atomic loads/stores are sound. (A prior `[u8; 3]` reinterpret-cast to
    /// `AtomicU16` tripped Miri's Stacked-Borrows model: a read-only,
    /// single-byte-provenance pointer cannot be retagged for atomic
    /// read-write access.) Same offset (22) and size as the bytes it
    /// replaces, so `ptr` stays at offset 24 and the layout is unchanged.
    pub rc: std::sync::atomic::AtomicU16,
    pub ptr: SyncRawPtr<u8>,
    /// Unique allocation identity, deliberately appended after the legacy
    /// 32-byte prefix whose offsets are part of the native ABI. Heap Vecs get
    /// this directly in the header; region Vecs are bulk-owned and use zero.
    pub generation: u64,
    /// Guarded-aggregate element metadata. Primitive Vecs keep this null, so
    /// their common path carries no separately allocated ownership state.
    pub elem_meta: SyncRawPtr<i64>,
    /// Lazily allocated metadata only for aggregate-owned element layouts.
    /// This remains an owned carrier rather than an address-keyed registry.
    pub owner: SyncRawPtr<VecOwner>,
    /// Incremented by every structural mutation. Appended after the existing
    /// ABI fields so their offsets remain stable. Lazy borrowed iterators
    /// snapshot it independently from allocation identity.
    pub mutation_generation: u64,
}

/// ABI-versioned optional ownership state for aggregate-owned Vec elements.
///
/// Unlike the former address-keyed metadata maps, this allocation is owned by
/// the Vec header and dies with it. Primitive and guarded-aggregate Vecs do
/// not allocate it; their identity and guarded metadata are in [`GosVec`].
#[repr(C)]
pub struct VecOwner {
    abi_version: u16,
    kind: u16,
    destructor: u32,
    slot_children: Option<Box<[VecSlotChild]>>,
}

const ABI_OWNER_VERSION: u16 = 1;
const ABI_OWNER_KIND_VEC: u16 = 1;
const ABI_OWNER_DTOR_VEC: u32 = 1;
static NEXT_VEC_GENERATION: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

fn new_vec_owner() -> SyncRawPtr<VecOwner> {
    let owner = Box::new(VecOwner {
        abi_version: ABI_OWNER_VERSION,
        kind: ABI_OWNER_KIND_VEC,
        destructor: ABI_OWNER_DTOR_VEC,
        slot_children: None,
    });
    let owner = Box::into_raw(owner);
    crate::c_abi::ledger::vec_owner_alloc(
        std::mem::size_of::<VecOwner>(),
        allocator_usable_bytes(owner.cast(), std::mem::size_of::<VecOwner>()),
    );
    SyncRawPtr::new(owner)
}

/// Allocation capacity returned by the allocator that owns runtime Vec
/// storage. The runtime deliberately uses the system allocator under TSan,
/// Miri, fuzzing, and wasm, where a mimalloc query would be invalid; those
/// configurations report the exact requested layout size instead.
#[inline]
fn allocator_usable_bytes(ptr: *const u8, requested: usize) -> usize {
    #[cfg(not(any(tsan, miri, fuzzing, target_arch = "wasm32")))]
    {
        let _ = requested;
        // SAFETY: `ptr` was just returned by the process-wide mimalloc-backed
        // global allocator and remains live for this query.
        unsafe { libmimalloc_sys::mi_usable_size(ptr.cast_mut().cast()) }
    }
    #[cfg(any(tsan, miri, fuzzing, target_arch = "wasm32"))]
    {
        let _ = ptr;
        requested
    }
}

#[inline]
fn vec_owner(v: &GosVec) -> Option<&VecOwner> {
    let owner = v.owner.as_ptr();
    if owner.is_null() {
        return None;
    }
    // SAFETY: a non-region Vec owner is created with its header and reclaimed
    // only after the header's last release.
    let owner = unsafe { &*owner };
    (owner.abi_version == ABI_OWNER_VERSION
        && owner.kind == ABI_OWNER_KIND_VEC
        && owner.destructor == ABI_OWNER_DTOR_VEC)
        .then_some(owner)
}

/// Generation of this Vec allocation, or zero for a region Vec. This is a
/// diagnostic/testing hook; it is stored independently of header address.
#[must_use]
pub fn vec_owner_generation(v: &GosVec) -> u64 {
    v.generation
}

/// Mark a structural mutation of a live Vec header.
#[inline]
pub(crate) fn bump_vec_mutation_generation(v: &mut GosVec) {
    v.mutation_generation = v.mutation_generation.wrapping_add(1);
}

#[inline]
fn vec_owner_mut(v: &mut GosVec) -> Option<&mut VecOwner> {
    let owner = v.owner.as_ptr();
    if owner.is_null() {
        return None;
    }
    // SAFETY: callers hold exclusive access while attaching metadata.
    let owner = unsafe { &mut *owner };
    (owner.abi_version == ABI_OWNER_VERSION
        && owner.kind == ABI_OWNER_KIND_VEC
        && owner.destructor == ABI_OWNER_DTOR_VEC)
        .then_some(owner)
}

/// Returns the lazily-created aggregate-owned metadata carrier. The header
/// holds generation and guarded metadata directly, so this allocation is only
/// paid by Vecs that need a dynamic slot-child layout.
fn ensure_vec_owner(v: &mut GosVec) -> &mut VecOwner {
    if v.owner.is_null() {
        v.owner = new_vec_owner();
    }
    vec_owner_mut(v).expect("fresh Vec owner must use the current ABI")
}

pub(crate) unsafe fn drop_vec_owner(v: &mut GosVec) {
    let owner = v.owner.as_ptr();
    if owner.is_null() {
        return;
    }
    v.owner = SyncRawPtr::NULL;
    // SAFETY: final Vec release owns the carrier exactly once.
    drop(unsafe { Box::from_raw(owner) });
}

/// `region_flag` bit marking a GosVec (and its backing buffer) as
/// arena-region-allocated: both live in region slabs, so `gos_rt_vec_free`
/// must skip them (they are freed wholesale at `arena_pop`).
const VEC_REGION_FLAG: u8 = 1;

/// `region_flag` bit marking a non-region GosVec whose `ptr` points at a
/// separately-allocated element buffer (grown past the inline capacity)
/// rather than the inline buffer that rides with the header. Set on the
/// first grow, or at construction for a capacity larger than the inline
/// buffer holds. `gos_rt_vec_free` reclaims that separate buffer only for
/// a split vec; an inline vec's buffer is freed with the header block.
const VEC_SPLIT_FLAG: u8 = 2;

/// Header flag for a row owned by [`PackedRows`]. Such rows borrow their
/// header and initial payload from the descriptor; freeing an observed row is
/// therefore a no-op and the descriptor releases all rows together.
const VEC_PACKED_ROW_FLAG: u8 = 4;
/// Header was allocated as `Box<GosVec>` without an unused inline buffer.
const VEC_COMPACT_HEADER_FLAG: u8 = 8;

/// Minimum row count at which replacing one allocation per row with a packed
/// descriptor amortises the conversion. The eligibility checks below remain
/// semantic rather than benchmark-specific: any sufficiently large, uniform,
/// primitive nested Vec can use it.
const PACKED_ROWS_MIN_ROWS: i64 = 1024;

/// Runtime storage for a uniform primitive nested Vec. `rows` supplies real
/// `GosVec` headers so every existing read-only Vec ABI consumer continues to
/// see an ordinary row pointer; `data` is one contiguous row-major payload.
/// The descriptor owns both allocations and is reached only through an outer
/// Vec tagged [`vec_elem_kind::PACKED_ROWS`].
pub(crate) struct PackedRows {
    pub(crate) rows: Box<[GosVec]>,
    // Owns the row-major allocation addressed by `rows[*].ptr`. This stays
    // raw so moving the descriptor does not retag and invalidate those row
    // pointers under Miri's Stacked Borrows model.
    data: SyncRawPtr<u64>,
    data_len: usize,
}

impl Drop for PackedRows {
    fn drop(&mut self) {
        if self.data_len == 0 {
            return;
        }
        unsafe {
            let data = std::ptr::slice_from_raw_parts_mut(self.data.as_ptr(), self.data_len);
            drop(Box::from_raw(data));
        }
    }
}

/// Element words held inline, immediately after the [`GosVec`] header, in a
/// single [`InlineVec`] allocation. Six words is the selected default from the
/// audited game/stress matrix; `inline-vec-words-8` remains available for
/// explicit A/B checks and wins when Cargo's `--all-features` also enables the
/// compatibility `inline-vec-words-6` flag. The trailing buffer is private to
/// this runtime allocation, while `GosVec`'s stable native ABI header remains
/// unchanged.
#[cfg(feature = "inline-vec-words-8")]
const INLINE_BUF_WORDS: usize = 8;
#[cfg(not(feature = "inline-vec-words-8"))]
const INLINE_BUF_WORDS: usize = 6;
const INLINE_BUF_BYTES: usize = INLINE_BUF_WORDS * 8;

/// A non-region `GosVec` header physically followed by its initial element
/// buffer, allocated as one `Box<InlineVec>`. `repr(C)` keeps `header` at
/// offset 0 so a `*mut GosVec` and a `*mut InlineVec` share an address and
/// every codegen path that reads header fields at fixed offsets is
/// unaffected. While the element count stays within the inline buffer,
/// `header.ptr` points into `buf` (one allocation, header and first
/// elements in one cache line); a push past the inline capacity moves `ptr`
/// to a separately-allocated buffer and sets [`VEC_SPLIT_FLAG`], after which
/// `buf` is unused space reclaimed with the header.
#[repr(C)]
pub(crate) struct InlineVec {
    pub(crate) header: GosVec,
    buf: [u64; INLINE_BUF_WORDS],
}

/// Inline element capacity for a given element width (how many elements of
/// `elem_bytes` fit in [`INLINE_BUF_BYTES`]). Zero for a zero or oversized
/// element width, which then always uses a separate buffer.
#[inline]
fn inline_cap(elem_bytes: u32) -> i64 {
    if elem_bytes == 0 || elem_bytes as usize > INLINE_BUF_BYTES {
        0
    } else {
        (INLINE_BUF_BYTES / elem_bytes as usize) as i64
    }
}

/// Allocates a non-region `GosVec` as a single `Box<InlineVec>` (header +
/// contiguous inline element buffer). When `cap` fits the inline buffer,
/// `header.ptr` points into it and the vec is not split; otherwise a
/// separate `cap * elem_bytes` buffer is allocated, `header.ptr` points at
/// it, and [`VEC_SPLIT_FLAG`] is set. `len` initialises the header length;
/// the data region is left zeroed for the caller to fill. Increments the
/// vec ledger and sets the strong count to 1, symmetric with
/// [`super::map::gos_rt_vec_free`].
pub(crate) unsafe fn alloc_box_vec(
    elem_bytes: u32,
    elem_kind: u8,
    cap: i64,
    len: i64,
) -> *mut GosVec {
    let cap = cap.max(len).max(0);
    let icap = inline_cap(elem_bytes);
    let (init_ptr, real_cap, flag) = if cap <= icap {
        (SyncRawPtr::NULL, icap, 0u8)
    } else {
        let bytes = checked_buffer_bytes(cap as usize, elem_bytes as usize);
        (
            SyncRawPtr::new(alloc_vec_buffer(bytes)),
            cap,
            VEC_SPLIT_FLAG,
        )
    };
    crate::c_abi::ledger::vec_inc();
    if flag != 0 {
        let boxed = Box::new(GosVec {
            len,
            cap: real_cap,
            elem_bytes,
            elem_kind,
            region_flag: flag | VEC_COMPACT_HEADER_FLAG,
            rc: std::sync::atomic::AtomicU16::new(1),
            ptr: init_ptr,
            generation: NEXT_VEC_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            mutation_generation: 0,
            elem_meta: SyncRawPtr::NULL,
            owner: SyncRawPtr::NULL,
        });
        let boxed_ptr = Box::into_raw(boxed);
        crate::c_abi::ledger::vec_inline_alloc(
            std::mem::size_of::<GosVec>(),
            allocator_usable_bytes(boxed_ptr.cast(), std::mem::size_of::<GosVec>()),
        );
        crate::c_abi::ledger::vec_split_alloc(
            checked_buffer_bytes(cap as usize, elem_bytes as usize),
            allocator_usable_bytes(
                init_ptr.as_const_ptr(),
                checked_buffer_bytes(cap as usize, elem_bytes as usize),
            ),
        );
        return boxed_ptr;
    }
    let boxed = Box::new(InlineVec {
        header: GosVec {
            len,
            cap: real_cap,
            elem_bytes,
            elem_kind,
            region_flag: flag,
            rc: std::sync::atomic::AtomicU16::new(1),
            ptr: init_ptr,
            generation: NEXT_VEC_GENERATION.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
            mutation_generation: 0,
            elem_meta: SyncRawPtr::NULL,
            owner: SyncRawPtr::NULL,
        },
        buf: [0u64; INLINE_BUF_WORDS],
    });
    let boxed_ptr = Box::into_raw(boxed);
    crate::c_abi::ledger::vec_inline_alloc(
        std::mem::size_of::<InlineVec>(),
        allocator_usable_bytes(boxed_ptr.cast(), std::mem::size_of::<InlineVec>()),
    );
    if flag != 0 {
        crate::c_abi::ledger::vec_split_alloc(
            checked_buffer_bytes(cap as usize, elem_bytes as usize),
            allocator_usable_bytes(
                init_ptr.as_const_ptr(),
                checked_buffer_bytes(cap as usize, elem_bytes as usize),
            ),
        );
    }
    if flag == 0 {
        // Inline: point `header.ptr` at this block's own contiguous buffer.
        // The self-pointer is derived from the raw allocation after
        // `into_raw`, not from a `&mut boxed.buf` borrow taken before it, so
        // its provenance spans the whole block and stays live across every
        // later `&mut *boxed_ptr` reborrow of the header. A borrow taken
        // before `into_raw` is narrower than the allocation and would be
        // invalidated when `into_raw` reasserts uniqueness over it.
        unsafe {
            let bufptr = (&raw mut (*boxed_ptr).buf).cast::<u8>();
            (*boxed_ptr).header.ptr = SyncRawPtr::new(bufptr);
        }
    }
    boxed_ptr.cast::<GosVec>()
}

/// True when this non-region GosVec's `ptr` is a separately-allocated
/// buffer (grown past the inline capacity) rather than the inline buffer
/// carried in the [`InlineVec`] header block. `gos_rt_vec_free` reclaims
/// that separate buffer only for a split vec.
#[inline]
pub(crate) fn vec_is_split(v: &GosVec) -> bool {
    v.region_flag & VEC_SPLIT_FLAG != 0
}

#[inline]
pub(crate) fn vec_has_compact_header(v: &GosVec) -> bool {
    v.region_flag & VEC_COMPACT_HEADER_FLAG != 0
}

pub(crate) struct OwnedByteBuffer {
    ptr: SyncRawPtr<u8>,
    len: u32,
    cap: u32,
}

impl OwnedByteBuffer {
    pub(crate) fn as_slice(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.ptr.as_ptr(), self.len as usize) }
    }
}

impl Drop for OwnedByteBuffer {
    fn drop(&mut self) {
        if self.cap != 0 {
            unsafe { free_vec_buffer(self.ptr.as_ptr(), self.cap as usize) };
        }
    }
}

pub(crate) unsafe fn take_owned_byte_buffer(v: *mut GosVec) -> OwnedByteBuffer {
    let vec = unsafe { &mut *v };
    if vec.elem_bytes == 1
        && vec.elem_kind == vec_elem_kind::PRIMITIVE
        && vec_is_split(vec)
        && !vec_is_region(vec)
        && vec_rc(vec) <= 3
        && let (Ok(len), Ok(cap)) = (u32::try_from(vec.len), u32::try_from(vec.cap))
    {
        let owned = OwnedByteBuffer {
            ptr: vec.ptr,
            len,
            cap,
        };
        vec.ptr = SyncRawPtr::NULL;
        vec.len = 0;
        vec.cap = 0;
        vec.region_flag &= !VEC_SPLIT_FLAG;
        vec_set_rc(vec, 1);
        unsafe { crate::c_abi::map::gos_rt_vec_free(v) };
        return owned;
    }

    let bytes = if vec.elem_bytes == 1 && vec.len > 0 && !vec.ptr.is_null() {
        unsafe { std::slice::from_raw_parts(vec.ptr.as_ptr(), vec.len as usize) }
    } else {
        &[]
    };
    let len = u32::try_from(bytes.len()).unwrap_or_else(|_| {
        unsafe { gos_rt_panic(c"byte vector is too large to store".as_ptr()) };
        0
    });
    let ptr = SyncRawPtr::new(alloc_vec_buffer(bytes.len()));
    if !bytes.is_empty() {
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), ptr.as_ptr(), bytes.len()) };
    }
    unsafe { crate::c_abi::map::gos_rt_vec_free(v) };
    OwnedByteBuffer { ptr, len, cap: len }
}

/// Replaces a large uniform `Vec<Vec<i64>>` with contiguous fixed-width row
/// storage. The conversion is deliberately conservative and is performed at
/// the first indexed read, after construction is complete: every row must be
/// uniquely owned, primitive, eight-byte wide, and have the same length.
/// Anything that does not meet those conditions remains an ordinary Vec.
///
/// The returned descriptor keeps stable `GosVec` row headers, so read-only
/// indexing and iteration need no new source-level type or ABI. A later row
/// growth detaches that row through the existing split-buffer path and is
/// released when the descriptor dies.
pub(crate) unsafe fn try_pack_primitive_rows(outer: *mut GosVec) -> bool {
    if outer.is_null() {
        return false;
    }
    let outer_ref = unsafe { &mut *outer };
    if outer_ref.elem_kind != vec_elem_kind::VEC
        || outer_ref.len < PACKED_ROWS_MIN_ROWS
        || vec_is_region(outer_ref)
    {
        return false;
    }
    let row_count = outer_ref.len as usize;
    let mut width: Option<usize> = None;
    for i in 0..row_count {
        let slot = unsafe {
            outer_ref
                .ptr
                .as_ptr()
                .add(i * 8)
                .cast::<usize>()
                .read_unaligned()
        };
        if slot == 0 {
            return false;
        }
        let row = unsafe { &*(std::ptr::with_exposed_provenance::<GosVec>(slot)) };
        if row.elem_kind != vec_elem_kind::PRIMITIVE
            || row.elem_bytes != 8
            || row.len < 0
            || row.rc.load(std::sync::atomic::Ordering::Acquire) != 1
        {
            return false;
        }
        match width {
            Some(expected) if expected != row.len as usize => return false,
            None => width = Some(row.len as usize),
            _ => {}
        }
    }
    let width = width.unwrap_or(0);
    let Some(words) = row_count.checked_mul(width) else {
        return false;
    };
    let mut data = std::mem::ManuallyDrop::new(vec![0u64; words].into_boxed_slice());
    let data_len = data.len();
    let data_base = data.as_mut_ptr();
    let mut rows = Vec::with_capacity(row_count);
    for i in 0..row_count {
        let slot = unsafe {
            outer_ref
                .ptr
                .as_ptr()
                .add(i * 8)
                .cast::<usize>()
                .read_unaligned()
        };
        let row_ptr = std::ptr::with_exposed_provenance_mut::<GosVec>(slot);
        let row = unsafe { &*row_ptr };
        if width != 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(
                    row.ptr.as_ptr(),
                    data_base.add(i * width).cast::<u8>(),
                    width * std::mem::size_of::<u64>(),
                );
            }
        }
        let data_ptr = if width == 0 {
            std::ptr::NonNull::<u64>::dangling().as_ptr().cast::<u8>()
        } else {
            unsafe { data_base.add(i * width).cast::<u8>() }
        };
        rows.push(GosVec {
            len: width as i64,
            cap: width as i64,
            elem_bytes: 8,
            elem_kind: vec_elem_kind::PRIMITIVE,
            region_flag: VEC_PACKED_ROW_FLAG,
            rc: std::sync::atomic::AtomicU16::new(1),
            ptr: SyncRawPtr::new(data_ptr),
            generation: 0,
            mutation_generation: 0,
            elem_meta: SyncRawPtr::NULL,
            owner: SyncRawPtr::NULL,
        });
        // The outer Vec owns the sole share of every eligible row. Release
        // it only after copying the primitive payload into the descriptor.
        unsafe { crate::c_abi::map::gos_rt_vec_free(row_ptr) };
    }
    if vec_is_split(outer_ref) {
        let bytes = checked_buffer_bytes(outer_ref.cap as usize, outer_ref.elem_bytes as usize);
        unsafe { free_vec_buffer(outer_ref.ptr.as_ptr(), bytes) };
    }
    let packed = Box::new(PackedRows {
        rows: rows.into_boxed_slice(),
        data: SyncRawPtr::new(data_base),
        data_len,
    });
    outer_ref.ptr = SyncRawPtr::new(Box::into_raw(packed).cast::<u8>());
    outer_ref.cap = outer_ref.len;
    outer_ref.elem_kind = vec_elem_kind::PACKED_ROWS;
    outer_ref.region_flag &= !VEC_SPLIT_FLAG;
    crate::c_abi::ledger::vec_packed_conversion(row_count, words * std::mem::size_of::<u64>());
    true
}

/// Returns the stable row header for a packed outer Vec.
pub(crate) unsafe fn packed_row_at(outer: *const GosVec, idx: i64) -> *mut u8 {
    if outer.is_null() || idx < 0 {
        return std::ptr::null_mut();
    }
    let outer = unsafe { &*outer };
    if outer.elem_kind != vec_elem_kind::PACKED_ROWS || idx >= outer.len || outer.ptr.is_null() {
        return std::ptr::null_mut();
    }
    let packed = unsafe { &*outer.ptr.as_ptr().cast::<PackedRows>() };
    packed
        .rows
        .get(idx as usize)
        .map_or(std::ptr::null_mut(), |row| {
            std::ptr::from_ref(row).cast_mut().cast::<u8>()
        })
}

/// Releases a packed descriptor and any row which detached to a normal split
/// buffer after a mutation. Initial row data is owned by `PackedRows::data`.
pub(crate) unsafe fn free_packed_rows(outer: &GosVec) {
    if outer.ptr.is_null() {
        return;
    }
    let packed = unsafe { Box::from_raw(outer.ptr.as_ptr().cast::<PackedRows>()) };
    for row in &packed.rows {
        if vec_is_split(row) {
            let bytes = checked_buffer_bytes(row.cap as usize, row.elem_bytes as usize);
            unsafe { free_vec_buffer(row.ptr.as_ptr(), bytes) };
        }
    }
    drop(packed);
}

/// Allocate a GosVec header from the active region if one is open (so it is
/// freed wholesale at pop and `gos_rt_vec_free` skips it), else from the
/// global allocator as a single `Box<InlineVec>` via [`alloc_box_vec`].
unsafe fn alloc_vec_header(mut v: GosVec) -> *mut GosVec {
    let p = crate::c_abi::rc::region_alloc_bytes(std::mem::size_of::<GosVec>());
    if p.is_null() {
        unsafe { alloc_box_vec(v.elem_bytes, v.elem_kind, v.cap, v.len) }
    } else {
        crate::c_abi::ledger::vec_region_alloc(std::mem::size_of::<GosVec>());
        v.region_flag = VEC_REGION_FLAG;
        let hp = p.cast::<GosVec>();
        unsafe { std::ptr::write(hp, v) };
        hp
    }
}

/// Constructs a Vec with a requested capacity while preserving the active
/// region allocation path.  `alloc_box_vec` is deliberately the non-region
/// implementation (it uses a boxed inline header), so calling it directly
/// from `with_capacity` bypasses the arena even when the caller's loop is
/// regioned.  Start from the ordinary region-aware empty constructor, then
/// reserve the requested capacity through the shared growth path.
unsafe fn alloc_vec_with_capacity(elem_bytes: u32, elem_kind: u8, cap: i64) -> *mut GosVec {
    if cap < 0 {
        unsafe { gos_rt_panic(c"Vec::with_capacity: capacity must be non-negative".as_ptr()) };
    }
    if !crate::c_abi::rc::region_is_active() {
        return unsafe { alloc_box_vec(elem_bytes, elem_kind, cap, 0) };
    }
    let v = unsafe {
        alloc_vec_header(GosVec {
            len: 0,
            cap: 0,
            elem_bytes,
            elem_kind,
            region_flag: 0,
            rc: std::sync::atomic::AtomicU16::new(0),
            ptr: SyncRawPtr::NULL,
            generation: 0,
            mutation_generation: 0,
            elem_meta: SyncRawPtr::NULL,
            owner: SyncRawPtr::NULL,
        })
    };
    if !v.is_null() && cap > 0 {
        unsafe { vec_reserve_to(&mut *v, cap, true) };
    }
    v
}

pub(crate) fn vec_elem_meta(v: *const GosVec) -> *const i64 {
    if v.is_null() {
        return std::ptr::null();
    }
    // SAFETY: callers only query live Vec headers.
    // SAFETY: callers only query live Vec headers.
    unsafe { (*v).elem_meta.as_const_ptr() }
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

/// Slot-children layout of an `AGGR_OWNED` vec, or `None` for any
/// other vec.
pub fn vec_slot_children(v: &GosVec) -> Option<&[VecSlotChild]> {
    vec_owner(v).and_then(|owner| owner.slot_children.as_deref())
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
    ensure_vec_owner(vec).slot_children = Some(children.into());
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
    let Some(children) = vec_slot_children(unsafe { &*v }) else {
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
/// `&Vec<T>` parameter fed a `&array`. Word-sized element layouts point
/// directly at the source array so mutable slice writes alias the caller's
/// storage. Fixed arrays store one word per element, so sub-word element
/// views use an 8-byte stride rather than the packed Vec stride.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_borrow_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if len < 0 {
            unsafe { gos_rt_panic(c"Vec length must be non-negative".as_ptr()) };
        }
        let view_elem_bytes = elem_bytes.max(8);
        let v = unsafe { alloc_box_vec(view_elem_bytes, vec_elem_kind::PRIMITIVE, 0, 0) };
        unsafe {
            (*v).len = len;
            (*v).cap = len;
            (*v).ptr = SyncRawPtr::new(data.cast_mut());
        }
        v
    })
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

/// Increment a `GosVec`'s strong count by one. The `String`-field
/// counterpart `gos_rt_str_retain` bumps a shared string; this bumps a
/// shared `Vec`/`[T]` reached as a by-value struct field, so a struct copy
/// (`let b = a`) gives both owners a share and each `gos_rt_vec_free` at
/// their deaths balances. Null-safe; region and count-0 headers handled by
/// `vec_retain_header`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_retain(v: *mut GosVec) {
    ffi_entry!((), {
        unsafe { vec_retain_header(v) };
    });
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
/// vecs own their shares. Covers `STRING`, `VEC`, `RC_ENUM` and
/// `AGGR_OWNED` element kinds; `AGGR_GUARDED` keeps its dedicated
/// copy-blob path at the existing call sites. No-op for primitive /
/// region / null vecs.
pub(crate) unsafe fn vec_share_owned_elements(src: *const GosVec, out: *mut GosVec) {
    if src.is_null() || out.is_null() {
        return;
    }
    let s = unsafe { &*src };
    match s.elem_kind {
        vec_elem_kind::STRING | vec_elem_kind::VEC | vec_elem_kind::RC_ENUM
            if s.elem_bytes == 8 =>
        {
            // This branch mutates the output tag but never calls another
            // helper that borrows the output header, so one exclusive borrow
            // may cover the whole operation.
            let o = unsafe { &mut *out };
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
                    vec_elem_kind::RC_ENUM => unsafe {
                        crate::c_abi::rc::gos_rt_rc_retain(child);
                    },
                    _ => unsafe { vec_retain_header(child.cast()) },
                }
            }
        }
        vec_elem_kind::AGGR_OWNED => {
            if let Some(children) = vec_slot_children(s) {
                // `vec_set_slot_children` takes its own exclusive header
                // borrow to create the lazy metadata carrier. Do not retain a
                // prior `&mut GosVec` across that call: doing so violates
                // Stacked Borrows when the clone path later reads its fields.
                vec_set_slot_children(out, children);
                let o = unsafe { &*out };
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
                let o = unsafe { &mut *out };
                o.elem_kind = vec_elem_kind::PRIMITIVE;
            }
        }
        _ => {}
    }
}

/// Tags `v` as owning reference-counted enum-node elements
/// ([`vec_elem_kind::RC_ENUM`]): `gos_rt_vec_free` releases each element
/// and storage duplication retains each copy. Emitted by the MIR
/// lowering right after constructing a vec whose element type is a
/// payload-bearing user enum. Only a `PRIMITIVE` vec is re-tagged, so a
/// meta set by a materializer shim is never clobbered. No-op for null /
/// region vecs (region storage is freed wholesale and never walked).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_mark_rc_elems(v: *mut GosVec) {
    if v.is_null() {
        return;
    }
    let vec = unsafe { &mut *v };
    if vec_is_region(vec) || vec.elem_kind != vec_elem_kind::PRIMITIVE || vec.elem_bytes != 8 {
        return;
    }
    vec.elem_kind = vec_elem_kind::RC_ENUM;
}

/// Tags `v` as owning nested-vec elements ([`vec_elem_kind::VEC`]):
/// `gos_rt_vec_free` releases each element vec's share and storage
/// duplication (clone / slice / filter) retains each copy. Emitted by
/// the MIR lowering right after constructing a vec whose element type
/// is itself `Vec`/`[T]`; the pushes minted the container's shares.
/// Only a `PRIMITIVE` vec is re-tagged; no-op for null / region vecs.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_mark_vec_elems(v: *mut GosVec) {
    if v.is_null() {
        return;
    }
    let vec = unsafe { &mut *v };
    if vec_is_region(vec) || vec.elem_kind != vec_elem_kind::PRIMITIVE || vec.elem_bytes != 8 {
        return;
    }
    vec.elem_kind = vec_elem_kind::VEC;
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
    vec.elem_meta = SyncRawPtr::from_const(meta);
}

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
    let vec = unsafe { &mut *v };
    if !vec_is_region(vec) {
        vec.elem_kind = vec_elem_kind::AGGR_OWNED;
        ensure_vec_owner(vec).slot_children = Some(children.into_boxed_slice());
    }
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
    v.region_flag & VEC_REGION_FLAG != 0
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
                generation: 0,
                mutation_generation: 0,
                elem_meta: SyncRawPtr::NULL,
                owner: SyncRawPtr::NULL,
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
                generation: 0,
                mutation_generation: 0,
                elem_meta: SyncRawPtr::NULL,
                owner: SyncRawPtr::NULL,
            })
        }
    })
}

/// Allocates an uninitialised `bytes`-byte vec element buffer with 8-byte
/// alignment. Vec slots hold 8-byte words (`i64` / pointer), so the
/// buffer must be word-aligned for the slot accesses across the
/// runtime to be sound; a `Vec<u8>` (align 1) only happens to work
/// because the system allocator over-aligns. Backed by a leaked
/// `Vec<u64>`; free with [`free_vec_buffer`] passing the same `bytes`.
///
/// Only slots below `GosVec.len` are readable. Spare capacity is never read
/// before `push` / copy initialises it, so reserving a large vector does not
/// need to touch every page up front.
pub(crate) fn alloc_vec_buffer(bytes: usize) -> *mut u8 {
    let words = bytes.div_ceil(8).max(1);
    let mut buf: Vec<u64> = Vec::with_capacity(words);
    let ptr = buf.as_mut_ptr().cast::<u8>();
    std::mem::forget(buf);
    ptr
}

/// Byte size of `count` elements of `elem_bytes` each. A Vec whose
/// byte size overflows the address space cannot be allocated: the
/// wrapping product would size a short buffer while the header still
/// records the oversized `cap`, so subsequent pushes write past the
/// allocation. Matching the VM's `capacity overflow` panic keeps the
/// three tiers identical instead of corrupting the heap.
#[inline]
fn checked_buffer_bytes(count: usize, elem_bytes: usize) -> usize {
    count.checked_mul(elem_bytes).unwrap_or_else(|| {
        let cs = std::ffi::CString::new("capacity overflow").unwrap_or_default();
        // SAFETY: `cs` is a valid NUL-terminated C string for the call's
        // duration; `gos_rt_panic` reads it and does not retain the pointer.
        // It exits the process (main goroutine) or unwinds (spawned
        // goroutine); this arm never returns.
        unsafe { gos_rt_panic(cs.as_ptr()) };
        0
    })
}

/// Frees a buffer from [`alloc_vec_buffer`]. `bytes` must equal the
/// value passed at allocation; every GosVec buffer is sized
/// `cap * elem_bytes`, stable across the buffer's life.
pub(crate) unsafe fn free_vec_buffer(ptr: *mut u8, bytes: usize) {
    let words = bytes.div_ceil(8).max(1);
    // SAFETY: `ptr` came from `alloc_vec_buffer(bytes)`, so the same
    // capacity reconstructs its `Vec<u64>` allocation exactly. Length is zero
    // because spare capacity may be uninitialised.
    drop(unsafe { Vec::<u64>::from_raw_parts(ptr.cast::<u64>(), 0, words) });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_with_capacity(elem_bytes: u32, cap: i64) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        // Header + reserved element buffer in one `Box<InlineVec>` (or a
        // separate buffer for a capacity larger than the inline slot). Only
        // initialized slots below len are readable; spare split capacity is
        // intentionally not zero-filled.
        unsafe { alloc_vec_with_capacity(elem_bytes, vec_elem_kind::PRIMITIVE, cap) }
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
        unsafe { alloc_vec_with_capacity(elem_bytes, kind, cap) }
    })
}

/// Builds a fresh `*mut GosVec` from a stack/heap array.
/// `Box::into_raw`s the resulting GosVec header.
///
/// Used at the binding-call boundary to convert a Gossamer
/// `[T; N]` array literal (or similarly-shaped value) into the
/// `*mut GosVec` shape the binding's C-ABI thunk expects for a
/// `Vec<T>` parameter.
///
/// The source is the inline `[T; N]` layout - one 8-byte slot per
/// element - so a sub-word element (`bool` / `u8`, canonical stride 1)
/// repacks from each slot's low byte; a flat `len * elem_bytes`
/// memcpy would read the first slots' spare bytes as elements.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_from_arr(
    elem_bytes: u32,
    data: *const u8,
    len: i64,
) -> *mut GosVec {
    ffi_entry!(std::ptr::null_mut(), {
        if len < 0 {
            unsafe { gos_rt_panic(c"Vec length must be non-negative".as_ptr()) };
        }
        let n = checked_buffer_bytes(len as usize, elem_bytes as usize);
        // Header + element buffer in one `Box<InlineVec>` (inline for a
        // small array, else a separate buffer); `ptr` lands at the data
        // region either way, so the copy target is uniform.
        let v = unsafe { alloc_box_vec(elem_bytes, vec_elem_kind::PRIMITIVE, len, len) };
        if n > 0 && !data.is_null() {
            let eb = elem_bytes as usize;
            if eb < 8 {
                for i in 0..(len as usize) {
                    unsafe {
                        std::ptr::copy_nonoverlapping(
                            data.add(i * 8),
                            (*v).ptr.as_ptr().add(i * eb),
                            eb,
                        );
                    }
                }
            } else {
                unsafe { std::ptr::copy_nonoverlapping(data, (*v).ptr.as_ptr(), n) };
            }
        }
        v
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
        // Rows use the inline word-per-slot layout (`inner_len` 8-byte
        // slots each), independent of the element's canonical stride;
        // `gos_rt_vec_from_arr` repacks each row's sub-word elements.
        let stride = checked_buffer_bytes(inner_len as usize, 8);
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

/// Returns the total element capacity of `v` without changing its allocation.
///
/// A null Vec is the canonical empty representation and therefore reports
/// zero, matching [`gos_rt_vec_len`].  Keeping this query in the runtime
/// rather than exposing the header layout lets the ABI retain freedom to
/// change the backing representation while callers continue to plan capacity
/// explicitly.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_capacity(v: *const GosVec) -> i64 {
    ffi_entry!(0, {
        if v.is_null() {
            return 0;
        }
        unsafe { (*v).cap.max(0) }
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

fn next_geometric_cap(old_cap: i64, min_cap: i64) -> i64 {
    let mut cap = if old_cap <= 0 { 4 } else { old_cap };
    while cap < min_cap {
        cap = cap
            .checked_mul(2)
            .filter(|next| *next > cap)
            .unwrap_or(min_cap);
    }
    cap
}

unsafe fn vec_reserve_to(vec: &mut GosVec, min_cap: i64, exact: bool) {
    let min_cap = min_cap.max(vec.len).max(0);
    if min_cap <= vec.cap {
        return;
    }
    let new_cap = if exact {
        min_cap
    } else {
        next_geometric_cap(vec.cap, min_cap)
    };
    let old_bytes = checked_buffer_bytes(vec.cap as usize, vec.elem_bytes as usize);
    let new_bytes = checked_buffer_bytes(new_cap as usize, vec.elem_bytes as usize);
    if vec_is_region(vec) {
        // Region-allocated vecs grow into a fresh region buffer and leave the
        // old one to the enclosing region's wholesale reclamation.
        let region_buf = crate::c_abi::rc::region_alloc_bytes(new_bytes);
        let new_buf = if region_buf.is_null() {
            let new_buf = alloc_vec_buffer(new_bytes);
            crate::c_abi::ledger::vec_split_alloc(
                new_bytes,
                allocator_usable_bytes(new_buf, new_bytes),
            );
            new_buf
        } else {
            crate::c_abi::ledger::vec_region_alloc(new_bytes);
            region_buf
        };
        if !vec.ptr.is_null() && old_bytes > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), new_buf, old_bytes);
            }
        }
        vec.ptr = SyncRawPtr::new(new_buf);
        vec.cap = new_cap;
        return;
    }

    // Spare split capacity is intentionally uninitialised; only the old live
    // slots copied below are readable.
    let new_buf = alloc_vec_buffer(new_bytes);
    crate::c_abi::ledger::vec_split_alloc(new_bytes, allocator_usable_bytes(new_buf, new_bytes));
    let was_split = vec.region_flag & VEC_SPLIT_FLAG != 0;
    if !vec.ptr.is_null() && old_bytes > 0 {
        unsafe {
            std::ptr::copy_nonoverlapping(vec.ptr.as_ptr(), new_buf, old_bytes);
        }
        if was_split {
            // The old buffer was a standalone `alloc_vec_buffer` block.
            unsafe { free_vec_buffer(vec.ptr.as_ptr(), old_bytes) };
        }
    }
    vec.ptr = SyncRawPtr::new(new_buf);
    vec.cap = new_cap;
    vec.region_flag |= VEC_SPLIT_FLAG;
}

/// Ensures `v.capacity() >= min_cap`, growing geometrically when needed.
/// The value is a total capacity, not an additional element count.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_reserve_at_least(v: *mut GosVec, min_cap: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        bump_vec_mutation_generation(vec);
        unsafe { vec_reserve_to(vec, min_cap, false) };
    });
}

/// Ensures `v.capacity() >= cap` without geometric over-allocation.
/// Existing larger capacity is preserved; this function never shrinks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_reserve_exact(v: *mut GosVec, cap: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        bump_vec_mutation_generation(vec);
        unsafe { vec_reserve_to(vec, cap, true) };
    });
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_push(v: *mut GosVec, elem: *const u8) {
    ffi_entry!((), {
        if v.is_null() || elem.is_null() {
            return;
        }
        let vec = unsafe { &mut *v };
        bump_vec_mutation_generation(vec);
        if vec.len == vec.cap {
            // Grow geometrically (cap -> max(4, cap*2)).
            unsafe { vec_reserve_to(vec, vec.len.saturating_add(1), false) };
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

unsafe fn vec_release_elem_at(v: *mut GosVec, idx: i64) {
    if v.is_null() || idx < 0 {
        return;
    }
    let vec = unsafe { &*v };
    if vec.ptr.is_null() || idx >= vec.len {
        return;
    }
    let stride = vec.elem_bytes as usize;
    if stride == 0 {
        return;
    }
    let slot = unsafe { vec.ptr.add((idx as usize) * stride) };
    if vec.elem_kind == vec_elem_kind::AGGR_GUARDED {
        let meta = vec_elem_meta(vec);
        if !meta.is_null() {
            unsafe { crate::c_abi::rc::gos_rt_aggr_release_children(slot, meta) };
        }
        return;
    }
    if vec.elem_kind == vec_elem_kind::AGGR_OWNED {
        if let Some(children) = vec_slot_children(vec) {
            unsafe {
                visit_slot_children(slot, children, |child, kind| match kind {
                    vec_elem_kind::STRING => crate::c_abi::string::gos_rt_str_free(child.cast()),
                    vec_elem_kind::VEC => crate::c_abi::map::gos_rt_vec_free(child.cast()),
                    vec_elem_kind::RC_NODE => crate::c_abi::rc::gos_rt_rc_release(child),
                    _ => {}
                });
            }
        }
        return;
    }
    if vec.elem_bytes as usize != 8 {
        return;
    }
    let raw = unsafe { slot.cast::<usize>().read_unaligned() };
    if raw == 0 {
        return;
    }
    let ptr: *mut u8 = std::ptr::with_exposed_provenance_mut(raw);
    unsafe {
        match vec.elem_kind {
            vec_elem_kind::STRING => crate::c_abi::string::gos_rt_str_free(ptr.cast()),
            vec_elem_kind::VEC => crate::c_abi::map::gos_rt_vec_free(ptr.cast()),
            vec_elem_kind::MAP => crate::c_abi::map::gos_rt_map_free(ptr.cast()),
            vec_elem_kind::RC_ENUM => crate::c_abi::rc::gos_rt_rc_release(ptr),
            _ => {}
        }
    }
}

unsafe fn vec_retain_elem_at_for_copy(v: *const GosVec, idx: i64) -> bool {
    if v.is_null() || idx < 0 {
        return false;
    }
    let vec = unsafe { &*v };
    if vec.ptr.is_null() || idx >= vec.len {
        return false;
    }
    let stride = vec.elem_bytes as usize;
    if stride == 0 {
        return false;
    }
    let slot = unsafe { vec.ptr.add((idx as usize) * stride) };
    if matches!(
        vec.elem_kind,
        vec_elem_kind::PRIMITIVE | vec_elem_kind::AGGR_GUARDED | vec_elem_kind::AGGR_OWNED
    ) {
        return true;
    }
    if vec.elem_bytes as usize != 8 {
        return false;
    }
    let raw = unsafe { slot.cast::<usize>().read_unaligned() };
    if raw == 0 {
        return true;
    }
    let ptr: *mut u8 = std::ptr::with_exposed_provenance_mut(raw);
    unsafe {
        match vec.elem_kind {
            vec_elem_kind::STRING => crate::c_abi::string::gos_rt_str_retain(ptr.cast()),
            vec_elem_kind::VEC => vec_retain_header(ptr.cast()),
            vec_elem_kind::RC_ENUM => crate::c_abi::rc::gos_rt_rc_retain(ptr),
            // GosMap and GosError do not currently have a retain protocol.
            vec_elem_kind::MAP | vec_elem_kind::ERROR => return false,
            _ => {}
        }
    }
    true
}

/// `v.clear()` - drop all live elements and keep capacity.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_clear(v: *mut GosVec) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        let len = unsafe { (*v).len.max(0) };
        unsafe { bump_vec_mutation_generation(&mut *v) };
        for idx in 0..len {
            unsafe { vec_release_elem_at(v, idx) };
        }
        unsafe {
            (*v).len = 0;
        }
    });
}

/// `v.truncate(n)` - drop elements at indices `n..len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_truncate(v: *mut GosVec, len: i64) {
    ffi_entry!((), {
        if v.is_null() {
            return;
        }
        if len < 0 {
            unsafe { gos_rt_panic(c"truncate: length must be non-negative".as_ptr()) };
        }
        let new_len = len;
        let old_len = unsafe { (*v).len.max(0) };
        if new_len >= old_len {
            return;
        }
        unsafe { bump_vec_mutation_generation(&mut *v) };
        for idx in new_len..old_len {
            unsafe { vec_release_elem_at(v, idx) };
        }
        unsafe {
            (*v).len = new_len;
        }
    });
}

/// `v.extend(xs)` / `v.extend_from_slice(xs)` / `v.append(xs)`.
///
/// Copies elements from `src` into `dst`. Pointer-bearing elements are retained
/// when the runtime has a retain protocol; map/error element vecs are left
/// unchanged rather than shallow-copied unsafely.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_vec_extend(dst: *mut GosVec, src: *const GosVec) {
    ffi_entry!((), {
        if dst.is_null() || src.is_null() {
            return;
        }
        if std::ptr::addr_eq(dst.cast_const(), src) {
            let snapshot = unsafe { crate::c_abi::gos_rt_vec_clone(src) };
            if snapshot.is_null() {
                return;
            }
            unsafe { gos_rt_vec_extend(dst, snapshot) };
            unsafe { crate::c_abi::map::gos_rt_vec_free(snapshot) };
            return;
        }
        let src_ref = unsafe { &*src };
        let dst_ref = unsafe { &*dst };
        if src_ref.elem_bytes != dst_ref.elem_bytes || src_ref.elem_kind != dst_ref.elem_kind {
            return;
        }
        if matches!(src_ref.elem_kind, vec_elem_kind::MAP | vec_elem_kind::ERROR) {
            return;
        }
        let len = src_ref.len.max(0);
        let stride = src_ref.elem_bytes as usize;
        if stride == 0 || src_ref.ptr.is_null() {
            return;
        }
        for idx in 0..len {
            if !unsafe { vec_retain_elem_at_for_copy(src, idx) } {
                return;
            }
            let elem = unsafe { src_ref.ptr.add((idx as usize) * stride) };
            unsafe { gos_rt_vec_push(dst, elem) };
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

/// Entry-point exit handler for a `main` that returns a `Result`, given its
/// unpacked discriminant and payload word (the native `@main` shim truncates
/// the packed i128 to two i64s to avoid the i128 C-ABI). Drains goroutines and
/// flushes stdout, then: `Ok` (disc 0) exits 0; `Err` additionally renders the
/// error's Display (colon-joined cause chain) to stderr before exiting 1 - so a
/// propagated entry-point error is reported instead of silently dropped,
/// matching the VM tier.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_main_exit_code_err(disc: i64, payload: i64) -> i32 {
    ffi_entry!(-1, {
        crate::sched_global::drain_goroutines_for_exit();
        unsafe { gos_rt_flush_stdout() };
        if disc == 0 {
            return 0;
        }
        if payload != 0 {
            let msg = unsafe { crate::c_abi::gos_rt_error_display(payload as *const _) };
            if !msg.is_null() {
                unsafe {
                    crate::c_abi::gos_rt_eprint_str(msg);
                    crate::c_abi::gos_rt_eprintln();
                    crate::c_abi::gos_rt_str_free(msg);
                }
            }
        }
        1
    })
}

#[cfg(test)]
mod packed_row_tests {
    use super::*;

    #[test]
    fn uniform_primitive_rows_pack_with_bulk_copy() {
        unsafe {
            let outer = gos_rt_vec_with_capacity_typed(8, PACKED_ROWS_MIN_ROWS, vec_elem_kind::VEC);
            for row_index in 0..PACKED_ROWS_MIN_ROWS {
                let row = gos_rt_vec_with_capacity(8, 3);
                gos_rt_vec_push_i64(row, row_index);
                gos_rt_vec_push_i64(row, row_index + 1);
                gos_rt_vec_push_i64(row, row_index + 2);
                let slot = row as usize;
                gos_rt_vec_push(outer, std::ptr::addr_of!(slot).cast());
            }
            assert!(try_pack_primitive_rows(outer));
            let row = packed_row_at(outer, 777).cast::<GosVec>();
            assert_eq!(gos_rt_vec_get_i64(row, 0), 777);
            assert_eq!(gos_rt_vec_get_i64(row, 2), 779);
            crate::c_abi::map::gos_rt_vec_free(outer);
        }
    }
}
