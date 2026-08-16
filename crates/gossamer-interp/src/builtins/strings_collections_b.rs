/// Bytes taken from a packed byte-sequence receiver, paired with the
/// constructor that rebuilds that receiver's own representation.
type PackedBytes = (Vec<u8>, fn(Vec<u8>) -> Value);

/// Bytes of a packed byte-sequence receiver plus the constructor that
/// rebuilds its own representation, so a mutator answers the same shape it
/// was handed. `None` for a receiver that is not a byte sequence.
///
/// `Vec<u8>` and `[u8; N]` reach a builtin as one of these packed forms
/// rather than a boxed `Array`, so a mutator that matches only `Array` /
/// `IntArray` / `FloatVec` would answer its receiver unchanged.
fn packed_bytes_receiver(recv: &Value) -> Option<PackedBytes> {
    match recv {
        Value::ByteVec(data) => Some((data.as_ref().clone(), |b| Value::ByteVec(Arc::new(b)))),
        Value::ByteArray(data) => {
            Some((data.to_vec(), |b| Value::ByteArray(Arc::new(b.into()))))
        }
        Value::InlineByteArray(data) => Some((data.to_vec(), |b| {
            Value::InlineByteArray(Arc::new(smallvec::SmallVec::from_vec(b)))
        })),
        _ => None,
    }
}

