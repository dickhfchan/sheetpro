//! Application-level commands: recent files, options, document inspector, text import,
//! windows (`file.recent`, `file.options`, `file.inspect`, `data.get_from_text`,
//! `view.new_window`, `view.split`, `view.hide_window`, `view.switch_windows`) and
//! problem reports (`app.report_problem`).
//!
//! Settings, recent files and reports live in the app data directory
//! ([`crate::settings::app_dir`]); without one these commands fail cleanly.

use crate::params::{bool_opt, str_opt};
use crate::settings::{self, Settings};
use crate::{CmdError, CmdResult, CommandSpec, Json, Selection, Session};
use serde::Serialize;
use serde_json::json;
use sp_core::CellAddr;
use sp_model::{Cell, Content, Visibility};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------------------
// Windows and split panes (view state: never part of the file or the undo history)

/// One window onto the session's workbook (the live session fields hold the current one).
#[derive(Debug, Clone, Copy, Serialize)]
pub struct WindowView {
    /// Stable id (for the shell's viewport ids and titles: "Book1:2").
    pub id: u32,
    pub sheet: usize,
    pub sel: Selection,
    pub scroll: CellAddr,
    pub hidden: bool,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct WindowState {
    /// All windows; empty means just the one implicit window. `current` indexes this.
    pub windows: Vec<WindowView>,
    pub current: usize,
    /// Split pane origin (first row/column of the second pane) of the current view.
    pub split: Option<CellAddr>,
    /// Scroll position of the second pane.
    pub split_scroll: CellAddr,
    /// Set when the last visible window was hidden: the shell minimizes and clears it.
    pub minimize_requested: bool,
}

impl WindowState {
    pub fn count(&self) -> usize {
        self.windows.len().max(1)
    }
}

fn live_view(s: &Session, id: u32) -> WindowView {
    WindowView {
        id,
        sheet: s.wb.active,
        sel: s.sel,
        scroll: s.view.scroll,
        hidden: false,
    }
}

fn ensure_windows(s: &mut Session) {
    if s.view.window.windows.is_empty() {
        let w = live_view(s, 1);
        s.view.window.windows.push(w);
        s.view.window.current = 0;
    }
}

/// Make window `i` the current one: store the live selection/scroll/sheet into the current
/// slot and load slot `i`. Out-of-range indexes are ignored.
pub fn activate_window(s: &mut Session, i: usize) {
    ensure_windows(s);
    let cur = s.view.window.current;
    if i == cur || i >= s.view.window.windows.len() {
        return;
    }
    let live = live_view(s, 0);
    if let Some(w) = s.view.window.windows.get_mut(cur) {
        w.sheet = live.sheet;
        w.sel = live.sel;
        w.scroll = live.scroll;
    }
    if let Some(w) = s.view.window.windows.get(i).copied() {
        if w.sheet < s.wb.sheets.len() {
            s.wb.active = w.sheet;
        }
        s.sel = w.sel;
        s.view.scroll = w.scroll;
        s.view.window.current = i;
    }
}

/// Close window `i` (the shell's close button on a secondary window).
pub fn close_window(s: &mut Session, i: usize) {
    if s.view.window.windows.len() <= 1 || i >= s.view.window.windows.len() {
        return;
    }
    if i == s.view.window.current {
        let next = if i == 0 { 1 } else { 0 };
        activate_window(s, next);
    }
    s.view.window.windows.remove(i);
    if s.view.window.current > i {
        s.view.window.current -= 1;
    }
}

fn new_window(s: &mut Session, _: &Json) -> CmdResult {
    ensure_windows(s);
    if s.view.window.windows.len() >= 16 {
        return Err(CmdError::Failed("too many windows (16)".into()));
    }
    let id = s
        .view
        .window
        .windows
        .iter()
        .map(|w| w.id)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let w = live_view(s, id);
    s.view.window.windows.push(w);
    let i = s.view.window.windows.len().saturating_sub(1);
    activate_window(s, i);
    s.status = format!("New window {id}");
    Ok(json!({ "window": id, "count": s.view.window.windows.len() }))
}

fn switch_windows(s: &mut Session, p: &Json) -> CmdResult {
    ensure_windows(s);
    let n = s.view.window.windows.len();
    let target = match p.get("window").and_then(Json::as_u64) {
        Some(id) => s
            .view
            .window
            .windows
            .iter()
            .position(|w| u64::from(w.id) == id)
            .ok_or_else(|| CmdError::Param(format!("no window {id}")))?,
        None => (1..=n)
            .map(|k| (s.view.window.current + k) % n)
            .find(|&i| s.view.window.windows.get(i).is_some_and(|w| !w.hidden))
            .unwrap_or(s.view.window.current),
    };
    if let Some(w) = s.view.window.windows.get_mut(target) {
        w.hidden = false;
    }
    activate_window(s, target);
    let id = s.view.window.windows.get(target).map_or(1, |w| w.id);
    Ok(json!({ "window": id }))
}

fn hide_window(s: &mut Session, p: &Json) -> CmdResult {
    ensure_windows(s);
    if bool_opt(p, "unhide").unwrap_or(false) {
        for w in &mut s.view.window.windows {
            w.hidden = false;
        }
        return Ok(json!({ "hidden": 0 }));
    }
    let cur = s.view.window.current;
    let next = s
        .view
        .window
        .windows
        .iter()
        .enumerate()
        .find(|(i, w)| *i != cur && !w.hidden)
        .map(|(i, _)| i);
    match next {
        Some(i) => {
            if let Some(w) = s.view.window.windows.get_mut(cur) {
                w.hidden = true;
            }
            activate_window(s, i);
        }
        // The only visible window: the shell minimizes the app window instead.
        None => s.view.window.minimize_requested = true,
    }
    let hidden = s.view.window.windows.iter().filter(|w| w.hidden).count();
    Ok(json!({ "hidden": hidden }))
}

fn split(s: &mut Session, p: &Json) -> CmdResult {
    let at = match str_opt(p, "cell") {
        Some(c) => Some(crate::params::cell(s, &json!({ "cell": c }), "cell")?.1),
        None => None,
    };
    let on = match bool_opt(p, "on") {
        Some(v) => v,
        None => at.is_some() || s.view.window.split.is_none(),
    };
    if !on {
        s.view.window.split = None;
        return Ok(Json::Null);
    }
    // Default: split at the active cell; at A1, through the middle of the visible page.
    let a = at.unwrap_or(s.sel.active);
    let origin = if a.row == 0 && a.col == 0 {
        CellAddr::new(s.view.scroll.row + s.view.page_rows / 2, 0)
    } else {
        a
    };
    s.view.window.split = Some(origin);
    s.view.window.split_scroll = s.view.scroll;
    Ok(json!(origin.to_a1()))
}

// ---------------------------------------------------------------------------------------
// Recent files and options

fn recent(s: &mut Session, p: &Json) -> CmdResult {
    let op = str_opt(p, "op").unwrap_or("list");
    match op {
        "list" => {
            let st = settings::current();
            let items: Vec<Json> = st
                .recent_files
                .iter()
                .map(|f| json!({ "path": f, "exists": Path::new(f).exists() }))
                .collect();
            Ok(Json::Array(items))
        }
        "clear" => {
            settings::update(|st| {
                st.recent_files.clear();
                Ok(())
            })?;
            Ok(json!([]))
        }
        "remove" => {
            let path = crate::params::str_req(p, "path")?.to_string();
            let st = settings::update(|st| {
                st.recent_files.retain(|f| *f != path);
                Ok(())
            })?;
            Ok(json!(st.recent_files))
        }
        "open" => {
            let st = settings::current();
            let path = match (str_opt(p, "path"), p.get("index").and_then(Json::as_u64)) {
                (Some(path), _) => path.to_string(),
                (None, Some(i)) => usize::try_from(i)
                    .ok()
                    .and_then(|i| st.recent_files.get(i))
                    .cloned()
                    .ok_or_else(|| CmdError::Param(format!("no recent file #{i}")))?,
                (None, None) => st
                    .recent_files
                    .first()
                    .cloned()
                    .ok_or_else(|| CmdError::Failed("the recent files list is empty".into()))?,
            };
            let r = s.execute("file.open", &json!({ "path": path }))?;
            let _ = settings::update(|st| {
                st.push_recent(Path::new(&path));
                Ok(())
            });
            Ok(r)
        }
        other => Err(CmdError::Param(format!(
            "op must be list, open, clear or remove (got '{other}')"
        ))),
    }
}

/// Record a file in the recent list (the desktop calls this after open/save).
pub fn note_recent(path: &Path) {
    let _ = settings::update(|st| {
        st.push_recent(path);
        Ok(())
    });
}

fn options(s: &mut Session, p: &Json) -> CmdResult {
    if let Some(set) = p.get("set") {
        let obj = set
            .as_object()
            .ok_or_else(|| CmdError::Param("set must be an object of {key: value}".into()))?;
        let st = settings::update(|st| {
            for (k, v) in obj {
                st.set(k, v)?;
            }
            Ok(())
        })?;
        if obj.contains_key("calc_mode") {
            let id = if st.calc_mode == "manual" {
                "formulas.calc_manual"
            } else {
                "formulas.calc_automatic"
            };
            s.execute(id, &json!({}))?;
        }
        return Ok(st.to_json());
    }
    let st = settings::app_dir().map_or_else(Settings::default, |d| Settings::load(&d));
    match p.get("get") {
        None | Some(Json::Null) => {
            let mut v = st.to_json();
            if let Some(o) = v.as_object_mut() {
                o.insert(
                    "keys".into(),
                    settings::KEYS
                        .iter()
                        .map(|(k, d)| (k.to_string(), json!(d)))
                        .collect::<serde_json::Map<_, _>>()
                        .into(),
                );
            }
            Ok(v)
        }
        Some(Json::String(k)) => st.get(k).map_err(CmdError::Param),
        Some(Json::Array(keys)) => {
            let mut out = serde_json::Map::new();
            for k in keys {
                let k = k
                    .as_str()
                    .ok_or_else(|| CmdError::Param("get keys must be strings".into()))?;
                out.insert(k.to_string(), st.get(k).map_err(CmdError::Param)?);
            }
            Ok(Json::Object(out))
        }
        Some(_) => Err(CmdError::Param(
            "get must be a key or a list of keys".into(),
        )),
    }
}

// ---------------------------------------------------------------------------------------
// Document inspector

const EXTERNAL_LINK_REL: &str = "/externalLink";

/// `[1]Sheet1!A1`-style external workbook reference in formula text (structured refs like
/// `Table1[2020]` follow an identifier character and don't count).
pub fn has_external_ref(f: &str) -> bool {
    let b = f.as_bytes();
    let mut prev: u8 = b'(';
    let mut in_str = false;
    let mut i = 0;
    while let Some(&c) = b.get(i) {
        if c == b'"' {
            in_str = !in_str;
        } else if !in_str
            && c == b'['
            && !(prev.is_ascii_alphanumeric() || matches!(prev, b'_' | b'.' | b']'))
        {
            let digits = b
                .get(i + 1..)
                .unwrap_or_default()
                .iter()
                .take_while(|d| d.is_ascii_digit())
                .count();
            if digits > 0 && b.get(i + 1 + digits) == Some(&b']') {
                return true;
            }
        }
        prev = c;
        i += 1;
    }
    false
}

fn external_link_targets(s: &Session) -> Vec<String> {
    let pt = &s.wb.passthrough;
    let mut out: Vec<String> = Vec::new();
    for r in pt
        .rels
        .iter()
        .filter(|r| r.rel_type.ends_with(EXTERNAL_LINK_REL))
    {
        let part = pt.parts.iter().find(|p| p.path == r.target);
        let ext = part
            .into_iter()
            .flat_map(|p| p.rels.iter())
            .find(|x| x.external)
            .map(|x| x.target.clone());
        out.push(ext.unwrap_or_else(|| r.target.clone()));
    }
    out
}

fn inspect_report(s: &Session) -> Json {
    let hidden: Vec<Json> = s
        .wb
        .sheets
        .iter()
        .filter(|sh| sh.visibility != Visibility::Visible)
        .map(
            |sh| json!({ "name": sh.name, "very_hidden": sh.visibility == Visibility::VeryHidden }),
        )
        .collect();
    let mut comment_count = 0usize;
    let mut authors: Vec<String> = Vec::new();
    let mut hidden_rows = 0usize;
    let mut hidden_cols = 0usize;
    let mut external_formulas = 0usize;
    for sh in &s.wb.sheets {
        comment_count += sh.comments.len();
        for c in &sh.comments {
            for a in std::iter::once(&c.author).chain(c.replies.iter().map(|r| &r.author)) {
                if !a.is_empty() && !authors.contains(a) {
                    authors.push(a.clone());
                }
            }
        }
        hidden_rows += sh.hidden_rows.len();
        hidden_cols += sh.hidden_cols.len();
        external_formulas += sh
            .iter()
            .filter(|(_, c)| matches!(&c.content, Content::Formula(f) if has_external_ref(f)))
            .count();
    }
    let hidden_names =
        s.wb.names
            .iter()
            .filter(|n| n.name.starts_with("_xlnm") || n.name.starts_with("_xl"))
            .count();
    let has_vba =
        s.wb.passthrough
            .parts
            .iter()
            .any(|p| p.path.ends_with("vbaProject.bin"));
    let custom_xml =
        s.wb.passthrough
            .parts
            .iter()
            .filter(|p| p.path.starts_with("customXml/"))
            .count();
    json!({
        "hidden_sheets": hidden,
        "comments": { "count": comment_count, "authors": authors },
        "personal_info": {
            "author": s.wb.props.author,
            "title": s.wb.props.title,
            "comment_authors": authors.len(),
        },
        "external_links": external_link_targets(s),
        "external_formulas": external_formulas,
        "hidden_rows": hidden_rows,
        "hidden_columns": hidden_cols,
        "internal_names": hidden_names,
        "macros": has_vba,
        "custom_xml_parts": custom_xml,
        "removable": ["hidden_sheets", "comments", "personal_info", "external_links"],
    })
}

fn inspect(s: &mut Session, p: &Json) -> CmdResult {
    let remove: Vec<String> = match p.get("remove") {
        None | Some(Json::Null) => Vec::new(),
        Some(Json::String(k)) => vec![k.clone()],
        Some(Json::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(str::to_string)
                    .ok_or_else(|| CmdError::Param("remove items must be strings".into()))
            })
            .collect::<Result<_, _>>()?,
        Some(_) => return Err(CmdError::Param("remove must be a list of strings".into())),
    };
    for k in &remove {
        if ![
            "hidden_sheets",
            "comments",
            "personal_info",
            "external_links",
        ]
        .contains(&k.as_str())
        {
            return Err(CmdError::Param(format!("cannot remove '{k}'")));
        }
    }
    let has = |k: &str| remove.iter().any(|x| x == k);
    if has("hidden_sheets") {
        let hidden: Vec<String> =
            s.wb.sheets
                .iter()
                .filter(|sh| sh.visibility != Visibility::Visible)
                .map(|sh| sh.name.clone())
                .collect();
        for name in hidden {
            s.execute("home.cells.delete_sheet", &json!({ "sheet": name }))?;
        }
    }
    if has("comments") || has("personal_info") || has("external_links") {
        let (comments, personal, links) =
            (has("comments"), has("personal_info"), has("external_links"));
        s.transact("Inspect Document", |tx, _| {
            tx.touch_book();
            let wb = &mut *tx.wb;
            if personal {
                wb.props.author.clear();
                for sh in &mut wb.sheets {
                    for c in &mut sh.comments {
                        c.author.clear();
                        for r in &mut c.replies {
                            r.author.clear();
                        }
                    }
                }
            }
            if comments {
                for sh in &mut wb.sheets {
                    sh.comments.clear();
                }
            }
            if links {
                // Break links: external formulas keep their last values.
                for sh in &mut wb.sheets {
                    let ext: Vec<(CellAddr, Cell)> = sh
                        .iter()
                        .filter(|(_, c)| {
                            matches!(&c.content, Content::Formula(f) if has_external_ref(f))
                        })
                        .map(|(a, c)| {
                            let mut c = c.clone();
                            c.content = Content::Value(c.value.clone());
                            (a, c)
                        })
                        .collect();
                    for (a, c) in ext {
                        sh.set(a, c);
                    }
                }
                wb.names.retain(|n| !has_external_ref(&n.formula));
                let pt = &mut wb.passthrough;
                let targets: Vec<String> = pt
                    .rels
                    .iter()
                    .filter(|r| r.rel_type.ends_with(EXTERNAL_LINK_REL))
                    .map(|r| r.target.clone())
                    .collect();
                pt.rels.retain(|r| !r.rel_type.ends_with(EXTERNAL_LINK_REL));
                pt.parts.retain(|p| !targets.contains(&p.path));
                pt.elements.retain(|e| e.name != "externalReferences");
            }
            Ok(())
        })?;
    }
    let mut report = inspect_report(s);
    if let Some(o) = report.as_object_mut() {
        o.insert("removed".into(), json!(remove));
    }
    Ok(report)
}

