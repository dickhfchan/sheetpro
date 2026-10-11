//! Conditional formatting (`home.styles.cf.*`): highlight rules, top/bottom, data bars,
//! colour scales, icon sets, formula rules, clear, and rule management.

use crate::params::{bool_opt, range, str_opt, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_core::CellRange;
use sp_model::condfmt::next_priority;
use sp_model::{
    Border, BorderStyle, CfKind, CfOp, CfRule, Cfvo, CfvoKind, Color, CondFormat, DiffStyle,
    TextOp, TimePeriod,
};

/// Rules per sheet (keeps priorities and evaluation bounded).
const MAX_RULES: usize = 10_000;

/// Default data bar / colour scale colours (SheetPro palette).
const BAR_BLUE: Color = Color(96, 140, 200);
const SCALE_GREEN: Color = Color(98, 187, 120);
const SCALE_YELLOW: Color = Color(255, 229, 130);
const SCALE_RED: Color = Color(240, 100, 104);
const SCALE_BLUE: Color = Color(92, 132, 210);

fn bad(m: impl Into<String>) -> CmdError {
    CmdError::Param(m.into())
}

fn color_of(v: &Json, what: &str) -> Result<Color, CmdError> {
    v.as_str()
        .and_then(|t| Color::from_hex(t).or_else(|| named_color(t)))
        .ok_or_else(|| bad(format!("{what}: colour must be hex like \"#FF0000\"")))
}

fn named_color(t: &str) -> Option<Color> {
    Some(match t.to_ascii_lowercase().as_str() {
        "blue" => BAR_BLUE,
        "green" => SCALE_GREEN,
        "red" => SCALE_RED,
        "yellow" => SCALE_YELLOW,
        "orange" => Color(240, 150, 60),
        "light_blue" => Color(0, 140, 220),
        "purple" => Color(150, 90, 180),
        "white" => Color::WHITE,
        "black" => Color::BLACK,
        _ => return None,
    })
}

/// `"format"`: a preset name or `{"fill","font_color","bold","italic","underline",
/// "strike","num_fmt","border"}`. Default: light red fill with dark red text.
fn format_param(p: &Json) -> Result<DiffStyle, CmdError> {
    match p.get("format") {
        None | Some(Json::Null) => Ok(DiffStyle::preset("red")),
        Some(Json::String(name)) => {
            let known = [
                "red",
                "yellow",
                "green",
                "red_fill",
                "light_red_fill",
                "red_text",
                "red_border",
            ];
            if known.contains(&name.to_ascii_lowercase().as_str()) {
                Ok(DiffStyle::preset(name))
            } else {
                Err(bad(format!("unknown format preset '{name}'")))
            }
        }
        Some(Json::Object(o)) => {
            let mut d = DiffStyle::default();
            if let Some(v) = o.get("fill") {
                d.fill = Some(color_of(v, "fill")?);
            }
            if let Some(v) = o.get("font_color") {
                d.font_color = Some(color_of(v, "font_color")?);
            }
            d.bold = o.get("bold").and_then(Json::as_bool);
            d.italic = o.get("italic").and_then(Json::as_bool);
            d.underline = o.get("underline").and_then(Json::as_bool);
            d.strike = o.get("strike").and_then(Json::as_bool);
            d.num_fmt = o.get("num_fmt").and_then(Json::as_str).map(str::to_string);
            if let Some(v) = o.get("border") {
                d.border = Some(Border {
                    style: BorderStyle::Thin,
                    color: Some(color_of(v, "border")?),
                });
            }
            Ok(d)
        }
        Some(_) => Err(bad("format must be a preset name or an object")),
    }
}

/// A rule operand: number, text, or `=formula` (stored without `=`). Plain text becomes a
/// quoted string literal.
fn operand(p: &Json, k: &str) -> Result<String, CmdError> {
    match p.get(k) {
        None | Some(Json::Null) => Ok(String::new()),
        Some(Json::Number(n)) => Ok(n.to_string()),
        Some(Json::Bool(b)) => Ok(if *b { "TRUE".into() } else { "FALSE".into() }),
        Some(Json::String(t)) => {
            let t = t.trim();
            if let Some(f) = t.strip_prefix('=') {
                sp_formula::parse(f).map_err(|e| bad(format!("bad formula '{t}': {e}")))?;
                Ok(f.to_string())
            } else if t.parse::<f64>().is_ok() {
                Ok(t.to_string())
            } else {
                Ok(format!("\"{}\"", t.replace('"', "\"\"")))
            }
        }
        Some(_) => Err(bad(format!("{k} must be a number or text"))),
    }
}

/// Build a highlight / top-bottom rule kind from `rule` and its operands.
fn rule_kind(p: &Json) -> Result<CfKind, CmdError> {
    let rule = str_opt(p, "rule").ok_or_else(|| bad("missing 'rule'"))?;
    let r = rule.to_ascii_lowercase().replace([' ', '-'], "_");
    let cell_is = |op: CfOp| -> Result<CfKind, CmdError> {
        let f1 = operand(p, "value")?;
        if f1.is_empty() {
            return Err(bad("this rule needs 'value'"));
        }
        let f2 = operand(p, "value2")?;
        if matches!(op, CfOp::Between | CfOp::NotBetween) && f2.is_empty() {
            return Err(bad("between rules need 'value2'"));
        }
        Ok(CfKind::CellIs { op, f1, f2 })
    };
    let text = |op: TextOp| -> Result<CfKind, CmdError> {
        let t = str_opt(p, "text")
            .or_else(|| str_opt(p, "value"))
            .ok_or_else(|| bad("text rules need 'text'"))?;
        Ok(CfKind::Text {
            op,
            text: t.to_string(),
        })
    };
    let rank = u32_opt(p, "rank")?.unwrap_or(10).clamp(1, 1000);
    let percent = bool_opt(p, "percent").unwrap_or(false);
    if percent && rank > 100 {
        return Err(bad("percent rank must be 1..100"));
    }
    let avg = |below: bool| -> Result<CfKind, CmdError> {
        Ok(CfKind::Average {
            below,
            equal: bool_opt(p, "equal").unwrap_or(false),
            std_dev: u32_opt(p, "std_dev")?.unwrap_or(0).min(3),
        })
    };
    Ok(match r.as_str() {
        "greater_than" | "greater" | ">" => cell_is(CfOp::Greater)?,
        "less_than" | "less" | "<" => cell_is(CfOp::Less)?,
        "greater_equal" | ">=" => cell_is(CfOp::GreaterEq)?,
        "less_equal" | "<=" => cell_is(CfOp::LessEq)?,
        "between" => cell_is(CfOp::Between)?,
        "not_between" => cell_is(CfOp::NotBetween)?,
        "equal_to" | "equal" | "=" => cell_is(CfOp::Equal)?,
        "not_equal" | "<>" => cell_is(CfOp::NotEqual)?,
        "text_contains" | "contains" => text(TextOp::Contains)?,
        "not_contains" => text(TextOp::NotContains)?,
        "begins_with" => text(TextOp::BeginsWith)?,
        "ends_with" => text(TextOp::EndsWith)?,
        "date_occurring" => {
            let per = str_opt(p, "period").unwrap_or("today");
            CfKind::TimePeriod(
                TimePeriod::from_name(&per.replace('_', ""))
                    .or_else(|| TimePeriod::from_name(per))
                    .ok_or_else(|| bad(format!("unknown period '{per}'")))?,
            )
        }
        "duplicates" | "duplicate" => CfKind::Duplicates,
        "unique" => CfKind::Unique,
        "blanks" => CfKind::Blanks,
        "no_blanks" => CfKind::NoBlanks,
        "errors" => CfKind::Errors,
        "no_errors" => CfKind::NoErrors,
        "top" | "top_items" => CfKind::Top {
            bottom: false,
            percent,
            rank,
        },
        "bottom" | "bottom_items" => CfKind::Top {
            bottom: true,
            percent,
            rank,
        },
        "top_percent" => CfKind::Top {
            bottom: false,
            percent: true,
            rank: rank.min(100),
        },
        "bottom_percent" => CfKind::Top {
            bottom: true,
            percent: true,
            rank: rank.min(100),
        },
        "above_average" => avg(false)?,
        "below_average" => avg(true)?,
        "formula" | "expression" => formula_kind(p)?,
        _ => return Err(bad(format!("unknown rule '{rule}'"))),
    })
}

fn formula_kind(p: &Json) -> Result<CfKind, CmdError> {
    let f = str_opt(p, "formula").ok_or_else(|| bad("formula rules need 'formula'"))?;
    let f = f.trim().trim_start_matches('=');
    sp_formula::parse(f).map_err(|e| bad(format!("bad formula '{f}': {e}")))?;
    Ok(CfKind::Expression(f.to_string()))
}

/// Add one rule on `range` at the top priority. Returns the new rule's priority.
fn add_rule(s: &mut Session, p: &Json, kind: CfKind, style: DiffStyle, label: &str) -> CmdResult {
    let (sheet, r) = range(s, p, "range")?;
    let stop = bool_opt(p, "stop_if_true").unwrap_or(false);
    s.transact(label, |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no such sheet".into()))?;
        let total: usize = sh.cond_formats.iter().map(|c| c.rules.len()).sum();
        if total >= MAX_RULES {
            return Err(CmdError::Failed(
                "too many conditional formatting rules".into(),
            ));
        }
        // New rules go first, as in the target app: shift existing priorities down.
        for cf in &mut sh.cond_formats {
            for rule in &mut cf.rules {
                rule.priority = rule.priority.saturating_add(1);
            }
        }
        sh.cond_formats.push(CondFormat {
            ranges: vec![r],
            rules: vec![CfRule {
                kind,
                style,
                priority: 1,
                stop_if_true: stop,
            }],
        });
        Ok(json!({"range": r.to_a1(), "priority": 1}))
    })
}

