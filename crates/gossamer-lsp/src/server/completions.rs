fn completion_item(label: &str, kind: u32) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    item.insert("kind".to_string(), Value::Number(f64::from(kind)));
    Value::Object(item)
}

fn completion_item_local(label: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    item.insert("kind".to_string(), Value::Number(6.0)); // Variable
    Value::Object(item)
}

fn completion_item_for(doc: &DocumentAnalysis, label: &str, _prefix: &str) -> Value {
    // If the index has a real DefinitionInfo for this name, decorate
    // the completion entry with the kind, signature, and docs so the
    // editor can render a richer popup.
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(label.to_string()));
    let mut kind = 3.0; // Function (LSP CompletionItemKind::Function)
    for (_, info) in doc.index_pairs() {
        if info.name == label {
            kind = match info.kind {
                DefKind::Fn => 3.0,
                DefKind::Struct => 22.0,
                DefKind::Enum => 13.0,
                DefKind::Trait => 8.0,
                DefKind::TypeAlias => 25.0,
                DefKind::Const => 21.0,
                DefKind::Static => 6.0,
                DefKind::Mod => 9.0,
                DefKind::Variant => 20.0,
                DefKind::TypeParam => 25.0,
            };
            if !info.signature.is_empty() {
                item.insert("detail".to_string(), Value::String(info.signature.clone()));
            }
            if !info.docs.is_empty() {
                let mut docs = BTreeMap::new();
                docs.insert("kind".to_string(), Value::String("markdown".to_string()));
                docs.insert("value".to_string(), Value::String(info.docs.clone()));
                item.insert("documentation".to_string(), Value::Object(docs));
            }
            break;
        }
    }
    item.insert("kind".to_string(), Value::Number(kind));
    Value::Object(item)
}

fn member_to_completion(spec: &MemberSpec) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(spec.name.clone()));
    item.insert("kind".to_string(), Value::Number(f64::from(spec.kind)));
    if let Some(detail) = &spec.detail {
        item.insert("detail".to_string(), Value::String(detail.clone()));
    }
    if let Some(doc) = &spec.doc {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(doc.clone()));
        item.insert("documentation".to_string(), Value::Object(docs));
    }
    // Function-like members carry a snippet so the editor opens the
    // parens for the user. Module / type / const stay as bare names.
    if spec.kind == 3 {
        item.insert(
            "insertText".to_string(),
            Value::String(format!("{}($0)", spec.name)),
        );
        item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    }
    Value::Object(item)
}

fn completion_function_item_with_snippet(name: &str) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(name.to_string()));
    item.insert("kind".to_string(), Value::Number(3.0));
    item.insert(
        "insertText".to_string(),
        Value::String(format!("{name}($0)")),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

/// Receiver-side identification used to look up methods.
#[derive(Debug, Clone)]
struct ReceiverDescriptor {
    /// Builtin classification (`Vec` / `String` / `HashMap` / `Option` / `Result` / …).
    builtin: BuiltinReceiver,
    /// User-facing type name extracted from `let r: Foo = …` or
    /// `struct Foo { … }`. Used to match `impl Foo` blocks.
    type_name: Option<String>,
}

impl ReceiverDescriptor {
    fn type_name(&self) -> Option<&str> {
        self.type_name.as_deref()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BuiltinReceiver {
    Vec,
    String,
    HashMap,
    HashSet,
    Iterator,
    Option,
    Result,
    Unknown,
}

fn receiver_descriptor(doc: &DocumentAnalysis, offset: u32) -> ReceiverDescriptor {
    // Locate the receiver expression: walk left from the dot in the
    // source, skipping the suffix word the user is typing.
    let bytes = doc.source().as_bytes();
    let mut idx = (offset as usize).min(bytes.len());
    let is_word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    while idx > 0 && is_word(bytes[idx - 1]) {
        idx -= 1;
    }
    if idx == 0 || bytes[idx - 1] != b'.' {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: None,
        };
    }
    // Walk left across the receiver expression (very conservative: stop
    // at common statement boundaries / unmatched parens).
    let dot_pos = idx - 1;
    let mut start = dot_pos;
    let mut depth: i32 = 0;
    while start > 0 {
        let b = bytes[start - 1];
        match b {
            b')' | b']' | b'}' => depth += 1,
            b'(' | b'[' | b'{' => {
                if depth == 0 {
                    break;
                }
                depth -= 1;
            }
            b';' | b',' | b'\n' if depth == 0 => break,
            _ => {}
        }
        start -= 1;
    }
    let receiver = std::str::from_utf8(&bytes[start..dot_pos])
        .unwrap_or("")
        .trim();
    classify_receiver(doc, receiver)
}

fn classify_receiver(doc: &DocumentAnalysis, expr: &str) -> ReceiverDescriptor {
    let head = expr.trim();
    // Direct string literal.
    if head.starts_with('"') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::String,
            type_name: Some("String".to_string()),
        };
    }
    // Vec literal `vec![...]` / `[...]`.
    if head.starts_with("vec![") || head.starts_with('[') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Vec,
            type_name: None,
        };
    }
    // Identifier - try resolving via let-binding type annotation.
    if let Some(name) = identifier_token(head) {
        if let Some(ty) = lookup_let_annotation(doc.source(), name) {
            return classify_type_string(&ty);
        }
    }
    ReceiverDescriptor {
        builtin: BuiltinReceiver::Unknown,
        type_name: None,
    }
}

