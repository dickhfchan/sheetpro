//! Workbook themes (`layout.themes`): apply a built-in theme, or just its colours or fonts,
//! or custom colours/fonts. Styles that refer to theme colours follow automatically (they
//! resolve through `Workbook::theme` when drawn); cells in the old body/heading font move
//! to the new one.

use crate::params::str_opt;
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use sp_model::Color;
use sp_model::theme::{FONT_SCHEMES, SLOT_LABELS, SLOT_NAMES, Theme, builtin_color_schemes, builtin_themes};

fn bad(m: impl Into<String>) -> CmdError {
    CmdError::Param(m.into())
}

fn hex(c: Color) -> String {
    format!("{:02X}{:02X}{:02X}", c.0, c.1, c.2)
}

/// The current theme as JSON (also the shape of `custom`).
pub fn theme_json(t: &Theme) -> Json {
    json!({
        "name": t.name,
        "colors_name": t.colors_name,
        "colors": t.colors.iter().map(|c| hex(*c)).collect::<Vec<_>>(),
        "slots": SLOT_NAMES,
        "labels": SLOT_LABELS,
        "fonts_name": t.fonts_name,
        "major_font": t.major_font,
        "minor_font": t.minor_font,
    })
}

fn listing(s: &Session) -> Json {
    json!({
        "current": theme_json(&s.wb.theme),
        "themes": builtin_themes().iter().map(theme_json).collect::<Vec<_>>(),
        "colors": builtin_color_schemes()
            .iter()
            .map(|(n, c)| json!({"name": n, "colors": c.iter().map(|x| hex(*x)).collect::<Vec<_>>()}))
            .collect::<Vec<_>>(),
        "fonts": FONT_SCHEMES
            .iter()
            .map(|(n, major, minor)| json!({"name": n, "major_font": major, "minor_font": minor}))
            .collect::<Vec<_>>(),
    })
}

fn font_name(p: &Json, k: &str) -> Result<Option<String>, CmdError> {
    match str_opt(p, k) {
        None => Ok(None),
        Some(f) if f.trim().is_empty() || f.len() > 31 => {
            Err(bad(format!("'{k}' must be a font name of 1-31 characters")))
        }
        Some(f) => Ok(Some(f.trim().to_string())),
    }
}