// ---------------------------------------------------------------------------------------
// Import a delimited text file into a new sheet

/// Largest text file `data.get_from_text` reads.
const MAX_IMPORT_BYTES: u64 = 512 << 20;

fn delimiter(p: &Json, path: &Path) -> Result<u8, CmdError> {
    if let Some(d) = str_opt(p, "delimiter") {
        return match d {
            "tab" | "\t" | "\\t" => Ok(b'\t'),
            "comma" => Ok(b','),
            "semicolon" => Ok(b';'),
            "space" => Ok(b' '),
            "pipe" => Ok(b'|'),
            d if d.len() == 1 && d.is_ascii() && d != "\"" && d != "\n" && d != "\r" => {
                Ok(d.as_bytes().first().copied().unwrap_or(b','))
            }
            _ => Err(CmdError::Param(
                "delimiter must be one ASCII character or tab/comma/semicolon/space/pipe".into(),
            )),
        };
    }
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if matches!(ext.as_deref(), Some("tsv" | "tab")) {
        return Ok(b'\t');
    }
    // Sniff the first line: tab, then semicolon, then comma.
    let head = std::fs::File::open(path)
        .and_then(|f| {
            use std::io::Read;
            let mut buf = Vec::new();
            f.take(4096).read_to_end(&mut buf)?;
            Ok(buf)
        })
        .unwrap_or_default();
    let line: Vec<u8> = head.iter().copied().take_while(|&c| c != b'\n').collect();
    let count = |d: u8| line.iter().filter(|&&c| c == d).count();
    Ok(if count(b'\t') > 0 {
        b'\t'
    } else if count(b';') > count(b',') {
        b';'
    } else {
        b','
    })
}