fn classify_type_string(ty: &str) -> ReceiverDescriptor {
    let ty = ty.trim();
    let head = ty
        .trim_start_matches(['&', '*', ' '])
        .trim_end_matches([',', ';', ' ']);
    if head.starts_with("Vec<") || head.starts_with("&[") || head.starts_with('[') {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Vec,
            type_name: None,
        };
    }
    if head.starts_with("HashMap<") {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::HashMap,
            type_name: None,
        };
    }
    if head.starts_with("HashSet<") {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::HashSet,
            type_name: None,
        };
    }
    if head == "String" || head == "&str" || head == "str" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::String,
            type_name: Some("String".to_string()),
        };
    }
    if head.starts_with("Option<") || head == "Option" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Option,
            type_name: Some("Option".to_string()),
        };
    }
    if head.starts_with("Iterator<") || head == "Iterator" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Iterator,
            type_name: Some("Iterator".to_string()),
        };
    }
    if head.starts_with("Result<") || head == "Result" {
        return ReceiverDescriptor {
            builtin: BuiltinReceiver::Result,
            type_name: Some("Result".to_string()),
        };
    }
    let bare = head.split(['<', '[', '(', ' ']).next().unwrap_or(head);
    if bare.is_empty() {
        ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: None,
        }
    } else {
        ReceiverDescriptor {
            builtin: BuiltinReceiver::Unknown,
            type_name: Some(bare.to_string()),
        }
    }
}

fn identifier_token(text: &str) -> Option<&str> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }
    if !trimmed
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_')
    {
        return None;
    }
    if trimmed.chars().next()?.is_ascii_digit() {
        return None;
    }
    Some(trimmed)
}

/// Looks for a `let <name>: <type> = ...` binding for `name` in the
/// document and returns the rendered type spelling.
fn lookup_let_annotation(source: &str, name: &str) -> Option<String> {
    let needle = format!("let {name}");
    let needle_mut = format!("let mut {name}");
    let mut start = 0usize;
    while start < source.len() {
        let position = source[start..].find(&needle)?;
        let absolute = start + position;
        let head_ok = absolute == 0
            || !matches!(
                source.as_bytes()[absolute - 1],
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'_'
            );
        if !head_ok {
            start = absolute + 1;
            continue;
        }
        // After the `let <name>` (or `let mut <name>`), look for a `:`
        // that starts the type annotation, stopping at `=` or newline.
        let after = if source[absolute..].starts_with(&needle_mut) {
            absolute + needle_mut.len()
        } else {
            absolute + needle.len()
        };
        let tail = &source[after..];
        // Strict word boundary: next char must not be word.
        if tail
            .as_bytes()
            .first()
            .copied()
            .is_some_and(|b| b.is_ascii_alphanumeric() || b == b'_')
        {
            start = absolute + 1;
            continue;
        }
        let stripped = tail.trim_start();
        if let Some(rest_with_ws) = stripped.strip_prefix(':') {
            let rest = rest_with_ws.trim_start();
            // Capture until `=` or newline at top depth.
            let mut depth: i32 = 0;
            let mut end = 0usize;
            for (i, ch) in rest.char_indices() {
                match ch {
                    '<' | '(' | '[' => depth += 1,
                    '>' | ')' | ']' => depth -= 1,
                    '=' | '\n' | ';' if depth == 0 => {
                        end = i;
                        break;
                    }
                    _ => {}
                }
            }
            if end == 0 {
                end = rest.len();
            }
            let ty = rest[..end].trim().trim_end_matches(',').trim();
            if !ty.is_empty() {
                return Some(ty.to_string());
            }
        }
        start = absolute + 1;
    }
    None
}

