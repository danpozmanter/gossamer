//! Conversions between Gossamer [`Value`]s and Rust types.
//!
//! [`FromGos`] marshals an argument out of `&[Value]`; [`ToGos`]
//! boxes a return value back into a `Value`. The
//! `register_module!` macro derives the call-site wrappers from
//! these traits, so binding authors write idiomatic Rust
//! signatures and never touch `Value` directly.

use std::sync::Arc;

use gossamer_interp::value::{RuntimeError, RuntimeResult, SmolStr, Value};

/// Materialises a typed Rust value out of a Gossamer [`Value`].
pub trait FromGos: Sized {
    /// Performs the conversion or returns a typed `RuntimeError`.
    fn from_gos(value: &Value) -> RuntimeResult<Self>;
}

/// Boxes a Rust value into a Gossamer [`Value`].
pub trait ToGos {
    /// Performs the conversion (infallible — panics on
    /// representation overflow, which can only happen if a
    /// binding violates its declared signature).
    fn to_gos(self) -> Value;
}

fn type_err<T>(expected: &str, found: &Value) -> RuntimeResult<T> {
    Err(RuntimeError::Type(format!(
        "expected {expected}, found {}",
        describe(found)
    )))
}

fn describe(v: &Value) -> &'static str {
    match v {
        Value::Unit => "()",
        Value::Bool(_) => "bool",
        Value::Int(_) => "i64",
        Value::Float(_) => "f64",
        Value::Char(_) => "char",
        Value::String(_) => "String",
        Value::Tuple(_) => "tuple",
        Value::Array(_) | Value::IntArray(_) | Value::FloatVec(_) | Value::FloatArray(_) => "vec",
        Value::Variant(_) => "enum variant",
        Value::Struct(_) => "struct",
        Value::Closure(_) | Value::Builtin(_) | Value::Native(_) => "callable",
        Value::Channel(_) => "channel",
        Value::Map(_) | Value::IntMap(_) => "map",
        Value::Void => "void",
    }
}

impl FromGos for () {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Unit => Ok(()),
            other => type_err("()", other),
        }
    }
}

impl ToGos for () {
    fn to_gos(self) -> Value {
        Value::Unit
    }
}

impl FromGos for bool {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Bool(b) => Ok(*b),
            other => type_err("bool", other),
        }
    }
}

impl ToGos for bool {
    fn to_gos(self) -> Value {
        Value::Bool(self)
    }
}

impl FromGos for i64 {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Int(i) => Ok(*i),
            other => type_err("i64", other),
        }
    }
}

impl ToGos for i64 {
    fn to_gos(self) -> Value {
        Value::Int(self)
    }
}

impl FromGos for u64 {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        let i = i64::from_gos(value)?;
        u64::try_from(i).map_err(|_| RuntimeError::Type(format!("expected u64, found {i}")))
    }
}

impl ToGos for u64 {
    fn to_gos(self) -> Value {
        Value::Int(i64::try_from(self).unwrap_or(i64::MAX))
    }
}

impl FromGos for usize {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        let i = i64::from_gos(value)?;
        usize::try_from(i).map_err(|_| RuntimeError::Type(format!("expected usize, found {i}")))
    }
}

impl ToGos for usize {
    fn to_gos(self) -> Value {
        Value::Int(i64::try_from(self).unwrap_or(i64::MAX))
    }
}

impl FromGos for u16 {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        let i = i64::from_gos(value)?;
        u16::try_from(i).map_err(|_| RuntimeError::Type(format!("expected u16, found {i}")))
    }
}

impl ToGos for u16 {
    fn to_gos(self) -> Value {
        Value::Int(i64::from(self))
    }
}

impl FromGos for u8 {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        let i = i64::from_gos(value)?;
        u8::try_from(i).map_err(|_| RuntimeError::Type(format!("expected u8, found {i}")))
    }
}

impl ToGos for u8 {
    fn to_gos(self) -> Value {
        Value::Int(i64::from(self))
    }
}

impl FromGos for f64 {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Float(f) => Ok(*f),
            // Lossy for |i| > 2^53; user code that passes such an
            // i64 to an f64 binding param has accepted that, the
            // explicit conversion just makes it visible.
            #[allow(clippy::cast_precision_loss)]
            Value::Int(i) => Ok(*i as f64),
            other => type_err("f64", other),
        }
    }
}

impl ToGos for f64 {
    fn to_gos(self) -> Value {
        Value::Float(self)
    }
}

impl FromGos for char {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Char(c) => Ok(*c),
            other => type_err("char", other),
        }
    }
}