fn builtin_remove(args: &[Value]) -> RuntimeResult<Value> {
    if matches!(
        args.first(),
        Some(Value::Map(_) | Value::IntMap(_) | Value::StrIntMap(_))
    ) {
        return builtin_map_remove(args);
    }
    let idx = args
        .get(1)
        .ok_or(RuntimeError::Type("index must be integer".to_string()))?;
    let idx = crate::vm::index_value(idx)?;
    match args.first() {
        Some(Value::Array(parts)) => {
            let len = parts.len() as i64;
            if idx < 0 || idx >= len {
                return Err(RuntimeError::Panic(format!(
                    "remove: index {idx} out of bounds for length {len}"
                )));
            }
            let mut owned = parts.as_ref().clone();
            owned.remove(idx as usize);
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let len = data.len() as i64;
            if idx < 0 || idx >= len {
                return Err(RuntimeError::Panic(format!(
                    "remove: index {idx} out of bounds for length {len}"
                )));
            }
            let mut owned = data.as_ref().clone();
            owned.remove(idx as usize);
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let len = data.len() as i64;
            if idx < 0 || idx >= len {
                return Err(RuntimeError::Panic(format!(
                    "remove: index {idx} out of bounds for length {len}"
                )));
            }
            let mut owned = data.as_ref().clone();
            owned.remove(idx as usize);
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_clear(args: &[Value]) -> RuntimeResult<Value> {
    if matches!(
        args.first(),
        Some(Value::Map(_) | Value::IntMap(_) | Value::StrIntMap(_))
    ) {
        return builtin_map_clear(args);
    }
    match args.first() {
        Some(Value::Array(_)) => Ok(Value::empty_array()),
        Some(Value::IntArray(_)) => Ok(Value::IntArray(Arc::new(Vec::new()))),
        Some(Value::FloatVec(_)) => Ok(Value::FloatVec(Arc::new(Vec::new()))),
        Some(Value::String(_)) => Ok(Value::String(SmolStr::from(String::new()))),
        Some(v) if packed_bytes_receiver(v).is_some() => Ok(Value::ByteVec(Arc::new(Vec::new()))),
        _ => Ok(args.first().cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_extend(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            if let Some(extra) = args.get(1).and_then(array_as_values) {
                owned.extend(extra);
            }
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            if let Some(extra) = args.get(1).and_then(array_as_values) {
                owned.extend(extra.into_iter().filter_map(|v| match v {
                    Value::Int(n) => Some(n),
                    _ => None,
                }));
            }
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            if let Some(extra) = args.get(1).and_then(array_as_values) {
                owned.extend(extra.into_iter().filter_map(|v| match v {
                    Value::Float(f) => Some(f),
                    Value::Int(n) => Some(n as f64),
                    _ => None,
                }));
            }
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(v) => {
            let Some((mut owned, rebuild)) = packed_bytes_receiver(v) else {
                return Ok(v.clone());
            };
            if let Some(extra) = args.get(1).and_then(array_as_values) {
                owned.extend(
                    extra
                        .into_iter()
                        .filter_map(|v| crate::builtins::value_to_int(&v))
                        .map(|n| n as u8),
                );
            }
            Ok(rebuild(owned))
        }
        None => Ok(Value::Unit),
    }
}

fn builtin_truncate(args: &[Value]) -> RuntimeResult<Value> {
    let cap = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        Some(Value::Int(_)) => {
            return Err(RuntimeError::Type(
                "truncate: length must be non-negative".to_string(),
            ));
        }
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            owned.truncate(cap);
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            owned.truncate(cap);
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            owned.truncate(cap);
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(Value::String(s)) => {
            let end = s
                .char_indices()
                .map(|(idx, _)| idx)
                .chain(std::iter::once(s.len()))
                .take_while(|idx| *idx <= cap)
                .last()
                .unwrap_or(0);
            Ok(Value::String(SmolStr::from(&s[..end])))
        }
        Some(v) => {
            let Some((mut owned, rebuild)) = packed_bytes_receiver(v) else {
                return Ok(v.clone());
            };
            owned.truncate(cap);
            Ok(rebuild(owned))
        }
        None => Ok(Value::Unit),
    }
}

fn builtin_vec_reserve(args: &[Value]) -> RuntimeResult<Value> {
    let min_capacity = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        Some(Value::Int(_)) => {
            return Err(RuntimeError::Type(
                "reserve: capacity must be non-negative".to_string(),
            ));
        }
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::ByteArray(data)) => {
            let mut owned = data.to_vec();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::InlineByteArray(data)) => {
            let mut owned = data.to_vec();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::ByteVec(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve(min_capacity - owned.capacity());
            }
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        other => Ok(other.cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_vec_reserve_exact(args: &[Value]) -> RuntimeResult<Value> {
    let min_capacity = match args.get(1) {
        Some(Value::Int(n)) if *n >= 0 => *n as usize,
        Some(Value::Int(_)) => {
            return Err(RuntimeError::Type(
                "reserve_exact: capacity must be non-negative".to_string(),
            ));
        }
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::ByteArray(data)) => {
            let mut owned = data.to_vec();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::InlineByteArray(data)) => {
            let mut owned = data.to_vec();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::ByteVec(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            if min_capacity > owned.capacity() {
                owned.reserve_exact(min_capacity - owned.capacity());
            }
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        other => Ok(other.cloned().unwrap_or(Value::Unit)),
    }
}

fn builtin_vec_capacity(args: &[Value]) -> RuntimeResult<Value> {
    let cap = match args.first() {
        Some(Value::Array(parts)) => parts.capacity(),
        Some(Value::IntArray(data)) => data.capacity(),
        Some(Value::ByteArray(data)) => data.len(),
        Some(Value::InlineByteArray(data)) => data.capacity(),
        Some(Value::ByteVec(data)) => data.capacity(),
        Some(Value::FloatVec(data)) => data.capacity(),
        Some(rx @ Value::FloatArray(_)) => match rx.float_array_to_value_array() {
            Value::Array(items) => items.capacity(),
            _ => 0,
        },
        _ => 0,
    };
    Ok(Value::Int(i64::try_from(cap).unwrap_or(i64::MAX)))
}

fn builtin_sort(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            owned.sort_by(crate::stdlib_builtins::iter::compare_values_total);
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            owned.sort_unstable();
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            owned.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(v) => {
            let Some((mut owned, rebuild)) = packed_bytes_receiver(v) else {
                return Ok(v.clone());
            };
            owned.sort_unstable();
            Ok(rebuild(owned))
        }
        None => Ok(Value::Unit),
    }
}

fn builtin_reverse(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            owned.reverse();
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            owned.reverse();
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            owned.reverse();
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(v) => {
            let Some((mut owned, rebuild)) = packed_bytes_receiver(v) else {
                return Ok(v.clone());
            };
            owned.reverse();
            Ok(rebuild(owned))
        }
        None => Ok(Value::Unit),
    }
}

fn builtin_swap(args: &[Value]) -> RuntimeResult<Value> {
    fn oob(i: i64, j: i64, len: usize) -> RuntimeError {
        RuntimeError::Panic(format!(
            "swap: indexes {i} and {j} out of bounds for length {len}"
        ))
    }
    let raw_i = match args.get(1) {
        Some(Value::Int(n)) => *n,
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    let raw_j = match args.get(2) {
        Some(Value::Int(n)) => *n,
        _ => return Ok(args.first().cloned().unwrap_or(Value::Unit)),
    };
    // `swap` is an indexed write, so an index outside `[0, len)` panics
    // exactly as `xs[i] = v` does rather than leaving the receiver untouched.
    let swapped = |len: usize| -> RuntimeResult<(usize, usize)> {
        if raw_i < 0 || raw_j < 0 || raw_i as usize >= len || raw_j as usize >= len {
            return Err(oob(raw_i, raw_j, len));
        }
        Ok((raw_i as usize, raw_j as usize))
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            let (i, j) = swapped(owned.len())?;
            owned.swap(i, j);
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(parts)) => {
            let mut owned = parts.as_ref().clone();
            let (i, j) = swapped(owned.len())?;
            owned.swap(i, j);
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(parts)) => {
            let mut owned = parts.as_ref().clone();
            let (i, j) = swapped(owned.len())?;
            owned.swap(i, j);
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(other) => Ok(other.clone()),
        None => Ok(Value::Unit),
    }
}

fn builtin_fill(args: &[Value]) -> RuntimeResult<Value> {
    let Some(value) = args.get(1) else {
        return Ok(args.first().cloned().unwrap_or(Value::Unit));
    };
    match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            owned.fill(value.clone());
            Ok(Value::Array(Arc::new(owned)))
        }
        Some(Value::IntArray(parts)) => {
            let Value::Int(value) = value else {
                return Err(RuntimeError::Type(
                    "fill expects an integer element".to_string(),
                ));
            };
            let mut owned = parts.as_ref().clone();
            owned.fill(*value);
            Ok(Value::IntArray(Arc::new(owned)))
        }
        Some(Value::FloatVec(parts)) => {
            let Value::Float(value) = value else {
                return Err(RuntimeError::Type(
                    "fill expects a float element".to_string(),
                ));
            };
            let mut owned = parts.as_ref().clone();
            owned.fill(*value);
            Ok(Value::FloatVec(Arc::new(owned)))
        }
        Some(Value::ByteArray(parts)) => {
            let Value::Int(value) = value else {
                return Err(RuntimeError::Type("fill expects a byte element".to_string()));
            };
            let byte = u8::try_from(*value)
                .map_err(|_| {
                    RuntimeError::Type("fill byte must be in the range 0..=255".to_string())
                })?;
            let mut owned = parts.as_ref().clone();
            owned.fill(byte);
            Ok(Value::ByteArray(Arc::new(owned)))
        }
        Some(Value::InlineByteArray(parts)) => {
            let Value::Int(value) = value else {
                return Err(RuntimeError::Type("fill expects a byte element".to_string()));
            };
            let byte = u8::try_from(*value)
                .map_err(|_| {
                    RuntimeError::Type("fill byte must be in the range 0..=255".to_string())
                })?;
            let mut owned = parts.as_ref().clone();
            owned.fill(byte);
            Ok(Value::InlineByteArray(Arc::new(owned)))
        }
        Some(Value::ByteVec(parts)) => {
            let Value::Int(value) = value else {
                return Err(RuntimeError::Type("fill expects a byte element".to_string()));
            };
            let byte = u8::try_from(*value)
                .map_err(|_| {
                    RuntimeError::Type("fill byte must be in the range 0..=255".to_string())
                })?;
            let mut owned = parts.as_ref().clone();
            owned.fill(byte);
            Ok(Value::ByteVec(Arc::new(owned)))
        }
        Some(other) => Ok(other.clone()),
        None => Ok(Value::Unit),
    }
}

fn builtin_clone(args: &[Value]) -> RuntimeResult<Value> {
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

/// `x.downgrade()` - produce a non-owning [`Value::Weak`] observing the
/// receiver's allocation.
fn builtin_downgrade(args: &[Value]) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    Ok(Value::Weak(crate::value::WeakValue::downgrade(&receiver)))
}

/// `w.upgrade()` - `Some(value)` while the referent is alive, else `None`.
fn builtin_upgrade(args: &[Value]) -> RuntimeResult<Value> {
    let some = |v: Value| Value::variant("Some", vec![v]);
    let none = Value::variant("None", vec![]);
    match args.first() {
        Some(Value::Weak(w)) => Ok(w.upgrade().map_or(none, some)),
        // A non-weak receiver can reach here only through an untyped
        // dispatch path; treat it as a live identity upgrade so the
        // value round-trips rather than vanishing.
        Some(other) => Ok(some(other.clone())),
        None => Ok(none),
    }
}

/// `it.next()` - returns `Some(first)` for non-empty
/// collection-shaped receivers and `None` for empty. The
/// for-loop fast paths handle real iterator state; this binding
/// covers user code that calls `.next()` once outside a
/// for-loop, and standalone `xs.iter().next()` shapes.
fn builtin_next(args: &[Value]) -> RuntimeResult<Value> {
    let none = Value::variant("None", vec![]);
    let some = |v: Value| Value::variant("Some", vec![v]);
    match args.first() {
        Some(Value::Array(items)) => {
            if let Some(first) = items.first() {
                Ok(some(first.clone()))
            } else {
                Ok(none)
            }
        }
        Some(Value::IntArray(items)) => {
            if let Some(first) = items.first() {
                Ok(some(Value::Int(*first)))
            } else {
                Ok(none)
            }
        }
        Some(Value::FloatVec(items)) => {
            if let Some(first) = items.first() {
                Ok(some(Value::Float(*first)))
            } else {
                Ok(none)
            }
        }
        Some(Value::Tuple(items)) => {
            if let Some(first) = items.first() {
                Ok(some(first.clone()))
            } else {
                Ok(none)
            }
        }
        Some(Value::String(s)) => {
            if let Some(c) = s.as_str().chars().next() {
                Ok(some(Value::Char(c)))
            } else {
                Ok(none)
            }
        }
        Some(value @ Value::LazyIter(_)) => {
            Ok(crate::stdlib_builtins::iter::lazy_iter_next_value(value)?.map_or_else(
                || none.clone(),
                some,
            ))
        }
        _ => Ok(none),
    }
}

fn builtin_path_join_v(args: &[Value]) -> RuntimeResult<Value> {
    let mut parts: Vec<String> = Vec::with_capacity(args.len());
    for a in args {
        if let Value::String(s) = a {
            parts.push(s.as_str().to_string());
        }
    }
    let mut p = std::path::PathBuf::new();
    for part in &parts {
        p.push(part);
    }
    Ok(Value::String(SmolStr::from(
        p.to_string_lossy().into_owned(),
    )))
}

fn builtin_btmap_new(args: &[Value]) -> RuntimeResult<Value> {
    builtin_map_new(args)
}

fn builtin_set_new(args: &[Value]) -> RuntimeResult<Value> {
    builtin_map_new(args)
}

fn builtin_duration_passthrough(args: &[Value]) -> RuntimeResult<Value> {
    Ok(args.first().cloned().unwrap_or(Value::Int(0)))
}

fn builtin_duration_secs_to_ms(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Int(n)) => Ok(Value::Int(n.saturating_mul(1000))),
        _ => Ok(Value::Int(0)),
    }
}

fn builtin_to_vec_v(args: &[Value]) -> RuntimeResult<Value> {
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

fn builtin_json_value_passthrough(args: &[Value]) -> RuntimeResult<Value> {
    Ok(args.first().cloned().unwrap_or(Value::Unit))
}

fn builtin_json_value_null(_args: &[Value]) -> RuntimeResult<Value> {
    Ok(Value::Unit)
}

fn builtin_json_value_object(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Array(parts)) = args.first() else {
        return Ok(Value::struct_(
            "json::Object",
            Arc::unwrap_or_clone(Arc::new(Vec::new())),
        ));
    };
    let mut fields: Vec<(&'static str, Value)> = Vec::with_capacity(parts.len());
    for entry in parts.iter() {
        let Value::Tuple(pair) = entry else { continue };
        if pair.len() < 2 {
            continue;
        }
        let key = match &pair[0] {
            Value::String(s) => s.as_str().to_string(),
            other => format!("{other:?}"),
        };
        fields.push((crate::value::intern_type_name(&key), pair[1].clone()));
    }
    Ok(Value::struct_(
        "json::Object",
        Arc::unwrap_or_clone(Arc::new(fields)),
    ))
}

/// `json::set(obj, key, value) -> json::Value` - append or
/// replace the named field on a `json::Value::object()`-shaped
/// receiver and return the updated value. Non-object receivers
/// fall through unchanged so callers don't have to special-case
/// `Null` / arrays. Mirrors the surface documented in the SKILL
/// card and used by askq's chat-round assembly.
fn builtin_json_set(args: &[Value]) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let key = match args.get(1) {
        Some(Value::String(s)) => s.as_str().to_string(),
        Some(other) => format!("{other}"),
        None => return Ok(receiver),
    };
    let value = args.get(2).cloned().unwrap_or(Value::Unit);
    if let Value::Json(json) = &receiver {
        let json_std::Value::Object(entries) = json.as_value() else {
            return Ok(receiver);
        };
        let mut updated = entries.clone();
        updated.insert(key, gossamer_to_json_value(&value));
        return Ok(Value::Json(Arc::new(JsonInner::new(
            json_std::Value::Object(updated),
        ))));
    }
    let Value::Struct(inner) = &receiver else {
        return Ok(receiver);
    };
    // `json::parse` builds objects named `Object`; the `Value::object`
    // constructor builds `json::Object`. Both are object-shaped.
    if inner.name != "json::Object" && inner.name != "Object" {
        return Ok(receiver);
    }
    let mut fields: Vec<(&'static str, Value)> = inner.fields.to_vec();
    if let Some(slot) = fields.iter_mut().find(|(name, _)| *name == key) {
        slot.1 = value;
    } else {
        fields.push((crate::value::intern_type_name(&key), value));
    }
    Ok(Value::struct_(
        "json::Object",
        Arc::unwrap_or_clone(Arc::new(fields)),
    ))
}

fn builtin_variant_unwrap(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Ok" || inner.name == "Some" => {
            inner.fields.first().cloned().ok_or_else(|| {
                RuntimeError::Panic(format!("unwrap on empty `{}` variant", inner.name))
            })
        }
        Some(Value::Variant(inner)) => Err(RuntimeError::Panic(format!(
            "unwrap on `{}` variant: {}",
            inner.name,
            inner
                .fields
                .first()
                .map(|v| format!("{v}"))
                .unwrap_or_default()
        ))),
        Some(other) => Ok(other.clone()),
        None => Err(RuntimeError::Panic("unwrap without receiver".to_string())),
    }
}

fn builtin_variant_unwrap_or(args: &[Value]) -> RuntimeResult<Value> {
    let default = args.get(1).cloned().unwrap_or(Value::Unit);
    match args.first() {
        Some(Value::Variant(inner))
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        _ => Ok(default),
    }
}

fn native_variant_unwrap_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let fallback = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        Value::Variant(inner) if inner.name == "Err" => {
            let err_value = inner.fields.first().cloned().unwrap_or(Value::Unit);
            invoke_callable(dispatch, &fallback, vec![err_value])
        }
        _ => invoke_callable(dispatch, &fallback, Vec::new()),
    }
}

