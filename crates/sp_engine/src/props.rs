//! Spreadsheet properties as a flat map, for tests that check exactly what an action changed.
//!
//! [`snapshot`] turns a session into `path → value` lines:
//!
//! ```text
//! Sheet1!B3.input              = "=C3*D3"
//! Sheet1!B3.value              = 15
//! Sheet1!B3.style.bold         = true         (only where it differs from the default style)
//! Sheet1.col_widths.1          = 18
//! Sheet1.page_setup.orientation= "Landscape"
//! workbook.names.0.name        = "Rate"
//! view.zoom                    = 1.5
//! ```
//!
//! Cell styles are resolved from the shared style table, so a style change shows up as the
//! properties that changed, not as a style id. Internal state is left out: the style pool,
//! unmodelled file content kept for saving, spill bookkeeping (spilled values are cell
//! values; only a legacy array's area shows, as `Sheet1!B3.array`), pivot cache records (derived from the source). Long byte arrays (images) are
//! summarised by length and hash.

use crate::Session;
use serde_json::Value as Json;
use sp_model::{Sheet, Workbook, style::Style};
use std::collections::BTreeMap;

pub type PropMap = BTreeMap<String, String>;

/// One property that differs between two snapshots (`None` = absent on that side).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Change {
    pub path: String,
    pub before: Option<String>,
    pub after: Option<String>,
}

impl std::fmt::Display for Change {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let v = |x: &Option<String>| x.clone().unwrap_or_else(|| "∅".into());
        write!(f, "{}: {} → {}", self.path, v(&self.before), v(&self.after))
    }
}

/// Fields that are internal state rather than spreadsheet properties.
const SKIP_WORKBOOK: [&str; 3] = ["styles", "passthrough", "sheets"];
const SKIP_SHEET: [&str; 3] = ["cells", "passthrough", "spills"];

fn scalar(v: &Json) -> String {
    match v {
        Json::String(s) => format!("{s:?}"),
        // Sizes, zoom, margins: 7 significant digits (a stored f32 such as 1.1 widens to
        // 1.100000023841858). Cell values are printed separately, at full precision.
        Json::Number(n) if n.is_f64() => {
            let x = n.as_f64().unwrap_or(0.0);
            if x == 0.0 || !x.is_finite() {
                return n.to_string();
            }
            let digits = 7 - (x.abs().log10().floor() as i32) - 1;
            let r = if digits > 0 {
                let p = 10f64.powi(digits);
                (x * p).round() / p
            } else {
                x.round()
            };
            serde_json::Number::from_f64(r).map_or_else(|| r.to_string(), |y| y.to_string())
        }
        other => other.to_string(),
    }
}

/// View fields that follow from the window size and zoom, not properties anyone sets.
const DERIVED_VIEW: [&str; 2] = ["page_cols", "page_rows"];

/// Flatten `v` under `prefix` into `out` (objects by key, arrays by index).
fn flatten(prefix: &str, v: &Json, out: &mut PropMap) {
    let join = |k: &str| {
        if prefix.is_empty() {
            k.to_string()
        } else {
            format!("{prefix}.{k}")
        }
    };
    match v {
        Json::Object(m) => {
            for (k, x) in m {
                // Pivot caches are rebuilt from their source on refresh.
                if k == "records" && prefix.contains("cache") {
                    continue;
                }
                flatten(&join(k), x, out);
            }
        }
        Json::Array(a) if a.len() > 64 && a.iter().all(Json::is_number) => {
            // Image bytes and the like: length and a hash, not 100k lines.
            let mut h: u64 = 0xcbf2_9ce4_8422_2325;
            for x in a {
                h ^= x.as_u64().unwrap_or(0);
                h = h.wrapping_mul(0x0100_0000_01b3);
            }
            out.insert(
                prefix.to_string(),
                format!("<{} numbers #{h:016x}>", a.len()),
            );
        }
        Json::Array(a) => {
            for (i, x) in a.iter().enumerate() {
                flatten(&join(&i.to_string()), x, out);
            }
        }
        // An unset optional property is simply absent.
        Json::Null => {}
        other => {
            out.insert(prefix.to_string(), scalar(other));
        }
    }
}