fn highlight(s: &mut Session, p: &Json) -> CmdResult {
    let kind = rule_kind(p)?;
    if matches!(kind, CfKind::Top { .. } | CfKind::Average { .. }) {
        return Err(bad(
            "use home.styles.cf.top_bottom for top/bottom and average rules",
        ));
    }
    let style = format_param(p)?;
    add_rule(s, p, kind, style, "Conditional Formatting")
}

fn top_bottom(s: &mut Session, p: &Json) -> CmdResult {
    let kind = rule_kind(p)?;
    if !matches!(kind, CfKind::Top { .. } | CfKind::Average { .. }) {
        return Err(bad(
            "rule must be top, bottom, top_percent, bottom_percent, above_average or below_average",
        ));
    }
    let style = format_param(p)?;
    add_rule(s, p, kind, style, "Conditional Formatting")
}

/// `{"type": "min|max|num|percent|percentile|formula", "value": …}`.
fn cfvo_param(p: &Json, k: &str, default: Cfvo) -> Result<Cfvo, CmdError> {
    let Some(v) = p.get(k) else {
        return Ok(default);
    };
    let t = v
        .get("type")
        .and_then(Json::as_str)
        .ok_or_else(|| bad(format!("{k}.type missing")))?;
    let kind = CfvoKind::from_name(t).ok_or_else(|| bad(format!("unknown {k}.type '{t}'")))?;
    let value = match v.get("value") {
        None | Some(Json::Null) => String::new(),
        Some(Json::Number(n)) => n.to_string(),
        Some(Json::String(s)) => {
            let s = s.trim().trim_start_matches('=');
            if kind == CfvoKind::Formula {
                sp_formula::parse(s).map_err(|e| bad(format!("bad {k} formula: {e}")))?;
            }
            s.to_string()
        }
        Some(_) => return Err(bad(format!("{k}.value must be a number or text"))),
    };
    if matches!(
        kind,
        CfvoKind::Num | CfvoKind::Percent | CfvoKind::Percentile | CfvoKind::Formula
    ) && value.is_empty()
    {
        return Err(bad(format!("{k}.value required for type '{t}'")));
    }
    Ok(Cfvo::new(kind, value))
}

