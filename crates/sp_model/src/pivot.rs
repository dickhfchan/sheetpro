//! Data-analysis objects of a sheet: pivot tables (with their cache snapshot), slicers,
//! what-if scenarios and data tables. Pure data; the layout engine lives in
//! `sp_formula::pivot`, the commands in `sp_engine::commands::{pivot_cmds, whatif_cmds}`.
//!
//! Every reference is stored as formula text (`Sheet1!$A$1:$D$20`, `$F$3`) so row/column
//! insert/delete and sheet renames rewrite them like any other formula (`sp_ops::pivot_shift`).

use serde::{Deserialize, Serialize};
use sp_core::Value;
use std::sync::Arc;

/// Most pivot tables / slicers / scenarios / data tables per sheet.
pub const MAX_OBJECTS: usize = 1_000;
/// Most records a pivot cache snapshots.
pub const MAX_RECORDS: usize = 1_000_000;
/// Most fields (source columns) of a pivot cache.
pub const MAX_FIELDS: usize = 16_384;

/// The analysis objects living on one sheet.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Analysis {
    #[serde(default)]
    pub pivots: Vec<PivotTable>,
    #[serde(default)]
    pub slicers: Vec<Slicer>,
    #[serde(default)]
    pub scenarios: Vec<Scenario>,
    #[serde(default)]
    pub data_tables: Vec<DataTable>,
}

impl Analysis {
    pub fn is_empty(&self) -> bool {
        self.pivots.is_empty()
            && self.slicers.is_empty()
            && self.scenarios.is_empty()
            && self.data_tables.is_empty()
    }

    pub fn pivot(&self, name: &str) -> Option<&PivotTable> {
        self.pivots
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
    }
}

/// How a data field summarizes its values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Aggregate {
    #[default]
    Sum,
    Count,
    Average,
    Max,
    Min,
    Product,
    CountNums,
    StdDev,
    StdDevP,
    Var,
    VarP,
}

impl Aggregate {
    pub const ALL: [Aggregate; 11] = [
        Aggregate::Sum,
        Aggregate::Count,
        Aggregate::Average,
        Aggregate::Max,
        Aggregate::Min,
        Aggregate::Product,
        Aggregate::CountNums,
        Aggregate::StdDev,
        Aggregate::StdDevP,
        Aggregate::Var,
        Aggregate::VarP,
    ];

    /// File-format / params name (`sum`, `count`, `countNums`, `stdDev`…).
    pub fn name(self) -> &'static str {
        match self {
            Aggregate::Sum => "sum",
            Aggregate::Count => "count",
            Aggregate::Average => "average",
            Aggregate::Max => "max",
            Aggregate::Min => "min",
            Aggregate::Product => "product",
            Aggregate::CountNums => "countNums",
            Aggregate::StdDev => "stdDev",
            Aggregate::StdDevP => "stdDevp",
            Aggregate::Var => "var",
            Aggregate::VarP => "varp",
        }
    }

    /// Caption prefix (`Sum of Sales`).
    pub fn caption(self) -> &'static str {
        match self {
            Aggregate::Sum => "Sum",
            Aggregate::Count => "Count",
            Aggregate::Average => "Average",
            Aggregate::Max => "Max",
            Aggregate::Min => "Min",
            Aggregate::Product => "Product",
            Aggregate::CountNums => "Count",
            Aggregate::StdDev => "StdDev",
            Aggregate::StdDevP => "StdDevp",
            Aggregate::Var => "Var",
            Aggregate::VarP => "Varp",
        }
    }

    /// Parse a name (case-insensitive; `avg`, `stdev`, `count_nums` accepted).
    pub fn from_name(s: &str) -> Option<Aggregate> {
        let k: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match k.as_str() {
            "sum" => Aggregate::Sum,
            "count" | "counta" => Aggregate::Count,
            "average" | "avg" | "mean" => Aggregate::Average,
            "max" => Aggregate::Max,
            "min" => Aggregate::Min,
            "product" => Aggregate::Product,
            "countnums" | "countnum" => Aggregate::CountNums,
            "stddev" | "stdev" | "stdevs" => Aggregate::StdDev,
            "stddevp" | "stdevp" => Aggregate::StdDevP,
            "var" | "vars" => Aggregate::Var,
            "varp" => Aggregate::VarP,
            _ => return None,
        })
    }
}