#[derive(Debug, Clone, Copy)]
struct BuiltinMethod {
    name: &'static str,
    signature: &'static str,
    doc: &'static str,
    snippet: &'static str,
}

const VEC_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "push",
        signature: "fn push(&mut self, value: T)",
        doc: "Appends `value` to the back of the vec.",
        snippet: "push($0)",
    },
    BuiltinMethod {
        name: "pop",
        signature: "fn pop(&mut self) -> Option<T>",
        doc: "Removes the last element and returns it, or `None` when empty.",
        snippet: "pop()$0",
    },
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Number of elements currently in the vec.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` when the vec has no elements.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "clear",
        signature: "fn clear(&mut self)",
        doc: "Removes every element, leaving the vec at length 0.",
        snippet: "clear()$0",
    },
    BuiltinMethod {
        name: "iter",
        signature: "fn iter(&self) -> Iter<T>",
        doc: "Returns an iterator over the vec's elements.",
        snippet: "iter()$0",
    },
    BuiltinMethod {
        name: "clone",
        signature: "fn clone(&self) -> Self",
        doc: "Clones every element into a new vec.",
        snippet: "clone()$0",
    },
    BuiltinMethod {
        name: "contains",
        signature: "fn contains(&self, value: &T) -> bool",
        doc: "Returns `true` when the vec contains an element equal to `value`.",
        snippet: "contains(&$0)",
    },
    BuiltinMethod {
        name: "sort",
        signature: "fn sort(&mut self)",
        doc: "Sorts the vec in place.",
        snippet: "sort()$0",
    },
];

const STRING_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Length of the string in bytes.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` for the empty string.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "to_uppercase",
        signature: "fn to_uppercase(&self) -> String",
        doc: "Returns the upper-cased clone of the string.",
        snippet: "to_uppercase()$0",
    },
    BuiltinMethod {
        name: "to_lowercase",
        signature: "fn to_lowercase(&self) -> String",
        doc: "Returns the lower-cased clone of the string.",
        snippet: "to_lowercase()$0",
    },
    BuiltinMethod {
        name: "trim",
        signature: "fn trim(&self) -> &str",
        doc: "Returns the string with leading + trailing whitespace stripped.",
        snippet: "trim()$0",
    },
    BuiltinMethod {
        name: "split",
        signature: "fn split(&self, sep: &str) -> Vec<String>",
        doc: "Splits on every occurrence of `sep`.",
        snippet: "split(\"$0\")",
    },
    BuiltinMethod {
        name: "lines",
        signature: "fn lines(&self) -> Vec<String>",
        doc: "Splits the string on `\\n`.",
        snippet: "lines()$0",
    },
    BuiltinMethod {
        name: "starts_with",
        signature: "fn starts_with(&self, prefix: &str) -> bool",
        doc: "True when the string begins with `prefix`.",
        snippet: "starts_with(\"$0\")",
    },
    BuiltinMethod {
        name: "ends_with",
        signature: "fn ends_with(&self, suffix: &str) -> bool",
        doc: "True when the string ends with `suffix`.",
        snippet: "ends_with(\"$0\")",
    },
    BuiltinMethod {
        name: "contains",
        signature: "fn contains(&self, needle: &str) -> bool",
        doc: "True when `needle` appears anywhere in the string.",
        snippet: "contains(\"$0\")",
    },
    BuiltinMethod {
        name: "repeat",
        signature: "fn repeat(&self, n: i64) -> String",
        doc: "Returns the string repeated `n` times.",
        snippet: "repeat($0)",
    },
    BuiltinMethod {
        name: "to_string",
        signature: "fn to_string(&self) -> String",
        doc: "Returns a fresh owned copy.",
        snippet: "to_string()$0",
    },
];