/// A cell value as the grid shows it: numbers plain, text quoted, errors by code.
fn value_text(v: &sp_core::Value) -> String {
    match v {
        sp_core::Value::Number(n) => {
            serde_json::Number::from_f64(*n).map_or_else(|| n.to_string(), |x| x.to_string())
        }
        sp_core::Value::Text(t) => format!("{:?}", &**t),
        sp_core::Value::Bool(b) => b.to_string(),
        sp_core::Value::Error(e) => e.as_str().to_string(),
        sp_core::Value::Empty => String::new(),
    }
}

fn cells(wb: &Workbook, sheet: &Sheet, out: &mut PropMap) {
    let default = serde_json::to_value(Style::default()).unwrap_or(Json::Null);
    for (a, c) in sheet.iter() {
        let at = format!("{}!{}", sheet.name, a.to_a1());
        let input = c.input_text();
        if !input.is_empty() {
            out.insert(format!("{at}.input"), format!("{input:?}"));
        }
        if !c.value.is_empty() {
            out.insert(format!("{at}.value"), value_text(&c.value));
        }
        // Only the style fields that differ from the default.
        let style = serde_json::to_value(wb.styles.get(c.style)).unwrap_or(Json::Null);
        if let (Json::Object(s), Json::Object(d)) = (&style, &default) {
            for (k, v) in s {
                if d.get(k) != Some(v) {
                    flatten(&format!("{at}.style.{k}"), v, out);
                }
            }
        }
    }
    // A legacy (Ctrl+Shift+Enter) array formula's area is a property of its top-left cell
    // (the file's `t="array" ref=…`); dynamic spill areas are derived and left out.
    for a in sheet.spills.iter().filter(|a| a.fixed) {
        out.insert(
            format!("{}!{}.array", sheet.name, a.range.start.to_a1()),
            format!("{:?}", a.range.to_a1()),
        );
    }
}

/// Every spreadsheet property of the session (workbook, sheets, cells, view).
pub fn snapshot(s: &Session) -> PropMap {
    let mut out = PropMap::new();
    let wb = &s.wb;
    if let Ok(Json::Object(m)) = serde_json::to_value(wb) {
        for (k, v) in &m {
            if !SKIP_WORKBOOK.contains(&k.as_str()) {
                flatten(&format!("workbook.{k}"), v, &mut out);
            }
        }
    }
    for (i, sh) in wb.sheets.iter().enumerate() {
        out.insert(
            format!("workbook.sheet_order.{i}"),
            format!("{:?}", sh.name),
        );
        if let Ok(Json::Object(m)) = serde_json::to_value(sh) {
            for (k, v) in &m {
                if !SKIP_SHEET.contains(&k.as_str()) && k != "name" {
                    flatten(&format!("{}.{k}", sh.name), v, &mut out);
                }
            }
        }
        cells(wb, sh, &mut out);
    }
    if let Ok(Json::Object(m)) = serde_json::to_value(&s.view) {
        for (k, v) in &m {
            if !DERIVED_VIEW.contains(&k.as_str()) {
                flatten(&format!("view.{k}"), v, &mut out);
            }
        }
    }
    out
}

/// Properties that differ between two snapshots, in path order.
pub fn diff(a: &PropMap, b: &PropMap) -> Vec<Change> {
    let mut out = Vec::new();
    for (k, v) in a {
        match b.get(k) {
            Some(w) if w == v => {}
            other => out.push(Change {
                path: k.clone(),
                before: Some(v.clone()),
                after: other.cloned(),
            }),
        }
    }
    for (k, w) in b {
        if !a.contains_key(k) {
            out.push(Change {
                path: k.clone(),
                before: None,
                after: Some(w.clone()),
            });
        }
    }
    out.sort_by(|x, y| x.path.cmp(&y.path));
    out
}

