//! Reference-counting type-meta ABI shared by the MIR lowerer (which
//! emits the descriptor blobs) and the runtime (which parses them in
//! `gos_rt_rc_release`). Single source of truth for the blob's kind
//! tags and layout so the two sides cannot drift.
//!
//! # Type-meta blob format (a flat, self-describing `[i64]`)
//!
//! Codegen emits one blob per RC-managed allocation shape as a single
//! contiguous module constant. The object header's `meta` field points
//! at word 0.
//!
//! ```text
//! [0] kind            — RC_KIND_*
//! [1] variant_count V
//! then V variant records, each variable-length:
//!     disc            — discriminant this record describes
//!     child_count C   — number of RC-pointer child words
//!     off_0 .. off_C  — payload WORD indices (byte offset / 8) holding
//!                       RC-managed child pointers to release
//! ```
//!
//! For an enum, release reads the live discriminant from payload word 0
//! and releases the matching record's children. For a struct/tuple
//! there is a single record and the discriminant is ignored. The MIR
//! lowerer emits one single-record `RC_KIND_STRUCT` blob per enum
//! variant (each allocation carries its own descriptor), so the enum
//! disc-search path is reserved for future shared descriptors.

/// `meta[0]` kind: enum (release reads the live disc from payload word 0
/// and matches a record).
pub const RC_KIND_ENUM: i64 = 0;
/// `meta[0]` kind: struct/tuple/single-variant (one record, disc ignored).
pub const RC_KIND_STRUCT: i64 = 1;
/// `meta[0]` kind: string-like heap object (wired in a later phase).
pub const RC_KIND_STRING: i64 = 2;
/// `meta[0]` kind: vec-like heap object (wired in a later phase).
pub const RC_KIND_VEC: i64 = 3;
/// `meta[0]` kind: map-like heap object (wired in a later phase).
pub const RC_KIND_MAP: i64 = 4;
/// `meta[0]` kind: closure environment (wired in a later phase).
pub const RC_KIND_CLOSURE: i64 = 5;