const HASHMAP_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "insert",
        signature: "fn insert(&mut self, key: K, value: V) -> Option<V>",
        doc: "Inserts a key/value pair, returning the previous value (if any).",
        snippet: "insert($1, $2)$0",
    },
    BuiltinMethod {
        name: "get",
        signature: "fn get(&self, key: &K) -> Option<&V>",
        doc: "Looks up `key`.",
        snippet: "get(&$0)",
    },
    BuiltinMethod {
        name: "get_or",
        signature: "fn get_or(&self, key: K, default: V) -> V",
        doc: "Looks up `key`, returning `default` when absent.",
        snippet: "get_or($1, $2)$0",
    },
    BuiltinMethod {
        name: "remove",
        signature: "fn remove(&mut self, key: &K) -> Option<V>",
        doc: "Removes `key`'s entry, returning the removed value.",
        snippet: "remove(&$0)",
    },
    BuiltinMethod {
        name: "len",
        signature: "fn len(&self) -> usize",
        doc: "Number of entries.",
        snippet: "len()$0",
    },
    BuiltinMethod {
        name: "is_empty",
        signature: "fn is_empty(&self) -> bool",
        doc: "Returns `true` when there are no entries.",
        snippet: "is_empty()$0",
    },
    BuiltinMethod {
        name: "contains_key",
        signature: "fn contains_key(&self, key: &K) -> bool",
        doc: "Returns `true` when an entry for `key` exists.",
        snippet: "contains_key(&$0)",
    },
    BuiltinMethod {
        name: "clear",
        signature: "fn clear(&mut self)",
        doc: "Removes every entry.",
        snippet: "clear()$0",
    },
    BuiltinMethod {
        name: "keys",
        signature: "fn keys(&self) -> Iter<K>",
        doc: "Iterator over keys.",
        snippet: "keys()$0",
    },
    BuiltinMethod {
        name: "values",
        signature: "fn values(&self) -> Iter<V>",
        doc: "Iterator over values.",
        snippet: "values()$0",
    },
];

const OPTION_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "and_then",
        signature: "fn and_then<U>(self, f: fn(T) -> Option<U>) -> Option<U>",
        doc: "Chains the contained value through an Option-returning function.",
        snippet: "and_then(|value| $0)",
    },
    BuiltinMethod {
        name: "filter",
        signature: "fn filter(self, predicate: fn(T) -> bool) -> Option<T>",
        doc: "Keeps the value only when the predicate accepts it.",
        snippet: "filter(|value| $0)",
    },
    BuiltinMethod {
        name: "flatten",
        signature: "fn flatten(self) -> Option<T>",
        doc: "Flattens one nested Option level.",
        snippet: "flatten()$0",
    },
    BuiltinMethod {
        name: "is_some",
        signature: "fn is_some(&self) -> bool",
        doc: "Returns `true` when the option is `Some`.",
        snippet: "is_some()$0",
    },
    BuiltinMethod {
        name: "is_none",
        signature: "fn is_none(&self) -> bool",
        doc: "Returns `true` when the option is `None`.",
        snippet: "is_none()$0",
    },
    BuiltinMethod {
        name: "unwrap_or",
        signature: "fn unwrap_or(self, default: T) -> T",
        doc: "Returns the contained value, or `default` if `None`.",
        snippet: "unwrap_or($0)",
    },
    BuiltinMethod {
        name: "iter",
        signature: "fn iter(self) -> Vec<T>",
        doc: "Returns a zero-or-one element sequence.",
        snippet: "iter()$0",
    },
    BuiltinMethod {
        name: "map",
        signature: "fn map<U>(self, f: fn(T) -> U) -> Option<U>",
        doc: "Maps the contained value through `f`.",
        snippet: "map(|x| $0)",
    },
    BuiltinMethod {
        name: "or",
        signature: "fn or(self, fallback: Option<T>) -> Option<T>",
        doc: "Returns this option when present, otherwise the fallback.",
        snippet: "or($0)",
    },
    BuiltinMethod {
        name: "or_else",
        signature: "fn or_else(self, fallback: fn() -> Option<T>) -> Option<T>",
        doc: "Computes a fallback only when this option is None.",
        snippet: "or_else(|| $0)",
    },
    BuiltinMethod {
        name: "unwrap_or_else",
        signature: "fn unwrap_or_else(self, fallback: fn() -> T) -> T",
        doc: "Computes a fallback only when this option is None.",
        snippet: "unwrap_or_else(|| $0)",
    },
    BuiltinMethod {
        name: "zip",
        signature: "fn zip<U>(self, other: Option<U>) -> Option<(T, U)>",
        doc: "Pairs the values when both options are present.",
        snippet: "zip($0)",
    },
];

