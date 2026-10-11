//! Commands behind the shell's dialogs: Paste Special, Format Cells (all tabs in one undoable
//! step, with explicit on/off values rather than toggles), and Find All.

use crate::params::{bool_opt, f64_opt, range, str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session, registry};
use serde_json::json;
use sp_core::CellAddr;
use sp_model::{Border, BorderStyle, Cell, Color, Content, HAlign, Style, StyleId, VAlign};

/// Same cap as the ribbon formatting commands: larger selections restyle only stored cells.
const DENSE_LIMIT: u64 = 200_000;
/// Find All returns at most this many hits.
const FIND_ALL_CAP: usize = 10_000;

fn paste_special(s: &mut Session, p: &Json) -> CmdResult {
    let id = match str_req(p, "mode")? {
        "all" => "home.clipboard.paste",
        "values" => "home.clipboard.paste_values",
        "formulas" => "home.clipboard.paste_formulas",
        "formats" => "home.clipboard.paste_formats",
        "transpose" => "home.clipboard.paste_transpose",
        other => {
            return Err(CmdError::Param(format!(
                "unknown paste mode '{other}' (all|values|formulas|formats|transpose)"
            )));
        }
    };
    let cmd = registry()
        .get(id)
        .ok_or_else(|| CmdError::Unknown(id.to_string()))?;
    if !(cmd.spec.enabled)(s) {
        return Err(CmdError::Disabled(id.to_string()));
    }
    let mut sub = json!({});
    if let (Some(c), Some(obj)) = (str_opt(p, "cell"), sub.as_object_mut()) {
        obj.insert("cell".into(), json!(c));
    }
    (cmd.spec.run)(s, &sub)
}

/// Which attributes a Format Cells OK changes. `None` = leave as is.
#[derive(Debug, Default, Clone, PartialEq)]
struct Patch {
    num_fmt: Option<String>,
    h_align: Option<HAlign>,
    v_align: Option<VAlign>,
    wrap: Option<bool>,
    indent: Option<u8>,
    rotation: Option<i16>,
    font_name: Option<String>,
    size_tenths: Option<u32>,
    bold: Option<bool>,
    italic: Option<bool>,
    underline: Option<u8>,
    strike: Option<bool>,
    font_color: Option<Option<Color>>,
    fill: Option<Option<Color>>,
    locked: Option<bool>,
    hidden: Option<bool>,
    borders: Option<BorderPatch>,
}

#[derive(Debug, Default, Clone, Copy, PartialEq)]
struct BorderPatch {
    left: Option<Border>,
    right: Option<Border>,
    top: Option<Border>,
    bottom: Option<Border>,
    inside_h: Option<Border>,
    inside_v: Option<Border>,
}

fn opt_color(p: &Json, k: &str) -> Result<Option<Option<Color>>, CmdError> {
    match p.get(k) {
        None => Ok(None),
        Some(Json::Null) => Ok(Some(None)),
        Some(v) => v
            .as_str()
            .and_then(Color::from_hex)
            .map(|c| Some(Some(c)))
            .ok_or_else(|| CmdError::Param(format!("'{k}' must be hex like \"FF0000\" or null"))),
    }
}

fn opt_bool(p: &Json, k: &str) -> Result<Option<bool>, CmdError> {
    match p.get(k) {
        None | Some(Json::Null) => Ok(None),
        Some(v) => v
            .as_bool()
            .map(Some)
            .ok_or_else(|| CmdError::Param(format!("'{k}' must be true or false"))),
    }
}

fn border_style(name: &str) -> Result<BorderStyle, CmdError> {
    Ok(match name {
        "none" => BorderStyle::None,
        "thin" => BorderStyle::Thin,
        "medium" => BorderStyle::Medium,
        "thick" => BorderStyle::Thick,
        "dashed" => BorderStyle::Dashed,
        "dotted" => BorderStyle::Dotted,
        "double" => BorderStyle::Double,
        other => return Err(CmdError::Param(format!("unknown border style '{other}'"))),
    })
}

