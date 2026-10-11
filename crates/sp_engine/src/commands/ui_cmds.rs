//! UI-shaped commands: function-library category menus, Use in Formula, editor helpers
//! (line break, F4 absolute-reference cycling), legacy array entry (Ctrl+Shift+Enter),
//! the Symbol picker and named cell styles.
//!
//! The editor helpers are pure text transforms (`{"text", "caret"}` in, the same out) so the
//! shell's edit buffer stays shell state while the behaviour stays a tested command.

use crate::commands::format_cmds::restyle;
use crate::params::{range, str_opt, str_req, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::CellAddr;
use sp_model::{Border, BorderStyle, Cell, Color, SpillArea, Style};

// ── Function library ─────────────────────────────────────────────────────────────────────

/// Function catalog categories behind each Formulas › Function Library menu.
pub fn library_categories(id: &str) -> &'static [&'static str] {
    match id {
        "formulas.library.financial" => &["Financial"],
        "formulas.library.logical" => &["Logical"],
        "formulas.library.text" => &["Text"],
        "formulas.library.datetime" => &["Date & Time"],
        "formulas.library.lookup" => &["Lookup & Reference"],
        "formulas.library.math" => &["Math & Trig"],
        "formulas.library.more" => &[
            "Statistical",
            "Engineering",
            "Information",
            "Compatibility",
            "Web & Cube",
            "Database",
        ],
        _ => &[],
    }
}

/// Implemented functions of the given categories, sorted, de-duplicated.
pub fn library_functions(categories: &[&str]) -> Vec<String> {
    let live = sp_formula::function_names();
    let mut v: Vec<String> = crate::catalog::functions()
        .iter()
        .filter(|g| categories.iter().any(|c| g.category.eq_ignore_ascii_case(c)))
        .flat_map(|g| g.names.iter().cloned())
        .filter(|n| live.contains(&n.as_str()))
        .collect();
    v.sort();
    v.dedup();
    v
}

/// `{"function"?: "PMT", "category"?: "Statistical"}`: no function → the menu (per category);
/// a function → the text to insert at the caret (`PMT(`; prefix `=` when not in a formula).
fn library(id: &'static str, p: &Json) -> CmdResult {
    let cats = library_categories(id);
    let cats: Vec<&str> = match str_opt(p, "category") {
        Some(c) => cats
            .iter()
            .copied()
            .filter(|x| x.eq_ignore_ascii_case(c))
            .collect(),
        None => cats.to_vec(),
    };
    match str_opt(p, "function") {
        None => {
            let groups: Vec<Json> = cats
                .iter()
                .map(|c| json!({ "category": c, "functions": library_functions(&[c]) }))
                .collect();
            Ok(json!({ "groups": groups }))
        }
        Some(f) => {
            let name = f.trim().to_ascii_uppercase();
            if !library_functions(&cats).contains(&name) {
                return Err(CmdError::Param(format!(
                    "'{f}' is not a function in this category"
                )));
            }
            Ok(json!({ "insert": format!("{name}(") }))
        }
    }
}

// ── Use in Formula ───────────────────────────────────────────────────────────────────────

/// Defined names visible from `sheet` (hidden built-ins skipped), sorted.
pub fn visible_names(s: &Session, sheet: usize) -> Vec<String> {
    let mut v: Vec<String> =
        s.wb.names
            .iter()
            .filter(|n| n.scope.is_none_or(|i| i == sheet))
            .filter(|n| !n.name.starts_with("_xlnm.") && !n.name.starts_with("_xl"))
            .map(|n| n.name.clone())
            .collect();
    // Table names can be used in formulas too.
    for sh in &s.wb.sheets {
        for t in &sh.tables {
            v.push(t.name.clone());
        }
    }
    v.sort_by_key(|n| n.to_lowercase());
    v.dedup();
    v
}

