// Runtime support for `std::encoding::xml` — XML parsing and encoding.
//
// Exposes a simple tree-based XML value type that covers the common
// cases: elements with attributes and mixed text/element children.
// For streaming use, the underlying quick-xml reader is exposed via
// `events()`. All strings are UTF-8.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

use quick_xml::Reader;
use quick_xml::events::Event;
use quick_xml::writer::Writer;

use crate::errors::Error;

/// A single attribute name → value pair.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attr {
    /// Attribute name.
    pub name: String,
    /// Attribute value.
    pub value: String,
}

/// A node in an XML tree.
#[derive(Debug, Clone, PartialEq)]
pub enum Node {
    /// An element with a tag name, attributes, and child nodes.
    Element {
        /// Tag name.
        name: String,
        /// Ordered attribute map.
        attrs: BTreeMap<String, String>,
        /// Child nodes.
        children: Vec<Node>,
    },
    /// A text node.
    Text(String),
}

impl Node {
    /// Returns the tag name if this is an element.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Element { name, .. } => Some(name),
            Self::Text(_) => None,
        }
    }

    /// Returns the text content if this is a `Text` node.
    #[must_use]
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Element { .. } => None,
        }
    }

    /// Collects the text of all direct `Text` children.
    #[must_use]
    pub fn inner_text(&self) -> String {
        match self {
            Self::Text(s) => s.clone(),
            Self::Element { children, .. } => children
                .iter()
                .filter_map(|c| {
                    if let Self::Text(s) = c {
                        Some(s.as_str())
                    } else {
                        None
                    }
                })
                .collect(),
        }
    }

    /// Returns child elements with the given tag name.
    #[must_use]
    pub fn children_named<'a>(&'a self, tag: &str) -> Vec<&'a Node> {
        match self {
            Self::Element { children, .. } => {
                children.iter().filter(|c| c.name() == Some(tag)).collect()
            }
            Self::Text(_) => vec![],
        }
    }

    /// Returns the value of an attribute.
    #[must_use]
    pub fn attr(&self, name: &str) -> Option<&str> {
        match self {
            Self::Element { attrs, .. } => attrs.get(name).map(String::as_str),
            Self::Text(_) => None,
        }
    }
}

/// Parses an XML document into a tree of [`Node`]s. Returns the root element.
pub fn parse(src: &str) -> Result<Node, Error> {
    let mut reader = Reader::from_str(src);
    reader.config_mut().trim_text(true);
    let mut stack: Vec<(String, BTreeMap<String, String>, Vec<Node>)> = Vec::new();
    let mut root: Option<Node> = None;
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.local_name().as_ref()).into_owned();
                let mut attrs = BTreeMap::new();
                for attr in e.attributes().flatten() {
                    let key = String::from_utf8_lossy(attr.key.local_name().as_ref()).into_owned();
                    let val = String::from_utf8_lossy(&attr.value).into_owned();
                    attrs.insert(key, val);
                }
                stack.push((name, attrs, Vec::new()));
            }
            Ok(Event::Empty(e)) => {
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
            Ok(Event::End(_)) => {
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
            Ok(Event::Text(e)) => {
                let text = e
                    .unescape()
                    .map_err(|err| Error::new(format!("xml: {err}")))?;
                if !text.trim().is_empty() {
                    if let Some(parent) = stack.last_mut() {
                        parent.2.push(Node::Text(text.into_owned()));
                    }
                }
            }
            Ok(Event::CData(e)) => {
                let text = String::from_utf8_lossy(e.as_ref()).into_owned();
                if let Some(parent) = stack.last_mut() {
                    parent.2.push(Node::Text(text));
                }
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(e) => return Err(Error::new(format!("xml: parse: {e}"))),
        }
    }
    root.ok_or_else(|| Error::new("xml: empty document"))
}

/// Serialises a [`Node`] tree to an XML string. Does not emit an XML declaration.
#[must_use]
pub fn encode(node: &Node) -> String {
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

/// Escapes XML special characters in `s` (`&`, `<`, `>`, `"`, `'`).
#[must_use]
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_element() {
        let doc = r#"<root><child name="hello">world</child></root>"#;
        let root = parse(doc).unwrap();
        assert_eq!(root.name(), Some("root"));
        let children = root.children_named("child");
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].attr("name"), Some("hello"));
        assert_eq!(children[0].inner_text(), "world");
    }

    #[test]
    fn parse_self_closing() {
        let doc = "<root><empty/></root>";
        let root = parse(doc).unwrap();
        let children = root.children_named("empty");
        assert_eq!(children.len(), 1);
    }

    #[test]
    fn encode_round_trips() {
        use std::collections::BTreeMap;
        let mut attrs = BTreeMap::new();
        attrs.insert("lang".to_string(), "gos".to_string());
        let node = Node::Element {
            name: "root".to_string(),
            attrs,
            children: vec![Node::Text("hello".to_string())],
        };
        let encoded = encode(&node);
        let decoded = parse(&encoded).unwrap();
        assert_eq!(decoded.name(), Some("root"));
        assert_eq!(decoded.inner_text(), "hello");
    }

    #[test]
    fn escape_special_chars() {
        assert_eq!(
            escape("<b>Hello & 'World'</b>"),
            "&lt;b&gt;Hello &amp; &apos;World&apos;&lt;/b&gt;"
        );
    }
}