fn builtin_variant_unwrap_or_default(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner))
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(inner.fields[0].clone())
        }
        _ => Ok(Value::Unit),
    }
}

fn builtin_variant_is<const TAG: char>(args: &[Value]) -> RuntimeResult<Value> {
    let want = match TAG {
        'S' => "Some",
        'N' => "None",
        'O' => "Ok",
        'E' => "Err",
        _ => return Ok(Value::Bool(false)),
    };
    let is = matches!(
        args.first(),
        Some(Value::Variant(inner)) if inner.name == want
    );
    Ok(Value::Bool(is))
}

fn builtin_variant_ok(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Ok" && !inner.fields.is_empty() => {
            Ok(some_variant(inner.fields[0].clone()))
        }
        _ => Ok(none_variant()),
    }
}

fn builtin_variant_err(args: &[Value]) -> RuntimeResult<Value> {
    match args.first() {
        Some(Value::Variant(inner)) if inner.name == "Err" && !inner.fields.is_empty() => {
            Ok(some_variant(inner.fields[0].clone()))
        }
        _ => Ok(none_variant()),
    }
}

/// `result.ok_or(new_err)` / `option.ok_or(new_err)`. Replaces a
/// missing-value variant (Err / None) with `Err(new_err)`; passes
/// the success variant through unchanged.
fn builtin_variant_ok_or(args: &[Value]) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let new_err = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Ok" || inner.name == "Some") && !inner.fields.is_empty() =>
        {
            Ok(Value::variant("Ok", vec![inner.fields[0].clone()]))
        }
        _ => Ok(Value::variant("Err", vec![new_err])),
    }
}

