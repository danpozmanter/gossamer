//! Query-string parsing.
//!
//! Pure text handling with no platform surface, so it is available on every
//! target the runtime builds for - the parser a `query_pairs` answer comes
//! from must not differ between a native tier and a wasm one.

/// `name=value` pairs of a query string, percent-decoded, with `+`
/// read as a space. A pair with no `=` is the name alone.
///
/// The one implementation the bytecode VM, both compiled tiers, and the
/// Rust-side `http::Request` accessor all read `query_pairs` through:
/// three copies of this had drifted into two different answers for
/// `b=hello+world`.
#[must_use]
pub fn parse_query_pairs(query: &str) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for pair in query.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (name, value) = pair.split_once('=').unwrap_or((pair, ""));
        out.push((decode_query_component(name), decode_query_component(value)));
    }
    out
}

/// One percent-decoded query component. An escape that is not two hex
/// digits is left as written, which is what a browser does with it.
#[must_use]
pub fn decode_query_component(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out: Vec<u8> = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' if i + 2 < bytes.len() => {
                let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok();
                if let Some(byte) = hex.and_then(|h| u8::from_str_radix(h, 16).ok()) {
                    out.push(byte);
                    i += 3;
                } else {
                    out.push(bytes[i]);
                    i += 1;
                }
            }
            byte => {
                out.push(byte);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}
