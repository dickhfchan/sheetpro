//! Printing and PDF (`file.export.pdf`, `file.print`, `insert.header_footer`) and the
//! paper-oriented views (`view.page_break_preview`, `view.page_layout`,
//! `view.ruler`); `view.normal` (in `view_cmds`) switches back to the normal view.
//!
//! Pagination and page layout live in `sp_render::paginate`; this module resolves chart and
//! sparkline data, fills in header/footer fields, and hands page scenes to
//! `sp_io::pdf` or to the shell's print preview.

use crate::commands::chart_cmds::{chart_data, sparkline_values};
use crate::params::{bool_opt, parse_ref, str_opt, u32_opt};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde::Serialize;
use serde_json::json;
use sp_core::CellRange;
use sp_core::date::{now_serial, serial_to_date, serial_to_time};
use sp_model::{Visibility, Workbook};
use sp_render::paginate::{
    self, HfContext, MAX_PAGES, PageScene, Pagination, PrintData, PrintOptions,
};
use std::path::{Path, PathBuf};

/// How the sheet area is shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum ViewMode {
    #[default]
    Normal,
    /// Grid with blue page boundaries and "Page N" watermarks.
    PageBreakPreview,
    /// Paper view (the shell shows page layout with margins and header/footer).
    PageLayout,
}

/// Print-related view state the shell draws (lives in `ViewState::print`).
#[derive(Debug, Clone, Serialize)]
pub struct PrintView {
    pub mode: ViewMode,
    /// Rulers in Page Layout view.
    pub ruler: bool,
    /// The print preview (Backstage › Print) is open.
    pub preview: bool,
    /// Page shown in the preview (0-based).
    pub preview_page: usize,
    /// Preview zoom (1.0 = whole page fits).
    pub preview_zoom: f32,
    /// Which sheets the preview shows: "active", "all" or "selection".
    pub preview_sheets: String,
}

impl Default for PrintView {
    fn default() -> Self {
        PrintView {
            mode: ViewMode::Normal,
            ruler: true,
            preview: false,
            preview_page: 0,
            preview_zoom: 1.0,
            preview_sheets: "active".into(),
        }
    }
}

/// Pages of a print job across one or more sheets.
pub struct PrintJob {
    /// One pagination (and resolved chart data) per printed sheet.
    pub sheets: Vec<(Pagination, PrintData)>,
    /// Job order: (index into `sheets`, page index in that pagination).
    pub pages: Vec<(usize, usize)>,
    pub file: String,
    pub path: String,
    pub date: String,
    pub time: String,
}

impl PrintJob {
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Lay out job page `i` (0-based); page numbers run across the whole job.
    pub fn scene(&self, wb: &Workbook, i: usize) -> Option<PageScene> {
        let (si, pi) = *self.pages.get(i)?;
        let (pg, data) = self.sheets.get(si)?;
        let page = pg.pages.get(pi)?;
        let ctx = HfContext {
            page: i + 1,
            pages: self.pages.len(),
            date: self.date.clone(),
            time: self.time.clone(),
            file: self.file.clone(),
            path: self.path.clone(),
            sheet: wb
                .sheet(pg.sheet)
                .map(|s| s.name.clone())
                .unwrap_or_default(),
        };
        Some(paginate::page_scene(wb, pg, page, data, &ctx))
    }
}

/// Chart and sparkline data for printing sheet `si`.
pub fn print_data(wb: &Workbook, si: usize) -> PrintData {
    let Some(sheet) = wb.sheet(si) else {
        return PrintData::default();
    };
    PrintData {
        charts: sheet
            .drawings
            .charts
            .iter()
            .map(|c| chart_data(wb, si, c))
            .collect(),
        sparklines: sheet
            .drawings
            .sparklines
            .iter()
            .map(|g| {
                g.items
                    .iter()
                    .map(|sp| sparkline_values(wb, si, sp))
                    .collect()
            })
            .collect(),
    }
}

fn today() -> (String, String) {
    let n = now_serial();
    let date = serial_to_date(n).map_or_else(String::new, |(y, m, d)| format!("{m}/{d}/{y}"));
    let (h, mi, _) = serial_to_time(n);
    let (h12, ap) = match h {
        0 => (12, "AM"),
        1..=11 => (h, "AM"),
        12 => (12, "PM"),
        _ => (h - 12, "PM"),
    };
    (date, format!("{h12}:{mi:02} {ap}"))
}

