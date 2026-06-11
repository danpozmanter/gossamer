//! Helpers shared with the `#[derive(GosStruct)]` proc-macro.
//!
//! The derive emits calls to these functions to materialise a
//! `Value::Struct` without depending on `gossamer-ast` from the
//! binding crate.

use gossamer_ast::Ident;
use gossamer_interp::value::Value;

/// Builds a `Value::Struct` with the supplied name and ordered
/// field list. Field names are converted to `Ident`s on the way
/// in so the wire layout matches `Value::struct_` exactly.
#[must_use]
pub fn build_struct(name: &str, fields: Vec<(String, Value)>) -> Value {
    let mapped: Vec<(Ident, Value)> = fields
        .into_iter()
        .map(|(k, v)| (Ident::new(k), v))
        .collect();
    Value::struct_(name, mapped)
}

/// Looks up a struct's field value by name, returning
/// `Value::Unit` if missing. Used by the derive's `FromGos`.
#[must_use]
pub fn struct_field<'a>(fields: &'a [(Ident, Value)], name: &str) -> &'a Value {
    fields
        .iter()
        .find_map(|(k, v)| if k.name == name { Some(v) } else { None })
        .unwrap_or(&Value::Unit)
}