fn use_in_formula(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let names = visible_names(s, sheet);
    match str_opt(p, "name") {
        None => Ok(json!({ "names": names })),
        Some(n) => match names.iter().find(|x| x.eq_ignore_ascii_case(n.trim())) {
            Some(found) => Ok(json!({ "insert": found })),
            None => Err(CmdError::Param(format!("no name '{n}'"))),
        },
    }
}

// ── Editor helpers ───────────────────────────────────────────────────────────────────────

/// Text and caret (in chars, clamped) from params; caret defaults to the end.
fn text_caret(p: &Json) -> Result<(Vec<char>, usize), CmdError> {
    let text: Vec<char> = str_req(p, "text")?.chars().collect();
    if text.len() > sp_core::MAX_TEXT_LEN.saturating_mul(2) {
        return Err(CmdError::Param("text too long".into()));
    }
    let caret = u32_opt(p, "caret")?
        .map_or(text.len(), |c| c as usize)
        .min(text.len());
    Ok((text, caret))
}

fn newline(p: &Json) -> CmdResult {
    let (mut t, caret) = text_caret(p)?;
    t.insert(caret, '\n');
    Ok(json!({ "text": t.iter().collect::<String>(), "caret": caret + 1 }))
}

/// One `$?COL$?ROW` reference found in formula text (char offsets).
#[derive(Debug, Clone, Copy, PartialEq)]
struct RefSpan {
    start: usize,
    end: usize,
    col_abs: bool,
    row_abs: bool,
    col: (usize, usize),
    row: (usize, usize),
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '.'
}

/// Cell references in `t` outside string literals (not function names, not part of words).
fn find_refs(t: &[char]) -> Vec<RefSpan> {
    let mut out = Vec::new();
    let mut i = 0usize;
    let mut in_str = false;
    while i < t.len() {
        let c = t.get(i).copied().unwrap_or(' ');
        if c == '"' {
            in_str = !in_str;
            i += 1;
            continue;
        }
        let prev_word = i > 0 && t.get(i - 1).is_some_and(|p| is_word(*p) || *p == '$');
        if in_str || prev_word || !(c == '$' || c.is_ascii_alphabetic()) {
            i += 1;
            continue;
        }
        let start = i;
        let mut j = i;
        let col_abs = t.get(j) == Some(&'$');
        if col_abs {
            j += 1;
        }
        let cs = j;
        while j < t.len() && t.get(j).is_some_and(char::is_ascii_alphabetic) && j - cs < 3 {
            j += 1;
        }
        let ce = j;
        let row_abs = t.get(j) == Some(&'$');
        if row_abs {
            j += 1;
        }
        let rs = j;
        while j < t.len() && t.get(j).is_some_and(char::is_ascii_digit) && j - rs < 7 {
            j += 1;
        }
        let re = j;
        let next = t.get(j).copied();
        let ok = ce > cs
            && re > rs
            && !next.is_some_and(|n| is_word(n) || n == '(' || n == '$' || n == '!');
        if ok {
            let col: String = t.get(cs..ce).unwrap_or(&[]).iter().collect();
            let row: String = t.get(rs..re).unwrap_or(&[]).iter().collect();
            let valid = sp_core::letters_to_col(&col).is_some_and(|c| c < sp_core::MAX_COLS)
                && row
                    .parse::<u32>()
                    .is_ok_and(|r| (1..=sp_core::MAX_ROWS).contains(&r));
            if valid {
                out.push(RefSpan {
                    start,
                    end: re,
                    col_abs,
                    row_abs,
                    col: (cs, ce),
                    row: (rs, re),
                });
                i = re;
                continue;
            }
        }
        // Skip the rest of this word.
        i += 1;
        while i < t.len() && t.get(i).is_some_and(|c| is_word(*c)) {
            i += 1;
        }
    }
    out
}