fn parse_borders(b: &Json) -> Result<BorderPatch, CmdError> {
    if !b.is_object() {
        return Err(CmdError::Param("'border' must be an object".into()));
    }
    let color = opt_color(b, "color")?.flatten();
    let edge = |k: &str| -> Result<Option<Border>, CmdError> {
        match b.get(k) {
            None | Some(Json::Null) => Ok(None),
            Some(v) => {
                let name = v
                    .as_str()
                    .ok_or_else(|| CmdError::Param(format!("border '{k}' must be a style name")))?;
                let style = border_style(name)?;
                Ok(Some(Border {
                    style,
                    color: if style == BorderStyle::None {
                        None
                    } else {
                        color
                    },
                }))
            }
        }
    };
    Ok(BorderPatch {
        left: edge("left")?,
        right: edge("right")?,
        top: edge("top")?,
        bottom: edge("bottom")?,
        inside_h: edge("inside_h")?,
        inside_v: edge("inside_v")?,
    })
}

fn parse_patch(p: &Json) -> Result<Patch, CmdError> {
    let mut x = Patch::default();
    if let Some(code) = str_opt(p, "code") {
        if code.is_empty() || code.len() > 255 {
            return Err(CmdError::Param(
                "format code must be 1-255 characters".into(),
            ));
        }
        x.num_fmt = Some(code.to_string());
    }
    if let Some(h) = str_opt(p, "h_align") {
        x.h_align = Some(match h {
            "general" => HAlign::General,
            "left" => HAlign::Left,
            "center" => HAlign::Center,
            "right" => HAlign::Right,
            "fill" => HAlign::Fill,
            "justify" => HAlign::Justify,
            "center_across" => HAlign::CenterAcross,
            other => return Err(CmdError::Param(format!("unknown h_align '{other}'"))),
        });
    }
    if let Some(v) = str_opt(p, "v_align") {
        x.v_align = Some(match v {
            "top" => VAlign::Top,
            "center" | "middle" => VAlign::Center,
            "bottom" => VAlign::Bottom,
            other => return Err(CmdError::Param(format!("unknown v_align '{other}'"))),
        });
    }
    x.wrap = opt_bool(p, "wrap")?;
    if let Some(n) = f64_opt(p, "indent")? {
        if !(0.0..=250.0).contains(&n) {
            return Err(CmdError::Param("indent must be between 0 and 250".into()));
        }
        x.indent = Some(n.round() as u8);
    }
    if let Some(d) = f64_opt(p, "rotation")? {
        if !(-90.0..=90.0).contains(&d) {
            return Err(CmdError::Param(
                "rotation must be between -90 and 90".into(),
            ));
        }
        x.rotation = Some(d.round() as i16);
    }
    if let Some(name) = str_opt(p, "font").or_else(|| str_opt(p, "name")) {
        if name.is_empty() || name.len() > 31 {
            return Err(CmdError::Param("font name must be 1-31 characters".into()));
        }
        x.font_name = Some(name.to_string());
    }
    if let Some(size) = f64_opt(p, "size")? {
        if !(1.0..=409.0).contains(&size) {
            return Err(CmdError::Param(
                "font size must be between 1 and 409".into(),
            ));
        }
        x.size_tenths = Some((size * 10.0).round() as u32);
    }
    x.bold = opt_bool(p, "bold")?;
    x.italic = opt_bool(p, "italic")?;
    x.underline = match p.get("underline") {
        None | Some(Json::Null) => None,
        Some(Json::Bool(b)) => Some(u8::from(*b)),
        Some(v) => match v
            .as_str()
            .map(str::to_string)
            .or_else(|| v.as_u64().map(|n| n.to_string()))
        {
            Some(t) => Some(match t.as_str() {
                "none" | "0" => 0,
                "single" | "1" => 1,
                "double" | "2" => 2,
                other => return Err(CmdError::Param(format!("unknown underline '{other}'"))),
            }),
            None => {
                return Err(CmdError::Param(
                    "'underline' must be none|single|double".into(),
                ));
            }
        },
    };
    x.strike = opt_bool(p, "strike")?;
    x.font_color = match opt_color(p, "font_color")? {
        Some(c) => Some(c),
        None => opt_color(p, "color")?,
    };
    x.fill = opt_color(p, "fill")?;
    x.locked = opt_bool(p, "locked")?;
    x.hidden = opt_bool(p, "hidden")?;
    if let Some(b) = p.get("border")
        && !b.is_null()
    {
        x.borders = Some(parse_borders(b)?);
    }
    Ok(x)
}