fn data_bars(s: &mut Session, p: &Json) -> CmdResult {
    let color = match p.get("color") {
        None | Some(Json::Null) => BAR_BLUE,
        Some(v) => color_of(v, "color")?,
    };
    let kind = CfKind::DataBar {
        min: cfvo_param(p, "min", Cfvo::new(CfvoKind::Min, ""))?,
        max: cfvo_param(p, "max", Cfvo::new(CfvoKind::Max, ""))?,
        color,
        gradient: bool_opt(p, "gradient").unwrap_or(true),
        show_value: bool_opt(p, "show_value").unwrap_or(true),
    };
    add_rule(s, p, kind, DiffStyle::default(), "Data Bars")
}

fn color_scales(s: &mut Session, p: &Json) -> CmdResult {
    let colors: Vec<Color> = match p.get("colors") {
        Some(Json::Array(a)) => a
            .iter()
            .map(|c| color_of(c, "colors"))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(bad("colors must be an array of 2 or 3 hex colours")),
        None => {
            let name = str_opt(p, "scale").unwrap_or("green_yellow_red");
            let w = Color::WHITE;
            match name.to_ascii_lowercase().as_str() {
                "green_yellow_red" => vec![SCALE_RED, SCALE_YELLOW, SCALE_GREEN],
                "red_yellow_green" => vec![SCALE_GREEN, SCALE_YELLOW, SCALE_RED],
                "green_white_red" => vec![SCALE_RED, w, SCALE_GREEN],
                "red_white_green" => vec![SCALE_GREEN, w, SCALE_RED],
                "blue_white_red" => vec![SCALE_RED, w, SCALE_BLUE],
                "red_white_blue" => vec![SCALE_BLUE, w, SCALE_RED],
                "green_white" => vec![w, SCALE_GREEN],
                "white_green" => vec![SCALE_GREEN, w],
                "red_white" => vec![w, SCALE_RED],
                "white_red" => vec![SCALE_RED, w],
                "green_yellow" => vec![SCALE_YELLOW, SCALE_GREEN],
                "yellow_green" => vec![SCALE_GREEN, SCALE_YELLOW],
                _ => return Err(bad(format!("unknown scale '{name}'"))),
            }
        }
    };
    // Lists are given low → high value (the scale name reads high → low).
    let points = match colors.as_slice() {
        [lo, hi] => vec![
            (Cfvo::new(CfvoKind::Min, ""), *lo),
            (Cfvo::new(CfvoKind::Max, ""), *hi),
        ],
        [lo, mid, hi] => vec![
            (Cfvo::new(CfvoKind::Min, ""), *lo),
            (Cfvo::new(CfvoKind::Percentile, "50"), *mid),
            (Cfvo::new(CfvoKind::Max, ""), *hi),
        ],
        _ => return Err(bad("a colour scale has 2 or 3 colours")),
    };
    add_rule(
        s,
        p,
        CfKind::ColorScale { points },
        DiffStyle::default(),
        "Color Scales",
    )
}