fn render_ref(t: &[char], r: &RefSpan, col_abs: bool, row_abs: bool) -> String {
    let mut s = String::new();
    if col_abs {
        s.push('$');
    }
    s.extend(t.get(r.col.0..r.col.1).unwrap_or(&[]));
    if row_abs {
        s.push('$');
    }
    s.extend(t.get(r.row.0..r.row.1).unwrap_or(&[]));
    s
}

/// F4: cycle `A1 → $A$1 → A$1 → $A1 → A1` on the reference (or `A1:B2` range) at the caret.
pub fn toggle_absolute(text: &str, caret: usize) -> (String, usize) {
    let t: Vec<char> = text.chars().collect();
    let caret = caret.min(t.len());
    let refs = find_refs(&t);
    // The reference under (or just before) the caret.
    let Some(k) = refs
        .iter()
        .position(|r| caret >= r.start && caret <= r.end)
    else {
        return (text.to_string(), caret);
    };
    // A range `X:Y` toggles both ends, following the first end's state.
    let mut group = vec![k];
    if let Some(r) = refs.get(k) {
        if r.end < t.len()
            && t.get(r.end) == Some(&':')
            && let Some(n) = refs.get(k + 1)
            && n.start == r.end + 1
        {
            group.push(k + 1);
        } else if k > 0
            && let Some(p) = refs.get(k - 1)
            && p.end + 1 == r.start
            && t.get(p.end) == Some(&':')
        {
            group.insert(0, k - 1);
        }
    }
    let Some(first) = group.first().and_then(|i| refs.get(*i)) else {
        return (text.to_string(), caret);
    };
    let next = match (first.col_abs, first.row_abs) {
        (false, false) => (true, true),
        (true, true) => (false, true),
        (false, true) => (true, false),
        (true, false) => (false, false),
    };
    let mut out = String::new();
    let mut pos = 0usize;
    let mut new_caret = caret;
    for i in &group {
        let Some(r) = refs.get(*i) else { continue };
        out.extend(t.get(pos..r.start).unwrap_or(&[]));
        out.push_str(&render_ref(&t, r, next.0, next.1));
        pos = r.end;
        new_caret = out.chars().count();
    }
    out.extend(t.get(pos..).unwrap_or(&[]));
    (out, new_caret)
}

fn toggle_absolute_cmd(p: &Json) -> CmdResult {
    let (t, caret) = text_caret(p)?;
    let text: String = t.iter().collect();
    let (out, c) = toggle_absolute(&text, caret);
    Ok(json!({ "text": out, "caret": c }))
}

// ── Legacy array formula ─────────────────────────────────────────────────────────────────

fn array_enter(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let input = str_req(p, "formula")?.trim().to_string();
    let src = input.strip_prefix('=').unwrap_or(&input).trim().to_string();
    if src.is_empty() {
        return Err(CmdError::Param("an array formula needs a formula".into()));
    }
    if r.cell_count() > 1_000_000 {
        return Err(CmdError::Param("array area too large".into()));
    }
    let e = sp_formula::parse(&src)
        .map_err(|e| CmdError::Param(format!("There's a problem with this formula: {e}")))?;
    let Some(sh) = s.wb.sheet(sheet) else {
        return Err(CmdError::Param("no such sheet".into()));
    };
    // Other arrays may not be cut in part.
    for a in &sh.spills {
        if a.fixed && a.range.intersect(&r).is_some() && a.range.intersect(&r) != Some(a.range) {
            return Err(CmdError::Failed("You can't change part of an array.".into()));
        }
    }
    if sh.merges.iter().any(|m| m.intersect(&r).is_some()) {
        return Err(CmdError::Failed(
            "Array formulas aren't valid in merged cells.".into(),
        ));
    }
    let style =
        sh.get(r.start)
            .map_or(sp_model::StyleId::DEFAULT, |c| c.style);
    let formula = sp_formula::to_formula(&e);
    s.transact("Array Formula", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.spills.retain(|a| !r.contains(a.range.start));
            let others: Vec<CellAddr> = sh
                .cells_in(r)
                .map(|(a, _)| a)
                .filter(|a| *a != r.start)
                .collect();
            for a in others {
                let st = sh.get(a).map(|c| c.style).unwrap_or_default();
                if st == sp_model::StyleId::DEFAULT {
                    sh.remove(a);
                } else {
                    sh.set(
                        a,
                        Cell {
                            style: st,
                            ..Cell::default()
                        },
                    );
                }
            }
            sh.set(
                r.start,
                Cell {
                    style,
                    ..Cell::formula(&formula)
                },
            );
            sh.spills.push(SpillArea {
                range: r,
                fixed: true,
                blocked: false,
            });
        }
        Ok(())
    })?;
    s.select_range(r, r.start);
    Ok(json!({ "range": r.to_a1(), "formula": format!("={formula}") }))
}

