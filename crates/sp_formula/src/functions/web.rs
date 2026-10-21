//! Web functions that compute locally: ENCODEURL and FILTERXML (an XPath subset).
//! WEBSERVICE (and IMAGE / STOCKHISTORY / the CUBE family) need a network or OLAP connection
//! and are not implemented: a formula never makes a network request.

use super::{Registry, lift1};
use crate::eval::{Array2, EvalValue, to_text};
use quick_xml::Reader;
use quick_xml::events::Event;
use sp_core::{ErrorKind, Value};

/// Percent-encode everything but RFC 3986 unreserved characters (UTF-8 bytes).
pub fn encode_url(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

// ---------------------------------------------------------------------------------------
// A small XML tree

#[derive(Debug, Clone, Default)]
struct Elem {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
}

#[derive(Debug, Clone)]
enum Node {
    Elem(Elem),
    Text(String),
}

impl Elem {
    fn elems(&self) -> impl Iterator<Item = &Elem> {
        self.children.iter().filter_map(|c| match c {
            Node::Elem(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// Concatenated text of the element and its descendants.
    fn text(&self) -> String {
        let mut s = String::new();
        for c in &self.children {
            match c {
                Node::Text(t) => s.push_str(t),
                Node::Elem(e) => s.push_str(&e.text()),
            }
        }
        s
    }

    fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(a, _)| a == k)
            .map(|(_, v)| v.as_str())
    }
}

const MAX_DEPTH: usize = 256;

/// Append text, merging with a preceding text node (entities split text into pieces).
fn push_text(e: &mut Elem, s: &str) {
    match e.children.last_mut() {
        Some(Node::Text(t)) => t.push_str(s),
        _ => e.children.push(Node::Text(s.to_string())),
    }
}

/// Parse a document; `None` when it isn't well-formed.
fn parse(xml: &str) -> Option<Elem> {
    let mut r = Reader::from_str(xml);
    r.config_mut().trim_text(false);
    // A virtual root holding the document element.
    let mut stack: Vec<Elem> = vec![Elem::default()];
    let attrs_of = |e: &quick_xml::events::BytesStart| -> Option<Vec<(String, String)>> {
        let mut v = Vec::new();
        for a in e.attributes() {
            let a = a.ok()?;
            let k = a.key.as_ref().to_string();
            let val = a
                .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                .ok()?
                .into_owned();
            v.push((k, val));
        }
        Some(v)
    };
    loop {
        match r.read_event().ok()? {
            Event::Start(e) => {
                if stack.len() > MAX_DEPTH {
                    return None;
                }
                stack.push(Elem {
                    name: e.name().as_ref().to_string(),
                    attrs: attrs_of(&e)?,
                    children: Vec::new(),
                });
            }
            Event::Empty(e) => {
                let el = Elem {
                    name: e.name().as_ref().to_string(),
                    attrs: attrs_of(&e)?,
                    children: Vec::new(),
                };
                stack.last_mut()?.children.push(Node::Elem(el));
            }
            Event::End(_) => {
                let done = stack.pop()?;
                stack.last_mut()?.children.push(Node::Elem(done));
            }
            Event::Text(t) => {
                let s = t.xml10_content().to_string();
                if stack.len() > 1 {
                    push_text(stack.last_mut()?, &s);
                } else if !s.trim().is_empty() {
                    return None; // text outside the document element
                }
            }
            Event::CData(t) => {
                let s = t.to_string();
                push_text(stack.last_mut()?, &s);
            }
            Event::GeneralRef(r) => {
                let s = match r.resolve_char_ref() {
                    Ok(Some(c)) => c.to_string(),
                    _ => match &*r {
                        "amp" => "&",
                        "lt" => "<",
                        "gt" => ">",
                        "quot" => "\"",
                        "apos" => "'",
                        _ => return None, // undeclared entity: not well-formed
                    }
                    .to_string(),
                };
                push_text(stack.last_mut()?, &s);
            }
            Event::Eof => break,
            _ => {}
        }
    }
    let root = stack.pop()?;
    (stack.is_empty() && root.elems().count() == 1).then_some(root)
}

// ---------------------------------------------------------------------------------------
// XPath subset

#[derive(Debug, Clone, PartialEq)]
enum Pred {
    Index(usize),
    Last,
    HasAttr(String),
    AttrEq(String, String),
    ChildEq(String, String),
    TextEq(String),
}

#[derive(Debug, Clone, PartialEq)]
enum Test {
    Name(String),
    Any,
    Attr(String),
    Text,
}

#[derive(Debug, Clone, PartialEq)]
struct Step {
    descendant: bool,
    test: Test,
    preds: Vec<Pred>,
}

fn unquote(s: &str) -> Option<String> {
    let s = s.trim();
    let q = s.chars().next()?;
    (matches!(q, '\'' | '"') && s.len() >= 2 && s.ends_with(q))
        .then(|| s[1..s.len() - 1].to_string())
}

fn parse_pred(p: &str) -> Option<Pred> {
    let p = p.trim();
    if p == "last()" {
        return Some(Pred::Last);
    }
    if let Ok(n) = p.parse::<usize>() {
        return (n >= 1).then_some(Pred::Index(n));
    }
    if let Some((l, r)) = p.split_once('=') {
        let v = unquote(r)?;
        let l = l.trim();
        return Some(if let Some(a) = l.strip_prefix('@') {
            Pred::AttrEq(a.to_string(), v)
        } else if l == "text()" || l == "." {
            Pred::TextEq(v)
        } else {
            Pred::ChildEq(l.to_string(), v)
        });
    }
    p.strip_prefix('@').map(|a| Pred::HasAttr(a.to_string()))
}

/// `//a/b[2]/@id` → steps. `None` for what the subset doesn't cover.
fn parse_path(path: &str) -> Option<Vec<Step>> {
    let path = path.trim();
    if path.is_empty() || path.len() > 4096 {
        return None;
    }
    let mut steps = Vec::new();
    let mut rest = path;
    // A relative path starts from the document element's parent (like an absolute one).
    let mut descendant = false;
    loop {
        if let Some(r) = rest.strip_prefix("//") {
            descendant = true;
            rest = r;
        } else if let Some(r) = rest.strip_prefix('/') {
            rest = r;
        }
        let end = {
            let mut depth = 0;
            let mut q: Option<char> = None;
            rest.char_indices()
                .find(|(_, c)| {
                    match (q, *c) {
                        (Some(x), c) if c == x => q = None,
                        (Some(_), _) => {}
                        (None, '\'' | '"') => q = Some(*c),
                        (None, '[') => depth += 1,
                        (None, ']') => depth -= 1,
                        (None, '/') if depth == 0 => return true,
                        _ => {}
                    }
                    false
                })
                .map_or(rest.len(), |(i, _)| i)
        };
        let raw = &rest[..end];
        rest = &rest[end..];
        let (name, mut tail) = match raw.find('[') {
            Some(i) => (&raw[..i], &raw[i..]),
            None => (raw, ""),
        };
        let mut preds = Vec::new();
        while let Some(t) = tail.strip_prefix('[') {
            let close = t.find(']')?;
            preds.push(parse_pred(&t[..close])?);
            tail = &t[close + 1..];
        }
        if !tail.is_empty() || name.is_empty() {
            return None;
        }
        let test = match name {
            "*" => Test::Any,
            "text()" => Test::Text,
            n => match n.strip_prefix('@') {
                Some(a) if !a.is_empty() => Test::Attr(a.to_string()),
                Some(_) => return None,
                None => {
                    if n.chars().any(|c| "()=,'\" ".contains(c)) {
                        return None;
                    }
                    Test::Name(n.to_string())
                }
            },
        };
        steps.push(Step {
            descendant,
            test,
            preds,
        });
        descendant = false;
        if rest.is_empty() {
            break;
        }
    }
    Some(steps)
}

/// A match: an element, or a text value (attribute / text()).
#[derive(Debug, Clone)]
enum Hit<'a> {
    Elem(&'a Elem),
    Text(String),
}

fn descendants<'a>(e: &'a Elem, out: &mut Vec<&'a Elem>) {
    for c in e.elems() {
        out.push(c);
        descendants(c, out);
    }
}

fn keep(e: &Elem, preds: &[Pred]) -> bool {
    preds.iter().all(|p| match p {
        Pred::HasAttr(a) => e.attr(a).is_some(),
        Pred::AttrEq(a, v) => e.attr(a) == Some(v.as_str()),
        Pred::ChildEq(n, v) => e.elems().any(|c| c.name == *n && c.text() == *v),
        Pred::TextEq(v) => e.text() == *v,
        Pred::Index(_) | Pred::Last => true,
    })
}

fn apply_positional<'a>(mut v: Vec<&'a Elem>, preds: &[Pred]) -> Vec<&'a Elem> {
    for p in preds {
        v = match p {
            Pred::Index(n) => v.get(n - 1).copied().into_iter().collect(),
            Pred::Last => v.last().copied().into_iter().collect(),
            _ => v,
        };
    }
    v
}