fn import_sheet_name(s: &Session, path: &Path, requested: Option<&str>) -> String {
    let base: String = requested
        .map(str::to_string)
        .or_else(|| {
            path.file_stem()
                .and_then(|n| n.to_str())
                .map(str::to_string)
        })
        .unwrap_or_default()
        .chars()
        .map(|c| {
            if sp_model::SHEET_NAME_FORBIDDEN.contains(&c) || c.is_control() {
                '_'
            } else {
                c
            }
        })
        .collect::<String>()
        .trim_matches('\'')
        .trim()
        .chars()
        .take(31)
        .collect();
    if base.is_empty() {
        return s.wb.next_sheet_name();
    }
    if s.wb.check_sheet_name(&base, None).is_ok() {
        return base;
    }
    for n in 2..1000 {
        let suffix = format!(" ({n})");
        let stem: String = base.chars().take(31 - suffix.chars().count()).collect();
        let cand = format!("{stem}{suffix}");
        if s.wb.check_sheet_name(&cand, None).is_ok() {
            return cand;
        }
    }
    s.wb.next_sheet_name()
}

fn get_from_text(s: &mut Session, p: &Json) -> CmdResult {
    let path = PathBuf::from(crate::params::str_req(p, "path")?);
    let md = std::fs::metadata(&path)
        .map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))?;
    if !md.is_file() {
        return Err(CmdError::Param(format!("{} is not a file", path.display())));
    }
    if md.len() > MAX_IMPORT_BYTES {
        return Err(CmdError::Failed(
            "file is too large to import (512 MB max)".into(),
        ));
    }
    let delim = delimiter(p, &path)?;
    let name = import_sheet_name(s, &path, str_opt(p, "sheet_name"));
    let file = std::fs::File::open(&path).map_err(|e| CmdError::Failed(e.to_string()))?;
    let (wb, report) = sp_io::csv_io::read_delimited(file, delim, &name)?;
    let mut sheet = wb
        .sheets
        .into_iter()
        .next()
        .ok_or_else(|| CmdError::Failed("nothing imported".into()))?;
    sheet.name = name.clone();
    s.transact("Import Text", |tx, _| {
        tx.touch_book();
        tx.wb.sheets.push(sheet);
        tx.wb.active = tx.wb.sheets.len().saturating_sub(1);
        Ok(())
    })?;
    s.select(CellAddr::default());
    s.status = format!("Imported {} cells into '{}'", report.cells, name);
    Ok(json!({
        "sheet": name,
        "cells": report.cells,
        "delimiter": (delim as char).to_string(),
        "warnings": report.warnings,
    }))
}