/// Build the job for `which` = "active" | "all" | "selection"; `range` (sheet, range)
/// overrides the area on that sheet.
pub fn print_job(
    s: &Session,
    which: &str,
    range: Option<(usize, CellRange)>,
) -> Result<PrintJob, CmdError> {
    let active = s.sheet_index();
    let (sheet_ids, ranges): (Vec<usize>, Option<Vec<CellRange>>) = match (range, which) {
        (Some((si, r)), _) => (vec![si], Some(vec![r])),
        (None, "active") => (vec![active], None),
        (None, "selection") => (vec![active], Some(vec![s.sel.range()])),
        (None, "all") => (
            s.wb.sheets
                .iter()
                .enumerate()
                .filter(|(_, sh)| sh.visibility == Visibility::Visible)
                .map(|(i, _)| i)
                .collect(),
            None,
        ),
        (None, o) => {
            return Err(CmdError::Param(format!(
                "unknown 'sheets' value '{o}' (active, all, selection)"
            )));
        }
    };
    let opts = PrintOptions {
        ranges,
        ignore_print_area: false,
    };
    let mut job = PrintJob {
        sheets: Vec::new(),
        pages: Vec::new(),
        file: String::new(),
        path: String::new(),
        date: String::new(),
        time: String::new(),
    };
    for si in sheet_ids {
        let pg = paginate::paginate(&s.wb, si, &opts);
        if pg.pages.is_empty() {
            continue;
        }
        let idx = job.sheets.len();
        for pi in 0..pg.pages.len() {
            if job.pages.len() < MAX_PAGES {
                job.pages.push((idx, pi));
            }
        }
        job.sheets.push((pg, print_data(&s.wb, si)));
    }
    let path = s.path.as_deref().unwrap_or(Path::new("Book1.xlsx"));
    job.file = path
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_default();
    job.path = path
        .parent()
        .map(|p| {
            let t = p.to_string_lossy().into_owned();
            if t.is_empty() { t } else { format!("{t}/") }
        })
        .unwrap_or_default();
    (job.date, job.time) = today();
    Ok(job)
}

fn job_from_params(s: &Session, p: &Json) -> Result<PrintJob, CmdError> {
    let range = match str_opt(p, "range") {
        Some(t) => Some(parse_ref(s, t)?),
        None => None,
    };
    let which = str_opt(p, "sheets").unwrap_or("active");
    print_job(s, which, range)
}

fn summary(s: &Session, job: &PrintJob) -> Json {
    json!({
        "pages": job.len(),
        "sheets": job.sheets.iter().map(|(pg, _)| json!({
            "sheet": s.wb.sheet(pg.sheet).map(|sh| sh.name.clone()),
            "pages": pg.pages.len(),
            "scale": (pg.scale * 100.0).round(),
            "paper": [pg.width, pg.height],
            "areas": pg.areas.iter().map(|a| a.to_a1()).collect::<Vec<_>>(),
        })).collect::<Vec<_>>(),
    })
}

/// Write `job` as a PDF at `path`.
pub fn write_job_pdf(s: &Session, job: &PrintJob, path: &Path) -> Result<usize, CmdError> {
    let title = job
        .file
        .rsplit_once('.')
        .map_or(job.file.as_str(), |(stem, _)| stem)
        .to_string();
    let bytes =
        sp_io::pdf::write_pdf_pages((0..job.len()).filter_map(|i| job.scene(&s.wb, i)), &title);
    std::fs::write(path, bytes)
        .map_err(|e| CmdError::Failed(format!("{}: {e}", path.display())))?;
    Ok(job.len())
}

fn pdf_path(p: &Json) -> Result<PathBuf, CmdError> {
    let path = PathBuf::from(
        str_opt(p, "path")
            .ok_or_else(|| CmdError::Param("missing string 'path' (…/book.pdf)".into()))?,
    );
    let is_pdf = path
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("pdf"));
    if !is_pdf {
        return Err(CmdError::Param("path must end in .pdf".into()));
    }
    Ok(path)
}

fn export_pdf(s: &mut Session, p: &Json) -> CmdResult {
    let path = pdf_path(p)?;
    let job = job_from_params(s, p)?;
    if job.is_empty() {
        return Err(CmdError::Failed(
            "Nothing to print: the area is empty.".into(),
        ));
    }
    let n = write_job_pdf(s, &job, &path)?;
    s.status = format!("Exported {n} page(s) to {}", path.display());
    let mut out = summary(s, &job);
    if let Some(o) = out.as_object_mut() {
        o.insert("path".into(), json!(path.display().to_string()));
    }
    Ok(out)
}

