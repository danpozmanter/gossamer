//! Every binding-ABI shape that is converted at the boundary rather
//! than crossing as a bare word: `Bytes`, `Map<K, V>`, a tuple, and a
//! `#[derive(GosStruct)]` struct. The Gossamer side asserts the same
//! values on the bytecode VM, the JIT, and the native build.

use std::collections::HashMap;

use gossamer_binding::native::{BindingAbi, GosDynVariant};
use gossamer_binding::{Bytes, DynValue, GosStruct, Type, VariantArm, gos_module};

/// A closed arm set. The binding declares the arms it can answer; the program
/// matches them as the ordinary Gossamer enum that spells the same names, so
/// the arm table is an ABI input rather than a second kind of type.
pub struct Reply(DynValue);

impl Reply {
    fn integer(n: i64) -> Self {
        Self(DynValue::Tagged {
            name: "Integer".to_string(),
            payload: ::std::vec![DynValue::Int(n)],
        })
    }
    fn text(s: &str) -> Self {
        Self(DynValue::Tagged {
            name: "Text".to_string(),
            payload: ::std::vec![DynValue::String(s.to_string())],
        })
    }
    fn nothing() -> Self {
        Self(DynValue::Tagged {
            name: "Nothing".to_string(),
            payload: ::std::vec![],
        })
    }
}

impl gossamer_binding::SigType for Reply {
    const TYPE: Type = <Reply as BindingAbi>::TYPE;
}

impl gossamer_binding::ToGos for Reply {
    fn to_gos(self) -> gossamer_binding::value::Value {
        <DynValue as gossamer_binding::ToGos>::to_gos(self.0)
    }
}

impl gossamer_binding::FromGos for Reply {
    fn from_gos(
        value: &gossamer_binding::value::Value,
    ) -> gossamer_binding::value::RuntimeResult<Self> {
        Ok(Self(<DynValue as gossamer_binding::FromGos>::from_gos(
            value,
        )?))
    }
}

impl BindingAbi for Reply {
    type Input = *const GosDynVariant;
    type Output = *mut GosDynVariant;
    const TYPE: Type = Type::Variant(&[
        VariantArm {
            name: "Integer",
            payload: &[Type::I64],
        },
        VariantArm {
            name: "Text",
            payload: &[Type::String],
        },
        VariantArm {
            name: "Nothing",
            payload: &[],
        },
    ]);

    unsafe fn from_input(input: *const GosDynVariant) -> Self {
        Self(unsafe { <DynValue as BindingAbi>::from_input(input) })
    }

    fn to_output(self) -> *mut GosDynVariant {
        <DynValue as BindingAbi>::to_output(self.0)
    }
}

#[derive(Default, Clone, GosStruct)]
pub struct Tagged {
    pub label: String,
    pub weight: f64,
    pub live: bool,
}

#[gos_module("shapes")]
mod bindings {
    use super::*;

    /// Reverses a byte buffer.
    pub fn reverse_bytes(b: Bytes) -> Bytes {
        let mut bytes = b.0;
        bytes.reverse();
        Bytes(bytes)
    }

    /// Counts occurrences, returning a map.
    pub fn counts(words: Vec<String>) -> HashMap<String, i64> {
        let mut out = HashMap::new();
        for word in words {
            *out.entry(word).or_insert(0) += 1;
        }
        out
    }

    /// Sums a map's values, reading one in the parameter position.
    pub fn map_total(m: HashMap<String, i64>) -> i64 {
        m.values().sum()
    }

    /// Rewrites a tuple, both directions.
    pub fn retuple(t: (i64, String, bool)) -> (i64, String, bool) {
        (t.0 * 2, t.1.to_uppercase(), !t.2)
    }

    /// A value whose shape the data decides, including an arm name built at
    /// run time and a nested arm.
    pub fn dynamic(pick: i64) -> DynValue {
        match pick {
            0 => DynValue::Nil,
            1 => DynValue::Bool(true),
            2 => DynValue::Int(-7),
            3 => DynValue::Float(1.5),
            4 => DynValue::Char('z'),
            5 => DynValue::String("hi".to_string()),
            6 => DynValue::Bytes(::std::vec![1, 2, 3]),
            7 => DynValue::List(::std::vec![DynValue::Int(1), DynValue::String("a".to_string())]),
            8 => DynValue::Tagged {
                name: format!("Row{pick}"),
                payload: ::std::vec![
                    DynValue::Int(9),
                    DynValue::Tagged {
                        name: "Inner".to_string(),
                        payload: ::std::vec![DynValue::Bool(false)],
                    },
                ],
            },
            _ => DynValue::Tagged {
                name: "Empty".to_string(),
                payload: ::std::vec![],
            },
        }
    }

    /// One arm of a declared set.
    pub fn reply(pick: i64) -> Reply {
        match pick {
            0 => Reply::integer(41),
            1 => Reply::text("hi"),
            _ => Reply::nothing(),
        }
    }

    /// Rewrites a struct, both directions.
    pub fn retag(t: Tagged) -> Tagged {
        Tagged {
            label: t.label.to_uppercase(),
            weight: t.weight * 2.0,
            live: !t.live,
        }
    }
}