/// Icon set names accepted (file-format names).
pub const ICON_SETS: &[&str] = &[
    "3Arrows",
    "3ArrowsGray",
    "3Flags",
    "3TrafficLights1",
    "3TrafficLights2",
    "3Signs",
    "3Symbols",
    "3Symbols2",
    "3Stars",
    "3Triangles",
    "4Arrows",
    "4ArrowsGray",
    "4RedToBlack",
    "4Rating",
    "4TrafficLights",
    "5Arrows",
    "5ArrowsGray",
    "5Rating",
    "5Quarters",
    "5Boxes",
];

fn icon_sets(s: &mut Session, p: &Json) -> CmdResult {
    let name = str_opt(p, "set").unwrap_or("3TrafficLights1");
    let set = ICON_SETS
        .iter()
        .find(|n| n.eq_ignore_ascii_case(name))
        .ok_or_else(|| {
            bad(format!(
                "unknown icon set '{name}' (one of {})",
                ICON_SETS.join(", ")
            ))
        })?;
    let count = CfKind::icon_count(set);
    let thresholds: Vec<f64> = match p.get("thresholds") {
        Some(Json::Array(a)) => a
            .iter()
            .map(|v| v.as_f64().ok_or_else(|| bad("thresholds are numbers")))
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(bad("thresholds must be an array of percents")),
        None => (1..count)
            .map(|i| (100.0 * i as f64 / count as f64).round())
            .collect(),
    };
    if thresholds.len() != count - 1 {
        return Err(bad(format!("{set} needs {} thresholds", count - 1)));
    }
    let mut points = vec![Cfvo::new(CfvoKind::Percent, "0")];
    points.extend(
        thresholds
            .iter()
            .map(|t| Cfvo::new(CfvoKind::Percent, sp_core::value::format_general(*t))),
    );
    let kind = CfKind::IconSet {
        set: (*set).to_string(),
        points,
        reverse: bool_opt(p, "reverse").unwrap_or(false),
        show_value: bool_opt(p, "show_value").unwrap_or(true),
    };
    add_rule(s, p, kind, DiffStyle::default(), "Icon Sets")
}

