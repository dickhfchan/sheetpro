//! L1 data model: workbook, sheets, cells, styles, defined names. Pure data.

pub mod chart;
pub mod condfmt;
pub mod features;
pub mod objects;
pub mod passthrough;
pub mod pivot;
pub mod style;
pub mod table;
pub mod theme;

use serde::{Deserialize, Serialize};
use sp_core::{CellAddr, CellRange, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

pub use condfmt::{
    CfKind, CfOp, CfRule, Cfvo, CfvoKind, CondFormat, DiffStyle, TextOp, TimePeriod,
};
pub use features::{
    AllowEditRange, AutoFilter, Comment, CommentKind, CommentReply, DataValidation, ErrorStyle,
    FilterColumn, FilterCond, FilterCriteria, HeaderFooter, Hyperlink, Margins, Orientation,
    Outline, PageSetup, PasswordHash, SheetProtection, ValidationKind, ValidationOp,
    WorkbookProtection,
};
pub use style::{Border, BorderStyle, Color, HAlign, Style, StyleId, StyleTable, VAlign};
pub use table::{Table, TableColumn};

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
                Value::Number(n) => sp_core::value::format_edit(*n),
                other => other.to_display(),
            },
        }
    }

    /// Text shown in the formula bar for a cell displayed with `num_fmt`: dates, times and
    /// percentages edit as typed (`1/31/2026`, `50%`), not as the stored serial or fraction.
    pub fn edit_text(&self, num_fmt: &str) -> String {
        if let Content::Value(Value::Number(n)) = &self.content
            && let Some(t) = sp_core::numfmt::edit_text(*n, num_fmt)
        {
            return t;
        }
        self.input_text()
    }

    /// A value spilled here by an array formula (no content of its own).
    pub fn is_spilled(&self) -> bool {
        matches!(self.content, Content::Value(Value::Empty)) && !self.value.is_empty()
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

/// Area covered by an array formula anchored at `range.start`. Maintained by recalculation:
/// dynamic-array results spill into the empty cells below/right of the anchor, which then
/// hold the spilled value in `Cell::value` with empty content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpillArea {
    pub range: CellRange,
    /// Legacy fixed-size array formula (Ctrl+Shift+Enter): the area never resizes or blocks.
    pub fixed: bool,
    /// Dynamic spill blocked by non-empty cells (the anchor shows `#SPILL!`); `range` is the
    /// area the result would need.
    pub blocked: bool,
}

