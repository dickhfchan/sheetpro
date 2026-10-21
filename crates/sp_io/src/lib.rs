//! L4 import/export: .xlsx and delimited text.

pub mod csv_io;
pub mod pdf;
pub mod xlsx_cf;
pub mod xlsx_charts;
pub mod xlsx_drawings;
pub mod xlsx_extras;
pub mod xlsx_passthrough;
pub mod xlsx_pivots;
pub mod xlsx_print;
pub mod xlsx_read;
pub mod xlsx_theme;
pub mod xlsx_write;
pub mod xml;

use sp_model::Workbook;
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum IoError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("zip error: {0}")]
    Zip(String),
    #[error("XML error: {0}")]
    Xml(String),
    #[error("unsupported or corrupt file: {0}")]
    Format(String),
}

/// What a load found, for the status bar and the corpus gate.
#[derive(Debug, Default, Clone)]
pub struct LoadReport {
    pub cells: usize,
    pub formulas: usize,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Xlsx,
    Csv,
    Tsv,
}

impl FileKind {
    pub fn from_path(p: &Path) -> Option<FileKind> {
        match p.extension()?.to_str()?.to_ascii_lowercase().as_str() {
            "xlsx" | "xlsm" => Some(FileKind::Xlsx),
            "csv" => Some(FileKind::Csv),
            "tsv" | "tab" | "txt" => Some(FileKind::Tsv),
            _ => None,
        }
    }
}

/// The zip entry for part `name`: exact, else the same name with `\` as `/` and any case
/// (some tools write `xl\workbook.xml` or `[content_types].xml`; the target app opens them).
pub(crate) fn entry_name<R: std::io::Read + std::io::Seek>(
    zip: &zip::ZipArchive<R>,
    name: &str,
) -> Option<String> {
    if zip.index_for_name(name).is_some() {
        return Some(name.to_string());
    }
    zip.file_names()
        .find(|n| n.replace('\\', "/").eq_ignore_ascii_case(name))
        .map(str::to_string)
}

/// Why an .xlsx path is not a readable workbook, in words for the user (checked before
/// unzipping): empty, an encrypted workbook, or an Excel 97-2003 file.
fn not_a_workbook(path: &Path) -> Option<String> {
    use std::io::Read;
    let len = std::fs::metadata(path).ok()?.len();
    if len == 0 {
        return Some("The file is empty.".into());
    }
    let mut head = [0u8; 8];
    std::fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if head != [0xD0, 0xCF, 0x11, 0xE0, 0xA1, 0xB1, 0x1A, 0xE1] {
        return None;
    }
    // An OLE2 container: an encrypted workbook keeps an "EncryptedPackage" stream.
    let bytes = std::fs::read(path).ok().filter(|_| len <= 256 << 20)?;
    let name: Vec<u8> = "EncryptedPackage"
        .encode_utf16()
        .flat_map(u16::to_le_bytes)
        .collect();
    Some(if bytes.windows(name.len()).any(|w| w == name) {
        "This workbook is protected with a password (encrypted). SheetPro can't open \
         password-protected workbooks yet."
            .into()
    } else {
        "This is an Excel 97-2003 workbook (.xls) with an .xlsx name. SheetPro opens .xlsx \
         workbooks only."
            .into()
    })
}

pub fn load(path: &Path) -> Result<(Workbook, LoadReport), IoError> {
    let kind = FileKind::from_path(path)
        .ok_or_else(|| IoError::Format(format!("unknown file type: {}", path.display())))?;
    if kind == FileKind::Xlsx
        && let Some(why) = not_a_workbook(path)
    {
        return Err(IoError::Format(why));
    }
    let file = std::fs::File::open(path)?;
    match kind {
        FileKind::Xlsx => {
            xlsx_read::read_xlsx(std::io::BufReader::new(file)).map_err(|e| match e {
                // A zip without its directory: cut short while saving or downloading.
                IoError::Zip(m) if m.contains("EOCD") || m.contains("Central Directory") => {
                    IoError::Format(
                        "The file is damaged or incomplete (its zip directory is missing).".into(),
                    )
                }
                other => other,
            })
        }
        FileKind::Csv => csv_io::read_delimited(file, b',', &sheet_name_for(path)),
        FileKind::Tsv => csv_io::read_delimited(file, b'\t', &sheet_name_for(path)),
    }
}