fn new_rule(s: &mut Session, p: &Json) -> CmdResult {
    let ty = str_opt(p, "type").unwrap_or(if p.get("formula").is_some() {
        "formula"
    } else {
        "highlight"
    });
    match ty {
        "formula" | "expression" => {
            let kind = formula_kind(p)?;
            let style = format_param(p)?;
            add_rule(s, p, kind, style, "New Formatting Rule")
        }
        "highlight" | "rule" => {
            let kind = rule_kind(p)?;
            let style = format_param(p)?;
            add_rule(s, p, kind, style, "New Formatting Rule")
        }
        "data_bar" => data_bars(s, p),
        "color_scale" => color_scales(s, p),
        "icon_set" => icon_sets(s, p),
        _ => Err(bad(format!("unknown rule type '{ty}'"))),
    }
}

fn clear(s: &mut Session, p: &Json) -> CmdResult {
    let whole = matches!(str_opt(p, "scope"), Some("sheet"));
    let (sheet, r) = range(s, p, "range")?;
    let removed = s.transact("Clear Rules", |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no such sheet".into()))?;
        let before: usize = sh.cond_formats.iter().map(|c| c.rules.len()).sum();
        if whole {
            sh.cond_formats.clear();
        } else {
            for cf in &mut sh.cond_formats {
                cf.ranges = cf
                    .ranges
                    .iter()
                    .flat_map(|x| super::validation_cmds::subtract(*x, r))
                    .collect();
            }
            sh.cond_formats.retain(|c| !c.ranges.is_empty());
        }
        let after: usize = sh.cond_formats.iter().map(|c| c.rules.len()).sum();
        Ok(before - after.min(before))
    })?;
    Ok(json!({"removed_rules": removed}))
}

/// Short description of a rule for listings.
pub fn describe(kind: &CfKind) -> Json {
    match kind {
        CfKind::CellIs { op, f1, f2 } => {
            json!({"type": "cell_is", "operator": op.name(), "value": f1, "value2": f2})
        }
        CfKind::Text { op, text } => json!({"type": "text", "operator": op.name(), "text": text}),
        CfKind::TimePeriod(t) => json!({"type": "date_occurring", "period": t.name()}),
        CfKind::Duplicates => json!({"type": "duplicates"}),
        CfKind::Unique => json!({"type": "unique"}),
        CfKind::Top {
            bottom,
            percent,
            rank,
        } => json!({"type": if *bottom {"bottom"} else {"top"}, "percent": percent, "rank": rank}),
        CfKind::Average {
            below,
            equal,
            std_dev,
        } => {
            json!({"type": if *below {"below_average"} else {"above_average"}, "equal": equal, "std_dev": std_dev})
        }
        CfKind::Blanks => json!({"type": "blanks"}),
        CfKind::NoBlanks => json!({"type": "no_blanks"}),
        CfKind::Errors => json!({"type": "errors"}),
        CfKind::NoErrors => json!({"type": "no_errors"}),
        CfKind::Expression(f) => json!({"type": "formula", "formula": format!("={f}")}),
        CfKind::DataBar { color, .. } => {
            json!({"type": "data_bar", "color": format!("#{}", color.to_argb_hex().get(2..).unwrap_or(""))})
        }
        CfKind::ColorScale { points } => json!({"type": "color_scale", "colors": points.len()}),
        CfKind::IconSet { set, .. } => json!({"type": "icon_set", "set": set}),
    }
}

fn list(s: &Session, sheet: usize, within: Option<CellRange>) -> Json {
    let Some(sh) = s.wb.sheet(sheet) else {
        return json!([]);
    };
    let mut rows: Vec<(u32, Json)> = Vec::new();
    for cf in &sh.cond_formats {
        if let Some(w) = within
            && !cf.ranges.iter().any(|r| r.intersect(&w).is_some())
        {
            continue;
        }
        let ranges: Vec<String> = cf.ranges.iter().map(CellRange::to_a1).collect();
        for r in &cf.rules {
            let mut d = describe(&r.kind);
            if let Some(o) = d.as_object_mut() {
                o.insert("priority".into(), json!(r.priority));
                o.insert("applies_to".into(), json!(ranges.join(" ")));
                o.insert("stop_if_true".into(), json!(r.stop_if_true));
            }
            rows.push((r.priority, d));
        }
    }
    rows.sort_by_key(|(p, _)| *p);
    Json::Array(rows.into_iter().map(|(_, d)| d).collect())
}