/// `opt.and_then(f)` / `res.and_then(f)` - `f(payload)` when
/// Some/Ok (f returns the next Option/Result), None/Err passthrough.
fn native_variant_and_then(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            invoke_callable(dispatch, &f, vec![inner.fields[0].clone()])
        }
        other => Ok(other.clone()),
    }
}

/// `opt.or_else(f)` (f takes no argument) / `res.or_else(f)` (f takes
/// the Err payload) - Some/Ok passthrough.
fn native_variant_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner) if inner.name == "None" => invoke_callable(dispatch, &f, Vec::new()),
        Value::Variant(inner) if inner.name == "Err" && !inner.fields.is_empty() => {
            invoke_callable(dispatch, &f, vec![inner.fields[0].clone()])
        }
        other => Ok(other.clone()),
    }
}

/// `opt.filter(p)` - keeps `Some(x)` only when `p(x)` holds.
fn native_variant_filter(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let p = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner) if inner.name == "Some" && !inner.fields.is_empty() => {
            if matches!(
                invoke_callable(dispatch, &p, vec![inner.fields[0].clone()])?,
                Value::Bool(true)
            ) {
                Ok(receiver.clone())
            } else {
                Ok(none_variant())
            }
        }
        other => Ok(other.clone()),
    }
}