/// Edge flags of a cell within the target range: bit 0 first col, 1 last col, 2 first row,
/// 3 last row.
fn position_class(a: CellAddr, r: sp_core::CellRange) -> u8 {
    u8::from(a.col == r.start.col)
        | u8::from(a.col == r.end.col) << 1
        | u8::from(a.row == r.start.row) << 2
        | u8::from(a.row == r.end.row) << 3
}

fn apply(st: &mut Style, x: &Patch, class: u8) {
    if let Some(v) = &x.num_fmt {
        st.num_fmt.clone_from(v);
    }
    if let Some(v) = x.h_align {
        st.h_align = v;
    }
    if let Some(v) = x.v_align {
        st.v_align = v;
    }
    if let Some(v) = x.wrap {
        st.wrap = v;
    }
    if let Some(v) = x.indent {
        st.indent = v;
    }
    if let Some(v) = x.rotation {
        st.rotation = v;
    }
    if let Some(v) = &x.font_name {
        st.font_name.clone_from(v);
    }
    if let Some(v) = x.size_tenths {
        st.font_size_tenths = v;
    }
    if let Some(v) = x.bold {
        st.bold = v;
    }
    if let Some(v) = x.italic {
        st.italic = v;
    }
    if let Some(v) = x.underline {
        st.underline = v;
    }
    if let Some(v) = x.strike {
        st.strike = v;
    }
    if let Some(v) = x.font_color {
        st.font_color = v;
    }
    if let Some(v) = x.fill {
        st.fill = v;
    }
    if let Some(v) = x.locked {
        st.locked = v;
    }
    if let Some(v) = x.hidden {
        st.hidden_formula = v;
    }
    if let Some(b) = x.borders {
        let pick = |outer: bool, edge: Option<Border>, inner: Option<Border>| {
            if outer { edge } else { inner }
        };
        if let Some(v) = pick(class & 1 != 0, b.left, b.inside_v) {
            st.border_left = v;
        }
        if let Some(v) = pick(class & 2 != 0, b.right, b.inside_v) {
            st.border_right = v;
        }
        if let Some(v) = pick(class & 4 != 0, b.top, b.inside_h) {
            st.border_top = v;
        }
        if let Some(v) = pick(class & 8 != 0, b.bottom, b.inside_h) {
            st.border_bottom = v;
        }
    }
}

fn format_cells(s: &mut Session, p: &Json, label: &str) -> CmdResult {
    let patch = parse_patch(p)?;
    if patch == Patch::default() {
        return Ok(json!(0));
    }
    let (sheet, r) = range(s, p, "range")?;
    let addrs: Vec<CellAddr> = if r.cell_count() <= DENSE_LIMIT {
        r.iter().collect()
    } else if patch.borders.is_some() {
        return Err(CmdError::Param("selection too large for borders".into()));
    } else {
        s.wb.sheet(sheet)
            .map(|sh| sh.cells_in(r).map(|(a, _)| a).collect())
            .unwrap_or_default()
    };
    let mut memo: std::collections::HashMap<(StyleId, u8), StyleId> =
        std::collections::HashMap::new();
    let mut updates = Vec::with_capacity(addrs.len());
    for a in addrs {
        let cur =
            s.wb.sheet(sheet)
                .and_then(|sh| sh.get(a))
                .cloned()
                .unwrap_or_default();
        // Only the border patch depends on position; skip the class otherwise.
        let class = if patch.borders.is_some() {
            position_class(a, r)
        } else {
            0
        };
        let key = (cur.style, class);
        let id = match memo.get(&key) {
            Some(id) => *id,
            None => {
                let id = s.wb.styles.derive(cur.style, |st| apply(st, &patch, class));
                memo.insert(key, id);
                id
            }
        };
        if id != cur.style {
            updates.push((a, Cell { style: id, ..cur }));
        }
    }
    let n = updates.len();
    s.transact(label, |tx, _| {
        for (a, c) in updates {
            tx.set_cell(sheet, a, Some(c));
        }
        Ok(())
    })?;
    Ok(json!(n))
}

