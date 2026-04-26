//! Value-layout constants shared by the VM and the native backend.
//! SPEC §13 commits every Gossamer value to a single byte-level layout
//! so the interpreter, bytecode VM, and Cranelift backend all agree on
//! the shape of a running program's heap. This module encodes those
//! invariants in `const` form so that layout drift produces a compile
//! failure rather than a subtle miscompilation.

#![forbid(unsafe_code)]
#![allow(clippy::many_single_char_names)]

use std::mem::{align_of, size_of};

/// Header present on every GC-managed heap object.
///
/// The header is word-sized on 64-bit targets and leaves room for an
/// inline mark byte plus two bytes of padding. Objects are always
/// preceded by the header in memory; the body follows immediately.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct ObjHeader {
    /// Type-info descriptor used by the GC to scan pointers inside the
    /// payload.
    pub type_info: *const TypeInfo,
    /// Mark byte set during the tracing phase of GC.
    pub gc_mark: u8,
    /// Reserved for future use — forwarding pointer bits, pinning,
    /// generational write-barrier state.
    pub flags: u8,
    /// Structure padding; must be zero on initialisation.
    padding: [u8; 6],
}

/// Per-type metadata referenced by every [`ObjHeader`].
///
/// The GC reads `scan_fn` to learn which bytes inside the payload hold
/// reference-like values. `drop_fn` fires for the small number of
/// types that own native resources (file handles, TLS contexts) —
/// pure-Gossamer types leave it as the no-op sentinel.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TypeInfo {
    /// Size of the payload in bytes.
    pub size: usize,
    /// Alignment of the payload in bytes.
    pub align: usize,
    /// Function that walks the payload and surfaces GC-reachable
    /// pointers. Implementation lives outside this crate.
    pub scan_fn: fn(*const u8, &mut dyn FnMut(*const u8)),
    /// Optional destructor invoked immediately before reclamation.
    /// `None` means "plain memory, no cleanup required".
    pub drop_fn: Option<fn(*mut u8)>,
}

/// Layout constants for the built-in `String` type.
pub mod string {
    use super::Ptr;

    /// `{ ptr, len, capacity }`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Repr {
        /// GC-managed pointer to the UTF-8 byte buffer.
        pub ptr: Ptr,
        /// Number of valid bytes.
        pub len: usize,
        /// Allocated capacity in bytes.
        pub capacity: usize,
    }
}

/// Layout constants for `Vec<T>`.
pub mod vec {
    use super::Ptr;

    /// `{ ptr, len, capacity }`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Repr {
        /// GC-managed pointer to the element buffer.
        pub ptr: Ptr,
        /// Number of initialised elements.
        pub len: usize,
        /// Allocated element capacity.
        pub capacity: usize,
    }
}

/// Layout constants for `HashMap<K, V>` (swiss-table shape).
pub mod hashmap {
    use super::Ptr;

    /// Swiss-table control bytes + bucket array pointer.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Repr {
        /// Control-byte table pointer.
        pub ctrl: Ptr,
        /// Bucket-array pointer.
        pub buckets: Ptr,
        /// Current element count.
        pub len: usize,
        /// Allocated bucket count.
        pub capacity: usize,
    }
}

/// Layout constants for trait objects `dyn Trait`.
pub mod dyn_ref {
    use super::Ptr;

    /// Fat-pointer representation: data pointer + vtable pointer.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Repr {
        /// Pointer to the concrete object.
        pub data: Ptr,
        /// Pointer to the vtable shared by every instance of the
        /// underlying type.
        pub vtable: Ptr,
    }
}

/// Layout constants for boxed closures.
pub mod closure {
    use super::Ptr;

    /// `{ code, env }`.
    #[repr(C)]
    #[derive(Debug, Clone, Copy)]
    pub struct Repr {
        /// Pointer to the compiled entry point.
        pub code: Ptr,
        /// Pointer to the captured-environment payload.
        pub env: Ptr,
    }
}

/// Generic word-sized pointer alias used by the layout descriptors
/// above. Kept opaque so stack maps and GC scans treat it as a GC root
/// by default.
#[repr(transparent)]
#[derive(Debug, Clone, Copy)]
pub struct Ptr(pub usize);

/// Word size of the target ABI (8 bytes on 64-bit targets).
pub const WORD_BYTES: usize = size_of::<usize>();

/// Guaranteed alignment for every heap allocation produced by the
/// runtime.
pub const HEAP_ALIGN: usize = align_of::<usize>();

/// Expected layout invariants. Evaluated at compile time so drift is
/// caught before tests run.
pub const _ASSERTIONS: () = {
    assert!(size_of::<ObjHeader>() == 16, "ObjHeader must be 16 bytes");
    assert!(align_of::<ObjHeader>() == WORD_BYTES);
    assert!(size_of::<Ptr>() == WORD_BYTES, "Ptr must be word sized");
    assert!(size_of::<string::Repr>() == 3 * WORD_BYTES);
    assert!(size_of::<vec::Repr>() == 3 * WORD_BYTES);
    assert!(size_of::<hashmap::Repr>() == 4 * WORD_BYTES);
    assert!(size_of::<dyn_ref::Repr>() == 2 * WORD_BYTES);
    assert!(size_of::<closure::Repr>() == 2 * WORD_BYTES);
};

/// Returns the size of an [`ObjHeader`] in bytes.
#[must_use]
pub const fn header_size() -> usize {
    size_of::<ObjHeader>()
}

/// Returns the required alignment of an [`ObjHeader`] in bytes.
#[must_use]
pub const fn header_align() -> usize {
    align_of::<ObjHeader>()
}