// ── Symbols ──────────────────────────────────────────────────────────────────────────────

/// Symbol picker sets (our own selection of Unicode characters).
pub const SYMBOL_SETS: [(&str, &str); 8] = [
    ("Currency", "$¢£¤¥€₹₩₽₺₪₫₱฿"),
    ("Math", "±×÷≠≈≤≥∞√∑∏∫∂∆∇∈∉∩∪⊂⊃∧∨¬∀∃°′″‰"),
    ("Arrows", "←↑→↓↔↕⇐⇑⇒⇓⇔↖↗↘↙↺↻"),
    ("Greek", "αβγδεζηθικλμνξοπρστυφχψωΓΔΘΛΞΠΣΦΨΩ"),
    ("Punctuation", "–—‘’“”‚„†‡•…‹›«»¡¿§¶©®™"),
    ("Fractions", "½⅓⅔¼¾⅕⅖⅗⅘⅙⅚⅛⅜⅝⅞¹²³⁴⁵⁶⁷⁸⁹⁰"),
    ("Shapes", "■□▪▫▲△▶▷▼▽◀◁◆◇○●◎★☆✓✔✗✘"),
    ("Latin", "ÀÁÂÃÄÅÆÇÈÉÊËÌÍÎÏÑÒÓÔÕÖØÙÚÛÜÝßàáâãäåæçèéêëìíîïñòóôõöøùúûüýÿ"),
];

fn symbol(s: &mut Session, p: &Json) -> CmdResult {
    let Some(sym) = str_opt(p, "symbol") else {
        let sets: Vec<Json> = SYMBOL_SETS
            .iter()
            .map(|(n, chars)| json!({ "set": n, "symbols": chars.chars().map(String::from).collect::<Vec<_>>() }))
            .collect();
        return Ok(json!({ "sets": sets }));
    };
    if sym.is_empty() || sym.chars().count() > 8 {
        return Err(CmdError::Param("'symbol' must be 1-8 characters".into()));
    }
    let (sheet, at) = crate::params::cell(s, p, "cell")?;
    let old = s.wb.sheet(sheet).and_then(|sh| sh.get(at)).cloned();
    let append = p.get("append").and_then(Json::as_bool).unwrap_or(false);
    let text = match (&old, append) {
        (Some(c), true) if c.formula_src().is_none() => format!("{}{sym}", c.value.to_display()),
        _ => sym.to_string(),
    };
    let style = old.as_ref().map(|c| c.style).unwrap_or_default();
    s.transact("Insert Symbol", |tx, _| {
        tx.set_cell(
            sheet,
            at,
            Some(Cell {
                style,
                ..Cell::constant(sp_core::Value::text(text.as_str()))
            }),
        );
        Ok(())
    })?;
    Ok(json!({ "cell": at.to_a1(), "insert": sym }))
}

// ── Cell styles ──────────────────────────────────────────────────────────────────────────