// ---------------------------------------------------------------------------------------
// Problem reports and crash reports (local files only: nothing is sent anywhere)

pub fn reports_dir(app_dir: &Path) -> PathBuf {
    app_dir.join("reports")
}

/// Version, OS and architecture, one per line.
pub fn environment_text() -> String {
    let mut t = format!(
        "SheetPro {}\nos: {} {}\n",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    if cfg!(target_os = "macos")
        && let Ok(o) = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
    {
        t.push_str(&format!(
            "macos: {}\n",
            String::from_utf8_lossy(&o.stdout).trim()
        ));
    }
    t
}

/// Human-readable summary of the session (no cell contents).
pub fn session_summary(s: &Session) -> String {
    let mut t = String::new();
    t.push_str(&format!(
        "path: {}\ndirty: {}\ncalc_mode: {:?}\nactive sheet: {}\nselection: {}\n",
        s.path
            .as_ref()
            .map_or_else(|| "(none)".to_string(), |p| p.display().to_string()),
        s.dirty,
        s.wb.calc_mode,
        s.sheet_index(),
        s.sel.range().to_a1()
    ));
    for (i, sh) in s.wb.sheets.iter().enumerate() {
        t.push_str(&format!(
            "sheet {i}: {} cells, {:?}, {} comments, {} charts\n",
            sh.cell_count(),
            sh.visibility,
            sh.comments.len(),
            sh.drawings.charts.len()
        ));
    }
    t.push_str(&format!(
        "undo: {:?}\nlast command: {}\nstatus: {}\nlast recalc: {:?}\nwindows: {}\n",
        s.history.undo_label(),
        s.last_command
            .as_ref()
            .map_or_else(String::new, |(id, _)| id.clone()),
        s.status,
        s.last_recalc,
        s.view.window.count()
    ));
    t
}

fn unique_dir(base: &Path, prefix: &str) -> Result<PathBuf, String> {
    std::fs::create_dir_all(base).map_err(|e| e.to_string())?;
    let now = settings::unix_now();
    for n in 0..1000u32 {
        let d = base.join(format!("{prefix}-{now}-{n}"));
        if std::fs::create_dir(&d).is_ok() {
            return Ok(d);
        }
    }
    Err("could not create a report directory".into())
}

/// Write a problem-report bundle into `<app dir>/reports/report-<time>-<n>/`.
pub fn write_report(
    app_dir: &Path,
    s: &Session,
    note: &str,
    include_workbook: bool,
) -> Result<PathBuf, String> {
    let dir = unique_dir(&reports_dir(app_dir), "report")?;
    let mut text = environment_text();
    text.push_str(&format!("time: {}\n\n", settings::unix_now()));
    if !note.is_empty() {
        text.push_str("note:\n");
        text.push_str(note);
        text.push_str("\n\n");
    }
    text.push_str("session:\n");
    text.push_str(&session_summary(s));
    settings::write_atomic(&dir.join("report.txt"), text.as_bytes())?;
    let log = settings::log_tail(app_dir, 64 * 1024);
    settings::write_atomic(&dir.join("log-tail.txt"), log.as_bytes())?;
    if include_workbook {
        sp_io::save(&s.wb, &dir.join("workbook.xlsx")).map_err(|e| e.to_string())?;
    }
    Ok(dir)
}

/// Write a crash report (panic message + backtrace) into `<app dir>/reports/`.
pub fn write_crash_report(app_dir: &Path, message: &str) -> Result<PathBuf, String> {
    let dir = unique_dir(&reports_dir(app_dir), "crash")?;
    let mut text = environment_text();
    text.push_str(&format!(
        "time: {}\n\npanic:\n{message}\n\n",
        settings::unix_now()
    ));
    text.push_str("log tail:\n");
    text.push_str(&settings::log_tail(app_dir, 32 * 1024));
    settings::write_atomic(&dir.join("crash.txt"), text.as_bytes())?;
    Ok(dir)
}

fn report_problem(s: &mut Session, p: &Json) -> CmdResult {
    let dir = settings::require_app_dir()?;
    let note: String = str_opt(p, "note")
        .unwrap_or("")
        .chars()
        .take(10_000)
        .collect();
    let out = write_report(
        &dir,
        s,
        &note,
        bool_opt(p, "include_workbook").unwrap_or(false),
    )
    .map_err(CmdError::Failed)?;
    settings::log_line(&format!("problem report written to {}", out.display()));
    s.status = format!("Problem report saved to {}", out.display());
    Ok(json!(out.display().to_string()))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "file.recent",
            r#"{"op"?: "list"|"open"|"clear"|"remove", "index"?: 0, "path"?: "/abs/book.xlsx"}"#,
            recent,
        ),
        CommandSpec::new(
            "file.options",
            r##"{"get"?: "calc_mode" | ["key", …], "set"?: {"autosave_interval_secs": 60, "default_font": "Calibri", "default_font_size": 11, "calc_mode": "automatic", "r1c1": false, "gridline_color": "#D4D4D4", "recent_count": 10}}"##,
            options,
        ),
        CommandSpec::new(
            "file.inspect",
            r#"{"remove"?: ["hidden_sheets", "comments", "personal_info", "external_links"]}"#,
            inspect,
        ),
        CommandSpec::new(
            "data.get_from_text",
            r#"{"path": "/abs/data.csv", "delimiter"?: "," | "tab" | ";", "sheet_name"?: "Data"}"#,
            get_from_text,
        ),
        CommandSpec::new("view.new_window", "{}", new_window),
        CommandSpec::new("view.split", r#"{"cell"?: "B10", "on"?: true}"#, split),
        CommandSpec::new("view.hide_window", r#"{"unhide"?: false}"#, hide_window),
        CommandSpec::new("view.switch_windows", r#"{"window"?: 2}"#, switch_windows),
        CommandSpec::new(
            "app.report_problem",
            r#"{"note"?: "what happened", "include_workbook"?: false}"#,
            report_problem,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!(
            "sp-appcmds-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    struct AppDir(PathBuf);
    impl AppDir {
        fn new(tag: &str) -> Self {
            let d = temp_dir(tag);
            settings::set_thread_app_dir(Some(d.clone()));
            AppDir(d)
        }
    }
    impl Drop for AppDir {
        fn drop(&mut self) {
            settings::set_thread_app_dir(None);
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn persistence_commands_fail_without_app_dir() {
        settings::set_thread_app_dir(None);
        if settings::app_dir().is_some() {
            return; // SHEETPRO_HOME set in the environment
        }
        let mut s = Session::new();
        assert!(s.execute("file.recent", &json!({"op": "clear"})).is_err());
        assert!(s.execute("app.report_problem", &json!({})).is_err());
        assert!(
            s.execute("file.options", &json!({"set": {"r1c1": true}}))
                .is_err()
        );
        // Reading works (defaults).
        assert_eq!(
            s.execute("file.options", &json!({"get": "calc_mode"}))
                .unwrap(),
            json!("automatic")
        );
    }

    #[test]
    fn recent_files_list_open_clear() {
        let d = AppDir::new("recent");
        let book = d.0.join("a.xlsx");
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "7"}))
            .unwrap();
        s.execute("file.save_as", &json!({"path": book.display().to_string()}))
            .unwrap();
        note_recent(&book);
        note_recent(Path::new("/missing/b.xlsx"));
        let l = s.execute("file.recent", &json!({})).unwrap();
        assert_eq!(l[0]["path"], json!("/missing/b.xlsx"));
        assert_eq!(l[0]["exists"], json!(false));
        assert_eq!(l[1]["exists"], json!(true));
        let mut fresh = Session::new();
        fresh
            .execute("file.recent", &json!({"op": "open", "index": 1}))
            .unwrap();
        assert_eq!(fresh.path.as_deref(), Some(book.as_path()));
        // Re-opened file moves to the front.
        let l = fresh.execute("file.recent", &json!({})).unwrap();
        assert_eq!(l[0]["path"], json!(book.display().to_string()));
        assert!(
            fresh
                .execute("file.recent", &json!({"op": "open", "index": 9}))
                .is_err()
        );
        assert!(
            fresh
                .execute("file.recent", &json!({"op": "bogus"}))
                .is_err()
        );
        fresh
            .execute("file.recent", &json!({"op": "clear"}))
            .unwrap();
        assert_eq!(fresh.execute("file.recent", &json!({})).unwrap(), json!([]));
    }

    #[test]
    fn options_get_set_persist() {
        let _d = AppDir::new("opts");
        let mut s = Session::new();
        let all = s.execute("file.options", &json!({})).unwrap();
        assert_eq!(all["autosave_interval_secs"], json!(60));
        assert!(all["keys"]["r1c1"].is_string());
        let v = s
            .execute(
                "file.options",
                &json!({"set": {"calc_mode": "manual", "autosave_interval_secs": 15, "r1c1": true}}),
            )
            .unwrap();
        assert_eq!(v["calc_mode"], json!("manual"));
        assert_eq!(s.wb.calc_mode, sp_model::CalcMode::Manual);
        assert_eq!(
            s.execute(
                "file.options",
                &json!({"get": ["autosave_interval_secs", "r1c1"]})
            )
            .unwrap(),
            json!({"autosave_interval_secs": 15, "r1c1": true})
        );
        // Invalid values change nothing.
        assert!(
            s.execute("file.options", &json!({"set": {"default_font_size": 0}}))
                .is_err()
        );
        assert!(
            s.execute("file.options", &json!({"set": {"nope": 1}}))
                .is_err()
        );
        assert!(s.execute("file.options", &json!({"get": "nope"})).is_err());
        assert_eq!(settings::current().autosave_interval_secs, 15);
    }

    #[test]
    fn inspect_reports_and_removes() {
        let mut s = Session::new();
        s.execute("home.cells.insert_sheet", &json!({"name": "Secret"}))
            .unwrap();
        s.execute("cell.set", &json!({"cell": "A1", "input": "5"}))
            .unwrap();
        s.wb.sheets[0].visibility = Visibility::Hidden;
        s.wb.active = 1;
        s.wb.props.author = "Ann".into();
        s.execute("cell.set", &json!({"cell": "B1", "input": "=[1]Other!A1"}))
            .ok();
        s.wb.sheets[1].comments.push(sp_model::Comment {
            addr: CellAddr::new(0, 0),
            kind: sp_model::CommentKind::Note,
            author: "Bob".into(),
            text: "hi".into(),
            replies: Vec::new(),
            visible: false,
            resolved: false,
        });
        let r = s.execute("file.inspect", &json!({})).unwrap();
        assert_eq!(r["hidden_sheets"][0]["name"], json!("Secret"));
        assert_eq!(r["comments"]["count"], json!(1));
        assert_eq!(r["comments"]["authors"], json!(["Bob"]));
        assert_eq!(r["personal_info"]["author"], json!("Ann"));
        let r = s
            .execute(
                "file.inspect",
                &json!({"remove": ["hidden_sheets", "comments", "personal_info"]}),
            )
            .unwrap();
        assert_eq!(r["hidden_sheets"], json!([]));
        assert_eq!(r["comments"]["count"], json!(0));
        assert_eq!(s.wb.props.author, "");
        assert_eq!(s.wb.sheets.len(), 1);
        assert!(
            s.execute("file.inspect", &json!({"remove": ["cells"]}))
                .is_err()
        );
        assert!(s.execute("file.inspect", &json!({"remove": 3})).is_err());
        // Undo brings the comments back.
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets[0].comments.len(), 1);
    }

    #[test]
    fn external_ref_detection_and_break_links() {
        assert!(has_external_ref("[1]Sheet1!A1"));
        assert!(has_external_ref("SUM('[2]My Sheet'!A1:B2)"));
        assert!(!has_external_ref("Table1[2020]"));
        assert!(!has_external_ref("\"[1]\"&A1"));
        assert!(!has_external_ref("Table1[[#This Row],[Qty]]"));
        assert!(!has_external_ref("["));
        let mut s = Session::new();
        s.wb.sheets[0].set(
            CellAddr::new(0, 0),
            Cell {
                content: Content::Formula("[1]Data!A1*2".into()),
                value: sp_core::Value::Number(10.0),
                style: sp_model::StyleId::default(),
            },
        );
        s.wb.passthrough.rels.push(sp_model::passthrough::RawRel {
            id: "rId9".into(),
            rel_type:
                "http://schemas.openxmlformats.org/officeDocument/2006/relationships/externalLink"
                    .into(),
            target: "xl/externalLinks/externalLink1.xml".into(),
            external: false,
        });
        s.wb.passthrough.parts.push(sp_model::passthrough::RawPart {
            path: "xl/externalLinks/externalLink1.xml".into(),
            content_type: "x".into(),
            data: std::sync::Arc::from(&b"<externalLink/>"[..]),
            rels: vec![sp_model::passthrough::RawRel {
                id: "rId1".into(),
                rel_type: "x/externalLinkPath".into(),
                target: "other.xlsx".into(),
                external: true,
            }],
        });
        let r = s.execute("file.inspect", &json!({})).unwrap();
        assert_eq!(r["external_links"], json!(["other.xlsx"]));
        assert_eq!(r["external_formulas"], json!(1));
        let r = s
            .execute("file.inspect", &json!({"remove": ["external_links"]}))
            .unwrap();
        assert_eq!(r["external_links"], json!([]));
        assert!(s.wb.passthrough.parts.is_empty());
        let c = s.wb.sheets[0].get(CellAddr::new(0, 0)).unwrap();
        assert_eq!(c.content, Content::Value(sp_core::Value::Number(10.0)));
    }

    #[test]
    fn get_from_text_imports_new_sheet() {
        let d = temp_dir("import");
        let csv = d.join("sales data.csv");
        std::fs::write(&csv, "name;qty\nann;3\nbob;4\n").unwrap();
        let mut s = Session::new();
        let r = s
            .execute(
                "data.get_from_text",
                &json!({"path": csv.display().to_string()}),
            )
            .unwrap();
        assert_eq!(r["delimiter"], json!(";"));
        assert_eq!(r["sheet"], json!("sales data"));
        assert_eq!(s.wb.sheets.len(), 2);
        assert_eq!(s.wb.active, 1);
        assert_eq!(
            s.wb.sheets[1].value(CellAddr::new(2, 1)),
            &sp_core::Value::Number(4.0)
        );
        // Same file again: unique name; explicit delimiter.
        let r = s
            .execute(
                "data.get_from_text",
                &json!({"path": csv.display().to_string(), "delimiter": ","}),
            )
            .unwrap();
        assert_eq!(r["sheet"], json!("sales data (2)"));
        assert_eq!(
            s.wb.sheets[2].value(CellAddr::new(1, 0)),
            &sp_core::Value::text("ann;3")
        );
        let tsv = d.join("t.tsv");
        std::fs::write(&tsv, "a\tb\n1\t2\n").unwrap();
        let r = s
            .execute(
                "data.get_from_text",
                &json!({"path": tsv.display().to_string(), "sheet_name": "Bad:Name[]"}),
            )
            .unwrap();
        assert_eq!(r["sheet"], json!("Bad_Name__"));
        assert!(
            s.execute("data.get_from_text", &json!({"path": "/no/such.csv"}))
                .is_err()
        );
        assert!(
            s.execute(
                "data.get_from_text",
                &json!({"path": csv.display().to_string(), "delimiter": "ab"})
            )
            .is_err()
        );
        s.execute("edit.undo", &json!({})).unwrap();
        assert_eq!(s.wb.sheets.len(), 3);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn windows_new_switch_hide() {
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "1"}))
            .unwrap();
        s.select(CellAddr::new(4, 2));
        let r = s.execute("view.new_window", &json!({})).unwrap();
        assert_eq!(r["window"], json!(2));
        assert_eq!(s.view.window.count(), 2);
        assert_eq!(s.view.window.current, 1);
        s.select(CellAddr::new(9, 9));
        s.execute("view.switch_windows", &json!({})).unwrap();
        assert_eq!(s.view.window.current, 0);
        assert_eq!(s.sel.active, CellAddr::new(4, 2));
        s.execute("view.switch_windows", &json!({"window": 2}))
            .unwrap();
        assert_eq!(s.sel.active, CellAddr::new(9, 9));
        assert!(
            s.execute("view.switch_windows", &json!({"window": 7}))
                .is_err()
        );
        // Hide window 2: window 1 becomes current.
        s.execute("view.hide_window", &json!({})).unwrap();
        assert_eq!(s.view.window.current, 0);
        assert!(s.view.window.windows[1].hidden);
        // Hiding the last visible window asks the shell to minimize.
        s.execute("view.hide_window", &json!({})).unwrap();
        assert!(s.view.window.minimize_requested);
        s.execute("view.hide_window", &json!({"unhide": true}))
            .unwrap();
        assert!(!s.view.window.windows[1].hidden);
        close_window(&mut s, 1);
        assert_eq!(s.view.window.count(), 1);
        close_window(&mut s, 0); // last window cannot close this way
        assert_eq!(s.view.window.count(), 1);
    }

    #[test]
    fn split_toggles() {
        let mut s = Session::new();
        assert!(s.execute("view.split", &json!({})).is_ok());
        let first = s.view.window.split;
        assert!(first.is_some_and(|a| a.row > 0));
        s.execute("view.split", &json!({})).unwrap();
        assert!(s.view.window.split.is_none());
        assert_eq!(
            s.execute("view.split", &json!({"cell": "C5"})).unwrap(),
            json!("C5")
        );
        s.execute("view.split", &json!({"on": false})).unwrap();
        assert!(s.view.window.split.is_none());
        assert!(s.execute("view.split", &json!({"cell": "nope"})).is_err());
    }

    #[test]
    fn report_bundle_written_locally() {
        let d = AppDir::new("report");
        settings::log_line("something happened");
        let mut s = Session::new();
        s.execute("cell.set", &json!({"cell": "A1", "input": "1"}))
            .unwrap();
        let out = s
            .execute(
                "app.report_problem",
                &json!({"note": "grid froze", "include_workbook": true}),
            )
            .unwrap();
        let dir = PathBuf::from(out.as_str().unwrap());
        assert!(dir.starts_with(d.0.join("reports")));
        let text = std::fs::read_to_string(dir.join("report.txt")).unwrap();
        assert!(text.contains("grid froze") && text.contains("dirty: true"));
        assert!(
            std::fs::read_to_string(dir.join("log-tail.txt"))
                .unwrap()
                .contains("something happened")
        );
        assert!(dir.join("workbook.xlsx").exists());
        let crash = write_crash_report(&d.0, "boom at x.rs:1").unwrap();
        assert!(
            std::fs::read_to_string(crash.join("crash.txt"))
                .unwrap()
                .contains("boom")
        );
    }
}
