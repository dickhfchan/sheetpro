//! L1 data model: workbook, sheets, cells, styles, defined names. Pure data.

pub mod features;
pub mod style;

use serde::{Deserialize, Serialize};
use sp_core::{CellAddr, CellRange, Value};
use std::collections::{BTreeMap, BTreeSet};

pub use features::{
    AllowEditRange, AutoFilter, Comment, CommentKind, CommentReply, DataValidation, ErrorStyle,
    FilterColumn, FilterCond, FilterCriteria, Hyperlink, Margins, Orientation, Outline,
    PageSetup, PasswordHash, SheetProtection, ValidationKind, ValidationOp, WorkbookProtection,
};
pub use style::{Border, BorderStyle, Color, HAlign, Style, StyleId, StyleTable, VAlign};

/// What the user entered into a cell.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Content {
    /// A constant.
    Value(Value),
    /// Formula source without the leading `=`.
    Formula(String),
}

impl Default for Content {
    fn default() -> Self {
        Content::Value(Value::Empty)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Cell {
    pub content: Content,
    /// Cached result (formulas) or the constant itself.
    pub value: Value,
    pub style: StyleId,
}

impl Cell {
    pub fn constant(v: Value) -> Self {
        Cell {
            content: Content::Value(v.clone()),
            value: v,
            style: StyleId::DEFAULT,
        }
    }

    pub fn formula(src: impl Into<String>) -> Self {
        Cell {
            content: Content::Formula(src.into()),
            value: Value::Empty,
            style: StyleId::DEFAULT,
        }
    }

    pub fn formula_src(&self) -> Option<&str> {
        match &self.content {
            Content::Formula(f) => Some(f),
            Content::Value(_) => None,
        }
    }

    /// Text shown in the formula bar.
    pub fn input_text(&self) -> String {
        match &self.content {
            Content::Formula(f) => format!("={f}"),
            Content::Value(v) => match v {
                Value::Text(s) if needs_quote_prefix(s) => format!("'{s}"),
                other => other.to_display(),
            },
        }
    }

    /// A cell carrying no content and the default style can be dropped from storage.
    pub fn is_blank(&self) -> bool {
        matches!(self.content, Content::Value(Value::Empty)) && self.style == StyleId::DEFAULT
    }
}

fn needs_quote_prefix(s: &str) -> bool {
    s.starts_with('=')
        || s.parse::<f64>().is_ok()
        || s.eq_ignore_ascii_case("true")
        || s.eq_ignore_ascii_case("false")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CalcMode {
    #[default]
    Automatic,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DefinedName {
    pub name: String,
    /// Formula text without `=`, e.g. `Sheet1!$A$1:$B$4`.
    pub formula: String,
    /// `None` = workbook scope, `Some(i)` = local to sheet `i`.
    pub scope: Option<usize>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Visibility {
    #[default]
    Visible,
    Hidden,
    VeryHidden,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub name: String,
    pub cells: BTreeMap<u32, BTreeMap<u32, Cell>>,
    /// Column widths in character units; absent = default.
    pub col_widths: BTreeMap<u32, f64>,
    /// Row heights in points; absent = default.
    pub row_heights: BTreeMap<u32, f64>,
    pub hidden_rows: BTreeSet<u32>,
    pub hidden_cols: BTreeSet<u32>,
    pub merges: Vec<CellRange>,
    /// Top-left cell of the scrollable pane; rows above / cols left are frozen.
    pub freeze: Option<CellAddr>,
    pub tab_color: Option<Color>,
    pub visibility: Visibility,
    pub show_gridlines: bool,
    pub default_col_width: f64,
    pub default_row_height: f64,
    // --- features (see `features.rs`) ---
    #[serde(default)]
    pub auto_filter: Option<AutoFilter>,
    #[serde(default)]
    pub outline: Outline,
    /// Notes and threaded comments, at most one per cell, sorted by address.
    #[serde(default)]
    pub comments: Vec<Comment>,
    #[serde(default)]
    pub hyperlinks: Vec<Hyperlink>,
    #[serde(default)]
    pub protection: Option<SheetProtection>,
    #[serde(default)]
    pub allow_edit_ranges: Vec<AllowEditRange>,
    #[serde(default)]
    pub validations: Vec<DataValidation>,
    #[serde(default)]
    pub page_setup: PageSetup,
}

pub const DEFAULT_COL_WIDTH: f64 = 8.43;
pub const DEFAULT_ROW_HEIGHT: f64 = 15.0;

impl Sheet {
    pub fn new(name: impl Into<String>) -> Self {
        Sheet {
            name: name.into(),
            cells: BTreeMap::new(),
            col_widths: BTreeMap::new(),
            row_heights: BTreeMap::new(),
            hidden_rows: BTreeSet::new(),
            hidden_cols: BTreeSet::new(),
            merges: Vec::new(),
            freeze: None,
            tab_color: None,
            visibility: Visibility::Visible,
            show_gridlines: true,
            default_col_width: DEFAULT_COL_WIDTH,
            default_row_height: DEFAULT_ROW_HEIGHT,
            auto_filter: None,
            outline: Outline::default(),
            comments: Vec::new(),
            hyperlinks: Vec::new(),
            protection: None,
            allow_edit_ranges: Vec::new(),
            validations: Vec::new(),
            page_setup: PageSetup::default(),
        }
    }

    pub fn comment_at(&self, a: CellAddr) -> Option<&Comment> {
        self.comments.iter().find(|c| c.addr == a)
    }

    pub fn hyperlink_at(&self, a: CellAddr) -> Option<&Hyperlink> {
        self.hyperlinks.iter().find(|h| h.range.contains(a))
    }

    /// The validation rule governing `a`, if any (the last one added wins).
    pub fn validation_at(&self, a: CellAddr) -> Option<&DataValidation> {
        self.validations.iter().rev().find(|v| v.covers(a))
    }

    pub fn get(&self, a: CellAddr) -> Option<&Cell> {
        self.cells.get(&a.row)?.get(&a.col)
    }

    pub fn get_mut(&mut self, a: CellAddr) -> Option<&mut Cell> {
        self.cells.get_mut(&a.row)?.get_mut(&a.col)
    }

    pub fn value(&self, a: CellAddr) -> &Value {
        static EMPTY: Value = Value::Empty;
        self.get(a).map_or(&EMPTY, |c| &c.value)
    }

    /// Insert or replace a cell; blank cells are removed to keep storage sparse.
    pub fn set(&mut self, a: CellAddr, cell: Cell) -> Option<Cell> {
        if cell.is_blank() {
            return self.remove(a);
        }
        self.cells.entry(a.row).or_default().insert(a.col, cell)
    }

    pub fn remove(&mut self, a: CellAddr) -> Option<Cell> {
        let row = self.cells.get_mut(&a.row)?;
        let old = row.remove(&a.col);
        if row.is_empty() {
            self.cells.remove(&a.row);
        }
        old
    }

    /// Mutable access, creating a default cell if missing.
    pub fn entry(&mut self, a: CellAddr) -> &mut Cell {
        self.cells
            .entry(a.row)
            .or_default()
            .entry(a.col)
            .or_default()
    }

    pub fn cell_count(&self) -> usize {
        self.cells.values().map(BTreeMap::len).sum()
    }

    /// Smallest range containing every stored cell (`None` for an empty sheet).
    pub fn used_range(&self) -> Option<CellRange> {
        let first_row = *self.cells.keys().next()?;
        let last_row = *self.cells.keys().next_back()?;
        let mut c0 = u32::MAX;
        let mut c1 = 0;
        for row in self.cells.values() {
            if let (Some(a), Some(b)) = (row.keys().next(), row.keys().next_back()) {
                c0 = c0.min(*a);
                c1 = c1.max(*b);
            }
        }
        (c0 != u32::MAX)
            .then(|| CellRange::new(CellAddr::new(first_row, c0), CellAddr::new(last_row, c1)))
    }

    /// Iterate stored cells inside `r`, row-major, without visiting empty addresses.
    pub fn cells_in(&self, r: CellRange) -> impl Iterator<Item = (CellAddr, &Cell)> {
        self.cells
            .range(r.start.row..=r.end.row)
            .flat_map(move |(row, cols)| {
                cols.range(r.start.col..=r.end.col)
                    .map(move |(col, c)| (CellAddr::new(*row, *col), c))
            })
    }

    pub fn iter(&self) -> impl Iterator<Item = (CellAddr, &Cell)> {
        self.cells.iter().flat_map(|(row, cols)| {
            cols.iter()
                .map(move |(col, c)| (CellAddr::new(*row, *col), c))
        })
    }

    pub fn col_width(&self, col: u32) -> f64 {
        if self.hidden_cols.contains(&col) {
            return 0.0;
        }
        self.col_widths
            .get(&col)
            .copied()
            .unwrap_or(self.default_col_width)
    }

    pub fn row_height(&self, row: u32) -> f64 {
        if self.hidden_rows.contains(&row) {
            return 0.0;
        }
        self.row_heights
            .get(&row)
            .copied()
            .unwrap_or(self.default_row_height)
    }

    pub fn merge_at(&self, a: CellAddr) -> Option<CellRange> {
        self.merges.iter().copied().find(|m| m.contains(a))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct DocProps {
    pub title: String,
    pub author: String,
    pub created: String,
    pub modified: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workbook {
    pub sheets: Vec<Sheet>,
    pub active: usize,
    pub names: Vec<DefinedName>,
    pub styles: StyleTable,
    pub calc_mode: CalcMode,
    pub props: DocProps,
    #[serde(default)]
    pub protection: Option<WorkbookProtection>,
}

impl Default for Workbook {
    fn default() -> Self {
        Self::new()
    }
}

/// Characters not allowed in sheet names.
pub const SHEET_NAME_FORBIDDEN: &[char] = &['[', ']', ':', '*', '?', '/', '\\'];

impl Workbook {
    /// A blank workbook with one sheet, `Sheet1`.
    pub fn new() -> Self {
        Workbook {
            sheets: vec![Sheet::new("Sheet1")],
            active: 0,
            names: Vec::new(),
            styles: StyleTable::default(),
            calc_mode: CalcMode::Automatic,
            props: DocProps::default(),
            protection: None,
        }
    }

    pub fn sheet(&self, i: usize) -> Option<&Sheet> {
        self.sheets.get(i)
    }

    pub fn sheet_mut(&mut self, i: usize) -> Option<&mut Sheet> {
        self.sheets.get_mut(i)
    }

    pub fn active_sheet(&self) -> Option<&Sheet> {
        self.sheets.get(self.active)
    }

    pub fn active_sheet_mut(&mut self) -> Option<&mut Sheet> {
        self.sheets.get_mut(self.active)
    }

    pub fn sheet_index(&self, name: &str) -> Option<usize> {
        self.sheets
            .iter()
            .position(|s| s.name.eq_ignore_ascii_case(name))
    }

    /// Validate a candidate sheet name (`except` = index allowed to already hold it).
    pub fn check_sheet_name(&self, name: &str, except: Option<usize>) -> Result<(), String> {
        if name.trim().is_empty() {
            return Err("sheet name cannot be blank".into());
        }
        if name.chars().count() > 31 {
            return Err("sheet name cannot exceed 31 characters".into());
        }
        if name.contains(SHEET_NAME_FORBIDDEN) || name.starts_with('\'') || name.ends_with('\'') {
            return Err("sheet name contains a forbidden character".into());
        }
        if name.eq_ignore_ascii_case("history") {
            return Err("'History' is a reserved sheet name".into());
        }
        match self.sheet_index(name) {
            Some(i) if Some(i) != except => Err(format!("a sheet named '{name}' already exists")),
            _ => Ok(()),
        }
    }

    /// Next free `SheetN` name.
    pub fn next_sheet_name(&self) -> String {
        (1..)
            .map(|n| format!("Sheet{n}"))
            .find(|n| self.sheet_index(n).is_none())
            .unwrap_or_else(|| "Sheet".to_string())
    }

    pub fn name_lookup(&self, name: &str, sheet: usize) -> Option<&DefinedName> {
        self.names
            .iter()
            .find(|n| n.scope == Some(sheet) && n.name.eq_ignore_ascii_case(name))
            .or_else(|| {
                self.names
                    .iter()
                    .find(|n| n.scope.is_none() && n.name.eq_ignore_ascii_case(name))
            })
    }

    pub fn visible_sheet_count(&self) -> usize {
        self.sheets
            .iter()
            .filter(|s| s.visibility == Visibility::Visible)
            .count()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sparse_storage() {
        let mut s = Sheet::new("S");
        let a = CellAddr::new(5, 3);
        s.set(a, Cell::constant(Value::Number(1.0)));
        assert_eq!(s.cell_count(), 1);
        assert_eq!(s.used_range().unwrap().to_a1(), "D6");
        s.set(a, Cell::default());
        assert_eq!(s.cell_count(), 0);
        assert!(s.cells.is_empty());
    }

    #[test]
    fn sheet_names() {
        let wb = Workbook::new();
        assert!(wb.check_sheet_name("sheet1", None).is_err());
        assert!(wb.check_sheet_name("a/b", None).is_err());
        assert!(wb.check_sheet_name("Data", None).is_ok());
        assert!(wb.check_sheet_name(&"x".repeat(32), None).is_err());
        assert_eq!(wb.next_sheet_name(), "Sheet2");
    }

    #[test]
    fn input_text() {
        assert_eq!(Cell::constant(Value::text("123")).input_text(), "'123");
        assert_eq!(Cell::formula("A1+1").input_text(), "=A1+1");
        assert_eq!(Cell::constant(Value::Number(2.5)).input_text(), "2.5");
    }
}
