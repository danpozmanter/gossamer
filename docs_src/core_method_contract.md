# Core method contract

This page defines the small inherent-method surface Gossamer treats as core.
Items listed as shipped must work consistently in `gos run`, forced-JIT
execution, and `gos build --release` unless a row explicitly says otherwise.

The contract is intentionally narrow. A method belongs here only when the
interpreter, MIR lowering, compiled runtime ABI, docs, and parity tests agree.

## Strings

Shipped:

| Method or associated function | Returns | Notes |
|---|---|---|
| `String::new()` | `String` | Empty owned string. |
| `String::with_capacity(n)` | `String` | Capacity hint is accepted for Rust familiarity; current string values are immutable/runtime-owned, so the hint is advisory. |
| `String::from(value)` | `String` | Identity for strings; display conversion for scalar values. |
| `String::from_utf8(bytes)` | `Result<String, errors::Error>` | Decodes a byte vector as UTF-8; invalid sequences return `Err`. |
| `s.len()` | `i64` | Byte length. |
| `s.is_empty()` | `bool` | Equivalent to `s.len() == 0`. |
| `s.clear()` | `()` | Mutating-method writeback replaces the string with `""`. |
| `s.truncate(n)` | `()` | Mutating-method writeback keeps at most `n` bytes, clamped to a valid UTF-8 boundary. |
| `s.push(ch)` / `s.push_char(ch)` | `()` | Mutating-method writeback appends a Unicode scalar. |
| `s.push_byte(b)` | `()` | Appends the byte as the matching Unicode scalar. |
| `s.push_str(t)` | `()` | Appends string contents. |
| `s.chars()` | `[char]` | Unicode scalar values. |
| `s.as_bytes()` | `&[u8]` | Zero-copy byte view. |
| `s.as_str()` | `&str` | Zero-copy string view. |
| `s.clone()` / `s.to_string()` | `String` | Owned string value. |

## Vectors

Shipped:

| Method or associated function | Returns | Notes |
|---|---|---|
| `Vec::new()` | `[T]` | Empty vector. |
| `Vec::with_capacity(n)` | `[T]` | Preallocates in compiled tiers; interpreter accepts the hint but does not expose capacity. |
| `v.push(item)` | `()` | Mutates by method writeback. |
| `v.pop()` | `Option<T>` | Last element, or `None`. |
| `v.clear()` | `()` | Drops all elements and preserves the current allocation where the tier can. |
| `v.truncate(n)` | `()` | Drops elements after `n`; negative lengths clamp to `0`. |
| `v.extend(xs)` / `v.extend_from_slice(xs)` | `()` | Copies elements from another vector or inline array with matching element layout. |
| `v.len()` | `i64` | Element count. |
| `v.is_empty()` | `bool` | Equivalent to `v.len() == 0`. |
| `v.first()` / `v.last()` | `Option<T>` | |
| `v.insert(i, item)` / `v.remove(i)` | `Result<_, errors::Error>` | Bounds-checked forms. |
| `v.swap(i, j)` | `()` | In-place when both indices are in range. |
| `v.sort()` / `v.sort_by(cmp)` / `v.sort_by_key(f)` | `()` | In-place. |
| `v.iter()` | `Iter<T>` | Lazy iterator. |
| Eager helpers (`map`, `filter`, `take`, `step_by`, `rev`) | `[T]` | Return new vectors. |

## Maps and sets

Shipped:

| Type | Methods |
|---|---|
| `HashMap<K, V>` | `insert`, `get`, `get_or`, `contains_key`, `contains`, `remove`, `pop`, `or_insert`, `inc`, `len`, `iter`, `keys`, `values` |
| `HashSet<T>` | `insert`, `contains`, `remove`, `len`, `clear`, `to_vec`, `union`, `intersection`, `difference`, `symmetric_difference`, `is_subset`, `is_superset`, `is_disjoint` |
| `BTreeMap<K, V>` | `insert`, `get`, `get_or`, `contains`, `contains_key`, `remove`, `len`, `iter`, `keys`, `values` |
| `VecDeque<T>` | `push_back`, `push_front`, `pop_back`, `pop_front`, `peek_back`, `peek_front`, `len`, `is_empty` |

## Option and Result

Shipped:

| Type | Methods |
|---|---|
| `Option<T>` | `is_some`, `is_none`, `unwrap`, `expect`, `unwrap_or`, `unwrap_or_else`, `map`, `and_then`, `filter`, `or_else`, `ok_or`, `ok_or_else`, `flatten`, `zip` |
| `Result<T, E>` | `is_ok`, `is_err`, `unwrap`, `expect`, `unwrap_or`, `unwrap_or_else`, `map`, `map_err`, `and_then`, `or_else`, `ok`, `err`, `transpose` |

## Compatibility APIs

Some older container helpers still return sentinel values such as `0`, `-1`,
or an empty string for absence or failure. New core APIs should prefer
`Option<T>` or `Result<T, errors::Error>`. Existing sentinel helpers remain
compatibility aliases until their callers have migrated.