const ITERATOR_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod { name: "map", signature: "fn map<U>(self, f: fn(T) -> U) -> Iterator<U>", doc: "Transforms each item.", snippet: "map(|value| $0)" },
    BuiltinMethod { name: "filter", signature: "fn filter(self, predicate: fn(T) -> bool) -> Iterator<T>", doc: "Keeps accepted items.", snippet: "filter(|value| $0)" },
    BuiltinMethod { name: "fold", signature: "fn fold<U>(self, initial: U, f: fn(U, T) -> U) -> U", doc: "Folds items into one value.", snippet: "fold($1, |acc, value| $0)" },
    BuiltinMethod { name: "collect", signature: "fn collect(self) -> Vec<T>", doc: "Materializes all items.", snippet: "collect()$0" },
    BuiltinMethod { name: "count", signature: "fn count(self) -> i64", doc: "Counts all items.", snippet: "count()$0" },
    BuiltinMethod { name: "sum", signature: "fn sum(self) -> T", doc: "Sums all items.", snippet: "sum()$0" },
    BuiltinMethod { name: "min", signature: "fn min(self) -> Option<T>", doc: "Returns the minimum item.", snippet: "min()$0" },
    BuiltinMethod { name: "max", signature: "fn max(self) -> Option<T>", doc: "Returns the maximum item.", snippet: "max()$0" },
    BuiltinMethod { name: "take", signature: "fn take(self, count: i64) -> Iterator<T>", doc: "Yields at most count items.", snippet: "take($0)" },
    BuiltinMethod { name: "skip", signature: "fn skip(self, count: i64) -> Iterator<T>", doc: "Skips the first count items.", snippet: "skip($0)" },
    BuiltinMethod { name: "enumerate", signature: "fn enumerate(self) -> Iterator<(i64, T)>", doc: "Pairs items with indexes.", snippet: "enumerate()$0" },
    BuiltinMethod { name: "chain", signature: "fn chain(self, other: Iterator<T>) -> Iterator<T>", doc: "Yields this iterator then another.", snippet: "chain($0)" },
    BuiltinMethod { name: "zip", signature: "fn zip<U>(self, other: Iterator<U>) -> Iterator<(T, U)>", doc: "Pairs items from two iterators.", snippet: "zip($0)" },
    BuiltinMethod { name: "flatten", signature: "fn flatten(self) -> Iterator<T>", doc: "Flattens one nested iterator level.", snippet: "flatten()$0" },
    BuiltinMethod { name: "rev", signature: "fn rev(self) -> Iterator<T>", doc: "Reverses iteration order.", snippet: "rev()$0" },
];