/// Renumber priorities 1..n in current order.
fn renumber(sh: &mut sp_model::Sheet) {
    let mut keys: Vec<(u32, usize, usize)> = Vec::new();
    for (i, cf) in sh.cond_formats.iter().enumerate() {
        for (j, r) in cf.rules.iter().enumerate() {
            keys.push((r.priority, i, j));
        }
    }
    keys.sort_unstable();
    for (n, (_, i, j)) in keys.into_iter().enumerate() {
        if let Some(r) = sh.cond_formats.get_mut(i).and_then(|c| c.rules.get_mut(j)) {
            r.priority = u32::try_from(n + 1).unwrap_or(u32::MAX);
        }
    }
}

fn manage(s: &mut Session, p: &Json) -> CmdResult {
    let action = str_opt(p, "action").unwrap_or("list");
    let sheet = s.sheet_index();
    if action == "list" {
        let within = if str_opt(p, "scope") == Some("selection") {
            Some(s.sel.range())
        } else {
            None
        };
        return Ok(list(s, sheet, within));
    }
    let pri = u32_opt(p, "priority")?.ok_or_else(|| bad("'priority' required"))?;
    let applies = match str_opt(p, "applies_to") {
        Some(t) => Some(
            t.split([' ', ','])
                .filter(|x| !x.is_empty())
                .map(|x| CellRange::parse(x).map_err(|_| bad(format!("bad range '{x}'"))))
                .collect::<Result<Vec<_>, _>>()?,
        ),
        None => None,
    };
    let style = if p.get("format").is_some() {
        Some(format_param(p)?)
    } else {
        None
    };
    let stop = bool_opt(p, "stop_if_true");
    s.transact("Manage Rules", |tx, _| {
        tx.touch_sheet(sheet);
        let sh = tx
            .wb
            .sheet_mut(sheet)
            .ok_or_else(|| CmdError::Failed("no such sheet".into()))?;
        renumber(sh);
        let find = |sh: &sp_model::Sheet, pr: u32| {
            sh.cond_formats.iter().enumerate().find_map(|(i, c)| {
                c.rules
                    .iter()
                    .position(|r| r.priority == pr)
                    .map(|j| (i, j))
            })
        };
        let (i, j) = find(sh, pri).ok_or_else(|| bad(format!("no rule with priority {pri}")))?;
        match action {
            "delete" => {
                if let Some(cf) = sh.cond_formats.get_mut(i) {
                    cf.rules.remove(j);
                }
                sh.cond_formats.retain(|c| !c.rules.is_empty());
            }
            "move_up" | "move_down" => {
                let other = if action == "move_up" {
                    pri.checked_sub(1)
                } else {
                    pri.checked_add(1)
                };
                if let Some(o) = other
                    && let Some((k, l)) = find(sh, o)
                {
                    if let Some(r) = sh.cond_formats.get_mut(k).and_then(|c| c.rules.get_mut(l)) {
                        r.priority = pri;
                    }
                    if let Some(r) = sh.cond_formats.get_mut(i).and_then(|c| c.rules.get_mut(j)) {
                        r.priority = o;
                    }
                }
            }
            "edit" => {
                let cf = sh
                    .cond_formats
                    .get_mut(i)
                    .ok_or_else(|| CmdError::Failed("rule vanished".into()))?;
                if let Some(a) = &applies {
                    if a.is_empty() {
                        return Err(bad("applies_to is empty"));
                    }
                    // Split the rule off into its own format when it shares ranges.
                    if cf.rules.len() > 1 {
                        let rule = cf.rules.remove(j);
                        sh.cond_formats.push(CondFormat {
                            ranges: a.clone(),
                            rules: vec![rule],
                        });
                    } else {
                        cf.ranges.clone_from(a);
                    }
                }
                let (i, j) = find(sh, pri).ok_or_else(|| bad("rule vanished"))?;
                if let Some(r) = sh.cond_formats.get_mut(i).and_then(|c| c.rules.get_mut(j)) {
                    if let Some(st) = &style {
                        r.style = st.clone();
                    }
                    if let Some(b) = stop {
                        r.stop_if_true = b;
                    }
                }
            }
            _ => return Err(bad(format!("unknown action '{action}'"))),
        }
        renumber(sh);
        Ok(())
    })?;
    Ok(list(s, sheet, None))
}

