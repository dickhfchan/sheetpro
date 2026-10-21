//! `--full`: every oracle corpus workbook (real files saved by the target app) opened in the
//! built app, saved, and reopened; each sheet's top-left block must read back the same.

use super::Ctx;
use super::scenarios::grid_diffs;
use anyhow::Result;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;

fn xlsx_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            xlsx_files(&p, out);
        } else if p
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("xlsx"))
        {
            out.push(p);
        }
    }
}

pub fn run(c: &mut Ctx) -> Result<()> {
    let root = crate::root().join("corpus/files");
    let mut files = Vec::new();
    xlsx_files(&root, &mut files);
    c.check(
        "corpus present (run `cargo xtask corpus`)",
        files.len() >= 250,
        files.len(),
    );
    if files.is_empty() {
        return Ok(());
    }
    let broken = crate::corpus::expected_failures();
    let mut refused_badly: Vec<String> = Vec::new();
    let mut a = c.launch()?;
    let mut failed_open: Vec<String> = Vec::new();
    let mut mismatched: Vec<String> = Vec::new();
    let mut slow: Vec<(String, f64)> = Vec::new();
    let mut crashed: Vec<String> = Vec::new();
    let out = c.path("roundtrip.xlsx");
    for f in &files {
        let name = f
            .strip_prefix(&root)
            .unwrap_or(f)
            .to_string_lossy()
            .to_string();
        let t = Instant::now();
        let opened = a.try_ex("file.open", json!({"path": f}));
        if broken.contains(f) {
            // Deliberately broken: an error, and the app carries on.
            if opened.is_ok() || !a.alive() {
                refused_badly.push(name.clone());
                if !a.alive() {
                    drop(a);
                    a = c.launch()?;
                }
            }
            continue;
        }
        let secs = t.elapsed().as_secs_f64();
        if secs > 10.0 {
            slow.push((name.clone(), secs));
        }
        if let Err(e) = opened {
            if !a.alive() {
                crashed.push(name.clone());
                drop(a);
                a = c.launch()?;
            } else {
                failed_open.push(format!("{name}: {e}"));
            }
            continue;
        }
        let sheets: Vec<String> = a.state()["sheets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s.as_str().map(str::to_string))
            .collect();
        let mut before = Vec::new();
        for s in &sheets {
            a.try_ex("sheet.activate", json!({"sheet": s})).ok();
            before.push(a.get("A1:T40"));
        }
        if let Err(e) = a.try_ex("file.save_as", json!({"path": out})) {
            failed_open.push(format!("{name}: save: {e}"));
            continue;
        }
        if let Err(e) = a.try_ex("file.open", json!({"path": out})) {
            failed_open.push(format!("{name}: reopen: {e}"));
            continue;
        }
        for (s, b) in sheets.iter().zip(&before) {
            a.try_ex("sheet.activate", json!({"sheet": s})).ok();
            let d = grid_diffs(b, &a.get("A1:T40"));
            if !d.is_empty() {
                mismatched.push(format!("{name} [{s}]: {:?}", d.first()));
                break;
            }
        }
        if !a.alive() {
            crashed.push(name.clone());
            drop(a);
            a = c.launch()?;
        }
    }
    c.check(
        &format!("{} broken files are refused cleanly", broken.len()),
        refused_badly.is_empty(),
        &refused_badly,
    );
    let n = files.len() - broken.len();
    c.check(
        &format!("{n} corpus files open without a crash"),
        crashed.is_empty(),
        &crashed,
    );
    c.check(
        &format!("{n} corpus files open and save"),
        failed_open.is_empty(),
        &failed_open,
    );
    c.check(
        &format!("{n} corpus files read back the same after save and reopen"),
        mismatched.is_empty(),
        &mismatched,
    );
    c.check(
        "no corpus file takes over 10 s to open",
        slow.is_empty(),
        &slow,
    );
    a.quit(c, "exits cleanly after the corpus");
    Ok(())
}