/// The schema path of a property: sheet names, cell addresses and list indexes replaced, so
/// `Sheet1!B3.style.bold` and `Data!Z9.style.bold` both count as `cell.style.bold`.
pub fn schema_path(path: &str, sheets: &[String]) -> String {
    let (head, rest) = match path.split_once('!') {
        Some((_, r)) => ("cell", r.split_once('.').map_or("", |(_, x)| x)),
        None => match path.split_once('.') {
            Some((h, r)) if sheets.iter().any(|s| s == h) => ("sheet", r),
            Some((h, r)) => (h, r),
            None => (path, ""),
        },
    };
    let parts: Vec<&str> = rest
        .split('.')
        .filter(|p| !p.is_empty() && !p.chars().all(|c| c.is_ascii_digit()))
        .collect();
    if parts.is_empty() {
        head.to_string()
    } else {
        format!("{head}.{}", parts.join("."))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn diff_names_exactly_what_changed() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "B3", "input": "10"}))
            .unwrap();
        let a = snapshot(&s);
        s.execute("home.font.bold", &json!({"range": "B3"}))
            .unwrap();
        let b = snapshot(&s);
        let d: Vec<String> = diff(&a, &b).iter().map(|c| c.to_string()).collect();
        assert_eq!(d, vec!["Sheet1!B3.style.bold: ∅ → true"]);
        // A new cell shows only its input and value, not every default style field.
        let mut s2 = Session::new();
        let a = snapshot(&s2);
        s2.execute("cell.set", &json!({"cell": "A1", "input": "=1+1"}))
            .unwrap();
        let d = diff(&a, &snapshot(&s2));
        let paths: Vec<&str> = d.iter().map(|c| c.path.as_str()).collect();
        assert_eq!(paths, vec!["Sheet1!A1.input", "Sheet1!A1.value"]);
    }

    /// A legacy array formula shows its area; a dynamic spill does not.
    #[test]
    fn legacy_array_area_is_a_property() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "={1;2}"}))
            .unwrap();
        assert!(!snapshot(&s).keys().any(|k| k.ends_with(".array")));
        s.execute(
            "cell.array_enter",
            &json!({"range": "C1:C2", "formula": "={1;2}"}),
        )
        .unwrap();
        let snap = snapshot(&s);
        assert_eq!(
            snap.get("Sheet1!C1.array").map(String::as_str),
            Some("\"C1:C2\"")
        );
        assert!(!snap.contains_key("Sheet1!A1.array"));
    }

    #[test]
    fn sheet_and_view_properties_are_included() {
        let mut s = Session::new();
        let a = snapshot(&s);
        s.execute("view.freeze_panes", &json!({"cell": "B2"}))
            .unwrap();
        s.execute("view.zoom", &json!({"percent": 150})).unwrap();
        let d = diff(&a, &snapshot(&s));
        let paths: Vec<&str> = d.iter().map(|c| c.path.as_str()).collect();
        assert!(
            paths.iter().any(|p| p.starts_with("Sheet1.freeze")),
            "{paths:?}"
        );
        assert!(paths.contains(&"view.zoom"), "{paths:?}");
    }

    #[test]
    fn schema_paths_generalise_names_and_indexes() {
        let sheets = vec!["Sheet1".to_string(), "Data".to_string()];
        assert_eq!(
            schema_path("Sheet1!B3.style.bold", &sheets),
            "cell.style.bold"
        );
        assert_eq!(
            schema_path("Data.col_widths.3", &sheets),
            "sheet.col_widths"
        );
        assert_eq!(
            schema_path("Sheet1.comments.0.text", &sheets),
            "sheet.comments.text"
        );
        assert_eq!(
            schema_path("workbook.names.0.name", &sheets),
            "workbook.names.name"
        );
        assert_eq!(schema_path("view.zoom", &sheets), "view.zoom");
    }
}