/// "Show Values As" calculation applied after aggregation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ShowAs {
    #[default]
    Normal,
    PercentOfTotal,
    PercentOfRow,
    PercentOfCol,
}

impl ShowAs {
    pub fn name(self) -> &'static str {
        match self {
            ShowAs::Normal => "normal",
            ShowAs::PercentOfTotal => "percentOfTotal",
            ShowAs::PercentOfRow => "percentOfRow",
            ShowAs::PercentOfCol => "percentOfCol",
        }
    }

    pub fn from_name(s: &str) -> Option<ShowAs> {
        let k: String = s
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect::<String>()
            .to_ascii_lowercase();
        Some(match k.as_str() {
            "normal" | "none" | "" => ShowAs::Normal,
            "percentoftotal" | "percentofgrandtotal" | "total" => ShowAs::PercentOfTotal,
            "percentofrow" | "percentofrowtotal" | "row" => ShowAs::PercentOfRow,
            "percentofcol" | "percentofcolumn" | "percentofcolumntotal" | "col" | "column" => {
                ShowAs::PercentOfCol
            }
            _ => return None,
        })
    }
}

/// One entry of the Values area.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DataField {
    /// Index into `PivotCache::fields`.
    pub field: usize,
    pub func: Aggregate,
    pub show_as: ShowAs,
    /// Caption; empty = generated (`Sum of Sales`).
    pub name: String,
}

/// Row/column label arrangement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PivotLayout {
    /// All row fields in one indented column, subtotals at the top of each group.
    #[default]
    Compact,
    /// One column per row field, subtotals at the top of each group.
    Outline,
    /// One column per row field, subtotals below each group.
    Tabular,
}

impl PivotLayout {
    pub fn name(self) -> &'static str {
        match self {
            PivotLayout::Compact => "compact",
            PivotLayout::Outline => "outline",
            PivotLayout::Tabular => "tabular",
        }
    }

    pub fn from_name(s: &str) -> Option<PivotLayout> {
        match s.to_ascii_lowercase().as_str() {
            "compact" => Some(PivotLayout::Compact),
            "outline" => Some(PivotLayout::Outline),
            "tabular" => Some(PivotLayout::Tabular),
            _ => None,
        }
    }
}

/// Snapshot of the source data taken at the last refresh.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PivotCache {
    /// Field names (the source's header row).
    pub fields: Vec<String>,
    /// One record per source data row, one value per field.
    pub records: Vec<Vec<Value>>,
}

impl PivotCache {
    pub fn field_index(&self, name: &str) -> Option<usize> {
        let n = name.trim();
        self.fields.iter().position(|f| f.eq_ignore_ascii_case(n))
    }
}

/// Items of one field excluded from the pivot (page-field selection, item filters, slicers).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct FieldFilter {
    pub field: usize,
    pub hidden: Vec<Value>,
}

/// A pivot table: definition, cache snapshot and where its output went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PivotTable {
    pub name: String,
    /// Source: reference text (`Sheet1!$A$1:$D$20`) or a table name.
    pub source: String,
    /// Top-left output cell on the owning sheet (reference text, `$F$3`); the report
    /// filter area (if any) starts here, the table itself below it.
    pub location: String,
    /// Area written by the last refresh (reference text; empty before the first one).
    pub output: String,
    pub cache: PivotCache,
    /// Row-area fields (indexes into `cache.fields`), outermost first.
    pub rows: Vec<usize>,
    pub cols: Vec<usize>,
    /// Report-filter (page) fields.
    pub filters: Vec<usize>,
    pub values: Vec<DataField>,
    pub layout: PivotLayout,
    /// Grand-total row at the bottom (totals of each column).
    pub grand_total_row: bool,
    /// Grand-total column at the right (totals of each row).
    pub grand_total_col: bool,
    pub subtotals: bool,
    /// Excluded items per field.
    pub field_filters: Vec<FieldFilter>,
    /// Style name kept for the file (`PivotStyleLight16`).
    pub style: String,
    /// The definition and parts as loaded from a file; while the pivot is unchanged the
    /// writer re-emits the original parts verbatim.
    #[serde(skip)]
    pub original: Option<Arc<PivotOriginal>>,
}