/// `file.print`: paginates exactly like `file.export.pdf` and opens the print preview (the
/// shell draws the page scenes). There is **no OS print-spooler integration**: headless, the
/// job is returned as a page summary and, with `path`, written as a PDF; to print on paper,
/// export the PDF and print it from the system viewer.
fn print(s: &mut Session, p: &Json) -> CmdResult {
    let job = job_from_params(s, p)?;
    let mut out = summary(s, &job);
    if str_opt(p, "path").is_some() {
        let path = pdf_path(p)?;
        if job.is_empty() {
            return Err(CmdError::Failed(
                "Nothing to print: the area is empty.".into(),
            ));
        }
        write_job_pdf(s, &job, &path)?;
        if let Some(o) = out.as_object_mut() {
            o.insert("path".into(), json!(path.display().to_string()));
        }
    }
    let pv = &mut s.view.print;
    pv.preview = !bool_opt(p, "close").unwrap_or(false);
    pv.preview_sheets = str_opt(p, "sheets").unwrap_or("active").to_string();
    if let Some(n) = u32_opt(p, "page")? {
        pv.preview_page = (n as usize).saturating_sub(1);
    }
    pv.preview_page = pv.preview_page.min(job.len().saturating_sub(1));
    Ok(out)
}

fn set_mode(s: &mut Session, mode: ViewMode) -> CmdResult {
    s.view.print.mode = mode;
    Ok(json!(format!("{mode:?}")))
}

fn ruler(s: &mut Session, p: &Json) -> CmdResult {
    let on = bool_opt(p, "on").unwrap_or(!s.view.print.ruler);
    s.view.print.ruler = on;
    Ok(json!(on))
}

/// Longest header/footer text accepted (the file format's limit).
const HF_MAX: usize = 255;