/// Save atomically: write to a sibling temp file, then rename over the target.
pub fn save(wb: &Workbook, path: &Path) -> Result<(), IoError> {
    let kind = FileKind::from_path(path)
        .ok_or_else(|| IoError::Format(format!("unknown file type: {}", path.display())))?;
    let tmp = path.with_extension(format!(
        "{}.sp-tmp",
        path.extension().and_then(|e| e.to_str()).unwrap_or("tmp")
    ));
    {
        let file = std::fs::File::create(&tmp)?;
        let mut w = std::io::BufWriter::new(file);
        let r = match kind {
            FileKind::Xlsx => xlsx_write::write_xlsx(wb, &mut w),
            FileKind::Csv => csv_io::write_delimited(wb, wb.active, b',', &mut w),
            FileKind::Tsv => csv_io::write_delimited(wb, wb.active, b'\t', &mut w),
        };
        if let Err(e) = r {
            let _ = std::fs::remove_file(&tmp);
            return Err(e);
        }
        std::io::Write::flush(&mut w)?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn sheet_name_for(path: &Path) -> String {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("Sheet1");
    let clean: String = stem
        .chars()
        .filter(|c| !sp_model::SHEET_NAME_FORBIDDEN.contains(c))
        .take(31)
        .collect();
    if clean.trim().is_empty() {
        "Sheet1".into()
    } else {
        clean
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::{CellAddr, Value};
    use sp_model::Cell;

    #[test]
    fn xlsx_roundtrip_in_memory() {
        let mut wb = Workbook::new();
        let bold = wb.styles.derive(sp_model::StyleId::DEFAULT, |s| {
            s.bold = true;
            s.num_fmt = "0.00%".into();
        });
        let s = &mut wb.sheets[0];
        s.set(CellAddr::new(0, 0), Cell::constant(Value::Number(1.5)));
        s.set(CellAddr::new(0, 1), Cell::constant(Value::text("a & <b>")));
        let mut f = Cell::formula("CONCAT(A1,\"x\")");
        f.value = Value::text("1.5x");
        f.style = bold;
        s.set(CellAddr::new(1, 0), f);
        s.set(CellAddr::new(2, 0), Cell::constant(Value::Bool(true)));
        s.merges.push(sp_core::CellRange::parse("C1:D2").unwrap());
        wb.sheets.push(sp_model::Sheet::new("Second Sheet"));
        wb.names.push(sp_model::DefinedName {
            name: "Rate".into(),
            formula: "Sheet1!$A$1".into(),
            scope: None,
        });

        let mut buf = std::io::Cursor::new(Vec::new());
        xlsx_write::write_xlsx(&wb, &mut buf).unwrap();
        buf.set_position(0);
        let (back, report) = xlsx_read::read_xlsx(buf).unwrap();
        assert_eq!(report.formulas, 1);
        assert_eq!(back.sheets.len(), 2);
        assert_eq!(back.sheets[1].name, "Second Sheet");
        let s = &back.sheets[0];
        assert_eq!(s.value(CellAddr::new(0, 0)), &Value::Number(1.5));
        assert_eq!(s.value(CellAddr::new(0, 1)), &Value::text("a & <b>"));
        let fc = s.get(CellAddr::new(1, 0)).unwrap();
        assert_eq!(fc.formula_src(), Some("CONCAT(A1,\"x\")"));
        assert_eq!(fc.value, Value::text("1.5x"));
        let st = back.styles.get(fc.style);
        assert!(st.bold);
        assert_eq!(st.num_fmt, "0.00%");
        assert_eq!(s.value(CellAddr::new(2, 0)), &Value::Bool(true));
        assert_eq!(s.merges.len(), 1);
        assert_eq!(back.names.len(), 1);
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        assert!(xlsx_read::read_xlsx(std::io::Cursor::new(b"not a zip".to_vec())).is_err());
        assert!(xlsx_read::read_xlsx(std::io::Cursor::new(Vec::new())).is_err());
    }
}