/// `opt.ok_or_else(f)` - `Ok(payload)` when Some, `Err(f())` when None.
fn native_variant_ok_or_else(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let f = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            Ok(Value::variant("Ok", vec![inner.fields[0].clone()]))
        }
        _ => Ok(Value::variant(
            "Err",
            vec![invoke_callable(dispatch, &f, Vec::new())?],
        )),
    }
}

fn native_variant_map(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let transform = args.get(1).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            let mapped = invoke_callable(dispatch, &transform, vec![inner.fields[0].clone()])?;
            Ok(Value::variant(inner.name.clone(), vec![mapped]))
        }
        other => Ok(other.clone()),
    }
}

fn native_variant_map_or(
    dispatch: &mut dyn NativeDispatch,
    args: &[Value],
) -> RuntimeResult<Value> {
    let receiver = args.first().cloned().unwrap_or(Value::Unit);
    let default = args.get(1).cloned().unwrap_or(Value::Unit);
    let mapper = args.get(2).cloned().unwrap_or(Value::Unit);
    match &receiver {
        Value::Variant(inner)
            if (inner.name == "Some" || inner.name == "Ok") && !inner.fields.is_empty() =>
        {
            invoke_callable(dispatch, &mapper, vec![inner.fields[0].clone()])
        }
        _ => Ok(default),
    }
}

