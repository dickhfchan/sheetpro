//! Parts that start with a UTF-8 byte-order mark (written by some Windows tools) load and
//! save cleanly. The XML reader skips the BOM and reports positions without it; slicing
//! text that kept it put every captured element 3 bytes off and corrupted workbook.xml.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use sp_core::{CellAddr, Value};
use sp_model::{Cell, Workbook};
use std::io::{Read, Write};

fn with_bom(src: &std::path::Path, dst: &std::path::Path, parts: &[&str]) {
    let mut zin = zip::ZipArchive::new(std::fs::File::open(src).unwrap()).unwrap();
    let mut zout = zip::ZipWriter::new(std::fs::File::create(dst).unwrap());
    for i in 0..zin.len() {
        let mut f = zin.by_index(i).unwrap();
        let name = f.name().to_string();
        let mut data = Vec::new();
        f.read_to_end(&mut data).unwrap();
        if parts.contains(&name.as_str()) {
            data.splice(0..0, [0xEF, 0xBB, 0xBF]);
        }
        zout.start_file(name, zip::write::SimpleFileOptions::default())
            .unwrap();
        zout.write_all(&data).unwrap();
    }
    zout.finish().unwrap();
}

#[test]
fn bom_prefixed_parts_round_trip() {
    let dir = std::env::temp_dir().join(format!("sp-bom-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut wb = Workbook::new();
    let sh = wb.sheets.first_mut().unwrap();
    sh.set(CellAddr::new(0, 0), Cell::constant(Value::Number(7.0)));
    sh.set(CellAddr::new(1, 0), Cell::formula("A1*6"));
    let plain = dir.join("plain.xlsx");
    sp_io::save(&wb, &plain).unwrap();
    let bom = dir.join("bom.xlsx");
    with_bom(
        &plain,
        &bom,
        &[
            "xl/workbook.xml",
            "xl/worksheets/sheet1.xml",
            "[Content_Types].xml",
        ],
    );

    let (loaded, _) = sp_io::load(&bom).unwrap();
    let again = dir.join("again.xlsx");
    sp_io::save(&loaded, &again).unwrap();
    let bytes = std::fs::read(&again).unwrap();
    let problems = sp_io::xlsx_passthrough::validate_package(&bytes);
    assert!(problems.is_empty(), "{problems:?}");
    let (back, _) = sp_io::load(&again).unwrap();
    let s = &back.sheets[0];
    assert_eq!(
        s.get(CellAddr::new(0, 0)).unwrap().value,
        Value::Number(7.0)
    );
    assert_eq!(
        s.get(CellAddr::new(1, 0)).unwrap().formula_src(),
        Some("A1*6")
    );
}

/// A carriage return in cell text survives saving (XML reads a raw "\r\n" as "\n").
#[test]
fn carriage_returns_in_text_round_trip() {
    let dir = std::env::temp_dir().join(format!("sp-cr-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut wb = Workbook::new();
    let text = "Rich Text\r\nTest\rend";
    wb.sheets
        .first_mut()
        .unwrap()
        .set(CellAddr::new(0, 0), Cell::constant(Value::text(text)));
    let p = dir.join("cr.xlsx");
    sp_io::save(&wb, &p).unwrap();
    let (back, _) = sp_io::load(&p).unwrap();
    assert_eq!(
        back.sheets[0].get(CellAddr::new(0, 0)).unwrap().value,
        Value::text(text)
    );
}
