//! Worksheet `headerFooter` (ECMA-376 Part 1 §18.3.1.46) ↔ `PageSetup::header_footer`.

use crate::xml::{Node, esc, unescape_ooxml};
use sp_model::HeaderFooter;
use std::fmt::Write as _;

/// Longest header/footer text kept (the file format allows 255 characters).
const MAX_LEN: usize = 4_096;

fn flag(v: Option<&str>, default: bool) -> bool {
    v.map_or(default, |v| v == "1" || v.eq_ignore_ascii_case("true"))
}

/// Read a `headerFooter` element.
pub fn read_header_footer(n: &Node) -> HeaderFooter {
    let text = |name: &str| {
        n.child(name)
            .map(|c| unescape_ooxml(&c.all_text()).chars().take(MAX_LEN).collect())
            .unwrap_or_default()
    };
    HeaderFooter {
        odd_header: text("oddHeader"),
        odd_footer: text("oddFooter"),
        even_header: text("evenHeader"),
        even_footer: text("evenFooter"),
        first_header: text("firstHeader"),
        first_footer: text("firstFooter"),
        different_odd_even: flag(n.attr("differentOddEven"), false),
        different_first: flag(n.attr("differentFirst"), false),
        scale_with_doc: flag(n.attr("scaleWithDoc"), true),
        align_with_margins: flag(n.attr("alignWithMargins"), true),
    }
}

/// The `headerFooter` element (empty string when everything is default).
pub fn header_footer_xml(hf: &HeaderFooter) -> String {
    if hf.is_empty() {
        return String::new();
    }
    let mut x = String::from("<headerFooter");
    for (name, on, default) in [
        ("differentOddEven", hf.different_odd_even, false),
        ("differentFirst", hf.different_first, false),
        ("scaleWithDoc", hf.scale_with_doc, true),
        ("alignWithMargins", hf.align_with_margins, true),
    ] {
        if on != default {
            let _ = write!(x, r#" {name}="{}""#, u8::from(on));
        }
    }
    x.push('>');
    for (name, text) in [
        ("oddHeader", &hf.odd_header),
        ("oddFooter", &hf.odd_footer),
        ("evenHeader", &hf.even_header),
        ("evenFooter", &hf.even_footer),
        ("firstHeader", &hf.first_header),
        ("firstFooter", &hf.first_footer),
    ] {
        if !text.is_empty() {
            let _ = write!(x, "<{name}>{}</{name}>", esc(text));
        }
    }
    x.push_str("</headerFooter>");
    x
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse_dom;

    #[test]
    fn round_trip() {
        let hf = HeaderFooter {
            odd_header: "&L&\"Arial,Bold\"Q&&A&C&P of &N".into(),
            odd_footer: "<page &P>".into(),
            first_footer: "first".into(),
            different_first: true,
            scale_with_doc: false,
            ..HeaderFooter::default()
        };
        let xml = header_footer_xml(&hf);
        assert!(xml.contains(r#"differentFirst="1""#));
        assert!(xml.contains("&amp;C&amp;P"));
        let back = read_header_footer(&parse_dom(&xml).unwrap_or_default());
        assert_eq!(back, hf);
        assert_eq!(header_footer_xml(&HeaderFooter::default()), "");
    }
}
