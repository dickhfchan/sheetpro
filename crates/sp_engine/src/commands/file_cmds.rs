//! File commands (`file.*`).

use crate::params::{str_opt, str_req};
use crate::{CmdError, CmdResult, CommandSpec, Json, Session};
use serde_json::json;
use std::path::PathBuf;

fn open(s: &mut Session, p: &Json) -> CmdResult {
    let path = PathBuf::from(str_req(p, "path")?);
    let (wb, report) = sp_io::load(&path)?;
    let mut fresh = Session::with_workbook(wb);
    fresh.path = Some(path.clone());
    fresh.view.page_rows = s.view.page_rows;
    fresh.view.page_cols = s.view.page_cols;
    fresh.status = format!(
        "Opened {} ({} cells, {} formulas)",
        path.display(),
        report.cells,
        report.formulas
    );
    *s = fresh;
    Ok(json!({ "cells": report.cells, "formulas": report.formulas, "warnings": report.warnings }))
}

fn save_to(s: &mut Session, path: PathBuf, adopt: bool) -> CmdResult {
    sp_io::save(&s.wb, &path)?;
    if adopt {
        s.path = Some(path.clone());
        s.dirty = false;
    }
    s.status = format!("Saved {}", path.display());
    Ok(json!(path.display().to_string()))
}

fn save(s: &mut Session, p: &Json) -> CmdResult {
    match (str_opt(p, "path"), s.path.clone()) {
        (Some(pth), _) => save_to(s, PathBuf::from(pth), true),
        (None, Some(pth)) => save_to(s, pth, true),
        (None, None) => Err(CmdError::Param(
            "this workbook has no file yet: pass {\"path\": …} (Save As)".into(),
        )),
    }
}

pub fn specs() -> Vec<CommandSpec> {
    vec![
        CommandSpec::new("file.new", "{}", |s, _| {
            let (pr, pc) = (s.view.page_rows, s.view.page_cols);
            *s = Session::new();
            s.view.page_rows = pr;
            s.view.page_cols = pc;
            Ok(Json::Null)
        }),
        CommandSpec::new("file.open", r#"{"path": "/abs/book.xlsx"}"#, open),
        CommandSpec::new("file.save", r#"{"path"?: "/abs/book.xlsx"}"#, save),
        CommandSpec::new(
            "file.save_as",
            r#"{"path": "/abs/book.xlsx|.csv|.tsv"}"#,
            |s, p| {
                let path = PathBuf::from(str_req(p, "path")?);
                save_to(s, path, true)
            },
        ),
        CommandSpec::new("file.save_copy", r#"{"path": "/abs/copy.xlsx"}"#, |s, p| {
            let path = PathBuf::from(str_req(p, "path")?);
            save_to(s, path, false)
        }),
        CommandSpec::new(
            "file.export.csv",
            r#"{"path": "/abs/sheet.csv"}"#,
            |s, p| {
                let path = PathBuf::from(str_req(p, "path")?);
                if sp_io::FileKind::from_path(&path) != Some(sp_io::FileKind::Csv) {
                    return Err(CmdError::Param("path must end in .csv".into()));
                }
                save_to(s, path, false)
            },
        ),
        CommandSpec::new("file.close", r#"{"discard"?: false}"#, |s, p| {
            if s.dirty && !crate::params::bool_opt(p, "discard").unwrap_or(false) {
                return Err(CmdError::Failed(
                    "workbook has unsaved changes: save first or pass {\"discard\": true}".into(),
                ));
            }
            *s = Session::new();
            Ok(Json::Null)
        }),
        CommandSpec::new("file.exit", r#"{"discard"?: false}"#, |s, p| {
            if s.dirty && !crate::params::bool_opt(p, "discard").unwrap_or(false) {
                return Err(CmdError::Failed(
                    "workbook has unsaved changes: save first or pass {\"discard\": true}".into(),
                ));
            }
            s.quit_requested = true;
            Ok(Json::Null)
        }),
        CommandSpec::new(
            "file.properties",
            r#"{"title"?: "…", "author"?: "…"} → current properties"#,
            |s, p| {
                let (t, a) = (
                    str_opt(p, "title").map(str::to_string),
                    str_opt(p, "author").map(str::to_string),
                );
                if t.is_some() || a.is_some() {
                    s.transact("Properties", |tx, _| {
                        tx.touch_book();
                        if let Some(t) = t {
                            tx.wb.props.title = t;
                        }
                        if let Some(a) = a {
                            tx.wb.props.author = a;
                        }
                        Ok(())
                    })?;
                }
                Ok(
                    json!({ "title": s.wb.props.title, "author": s.wb.props.author, "path": s.path.as_ref().map(|p| p.display().to_string()) }),
                )
            },
        ),
    ]
}