fn invoke_callable(
    dispatch: &mut dyn NativeDispatch,
    callable: &Value,
    args: Vec<Value>,
) -> RuntimeResult<Value> {
    dispatch.call_value(callable, args)
}

/// `arr.sort_by(|a, b| ordering)` - drives Rust's `sort_by` with a
/// Gossamer comparator. The comparator returns an i64 (negative
/// if a < b, zero if equal, positive if a > b), matching Rust's
/// `Ordering::cmp`. Falls back to identity when the receiver isn't
/// an array or the second arg isn't callable.
fn native_sort_by(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let comparator = args.get(1).cloned().unwrap_or(Value::Unit);
    let mut cmp_with = |a: Value, b: Value, sort_err: &mut Option<RuntimeError>| {
        if sort_err.is_some() {
            return std::cmp::Ordering::Equal;
        }
        match invoke_callable(dispatch, &comparator, vec![a, b]) {
            Ok(Value::Int(n)) => n.cmp(&0),
            Ok(Value::Float(f)) => f.partial_cmp(&0.0).unwrap_or(std::cmp::Ordering::Equal),
            Ok(_) => std::cmp::Ordering::Equal,
            Err(e) => {
                *sort_err = Some(e);
                std::cmp::Ordering::Equal
            }
        }
    };
    let mut sort_err: Option<RuntimeError> = None;
    let result = match args.first() {
        Some(Value::Array(parts)) => {
            let mut owned = parts.as_ref().clone();
            owned.sort_by(|a, b| cmp_with(a.clone(), b.clone(), &mut sort_err));
            Value::Array(Arc::new(owned))
        }
        Some(Value::IntArray(data)) => {
            let mut owned = data.as_ref().clone();
            owned.sort_by(|a, b| cmp_with(Value::Int(*a), Value::Int(*b), &mut sort_err));
            Value::IntArray(Arc::new(owned))
        }
        Some(Value::FloatVec(data)) => {
            let mut owned = data.as_ref().clone();
            owned.sort_by(|a, b| cmp_with(Value::Float(*a), Value::Float(*b), &mut sort_err));
            Value::FloatVec(Arc::new(owned))
        }
        other => return Ok(other.cloned().unwrap_or(Value::Unit)),
    };
    if let Some(err) = sort_err {
        return Err(err);
    }
    Ok(result)
}

fn native_spawn(dispatch: &mut dyn NativeDispatch, args: &[Value]) -> RuntimeResult<Value> {
    let Some(callable) = args.first().cloned() else {
        return Ok(Value::Unit);
    };
    let rest = args.iter().skip(1).cloned().collect();
    // `spawn(f)` returns a join handle (a one-shot channel) whose
    // `.join()` blocks for the goroutine's `Result<T, String>`.
    dispatch.spawn_join(callable, rest)
}

/// `handle.join() -> Result<T, String>` - blocks on the one-shot
/// handle channel for the spawned goroutine's outcome variant.
fn builtin_channel_join(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Channel(channel)) = args.first() else {
        return Ok(Value::variant(
            "Err",
            vec![Value::String("join on a non-handle value".into())],
        ));
    };
    let outcome = channel.recv();
    // Joining is how a child's outcome reaches the program, so a failure
    // read here is not one the root cohort reports as unobserved at exit.
    // Marked after the outcome arrives, by which point the child has
    // already recorded the failure.
    crate::stdlib_builtins::cohort::mark_handle_observed(channel.identity());
    match outcome {
        crate::value::RecvOutcome::Value(outcome) => Ok(outcome),
        crate::value::RecvOutcome::Closed => Ok(Value::variant(
            "Err",
            vec![Value::String("join: handle channel closed".into())],
        )),
        crate::value::RecvOutcome::Deadlocked => Err(crate::value::deadlock_error("join")),
    }
}

fn builtin_testing_check(args: &[Value]) -> RuntimeResult<Value> {
    let cond = matches!(args.first(), Some(Value::Bool(true)));
    let message = args.get(1).and_then(as_str).unwrap_or("check failed");
    let location = current_assertion_location()
        .map(|s| format!(" at {s}"))
        .unwrap_or_default();
    observe_assertion(cond, format!("check: {message}{location}"));
    if cond {
        Ok(ok_variant(Value::Unit))
    } else {
        Ok(err_variant(format!("assertion failed: {message}")))
    }
}