/// One row's cells by column. Shared copy-on-write between sheet snapshots (undo history),
/// so cloning a sheet costs O(rows), not O(cells); mutate through [`Sheet::row_mut`] or
/// `Arc::make_mut`.
pub type CellRow = Arc<BTreeMap<u32, Cell>>;
/// A sheet's cells: row → [`CellRow`].
pub type Cells = BTreeMap<u32, CellRow>;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
    pub name: String,
    pub cells: Cells,
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
    /// Array-formula areas, sorted by anchor (row-major).
    #[serde(default)]
    pub spills: Vec<SpillArea>,
    /// Floating charts and sparkline groups (see `chart.rs`).
    #[serde(default)]
    pub drawings: chart::Drawings,
    /// Conditional formats (see `condfmt.rs`).
    #[serde(default)]
    pub cond_formats: Vec<CondFormat>,
    /// Structured tables (see `table.rs`).
    #[serde(default)]
    pub tables: Vec<Table>,
    /// File content we don't model, re-emitted on save (see `passthrough.rs`).
    #[serde(default)]
    pub passthrough: passthrough::Passthrough,
    /// Pivot tables, slicers, scenarios and data tables (see `pivot.rs`).
    #[serde(default)]
    pub analysis: pivot::Analysis,
    /// Pictures, shapes and text boxes, bottom to top (see `objects.rs`).
    #[serde(default)]
    pub objects: Vec<objects::DrawObject>,
    /// Sheet background picture (tiled behind the cells on screen, not printed).
    #[serde(default)]
    pub background: Option<objects::ImageData>,
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
            spills: Vec::new(),
            drawings: chart::Drawings::default(),
            cond_formats: Vec::new(),
            tables: Vec::new(),
            passthrough: Default::default(),
            analysis: Default::default(),
            objects: Vec::new(),
            background: None,
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
        let row = self.cells.get_mut(&a.row)?;
        if !row.contains_key(&a.col) {
            return None;
        }
        Arc::make_mut(row).get_mut(&a.col)
    }

    /// Mutable access to an existing row (unsharing it from snapshots if needed).
    pub fn row_mut(&mut self, row: u32) -> Option<&mut BTreeMap<u32, Cell>> {
        self.cells.get_mut(&row).map(Arc::make_mut)
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
        Arc::make_mut(self.cells.entry(a.row).or_default()).insert(a.col, cell)
    }

    pub fn remove(&mut self, a: CellAddr) -> Option<Cell> {
        let row = self.cells.get_mut(&a.row)?;
        if !row.contains_key(&a.col) {
            return None;
        }
        let old = Arc::make_mut(row).remove(&a.col);
        if row.is_empty() {
            self.cells.remove(&a.row);
        }
        old
    }

    /// Mutable access, creating a default cell if missing.
    pub fn entry(&mut self, a: CellAddr) -> &mut Cell {
        Arc::make_mut(self.cells.entry(a.row).or_default())
            .entry(a.col)
            .or_default()
    }

    pub fn cell_count(&self) -> usize {
        self.cells.values().map(|r| r.len()).sum()
    }

    /// Cells whose stored state differs between `self` and `other` (another version of the
    /// same sheet), row-major. Rows still shared between the two versions are skipped
    /// without comparing their cells. Stops early with `None` once more than `limit`
    /// addresses differ.
    pub fn changed_cells(&self, other: &Sheet, limit: usize) -> Option<Vec<CellAddr>> {
        let mut out = Vec::new();
        let (mut a, mut b) = (self.cells.iter().peekable(), other.cells.iter().peekable());
        let empty = BTreeMap::new();
        loop {
            let (row, ra, rb) = match (a.peek(), b.peek()) {
                (None, None) => break,
                (Some((ka, _)), Some((kb, _))) if ka == kb => {
                    let ((k, ra), (_, rb)) = (a.next()?, b.next()?);
                    if Arc::ptr_eq(ra, rb) {
                        continue;
                    }
                    (*k, &**ra, &**rb)
                }
                (Some((ka, _)), Some((kb, _))) if ka < kb => {
                    let (k, ra) = a.next()?;
                    (*k, &**ra, &empty)
                }
                (Some(_), None) => {
                    let (k, ra) = a.next()?;
                    (*k, &**ra, &empty)
                }
                _ => {
                    let (k, rb) = b.next()?;
                    (*k, &empty, &**rb)
                }
            };
            let (mut ca, mut cb) = (ra.iter().peekable(), rb.iter().peekable());
            loop {
                let col = match (ca.peek(), cb.peek()) {
                    (None, None) => break,
                    (Some((x, cx)), Some((y, cy))) if x == y => {
                        let (col, same) = (**x, cx == cy);
                        ca.next();
                        cb.next();
                        if same {
                            continue;
                        }
                        col
                    }
                    (Some((x, _)), Some((y, _))) if x < y => {
                        let col = **x;
                        ca.next();
                        col
                    }
                    (Some((x, _)), None) => {
                        let col = **x;
                        ca.next();
                        col
                    }
                    (_, Some((y, _))) => {
                        let col = **y;
                        cb.next();
                        col
                    }
                };
                out.push(CellAddr::new(row, col));
                if out.len() > limit {
                    return None;
                }
            }
        }
        Some(out)
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

    /// Array area anchored at `anchor`, if any.
    pub fn spill_at_anchor(&self, anchor: CellAddr) -> Option<&SpillArea> {
        self.spills.iter().find(|s| s.range.start == anchor)
    }

    /// Unblocked array area covering `a` (the anchor itself included).
    pub fn spill_covering(&self, a: CellAddr) -> Option<&SpillArea> {
        self.spills
            .iter()
            .find(|s| !s.blocked && s.range.contains(a))
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
    /// File content we don't model, re-emitted on save (see `passthrough.rs`).
    #[serde(default)]
    pub passthrough: passthrough::Passthrough,
    /// Workbook theme (colour slots and fonts; see `theme.rs`).
    #[serde(default)]
    pub theme: theme::Theme,
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
            passthrough: Default::default(),
            theme: theme::Theme::default(),
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
    fn snapshots_share_rows_copy_on_write() {
        let mut s = Sheet::new("S");
        for r in 0..100 {
            s.set(
                CellAddr::new(r, 0),
                Cell::constant(Value::Number(f64::from(r))),
            );
        }
        let snap = s.clone();
        // Cloning shares every row.
        assert!(
            s.cells
                .iter()
                .zip(&snap.cells)
                .all(|((_, a), (_, b))| Arc::ptr_eq(a, b))
        );
        s.set(CellAddr::new(5, 1), Cell::constant(Value::Bool(true)));
        s.get_mut(CellAddr::new(7, 0)).unwrap().value = Value::Number(-1.0);
        s.remove(CellAddr::new(9, 0));
        s.remove(CellAddr::new(11, 3)); // absent: no unsharing
        // The snapshot is untouched; only the edited rows were copied.
        assert_eq!(snap.value(CellAddr::new(7, 0)), &Value::Number(7.0));
        assert!(snap.get(CellAddr::new(5, 1)).is_none());
        assert_eq!(snap.cell_count(), 100);
        let unshared: Vec<u32> = s
            .cells
            .iter()
            .filter(|(r, row)| !snap.cells.get(r).is_some_and(|o| Arc::ptr_eq(o, row)))
            .map(|(r, _)| *r)
            .collect();
        assert_eq!(unshared, vec![5, 7]);
    }

    #[test]
    fn changed_cells_diff() {
        let mut a = Sheet::new("S");
        for r in 0..50 {
            for c in 0..3 {
                a.set(
                    CellAddr::new(r, c),
                    Cell::constant(Value::Number(f64::from(r + c))),
                );
            }
        }
        let before = a.clone();
        assert_eq!(a.changed_cells(&before, 10), Some(vec![]));
        a.set(CellAddr::new(3, 1), Cell::formula("A1"));
        a.set(CellAddr::new(3, 7), Cell::constant(Value::Bool(false)));
        a.remove(CellAddr::new(20, 0));
        a.set(CellAddr::new(60, 2), Cell::constant(Value::text("new row")));
        for r in 40..50 {
            a.remove(CellAddr::new(r, 2));
            a.remove(CellAddr::new(r, 1));
            a.remove(CellAddr::new(r, 0));
        }
        let mut want = vec![
            CellAddr::new(3, 1),
            CellAddr::new(3, 7),
            CellAddr::new(20, 0),
        ];
        for r in 40..50 {
            want.extend((0..3).map(|c| CellAddr::new(r, c)));
        }
        want.push(CellAddr::new(60, 2));
        assert_eq!(a.changed_cells(&before, 1000).as_ref(), Some(&want));
        // Symmetric.
        assert_eq!(before.changed_cells(&a, 1000), Some(want));
        // An unshared but equal row reports nothing.
        let mut c = before.clone();
        let v = c.get(CellAddr::new(1, 1)).cloned().unwrap();
        c.set(CellAddr::new(1, 1), v);
        assert_eq!(c.changed_cells(&before, 10), Some(vec![]));
        // Limit.
        assert_eq!(a.changed_cells(&before, 5), None);
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
        // 15 significant digits, not General's 10: editing must not round the value.
        for (n, s) in [
            (9428.135032823471, "9428.13503282347"),
            (0.1 + 0.2, "0.3"),
            (1e20, "1E+20"),
            (-1.5e-7, "-1.5E-07"),
            (123456789012.5, "123456789012.5"),
        ] {
            assert_eq!(Cell::constant(Value::Number(n)).input_text(), s);
        }
    }
}
