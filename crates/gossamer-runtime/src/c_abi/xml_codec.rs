#![allow(clippy::missing_safety_doc)]
#![allow(missing_docs)]
#![allow(clippy::not_unsafe_ptr_arg_deref)]
#![allow(clippy::doc_markdown)]

//! C-ABI shims for `std::encoding::xml::{parse, encode}` so the
//! compiled tier lowers them to direct runtime calls instead of
//! emitting undefined `@encoding::xml::parse` / `@encoding::xml::encode`
//! references. `gossamer-runtime` cannot depend on `gossamer-std`
//! (that would be a dependency cycle), so the parse/encode logic is
//! reimplemented here against the same `quick-xml 0.37` the VM tier
//! uses - the bytes mirror `gossamer_std::encoding::xml` exactly, so
//! a parse->encode round-trip is bit-identical across tiers.
//!
//! The parsed tree is handed to user code as an opaque `*mut GosXml`
//! handle threaded through a normal i64 slot (the same opaque-handle
//! shape `gos_rt_json_*` uses for `serde_json::Value`). `parse`
//! returns `Result<i64-handle, errors::Error>`; `encode` consumes the
//! handle and returns the serialised `String`. Between the two the
//! handle is opaque - user code does not navigate it (field access on
//! an xml node is not part of this surface), matching the VM tier
//! where the node round-trips straight back into `encode`.

use std::collections::BTreeMap;
use std::os::raw::c_char;

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::writer::Writer;

use super::string::alloc_cstring;

/// Mirrors `gossamer_std::encoding::xml`'s default parser caps so the
/// error path is byte-identical to the VM on oversize / over-deep
/// input. The VM's `set_max_*` setters are not part of the compiled
/// surface, so these are fixed at the same defaults.
const DEFAULT_MAX_DEPTH: usize = 128;
const DEFAULT_MAX_SIZE: usize = 16 * 1024 * 1024;

/// A node in the parsed XML tree. Mirrors
/// `gossamer_std::encoding::xml::Node`; attributes are ordered
/// (`BTreeMap`) so attribute emission is deterministic and matches
/// the VM tier.
enum Node {
    Element {
        name: String,
        attrs: BTreeMap<String, String>,
        children: Vec<Node>,
    },
    Text(String),
}

/// Opaque handle wrapping the parsed root node. The compiled tier
/// shuttles the raw `*mut GosXml` through an i64 slot. Reclamation
/// would require a dedicated `TyKind::XmlNode` so the MIR drop pass
/// can key on it (the json model); the parse result is currently a
/// plain `i64`, indistinguishable from any other integer, so - like
/// the raw-i64 SQL handles - the tree is intentionally leaked rather
/// than risk freeing a non-handle i64. See the module docs.
pub struct GosXml {
    node: Node,
}

fn cstr_to_str<'a>(s: *const c_char) -> &'a str {
    // SAFETY: callers pass a Gossamer `String`, read through its length
    // header so interior NUL bytes survive; non-UTF-8 falls back to empty.
    unsafe { crate::c_abi::gos_str_arg_text(s) }
}

fn err_result(msg: &str) -> i128 {
    let err = crate::c_abi::errors::error_new_from_bytes(msg.as_bytes());
    unsafe { super::vec::gos_rt_result_new(1, err as i64) }
}

/// Parses an XML document into a tree, returning the root element.
/// Byte-for-byte mirror of `gossamer_std::encoding::xml::parse`.
fn parse(src: &str) -> Result<Node, String> {
    if src.len() > DEFAULT_MAX_SIZE {
        return Err(format!(
            "xml input exceeds max_size ({} > {DEFAULT_MAX_SIZE})",
            src.len()
        ));
    }
    let mut reader = Reader::from_str(src);
    let mut stack: Vec<(String, BTreeMap<String, String>, Vec<Node>)> = Vec::new();
    let mut root: Option<Node> = None;
    // `quick-xml` reports entity references (`&lt;` etc.) as their own
    // `GeneralRef` events and splits surrounding character data, so text
    // is reassembled here and entity-resolved as one piece at each
    // element boundary.
    let mut text_buf = String::new();
    loop {
        let event = reader
            .read_event()
            .map_err(|e| format!("xml: parse: {e}"))?;
        if !matches!(event, Event::Text(_) | Event::GeneralRef(_)) {
            flush_text(&mut text_buf, &mut stack)?;
        }
        match event {
            Event::Start(e) => {
                if stack.len() >= DEFAULT_MAX_DEPTH {
                    return Err(format!(
                        "xml nesting depth exceeds max_depth ({DEFAULT_MAX_DEPTH})"
                    ));
                }
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let mut attrs = BTreeMap::new();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
                    let val = String::from_utf8_lossy(&attr.value).into_owned();
                    attrs.insert(key, val);
                }
                stack.push((name, attrs, Vec::new()));
            }
            Event::Empty(e) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let mut attrs = BTreeMap::new();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
                    let val = String::from_utf8_lossy(&attr.value).into_owned();
                    attrs.insert(key, val);
                }
                let node = Node::Element {
                    name,
                    attrs,
                    children: vec![],
                };
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(node);
                } else {
                    root = Some(node);
                }
            }
            Event::End(_) => {
                if let Some((name, attrs, children)) = stack.pop() {
                    let node = Node::Element {
                        name,
                        attrs,
                        children,
                    };
                    if let Some(parent) = stack.last_mut() {
                        parent.2.push(node);
                    } else {
                        root = Some(node);
                    }
                }
            }
            Event::Text(e) => {
                let decoded = e.decode().map_err(|err| format!("xml: {err}"))?;
                text_buf.push_str(&decoded);
            }
            Event::GeneralRef(e) => {
                let name = e.decode().map_err(|err| format!("xml: {err}"))?;
                text_buf.push('&');
                text_buf.push_str(&name);
                text_buf.push(';');
            }
            Event::CData(e) => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Text(text));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.ok_or_else(|| "xml: empty document".to_string())
}

