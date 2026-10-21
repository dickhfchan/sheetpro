//! Cell formatting: font, alignment, number formats, borders, merges (`home.font.*`,
//! `home.align.*`, `home.number.*`).

use crate::params::{f64_req, range, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::{CellAddr, CellRange};
use sp_model::{Border, BorderStyle, Cell, Color, HAlign, Style, StyleId, VAlign};

/// Cap on cells restyled one by one; larger selections restyle only stored cells.
const DENSE_RESTYLE_LIMIT: u64 = 200_000;

/// Apply `f` to the style of every cell in the selection (or `range` param).
pub fn restyle(s: &mut Session, p: &Json, label: &str, f: &dyn Fn(&mut Style)) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let addrs: Vec<CellAddr> = if r.cell_count() <= DENSE_RESTYLE_LIMIT {
        r.iter().collect()
    } else {
        s.wb.sheet(sheet)
            .map(|sh| sh.cells_in(r).map(|(a, _)| a).collect())
            .unwrap_or_default()
    };
    let mut updates = Vec::with_capacity(addrs.len());
    let mut memo: std::collections::HashMap<StyleId, StyleId> = std::collections::HashMap::new();
    for a in addrs {
        let cur =
            s.wb.sheet(sheet)
                .and_then(|sh| sh.get(a))
                .cloned()
                .unwrap_or_default();
        let new_style = match memo.get(&cur.style) {
            Some(id) => *id,
            None => {
                let id = s.wb.styles.derive(cur.style, f);
                memo.insert(cur.style, id);
                id
            }
        };
        if new_style != cur.style {
            updates.push((
                a,
                Cell {
                    style: new_style,
                    ..cur
                },
            ));
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

fn active_style(s: &Session) -> Style {
    let c =
        s.wb.sheet(s.sheet_index())
            .and_then(|sh| sh.get(s.sel.active));
    s.wb.styles
        .get(c.map_or(StyleId::DEFAULT, |c| c.style))
        .clone()
}

fn toggle(
    s: &mut Session,
    p: &Json,
    label: &str,
    get: fn(&Style) -> bool,
    set: fn(&mut Style, bool),
) -> CmdResult {
    let on = !get(&active_style(s));
    restyle(s, p, label, &move |st| set(st, on))
}

fn color_param(p: &Json) -> Result<Option<Color>, CmdError> {
    match p.get("color") {
        None | Some(Json::Null) => Ok(None),
        Some(v) => v
            .as_str()
            .and_then(Color::from_hex)
            .map(Some)
            .ok_or_else(|| CmdError::Param("color must be hex like \"#FF0000\" or null".into())),
    }
}

const FONT_SIZES: [f64; 16] = [
    8.0, 9.0, 10.0, 11.0, 12.0, 14.0, 16.0, 18.0, 20.0, 22.0, 24.0, 26.0, 28.0, 36.0, 48.0, 72.0,
];

/// The target's built-in currency (Ctrl+Shift+$), comma and accounting formats (en-US,
/// ECMA-376 built-in ids 7, 43 and 44), exactly as it stores them.
const CURRENCY: &str = "\"$\"#,##0.00_);(\"$\"#,##0.00)";
const COMMA: &str = "_(* #,##0.00_);_(* \\(#,##0.00\\);_(* \"-\"??_);_(@_)";
const ACCOUNTING: &str = "_(\"$\"* #,##0.00_);_(\"$\"* \\(#,##0.00\\);_(\"$\"* \"-\"??_);_(@_)";

fn decimals_of(fmt: &str) -> usize {
    let first = fmt.split(';').next().unwrap_or("");
    first
        .split_once('.')
        .map_or(0, |(_, d)| d.chars().take_while(|c| *c == '0').count())
}

/// Decimals the active cell shows: its format's, or under General those of the value as
/// General displays it (2.25 shows two), so Increase/Decrease Decimal start from what the
/// user sees.
fn shown_decimals(s: &Session) -> usize {
    let st = active_style(s);
    if !st.num_fmt.eq_ignore_ascii_case("general") {
        return decimals_of(&st.num_fmt);
    }
    let v =
        s.wb.sheet(s.sheet_index())
            .and_then(|sh| sh.get(s.sel.active))
            .map(|c| c.value.clone());
    match v {
        Some(v @ sp_core::Value::Number(_)) => {
            let text = sp_core::numfmt::format_value(&v, "General").text;
            // Scientific display (1E+20) counts the mantissa's decimals.
            let mantissa = text.split(['E', 'e']).next().unwrap_or("");
            mantissa.split_once('.').map_or(0, |(_, d)| {
                d.chars().take_while(char::is_ascii_digit).count()
            })
        }
        _ => 0,
    }
}

fn with_decimals(fmt: &str, n: usize) -> String {
    let base = if fmt.eq_ignore_ascii_case("general") {
        "0"
    } else {
        fmt
    };
    base.split(';')
        .map(|sec| {
            // Sections with no digit placeholder (`_(@_)`, `_(* "-"??_)`) stay as they are.
            if !sec.contains(['0', '#']) {
                return sec.to_string();
            }
            let (int, rest) = match sec.split_once('.') {
                Some((i, r)) => (i.to_string(), r.trim_start_matches('0').to_string()),
                None => {
                    // Split before any trailing literal/percent.
                    let idx = sec.rfind(['0', '#']).map_or(sec.len(), |i| i + 1);
                    (
                        sec.get(..idx).unwrap_or(sec).to_string(),
                        sec.get(idx..).unwrap_or("").to_string(),
                    )
                }
            };
            if n == 0 {
                format!("{int}{rest}")
            } else {
                format!("{int}.{}{rest}", "0".repeat(n))
            }
        })
        .collect::<Vec<_>>()
        .join(";")
}

fn set_borders(
    s: &mut Session,
    p: &Json,
    label: &str,
    outline: bool,
    style: BorderStyle,
) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    if r.cell_count() > DENSE_RESTYLE_LIMIT {
        return Err(CmdError::Param("selection too large for borders".into()));
    }
    let b = Border { style, color: None };
    let mut updates = Vec::new();
    for a in r.iter() {
        let cur =
            s.wb.sheet(sheet)
                .and_then(|sh| sh.get(a))
                .cloned()
                .unwrap_or_default();
        let id = s.wb.styles.derive(cur.style, |st| {
            if !outline || a.col == r.start.col {
                st.border_left = b;
            }
            if !outline || a.col == r.end.col {
                st.border_right = b;
            }
            if !outline || a.row == r.start.row {
                st.border_top = b;
            }
            if !outline || a.row == r.end.row {
                st.border_bottom = b;
            }
        });
        updates.push((a, Cell { style: id, ..cur }));
    }
    s.transact(label, |tx, _| {
        for (a, c) in updates {
            tx.set_cell(sheet, a, Some(c));
        }
        Ok(())
    })?;
    Ok(Json::Null)
}

fn merge(s: &mut Session, p: &Json, center: bool, across: bool) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    if r.is_single() {
        return Ok(Json::Null);
    }
    if across && r.rows() > 100_000 {
        return Err(CmdError::Param(
            "Merge Across is limited to 100,000 rows".into(),
        ));
    }
    let pieces: Vec<CellRange> = if across {
        (r.start.row..=r.end.row)
            .map(|row| {
                CellRange::new(
                    CellAddr::new(row, r.start.col),
                    CellAddr::new(row, r.end.col),
                )
            })
            .filter(|x| !x.is_single())
            .collect()
    } else {
        vec![r]
    };
    let center_style = if center {
        let cur =
            s.wb.sheet(sheet)
                .and_then(|sh| sh.get(r.start))
                .map_or(StyleId::DEFAULT, |c| c.style);
        Some(s.wb.styles.derive(cur, |st| st.h_align = HAlign::Center))
    } else {
        None
    };
    s.transact(
        if center {
            "Merge & Center"
        } else {
            "Merge Cells"
        },
        |tx, _| {
            tx.touch_sheet(sheet);
            let sh = tx
                .wb
                .sheet_mut(sheet)
                .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
            // Existing merges overlapping the selection are replaced (one pass, not per piece).
            sh.merges.retain(|m| m.intersect(&r).is_none());
            for piece in &pieces {
                // Merging keeps only the upper-left value.
                let others: Vec<CellAddr> = sh
                    .cells_in(*piece)
                    .map(|(a, _)| a)
                    .filter(|a| *a != piece.start)
                    .collect();
                for a in others {
                    sh.remove(a);
                }
                sh.merges.push(*piece);
                if let Some(cs) = center_style {
                    sh.entry(piece.start).style = cs;
                }
            }
            Ok(())
        },
    )?;
    Ok(json!(pieces.len()))
}

fn unmerge(s: &mut Session, p: &Json) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    s.transact("Unmerge Cells", |tx, _| {
        tx.touch_sheet(sheet);
        if let Some(sh) = tx.wb.sheet_mut(sheet) {
            sh.merges.retain(|m| m.intersect(&r).is_none());
        }
        Ok(())
    })?;
    Ok(Json::Null)
}

