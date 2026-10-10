//! Delimited text import/export.

use crate::{IoError, LoadReport};
use sp_core::parse::parse_numeric_input;
use sp_core::{CellAddr, MAX_COLS, MAX_ROWS, Value};
use sp_model::{Cell, Sheet, Workbook};
use std::io::{Read, Write};

/// Interpret one imported field: numbers, booleans and dates become typed values.
pub fn field_value(s: &str) -> Value {
    if s.is_empty() {
        return Value::Empty;
    }
    if s.eq_ignore_ascii_case("TRUE") {
        return Value::Bool(true);
    }
    if s.eq_ignore_ascii_case("FALSE") {
        return Value::Bool(false);
    }
    match parse_numeric_input(s, 2000) {
        Some((n, _)) => Value::Number(n),
        None => Value::text(s),
    }
}

pub fn read_delimited<R: Read>(
    r: R,
    delim: u8,
    sheet_name: &str,
) -> Result<(Workbook, LoadReport), IoError> {
    let mut rdr = csv::ReaderBuilder::new()
        .delimiter(delim)
        .has_headers(false)
        .flexible(true)
        .from_reader(r);
    let mut wb = Workbook::new();
    let mut sheet = Sheet::new(sheet_name);
    let mut report = LoadReport::default();
    for (ri, rec) in rdr.records().enumerate() {
        let rec = rec.map_err(|e| IoError::Format(e.to_string()))?;
        if ri >= MAX_ROWS as usize {
            report.warnings.push(format!(
                "file has more than {MAX_ROWS} rows; extra rows dropped"
            ));
            break;
        }
        for (ci, field) in rec.iter().enumerate() {
            if ci >= MAX_COLS as usize {
                report.warnings.push(format!(
                    "row {} has more than {MAX_COLS} columns; extra columns dropped",
                    ri + 1
                ));
                break;
            }
            let v = field_value(field);
            if !v.is_empty() {
                sheet.set(CellAddr::new(ri as u32, ci as u32), Cell::constant(v));
                report.cells += 1;
            }
        }
    }
    wb.sheets = vec![sheet];
    Ok((wb, report))
}

/// Write the displayed values of one sheet.
pub fn write_delimited<W: Write>(
    wb: &Workbook,
    sheet: usize,
    delim: u8,
    w: W,
) -> Result<(), IoError> {
    let mut wr = csv::WriterBuilder::new()
        .delimiter(delim)
        .flexible(true)
        .from_writer(w);
    let Some(s) = wb.sheet(sheet) else {
        return Err(IoError::Format("no such sheet".into()));
    };
    if let Some(u) = s.used_range() {
        for r in 0..=u.end.row {
            let row: Vec<String> = (0..=u.end.col)
                .map(|c| s.value(CellAddr::new(r, c)).to_display())
                .collect();
            wr.write_record(&row)
                .map_err(|e| IoError::Format(e.to_string()))?;
        }
    }
    wr.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_roundtrip() {
        let data = "a,1,TRUE\n\"x,y\",2.5,\n";
        let (wb, rep) = read_delimited(data.as_bytes(), b',', "S").unwrap();
        assert_eq!(rep.cells, 5);
        let s = &wb.sheets[0];
        assert_eq!(s.value(CellAddr::new(0, 1)), &Value::Number(1.0));
        assert_eq!(s.value(CellAddr::new(1, 0)), &Value::text("x,y"));
        let mut out = Vec::new();
        write_delimited(&wb, 0, b',', &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "a,1,TRUE\n\"x,y\",2.5,\n");
    }
}