const RESULT_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "is_ok",
        signature: "fn is_ok(&self) -> bool",
        doc: "Returns `true` when the result is `Ok`.",
        snippet: "is_ok()$0",
    },
    BuiltinMethod {
        name: "is_err",
        signature: "fn is_err(&self) -> bool",
        doc: "Returns `true` when the result is `Err`.",
        snippet: "is_err()$0",
    },
    BuiltinMethod {
        name: "unwrap",
        signature: "fn unwrap(self) -> T",
        doc: "Returns the `Ok` value, panicking on `Err`.",
        snippet: "unwrap()$0",
    },
    BuiltinMethod {
        name: "unwrap_or",
        signature: "fn unwrap_or(self, default: T) -> T",
        doc: "Returns the `Ok` value, or `default` on `Err`.",
        snippet: "unwrap_or($0)",
    },
    BuiltinMethod {
        name: "map",
        signature: "fn map<U>(self, f: fn(T) -> U) -> Result<U, E>",
        doc: "Maps the `Ok` value.",
        snippet: "map(|x| $0)",
    },
    BuiltinMethod {
        name: "map_err",
        signature: "fn map_err<F>(self, f: fn(E) -> F) -> Result<T, F>",
        doc: "Maps the `Err` value.",
        snippet: "map_err(|e| $0)",
    },
];

const ALL_BUILTIN_METHODS: &[BuiltinMethod] = &[
    BuiltinMethod {
        name: "to_string",
        signature: "fn to_string(&self) -> String",
        doc: "Default ToString rendering.",
        snippet: "to_string()$0",
    },
    BuiltinMethod {
        name: "clone",
        signature: "fn clone(&self) -> Self",
        doc: "Clones the receiver.",
        snippet: "clone()$0",
    },
];

fn builtin_methods_for(receiver: &ReceiverDescriptor) -> &'static [BuiltinMethod] {
    match receiver.builtin {
        BuiltinReceiver::Vec => VEC_METHODS,
        BuiltinReceiver::String => STRING_METHODS,
        BuiltinReceiver::HashMap | BuiltinReceiver::HashSet => HASHMAP_METHODS,
        BuiltinReceiver::Iterator => ITERATOR_METHODS,
        BuiltinReceiver::Option => OPTION_METHODS,
        BuiltinReceiver::Result => RESULT_METHODS,
        BuiltinReceiver::Unknown => &[],
    }
}