pub fn specs() -> Vec<CommandSpec> {
    let r = r#"{"range"?: "A1:B2"}"#;
    vec![
        CommandSpec::new("home.font.bold", r, |s, p| {
            toggle(s, p, "Bold", |st| st.bold, |st, v| st.bold = v)
        }),
        CommandSpec::new("home.font.italic", r, |s, p| {
            toggle(s, p, "Italic", |st| st.italic, |st, v| st.italic = v)
        }),
        CommandSpec::new("home.font.underline", r, |s, p| {
            toggle(
                s,
                p,
                "Underline",
                |st| st.underline == 1,
                |st, v| st.underline = u8::from(v),
            )
        }),
        CommandSpec::new("home.font.double_underline", r, |s, p| {
            toggle(
                s,
                p,
                "Double Underline",
                |st| st.underline == 2,
                |st, v| st.underline = if v { 2 } else { 0 },
            )
        }),
        CommandSpec::new("home.font.strikethrough", r, |s, p| {
            toggle(s, p, "Strikethrough", |st| st.strike, |st, v| st.strike = v)
        }),
        CommandSpec::new(
            "home.font.family",
            r#"{"range"?: "A1", "name": "Arial"}"#,
            |s, p| {
                let name = str_req(p, "name")?.to_string();
                if name.is_empty() || name.len() > 31 {
                    return Err(CmdError::Param("font name must be 1-31 characters".into()));
                }
                restyle(s, p, "Font", &move |st| st.font_name = name.clone())
            },
        ),
        CommandSpec::new(
            "home.font.size",
            r#"{"range"?: "A1", "size": 14}"#,
            |s, p| {
                let size = f64_req(p, "size")?;
                if !(1.0..=409.0).contains(&size) {
                    return Err(CmdError::Param(
                        "font size must be between 1 and 409".into(),
                    ));
                }
                restyle(s, p, "Font Size", &move |st| {
                    st.font_size_tenths = (size * 10.0).round() as u32
                })
            },
        ),
        CommandSpec::new("home.font.grow", r, |s, p| {
            let cur = active_style(s).font_size();
            let next = FONT_SIZES
                .iter()
                .copied()
                .find(|x| *x > cur)
                .unwrap_or(cur.max(72.0));
            restyle(s, p, "Increase Font Size", &move |st| {
                st.font_size_tenths = (next * 10.0) as u32
            })
        }),
        CommandSpec::new("home.font.shrink", r, |s, p| {
            let cur = active_style(s).font_size();
            let next = FONT_SIZES
                .iter()
                .rev()
                .copied()
                .find(|x| *x < cur)
                .unwrap_or(1.0f64.max(cur.min(8.0)));
            restyle(s, p, "Decrease Font Size", &move |st| {
                st.font_size_tenths = (next * 10.0) as u32
            })
        }),
        CommandSpec::new(
            "home.font.fill_color",
            r#"{"range"?: "A1", "color": "FFFF00" | null}"#,
            |s, p| {
                let c = color_param(p)?;
                restyle(s, p, "Fill Color", &move |st| st.fill = c)
            },
        ),
        CommandSpec::new(
            "home.font.font_color",
            r#"{"range"?: "A1", "color": "FF0000" | null}"#,
            |s, p| {
                let c = color_param(p)?;
                restyle(s, p, "Font Color", &move |st| st.font_color = c)
            },
        ),
        CommandSpec::new(
            "home.font.borders",
            r#"{"range"?: "A1:B2", "style"?: "thin|medium|thick|double|dashed|dotted|none"}"#,
            |s, p| {
                let style = match crate::params::str_opt(p, "style").unwrap_or("thin") {
                    "thin" => BorderStyle::Thin,
                    "medium" => BorderStyle::Medium,
                    "thick" => BorderStyle::Thick,
                    "double" => BorderStyle::Double,
                    "dashed" => BorderStyle::Dashed,
                    "dotted" => BorderStyle::Dotted,
                    "none" => BorderStyle::None,
                    other => {
                        return Err(CmdError::Param(format!("unknown border style '{other}'")));
                    }
                };
                set_borders(s, p, "Borders", false, style)
            },
        ),
        CommandSpec::new("home.font.border_outline", r, |s, p| {
            set_borders(s, p, "Outline Border", true, BorderStyle::Thin)
        }),
        CommandSpec::new("home.font.border_none", r, |s, p| {
            set_borders(s, p, "No Border", false, BorderStyle::None)
        }),
        CommandSpec::new("home.align.left", r, |s, p| {
            restyle(s, p, "Align Left", &|st| st.h_align = HAlign::Left)
        }),
        CommandSpec::new("home.align.center", r, |s, p| {
            restyle(s, p, "Center", &|st| st.h_align = HAlign::Center)
        }),
        CommandSpec::new("home.align.right", r, |s, p| {
            restyle(s, p, "Align Right", &|st| st.h_align = HAlign::Right)
        }),
        CommandSpec::new("home.align.top", r, |s, p| {
            restyle(s, p, "Top Align", &|st| st.v_align = VAlign::Top)
        }),
        CommandSpec::new("home.align.middle", r, |s, p| {
            restyle(s, p, "Middle Align", &|st| st.v_align = VAlign::Center)
        }),
        CommandSpec::new("home.align.bottom", r, |s, p| {
            restyle(s, p, "Bottom Align", &|st| st.v_align = VAlign::Bottom)
        }),
        CommandSpec::new("home.align.wrap_text", r, |s, p| {
            toggle(s, p, "Wrap Text", |st| st.wrap, |st, v| st.wrap = v)
        }),
        CommandSpec::new("home.align.indent_increase", r, |s, p| {
            restyle(s, p, "Increase Indent", &|st| {
                st.indent = st.indent.saturating_add(1).min(250);
                // Indents apply to left- or right-aligned text: anything else turns left.
                if !matches!(st.h_align, HAlign::Left | HAlign::Right) {
                    st.h_align = HAlign::Left;
                }
            })
        }),
        CommandSpec::new("home.align.indent_decrease", r, |s, p| {
            restyle(s, p, "Decrease Indent", &|st| {
                st.indent = st.indent.saturating_sub(1)
            })
        }),
        CommandSpec::new(
            "home.align.orientation",
            r#"{"range"?: "A1", "degrees": 45}"#,
            |s, p| {
                let d = f64_req(p, "degrees")?;
                if !(-90.0..=90.0).contains(&d) {
                    return Err(CmdError::Param("degrees must be between -90 and 90".into()));
                }
                restyle(s, p, "Orientation", &move |st| {
                    st.rotation = d.round() as i16
                })
            },
        ),
        CommandSpec::new("home.align.merge_center", r, |s, p| {
            merge(s, p, true, false)
        }),
        CommandSpec::new("home.align.merge_cells", r, |s, p| {
            merge(s, p, false, false)
        }),
        CommandSpec::new("home.align.merge_across", r, |s, p| {
            merge(s, p, false, true)
        }),
        CommandSpec::new("home.align.unmerge", r, unmerge),
        CommandSpec::new(
            "home.number.format",
            r#"{"range"?: "A1", "code": "0.00"}"#,
            |s, p| {
                let code = str_req(p, "code")?.to_string();
                if code.len() > 255 {
                    return Err(CmdError::Param("format code too long".into()));
                }
                restyle(s, p, "Number Format", &move |st| st.num_fmt = code.clone())
            },
        ),
        CommandSpec::new(
            "home.number.dialog",
            r#"{"range"?: "A1", "code": "0.00"}  (Format Cells, number tab)"#,
            |s, p| {
                let code = str_req(p, "code")?.to_string();
                restyle(s, p, "Format Cells", &move |st| st.num_fmt = code.clone())
            },
        ),
        CommandSpec::new("home.number.general", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "General".into())
        }),
        CommandSpec::new("home.number.number", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "#,##0.00".into())
        }),
        CommandSpec::new("home.number.currency", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = CURRENCY.into())
        }),
        CommandSpec::new("home.number.accounting", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = ACCOUNTING.into())
        }),
        CommandSpec::new("home.number.percent", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "0%".into())
        }),
        CommandSpec::new("home.number.comma", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = COMMA.into())
        }),
        CommandSpec::new("home.number.date_short", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "d-mmm-yy".into())
        }),
        CommandSpec::new("home.number.time", r, |s, p| {
            restyle(s, p, "Number Format", &|st| {
                st.num_fmt = "h:mm AM/PM".into()
            })
        }),
        CommandSpec::new("home.number.scientific", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "0.00E+00".into())
        }),
        CommandSpec::new("home.number.text", r, |s, p| {
            restyle(s, p, "Number Format", &|st| st.num_fmt = "@".into())
        }),
        CommandSpec::new("home.number.decimal_increase", r, |s, p| {
            let n = shown_decimals(s);
            restyle(s, p, "Increase Decimal", &move |st| {
                st.num_fmt = with_decimals(&st.num_fmt, (n + 1).min(30))
            })
        }),
        CommandSpec::new("home.number.decimal_decrease", r, |s, p| {
            let n = shown_decimals(s);
            restyle(s, p, "Decrease Decimal", &move |st| {
                st.num_fmt = with_decimals(&st.num_fmt, n.saturating_sub(1))
            })
        }),
        CommandSpec::new("home.cells.lock_cell", r, |s, p| {
            toggle(s, p, "Lock Cell", |st| st.locked, |st, v| st.locked = v)
        }),
        CommandSpec::new(
            "home.clipboard.format_painter",
            r#"{"from"?: "A1", "range": "C1:C5"}"#,
            |s, p| {
                let (fs, from) = crate::params::cell(s, p, "from")?;
                let style =
                    s.wb.sheet(fs)
                        .and_then(|sh| sh.get(from))
                        .map_or(StyleId::DEFAULT, |c| c.style);
                let target = s.wb.styles.get(style).clone();
                restyle(s, p, "Format Painter", &move |st| *st = target.clone())
            },
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimals() {
        assert_eq!(with_decimals("0.00", 3), "0.000");
        assert_eq!(with_decimals("0%", 1), "0.0%");
        assert_eq!(with_decimals("General", 1), "0.0");
        assert_eq!(with_decimals("#,##0.00;(#,##0.00)", 0), "#,##0;(#,##0)");
        assert_eq!(decimals_of("0.000%"), 3);
        // Text and dash sections keep their shape.
        assert_eq!(
            with_decimals(COMMA, 3),
            "_(* #,##0.000_);_(* \\(#,##0.000\\);_(* \"-\"??_);_(@_)"
        );
    }

    fn run(s: &mut Session, id: &str, p: Json) {
        s.execute(id, &p).unwrap();
    }

    fn fmt_of(s: &Session, a1: &str) -> String {
        let c = s.wb.sheets[0].get(sp_core::CellAddr::parse(a1).unwrap());
        s.wb.styles
            .get(c.map_or(StyleId::DEFAULT, |c| c.style))
            .num_fmt
            .clone()
    }

    /// Under General, Increase/Decrease Decimal start from the decimals the value shows
    /// (2.25 → 2.250 / 2.3), as the target does; before, both started from zero.
    #[test]
    fn decimal_buttons_start_from_the_shown_decimals() {
        let mut s = Session::new();
        run(
            &mut s,
            "cell.set",
            serde_json::json!({"cell": "A1", "input": "2.25"}),
        );
        run(
            &mut s,
            "cell.set",
            serde_json::json!({"cell": "A2", "input": "2.25"}),
        );
        run(
            &mut s,
            "cell.set",
            serde_json::json!({"cell": "A3", "input": "10"}),
        );
        s.select_range(
            sp_core::CellRange::parse("A1").unwrap(),
            sp_core::CellAddr::new(0, 0),
        );
        run(
            &mut s,
            "home.number.decimal_increase",
            serde_json::json!({}),
        );
        assert_eq!(fmt_of(&s, "A1"), "0.000");
        s.select_range(
            sp_core::CellRange::parse("A2").unwrap(),
            sp_core::CellAddr::new(1, 0),
        );
        run(
            &mut s,
            "home.number.decimal_decrease",
            serde_json::json!({}),
        );
        assert_eq!(fmt_of(&s, "A2"), "0.0");
        s.select_range(
            sp_core::CellRange::parse("A3").unwrap(),
            sp_core::CellAddr::new(2, 0),
        );
        run(
            &mut s,
            "home.number.decimal_increase",
            serde_json::json!({}),
        );
        assert_eq!(fmt_of(&s, "A3"), "0.0");
    }

    /// Increase Indent left-aligns a General cell (an indent needs a left or right
    /// alignment, and the saved file drops it otherwise); a right-aligned cell stays right.
    #[test]
    fn increase_indent_left_aligns_general_cells() {
        let mut s = Session::new();
        run(
            &mut s,
            "home.align.indent_increase",
            serde_json::json!({"range": "A1"}),
        );
        let st = s.wb.styles.get(
            s.wb.sheets[0]
                .get(sp_core::CellAddr::new(0, 0))
                .unwrap()
                .style,
        );
        assert_eq!((st.indent, st.h_align), (1, HAlign::Left));
        run(
            &mut s,
            "home.align.right",
            serde_json::json!({"range": "B1"}),
        );
        run(
            &mut s,
            "home.align.indent_increase",
            serde_json::json!({"range": "B1"}),
        );
        let st = s.wb.styles.get(
            s.wb.sheets[0]
                .get(sp_core::CellAddr::new(0, 1))
                .unwrap()
                .style,
        );
        assert_eq!((st.indent, st.h_align), (1, HAlign::Right));
    }

    /// Currency, Comma and Accounting are the target's built-in codes and display as it
    /// does (negatives in parentheses).
    #[test]
    fn builtin_currency_comma_accounting_codes() {
        let v = sp_core::Value::Number(-1234.5);
        let t = |code| sp_core::numfmt::format_value(&v, code).text;
        assert_eq!(t(CURRENCY), "($1,234.50)");
        assert_eq!(t(COMMA).trim(), "(1,234.50)");
        assert!(t(ACCOUNTING).contains("(1,234.50)"), "{}", t(ACCOUNTING));
        let pos = sp_core::numfmt::format_value(&sp_core::Value::Number(5.0), CURRENCY).text;
        assert_eq!(pos.trim_end(), "$5.00");
    }
}