fn select<'a>(root: &'a Elem, steps: &[Step]) -> Vec<Hit<'a>> {
    let mut ctx: Vec<&Elem> = vec![root];
    let mut hits: Vec<Hit<'a>> = Vec::new();
    for (i, st) in steps.iter().enumerate() {
        let last = i + 1 == steps.len();
        let mut next: Vec<&Elem> = Vec::new();
        for e in &ctx {
            let pool: Vec<&Elem> = if st.descendant {
                let mut d = Vec::new();
                descendants(e, &mut d);
                d
            } else {
                e.elems().collect()
            };
            match &st.test {
                Test::Name(_) | Test::Any => {
                    let matched: Vec<&Elem> = pool
                        .into_iter()
                        .filter(|c| match &st.test {
                            Test::Name(n) => c.name == *n,
                            _ => true,
                        })
                        .filter(|c| keep(c, &st.preds))
                        .collect();
                    next.extend(apply_positional(matched, &st.preds));
                }
                Test::Attr(a) if last => {
                    let owners: Vec<&Elem> = if st.descendant {
                        let mut d = vec![*e];
                        descendants(e, &mut d);
                        d
                    } else {
                        vec![*e]
                    };
                    hits.extend(
                        owners
                            .iter()
                            .filter_map(|o| o.attr(a))
                            .map(|v| Hit::Text(v.to_string())),
                    );
                }
                Test::Text if last => {
                    hits.extend(e.children.iter().filter_map(|c| match c {
                        Node::Text(t) if !t.trim().is_empty() => Some(Hit::Text(t.clone())),
                        _ => None,
                    }));
                }
                _ => return Vec::new(),
            }
        }
        if matches!(st.test, Test::Attr(_) | Test::Text) {
            return hits;
        }
        ctx = next;
    }
    ctx.into_iter().map(Hit::Elem).collect()
}