/// Next priority for a sheet (used by importers/tests).
pub fn priority_after(s: &Session, sheet: usize) -> u32 {
    s.wb.sheet(sheet)
        .map_or(1, |sh| next_priority(&sh.cond_formats))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "home.styles.cf.highlight_rules",
            r#"{"range"?: "A1:A10", "rule": "greater_than|less_than|greater_equal|less_equal|between|not_between|equal_to|not_equal|text_contains|not_contains|begins_with|ends_with|date_occurring|duplicates|unique|blanks|no_blanks|errors|no_errors", "value"?: 10 | "=$B$1" | "text", "value2"?: 20, "text"?: "abc", "period"?: "yesterday|today|tomorrow|last7days|last_week|this_week|next_week|last_month|this_month|next_month", "format"?, "stop_if_true"?} → {"range", "priority"}"#,
            highlight,
        ),
        CommandSpec::new(
            "home.styles.cf.top_bottom",
            r#"{"range"?: "A1:A10", "rule": "top|bottom|top_percent|bottom_percent|above_average|below_average", "rank"?: 10, "percent"?: false, "equal"?: false, "std_dev"?: 0..3, "format"?, "stop_if_true"?}"#,
            top_bottom,
        ),
        CommandSpec::new(
            "home.styles.cf.data_bars",
            r##"{"range"?: "A1:A10", "color"?: "#638EC6" | "blue|green|red|orange|light_blue|purple", "gradient"?: true, "show_value"?: true, "min"?: {"type": "min|num|percent|percentile|formula|autoMin", "value"?: 0}, "max"?: {"type": "max|…", "value"?: 100}}"##,
            data_bars,
        ),
        CommandSpec::new(
            "home.styles.cf.color_scales",
            r##"{"range"?: "A1:A10", "scale"?: "green_yellow_red|red_yellow_green|green_white_red|red_white_green|blue_white_red|red_white_blue|green_white|white_green|red_white|white_red|green_yellow|yellow_green", "colors"?: ["#low", "#mid"?, "#high"]}"##,
            color_scales,
        ),
        CommandSpec::new(
            "home.styles.cf.icon_sets",
            r#"{"range"?: "A1:A10", "set"?: "3TrafficLights1|3Arrows|3ArrowsGray|3Flags|3TrafficLights2|3Signs|3Symbols|3Symbols2|3Stars|3Triangles|4Arrows|4ArrowsGray|4RedToBlack|4Rating|4TrafficLights|5Arrows|5ArrowsGray|5Rating|5Quarters|5Boxes", "thresholds"?: [33, 67] (percents, one fewer than icons), "reverse"?: false, "show_value"?: true}"#,
            icon_sets,
        ),
        CommandSpec::new(
            "home.styles.cf.new_rule",
            r#"{"range"?: "A1:C10", "type"?: "formula|highlight|data_bar|color_scale|icon_set", "formula"?: "=$A1>100" (relative to the range's top-left), "rule"?: (as highlight_rules/top_bottom), "format"?, "stop_if_true"?, …type-specific params}"#,
            new_rule,
        ),
        CommandSpec::new(
            "home.styles.cf.clear",
            r#"{"range"?: "A1:C10" (default: selection), "scope"?: "selection|sheet"} → {"removed_rules"}"#,
            clear,
        ),
        CommandSpec::new(
            "home.styles.cf.manage",
            r#"{"action"?: "list|delete|move_up|move_down|edit", "scope"?: "sheet|selection" (list), "priority"?: 1, "applies_to"?: "A1:A10 C1:C10", "format"?, "stop_if_true"?} → rules [{"priority", "type", "applies_to", "stop_if_true", …}] in priority order"#,
            manage,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{CellAddr, Value};

    fn session() -> Session {
        let mut s = Session::new();
        for (i, v) in [5.0, 50.0, 95.0, 50.0].iter().enumerate() {
            s.execute(
                "cell.set",
                &json!({"cell": format!("A{}", i + 1), "input": v.to_string()}),
            )
            .unwrap();
        }
        s
    }

    fn rules(s: &mut Session) -> Vec<Json> {
        s.execute("home.styles.cf.manage", &json!({}))
            .unwrap()
            .as_array()
            .cloned()
            .unwrap()
    }

    #[test]
    fn add_list_manage_clear() {
        let mut s = session();
        s.execute(
            "home.styles.cf.highlight_rules",
            &json!({"range": "A1:A4", "rule": "greater_than", "value": 40}),
        )
        .unwrap();
        s.execute(
            "home.styles.cf.top_bottom",
            &json!({"range": "A1:A4", "rule": "top", "rank": 1, "format": {"bold": true}}),
        )
        .unwrap();
        s.execute("home.styles.cf.data_bars", &json!({"range": "A1:A4"}))
            .unwrap();
        s.execute(
            "home.styles.cf.color_scales",
            &json!({"range": "B1:B4", "scale": "red_white_blue"}),
        )
        .unwrap();
        s.execute(
            "home.styles.cf.icon_sets",
            &json!({"range": "C1:C4", "set": "3arrows"}),
        )
        .unwrap();
        s.execute(
            "home.styles.cf.new_rule",
            &json!({"range": "D1:D4", "formula": "=$A1>10", "format": "green"}),
        )
        .unwrap();
        let r = rules(&mut s);
        assert_eq!(r.len(), 6);
        assert_eq!(r[0]["type"], "formula");
        assert_eq!(r[5]["type"], "cell_is");
        // Evaluate through the render model.
        let wb = &s.wb;
        let sh = &wb.sheets[0];
        let mut ev = sp_render::condfmt::CfEval::new(wb, 0, sh);
        let o = ev.cell(CellAddr::new(2, 0), &Value::Number(95.0)).unwrap();
        assert_eq!(o.style.bold, Some(true));
        assert!(o.style.fill.is_some());
        assert!(o.data_bar.is_some());
        // Move the cell_is rule to the top, then delete it.
        s.execute(
            "home.styles.cf.manage",
            &json!({"action": "move_up", "priority": 6}),
        )
        .unwrap();
        assert_eq!(rules(&mut s)[4]["type"], "cell_is");
        s.execute(
            "home.styles.cf.manage",
            &json!({"action": "edit", "priority": 5, "stop_if_true": true, "applies_to": "A1:A2"}),
        )
        .unwrap();
        let r = rules(&mut s);
        assert_eq!(r[4]["stop_if_true"], true);
        assert_eq!(r[4]["applies_to"], "A1:A2");
        s.execute(
            "home.styles.cf.manage",
            &json!({"action": "delete", "priority": 5}),
        )
        .unwrap();
        assert_eq!(rules(&mut s).len(), 5);
        // Clear from a selection carves ranges; undo restores.
        let out = s
            .execute("home.styles.cf.clear", &json!({"range": "A1:A4"}))
            .unwrap();
        assert_eq!(out["removed_rules"], 2);
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(rules(&mut s).len(), 5);
        s.execute("home.styles.cf.clear", &json!({"scope": "sheet"}))
            .unwrap();
        assert!(rules(&mut s).is_empty());
    }

    #[test]
    fn bad_params_are_errors() {
        let mut s = session();
        for (id, p) in [
            ("home.styles.cf.highlight_rules", json!({"rule": "nope"})),
            (
                "home.styles.cf.highlight_rules",
                json!({"rule": "between", "value": 1}),
            ),
            ("home.styles.cf.highlight_rules", json!({"rule": "top"})),
            (
                "home.styles.cf.top_bottom",
                json!({"rule": "greater_than", "value": 1}),
            ),
            (
                "home.styles.cf.top_bottom",
                json!({"rule": "top", "rank": 500, "percent": true}),
            ),
            ("home.styles.cf.data_bars", json!({"color": "zz"})),
            ("home.styles.cf.data_bars", json!({"min": {"type": "num"}})),
            (
                "home.styles.cf.color_scales",
                json!({"colors": ["#FF0000"]}),
            ),
            ("home.styles.cf.icon_sets", json!({"set": "9Nope"})),
            (
                "home.styles.cf.icon_sets",
                json!({"set": "3Arrows", "thresholds": [1]}),
            ),
            ("home.styles.cf.new_rule", json!({"formula": "=SUM("})),
            (
                "home.styles.cf.manage",
                json!({"action": "delete", "priority": 99}),
            ),
            (
                "home.styles.cf.manage",
                json!({"action": "explode", "priority": 1}),
            ),
        ] {
            assert!(s.execute(id, &p).is_err(), "{id} {p}");
        }
    }
}
