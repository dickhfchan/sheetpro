//! Every corpus file (corpus/files, fetched by `cargo xtask corpus`; skipped when absent):
//! load → save must give a well-formed package (content types, relationship targets,
//! `r:id`s, schema element order), and every part we don't model must survive
//! byte-identical — also after a second load/save cycle.
#![allow(clippy::unwrap_used, clippy::panic, clippy::indexing_slicing)]

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};

fn corpus_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../corpus/files");
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(d) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&d) else {
            continue;
        };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                // corpus/files/poi: real-world compatibility files, some deliberately
                // broken; `cargo xtask test-corpus` checks those (package validity included).
                if p.file_name().is_some_and(|n| n == "poi") {
                    continue;
                }
                stack.push(p);
            } else if p.extension().is_some_and(|x| x == "xlsx" || x == "xlsm") {
                out.push(p);
            }
        }
    }
    out.sort();
    out
}

fn parts(bytes: &[u8]) -> HashMap<String, Vec<u8>> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
    let mut out = HashMap::new();
    for i in 0..z.len() {
        let mut f = z.by_index(i).unwrap();
        if f.is_dir() {
            continue;
        }
        let mut b = Vec::new();
        f.read_to_end(&mut b).unwrap();
        out.insert(f.name().to_string(), b);
    }
    out
}

/// Parts the writer regenerates from the model or deliberately drops.
fn regenerated(name: &str, content_type: &str) -> bool {
    const GEN: &[&str] = &[
        "spreadsheetml.worksheet+xml",
        "spreadsheetml.sheet.main+xml",
        "sheet.macroEnabled.main+xml",
        "spreadsheetml.styles+xml",
        "spreadsheetml.sharedStrings+xml",
        "spreadsheetml.calcChain+xml",
        "spreadsheetml.sheetMetadata+xml",
        "core-properties+xml",
        "extended-properties+xml",
        "spreadsheetml.comments+xml",
        "threadedcomments+xml",
        // The people of threaded comments, written from the comments' authors.
        "person+xml",
        "vmlDrawing",
        "printerSettings",
        "spreadsheetml.table+xml",
        // Drawings holding only modelled charts are regenerated from `Sheet::drawings`
        // (style/colour presets dropped); other drawings still pass through.
        "drawing+xml",
        "drawingml.chart+xml",
        "chartex+xml",
        "chartstyle+xml",
        "chartcolorstyle+xml",
        // Adopted pictures and backgrounds are re-stored under fresh `xl/media` names;
        // a checkbox-only feature property bag is regenerated.
        "image/",
        "featurepropertybag+xml",
    ];
    name.ends_with(".rels")
        || name == "[Content_Types].xml"
        || name.starts_with("xl/richData/")
        || GEN.iter().any(|g| content_type.contains(g))
}

fn save(wb: &sp_model::Workbook) -> Vec<u8> {
    let mut buf = std::io::Cursor::new(Vec::new());
    sp_io::xlsx_write::write_xlsx(wb, &mut buf).unwrap();
    buf.into_inner()
}

#[test]
fn corpus_packages_are_valid_and_keep_unmodelled_parts() {
    let files = corpus_files();
    if files.is_empty() {
        eprintln!("corpus not fetched; skipping");
        return;
    }
    let mut failures = Vec::new();
    for f in &files {
        let orig = std::fs::read(f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let (wb, _) = match sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(orig.clone())) {
            Ok(x) => x,
            Err(e) => {
                failures.push(format!("{name}: load failed: {e}"));
                continue;
            }
        };
        let out = save(&wb);
        for p in sp_io::xlsx_passthrough::validate_package(&out) {
            failures.push(format!("{name}: {p}"));
        }
        let o = parts(&orig);
        let ct_xml = String::from_utf8_lossy(o.get("[Content_Types].xml").unwrap()).to_string();
        let ct = sp_io::xlsx_passthrough::ContentTypes::parse(&ct_xml).unwrap();
        let saved = parts(&out);
        for (pname, bytes) in &o {
            let t = ct.get(pname).unwrap_or("");
            if regenerated(pname, t) {
                continue;
            }
            match saved.get(pname) {
                Some(b) if b == bytes => {}
                Some(_) => failures.push(format!("{name}: {pname} changed")),
                None => failures.push(format!("{name}: {pname} dropped")),
            }
        }
        // Second cycle: what we wrote loads and saves to the same unmodelled parts.
        let (wb2, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(out.clone())).unwrap();
        let out2 = save(&wb2);
        for p in sp_io::xlsx_passthrough::validate_package(&out2) {
            failures.push(format!("{name} (2nd save): {p}"));
        }
        let saved2 = parts(&out2);
        for (pname, bytes) in &saved {
            if pname.ends_with(".rels") || pname.contains("worksheets/") {
                continue;
            }
            if !regenerated(pname, ct.get(pname).unwrap_or("")) && saved2.get(pname) != Some(bytes)
            {
                failures.push(format!("{name} (2nd save): {pname} not stable"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} problems in {} files:\n{}",
        failures.len(),
        files.len(),
        failures.join("\n")
    );
    eprintln!("{} corpus packages valid", files.len());
}

/// Sheet-level edits (duplicate, rename, reorder, delete) never produce an invalid package:
/// copied parts are renamed, duplicated tables dropped, deleted sheets take their parts along.
#[test]
fn corpus_packages_stay_valid_after_sheet_edits() {
    let files = corpus_files();
    let mut failures = Vec::new();
    for f in &files {
        let orig = std::fs::read(f).unwrap();
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let Ok((mut wb, _)) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(orig)) else {
            continue;
        };
        // Duplicate every sheet, rename the copies, reverse the order, delete the first.
        let copies: Vec<sp_model::Sheet> = wb
            .sheets
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let mut c = s.clone();
                c.name = format!("Copy{i}");
                c
            })
            .collect();
        wb.sheets.extend(copies);
        wb.sheets.reverse();
        wb.sheets.remove(0);
        wb.active = 0;
        wb.names.clear();
        let out = save(&wb);
        for p in sp_io::xlsx_passthrough::validate_package(&out) {
            failures.push(format!("{name}: {p}"));
        }
        let (back, _) = sp_io::xlsx_read::read_xlsx(std::io::Cursor::new(out)).unwrap();
        if back.sheets.len() != wb.sheets.len() {
            failures.push(format!("{name}: sheet count changed"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
