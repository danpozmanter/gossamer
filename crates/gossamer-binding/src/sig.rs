//! Compile-time type advertisement.
//!
//! Each Rust type usable in a binding signature implements
//! [`SigType`] with an associated `const TYPE` that the
//! `register_module!` macro picks up to populate
//! [`crate::Signature::params`] / [`crate::Signature::ret`].

use crate::types::Type;

/// Lifts a Rust type into the binding's [`Type`] vocabulary.
pub trait SigType {
    /// Static [`Type`] tag identifying `Self`.
    const TYPE: Type;
}

impl SigType for () {
    const TYPE: Type = Type::Unit;
}

impl SigType for bool {
    const TYPE: Type = Type::Bool;
}

impl SigType for i64 {
    const TYPE: Type = Type::I64;
}

impl SigType for u64 {
    const TYPE: Type = Type::I64;
}

impl SigType for usize {
    const TYPE: Type = Type::I64;
}

impl SigType for u16 {
    const TYPE: Type = Type::I64;
}

impl SigType for u8 {
    const TYPE: Type = Type::I64;
}

impl SigType for f64 {
    const TYPE: Type = Type::F64;
}

impl SigType for char {
    const TYPE: Type = Type::Char;
}

impl SigType for String {
    const TYPE: Type = Type::String;
}

impl<T: SigType> SigType for Option<T> {
    const TYPE: Type = Type::Option(&T::TYPE);
}

impl<T: SigType, E: SigType> SigType for Result<T, E> {
    const TYPE: Type = Type::Result(&T::TYPE, &E::TYPE);
}

impl<T: SigType> SigType for Vec<T> {
    const TYPE: Type = Type::Vec(&T::TYPE);
}

impl SigType for crate::Value {
    /// `Value` is the universal pass-through; the type checker
    /// accepts anything in this slot.
    const TYPE: Type = Type::Any;
}