fn builtin_testing_check_eq(args: &[Value]) -> RuntimeResult<Value> {
    let left = args.first().cloned().unwrap_or(Value::Unit);
    let right = args.get(1).cloned().unwrap_or(Value::Unit);
    let message = args.get(2).and_then(as_str).unwrap_or("check_eq failed");
    let ok = values_equal_for_assertion(&left, &right);
    let location = current_assertion_location()
        .map(|s| format!(" at {s}"))
        .unwrap_or_default();
    // `{:?}` (Debug) wraps strings in quotes so a failing
    // `"foo "` vs `"foo"` (trailing space) is visible. Bare
    // `Display` would render them identically.
    observe_assertion(
        ok,
        format!("check_eq: {message}{location}: left={left:?}, right={right:?}"),
    );
    if ok {
        Ok(ok_variant(Value::Unit))
    } else {
        Ok(err_variant(format!(
            "{message}: left={left:?}, right={right:?}"
        )))
    }
}

fn builtin_testing_check_ok(args: &[Value]) -> RuntimeResult<Value> {
    let result = args.first().cloned().unwrap_or(Value::Unit);
    let message = args.get(1).and_then(as_str).unwrap_or("check_ok failed");
    match &result {
        Value::Variant(inner) if inner.name == "Ok" && !inner.fields.is_empty() => {
            observe_assertion(true, format!("check_ok: {message}"));
            Ok(ok_variant(inner.fields[0].clone()))
        }
        Value::Variant(inner) if inner.name == "Err" => {
            let msg = inner
                .fields
                .first()
                .map(|v| format!("{v}"))
                .unwrap_or_default();
            observe_assertion(false, format!("check_ok: {message}: {msg}"));
            Ok(err_variant(format!("{message}: {msg}")))
        }
        other => {
            observe_assertion(
                false,
                format!("check_ok: {message}: not a Result variant: {other}"),
            );
            Ok(err_variant(format!(
                "{message}: expected Result, got {other}"
            )))
        }
    }
}

fn builtin_testing_wait_for_scheduler_idle(args: &[Value]) -> RuntimeResult<Value> {
    let timeout_ms = match args.first().and_then(value_to_int) {
        Some(n) if n < 0 => {
            return Err(RuntimeError::Type(
                "testing::wait_for_scheduler_idle: timeout_ms must be non-negative".to_string(),
            ));
        }
        Some(n) => n,
        None => 1000,
    };
    let deadline = std::time::Instant::now()
        + std::time::Duration::from_millis(u64::try_from(timeout_ms).unwrap_or(0));
    let scheduler = gossamer_runtime::sched_global::scheduler();
    loop {
        let stats = scheduler.stats();
        if scheduler.live_goroutines() == 0 && stats.spawned == stats.finished {
            return Ok(Value::Bool(true));
        }
        if std::time::Instant::now() >= deadline {
            return Ok(Value::Bool(false));
        }
        std::thread::sleep(std::time::Duration::from_millis(1));
    }
}

/// Structural equality check used by `testing::check_eq`; more
/// forgiving than `==` on values since it walks aggregates instead
/// of bailing out on type mismatch. Returns `false` rather than an
/// error on cross-kind operands.
fn values_equal_for_assertion(a: &Value, b: &Value) -> bool {
    // `assert_eq` reports what `==` reports. A sequence has several runtime
    // representations - a grown Vec and a literal of the same elements do not
    // share one - so the structural comparison behind `==` decides here too.
    if crate::vm::values_equal(a, b) {
        return true;
    }
    match (a, b) {
        (Value::Unit, Value::Unit) => true,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Int(x), Value::Int(y)) => x == y,
        (Value::Float(x), Value::Float(y)) => x == y,
        (Value::Char(x), Value::Char(y)) => x == y,
        (Value::String(x), Value::String(y)) => x == y,
        (Value::Tuple(x), Value::Tuple(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| values_equal_for_assertion(a, b))
        }
        (Value::Array(x), Value::Array(y)) => {
            x.len() == y.len()
                && x.iter()
                    .zip(y.iter())
                    .all(|(a, b)| values_equal_for_assertion(a, b))
        }
        (Value::Variant(a), Value::Variant(b)) => {
            a.name == b.name
                && a.fields.len() == b.fields.len()
                && a.fields
                    .iter()
                    .zip(b.fields.iter())
                    .all(|(x, y)| values_equal_for_assertion(x, y))
        }
        _ => false,
    }
}