fn method_completion_item(method: &BuiltinMethod) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(method.name.to_string()));
    item.insert("kind".to_string(), Value::Number(2.0)); // Method
    item.insert(
        "detail".to_string(),
        Value::String(method.signature.to_string()),
    );
    let mut docs = BTreeMap::new();
    docs.insert("kind".to_string(), Value::String("markdown".to_string()));
    docs.insert("value".to_string(), Value::String(method.doc.to_string()));
    item.insert("documentation".to_string(), Value::Object(docs));
    item.insert(
        "insertText".to_string(),
        Value::String(method.snippet.to_string()),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

#[derive(Debug, Clone)]
struct UserMethod {
    name: String,
    signature: String,
    doc: String,
    is_associated: bool,
}

fn user_methods_for(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    collect_impl_items(doc, type_name, false)
}

fn user_associated_items(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    let mut out = collect_impl_items(doc, type_name, true);
    // Add enum variants of `type_name` if any.
    out.extend(enum_variants_for(doc, type_name));
    out
}

fn collect_impl_items(
    doc: &DocumentAnalysis,
    type_name: &str,
    want_associated: bool,
) -> Vec<UserMethod> {
    use gossamer_ast::{FnParam, ImplItem, ItemKind, TypeKind};
    let mut out: Vec<UserMethod> = Vec::new();
    for item in &doc.sf.items {
        let ItemKind::Impl(decl) = &item.kind else {
            continue;
        };
        let TypeKind::Path(path) = &decl.self_ty.kind else {
            continue;
        };
        let Some(seg) = path.segments.last() else {
            continue;
        };
        if seg.name.name != type_name {
            continue;
        }
        for impl_item in &decl.items {
            let ImplItem::Fn(fn_decl) = impl_item else {
                continue;
            };
            let has_receiver = fn_decl
                .params
                .first()
                .is_some_and(|p| matches!(p, FnParam::Receiver(_)));
            let is_associated = !has_receiver;
            if is_associated != want_associated {
                continue;
            }
            let signature = render_user_signature(fn_decl);
            out.push(UserMethod {
                name: fn_decl.name.name.clone(),
                signature,
                doc: String::new(),
                is_associated,
            });
        }
    }
    out
}

fn enum_variants_for(doc: &DocumentAnalysis, type_name: &str) -> Vec<UserMethod> {
    use gossamer_ast::ItemKind;
    let mut out: Vec<UserMethod> = Vec::new();
    for item in &doc.sf.items {
        let ItemKind::Enum(decl) = &item.kind else {
            continue;
        };
        if decl.name.name != type_name {
            continue;
        }
        for variant in &decl.variants {
            out.push(UserMethod {
                name: variant.name.name.clone(),
                signature: format!("{}::{}", type_name, variant.name.name),
                doc: String::new(),
                is_associated: true,
            });
        }
    }
    out
}

fn render_user_signature(decl: &gossamer_ast::FnDecl) -> String {
    use gossamer_ast::FnParam;
    let mut out = String::new();
    out.push_str("fn ");
    out.push_str(&decl.name.name);
    out.push('(');
    let mut first = true;
    for param in &decl.params {
        if !first {
            out.push_str(", ");
        }
        first = false;
        match param {
            FnParam::Receiver(receiver) => out.push_str(receiver.as_str()),
            FnParam::Typed { pattern, ty, .. } => {
                let mut printer = gossamer_ast::Printer::new();
                printer.print_type(ty);
                out.push_str(&pattern_label(pattern));
                out.push_str(": ");
                out.push_str(&printer.finish());
            }
        }
    }
    out.push(')');
    if let Some(ret) = &decl.ret {
        out.push_str(" -> ");
        let mut printer = gossamer_ast::Printer::new();
        printer.print_type(ret);
        out.push_str(&printer.finish());
    }
    out
}

fn pattern_label(pattern: &gossamer_ast::Pattern) -> String {
    use gossamer_ast::PatternKind;
    match &pattern.kind {
        PatternKind::Ident { name, .. } => name.name.clone(),
        _ => "_".to_string(),
    }
}

fn user_method_completion_item(method: &UserMethod) -> Value {
    let mut item = BTreeMap::new();
    item.insert("label".to_string(), Value::String(method.name.clone()));
    let kind = if method.is_associated { 3.0 } else { 2.0 };
    item.insert("kind".to_string(), Value::Number(kind));
    item.insert(
        "detail".to_string(),
        Value::String(method.signature.clone()),
    );
    if !method.doc.is_empty() {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(method.doc.clone()));
        item.insert("documentation".to_string(), Value::Object(docs));
    }
    item.insert(
        "insertText".to_string(),
        Value::String(format!("{}($0)", method.name)),
    );
    item.insert("insertTextFormat".to_string(), Value::Number(2.0));
    Value::Object(item)
}

fn workspace_completion_item(item: &WorkspaceItem) -> Value {
    let mut entry = BTreeMap::new();
    entry.insert("label".to_string(), Value::String(item.name.clone()));
    let kind = match item.kind {
        DefKind::Fn => 3.0,
        DefKind::Struct => 22.0,
        DefKind::Enum => 13.0,
        DefKind::Trait => 8.0,
        DefKind::TypeAlias => 25.0,
        DefKind::Const => 21.0,
        DefKind::Static => 6.0,
        DefKind::Mod => 9.0,
        DefKind::Variant => 20.0,
        DefKind::TypeParam => 25.0,
    };
    entry.insert("kind".to_string(), Value::Number(kind));
    if !item.signature.is_empty() {
        entry.insert(
            "detail".to_string(),
            Value::String(format!("{}  // {}", item.signature, short_uri(&item.uri))),
        );
    }
    if !item.doc.is_empty() {
        let mut docs = BTreeMap::new();
        docs.insert("kind".to_string(), Value::String("markdown".to_string()));
        docs.insert("value".to_string(), Value::String(item.doc.clone()));
        entry.insert("documentation".to_string(), Value::Object(docs));
    }
    if matches!(item.kind, DefKind::Fn) {
        entry.insert(
            "insertText".to_string(),
            Value::String(format!("{}($0)", item.name)),
        );
        entry.insert("insertTextFormat".to_string(), Value::Number(2.0));
    }
    Value::Object(entry)
}

fn short_uri(uri: &str) -> String {
    uri.rsplit('/').next().unwrap_or(uri).to_string()
}
