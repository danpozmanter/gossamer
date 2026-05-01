//! Compiled-mode export ABI for binding items.
//!
//! Bridges Rust types in user binding signatures to C-ABI shapes
//! the gossamer codegen emits calls against. Each supported
//! `Type` variant lowers to a stable C-ABI input/output type via
//! [`BindingAbi`]; the `register_module!` macro uses these
//! associated types to synthesize an `extern "C"` thunk per
//! binding fn.
//!
//! See `~/dev/contexts/lang/ffi_compiled.md` Stage 1.

// FFI bridge: pointer reinterprets are deliberate. The runtime
// lays out `GosVec<T>` payloads as a tightly-packed buffer of
// `T`-sized cells; `cast::<T>()` lets us read through that buffer
// without copying. Alignment is enforced upstream by the GC's
// 8-byte allocator, so the cast-ptr-alignment lint is wrong here.
#![allow(clippy::cast_ptr_alignment)]
// `unsafe extern "C"` thunks: every call comes from generated code
// over a contract documented at the call site (see ffi_compiled.md).
#![allow(
    unsafe_code,
    clippy::missing_safety_doc,
    clippy::not_unsafe_ptr_arg_deref
)]

use std::ffi::CStr;
use std::os::raw::c_char;

use crate::types::Type;

// Bring `gos_rt_gc_alloc` into scope. Defined in
// `gossamer-runtime`'s `c_abi.rs`; the binding crate links it
// transitively via `gossamer-interp`. Allocations from this
// arena are what the runtime expects to read past for compound
// types — matching domains is what makes Vec/String/Option/etc.
// flow correctly through the compiled-mode boundary.
unsafe extern "C" {
    fn gos_rt_gc_alloc(size: u64) -> *mut u8;
}

/// Arena-backed allocator used by every compound `to_output`
/// path. Returns a raw pointer (not `Box`) so the runtime's
/// `gos_rt_*` readers can dereference it safely; the runtime
/// owns reclamation via `gos_rt_gc_reset`.
///
/// Rounds the request up to a multiple of 8 bytes. The
/// underlying bump allocator is byte-aligned, so without this
/// rounding a header struct following a non-multiple-of-8
/// allocation (e.g. a 13-byte cstring) would land at a
/// misaligned offset and trip `ptr::copy_nonoverlapping`'s
/// alignment precondition. 8 bytes covers every shape this
/// crate writes (`GosVec`, `GosVariant`, `GosTuple`,
/// `GosVariantValue`).
fn arena_alloc(bytes: usize) -> *mut u8 {
    if bytes == 0 {
        return std::ptr::null_mut();
    }
    let aligned = bytes.div_ceil(8) * 8;
    // SAFETY: `gos_rt_gc_alloc` is part of `gossamer-runtime`'s
    // C-ABI surface; the binding's staticlib links it. Returns a
    // pointer into a thread-local arena valid until the next
    // `gos_rt_gc_reset` (the runtime's tick boundary).
    unsafe { gos_rt_gc_alloc(aligned as u64) }
}

/// Allocates one `T`-shaped slot in the arena, writes `value`
/// into it, and returns the pointer. Used to manufacture
/// header structs (`GosVec`, `GosVariant`, `GosTuple`,
/// `GosVariantValue`) without going through Box.
fn arena_box<T>(value: T) -> *mut T {
    let p = arena_alloc(std::mem::size_of::<T>()).cast::<T>();
    if !p.is_null() {
        // SAFETY: `p` is a fresh arena allocation aligned for
        // `T` (the runtime's bump arena returns word-aligned
        // pointers, which suffices for every shape we
        // manufacture here — `GosVec`, `GosVariant`, etc. all
        // have alignment ≤ 8).
        unsafe {
            std::ptr::write(p, value);
        }
    }
    p
}

/// Aggregate matching the runtime's `gos_rt_*` vec ABI.
///
/// Storage layout is deliberately fixed so codegen on either tier
/// can manufacture and consume these without reaching into
/// `gossamer-runtime` internals. `len` / `cap` are element counts
/// (not byte counts); `elem_bytes` records the homogeneous element
/// size; `ptr` points at `len * elem_bytes` bytes.
#[repr(C)]
#[derive(Debug)]
pub struct GosVec {
    /// Number of elements.
    pub len: i64,
    /// Allocated element capacity.
    pub cap: i64,
    /// Element width in bytes.
    pub elem_bytes: u32,
    /// Element data buffer.
    pub ptr: *mut u8,
}

