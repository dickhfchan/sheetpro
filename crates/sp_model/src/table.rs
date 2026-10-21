//! Structured tables ("Format as Table"): a named range with a header row, optional totals
//! row, named columns, a style and banding options. Pure data.

use crate::{Sheet, Workbook};
use serde::{Deserialize, Serialize};
use sp_core::{CellAddr, CellRange};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct TableColumn {
    pub name: String,
    /// Totals-row aggregate (`sum`, `average`, `count`, `countNums`, `max`, `min`, `stdDev`,
    /// `var`, `custom`), if any.
    #[serde(default)]
    pub totals_function: Option<String>,
    /// Totals-row label text (used when there is no function).
    #[serde(default)]
    pub totals_label: Option<String>,
    /// Totals-row custom formula (`totals_function = custom`), without `=`.
    #[serde(default)]
    pub totals_formula: Option<String>,
    /// Calculated-column formula, without `=`.
    #[serde(default)]
    pub calc_formula: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Table {
    /// Workbook-unique numeric id (file format `id`).
    pub id: u32,
    /// Workbook-unique name used by structured references.
    pub name: String,
    /// Whole table area, header and totals rows included.
    pub range: CellRange,
    pub header_row: bool,
    pub totals_row: bool,
    pub columns: Vec<TableColumn>,
    /// Style name, e.g. `TableStyleMedium2`.
    pub style: String,
    pub banded_rows: bool,
    pub banded_cols: bool,
    pub first_col: bool,
    pub last_col: bool,
    /// Header filter buttons shown.
    pub autofilter: bool,
    /// Active filter criteria of the table's own AutoFilter (`colId` relative to the
    /// table). Rows hidden inside a filtered table are filtered rows for SUBTOTAL.
    #[serde(default)]
    pub filter_columns: Vec<crate::features::FilterColumn>,
}

pub const DEFAULT_TABLE_STYLE: &str = "TableStyleMedium2";

impl Table {
    /// The range its own AutoFilter covers: header row through the last data row (`None`
    /// without a header row or filter buttons).
    pub fn filter_range(&self) -> Option<CellRange> {
        if !self.header_row || !self.autofilter {
            return None;
        }
        let end = if self.totals_row {
            self.range.end.row.saturating_sub(1).max(self.range.start.row)
        } else {
            self.range.end.row
        };
        Some(CellRange::new(
            self.range.start,
            CellAddr::new(end, self.range.end.col),
        ))
    }

    pub fn new(id: u32, name: impl Into<String>, range: CellRange, columns: Vec<String>) -> Self {
        Table {
            id,
            name: name.into(),
            range,
            header_row: true,
            totals_row: false,
            columns: columns
                .into_iter()
                .map(|name| TableColumn {
                    name,
                    ..TableColumn::default()
                })
                .collect(),
            style: DEFAULT_TABLE_STYLE.into(),
            banded_rows: true,
            banded_cols: false,
            first_col: false,
            last_col: false,
            autofilter: true,
            filter_columns: Vec::new(),
        }
    }

    pub fn header_range(&self) -> Option<CellRange> {
        self.header_row.then(|| {
            CellRange::new(
                self.range.start,
                CellAddr::new(self.range.start.row, self.range.end.col),
            )
        })
    }

    pub fn totals_range(&self) -> Option<CellRange> {
        self.totals_row.then(|| {
            CellRange::new(
                CellAddr::new(self.range.end.row, self.range.start.col),
                self.range.end,
            )
        })
    }

    /// Body rows (between header and totals). `None` when the table has no data rows.
    pub fn data_range(&self) -> Option<CellRange> {
        let r0 = self.range.start.row + u32::from(self.header_row);
        let r1 = self.range.end.row.checked_sub(u32::from(self.totals_row))?;
        (r0 <= r1).then(|| {
            CellRange::new(
                CellAddr::new(r0, self.range.start.col),
                CellAddr::new(r1, self.range.end.col),
            )
        })
    }

    /// 0-based index of a column by name (case-insensitive).
    pub fn column_index(&self, name: &str) -> Option<u32> {
        let name = name.trim();
        self.columns
            .iter()
            .position(|c| c.name.eq_ignore_ascii_case(name))
            .and_then(|i| u32::try_from(i).ok())
    }

    /// Sheet column of table column `i`.
    pub fn sheet_col(&self, i: u32) -> Option<u32> {
        let c = self.range.start.col.checked_add(i)?;
        (c <= self.range.end.col).then_some(c)
    }
}

/// Is `name` usable as a table name (and not a cell reference)?
pub fn valid_table_name(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first.is_alphabetic() || first == '_' || first == '\\')
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '.' || c == '\\')
        && name.chars().count() <= 255
        && sp_core::addr::parse_a1_parts(name).is_none()
        && !name.eq_ignore_ascii_case("r")
        && !name.eq_ignore_ascii_case("c")
}

impl Sheet {
    /// Table covering `a`, if any.
    pub fn table_at(&self, a: CellAddr) -> Option<&Table> {
        self.tables.iter().find(|t| t.range.contains(a))
    }
}

impl Workbook {
    /// (sheet index, table) for a table name (case-insensitive).
    pub fn find_table(&self, name: &str) -> Option<(usize, &Table)> {
        self.sheets.iter().enumerate().find_map(|(i, s)| {
            s.tables
                .iter()
                .find(|t| t.name.eq_ignore_ascii_case(name))
                .map(|t| (i, t))
        })
    }

    /// Next free table id and default `TableN` name.
    pub fn next_table_id_and_name(&self) -> (u32, String) {
        let id = self
            .sheets
            .iter()
            .flat_map(|s| s.tables.iter().map(|t| t.id))
            .max()
            .map_or(1, |m| m.saturating_add(1));
        let name = (1u32..)
            .map(|n| format!("Table{n}"))
            .find(|n| self.find_table(n).is_none() && self.name_lookup(n, 0).is_none())
            .unwrap_or_else(|| format!("Table_{id}"));
        (id, name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t() -> Table {
        let mut t = Table::new(
            1,
            "Sales",
            CellRange::parse("B2:D6").unwrap(),
            vec!["Region".into(), "Q1".into(), "Q2".into()],
        );
        t.totals_row = true;
        t
    }

    #[test]
    fn areas() {
        let t = t();
        assert_eq!(t.header_range().unwrap().to_a1(), "B2:D2");
        assert_eq!(t.data_range().unwrap().to_a1(), "B3:D5");
        assert_eq!(t.totals_range().unwrap().to_a1(), "B6:D6");
        assert_eq!(t.column_index("q2"), Some(2));
        assert_eq!(t.sheet_col(2), Some(3));
        assert_eq!(t.sheet_col(3), None);
        let mut h = t.clone();
        h.range = CellRange::parse("B2:D3").unwrap();
        assert!(h.data_range().is_none());
    }

    #[test]
    fn names() {
        assert!(valid_table_name("Table1"));
        assert!(valid_table_name("_x"));
        assert!(!valid_table_name("A1"));
        assert!(!valid_table_name("1abc"));
        assert!(!valid_table_name("a b"));
        assert!(!valid_table_name("R"));
        let mut wb = Workbook::new();
        assert_eq!(wb.next_table_id_and_name(), (1, "Table1".into()));
        wb.sheets[0].tables.push(t());
        assert_eq!(wb.next_table_id_and_name().0, 2);
        assert_eq!(wb.find_table("SALES").map(|x| x.0), Some(0));
        assert!(wb.sheets[0].table_at(CellAddr::new(3, 2)).is_some());
    }
}