fn header_footer(s: &mut Session, p: &Json) -> CmdResult {
    let sheet = s.sheet_index();
    let fields = [
        "header",
        "footer",
        "first_header",
        "first_footer",
        "even_header",
        "even_footer",
    ];
    let mut texts: Vec<(&str, String)> = Vec::new();
    for k in fields {
        match p.get(k) {
            None | Some(Json::Null) => {}
            Some(Json::String(t)) => {
                if t.chars().count() > HF_MAX {
                    return Err(CmdError::Param(format!(
                        "'{k}' is longer than {HF_MAX} characters"
                    )));
                }
                texts.push((k, t.clone()));
            }
            Some(_) => return Err(CmdError::Param(format!("'{k}' must be a string"))),
        }
    }
    let flags: Vec<(&str, bool)> = [
        "different_first",
        "different_odd_even",
        "scale_with_doc",
        "align_with_margins",
    ]
    .into_iter()
    .filter_map(|k| bool_opt(p, k).map(|b| (k, b)))
    .collect();
    if !texts.is_empty() || !flags.is_empty() {
        s.transact("Header & Footer", |tx, _| {
            tx.touch_sheet(sheet);
            let sh = tx
                .wb
                .sheet_mut(sheet)
                .ok_or_else(|| CmdError::Failed("no sheet".into()))?;
            let hf = &mut sh.page_setup.header_footer;
            for (k, t) in &texts {
                let slot = match *k {
                    "header" => &mut hf.odd_header,
                    "footer" => &mut hf.odd_footer,
                    "first_header" => &mut hf.first_header,
                    "first_footer" => &mut hf.first_footer,
                    "even_header" => &mut hf.even_header,
                    _ => &mut hf.even_footer,
                };
                slot.clone_from(t);
            }
            for (k, b) in &flags {
                match *k {
                    "different_first" => hf.different_first = *b,
                    "different_odd_even" => hf.different_odd_even = *b,
                    "scale_with_doc" => hf.scale_with_doc = *b,
                    _ => hf.align_with_margins = *b,
                }
            }
            Ok(())
        })?;
    }
    // Editing headers happens in Page Layout view.
    s.view.print.mode = ViewMode::PageLayout;
    let hf =
        s.wb.sheet(sheet)
            .map(|sh| sh.page_setup.header_footer.clone())
            .unwrap_or_default();
    Ok(json!({
        "header": hf.odd_header, "footer": hf.odd_footer,
        "first_header": hf.first_header, "first_footer": hf.first_footer,
        "even_header": hf.even_header, "even_footer": hf.even_footer,
        "different_first": hf.different_first, "different_odd_even": hf.different_odd_even,
        "scale_with_doc": hf.scale_with_doc, "align_with_margins": hf.align_with_margins,
    }))
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new(
            "file.export.pdf",
            r#"{"path": "/abs/out.pdf", "range"?: "Sheet1!A1:F40", "sheets"?: "active|all|selection"}"#,
            export_pdf,
        ),
        CommandSpec::new(
            "file.print",
            r#"{"sheets"?: "active|all|selection", "range"?, "page"?: 1, "path"?: "/abs/out.pdf" (also export), "close"?: false} → page summary; opens the print preview. No OS print spooler: use "path" to get the PDF"#,
            print,
        ),
        CommandSpec::new("view.page_break_preview", "{}", |s, _| {
            set_mode(s, ViewMode::PageBreakPreview)
        }),
        CommandSpec::new("view.page_layout", "{}", |s, _| {
            set_mode(s, ViewMode::PageLayout)
        }),
        CommandSpec::new("view.ruler", r#"{"on"?: true} (default toggles)"#, ruler),
        CommandSpec::new(
            "insert.header_footer",
            r#"{"header"?: "&L&A&CPage &P of &N&R&D", "footer"?, "first_header"?, "first_footer"?, "even_header"?, "even_footer"?, "different_first"?, "different_odd_even"?, "scale_with_doc"?, "align_with_margins"?} → current texts (codes: &P &N &D &T &F &Z &A &B &I &U &"Font,Style" &nn &KRRGGBB &L &C &R)"#,
            header_footer,
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn sample() -> Session {
        let mut s = Session::new();
        for r in 1..=120 {
            let _ = s.execute(
                "cell.set",
                &json!({"cell": format!("A{r}"), "input": format!("Item {r}")}),
            );
            let _ = s.execute(
                "cell.set",
                &json!({"cell": format!("B{r}"), "input": format!("{}", r * 3)}),
            );
        }
        s
    }

    fn tmp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sp_print_{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        dir.join(name)
    }

    #[test]
    fn export_pdf_writes_valid_file() {
        let mut s = sample();
        let path = tmp("out.pdf");
        let r = s
            .execute(
                "insert.header_footer",
                &json!({"footer": "&CPage &P of &N"}),
            )
            .unwrap_or_default();
        assert_eq!(r["footer"], "&CPage &P of &N");
        assert_eq!(s.view.print.mode, ViewMode::PageLayout);
        let out = s
            .execute(
                "file.export.pdf",
                &json!({"path": path.display().to_string()}),
            )
            .unwrap_or_else(|e| panic!("{e:?}"));
        assert_eq!(out["pages"], 3);
        let bytes = std::fs::read(&path).unwrap_or_default();
        let sum = sp_io::pdf::validate(&bytes).unwrap_or_else(|e| panic!("{e}"));
        assert_eq!(sum.pages, 3);
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("(Page 2 of 3) Tj"));
        assert!(text.contains("(Item 120) Tj"));
        // Undo restores the empty footer.
        let _ = s.execute("edit.undo", &json!({}));
        assert!(
            s.wb.sheets[0]
                .page_setup
                .header_footer
                .odd_footer
                .is_empty()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn export_params_and_errors() {
        let mut s = sample();
        assert!(s.execute("file.export.pdf", &json!({})).is_err());
        assert!(
            s.execute("file.export.pdf", &json!({"path": "/tmp/x.xlsx"}))
                .is_err()
        );
        let path = tmp("sel.pdf");
        let p = path.display().to_string();
        let out = s
            .execute("file.export.pdf", &json!({"path": p, "range": "A1:B10"}))
            .unwrap_or_default();
        assert_eq!(out["pages"], 1);
        assert!(
            s.execute("file.export.pdf", &json!({"path": p, "sheets": "nope"}))
                .is_err()
        );
        let _ = s.execute("home.cells.insert_sheet", &json!({"name": "Two"}));
        let _ = s.execute("cell.set", &json!({"cell": "C3", "input": "x"}));
        let out = s
            .execute("file.export.pdf", &json!({"path": p, "sheets": "all"}))
            .unwrap_or_default();
        assert_eq!(out["pages"], 4);
        let mut empty = Session::new();
        assert!(
            empty
                .execute("file.export.pdf", &json!({"path": p}))
                .is_err()
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn print_preview_and_views() {
        let mut s = sample();
        let out = s
            .execute("file.print", &json!({"page": 2}))
            .unwrap_or_default();
        assert_eq!(out["pages"], 3);
        assert!(s.view.print.preview);
        assert_eq!(s.view.print.preview_page, 1);
        let _ = s.execute("file.print", &json!({"close": true}));
        assert!(!s.view.print.preview);
        let _ = s.execute("view.page_break_preview", &json!({}));
        assert_eq!(s.view.print.mode, ViewMode::PageBreakPreview);
        let _ = s.execute("view.normal", &json!({}));
        assert_eq!(s.view.print.mode, ViewMode::Normal);
        let _ = s.execute("view.page_layout", &json!({}));
        assert_eq!(s.view.print.mode, ViewMode::PageLayout);
        let on = s.view.print.ruler;
        let _ = s.execute("view.ruler", &json!({}));
        assert_eq!(s.view.print.ruler, !on);
        let job = print_job(&s, "active", None).unwrap_or_else(|e| panic!("{e:?}"));
        let sc = job.scene(&s.wb, 0).unwrap_or_else(|| panic!("scene"));
        assert!(sc.texts().contains(&"Item 1".to_string()));
        assert!(job.scene(&s.wb, 99).is_none());
    }
}