/// Aggregate matching the runtime's `gos_rt_variant` ABI for
/// `Option`, `Result`, and other tagged sums.
#[repr(C)]
#[derive(Debug)]
pub struct GosVariant {
    /// Variant tag — the macro encodes:
    /// - `0` for `None` / `Err`
    /// - `1` for `Some` / `Ok`
    ///
    /// Bindings authoring custom enums set their own values.
    pub tag: i32,
    /// Number of payload values.
    pub payload_len: i32,
    /// Payload pointer; layout is `payload_len`
    /// `GosVariantValue`s. Null when there is no payload.
    pub payload: *mut GosVariantValue,
}

/// Tagged-union element used inside a [`GosVariant`] payload or
/// a [`GosTuple`] field array. The tag picks which member of the
/// `data` field is live.
#[repr(C)]
#[derive(Debug)]
pub struct GosVariantValue {
    /// Tag (`0` = i64, `1` = f64, `2` = bool, `3` = char,
    /// `4` = string, `5` = vec, `6` = variant, `7` = tuple,
    /// `8` = opaque).
    pub tag: i32,
    /// Payload data — readers consult `tag` to pick the live
    /// member.
    pub data: GosVariantPayload,
}

/// Untagged-union payload sized to the largest variant. Reading
/// fields that don't match the [`GosVariantValue::tag`] is
/// undefined behaviour at the C-ABI level.
#[repr(C)]
#[derive(Clone, Copy)]
pub union GosVariantPayload {
    /// `i64` payload.
    pub i64_: i64,
    /// `f64` payload.
    pub f64_: f64,
    /// `bool` payload.
    pub bool_: bool,
    /// `char` payload (`u32` Unicode code point).
    pub char_: u32,
    /// String payload (NUL-terminated, arena-allocated).
    pub string: *mut c_char,
    /// Nested vec payload.
    pub vec: *mut GosVec,
    /// Nested variant payload.
    pub variant: *mut GosVariant,
    /// Nested tuple payload.
    pub tuple: *mut GosTuple,
}

impl std::fmt::Debug for GosVariantPayload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<gos variant payload>")
    }
}

/// Aggregate matching the runtime's tuple ABI. Stores `len`
/// fields each as a [`GosVariantValue`]; the field count and
/// per-field types are fixed by the binding signature.
#[repr(C)]
#[derive(Debug)]
pub struct GosTuple {
    /// Field count.
    pub len: i32,
    /// Field array; layout matches `payload` of [`GosVariant`].
    pub fields: *mut GosVariantValue,
}

// --- BindingAbi -----------------------------------------------------

/// Maps a Rust binding-signature type to its compiled-mode
/// C-ABI input / output shape.
///
/// The macro reads `Input` for parameter types and `Output` for
/// the return type; the codegen emits the call with the same
/// shapes determined from the binding's declared `Signature`.
pub trait BindingAbi: Sized {
    /// C-ABI shape used in argument position.
    type Input: Copy;
    /// C-ABI shape used in return position.
    type Output;

    /// Picks the [`Type`] variant the codegen sees in the
    /// binding's advertised signature; used by the codegen to
    /// pick the matching pack/unpack lowering.
    const TYPE: Type;

    /// Materialises the Rust value from its `Input` shape. Called
    /// at the start of the macro-generated thunk.
    ///
    /// # Safety
    /// Pointers received via `Input` must be valid for the
    /// duration of the call. The codegen guarantees this; binding
    /// authors should not call this manually.
    unsafe fn from_input(input: Self::Input) -> Self;

    /// Boxes the Rust value into its `Output` shape. Called at
    /// the end of the macro-generated thunk to hand the value
    /// back to the calling Gossamer code.
    fn to_output(self) -> Self::Output;
}

// --- Primitive impls ------------------------------------------------

impl BindingAbi for i64 {
    type Input = i64;
    type Output = i64;
    const TYPE: Type = Type::I64;

    unsafe fn from_input(input: i64) -> Self {
        input
    }
    fn to_output(self) -> i64 {
        self
    }
}

impl BindingAbi for f64 {
    type Input = f64;
    type Output = f64;
    const TYPE: Type = Type::F64;

    unsafe fn from_input(input: f64) -> Self {
        input
    }
    fn to_output(self) -> f64 {
        self
    }
}

impl BindingAbi for bool {
    type Input = bool;
    type Output = bool;
    const TYPE: Type = Type::Bool;