fn run(s: &mut Session, p: &Json) -> CmdResult {
    let mut t = s.wb.theme.clone();
    let mut changed = false;
    if let Some(n) = str_opt(p, "theme") {
        t = builtin_themes()
            .into_iter()
            .find(|x| x.name.eq_ignore_ascii_case(n.trim()))
            .ok_or_else(|| bad(format!("no built-in theme named '{n}'")))?;
        changed = true;
    }
    if let Some(n) = str_opt(p, "colors") {
        let (name, cols) = builtin_color_schemes()
            .into_iter()
            .find(|(x, _)| x.eq_ignore_ascii_case(n.trim()))
            .ok_or_else(|| bad(format!("no built-in colour scheme named '{n}'")))?;
        t.colors = cols;
        t.colors_name = name.to_string();
        changed = true;
    }
    if let Some(n) = str_opt(p, "fonts") {
        let (name, major, minor) = FONT_SCHEMES
            .iter()
            .find(|(x, _, _)| x.eq_ignore_ascii_case(n.trim()))
            .copied()
            .ok_or_else(|| bad(format!("no built-in font scheme named '{n}'")))?;
        t.major_font = major.into();
        t.minor_font = minor.into();
        t.fonts_name = name.into();
        changed = true;
    }
    match p.get("custom_colors") {
        None | Some(Json::Null) => {}
        Some(Json::Array(a)) if a.len() == 12 => {
            for (i, v) in a.iter().enumerate() {
                let c = v
                    .as_str()
                    .and_then(Color::from_hex)
                    .ok_or_else(|| bad(format!("custom_colors[{i}] is not an RRGGBB colour")))?;
                if let Some(slot) = t.colors.get_mut(i) {
                    *slot = c;
                }
            }
            t.colors_name = str_opt(p, "name").unwrap_or("Custom").chars().take(64).collect();
            changed = true;
        }
        Some(_) => return Err(bad("'custom_colors' must list 12 RRGGBB colours")),
    }
    if let Some(f) = font_name(p, "major_font")? {
        t.major_font = f;
        t.fonts_name = "Custom".into();
        changed = true;
    }
    if let Some(f) = font_name(p, "minor_font")? {
        t.minor_font = f;
        t.fonts_name = "Custom".into();
        changed = true;
    }
    if !changed {
        return Ok(listing(s));
    }
    if t.colors_name != t.name || t.fonts_name != t.name {
        // A mix of schemes is a custom theme.
        if !builtin_themes().iter().any(|b| *b == t) {
            t.name = str_opt(p, "name").unwrap_or("Custom").chars().take(64).collect();
        }
    }
    let old = s.wb.theme.clone();
    if old == t {
        return Ok(json!({"theme": theme_json(&t), "changed": false}));
    }
    let new = t.clone();
    s.transact("Theme", move |tx, _| {
        tx.touch_book();
        // Cells in the theme's body/heading font take the new one.
        if old.minor_font != new.minor_font || old.major_font != new.major_font {
            tx.wb.styles.update_all(|st| {
                if st.font_name.eq_ignore_ascii_case(&old.minor_font) {
                    st.font_name.clone_from(&new.minor_font);
                } else if st.font_name.eq_ignore_ascii_case(&old.major_font) {
                    st.font_name.clone_from(&new.major_font);
                }
            });
        }
        tx.wb.theme = new;
        Ok(())
    })?;
    s.status = format!("Theme: {}", t.name);
    Ok(json!({"theme": theme_json(&t), "changed": true}))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![CommandSpec::new(
        "layout.themes",
        r#"{"theme"?: "Harbor" (built-in theme), "colors"?: "Meadow" (colours only), "fonts"?: "Classic" (fonts only), "custom_colors"?: ["RRGGBB" ×12: dk1 lt1 dk2 lt2 accent1-6 hlink folHlink], "major_font"?: "Georgia", "minor_font"?: "Verdana", "name"?: "My Theme"} → no params: {"current", "themes", "colors", "fonts"}"#,
        run,
    )]
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use crate::Session;
    use serde_json::json;
    use sp_core::CellAddr;
    use sp_model::Color;

    #[test]
    fn apply_theme_colors_fonts_and_undo() {
        let mut s = Session::new();
        let list = s.execute("layout.themes", &json!({})).unwrap();
        assert_eq!(list["themes"].as_array().unwrap().len(), 6);
        assert_eq!(list["current"]["name"], "SheetPro");
        s.execute("cell.set", &json!({"cell": "A1", "input": "x"})).unwrap();
        let r = s.execute("layout.themes", &json!({"theme": "Harbor"})).unwrap();
        assert_eq!(r["changed"], true);
        assert_eq!(s.wb.theme.name, "Harbor");
        // Default-font cells moved to the body font.
        let st = s.wb.styles.get(s.wb.sheets[0].get(CellAddr::new(0, 0)).unwrap().style);
        assert_eq!(st.font_name, "Verdana");
        s.execute("layout.themes", &json!({"colors": "Ember"})).unwrap();
        assert_eq!(s.wb.theme.colors_name, "Ember");
        assert_eq!(s.wb.theme.name, "Custom");
        s.execute("layout.themes", &json!({"fonts": "Classic"})).unwrap();
        assert_eq!(s.wb.theme.minor_font, "Arial");
        let mut cols = vec!["112233"; 12];
        cols[4] = "FF0000";
        s.execute("layout.themes", &json!({"custom_colors": cols, "name": "Mine"}))
            .unwrap();
        assert_eq!(s.wb.theme.colors[4], Color(255, 0, 0));
        for _ in 0..4 {
            s.execute("edit.undo", &json!({})).unwrap();
        }
        assert_eq!(s.wb.theme.name, "SheetPro");
        let st = s.wb.styles.get(s.wb.sheets[0].get(CellAddr::new(0, 0)).unwrap().style);
        assert_eq!(st.font_name, "Calibri");
        for bad in [
            json!({"theme": "Nope"}),
            json!({"custom_colors": ["zz"]}),
            json!({"custom_colors": vec!["GGGGGG"; 12]}),
            json!({"major_font": ""}),
        ] {
            assert!(s.execute("layout.themes", &bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn theme_colored_fill_follows_theme() {
        let mut s = Session::new();
        let accent = sp_model::theme::resolve_theme_color(&s.wb.theme, 4, 0.4);
        let id = s.wb.styles.derive(sp_model::StyleId::DEFAULT, |st| {
            st.fill = Some(accent);
            st.fill_theme = Some(sp_model::theme::ThemeColor {
                idx: 4,
                tint: 400_000,
                rgb: accent,
            });
        });
        s.wb.sheets[0].set(
            CellAddr::new(0, 0),
            sp_model::Cell {
                style: id,
                ..sp_model::Cell::constant(sp_core::Value::Number(1.0))
            },
        );
        let fill = |s: &Session| {
            let sh = &s.wb.sheets[0];
            let v = sp_render::layout(&s.wb, sh, CellAddr::new(0, 0), 200.0, 100.0, 1.0, false);
            v.cells.iter().find(|d| d.addr == CellAddr::new(0, 0)).unwrap().fill
        };
        assert_eq!(fill(&s), Some(accent));
        s.execute("layout.themes", &json!({"theme": "Orchid"})).unwrap();
        let orchid = sp_model::theme::resolve_theme_color(&s.wb.theme, 4, 0.4);
        assert_ne!(orchid, accent);
        assert_eq!(fill(&s), Some(orchid));
        // A direct fill replaces the theme colour.
        s.execute("home.font.fill_color", &json!({"color": "00FF00"})).unwrap();
        assert_eq!(fill(&s), Some(Color(0, 255, 0)));
    }
}