/// Named cell styles, grouped as the gallery shows them.
pub const CELL_STYLE_GROUPS: [(&str, &[&str]); 5] = [
    ("Good, Bad and Neutral", &["Normal", "Good", "Bad", "Neutral"]),
    (
        "Data and Model",
        &[
            "Calculation",
            "Check Cell",
            "Explanatory Text",
            "Input",
            "Linked Cell",
            "Note",
            "Output",
            "Warning Text",
        ],
    ),
    (
        "Titles and Headings",
        &[
            "Heading 1",
            "Heading 2",
            "Heading 3",
            "Heading 4",
            "Title",
            "Total",
        ],
    ),
    (
        "Themed Cell Styles",
        &[
            "Accent1", "Accent2", "Accent3", "Accent4", "Accent5", "Accent6",
        ],
    ),
    (
        "Number Format",
        &["Comma", "Comma [0]", "Currency", "Currency [0]", "Percent"],
    ),
];

const ACCENTS: [Color; 6] = [
    Color(52, 101, 164),
    Color(214, 112, 40),
    Color(118, 118, 118),
    Color(206, 160, 20),
    Color(70, 130, 200),
    Color(90, 150, 70),
];

fn line(style: BorderStyle, color: Color) -> Border {
    Border {
        style,
        color: Some(color),
    }
}

/// Apply named style `name` to `st` (font/fill/border/format parts the style defines; the
/// rest of the cell's formatting stays). `None` for an unknown name.
pub fn apply_cell_style(name: &str, st: &mut Style) -> Option<()> {
    let n = name.trim().to_ascii_lowercase();
    let heading = Color(40, 60, 100);
    match n.as_str() {
        "normal" => {
            let keep = (st.locked, st.hidden_formula);
            *st = Style::default();
            (st.locked, st.hidden_formula) = keep;
        }
        "good" => {
            st.fill = Some(Color(214, 238, 218));
            st.font_color = Some(Color(28, 98, 44));
        }
        "bad" => {
            st.fill = Some(Color(250, 214, 216));
            st.font_color = Some(Color(150, 24, 32));
        }
        "neutral" => {
            st.fill = Some(Color(255, 238, 190));
            st.font_color = Some(Color(140, 92, 0));
        }
        "calculation" => {
            st.fill = Some(Color(240, 240, 240));
            st.font_color = Some(Color(214, 112, 40));
            st.bold = true;
            let b = line(BorderStyle::Thin, Color(128, 128, 128));
            (st.border_left, st.border_right, st.border_top, st.border_bottom) = (b, b, b, b);
        }
        "check cell" => {
            st.fill = Some(Color(160, 160, 160));
            st.font_color = Some(Color::WHITE);
            st.bold = true;
            let b = line(BorderStyle::Double, Color(64, 64, 64));
            (st.border_left, st.border_right, st.border_top, st.border_bottom) = (b, b, b, b);
        }
        "explanatory text" => {
            st.italic = true;
            st.font_color = Some(Color(120, 120, 120));
        }
        "input" => {
            st.fill = Some(Color(255, 210, 160));
            st.font_color = Some(Color(60, 60, 120));
            let b = line(BorderStyle::Thin, Color(128, 128, 128));
            (st.border_left, st.border_right, st.border_top, st.border_bottom) = (b, b, b, b);
        }
        "linked cell" => {
            st.font_color = Some(Color(214, 112, 40));
            st.border_bottom = line(BorderStyle::Double, Color(214, 112, 40));
        }
        "note" => {
            st.fill = Some(Color(255, 252, 206));
            let b = line(BorderStyle::Thin, Color(180, 180, 180));
            (st.border_left, st.border_right, st.border_top, st.border_bottom) = (b, b, b, b);
        }
        "output" => {
            st.fill = Some(Color(242, 242, 242));
            st.font_color = Some(Color(64, 64, 64));
            st.bold = true;
            let b = line(BorderStyle::Thin, Color(64, 64, 64));
            (st.border_left, st.border_right, st.border_top, st.border_bottom) = (b, b, b, b);
        }
        "warning text" => st.font_color = Some(Color(200, 0, 0)),
        "heading 1" => {
            st.bold = true;
            st.font_size_tenths = 150;
            st.font_color = Some(heading);
            st.border_bottom = line(BorderStyle::Thick, ACCENTS[0]);
        }
        "heading 2" => {
            st.bold = true;
            st.font_size_tenths = 130;
            st.font_color = Some(heading);
            st.border_bottom = line(BorderStyle::Thick, Color(160, 186, 222));
        }
        "heading 3" => {
            st.bold = true;
            st.font_size_tenths = 110;
            st.font_color = Some(heading);
            st.border_bottom = line(BorderStyle::Medium, Color(160, 186, 222));
        }
        "heading 4" => {
            st.bold = true;
            st.font_color = Some(heading);
        }
        "title" => {
            st.font_size_tenths = 180;
            st.font_color = Some(heading);
        }
        "total" => {
            st.bold = true;
            st.border_top = line(BorderStyle::Thin, ACCENTS[0]);
            st.border_bottom = line(BorderStyle::Double, ACCENTS[0]);
        }
        "comma" => st.num_fmt = "#,##0.00".into(),
        "comma [0]" => st.num_fmt = "#,##0".into(),
        "currency" => st.num_fmt = "$#,##0.00".into(),
        "currency [0]" => st.num_fmt = "$#,##0".into(),
        "percent" => st.num_fmt = "0%".into(),
        other => {
            let i: usize = other.strip_prefix("accent")?.parse().ok()?;
            let c = *ACCENTS.get(i.checked_sub(1)?)?;
            st.fill = Some(c);
            st.font_color = Some(Color::WHITE);
        }
    }
    Some(())
}