/// Replace `what` with `with` in `text`, optionally ignoring case; `None` if no occurrence.
fn replace_text(text: &str, what: &str, with: &str, match_case: bool) -> Option<String> {
    if match_case {
        return text.contains(what).then(|| text.replace(what, with));
    }
    let lower = text.to_lowercase();
    let wl = what.to_lowercase();
    // Lowercasing can change byte lengths; fall back to an exact-case replace then.
    if lower.len() != text.len() || wl.len() != what.len() {
        return text.contains(what).then(|| text.replace(what, with));
    }
    let mut out = String::new();
    let mut i = 0;
    let mut found = false;
    while let Some(pos) = lower.get(i..).and_then(|t| t.find(&wl)) {
        found = true;
        out.push_str(text.get(i..i + pos).unwrap_or(""));
        out.push_str(with);
        i += pos + wl.len();
    }
    out.push_str(text.get(i..).unwrap_or(""));
    found.then_some(out)
}

/// Replace in the active cell (when it matches), then move to the next match, like the
/// Replace button of the Find & Replace dialog.
fn replace_current(s: &mut Session, p: &Json) -> CmdResult {
    let what = str_req(p, "what")?.to_string();
    if what.is_empty() {
        return Err(CmdError::Param("search text is empty".into()));
    }
    let with = str_opt(p, "with").unwrap_or("").to_string();
    let match_case = bool_opt(p, "match_case").unwrap_or(false);
    let (sheet, at) = (s.sheet_index(), s.sel.active);
    let old = s.wb.sheet(sheet).and_then(|sh| sh.get(at)).cloned();
    let mut replaced = false;
    if let Some(old) = old
        && let Some(new_text) = replace_text(&old.input_text(), &what, &with, match_case)
    {
        let new = super::cell_cmds::parse_input(&new_text, Some(&old), &mut s.wb.styles)?;
        s.transact("Replace", |tx, _| {
            tx.set_cell(sheet, at, new);
            Ok(())
        })?;
        replaced = true;
    }
    let find = registry()
        .get("home.editing.find")
        .ok_or_else(|| CmdError::Unknown("home.editing.find".into()))?;
    let next = (find.spec.run)(s, &json!({"what": what, "match_case": match_case})).ok();
    Ok(json!({ "replaced": replaced, "next": next }))
}

fn find_all(s: &mut Session, p: &Json) -> CmdResult {
    let what = str_req(p, "what")?;
    if what.is_empty() {
        return Err(CmdError::Param("search text is empty".into()));
    }
    let in_formulas = bool_opt(p, "formulas").unwrap_or(true);
    let match_case = bool_opt(p, "match_case").unwrap_or(false);
    let whole = bool_opt(p, "whole").unwrap_or(false);
    let needle = if match_case {
        what.to_string()
    } else {
        what.to_lowercase()
    };
    let mut hits = Vec::new();
    'sheets: for sh in &s.wb.sheets {
        let mut cells: Vec<(CellAddr, &Cell)> = sh.iter().collect();
        cells.sort_by_key(|(a, _)| (a.row, a.col));
        for (a, c) in cells {
            let hay = if in_formulas {
                c.input_text()
            } else {
                c.value.to_display()
            };
            let hay_cmp = if match_case {
                hay.clone()
            } else {
                hay.to_lowercase()
            };
            if (whole && hay_cmp == needle) || (!whole && hay_cmp.contains(&needle)) {
                let formula = matches!(c.content, Content::Formula(_)).then(|| c.input_text());
                hits.push(json!({
                    "sheet": sh.name,
                    "cell": a.to_a1(),
                    "value": c.value.to_display(),
                    "formula": formula,
                }));
                if hits.len() >= FIND_ALL_CAP {
                    break 'sheets;
                }
            }
        }
    }
    s.last_find = Some(what.to_string());
    Ok(Json::Array(hits))
}