/// Resolves the accumulated character data (entity references
/// reconstructed as `&name;`), trims surrounding whitespace, and
/// appends it as a text child of the current element. Whitespace-only
/// runs collapse to nothing, matching the old `trim_text` behaviour.
fn flush_text(
    buf: &mut String,
    stack: &mut [(String, BTreeMap<String, String>, Vec<Node>)],
) -> Result<(), String> {
    if buf.is_empty() {
        return Ok(());
    }
    let resolved = quick_xml::escape::unescape(buf).map_err(|err| format!("xml: {err}"))?;
    let trimmed = resolved.trim().to_owned();
    buf.clear();
    if !trimmed.is_empty() {
        if let Some(parent) = stack.last_mut() {
            parent.2.push(Node::Text(trimmed));
        }
    }
    Ok(())
}

/// Serialises a node tree to XML. Byte-for-byte mirror of
/// `gossamer_std::encoding::xml::encode`; emits no XML declaration.
fn encode(node: &Node) -> String {
    let mut buf = Vec::new();
    let mut writer = Writer::new(&mut buf);
    write_node(&mut writer, node);
    String::from_utf8(buf).unwrap_or_default()
}

fn write_node(w: &mut Writer<impl std::io::Write>, node: &Node) {
    match node {
        Node::Text(s) => {
            let _ = w.write_event(Event::Text(quick_xml::events::BytesText::new(s)));
        }
        Node::Element {
            name,
            attrs,
            children,
        } => {
            let mut elem = quick_xml::events::BytesStart::new(name.as_str());
            for (k, v) in attrs {
                elem.push_attribute((k.as_str(), v.as_str()));
            }
            if children.is_empty() {
                let _ = w.write_event(Event::Empty(elem));
            } else {
                let _ = w.write_event(Event::Start(elem));
                for child in children {
                    write_node(w, child);
                }
                let _ = w.write_event(Event::End(quick_xml::events::BytesEnd::new(name.as_str())));
            }
        }
    }
}

/// `encoding::xml::parse(s) -> Result<Node, errors::Error>`. The Ok
/// payload is an opaque `*mut GosXml` handle (cast to i64); the Err
/// payload is a gos error handle. Returns a packed `GosResult` i128
/// (disc 0 = Ok, disc 1 = Err).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_xml_parse(s: *const c_char) -> i128 {
    ffi_entry!(0i128, {
        match parse(cstr_to_str(s)) {
            Ok(node) => {
                let handle = Box::into_raw(Box::new(GosXml { node }));
                unsafe { super::vec::gos_rt_result_new(0, handle as i64) }
            }
            Err(e) => err_result(&e),
        }
    })
}

/// `encoding::xml::encode(node) -> String`. Consumes the opaque
/// `*mut GosXml` handle (passed as an i64) and returns the serialised
/// document. A null / zero handle yields the empty string, matching
/// the VM tier's behaviour for a non-node argument.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn gos_rt_xml_encode(node: i64) -> *mut c_char {
    ffi_entry!(std::ptr::null_mut(), {
        let handle = node as *const GosXml;
        if handle.is_null() {
            return alloc_cstring(b"");
        }
        // SAFETY: `handle` was produced by `gos_rt_xml_parse`'s
        // `Box::into_raw` and is still live (the handle outlives the
        // encode call; ownership is not transferred here).
        let xml = unsafe { &*handle };
        alloc_cstring(encode(&xml.node).as_bytes())
    })
}

#[cfg(test)]
mod xml_codec_tests {
    use super::*;

    #[test]
    fn roundtrip_matches_quick_xml_bytes() {
        let src = "<note id=\"7\"><to>Tove</to><from>Jani</from>\
                   <body>Don't &lt;forget&gt; me</body><empty/></note>";
        let node = parse(src).expect("parse");
        let out = encode(&node);
        assert_eq!(
            out,
            "<note id=\"7\"><to>Tove</to><from>Jani</from>\
             <body>Don&apos;t &lt;forget&gt; me</body><empty/></note>"
        );
    }

    #[test]
    fn empty_document_is_an_error() {
        assert!(parse("   ").is_err());
    }
}