impl Default for PivotTable {
    fn default() -> Self {
        PivotTable {
            name: String::new(),
            source: String::new(),
            location: String::new(),
            output: String::new(),
            cache: PivotCache::default(),
            rows: Vec::new(),
            cols: Vec::new(),
            filters: Vec::new(),
            values: Vec::new(),
            layout: PivotLayout::Compact,
            grand_total_row: true,
            grand_total_col: true,
            subtotals: true,
            field_filters: Vec::new(),
            style: "PivotStyleLight16".into(),
            original: None,
        }
    }
}

/// A loaded pivot table's file content (see `PivotTable::original`).
#[derive(Debug, Clone, PartialEq)]
pub struct PivotOriginal {
    /// The model as loaded (with `original` cleared), to detect edits.
    pub model: PivotTable,
    pub table_xml: Arc<[u8]>,
    pub cache_xml: Arc<[u8]>,
    pub records_xml: Option<Arc<[u8]>>,
}

impl PivotTable {
    /// Whether the pivot is unchanged since it was loaded from a file.
    pub fn is_pristine(&self) -> bool {
        let Some(o) = &self.original else {
            return false;
        };
        let mut me = self.clone();
        me.original = None;
        me == o.model
    }

    /// Items of `field` excluded from the pivot.
    pub fn hidden_items(&self, field: usize) -> &[Value] {
        self.field_filters
            .iter()
            .find(|f| f.field == field)
            .map_or(&[], |f| f.hidden.as_slice())
    }

    /// Caption of data field `i` (`Sum of Sales` unless renamed).
    pub fn data_caption(&self, i: usize) -> String {
        let Some(d) = self.values.get(i) else {
            return String::new();
        };
        if !d.name.is_empty() {
            return d.name.clone();
        }
        let f = self.cache.fields.get(d.field).map_or("", String::as_str);
        format!("{} of {f}", d.func.caption())
    }

    pub fn field_name(&self, i: usize) -> &str {
        self.cache.fields.get(i).map_or("", String::as_str)
    }
}

/// A slicer: a button list filtering one field of a pivot table.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Slicer {
    pub name: String,
    pub caption: String,
    /// Name of the pivot table (on the same sheet) it filters.
    pub pivot: String,
    /// Field index in that pivot's cache.
    pub field: usize,
}

/// A saved set of input values (Scenario Manager).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Scenario {
    pub name: String,
    /// Changing cells (reference text on the owning sheet, `$B$1,$B$2` or `$B$1:$B$3`).
    pub changing: String,
    /// One value per changing cell, row-major per area.
    pub values: Vec<Value>,
    pub comment: String,
}

/// A what-if data table: `range` holds the input values along its top row and/or left
/// column, the formula(s) on the other edge, and `TABLE()` cells in the body.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct DataTable {
    /// The whole block (reference text, `$A$3:$D$8`).
    pub range: String,
    /// Row input cell (values along the top row are substituted here); empty = none.
    pub row_input: String,
    /// Column input cell (values down the left column); empty = none.
    pub col_input: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_round_trip() {
        for a in Aggregate::ALL {
            assert_eq!(Aggregate::from_name(a.name()), Some(a));
        }
        assert_eq!(ShowAs::from_name("percent_of_total"), Some(ShowAs::PercentOfTotal));
        assert_eq!(PivotLayout::from_name("Tabular"), Some(PivotLayout::Tabular));
    }

    #[test]
    fn pristine_tracks_edits() {
        let mut p = PivotTable {
            name: "P".into(),
            ..PivotTable::default()
        };
        assert!(!p.is_pristine());
        p.original = Some(Arc::new(PivotOriginal {
            model: p.clone(),
            table_xml: Arc::from(Vec::new()),
            cache_xml: Arc::from(Vec::new()),
            records_xml: None,
        }));
        assert!(p.is_pristine());
        p.rows.push(0);
        assert!(!p.is_pristine());
        assert_eq!(p.data_caption(0), "");
    }
}