/// The full style a named style gives a default cell (for gallery previews).
pub fn cell_style_preview(name: &str) -> Option<Style> {
    let mut st = Style::default();
    apply_cell_style(name, &mut st)?;
    Some(st)
}

fn cell_styles(s: &mut Session, p: &Json) -> CmdResult {
    let Some(name) = str_opt(p, "style") else {
        let groups: Vec<Json> = CELL_STYLE_GROUPS
            .iter()
            .map(|(g, names)| json!({ "group": g, "styles": names }))
            .collect();
        return Ok(json!({ "groups": groups }));
    };
    if cell_style_preview(name).is_none() {
        return Err(CmdError::Param(format!("unknown cell style '{name}'")));
    }
    let name = name.to_string();
    let label = format!("Style {name}");
    restyle(s, p, &label, &|st| {
        let _ = apply_cell_style(&name, st);
    })
}

pub fn specs() -> Vec<CommandSpec> {
    const LIB: &str = r#"{"function"?: "PMT", "category"?: "Statistical"} → no function: {"groups": [{"category", "functions"}]}; a function: {"insert": "PMT("} (text the editor inserts at the caret)"#;
    vec![
        CommandSpec::new("formulas.library.financial", LIB, |_, p| {
            library("formulas.library.financial", p)
        }),
        CommandSpec::new("formulas.library.logical", LIB, |_, p| {
            library("formulas.library.logical", p)
        }),
        CommandSpec::new("formulas.library.text", LIB, |_, p| {
            library("formulas.library.text", p)
        }),
        CommandSpec::new("formulas.library.datetime", LIB, |_, p| {
            library("formulas.library.datetime", p)
        }),
        CommandSpec::new("formulas.library.lookup", LIB, |_, p| {
            library("formulas.library.lookup", p)
        }),
        CommandSpec::new("formulas.library.math", LIB, |_, p| {
            library("formulas.library.math", p)
        }),
        CommandSpec::new("formulas.library.more", LIB, |_, p| {
            library("formulas.library.more", p)
        }),
        CommandSpec::new(
            "formulas.use_in_formula",
            r#"{"name"?: "Sales"} → no name: {"names": [...]}; a name: {"insert": "Sales"}"#,
            use_in_formula,
        ),
        CommandSpec::new(
            "cell.newline",
            r#"{"text": "ab", "caret"?: 1} → {"text": "a\nb", "caret": 2} (editor line break)"#,
            |_, p| newline(p),
        ),
        CommandSpec::new(
            "cell.toggle_absolute",
            r#"{"text": "=A1+B2", "caret"?: 3} → {"text": "=$A$1+B2", "caret": 5} (cycles A1 → $A$1 → A$1 → $A1 on the reference at the caret)"#,
            |_, p| toggle_absolute_cmd(p),
        ),
        CommandSpec::new(
            "cell.array_enter",
            r#"{"range"?: "C1:C3" (default selection), "formula": "=A1:A3*B1:B3"} → legacy (fixed-size) array formula over the range"#,
            array_enter,
        ),
        CommandSpec::new(
            "insert.symbol",
            r#"{"symbol"?: "€", "cell"?: "A1", "append"?: bool} → no symbol: {"sets": [{"set", "symbols"}]}; a symbol: enters it in the cell"#,
            symbol,
        ),
        CommandSpec::new(
            "home.styles.cell_styles",
            r#"{"style"?: "Good|Bad|Neutral|Normal|Heading 1..4|Title|Total|Note|Input|Output|Calculation|Check Cell|Accent1..6|Comma|Currency|Percent…", "range"?: "A1:B2"} → no style: {"groups": [{"group", "styles"}]}"#,
            cell_styles,
        ),
    ]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    fn run(s: &mut Session, id: &str, p: Json) -> Json {
        s.execute(id, &p).unwrap()
    }

    #[test]
    fn library_lists_and_inserts() {
        let mut s = Session::new();
        let v = run(&mut s, "formulas.library.financial", json!({}));
        let fns = v["groups"][0]["functions"].as_array().unwrap();
        assert!(fns.iter().any(|f| f == "PMT"));
        let v = run(&mut s, "formulas.library.financial", json!({"function": "pmt"}));
        assert_eq!(v["insert"], "PMT(");
        assert!(
            s.execute("formulas.library.financial", &json!({"function": "SUM"}))
                .is_err()
        );
        let v = run(&mut s, "formulas.library.more", json!({}));
        assert!(v["groups"].as_array().unwrap().len() >= 4);
        for id in [
            "formulas.library.logical",
            "formulas.library.text",
            "formulas.library.datetime",
            "formulas.library.lookup",
            "formulas.library.math",
        ] {
            let v = run(&mut s, id, json!({}));
            assert!(!v["groups"][0]["functions"].as_array().unwrap().is_empty(), "{id}");
        }
    }

    #[test]
    fn use_in_formula_lists_names() {
        let mut s = Session::new();
        run(
            &mut s,
            "formulas.define_name",
            json!({"name": "Rate", "refers_to": "=Sheet1!$A$1"}),
        );
        let v = run(&mut s, "formulas.use_in_formula", json!({}));
        assert_eq!(v["names"], json!(["Rate"]));
        let v = run(&mut s, "formulas.use_in_formula", json!({"name": "rate"}));
        assert_eq!(v["insert"], "Rate");
        assert!(
            s.execute("formulas.use_in_formula", &json!({"name": "nope"}))
                .is_err()
        );
    }

    #[test]
    fn newline_inserts_at_caret() {
        let mut s = Session::new();
        let v = run(&mut s, "cell.newline", json!({"text": "ab", "caret": 1}));
        assert_eq!(v, json!({"text": "a\nb", "caret": 2}));
        let v = run(&mut s, "cell.newline", json!({"text": "ab", "caret": 99}));
        assert_eq!(v["text"], "ab\n");
    }

    #[test]
    fn f4_cycles_reference_at_caret() {
        let cyc = |t: &str, c: usize| toggle_absolute(t, c);
        assert_eq!(cyc("=A1+B2", 3), ("=$A$1+B2".into(), 5));
        assert_eq!(cyc("=$A$1+B2", 5), ("=A$1+B2".into(), 4));
        assert_eq!(cyc("=A$1+B2", 4), ("=$A1+B2".into(), 4));
        assert_eq!(cyc("=$A1+B2", 4), ("=A1+B2".into(), 3));
        // Caret on the second reference.
        assert_eq!(cyc("=A1+B2", 6), ("=A1+$B$2".into(), 8));
        // Ranges toggle both ends.
        assert_eq!(cyc("=SUM(A1:B3)", 7), ("=SUM($A$1:$B$3)".into(), 14));
        // Function names and strings are not references.
        assert_eq!(cyc("=LOG10(5)", 4), ("=LOG10(5)".into(), 4));
        assert_eq!(cyc("=\"A1\"", 3), ("=\"A1\"".into(), 3));
        // Sheet-qualified.
        assert_eq!(cyc("=Sheet2!C4", 10), ("=Sheet2!$C$4".into(), 12));
        // Hostile caret.
        assert_eq!(cyc("", 50), (String::new(), 0));
        let mut s = Session::new();
        let v = run(
            &mut s,
            "cell.toggle_absolute",
            json!({"text": "=A1", "caret": 3}),
        );
        assert_eq!(v, json!({"text": "=$A$1", "caret": 5}));
    }

    #[test]
    fn array_enter_fills_fixed_area_and_undoes() {
        let mut s = Session::new();
        for (c, v) in [("A1", "1"), ("A2", "2"), ("A3", "3"), ("C2", "x")] {
            run(&mut s, "cell.set", json!({"cell": c, "input": v}));
        }
        run(
            &mut s,
            "cell.array_enter",
            json!({"range": "C1:C3", "formula": "=A1:A3*10"}),
        );
        let sh = &s.wb.sheets[0];
        assert_eq!(sh.value(CellAddr::new(2, 2)).to_display(), "30");
        assert!(sh.spill_at_anchor(CellAddr::new(0, 2)).unwrap().fixed);
        run(&mut s, "edit.undo", json!({}));
        assert_eq!(s.wb.sheets[0].value(CellAddr::new(1, 2)).to_display(), "x");
        assert!(s.wb.sheets[0].spills.is_empty());
        assert!(
            s.execute("cell.array_enter", &json!({"formula": "=("}))
                .is_err()
        );
        assert!(
            s.execute("cell.array_enter", &json!({"formula": ""}))
                .is_err()
        );
    }

    #[test]
    fn symbol_and_cell_styles() {
        let mut s = Session::new();
        let v = run(&mut s, "insert.symbol", json!({}));
        assert!(v["sets"].as_array().unwrap().len() >= 5);
        run(&mut s, "insert.symbol", json!({"symbol": "€", "cell": "B2"}));
        run(
            &mut s,
            "insert.symbol",
            json!({"symbol": "±", "cell": "B2", "append": true}),
        );
        assert_eq!(s.wb.sheets[0].value(CellAddr::new(1, 1)).to_display(), "€±");
        let v = run(&mut s, "home.styles.cell_styles", json!({}));
        assert_eq!(v["groups"].as_array().unwrap().len(), CELL_STYLE_GROUPS.len());
        run(
            &mut s,
            "home.styles.cell_styles",
            json!({"style": "Good", "range": "B2"}),
        );
        let st = s.wb.styles.get(s.wb.sheets[0].get(CellAddr::new(1, 1)).unwrap().style);
        assert!(st.fill.is_some());
        run(
            &mut s,
            "home.styles.cell_styles",
            json!({"style": "Normal", "range": "B2"}),
        );
        let st = s.wb.styles.get(s.wb.sheets[0].get(CellAddr::new(1, 1)).unwrap().style);
        assert!(st.fill.is_none());
        for (_, names) in CELL_STYLE_GROUPS {
            for n in names {
                assert!(cell_style_preview(n).is_some(), "{n}");
            }
        }
        assert!(
            s.execute("home.styles.cell_styles", &json!({"style": "Bogus"}))
                .is_err()
        );
    }
}
