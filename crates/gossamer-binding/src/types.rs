//! Type vocabulary exposed to the Gossamer type checker.
//!
//! Bindings advertise their function signatures with these types;
//! the type checker uses them to validate call sites in `.gos`
//! source. The vocabulary is intentionally narrower than
//! Gossamer's full type system — no generics, no traits — so the
//! mapping is a flat function on each variant.
//!
//! # ABI version 0.4
//!
//! Adds four shapes that unblock the ecosystem libraries
//! (Postgres typed columns, Redis RESP returns, OpenTelemetry
//! attribute maps, HTTP/gRPC streaming callbacks):
//!
//! - [`Type::Bytes`]      — first-class `[u8]` payload. Distinct
//!   from `Vec<i64>` at the source level. Zero-copy on the
//!   compiled tier via [`crate::native::GosBytes`].
//! - [`Type::Variant`]    — tagged-union return shape with named
//!   arms. Rust authors hand back / receive
//!   [`crate::conv::DynValue`].
//! - [`Type::Map`]        — key/value collection. Maps to
//!   `HashMap<K, V>` on the Rust side.
//! - [`Type::Callback`]   — a Gossamer-side fn handle the binding
//!   can re-invoke. Lifetime is call-scoped — the binding must
//!   not retain the [`crate::conv::BindingCallback`] past the
//!   binding fn's return.

/// A Gossamer-visible type, as advertised by a binding signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Type {
    /// `()`.
    Unit,
    /// `bool`.
    Bool,
    /// `i64` (Gossamer's default integer).
    I64,
    /// `f64`.
    F64,
    /// `char`.
    Char,
    /// `String`.
    String,
    /// `Bytes` — opaque byte buffer with zero-copy
    /// compiled-tier ABI. Source spelling is `Bytes`; Rust shape
    /// is `Vec<u8>`.
    Bytes,
    /// `(T1, T2, ...)`.
    Tuple(&'static [Type]),
    /// `[T]`.
    Vec(&'static Type),
    /// `Option<T>`.
    Option(&'static Type),
    /// `Result<T, E>`.
    Result(&'static Type, &'static Type),
    /// `Map<K, V>` — keyed collection. Source spelling is
    /// `Map<K, V>`; Rust shape is `HashMap<K, V>`.
    Map(&'static Type, &'static Type),
    /// Tagged-union return shape. Each arm has a name plus a
    /// payload-type list. Source spelling is `Variant`; Rust
    /// shape is [`crate::conv::DynValue`].
    Variant(&'static [VariantArm]),
    /// Gossamer-side callable. The binding receives a
    /// [`crate::conv::BindingCallback`] handle and may invoke
    /// it during the binding call. The handle is NOT
    /// retain-safe across the binding return.
    Callback(&'static [Type], &'static Type),
    /// User-defined opaque struct or enum, identified by name.
    Opaque(&'static str),
    /// `Any` — the type checker accepts anything for this slot
    /// (useful for variadics and pre-typed-system bindings).
    Any,
}

/// One arm of a [`Type::Variant`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VariantArm {
    /// Arm name, e.g. `"Nil"`, `"Integer"`, `"Array"`.
    pub name: &'static str,
    /// Positional payload types for this arm.
    pub payload: &'static [Type],
}

impl Type {
    /// Renders the type to its Gossamer-source spelling.
    #[must_use]
    pub fn to_source(&self) -> String {
        match self {
            Self::Unit => "()".to_string(),
            Self::Bool => "bool".to_string(),
            Self::I64 => "i64".to_string(),
            Self::F64 => "f64".to_string(),
            Self::Char => "char".to_string(),
            Self::String => "String".to_string(),
            Self::Bytes => "Bytes".to_string(),
            Self::Tuple(ts) => {
                let inner = ts
                    .iter()
                    .map(Self::to_source)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("({inner})")
            }
            Self::Vec(t) => format!("[{}]", t.to_source()),
            Self::Option(t) => format!("Option<{}>", t.to_source()),
            Self::Result(t, e) => format!("Result<{}, {}>", t.to_source(), e.to_source()),
            Self::Map(k, v) => format!("Map<{}, {}>", k.to_source(), v.to_source()),
            Self::Variant(arms) => {
                let body = arms
                    .iter()
                    .map(|a| {
                        if a.payload.is_empty() {
                            a.name.to_string()
                        } else {
                            let payload = a
                                .payload
                                .iter()
                                .map(Self::to_source)
                                .collect::<Vec<_>>()
                                .join(", ");
                            format!("{}({payload})", a.name)
                        }
                    })
                    .collect::<Vec<_>>()
                    .join(" | ");
                format!("Variant<{body}>")
            }
            Self::Callback(args, ret) => {
                let params = args
                    .iter()
                    .map(Self::to_source)
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("Fn({params}) -> {}", ret.to_source())
            }
            Self::Opaque(name) => (*name).to_string(),
            Self::Any => "_".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn primitive_source_spellings() {
        assert_eq!(Type::I64.to_source(), "i64");
        assert_eq!(Type::String.to_source(), "String");
        assert_eq!(Type::Unit.to_source(), "()");
        assert_eq!(Type::Bytes.to_source(), "Bytes");
    }

    #[test]
    fn vec_option_result_compose() {
        const T: Type = Type::Vec(&Type::I64);
        const O: Type = Type::Option(&Type::String);
        const R: Type = Type::Result(&Type::I64, &Type::String);
        assert_eq!(T.to_source(), "[i64]");
        assert_eq!(O.to_source(), "Option<String>");
        assert_eq!(R.to_source(), "Result<i64, String>");
    }

    #[test]
    fn tuple_source_spelling() {
        const T: Type = Type::Tuple(&[Type::I64, Type::String, Type::Bool]);
        assert_eq!(T.to_source(), "(i64, String, bool)");
    }

    #[test]
    fn opaque_uses_supplied_name() {
        const T: Type = Type::Opaque("Terminal");
        assert_eq!(T.to_source(), "Terminal");
    }

    #[test]
    fn map_source_spelling() {
        const M: Type = Type::Map(&Type::String, &Type::I64);
        assert_eq!(M.to_source(), "Map<String, i64>");
    }

    #[test]
    fn variant_source_spelling() {
        const ARMS: &[VariantArm] = &[
            VariantArm {
                name: "Nil",
                payload: &[],
            },
            VariantArm {
                name: "Integer",
                payload: &[Type::I64],
            },
            VariantArm {
                name: "BulkString",
                payload: &[Type::Bytes],
            },
        ];
        const V: Type = Type::Variant(ARMS);
        assert_eq!(
            V.to_source(),
            "Variant<Nil | Integer(i64) | BulkString(Bytes)>"
        );
    }

    #[test]
    fn callback_source_spelling() {
        const ARGS: &[Type] = &[Type::String, Type::I64];
        const C: Type = Type::Callback(ARGS, &Type::Bool);
        assert_eq!(C.to_source(), "Fn(String, i64) -> bool");
    }
}
