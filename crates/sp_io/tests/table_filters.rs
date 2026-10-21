//! A filter over a table's range is the table's own filter in the file (a sheet-level
//! AutoFilter overlapping a table makes the target app repair the file), and comes back as
//! the sheet's filter in the app.

#![allow(clippy::unwrap_used, clippy::indexing_slicing)]
use sp_core::{CellAddr, CellRange, Value};
use sp_model::{AutoFilter, Cell, FilterColumn, FilterCriteria, Table, Workbook};

fn read_part(path: &std::path::Path, name: &str) -> String {
    let f = std::fs::File::open(path).unwrap();
    let mut z = zip::ZipArchive::new(f).unwrap();
    let mut s = String::new();
    std::io::Read::read_to_string(&mut z.by_name(name).unwrap(), &mut s).unwrap();
    s
}

#[test]
fn table_filter_lives_in_the_table_part_and_round_trips() {
    let mut wb = Workbook::new();
    let sh = wb.sheets.first_mut().unwrap();
    for (r, (a, b)) in [("Name", "Qty"), ("a", "5"), ("b", "50")]
        .iter()
        .enumerate()
    {
        sh.set(CellAddr::new(r as u32, 0), Cell::constant(Value::text(*a)));
        let v = b.parse::<f64>().map_or(Value::text(*b), Value::Number);
        sh.set(CellAddr::new(r as u32, 1), Cell::constant(v));
    }
    let range = CellRange::parse("A1:B3").unwrap();
    sh.tables
        .push(Table::new(1, "T", range, vec!["Name".into(), "Qty".into()]));
    let columns = vec![FilterColumn {
        col: 1,
        criteria: FilterCriteria::Values {
            values: vec!["50".into()],
            blanks: false,
        },
    }];
    sh.auto_filter = Some(AutoFilter {
        range,
        columns: columns.clone(),
    });
    sh.hidden_rows.insert(1);

    let dir = std::env::temp_dir().join(format!("sp-table-filter-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.xlsx");
    sp_io::save(&wb, &path).unwrap();
    assert!(
        !read_part(&path, "xl/worksheets/sheet1.xml").contains("<autoFilter"),
        "no sheet-level AutoFilter over a table"
    );
    let table = read_part(&path, "xl/tables/table1.xml");
    assert!(table.contains(r#"<filterColumn colId="1">"#), "{table}");

    let (back, _) = sp_io::load(&path).unwrap();
    let s = &back.sheets[0];
    assert_eq!(
        s.auto_filter.as_ref().map(|f| f.columns.clone()),
        Some(columns)
    );
    assert!(
        s.tables[0].filter_columns.is_empty(),
        "one source of truth in the app"
    );
    assert!(s.hidden_rows.contains(&1));
}