const FORMAT_PARAMS: &str = r#"{"range"?: "A1:B2", "code"?: "0.00", "h_align"?: "general|left|center|right|fill|justify|center_across", "v_align"?: "top|center|bottom", "wrap"?: bool, "indent"?: 0, "rotation"?: 45, "font"?: "Arial", "size"?: 11, "bold"?: bool, "italic"?: bool, "underline"?: "none|single|double", "strike"?: bool, "font_color"?: "FF0000" | null, "fill"?: "FFFF00" | null, "locked"?: bool, "hidden"?: bool, "border"?: {"left|right|top|bottom|inside_h|inside_v"?: "thin|medium|thick|dashed|dotted|double|none", "color"?: "000000"}}"#;

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "home.clipboard.paste_special",
            r#"{"cell"?: "D1", "mode": "all|values|formulas|formats|transpose"}"#,
            paste_special,
        )
        .when(|s| s.clipboard.is_some()),
        CommandSpec::new("format.cells", FORMAT_PARAMS, |s, p| {
            format_cells(s, p, "Format Cells")
        }),
        CommandSpec::new(
            "home.font.dialog",
            r#"{"range"?: "A1:B2", "font"?: "Arial", "size"?: 11, "bold"?: bool, "italic"?: bool, "underline"?: "none|single|double", "strike"?: bool, "color"?: "FF0000" | null}"#,
            |s, p| format_cells(s, p, "Font"),
        ),
        CommandSpec::new(
            "edit.replace_current",
            r#"{"what": "a", "with": "b", "match_case"?: false} → {replaced, next}"#,
            replace_current,
        ),
        CommandSpec::new(
            "edit.find_all",
            r#"{"what": "text", "formulas"?: true, "match_case"?: false, "whole"?: false} → [{sheet, cell, value, formula}]"#,
            find_all,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session() -> Session {
        let mut s = Session::new();
        for (c, v) in [("A1", "apple"), ("A2", "=1+1"), ("B3", "Apple pie")] {
            s.execute("cell.set", &json!({"cell": c, "input": v}))
                .unwrap();
        }
        s
    }

    fn style_at(s: &Session, a1: &str) -> Style {
        let a = sp_core::CellRange::parse(a1).unwrap().start;
        let id =
            s.wb.sheet(0)
                .and_then(|sh| sh.get(a))
                .map_or(StyleId::DEFAULT, |c| c.style);
        s.wb.styles.get(id).clone()
    }

    #[test]
    fn format_cells_sets_everything_in_one_undo_step() {
        let mut s = session();
        let undo_before = s.history.can_undo();
        let r = s.execute(
            "format.cells",
            &json!({"range": "A1:B2", "code": "0.00", "h_align": "center", "wrap": true,
                    "bold": true, "underline": "double", "fill": "FFFF00", "locked": false,
                    "hidden": true, "size": 14, "font": "Arial", "rotation": 45,
                    "border": {"left": "thin", "right": "thin", "top": "medium", "bottom": "thin",
                               "inside_h": "dotted", "inside_v": "none", "color": "FF0000"}}),
        );
        assert_eq!(r, Ok(json!(4)));
        let a1 = style_at(&s, "A1");
        assert_eq!(a1.num_fmt, "0.00");
        assert_eq!(a1.h_align, HAlign::Center);
        assert!(a1.wrap && a1.bold && !a1.locked && a1.hidden_formula);
        assert_eq!(a1.underline, 2);
        assert_eq!(a1.font_name, "Arial");
        assert_eq!(a1.font_size_tenths, 140);
        assert_eq!(a1.fill, Some(Color(255, 255, 0)));
        assert_eq!(a1.border_top.style, BorderStyle::Medium);
        assert_eq!(a1.border_top.color, Some(Color(255, 0, 0)));
        assert_eq!(a1.border_bottom.style, BorderStyle::Dotted);
        assert_eq!(a1.border_right.style, BorderStyle::None);
        let b2 = style_at(&s, "B2");
        assert_eq!(b2.border_bottom.style, BorderStyle::Thin);
        assert_eq!(b2.border_right.style, BorderStyle::Thin);
        assert_eq!(b2.border_top.style, BorderStyle::Dotted);
        assert!(!undo_before || s.history.can_undo());
        s.execute("edit.undo", &json!({})).unwrap();
        assert!(!style_at(&s, "A1").bold);
    }

    #[test]
    fn format_cells_rejects_bad_params() {
        let mut s = session();
        for p in [
            json!({"h_align": "sideways"}),
            json!({"rotation": 200}),
            json!({"fill": "nothex"}),
            json!({"border": {"left": "wavy"}}),
            json!({"border": 3}),
            json!({"underline": "triple"}),
            json!({"bold": "yes"}),
            json!({"code": ""}),
        ] {
            assert!(s.execute("format.cells", &p).is_err(), "{p}");
        }
        assert_eq!(s.execute("format.cells", &json!({})), Ok(json!(0)));
    }

    #[test]
    fn font_dialog_sets_explicit_values() {
        let mut s = session();
        s.execute("home.font.bold", &json!({"range": "A1"}))
            .unwrap();
        s.execute(
            "home.font.dialog",
            &json!({"range": "A1", "bold": true, "color": "0000FF"}),
        )
        .unwrap();
        let st = style_at(&s, "A1");
        assert!(st.bold, "explicit true must not toggle off");
        assert_eq!(st.font_color, Some(Color(0, 0, 255)));
    }

    #[test]
    fn paste_special_modes() {
        let mut s = session();
        assert!(
            s.execute("home.clipboard.paste_special", &json!({"mode": "values"}))
                .is_err()
        );
        s.execute("home.clipboard.copy", &json!({"range": "A2"}))
            .unwrap();
        s.execute(
            "home.clipboard.paste_special",
            &json!({"mode": "values", "cell": "D1"}),
        )
        .unwrap();
        let d1 =
            s.wb.sheet(0)
                .and_then(|sh| sh.get(CellAddr::new(0, 3)))
                .unwrap();
        assert_eq!(d1.input_text(), "2");
        assert!(
            s.execute("home.clipboard.paste_special", &json!({"mode": "x"}))
                .is_err()
        );
        s.execute(
            "home.clipboard.paste_special",
            &json!({"mode": "formulas", "cell": "D2"}),
        )
        .unwrap();
        let d2 =
            s.wb.sheet(0)
                .and_then(|sh| sh.get(CellAddr::new(1, 3)))
                .unwrap();
        assert_eq!(d2.input_text(), "=1+1");
    }

    #[test]
    fn replace_current_replaces_active_then_moves_on() {
        let mut s = session();
        s.select(CellAddr::new(0, 0));
        let r = s
            .execute(
                "edit.replace_current",
                &json!({"what": "APPLE", "with": "pear"}),
            )
            .unwrap();
        assert_eq!(r["replaced"], true);
        assert_eq!(r["next"]["cell"], "B3");
        let a1 =
            s.wb.sheet(0)
                .and_then(|sh| sh.get(CellAddr::new(0, 0)))
                .unwrap();
        assert_eq!(a1.input_text(), "pear");
        assert_eq!(s.sel.active, CellAddr::new(2, 1));
        assert_eq!(replace_text("aXa", "x", "-", false).as_deref(), Some("a-a"));
        assert_eq!(replace_text("aXa", "x", "-", true), None);
    }

    #[test]
    fn find_all_lists_every_hit() {
        let mut s = session();
        let r = s
            .execute("edit.find_all", &json!({"what": "apple"}))
            .unwrap();
        let hits = r.as_array().unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0]["cell"], "A1");
        assert_eq!(hits[1]["cell"], "B3");
        let r = s
            .execute(
                "edit.find_all",
                &json!({"what": "Apple", "match_case": true}),
            )
            .unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        let r = s
            .execute("edit.find_all", &json!({"what": "apple", "whole": true}))
            .unwrap();
        assert_eq!(r.as_array().unwrap().len(), 1);
        let r = s
            .execute("edit.find_all", &json!({"what": "2", "formulas": false}))
            .unwrap();
        assert_eq!(r.as_array().unwrap()[0]["formula"], "=1+1");
        assert!(s.execute("edit.find_all", &json!({"what": ""})).is_err());
    }
}