    unsafe fn from_input(input: bool) -> Self {
        input
    }
    fn to_output(self) -> bool {
        self
    }
}

impl BindingAbi for char {
    type Input = u32;
    type Output = u32;
    const TYPE: Type = Type::Char;

    unsafe fn from_input(input: u32) -> Self {
        char::from_u32(input).unwrap_or('\0')
    }
    fn to_output(self) -> u32 {
        self as u32
    }
}

impl BindingAbi for () {
    type Input = ();
    type Output = ();
    const TYPE: Type = Type::Unit;

    unsafe fn from_input(_input: ()) -> Self {}
    fn to_output(self) {}
}

// --- String ---------------------------------------------------------

impl BindingAbi for String {
    type Input = *const c_char;
    type Output = *mut c_char;
    const TYPE: Type = Type::String;

    unsafe fn from_input(input: *const c_char) -> Self {
        if input.is_null() {
            return String::new();
        }
        unsafe { CStr::from_ptr(input) }
            .to_string_lossy()
            .into_owned()
    }

    fn to_output(self) -> *mut c_char {
        // Strip interior NULs so the C-string view stops at our
        // explicit terminator. Allocate `len + 1` arena bytes,
        // copy payload, write trailing NUL.
        let bytes: Vec<u8> = self.into_bytes().into_iter().filter(|b| *b != 0).collect();
        let total = bytes.len() + 1;
        let p = arena_alloc(total);
        if p.is_null() {
            return std::ptr::null_mut();
        }
        // SAFETY: arena allocation is `total` bytes; we write
        // exactly `total` bytes (`bytes.len()` payload + NUL).
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), p, bytes.len());
            *p.add(bytes.len()) = 0;
        }
        p.cast::<c_char>()
    }
}

// --- Vec ------------------------------------------------------------

/// Builds a `GosVec` from `elements` using the same heap-owned
/// allocation pattern the runtime's own `gos_rt_vec_with_capacity`
/// uses: header via `Box::into_raw`, data buffer via `Vec::leak`.
/// `gos_rt_vec_free` (`Box::from_raw` + `Vec::from_raw_parts`) reclaims
/// both at end of lifetime. Arena-allocating instead would crash
/// inside libc when the compiled program drops the value.
fn make_gos_vec<T: Copy>(elements: &[T]) -> *mut GosVec {
    let elem_bytes = u32::try_from(std::mem::size_of::<T>()).unwrap_or(0);
    let len = i64::try_from(elements.len()).unwrap_or(0);
    let cap = len;
    let bytes = std::mem::size_of_val(elements);
    let raw_ptr = if bytes == 0 {
        std::ptr::null_mut()
    } else {
        let mut buf: Vec<u8> = vec![0u8; bytes];
        let p = buf.as_mut_ptr();
        // SAFETY: `T: Copy`; byte-blit preserves semantics.
        unsafe {
            std::ptr::copy_nonoverlapping(elements.as_ptr().cast::<u8>(), p, bytes);
        }
        std::mem::forget(buf);
        p
    };
    Box::into_raw(Box::new(GosVec {
        len,
        cap,
        elem_bytes,
        ptr: raw_ptr,
    }))
}

unsafe fn read_gos_vec_i64(p: *const GosVec) -> Vec<i64> {
    if p.is_null() {
        return Vec::new();
    }
    let header = unsafe { &*p };
    let len = usize::try_from(header.len.max(0)).unwrap_or(0);
    if header.ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(header.ptr.cast::<i64>(), len) };
    slice.to_vec()
}

unsafe fn read_gos_vec_f64(p: *const GosVec) -> Vec<f64> {
    if p.is_null() {
        return Vec::new();
    }
    let header = unsafe { &*p };
    let len = usize::try_from(header.len.max(0)).unwrap_or(0);
    if header.ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(header.ptr.cast::<f64>(), len) };
    slice.to_vec()
}

unsafe fn read_gos_vec_strings(p: *const GosVec) -> Vec<String> {
    if p.is_null() {
        return Vec::new();
    }
    let header = unsafe { &*p };
    let len = usize::try_from(header.len.max(0)).unwrap_or(0);
    if header.ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(header.ptr.cast::<*const c_char>(), len) };
    slice
        .iter()
        .map(|p| unsafe { String::from_input(*p) })
        .collect()
}

