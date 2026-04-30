//! Type vocabulary exposed to the Gossamer type checker.
//!
//! Bindings advertise their function signatures with these types;
//! the type checker uses them to validate call sites in `.gos`
//! source. The vocabulary is intentionally narrower than
//! Gossamer's full type system — no generics, no traits — so the
//! mapping is a flat function on each variant.

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
    /// `(T1, T2, ...)`.
    Tuple(&'static [Type]),
    /// `[T]`.
    Vec(&'static Type),
    /// `Option<T>`.
    Option(&'static Type),
    /// `Result<T, E>`.
    Result(&'static Type, &'static Type),
    /// User-defined opaque struct or enum, identified by name.
    Opaque(&'static str),
    /// `Any` — the type checker accepts anything for this slot
    /// (useful for variadics and pre-typed-system bindings).
    Any,
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
}