/// FILTERXML: the matches' text values; numbers come back as numbers.
pub fn filter_xml(xml: &str, path: &str) -> Result<Vec<Value>, ErrorKind> {
    let root = parse(xml).ok_or(ErrorKind::Value)?;
    let steps = parse_path(path).ok_or(ErrorKind::Value)?;
    let out: Vec<Value> = select(&root, &steps)
        .into_iter()
        .map(|h| {
            let s = match h {
                Hit::Elem(e) => e.text(),
                Hit::Text(t) => t,
            };
            match s.trim().parse::<f64>() {
                Ok(n) if n.is_finite() && !s.trim().is_empty() => Value::Number(n),
                _ => Value::text(s),
            }
        })
        .collect();
    if out.is_empty() {
        Err(ErrorKind::Value)
    } else {
        Ok(out)
    }
}

pub fn register(r: &mut Registry) {
    r.add("ENCODEURL", 1, 1, |ev, a| {
        let Some(x) = a.first() else {
            return EvalValue::err(ErrorKind::Value);
        };
        let v = ev.eval(x);
        lift1(ev, v, |x| match to_text(x) {
            Ok(s) => Value::text(encode_url(&s)),
            Err(e) => Value::Error(e),
        })
    });
    r.add("FILTERXML", 2, 2, |ev, a| {
        let (Some(x), Some(p)) = (a.first(), a.get(1)) else {
            return EvalValue::err(ErrorKind::Value);
        };
        let xml = match ev.text(x) {
            Ok(s) => s,
            Err(e) => return EvalValue::err(e),
        };
        let path = match ev.text(p) {
            Ok(s) => s,
            Err(e) => return EvalValue::err(e),
        };
        match filter_xml(&xml, &path) {
            Ok(v) if v.len() == 1 => {
                EvalValue::Scalar(v.into_iter().next().unwrap_or(Value::Empty))
            }
            Ok(v) => EvalValue::Array(Array2::new(v.len(), 1, v)),
            Err(e) => EvalValue::err(e),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fx(xml: &str, p: &str) -> Result<Vec<Value>, ErrorKind> {
        filter_xml(xml, p)
    }

    #[test]
    fn encode_url_like_the_target() {
        assert_eq!(
            encode_url("http://contoso.sharepoint.com/Finance/Profit and Loss Statement.xlsx"),
            "http%3A%2F%2Fcontoso.sharepoint.com%2FFinance%2FProfit%20and%20Loss%20Statement.xlsx"
        );
        assert_eq!(encode_url("a-b_c.d~e"), "a-b_c.d~e");
        assert_eq!(encode_url("é"), "%C3%A9");
        assert_eq!(encode_url(""), "");
    }

    #[test]
    fn filter_xml_paths() {
        let x = r#"<r><item id="1"><name>Apple</name><qty>3</qty></item><item id="2"><name>Pear</name><qty>5</qty></item></r>"#;
        assert_eq!(
            fx(x, "//name").unwrap(),
            vec![Value::text("Apple"), Value::text("Pear")]
        );
        assert_eq!(
            fx(x, "/r/item/qty").unwrap(),
            vec![Value::Number(3.0), Value::Number(5.0)]
        );
        assert_eq!(fx(x, "//item[2]/name").unwrap(), vec![Value::text("Pear")]);
        assert_eq!(
            fx(x, "//item[last()]/@id").unwrap(),
            vec![Value::Number(2.0)]
        );
        assert_eq!(
            fx(x, "//item[@id='1']/name").unwrap(),
            vec![Value::text("Apple")]
        );
        assert_eq!(
            fx(x, "//item[name='Pear']/qty").unwrap(),
            vec![Value::Number(5.0)]
        );
        assert_eq!(
            fx(x, "//name/text()").unwrap(),
            vec![Value::text("Apple"), Value::text("Pear")]
        );
        assert_eq!(
            fx(x, "//@id").unwrap(),
            vec![Value::Number(1.0), Value::Number(2.0)]
        );
        assert_eq!(fx(x, "/r/*[1]/name").unwrap(), vec![Value::text("Apple")]);
        // No match, bad XML, bad path.
        assert_eq!(fx(x, "//nope"), Err(ErrorKind::Value));
        assert_eq!(fx("<a><b></a>", "//b"), Err(ErrorKind::Value));
        assert_eq!(fx("not xml", "//b"), Err(ErrorKind::Value));
        assert_eq!(fx(x, "//item[contains(name,'A')]"), Err(ErrorKind::Value));
        assert_eq!(fx(x, ""), Err(ErrorKind::Value));
        // Entities and CDATA.
        assert_eq!(
            fx("<a><b>x &amp; y</b><c><![CDATA[<z>]]></c></a>", "//b").unwrap(),
            vec![Value::text("x & y")]
        );
        assert_eq!(
            fx("<a><c><![CDATA[<z>]]></c></a>", "//c").unwrap(),
            vec![Value::text("<z>")]
        );
    }

    #[test]
    fn deep_or_hostile_input_is_an_error_not_a_crash() {
        let deep = format!("{}{}", "<a>".repeat(5000), "</a>".repeat(5000));
        assert_eq!(fx(&deep, "//a"), Err(ErrorKind::Value));
        assert!(fx("<a/>", &"/a".repeat(3000)).is_err());
        assert!(fx("<a/>", "//a[").is_err());
        assert!(fx("<a/>", "//a[0]").is_err());
    }
}