unsafe fn read_gos_vec_bools(p: *const GosVec) -> Vec<bool> {
    if p.is_null() {
        return Vec::new();
    }
    let header = unsafe { &*p };
    let len = usize::try_from(header.len.max(0)).unwrap_or(0);
    if header.ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(header.ptr.cast::<u8>(), len) };
    slice.iter().map(|b| *b != 0).collect()
}

unsafe fn read_gos_vec_vec_i64(p: *const GosVec) -> Vec<Vec<i64>> {
    if p.is_null() {
        return Vec::new();
    }
    let header = unsafe { &*p };
    let len = usize::try_from(header.len.max(0)).unwrap_or(0);
    if header.ptr.is_null() || len == 0 {
        return Vec::new();
    }
    let slice = unsafe { std::slice::from_raw_parts(header.ptr.cast::<*const GosVec>(), len) };
    slice
        .iter()
        .map(|inner| unsafe { read_gos_vec_i64(*inner) })
        .collect()
}

impl BindingAbi for Vec<i64> {
    type Input = *const GosVec;
    type Output = *mut GosVec;
    const TYPE: Type = Type::Vec(&Type::I64);

    unsafe fn from_input(input: *const GosVec) -> Self {
        unsafe { read_gos_vec_i64(input) }
    }

    fn to_output(self) -> *mut GosVec {
        make_gos_vec(&self)
    }
}

impl BindingAbi for Vec<f64> {
    type Input = *const GosVec;
    type Output = *mut GosVec;
    const TYPE: Type = Type::Vec(&Type::F64);

    unsafe fn from_input(input: *const GosVec) -> Self {
        unsafe { read_gos_vec_f64(input) }
    }

    fn to_output(self) -> *mut GosVec {
        make_gos_vec(&self)
    }
}

impl BindingAbi for Vec<bool> {
    type Input = *const GosVec;
    type Output = *mut GosVec;
    const TYPE: Type = Type::Vec(&Type::Bool);

    unsafe fn from_input(input: *const GosVec) -> Self {
        unsafe { read_gos_vec_bools(input) }
    }

    fn to_output(self) -> *mut GosVec {
        let bytes: Vec<u8> = self.into_iter().map(u8::from).collect();
        make_gos_vec(&bytes)
    }
}

impl BindingAbi for Vec<String> {
    type Input = *const GosVec;
    type Output = *mut GosVec;
    const TYPE: Type = Type::Vec(&Type::String);

    unsafe fn from_input(input: *const GosVec) -> Self {
        unsafe { read_gos_vec_strings(input) }
    }

    fn to_output(self) -> *mut GosVec {
        let ptrs: Vec<*mut c_char> = self.into_iter().map(BindingAbi::to_output).collect();
        make_gos_vec(&ptrs)
    }
}

impl BindingAbi for Vec<Vec<i64>> {
    type Input = *const GosVec;
    type Output = *mut GosVec;
    const TYPE: Type = Type::Vec(&Type::Vec(&Type::I64));

    unsafe fn from_input(input: *const GosVec) -> Self {
        unsafe { read_gos_vec_vec_i64(input) }
    }

    fn to_output(self) -> *mut GosVec {
        let ptrs: Vec<*mut GosVec> = self.into_iter().map(BindingAbi::to_output).collect();
        make_gos_vec(&ptrs)
    }
}

// --- Option<i64>, Result<i64, String> -----------------------------

unsafe fn variant_value_i64(v: i64) -> GosVariantValue {
    GosVariantValue {
        tag: 0,
        data: GosVariantPayload { i64_: v },
    }
}

unsafe fn variant_value_string(s: String) -> GosVariantValue {
    GosVariantValue {
        tag: 4,
        data: GosVariantPayload {
            string: s.to_output(),
        },
    }
}

fn make_variant(tag: i32, payload: Vec<GosVariantValue>) -> *mut GosVariant {
    let payload_len = i32::try_from(payload.len()).unwrap_or(0);
    let payload_ptr: *mut GosVariantValue = if payload.is_empty() {
        std::ptr::null_mut()
    } else {
        let bytes = std::mem::size_of_val(payload.as_slice());
        let buf = arena_alloc(bytes).cast::<GosVariantValue>();
        if !buf.is_null() {
            // SAFETY: arena buffer is `bytes` long, fresh, and
            // exclusively ours; payload slice is non-overlapping.
            unsafe {
                std::ptr::copy_nonoverlapping(payload.as_ptr(), buf, payload.len());
            }
        }
        buf
    };
    arena_box(GosVariant {
        tag,
        payload_len,
        payload: payload_ptr,
    })
}

