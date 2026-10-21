//! Small XML helpers over quick-xml: a DOM for small parts, escaping for the writer.

use quick_xml::Reader;
use quick_xml::events::{BytesRef, BytesStart, Event};

use crate::IoError;

#[derive(Debug, Clone, Default)]
pub struct Node {
    pub name: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
    pub text: String,
}

impl Node {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }

    pub fn child(&self, name: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.name == name)
    }

    pub fn children_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Node> + 'a {
        self.children.iter().filter(move |c| c.name == name)
    }

    /// Concatenated text of this node and all descendants.
    pub fn all_text(&self) -> String {
        let mut s = self.text.clone();
        for c in &self.children {
            s.push_str(&c.all_text());
        }
        s
    }
}

/// Drop a leading UTF-8 byte-order mark. The XML reader skips it and reports positions
/// without it, so text that keeps it would be sliced 3 bytes off.
pub fn strip_bom(mut s: String) -> String {
    if s.starts_with('\u{feff}') {
        s.drain(..'\u{feff}'.len_utf8());
    }
    s
}

pub fn local(name: &str) -> &str {
    name.rsplit(':').next().unwrap_or(name)
}

/// Attributes with namespace prefixes stripped from keys (`r:id` → `id`).
pub fn attrs(e: &BytesStart<'_>) -> Vec<(String, String)> {
    e.attributes()
        .filter_map(Result::ok)
        .map(|a| {
            let k = local(a.key.as_ref()).to_string();
            let v = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .map(|c| c.into_owned())
                .unwrap_or_default();
            (k, v)
        })
        .collect()
}

/// Resolve `&amp;`-style and `&#x30;`-style references reported as separate events.
pub fn resolve_ref(r: &BytesRef<'_>) -> String {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return c.to_string();
    }
    match &**r {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => "",
    }
    .to_string()
}

/// Maximum element nesting accepted when building a DOM.
const MAX_DEPTH: usize = 256;

pub fn parse_dom(xml: &str) -> Result<Node, IoError> {
    let mut reader = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![Node::default()];
    loop {
        match reader
            .read_event()
            .map_err(|e| IoError::Xml(e.to_string()))?
        {
            Event::Start(e) => {
                if stack.len() > MAX_DEPTH {
                    return Err(IoError::Xml("document nested too deeply".into()));
                }
                stack.push(Node {
                    name: local(e.name().as_ref()).to_string(),
                    attrs: attrs(&e),
                    ..Node::default()
                });
            }
            Event::Empty(e) => {
                let n = Node {
                    name: local(e.name().as_ref()).to_string(),
                    attrs: attrs(&e),
                    ..Node::default()
                };
                if let Some(top) = stack.last_mut() {
                    top.children.push(n);
                }
            }
            Event::End(_) => {
                if stack.len() > 1
                    && let Some(n) = stack.pop()
                    && let Some(top) = stack.last_mut()
                {
                    top.children.push(n);
                }
            }
            Event::Text(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t.xml10_content());
                }
            }
            Event::CData(t) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&t);
                }
            }
            Event::GeneralRef(r) => {
                if let Some(top) = stack.last_mut() {
                    top.text.push_str(&resolve_ref(&r));
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let mut root = stack.into_iter().next().unwrap_or_default();
    Ok(if root.children.len() == 1 {
        root.children.pop().unwrap_or_default()
    } else {
        root
    })
}

pub fn esc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            // A raw CR would be read back as part of a line break ("\r\n" → "\n").
            '\r' => out.push_str("&#13;"),
            // Control characters other than tab/newline are not allowed in XML 1.0.
            c if (c as u32) < 0x20 && c != '\t' && c != '\n' => {
                out.push_str(&format!("_x{:04X}_", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// Decode the `_xHHHH_` escapes used in shared strings for control characters.
pub fn unescape_ooxml(s: &str) -> String {
    if !s.contains("_x") {
        return s.to_string();
    }
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find("_x") {
        out.push_str(rest.get(..i).unwrap_or(""));
        let tail = rest.get(i..).unwrap_or("");
        let hex = tail.get(2..6).unwrap_or("");
        if tail.get(6..7) == Some("_")
            && hex.len() == 4
            && let Ok(code) = u32::from_str_radix(hex, 16)
            && let Some(c) = char::from_u32(code)
        {
            out.push(c);
            rest = tail.get(7..).unwrap_or("");
        } else {
            out.push_str("_x");
            rest = tail.get(2..).unwrap_or("");
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dom_and_entities() {
        let n = parse_dom(r#"<a x="1&amp;2"><b>t&lt;u</b><c/></a>"#).unwrap();
        assert_eq!(n.name, "a");
        assert_eq!(n.attr("x"), Some("1&2"));
        assert_eq!(n.child("b").unwrap().text, "t<u");
        assert!(n.child("c").is_some());
    }

    #[test]
    fn ooxml_escapes() {
        assert_eq!(unescape_ooxml("a_x000D_b"), "a\rb");
        assert_eq!(unescape_ooxml("_x_y"), "_x_y");
        assert_eq!(esc("a\u{1}<"), "a_x0001_&lt;");
        assert_eq!(esc("a\r\nb"), "a&#13;\nb");
    }
}