fn builtin_struct_new(args: &[Value]) -> RuntimeResult<Value> {
    let name: String = match args.first() {
        Some(Value::String(s)) => s.as_str().to_string(),
        _ => String::new(),
    };
    // Collect (field_name, value) pairs in source-literal order.
    // The synthetic `"__base"` key carries the functional-update
    // base value (from `Outer { n: 99, ..base }`); strip it out and
    // remember it so missing fields fill from `base.field` below.
    let mut pairs: Vec<(String, Value)> = Vec::with_capacity(args.len() / 2);
    let mut base: Option<Value> = None;
    let mut iter = args.iter().skip(1);
    while let (Some(key), Some(value)) = (iter.next(), iter.next()) {
        let Value::String(field_name) = key else {
            continue;
        };
        if field_name.as_str() == "__base" {
            base = Some(value.clone());
            continue;
        }
        pairs.push((field_name.to_string(), value.clone()));
    }
    let lookup_base_field = |field_name: &str| -> Option<Value> {
        match &base {
            Some(Value::Struct(inner)) => inner
                .fields
                .iter()
                .find(|(n, _)| (**n) == field_name)
                .map(|(_, v)| v.clone()),
            _ => None,
        }
    };
    // Reorder to declaration order when the struct's layout is
    // known. This makes every `Value::Struct { name: "Body" }`
    // share the same `fields[i]` layout, which lets the VM
    // emit compile-time offsets for field reads instead of
    // doing a linear name scan per read.
    let fields: Vec<(&'static str, Value)> = STRUCT_LAYOUTS.with(|cell| {
        let layouts = cell.borrow();
        if let Some(order) = layouts.get(&name) {
            let mut out: Vec<(&'static str, Value)> = Vec::with_capacity(order.len());
            for field_name in order {
                let value = pairs
                    .iter()
                    .find(|(n, _)| n.as_str() == *field_name)
                    .map(|(_, v)| v.clone())
                    .or_else(|| lookup_base_field(field_name))
                    .unwrap_or(Value::Unit);
                out.push((*field_name, value));
            }
            // Preserve any extra fields present in the literal
            // but not declared (should be rare; keeps program
            // state visible for debugging).
            for (n, v) in &pairs {
                // `order` holds `&'static str` but `n.as_str()` is a non-static
                // borrow, so `[T]::contains` (which wants `&&'static str`) does
                // not typecheck; the manual `any` is the only well-typed form.
                #[allow(clippy::manual_contains)]
                let declared = order.iter().any(|o| *o == n.as_str());
                if !declared {
                    out.push((crate::value::intern_type_name(n.as_str()), v.clone()));
                }
            }
            out
        } else if base.is_some() {
            // Layout unknown but a base is provided: start from the
            // base's fields and overlay the explicit overrides.
            let mut out: Vec<(&'static str, Value)> = match &base {
                Some(Value::Struct(inner)) => {
                    inner.fields.iter().map(|(n, v)| (*n, v.clone())).collect()
                }
                _ => Vec::new(),
            };
            for (n, v) in &pairs {
                if let Some(slot) = out.iter_mut().find(|(name, _)| *name == n.as_str()) {
                    slot.1 = v.clone();
                } else {
                    out.push((crate::value::intern_type_name(n.as_str()), v.clone()));
                }
            }
            out
        } else {
            pairs
                .into_iter()
                .map(|(n, v)| (crate::value::intern_type_name(n.as_str()), v))
                .collect()
        }
    });
    Ok(Value::struct_(name, Arc::unwrap_or_clone(Arc::new(fields))))
}

fn builtin_channel_new(args: &[Value]) -> RuntimeResult<Value> {
    // `channel()` / `channel(0)` is an unbuffered rendezvous channel,
    // matching Go's zero-capacity channel. `channel(N)` for positive N
    // is bounded. Use `channel::unbounded()` for the old queue form.
    let capacity = match args.first() {
        Some(Value::Int(n)) if *n < 0 => {
            return Err(RuntimeError::Type(
                "channel: capacity must be non-negative".to_string(),
            ));
        }
        Some(Value::Int(n)) if *n > 0 => *n as usize,
        _ => 0,
    };
    let channel = crate::value::Channel::with_capacity(capacity);
    let sender = Value::Channel(channel.clone());
    let receiver = Value::Channel(channel);
    Ok(Value::Tuple(Arc::from(vec![sender, receiver])))
}

fn builtin_channel_unbounded(_args: &[Value]) -> RuntimeResult<Value> {
    let channel = crate::value::Channel::unbounded();
    let sender = Value::Channel(channel.clone());
    let receiver = Value::Channel(channel);
    Ok(Value::Tuple(Arc::from(vec![sender, receiver])))
}

fn builtin_channel_send(args: &[Value]) -> RuntimeResult<Value> {
    let Some(Value::Channel(channel)) = args.first() else {
        return Err(RuntimeError::Type(
            "send: receiver must be a channel".to_string(),
        ));
    };
    let value = args.get(1).cloned().unwrap_or(Value::Unit);
    if channel.send(value) {
        Ok(Value::Unit)
    } else {
        Err(crate::value::deadlock_error("send"))
    }
}