unsafe fn read_option_i64(p: *const GosVariant) -> Option<i64> {
    if p.is_null() {
        return None;
    }
    let v = unsafe { &*p };
    if v.tag == 0 || v.payload_len == 0 || v.payload.is_null() {
        return None;
    }
    let payload = unsafe { &*v.payload };
    if payload.tag != 0 {
        return None;
    }
    Some(unsafe { payload.data.i64_ })
}

unsafe fn read_result_i64_string(p: *const GosVariant) -> Result<i64, String> {
    if p.is_null() {
        return Err(String::new());
    }
    let v = unsafe { &*p };
    if v.payload_len == 0 || v.payload.is_null() {
        return Err(String::new());
    }
    let payload = unsafe { &*v.payload };
    if v.tag == 1 {
        Ok(unsafe { payload.data.i64_ })
    } else {
        Err(unsafe { String::from_input(payload.data.string) })
    }
}

impl BindingAbi for Option<i64> {
    type Input = *const GosVariant;
    type Output = *mut GosVariant;
    const TYPE: Type = Type::Option(&Type::I64);

    unsafe fn from_input(input: *const GosVariant) -> Self {
        unsafe { read_option_i64(input) }
    }

    fn to_output(self) -> *mut GosVariant {
        match self {
            None => make_variant(0, Vec::new()),
            Some(v) => make_variant(1, vec![unsafe { variant_value_i64(v) }]),
        }
    }
}

impl BindingAbi for Result<i64, String> {
    type Input = *const GosVariant;
    type Output = *mut GosVariant;
    const TYPE: Type = Type::Result(&Type::I64, &Type::String);

    unsafe fn from_input(input: *const GosVariant) -> Self {
        unsafe { read_result_i64_string(input) }
    }

    fn to_output(self) -> *mut GosVariant {
        match self {
            Ok(v) => make_variant(1, vec![unsafe { variant_value_i64(v) }]),
            Err(e) => make_variant(0, vec![unsafe { variant_value_string(e) }]),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_round_trip_through_abi() {
        unsafe {
            assert_eq!(<i64 as BindingAbi>::from_input(7), 7);
            assert!(<bool as BindingAbi>::from_input(true));
            assert_eq!(<f64 as BindingAbi>::from_input(1.5), 1.5);
        }
    }

    #[test]
    fn string_round_trip() {
        let s = String::from("hello");
        let raw = s.to_output();
        let back = unsafe { String::from_input(raw) };
        assert_eq!(back, "hello");
        // No explicit free: arena reclamation lives behind
        // `gos_rt_gc_reset`, called at the runtime's tick
        // boundary. The test exits before any reset, so the
        // arena holds the bytes for the duration of the test.
    }

    #[test]
    fn vec_i64_round_trip() {
        let v: Vec<i64> = vec![1, 2, 3];
        let raw = v.to_output();
        let back: Vec<i64> = unsafe { <Vec<i64> as BindingAbi>::from_input(raw) };
        assert_eq!(back, vec![1, 2, 3]);
    }

    #[test]
    fn vec_vec_i64_round_trip() {
        let v: Vec<Vec<i64>> = vec![vec![1, 2], vec![3, 4]];
        let raw = v.to_output();
        let back: Vec<Vec<i64>> = unsafe { <Vec<Vec<i64>> as BindingAbi>::from_input(raw) };
        assert_eq!(back, vec![vec![1, 2], vec![3, 4]]);
    }

    #[test]
    fn option_round_trip() {
        let some_raw = Some(42_i64).to_output();
        assert_eq!(
            unsafe { <Option<i64> as BindingAbi>::from_input(some_raw) },
            Some(42)
        );

        let none_raw = Option::<i64>::None.to_output();
        assert_eq!(
            unsafe { <Option<i64> as BindingAbi>::from_input(none_raw) },
            None
        );
    }

    #[test]
    fn result_round_trip() {
        let ok_raw = Ok::<i64, String>(7).to_output();
        let back = unsafe { <Result<i64, String> as BindingAbi>::from_input(ok_raw) };
        assert_eq!(back, Ok(7));

        let err_raw = Err::<i64, _>("nope".to_string()).to_output();
        let back = unsafe { <Result<i64, String> as BindingAbi>::from_input(err_raw) };
        assert_eq!(back, Err("nope".to_string()));
    }
}