impl ToGos for char {
    fn to_gos(self) -> Value {
        Value::Char(self)
    }
}

impl FromGos for String {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::String(s) => Ok(s.as_str().to_string()),
            other => type_err("String", other),
        }
    }
}

impl ToGos for String {
    fn to_gos(self) -> Value {
        Value::String(SmolStr::from_string(self))
    }
}

impl ToGos for &str {
    fn to_gos(self) -> Value {
        Value::String(SmolStr::from_str(self))
    }
}

impl<T: FromGos> FromGos for Option<T> {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Variant(inner) => {
                if inner.name == "None" {
                    Ok(None)
                } else if inner.name == "Some" {
                    let payload = inner
                        .fields
                        .first()
                        .ok_or_else(|| RuntimeError::Type("Some(_) without payload".to_string()))?;
                    Ok(Some(T::from_gos(payload)?))
                } else {
                    type_err("Option<T>", value)
                }
            }
            other => type_err("Option<T>", other),
        }
    }
}

impl<T: ToGos> ToGos for Option<T> {
    fn to_gos(self) -> Value {
        match self {
            None => Value::variant("None", Arc::new(Vec::new())),
            Some(t) => Value::variant("Some", Arc::new(vec![t.to_gos()])),
        }
    }
}

impl<T: FromGos, E: FromGos> FromGos for Result<T, E> {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        match value {
            Value::Variant(inner) => {
                let first = inner.fields.first().ok_or_else(|| {
                    RuntimeError::Type("Result<_, _> without payload".to_string())
                })?;
                if inner.name == "Ok" {
                    Ok(Ok(T::from_gos(first)?))
                } else if inner.name == "Err" {
                    Ok(Err(E::from_gos(first)?))
                } else {
                    type_err("Result<T, E>", value)
                }
            }
            other => type_err("Result<T, E>", other),
        }
    }
}

impl<T: ToGos, E: ToGos> ToGos for Result<T, E> {
    fn to_gos(self) -> Value {
        match self {
            Ok(t) => Value::variant("Ok", Arc::new(vec![t.to_gos()])),
            Err(e) => Value::variant("Err", Arc::new(vec![e.to_gos()])),
        }
    }
}

impl FromGos for Value {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        Ok(value.clone())
    }
}

impl ToGos for Value {
    fn to_gos(self) -> Value {
        self
    }
}

impl<T: FromGos> FromGos for Vec<T> {
    fn from_gos(value: &Value) -> RuntimeResult<Self> {
        let items: &[Value] = match value {
            Value::Array(arc) | Value::Tuple(arc) => arc.as_slice(),
            other => return type_err("[T]", other),
        };
        items.iter().map(T::from_gos).collect()
    }
}

impl<T: ToGos> ToGos for Vec<T> {
    fn to_gos(self) -> Value {
        let items: Vec<Value> = self.into_iter().map(ToGos::to_gos).collect();
        Value::Array(Arc::new(items))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_i64() {
        let v: Value = 42_i64.to_gos();
        assert_eq!(i64::from_gos(&v).unwrap(), 42);
    }

    #[test]
    fn round_trip_string() {
        let v: Value = "hello".to_gos();
        assert_eq!(String::from_gos(&v).unwrap(), "hello");
    }

    #[test]
    fn round_trip_option_some_none() {
        let v: Value = Some(5_i64).to_gos();
        assert_eq!(Option::<i64>::from_gos(&v).unwrap(), Some(5));

        let v: Value = Option::<i64>::None.to_gos();
        assert_eq!(Option::<i64>::from_gos(&v).unwrap(), None);
    }

    #[test]
    fn round_trip_result_ok_err() {
        let v: Value = Ok::<_, String>(7_i64).to_gos();
        assert_eq!(Result::<i64, String>::from_gos(&v).unwrap(), Ok(7));

        let v: Value = Err::<i64, _>("bad".to_string()).to_gos();
        assert_eq!(
            Result::<i64, String>::from_gos(&v).unwrap(),
            Err("bad".to_string())
        );
    }

    #[test]
    fn round_trip_vec() {
        let v: Value = vec![1_i64, 2, 3].to_gos();
        assert_eq!(Vec::<i64>::from_gos(&v).unwrap(), vec![1, 2, 3]);
    }

    #[test]
    fn type_mismatch_returns_typed_error() {
        let v: Value = "hello".to_gos();
        let err = i64::from_gos(&v).unwrap_err();
        assert!(matches!(err, RuntimeError::Type(_)));
    }
}
